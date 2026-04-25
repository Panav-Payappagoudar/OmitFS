pub mod db;
pub mod embedding;
pub mod fuse;
pub mod watcher;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tracing::{info, Level, error};

use db::OmitDb;
use embedding::EmbeddingEngine;
use fuse::OmitFs;

/// OmitFS: Zero-dependency semantic file system
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Initializes the hidden storage folder and LanceDB database
    Init,
    /// Starts the background file watcher and ingestion pipeline
    Daemon,
    /// Mounts the FUSE virtual file system to the specified directory
    Mount {
        /// The mount point directory
        mount_point: PathBuf,
    },
    /// Interactive file manager: search, open, copy, move, delete
    Select {
        /// Natural language query (e.g. "my calculus notes")
        query: String,
    },
}

fn setup_logging() -> Result<tracing_appender::non_blocking::WorkerGuard> {
    let data_dir = dirs::home_dir()
        .context("Could not find home directory")?
        .join(".omitfs_data");

    if !data_dir.exists() {
        std::fs::create_dir_all(&data_dir)?;
    }

    let file_appender = tracing_appender::rolling::daily(data_dir, "omitfs.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .with_writer(non_blocking)
        .with_ansi(false)
        .init();

    Ok(guard)
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let _log_guard = setup_logging().context("Failed to initialize logging")?;

    let data_dir = dirs::home_dir().unwrap().join(".omitfs_data");
    let raw_dir = data_dir.join("raw");

    match cli.command {
        Commands::Init => {
            info!("Initializing OmitFS infrastructure");
            if !raw_dir.exists() {
                std::fs::create_dir_all(&raw_dir).context("Failed to create raw directory")?;
            }
            
            let db_path = data_dir.join("lancedb");
            let _db = OmitDb::init(db_path.clone()).await?;
            
            // Warm up / download the model
            info!("Downloading/Warming up Embedding Engine...");
            let _engine = EmbeddingEngine::new()?;
            
            println!("OmitFS initialized successfully at {:?}", data_dir);
        }
        Commands::Daemon => {
            info!("Starting OmitFS background daemon");
            println!("OmitFS daemon running. Watching for files...");
            
            let db_path = data_dir.join("lancedb");
            let db = Arc::new(OmitDb::init(db_path).await?);
            let engine = Arc::new(Mutex::new(EmbeddingEngine::new()?));

            let (tx, mut rx) = mpsc::unbounded_channel();
            let _watcher = watcher::start_watcher(&raw_dir, tx)?;

            // Background ingestion pipeline
            while let Some(event) = rx.recv().await {
                info!("File event detected: {:?}", event);
                
                // Demo ingestion logic for new files
                if event.kind.is_create() || event.kind.is_modify() {
                    for path in event.paths {
                        if path.is_file() {
                            let text = if path.extension().and_then(|s| s.to_str()) == Some("pdf") {
                                match pdf_extract::extract_text(&path) {
                                    Ok(t) => t,
                                    Err(e) => {
                                        error!("Failed to extract PDF {:?}: {}", path, e);
                                        continue;
                                    }
                                }
                            } else {
                                match std::fs::read_to_string(&path) {
                                    Ok(t) => t,
                                    Err(e) => {
                                        error!("Failed to read text {:?}: {}", path, e);
                                        continue;
                                    }
                                }
                            };
                            
                            if text.trim().is_empty() { continue; }
                            
                            // Semantic Chunking: Split into overlapping ~200 word chunks
                            let words: Vec<&str> = text.split_whitespace().collect();
                            let chunk_size = 200;
                            let overlap = 50;
                            let step = if chunk_size > overlap { chunk_size - overlap } else { chunk_size };
                            
                            let mut chunks = Vec::new();
                            let mut i = 0;
                            while i < words.len() {
                                let end = std::cmp::min(i + chunk_size, words.len());
                                chunks.push(words[i..end].join(" "));
                                if end == words.len() { break; }
                                i += step;
                            }

                            let filename = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                            let phys_path = path.to_string_lossy().to_string();

                            let mut eng = engine.lock().unwrap();
                            let mut success_count = 0;
                            
                            for chunk in chunks {
                                match eng.embed(&chunk) {
                                    Ok(vector) => {
                                        let id = uuid::Uuid::new_v4().to_string();
                                        if let Err(e) = db.insert_file(&id, &filename, &phys_path, vector).await {
                                            error!("Failed to insert chunk to LanceDB: {}", e);
                                        } else {
                                            success_count += 1;
                                        }
                                    }
                                    Err(e) => error!("Embedding failed for chunk in {:?}: {}", path, e),
                                }
                            }
                            info!("Ingested {:?} into {} semantic chunks", path, success_count);
                        }
                    }
                }
            }
        }
        Commands::Mount { mount_point } => {
            info!("Mounting FUSE filesystem to {:?}", mount_point);
            println!("Mounting FUSE at {:?}", mount_point);
            
            let db_path = data_dir.join("lancedb");
            let db = Arc::new(OmitDb::init(db_path).await?);
            let engine = Arc::new(Mutex::new(EmbeddingEngine::new()?));

            let fs = OmitFs::new(db, engine, raw_dir);
            let options = vec![
                fuser::MountOption::FSName("omitfs".to_string()),
                fuser::MountOption::AutoUnmount,
            ];
            
            fuser::mount2(fs, mount_point, &options)?;
        }

        Commands::Select { query } => {
            let db_path = data_dir.join("lancedb");
            let db = Arc::new(OmitDb::init(db_path).await?);
            let mut engine = EmbeddingEngine::new()?;

            println!("\n🔍 Searching for: \"{}\"...\n", query);
            let vector = engine.embed(&query)?;
            let results = db.search(vector, 10).await?;

            if results.is_empty() {
                println!("No files found for query: \"{}\"", query);
                return Ok(());
            }

            println!("Found {} file(s):\n", results.len());
            for (i, (name, path)) in results.iter().enumerate() {
                println!("  [{}] {}  →  {}", i + 1, name, path);
            }

            println!("\nSelect a file number (or 0 to quit): ");
            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;
            let choice: usize = input.trim().parse().unwrap_or(0);

            if choice == 0 || choice > results.len() {
                println!("Aborted.");
                return Ok(());
            }

            let (filename, phys_path) = &results[choice - 1];
            println!("\nSelected: {}  ({})", filename, phys_path);
            println!("");
            println!("What would you like to do?");
            println!("  [o] Open   — opens with $EDITOR / xdg-open");
            println!("  [d] Delete — permanently removes the file from the void");
            println!("  [p] Print path — prints the physical path to stdout");
            println!("  [c] Copy   — duplicates the file to a new location");
            println!("  [m] Move   — relocates the file to a new path");
            println!("  [q] Quit");
            println!("");
            print!("Choice: ");

            let mut action = String::new();
            std::io::stdin().read_line(&mut action)?;

            match action.trim() {
                "o" => {
                    // Open with $EDITOR if set, otherwise xdg-open / open
                    let editor = std::env::var("EDITOR").unwrap_or_else(|_| {
                        if cfg!(target_os = "macos") { "open".into() } else { "xdg-open".into() }
                    });
                    std::process::Command::new(&editor).arg(phys_path).status()?;
                }
                "d" => {
                    println!("Are you sure you want to delete \"{}\"? [y/N]: ", filename);
                    let mut confirm = String::new();
                    std::io::stdin().read_line(&mut confirm)?;
                    if confirm.trim().to_lowercase() == "y" {
                        std::fs::remove_file(phys_path)?;
                        println!("Deleted.");
                    } else {
                        println!("Aborted.");
                    }
                }
                "p" => {
                    println!("\n📂 Physical path:");
                    println!("{}", phys_path);
                }
                "c" => {
                    println!("Destination path (e.g. ~/Documents/copy.pdf): ");
                    let mut dest = String::new();
                    std::io::stdin().read_line(&mut dest)?;
                    let dest = shellexpand::tilde(dest.trim()).to_string();
                    std::fs::copy(phys_path, &dest)?;
                    println!("Copied → {}", dest);
                }
                "m" => {
                    println!("Destination path (e.g. ~/Documents/moved.pdf): ");
                    let mut dest = String::new();
                    std::io::stdin().read_line(&mut dest)?;
                    let dest = shellexpand::tilde(dest.trim()).to_string();
                    std::fs::rename(phys_path, &dest)?;
                    println!("Moved → {}", dest);
                }
                _ => println!("Quit."),
            }
        }
    }

    Ok(())
}
