pub mod db;
pub mod embedding;
pub mod fuse;
pub mod watcher;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::io::Write; // needed for print! + flush
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tracing::{error, info, Level};

use db::OmitDb;
use embedding::EmbeddingEngine;
use fuse::OmitFs;

// ─── CLI Definition ────────────────────────────────────────────────────────────

/// OmitFS — Intent-driven semantic file system
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Create ~/.omitfs_data, init LanceDB, download SLM weights
    Init,

    /// Run background ingestion daemon (watch raw/ and embed new files)
    Daemon,

    /// Mount the FUSE semantic filesystem at <mount_point>
    Mount {
        /// Directory to mount (must already exist)
        mount_point: PathBuf,
    },

    /// Interactive semantic file manager: search → open / copy / move / delete
    Select {
        /// Natural-language query, e.g. "my calculus notes"
        query: String,
    },
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

/// Extract raw text from a file. PDFs use pdf-extract; everything else is UTF-8.
fn extract_text(path: &std::path::Path) -> Option<String> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    match ext.as_str() {
        "pdf" => {
            match pdf_extract::extract_text(path) {
                Ok(t) if !t.trim().is_empty() => Some(t),
                Ok(_) => { info!("PDF {:?} yielded no text, skipping", path); None }
                Err(e) => { error!("PDF extract {:?}: {}", path, e); None }
            }
        }
        "jpg" | "jpeg" | "png" | "gif" | "exe" | "dll" | "so" | "dylib" | "zip" | "tar" | "gz" | "bin" | "class" | "pyc" => {
            info!("Skipping known binary format: {:?}", path);
            None
        }
        _ => {
            let buf = match std::fs::read(path) {
                Ok(b) => b,
                Err(e) => { error!("Read {:?}: {}", path, e); return None; }
            };
            if buf.iter().take(1024).any(|&b| b == 0) {
                 info!("Skipping likely binary file: {:?}", path);
                 None
            } else {
                 match String::from_utf8(buf) {
                     Ok(t) if !t.trim().is_empty() => Some(t),
                     _ => None,
                 }
            }
        }
    }
}

/// Chunk text into overlapping 200-word windows (50-word stride).
fn chunk_text(text: &str) -> Vec<String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() { return vec![]; }

    const CHUNK:   usize = 200;
    const OVERLAP: usize = 50;
    const STEP:    usize = CHUNK - OVERLAP;

    let mut chunks = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let end = (i + CHUNK).min(words.len());
        chunks.push(words[i..end].join(" "));
        if end == words.len() { break; }
        i += STEP;
    }
    chunks
}

// ─── Main ────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let data_dir = dirs::home_dir()
        .context("Cannot determine home directory")?
        .join(".omitfs_data");

    let raw_dir = data_dir.join("raw");

    // Ensure at minimum data_dir exists before logging starts
    if !data_dir.exists() {
        std::fs::create_dir_all(&data_dir)
            .context("Failed to create ~/.omitfs_data")?;
    }

    let _log_guard = setup_logging(&data_dir)?;

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
            println!("    Drop files into: {:?}", raw_dir);
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
            // Keep watcher alive for the duration of the daemon
            let _watcher = watcher::start_watcher(&raw_dir, tx)?;

            while let Some(event) = rx.recv().await {
                if !event.kind.is_create() && !event.kind.is_modify() {
                    continue;
                }
                for path in event.paths {
                    if !path.is_file() { continue; }

                    let Some(text) = extract_text(&path) else { continue; };
                    let chunks = chunk_text(&text);

                    let filename  = path.file_name().unwrap_or_default()
                                       .to_string_lossy().to_string();
                    let phys_path = path.to_string_lossy().to_string();

                    let mut eng = match engine.lock() {
                        Ok(guard) => guard,
                        Err(poisoned) => poisoned.into_inner(),
                    };
                    let mut n_ok = 0usize;

                    for chunk in &chunks {
                        match eng.embed(chunk) {
                            Ok(vec) => {
                                let id = uuid::Uuid::new_v4().to_string();
                                match db.insert_file(&id, &filename, &phys_path, vec).await {
                                    Ok(_)  => n_ok += 1,
                                    Err(e) => error!("LanceDB insert: {}", e),
                                }
                            }
                            Err(e) => error!("Embed chunk from {:?}: {}", path, e),
                        }
                    }
                    info!("Ingested {:?} → {} chunks", path, n_ok);
                    println!("📄  Ingested {} → {} chunk(s)", filename, n_ok);
                }
            }
        }

        // ── mount ─────────────────────────────────────────────────────────────
        Commands::Mount { mount_point } => {
            info!("omitfs mount {:?}", mount_point);

            if !mount_point.exists() {
                std::fs::create_dir_all(&mount_point)
                    .context("Failed to create mount point")?;
            }

            let db_path = data_dir.join("lancedb");
            let db      = Arc::new(OmitDb::init(db_path).await?);
            let engine  = Arc::new(Mutex::new(EmbeddingEngine::new()?));
            let fs      = OmitFs::new(db, engine, raw_dir);

            println!("🌌  Mounting OmitFS at {:?}", mount_point);
            println!("    Navigate with: cd \"{}\"/your intent here\"\"", mount_point.display());

            let options = vec![
                fuser::MountOption::FSName("omitfs".to_string()),
                fuser::MountOption::AutoUnmount,
                fuser::MountOption::AllowOther,
            ];
            fuser::mount2(fs, &mount_point, &options)
                .context("FUSE mount failed")?;
        }

        // ── select ────────────────────────────────────────────────────────────
        Commands::Select { query } => {
            let db_path = data_dir.join("lancedb");
            let db      = Arc::new(OmitDb::init(db_path).await?);
            let mut engine = EmbeddingEngine::new()?;

            println!("\n🔍  Searching: \"{}\"\n", query);
            let vector  = engine.embed(&query)?;
            let results = db.search(vector, 10).await?;

            if results.is_empty() {
                println!("No files matched \"{}\".", query);
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
            println!("  [o]  Open        — launch with $EDITOR / xdg-open");
            println!("  [d]  Delete      — remove file permanently from the void");
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
                    let editor = std::env::var("EDITOR").unwrap_or_else(|_| {
                        if cfg!(target_os = "macos") { "open".into() }
                        else if cfg!(target_os = "windows") { "cmd".into() }
                        else { "xdg-open".into() }
                    });
                    
                    if cfg!(target_os = "windows") && std::env::var("EDITOR").is_err() {
                        std::process::Command::new("cmd")
                            .args(["/C", "start", "", phys_path])
                            .status()
                            .context("Failed to open file")?;
                    } else {
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
                        println!("Deleted.");
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
    }

    Ok(())
}
