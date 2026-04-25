use anyhow::Result;
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::Path;
use tokio::sync::mpsc;
use tracing::{error, info};

pub fn start_watcher(raw_dir: &Path, tx: mpsc::UnboundedSender<Event>) -> Result<RecommendedWatcher> {
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        match res {
            Ok(event) => {
                if let Err(e) = tx.send(event) {
                    error!("Failed to send watch event: {:?}", e);
                }
            }
            Err(e) => error!("watch error: {:?}", e),
        }
    })?;

    watcher.watch(raw_dir, RecursiveMode::Recursive)?;
    info!("Watching {:?} for new files...", raw_dir);
    
    Ok(watcher)
}
