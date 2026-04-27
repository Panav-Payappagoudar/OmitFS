pub mod config;
pub mod crypto;
pub mod db;
pub mod embedding;
pub mod hasher;
pub mod mcp;
pub mod ocr;
pub mod rag;
pub mod reranker;
pub mod server;
pub mod watcher;
pub mod whisper_transcribe;

#[cfg(unix)]
pub mod fuse;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use config::Config;
use db::OmitDb;
use embedding::EmbeddingEngine;
use hasher::HashManifest;
use notify::EventKind;
use std::io::Write;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tracing::{error, info, warn, Level};

#[cfg(unix)]
use fuse::OmitFs;

// ─── CLI ──────────────────────────────────────────────────────────────────────

/// OmitFS — 100% local intent-driven semantic file system
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Initialise ~/.omitfs_data, download SLM weights, write default config
    Init,

    /// Background daemon: watch raw/, embed new/changed files, purge deleted ones
    Daemon,

    /// Force re-index ALL files in raw/ (ignores the hash manifest)
    Reindex,

    /// Mount the FUSE semantic filesystem (Unix only)
    Mount {
        /// Directory to mount (created if missing)
        mount_point: PathBuf,
    },

    /// Interactive TUI file manager: search → open / copy / move / delete
    Select {
        /// Natural-language query
        query: String,
    },

    /// Answer a question from your local files using RAG + local Ollama LLM
    Ask {
        /// The question to answer
        question: String,
        /// Ollama model override (default: from config)
        #[arg(long)]
        model: Option<String>,
    },

    /// Start a local web UI + REST API at http://localhost:<port>
    Serve {
        /// Override the port (default: from config, 3030)
        #[arg(long)]
        port: Option<u16>,
    },

    /// Expose OmitFS as an MCP tool server (JSON-RPC 2.0 over stdio)
    Mcp,

    /// Register the daemon as an OS background service (auto-start on login)
    InstallService,

    /// Remove the OS background service
    UninstallService,
}

// ─── Logging ─────────────────────────────────────────────────────────────────

fn setup_logging(data_dir: &std::path::Path) -> Result<tracing_appender::non_blocking::WorkerGuard> {
    let appender = tracing_appender::rolling::daily(data_dir, "omitfs.log");
    let (nb, guard) = tracing_appender::non_blocking(appender);
    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .with_writer(nb)
        .with_ansi(false)
        .init();
    Ok(guard)
}

// ─── Text extraction ──────────────────────────────────────────────────────────

fn extract_text(path: &std::path::Path, ocr_enabled: bool, whisper_enabled: bool) -> Option<String> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    match ext.as_str() {
        "pdf" => {
            match pdf_extract::extract_text(path) {
                Ok(t) if !t.trim().is_empty() => Some(t),
                Ok(_)  => { info!("PDF {:?} has no text", path); None }
                Err(e) => { error!("PDF {:?}: {}", path, e); None }
            }
        }
        "docx" => extract_docx(path).ok().filter(|t| !t.trim().is_empty()),
        "xlsx" | "xls" | "xlsm" | "ods" => extract_sheet(path).ok().filter(|t| !t.trim().is_empty()),
        "csv" => std::fs::read_to_string(path).ok().filter(|t| !t.trim().is_empty()),
        "jpg"|"jpeg"|"png"|"gif"|"bmp"|"webp"|"tiff"|"tif" => {
            if ocr_enabled { ocr::extract_image_text(path) } else { None }
        }
        "mp3"|"wav"|"flac"|"ogg"|"m4a"|"mp4"|"mov"|"avi"|"mkv"|"webm" => {
            if whisper_enabled { whisper_transcribe::transcribe(path) } else { None }
        }
        _ => {
            let buf = std::fs::read(path).ok()?;
            if buf.iter().take(1024).any(|&b| b == 0) { return None; }
            String::from_utf8(buf).ok().filter(|t| !t.trim().is_empty())
        }
    }
}

fn extract_docx(path: &std::path::Path) -> anyhow::Result<String> {
    let bytes = std::fs::read(path)?;
    let docx  = docx_rs::read_docx(&bytes)
        .map_err(|e| anyhow::anyhow!("{:?}", e))?;
    let mut out = String::new();
    for child in &docx.document.children {
        if let docx_rs::DocumentChild::Paragraph(para) = child {
            for pc in &para.children {
                if let docx_rs::ParagraphChild::Run(run) = pc {
                    for rc in &run.children {
                        if let docx_rs::RunChild::Text(t) = rc {
                            out.push_str(&t.text);
                            out.push(' ');
                        }
                    }
                }
            }
            out.push('\n');
        }
    }
    Ok(out)
}

fn extract_sheet(path: &std::path::Path) -> anyhow::Result<String> {
    use calamine::{open_workbook_auto, Reader};
    let mut wb  = open_workbook_auto(path)?;
    let mut out = String::new();
    for name in wb.sheet_names().to_owned() {
        if let Ok(range) = wb.worksheet_range(&name) {
            for row in range.rows() {
                out.push_str(&row.iter().map(|c| c.to_string()).collect::<Vec<_>>().join("\t"));
                out.push('\n');
            }
        }
    }
    Ok(out)
}

fn chunk_text(text: &str, chunk_words: usize, overlap_words: usize) -> Vec<String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() { return vec![]; }
    let step = chunk_words.saturating_sub(overlap_words).max(1);
    let mut chunks = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let end = (i + chunk_words).min(words.len());
        chunks.push(words[i..end].join(" "));
        if end == words.len() { break; }
        i += step;
    }
    chunks
}

// ─── Core ingest (shared by daemon + reindex) ─────────────────────────────────

async fn ingest_file(
    path:      &std::path::Path,
    db:        &OmitDb,
    engine:    &Arc<Mutex<EmbeddingEngine>>,
    cfg:       &Config,
    is_modify: bool,
    manifest:  Option<&mut HashManifest>,
) {
    if !path.is_file() { return; }

    // Skip if already indexed and unchanged (manifest check)
    if let Some(ref m) = manifest {
        if !is_modify && !m.is_stale(path) {
            info!("Skipping unchanged file: {:?}", path);
            return;
        }
    }

    let filename  = path.file_name().unwrap_or_default().to_string_lossy().to_string();
    let phys_path = path.to_string_lossy().to_string();

    if is_modify {
        if let Err(e) = db.delete_by_path(&phys_path).await {
            error!("Purge stale vectors for {:?}: {}", path, e);
        }
    }

    // Always embed the filename so binary files are searchable
    let mut text_chunks: Vec<String> = vec![filename.clone()];
    if let Some(text) = extract_text(path, cfg.ocr_enabled, cfg.whisper_enabled) {
        text_chunks.extend(chunk_text(&text, cfg.chunk_words, cfg.overlap_words));
    }

    // Optional AES-256-GCM encryption of chunk text before DB storage
    let encryptor: Option<crypto::Encryptor> = if cfg.encryption_enabled {
        match crypto::Encryptor::load_or_create(
            &path.parent().and_then(|p| p.parent()).unwrap_or(std::path::Path::new(".")),
        ) {
            Ok(e)  => Some(e),
            Err(e) => { error!("Encryption init failed: {}", e); None }
        }
    } else {
        None
    };

    let mut n_ok = 0usize;
    for chunk in &text_chunks {
        let vec = {
            let mut eng = match engine.lock() {
                Ok(g)  => g,
                Err(p) => p.into_inner(),
            };
            match eng.embed(chunk) {
                Ok(v)  => v,
                Err(e) => { error!("Embed {:?}: {}", path, e); continue; }
            }
        };
        let id = uuid::Uuid::new_v4().to_string();
        // Encrypt chunk_text before storage if encryption is enabled
        let stored_chunk = match &encryptor {
            Some(enc) => enc.encrypt(chunk).unwrap_or_else(|_| chunk.clone()),
            None      => chunk.clone(),
        };
        match db.insert_file(&id, &filename, &phys_path, &stored_chunk, vec).await {
            Ok(_)  => n_ok += 1,
            Err(e) => error!("LanceDB insert: {}", e),
        }
    }

    info!("Ingested {:?} → {} chunk(s)", path, n_ok);
    println!("📄  Ingested {} → {} chunk(s)", filename, n_ok);

    if let Some(m) = manifest {
        if let Err(e) = m.mark_indexed(path) {
            warn!("Failed to update manifest for {:?}: {}", path, e);
        }
        if let Err(e) = m.save() {
            warn!("Failed to save manifest: {}", e);
        }
    }
}

// ─── OS service helpers ───────────────────────────────────────────────────────

fn install_service(exe: &str) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        let xml = format!(r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <Triggers><LogonTrigger><Enabled>true</Enabled></LogonTrigger></Triggers>
  <Settings><MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit></Settings>
  <Actions><Exec><Command>{exe}</Command><Arguments>daemon</Arguments></Exec></Actions>
</Task>"#);
        let xml_path = std::env::temp_dir().join("omitfs_task.xml");
        std::fs::write(&xml_path, xml.as_bytes())?;
        let ok = std::process::Command::new("schtasks")
            .args(["/Create", "/TN", "OmitFS\\Daemon", "/XML", xml_path.to_str().unwrap_or(""), "/F"])
            .status().context("schtasks failed")?;
        if !ok.success() { anyhow::bail!("schtasks returned error"); }
        println!("✅  Registered as Windows Task Scheduler task 'OmitFS\\Daemon'.");
    }
    #[cfg(target_os = "macos")]
    {
        let plist = format!(r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>com.omitfs.daemon</string>
  <key>ProgramArguments</key><array><string>{exe}</string><string>daemon</string></array>
  <key>RunAtLoad</key><true/><key>KeepAlive</key><true/>
</dict></plist>"#);
        let p = dirs::home_dir().context("No home dir")?.join("Library/LaunchAgents/com.omitfs.daemon.plist");
        std::fs::write(&p, plist)?;
        std::process::Command::new("launchctl").args(["load", p.to_str().unwrap_or("")]).status()?;
        println!("✅  Registered macOS LaunchAgent (com.omitfs.daemon).");
    }
    #[cfg(target_os = "linux")]
    {
        let unit = format!("[Unit]\nDescription=OmitFS daemon\nAfter=network.target\n\n[Service]\nExecStart={exe} daemon\nRestart=on-failure\nRestartSec=5\n\n[Install]\nWantedBy=default.target\n");
        let dir = dirs::home_dir().context("No home dir")?.join(".config/systemd/user");
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("omitfs.service"), unit)?;
        std::process::Command::new("systemctl").args(["--user","daemon-reload"]).status()?;
        std::process::Command::new("systemctl").args(["--user","enable","--now","omitfs.service"]).status()?;
        println!("✅  Registered systemd user service (omitfs.service).");
    }
    #[allow(unused_variables)]
    let _ = exe;
    Ok(())
}

fn uninstall_service() -> Result<()> {
    #[cfg(target_os = "windows")]
    { std::process::Command::new("schtasks").args(["/Delete","/TN","OmitFS\\Daemon","/F"]).status()?; println!("✅  Task removed."); }
    #[cfg(target_os = "macos")]
    {
        let p = dirs::home_dir().context("No home dir")?.join("Library/LaunchAgents/com.omitfs.daemon.plist");
        let _ = std::process::Command::new("launchctl").args(["unload", p.to_str().unwrap_or("")]).status();
        let _ = std::fs::remove_file(&p);
        println!("✅  LaunchAgent removed.");
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("systemctl").args(["--user","disable","--now","omitfs.service"]).status();
        let p = dirs::home_dir().context("No home dir")?.join(".config/systemd/user/omitfs.service");
        let _ = std::fs::remove_file(&p);
        let _ = std::process::Command::new("systemctl").args(["--user","daemon-reload"]).status();
        println!("✅  Service removed.");
    }
    Ok(())
}

// ─── Main ────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let data_dir = dirs::home_dir()
        .context("Cannot determine home directory")?
        .join(".omitfs_data");
    let raw_dir = data_dir.join("raw");

    if !data_dir.exists() {
        std::fs::create_dir_all(&data_dir).context("create ~/.omitfs_data")?;
    }

    let _log_guard = setup_logging(&data_dir)?;
    let cfg = Config::load(&data_dir)?;

    match cli.command {
        // ── init ─────────────────────────────────────────────────────────────
        Commands::Init => {
            info!("omitfs init");
            std::fs::create_dir_all(&raw_dir).context("create raw dir")?;
            let _db = OmitDb::init(data_dir.join("lancedb")).await?;
            println!("Downloading SLM weights (one-time ~80 MB)…");
            EmbeddingEngine::new().context("init embedding engine")?;
            println!("✅  OmitFS v{} ready at {:?}", env!("CARGO_PKG_VERSION"), data_dir);
            println!("    Drop files into : {:?}", raw_dir);
            println!("    Config          : {:?}", data_dir.join("config.toml"));
        }

        // ── daemon ────────────────────────────────────────────────────────────
        Commands::Daemon => {
            info!("omitfs daemon starting");
            std::fs::create_dir_all(&raw_dir).context("create raw dir")?;
            println!("🛰  OmitFS daemon watching {:?}", raw_dir);

            let db     = Arc::new(OmitDb::init(data_dir.join("lancedb")).await?);
            let engine = Arc::new(Mutex::new(EmbeddingEngine::new()?));
            let mut manifest = HashManifest::load(&data_dir)?;

            // Index any existing unindexed files on startup
            for entry in std::fs::read_dir(&raw_dir).context("read raw dir")? {
                if let Ok(e) = entry {
                    let p = e.path();
                    if p.is_file() && manifest.is_stale(&p) {
                        ingest_file(&p, &db, &engine, &cfg, false, Some(&mut manifest)).await;
                    }
                }
            }

            let (tx, mut rx) = mpsc::channel(1000);
            let _watcher = watcher::start_watcher(&raw_dir, tx)?;

            while let Some(event) = rx.recv().await {
                match event.kind {
                    EventKind::Create(_) => {
                        for path in event.paths {
                            ingest_file(&path, &db, &engine, &cfg, false, Some(&mut manifest)).await;
                        }
                    }
                    EventKind::Modify(_) => {
                        for path in event.paths {
                            ingest_file(&path, &db, &engine, &cfg, true, Some(&mut manifest)).await;
                        }
                    }
                    EventKind::Remove(_) => {
                        for path in event.paths {
                            let s = path.to_string_lossy().to_string();
                            if let Err(e) = db.delete_by_path(&s).await {
                                error!("Delete {:?} from DB: {}", path, e);
                            } else {
                                manifest.remove(&path);
                                let _ = manifest.save();
                                println!("🗑️  Purged {}", s);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        // ── reindex ───────────────────────────────────────────────────────────
        Commands::Reindex => {
            println!("♻️   Re-indexing all files in {:?}…", raw_dir);
            let db     = Arc::new(OmitDb::init(data_dir.join("lancedb")).await?);
            let engine = Arc::new(Mutex::new(EmbeddingEngine::new()?));
            let mut n  = 0usize;
            for entry in std::fs::read_dir(&raw_dir).context("read raw dir")? {
                if let Ok(e) = entry {
                    let p = e.path();
                    if p.is_file() {
                        // delete old then re-insert
                        let _ = db.delete_by_path(&p.to_string_lossy()).await;
                        ingest_file(&p, &db, &engine, &cfg, false, None).await;
                        n += 1;
                    }
                }
            }
            // Reset manifest
            let mut mf = HashManifest::load(&data_dir)?;
            for entry in std::fs::read_dir(&raw_dir).context("read raw dir")? {
                if let Ok(e) = entry { let _ = mf.mark_indexed(&e.path()); }
            }
            let _ = mf.save();
            println!("✅  Re-indexed {} files.", n);
        }

        // ── mount ─────────────────────────────────────────────────────────────
        Commands::Mount { mount_point } => {
            #[cfg(unix)]
            {
                std::fs::create_dir_all(&mount_point).context("create mount point")?;
                let db     = Arc::new(OmitDb::init(data_dir.join("lancedb")).await?);
                let engine = Arc::new(Mutex::new(EmbeddingEngine::new()?));
                let fs     = OmitFs::new(db, engine, raw_dir, cfg);
                println!("🌌  Mounted at {:?} — cd into any concept to search", mount_point);
                let opts = vec![
                    fuser::MountOption::FSName("omitfs".into()),
                    fuser::MountOption::AutoUnmount,
                    fuser::MountOption::AllowOther,
                ];
                fuser::mount2(fs, &mount_point, &opts).context("FUSE mount failed")?;
            }
            #[cfg(not(unix))]
            {
                let _ = mount_point;
                eprintln!("❌  FUSE is Unix-only. On Windows install WinFSP (https://winfsp.dev/).");
                eprintln!("    Use `omitfs select` or `omitfs serve` instead.");
                std::process::exit(1);
            }
        }

        // ── select ────────────────────────────────────────────────────────────
        Commands::Select { query } => {
            let db     = Arc::new(OmitDb::init(data_dir.join("lancedb")).await?);
            let mut engine = EmbeddingEngine::new()?;

            println!("\n🔍  Searching: \"{query}\"\n");
            let vector  = engine.embed(&query)?;
            let raw     = db.search_with_chunks(vector, cfg.max_results, cfg.overfetch_factor).await?;
            let results = reranker::rerank(&query, raw);

            if results.is_empty() {
                println!("No files matched. Run `omitfs daemon` to index files in {:?}.", raw_dir);
                return Ok(());
            }
            println!("Found {} file(s):\n", results.len());
            for (i, (name, path)) in results.iter().enumerate() {
                println!("  [{}]  {}  →  {}", i + 1, name, path);
            }
            print!("\nSelect number (0 to quit): ");
            std::io::stdout().flush()?;
            let mut buf = String::new();
            std::io::stdin().read_line(&mut buf)?;
            let choice: usize = buf.trim().parse().unwrap_or(0);
            if choice == 0 || choice > results.len() { println!("Quit."); return Ok(()); }

            let (filename, phys_path) = &results[choice - 1];
            println!("\nSelected: {filename}  ({phys_path})\n");
            println!("  [o] Open   [d] Delete   [p] Print path   [c] Copy   [m] Move   [q] Quit");
            print!("Choice: ");
            std::io::stdout().flush()?;
            let mut action = String::new();
            std::io::stdin().read_line(&mut action)?;

            match action.trim() {
                "o" => {
                    if cfg!(target_os = "windows") && std::env::var("EDITOR").is_err() {
                        std::process::Command::new("cmd").args(["/C","start","",phys_path]).status()?;
                    } else {
                        let ed = std::env::var("EDITOR").unwrap_or_else(|_|
                            if cfg!(target_os = "macos") { "open".into() } else { "xdg-open".into() });
                        std::process::Command::new(&ed).arg(phys_path).status()?;
                    }
                }
                "d" => {
                    print!("Delete \"{filename}\"? [y/N]: ");
                    std::io::stdout().flush()?;
                    let mut c = String::new();
                    std::io::stdin().read_line(&mut c)?;
                    if c.trim().eq_ignore_ascii_case("y") {
                        std::fs::remove_file(phys_path).context("delete file")?;
                        let db2 = OmitDb::init(data_dir.join("lancedb")).await?;
                        if let Err(e) = db2.delete_by_path(phys_path).await {
                            warn!("DB purge after delete: {}", e);
                        }
                        println!("Deleted and removed from index.");
                    } else { println!("Aborted."); }
                }
                "p" => println!("\n📂  {phys_path}"),
                "c" => {
                    print!("Destination path: "); std::io::stdout().flush()?;
                    let mut d = String::new(); std::io::stdin().read_line(&mut d)?;
                    let d = shellexpand::tilde(d.trim()).to_string();
                    std::fs::copy(phys_path, &d).context("copy")?;
                    println!("Copied → {d}");
                }
                "m" => {
                    print!("Destination path: "); std::io::stdout().flush()?;
                    let mut d = String::new(); std::io::stdin().read_line(&mut d)?;
                    let d = shellexpand::tilde(d.trim()).to_string();
                    std::fs::rename(phys_path, &d).context("move")?;
                    println!("Moved → {d}");
                }
                _ => println!("Quit."),
            }
        }

        // ── ask ───────────────────────────────────────────────────────────────
        Commands::Ask { question, model } => {
            let db     = Arc::new(OmitDb::init(data_dir.join("lancedb")).await?);
            let mut engine = EmbeddingEngine::new()?;
            let mdl    = model.unwrap_or_else(|| cfg.ollama_model.clone());

            println!("\n🤖  Searching context for: \"{question}\"\n");
            let vector = engine.embed(&question)?;
            let raw    = db.search_with_chunks(vector, cfg.max_results, cfg.overfetch_factor).await?;
            let chunks = {
                let reranked_names = reranker::rerank(&question, raw.clone());
                // Re-attach chunk text in re-ranked order
                let mut ordered = Vec::new();
                for (fname, path) in reranked_names {
                    if let Some(triple) = raw.iter().find(|(f,p,_)| f == &fname && p == &path) {
                        ordered.push(triple.clone());
                    }
                }
                ordered
            };

            if chunks.is_empty() {
                println!("No context found. Run `omitfs daemon` to index files first.");
                return Ok(());
            }

            println!("📚  Using {} source(s):", chunks.len());
            for (i, (fname, _, _)) in chunks.iter().enumerate() {
                println!("    [{}] {}", i + 1, fname);
            }
            println!("\n💬  Answer:\n");
            rag::ask(&question, &chunks, &cfg.ollama_url, &mdl).await?;
        }

        // ── serve ─────────────────────────────────────────────────────────────
        Commands::Serve { port } => {
            let port   = port.unwrap_or(cfg.serve_port);
            let db     = Arc::new(OmitDb::init(data_dir.join("lancedb")).await?);
            let engine = Arc::new(Mutex::new(EmbeddingEngine::new()?));
            let state  = server::AppState { db, engine, cfg: Arc::new(cfg) };
            let app    = server::build_router(state);
            let addr: SocketAddr = format!("127.0.0.1:{port}").parse().context("Invalid address")?;
            println!("🌐  OmitFS web UI → http://localhost:{port}");
            println!("    Press Ctrl+C to stop.");
            let listener = tokio::net::TcpListener::bind(addr).await.context("bind failed")?;
            axum::serve(listener, app).await.context("server error")?;
        }

        // ── mcp ───────────────────────────────────────────────────────────────
        Commands::Mcp => {
            let db     = Arc::new(OmitDb::init(data_dir.join("lancedb")).await?);
            let engine = Arc::new(Mutex::new(EmbeddingEngine::new()?));
            mcp::run_mcp_server(db, engine, Arc::new(cfg)).await?;
        }

        // ── install-service ───────────────────────────────────────────────────
        Commands::InstallService => {
            let exe = std::env::current_exe()?.to_string_lossy().to_string();
            install_service(&exe)?;
        }

        // ── uninstall-service ─────────────────────────────────────────────────
        Commands::UninstallService => {
            uninstall_service()?;
        }
    }

    Ok(())
}
