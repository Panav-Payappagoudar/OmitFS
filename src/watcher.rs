use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::Path;
use tokio::sync::mpsc::Sender;
use tracing::{error, info};

/// Spawn a native OS filesystem watcher on `raw_dir`.
/// All events are forwarded through the provided tokio mpsc sender.
pub fn start_watcher(
    raw_dir: &Path,
    tx: Sender<Event>,
) -> anyhow::Result<RecommendedWatcher> {
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        match res {
            Ok(event)  => { let _ = tx.blocking_send(event); }
            Err(e)     => error!("watcher error: {}", e),
        }
    })?;

    watcher.watch(raw_dir, RecursiveMode::Recursive)?;
    info!("File watcher active on {:?}", raw_dir);

    Ok(watcher)
}
