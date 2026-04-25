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
                            let text = match std::fs::read_to_string(&path) {
                                Ok(t) => t,
                                Err(e) => {
                                    error!("Failed to read {:?}: {}", path, e);
                                    continue;
                                }
                            };
                            
                            let mut eng = engine.lock().unwrap();
                            match eng.embed(&text) {
                                Ok(vector) => {
                                    let filename = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                                    let id = uuid::Uuid::new_v4().to_string();
                                    let phys_path = path.to_string_lossy().to_string();
                                    
                                    if let Err(e) = db.insert_file(&id, &filename, &phys_path, vector).await {
                                        error!("Failed to insert to LanceDB: {}", e);
                                    } else {
                                        info!("Ingested {:?}", path);
                                    }
                                }
                                Err(e) => error!("Embedding failed for {:?}: {}", path, e),
                            }
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
    }

    Ok(())
}
