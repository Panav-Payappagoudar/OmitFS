pub mod config;
pub mod db;
pub mod embedding;
pub mod watcher;

#[cfg(unix)]
pub mod fuse;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use config::Config;
use db::OmitDb;
use embedding::EmbeddingEngine;
use notify::EventKind;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tracing::{error, info, warn, Level};

#[cfg(unix)]
use fuse::OmitFs;

// ─── CLI Definition ────────────────────────────────────────────────────────────

/// OmitFS — Intent-driven, 100% local semantic file system
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Create ~/.omitfs_data, init LanceDB, write default config, download SLM weights
    Init,

    /// Run background ingestion daemon (watches raw/ and embeds new/modified/deleted files)
    Daemon,

    /// Mount the FUSE semantic filesystem at <mount_point> (Unix only)
    Mount {
        /// Directory to mount (created if missing)
        mount_point: PathBuf,
    },

    /// Interactive semantic file manager: search → open / copy / move / delete
    Select {
        /// Natural-language query, e.g. "my calculus notes"
        query: String,
    },

    /// Install omitfs daemon as a background service (auto-start on boot)
    InstallService,

    /// Remove the background service installed by install-service
    UninstallService,
}

// ─── Logging ──────────────────────────────────────────────────────────────────

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

// ─── Ingestion helpers ────────────────────────────────────────────────────────

/// Extract raw text from a file.
/// Supported: PDF, Word (.docx), Excel/CSV (.xlsx, .xls, .csv), plain-text / code.
/// All other binary types are gracefully skipped — they are still searchable by filename.
fn extract_text(path: &std::path::Path) -> Option<String> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();

    match ext.as_str() {
        // ── PDF ──────────────────────────────────────────────────────────────
        "pdf" => {
            match pdf_extract::extract_text(path) {
                Ok(t) if !t.trim().is_empty() => Some(t),
                Ok(_)  => { info!("PDF {:?} yielded no text, skipping content", path); None }
                Err(e) => { error!("PDF extract {:?}: {}", path, e); None }
            }
        }

        // ── Word documents ────────────────────────────────────────────────────
        "docx" => {
            match extract_docx(path) {
                Ok(t) if !t.trim().is_empty() => Some(t),
                Ok(_)  => None,
                Err(e) => { error!("DOCX extract {:?}: {}", path, e); None }
            }
        }

        // ── Excel / spreadsheets ──────────────────────────────────────────────
        "xlsx" | "xls" | "xlsm" | "ods" => {
            match extract_spreadsheet(path) {
                Ok(t) if !t.trim().is_empty() => Some(t),
                Ok(_)  => None,
                Err(e) => { error!("Spreadsheet extract {:?}: {}", path, e); None }
            }
        }

        // ── CSV ───────────────────────────────────────────────────────────────
        "csv" => {
            match std::fs::read_to_string(path) {
                Ok(t) if !t.trim().is_empty() => Some(t),
                _ => None,
            }
        }

        // ── Known binary — skip content extraction but not filename index ─────
        "jpg" | "jpeg" | "png" | "gif" | "bmp" | "webp" | "mp3" | "mp4" | "mov" | "avi"
        | "exe" | "dll" | "so" | "dylib" | "zip" | "tar" | "gz" | "7z" | "rar"
        | "bin" | "class" | "pyc" | "wasm" | "psd" | "ai" | "sketch" => {
            info!("Skipping known binary format: {:?}", path);
            None
        }

        // ── Everything else: attempt UTF-8 text, detect binary via null bytes ──
        _ => {
            let buf = match std::fs::read(path) {
                Ok(b)  => b,
                Err(e) => { error!("Read {:?}: {}", path, e); return None; }
            };
            // Fast binary sniff: null byte in first 1 KB → binary
            if buf.iter().take(1024).any(|&b| b == 0) {
                info!("Skipping likely binary file: {:?}", path);
                return None;
            }
            match String::from_utf8(buf) {
                Ok(t) if !t.trim().is_empty() => Some(t),
                _ => None,
            }
        }
    }
}

/// Extract text from a `.docx` Word file via `docx-rs`.
fn extract_docx(path: &std::path::Path) -> Result<String> {
    let bytes = std::fs::read(path).context("Failed to read docx file")?;
    let docx = docx_rs::read_docx(&bytes).map_err(|e| anyhow::anyhow!("docx parse error: {:?}", e))?;
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

/// Extract text from Excel / ODS spreadsheets via `calamine`.
fn extract_spreadsheet(path: &std::path::Path) -> Result<String> {
    use calamine::{open_workbook_auto, Reader};
    let mut wb = open_workbook_auto(path).context("Failed to open spreadsheet")?;
    let mut out = String::new();
    for sheet_name in wb.sheet_names().to_owned() {
        if let Ok(range) = wb.worksheet_range(&sheet_name) {
            for row in range.rows() {
                let row_str: Vec<String> = row.iter().map(|c| c.to_string()).collect();
                out.push_str(&row_str.join("\t"));
                out.push('\n');
            }
        }
    }
    Ok(out)
}

/// Chunk text into overlapping windows.
/// Window size and overlap are driven by the user config.
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

// ─── Ingest a single file (used by daemon for both create and modify) ─────────

async fn ingest_file(
    path: &std::path::Path,
    db: &OmitDb,
    engine: &Arc<Mutex<EmbeddingEngine>>,
    cfg: &Config,
    is_modify: bool,
) {
    if !path.is_file() { return; }

    let filename  = path.file_name().unwrap_or_default().to_string_lossy().to_string();
    let phys_path = path.to_string_lossy().to_string();

    // On modify: purge old stale vectors first (upsert semantics)
    if is_modify {
        if let Err(e) = db.delete_by_path(&phys_path).await {
            error!("Failed to purge stale vectors for {:?}: {}", path, e);
        }
    }

    // Build chunk list — always starts with the filename itself
    let mut chunks = vec![filename.clone()];
    if let Some(text) = extract_text(path) {
        chunks.extend(chunk_text(&text, cfg.chunk_words, cfg.overlap_words));
    }

    let mut n_ok = 0usize;
    for chunk in &chunks {
        let vec = {
            let mut eng = match engine.lock() {
                Ok(g)  => g,
                Err(p) => p.into_inner(),
            };
            match eng.embed(chunk) {
                Ok(v)  => v,
                Err(e) => { error!("Embed chunk from {:?}: {}", path, e); continue; }
            }
        };
        let id = uuid::Uuid::new_v4().to_string();
        match db.insert_file(&id, &filename, &phys_path, vec).await {
            Ok(_)  => n_ok += 1,
            Err(e) => error!("LanceDB insert: {}", e),
        }
    }
    info!("Ingested {:?} → {} chunks", path, n_ok);
    println!("📄  Ingested {} → {} chunk(s)", filename, n_ok);
}

// ─── OS Service installation ──────────────────────────────────────────────────

fn install_service(exe: &str) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        // Register as a Windows Task Scheduler task (no admin required for user tasks)
        let task_xml = format!(
            r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <Triggers><LogonTrigger><Enabled>true</Enabled></LogonTrigger></Triggers>
  <Settings><MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit></Settings>
  <Actions><Exec><Command>{exe}</Command>
    <Arguments>daemon</Arguments></Exec></Actions>
</Task>"#,
            exe = exe
        );
        let xml_path = std::env::temp_dir().join("omitfs_task.xml");
        std::fs::write(&xml_path, task_xml.as_bytes())
            .context("Failed to write task XML")?;
        let status = std::process::Command::new("schtasks")
            .args(["/Create", "/TN", "OmitFS\\Daemon", "/XML",
                   xml_path.to_str().unwrap_or(""), "/F"])
            .status()
            .context("Failed to run schtasks")?;
        if !status.success() {
            anyhow::bail!("schtasks failed — run as administrator or check Task Scheduler");
        }
        println!("✅  OmitFS daemon registered as Windows Task Scheduler task 'OmitFS\\Daemon'.");
        println!("    It will start automatically at every login.");
    }

    #[cfg(target_os = "macos")]
    {
        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>com.omitfs.daemon</string>
  <key>ProgramArguments</key>
  <array><string>{exe}</string><string>daemon</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
</dict></plist>"#,
            exe = exe
        );
        let plist_path = dirs::home_dir()
            .context("Cannot determine home dir")?
            .join("Library/LaunchAgents/com.omitfs.daemon.plist");
        std::fs::write(&plist_path, plist).context("Failed to write plist")?;
        std::process::Command::new("launchctl")
            .args(["load", plist_path.to_str().unwrap_or("")])
            .status()
            .context("launchctl load failed")?;
        println!("✅  OmitFS daemon registered as macOS LaunchAgent (com.omitfs.daemon).");
        println!("    It will start automatically at every login.");
    }

    #[cfg(target_os = "linux")]
    {
        let unit = format!(
            "[Unit]\nDescription=OmitFS semantic daemon\nAfter=network.target\n\n\
             [Service]\nExecStart={exe} daemon\nRestart=on-failure\nRestartSec=5\n\n\
             [Install]\nWantedBy=default.target\n",
            exe = exe
        );
        let unit_dir = dirs::home_dir()
            .context("Cannot determine home dir")?
            .join(".config/systemd/user");
        std::fs::create_dir_all(&unit_dir).context("Failed to create systemd user dir")?;
        let unit_path = unit_dir.join("omitfs.service");
        std::fs::write(&unit_path, unit).context("Failed to write systemd unit")?;
        std::process::Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .status().context("systemctl daemon-reload failed")?;
        std::process::Command::new("systemctl")
            .args(["--user", "enable", "--now", "omitfs.service"])
            .status().context("systemctl enable failed")?;
        println!("✅  OmitFS daemon registered as systemd user service (omitfs.service).");
        println!("    It will start automatically at every login.");
    }

    #[allow(unused_variables)]
    let _ = exe;
    Ok(())
}

fn uninstall_service() -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        let status = std::process::Command::new("schtasks")
            .args(["/Delete", "/TN", "OmitFS\\Daemon", "/F"])
            .status()
            .context("Failed to run schtasks")?;
        if !status.success() {
            anyhow::bail!("schtasks /Delete failed — check Task Scheduler manually");
        }
        println!("✅  OmitFS daemon removed from Windows Task Scheduler.");
    }

    #[cfg(target_os = "macos")]
    {
        let plist_path = dirs::home_dir()
            .context("Cannot determine home dir")?
            .join("Library/LaunchAgents/com.omitfs.daemon.plist");
        let _ = std::process::Command::new("launchctl")
            .args(["unload", plist_path.to_str().unwrap_or("")])
            .status();
        let _ = std::fs::remove_file(&plist_path);
        println!("✅  OmitFS LaunchAgent removed.");
    }

    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "disable", "--now", "omitfs.service"])
            .status();
        let unit_path = dirs::home_dir()
            .context("Cannot determine home dir")?
            .join(".config/systemd/user/omitfs.service");
        let _ = std::fs::remove_file(&unit_path);
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .status();
        println!("✅  OmitFS systemd service removed.");
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

    // Ensure data_dir exists before logging starts
    if !data_dir.exists() {
        std::fs::create_dir_all(&data_dir)
            .context("Failed to create ~/.omitfs_data")?;
    }

    let _log_guard = setup_logging(&data_dir)?;

    // Load config (writes defaults on first run)
    let cfg = Config::load(&data_dir)?;

    match cli.command {
        // ── init ─────────────────────────────────────────────────────────────
        Commands::Init => {
            info!("omitfs init");
            if !raw_dir.exists() {
                std::fs::create_dir_all(&raw_dir)
                    .context("Failed to create raw dir")?;
            }

            let db_path = data_dir.join("lancedb");
            let _db = OmitDb::init(db_path).await?;

            println!("Downloading SLM weights (one-time, ~80 MB)...");
            let _engine = EmbeddingEngine::new()
                .context("Failed to initialize embedding engine")?;

            println!("✅  OmitFS initialized at {:?}", data_dir);
            println!("    Drop files into : {:?}", raw_dir);
            println!("    Edit config     : {:?}", data_dir.join("config.toml"));
            println!("\n    Config defaults:");
            println!("      max_results   = {}", cfg.max_results);
            println!("      chunk_words   = {}", cfg.chunk_words);
            println!("      overlap_words = {}", cfg.overlap_words);
        }

        // ── daemon ────────────────────────────────────────────────────────────
        Commands::Daemon => {
            info!("omitfs daemon starting");
            println!("🛰  OmitFS daemon watching {:?}", raw_dir);

            if !raw_dir.exists() {
                std::fs::create_dir_all(&raw_dir)
                    .context("raw dir missing — run `omitfs init` first")?;
            }

            let db_path = data_dir.join("lancedb");
            let db      = Arc::new(OmitDb::init(db_path).await?);
            let engine  = Arc::new(Mutex::new(EmbeddingEngine::new()?));

            let (tx, mut rx) = mpsc::channel(1000);
            let _watcher = watcher::start_watcher(&raw_dir, tx)?;

            while let Some(event) = rx.recv().await {
                match event.kind {
                    // ── File created or written ───────────────────────────
                    EventKind::Create(_) | EventKind::Modify(_) => {
                        let is_modify = matches!(event.kind, EventKind::Modify(_));
                        for path in event.paths {
                            ingest_file(&path, &db, &engine, &cfg, is_modify).await;
                        }
                    }

                    // ── File removed — purge its vectors from DB ──────────
                    EventKind::Remove(_) => {
                        for path in event.paths {
                            let phys_path = path.to_string_lossy().to_string();
                            info!("File removed: {:?} — purging from DB", path);
                            if let Err(e) = db.delete_by_path(&phys_path).await {
                                error!("Failed to delete {:?} from LanceDB: {}", path, e);
                            } else {
                                println!("🗑️  Purged {} from index.", phys_path);
                            }
                        }
                    }

                    _ => {}
                }
            }
        }

        // ── mount ─────────────────────────────────────────────────────────────
        Commands::Mount { mount_point } => {
            #[cfg(unix)]
            {
                info!("omitfs mount {:?}", mount_point);

                if !mount_point.exists() {
                    std::fs::create_dir_all(&mount_point)
                        .context("Failed to create mount point")?;
                }

                let db_path = data_dir.join("lancedb");
                let db      = Arc::new(OmitDb::init(db_path).await?);
                let engine  = Arc::new(Mutex::new(EmbeddingEngine::new()?));
                let fs      = OmitFs::new(db, engine, raw_dir, cfg);

                println!("🌌  Mounting OmitFS at {:?}", mount_point);
                println!("    Navigate with: cd \"{}/your intent here\"", mount_point.display());

                let options = vec![
                    fuser::MountOption::FSName("omitfs".to_string()),
                    fuser::MountOption::AutoUnmount,
                    fuser::MountOption::AllowOther,
                ];
                fuser::mount2(fs, &mount_point, &options)
                    .context("FUSE mount failed")?;
            }

            #[cfg(not(unix))]
            {
                let _ = mount_point;
                eprintln!("❌  FUSE mount is only supported on Linux/macOS.");
                eprintln!("    On Windows, install WinFSP (https://winfsp.dev/) and recompile");
                eprintln!("    with the winfsp feature enabled, or use `omitfs select` instead.");
                std::process::exit(1);
            }
        }

        // ── select ────────────────────────────────────────────────────────────
        Commands::Select { query } => {
            let db_path = data_dir.join("lancedb");
            let db      = Arc::new(OmitDb::init(db_path).await?);
            let mut engine = EmbeddingEngine::new()?;

            println!("\n🔍  Searching: \"{}\"\n", query);
            let vector  = engine.embed(&query)?;
            let results = db.search(vector, cfg.max_results, cfg.overfetch_factor).await?;

            if results.is_empty() {
                println!("No files matched \"{}\".", query);
                println!("Tip: run `omitfs daemon` to index files in {:?}", raw_dir);
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

            if choice == 0 || choice > results.len() {
                println!("Quit.");
                return Ok(());
            }

            let (filename, phys_path) = &results[choice - 1];
            println!("\nSelected: {}  ({})\n", filename, phys_path);
            println!("  [o]  Open        — launch with $EDITOR / system default");
            println!("  [d]  Delete      — remove file and purge from index");
            println!("  [p]  Print path  — print absolute physical path");
            println!("  [c]  Copy        — duplicate to a new location");
            println!("  [m]  Move        — relocate the physical file");
            println!("  [q]  Quit\n");
            print!("Choice: ");
            std::io::stdout().flush()?;

            let mut action = String::new();
            std::io::stdin().read_line(&mut action)?;

            match action.trim() {
                "o" => {
                    if cfg!(target_os = "windows") && std::env::var("EDITOR").is_err() {
                        std::process::Command::new("cmd")
                            .args(["/C", "start", "", phys_path])
                            .status()
                            .context("Failed to open file")?;
                    } else {
                        let editor = std::env::var("EDITOR").unwrap_or_else(|_| {
                            if cfg!(target_os = "macos") { "open".into() }
                            else { "xdg-open".into() }
                        });
                        std::process::Command::new(&editor)
                            .arg(phys_path)
                            .status()
                            .context("Failed to open file")?;
                    }
                }
                "d" => {
                    print!("Delete \"{}\"? [y/N]: ", filename);
                    std::io::stdout().flush()?;
                    let mut c = String::new();
                    std::io::stdin().read_line(&mut c)?;
                    if c.trim().eq_ignore_ascii_case("y") {
                        std::fs::remove_file(phys_path)
                            .context("Failed to delete file")?;
                        // Also purge from the vector DB immediately
                        let db_path = data_dir.join("lancedb");
                        if let Ok(db2) = OmitDb::init(db_path).await {
                            if let Err(e) = db2.delete_by_path(phys_path).await {
                                warn!("File deleted but DB purge failed: {}", e);
                            }
                        }
                        println!("Deleted and purged from index.");
                    } else {
                        println!("Aborted.");
                    }
                }
                "p" => {
                    println!("\n📂  {}", phys_path);
                }
                "c" => {
                    print!("Destination path: ");
                    std::io::stdout().flush()?;
                    let mut dest = String::new();
                    std::io::stdin().read_line(&mut dest)?;
                    let dest = shellexpand::tilde(dest.trim()).to_string();
                    std::fs::copy(phys_path, &dest)
                        .context("Copy failed")?;
                    println!("Copied → {}", dest);
                }
                "m" => {
                    print!("Destination path: ");
                    std::io::stdout().flush()?;
                    let mut dest = String::new();
                    std::io::stdin().read_line(&mut dest)?;
                    let dest = shellexpand::tilde(dest.trim()).to_string();
                    std::fs::rename(phys_path, &dest)
                        .context("Move failed")?;
                    println!("Moved → {}", dest);
                }
                _ => println!("Quit."),
            }
        }

        // ── install-service ───────────────────────────────────────────────────
        Commands::InstallService => {
            let exe = std::env::current_exe()
                .context("Cannot determine current executable path")?
                .to_string_lossy()
                .to_string();
            install_service(&exe)?;
        }

        // ── uninstall-service ─────────────────────────────────────────────────
        Commands::UninstallService => {
            uninstall_service()?;
        }
    }

    Ok(())
}
