use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FileEntry {
    pub sha256:     String,
    pub indexed_at: u64,
    pub size:       u64,
}

/// Persistent manifest mapping physical path → last-indexed SHA-256 + size.
/// Stored as JSON at `~/.omitfs_data/manifest.json`.
pub struct HashManifest {
    path:    PathBuf,
    entries: HashMap<String, FileEntry>,
}

impl HashManifest {
    pub fn load(data_dir: &Path) -> Result<Self> {
        let path = data_dir.join("manifest.json");
        let entries: HashMap<String, FileEntry> = if path.exists() {
            let raw = std::fs::read_to_string(&path)
                .context("Failed to read manifest.json")?;
            serde_json::from_str(&raw).unwrap_or_default()
        } else {
            HashMap::new()
        };
        Ok(Self { path, entries })
    }

    pub fn save(&self) -> Result<()> {
        let json = serde_json::to_string_pretty(&self.entries)
            .context("Failed to serialize manifest")?;
        std::fs::write(&self.path, json)
            .context("Failed to write manifest.json")?;
        Ok(())
    }

    /// Returns `true` if the file is new or its content has changed since last index.
    pub fn is_stale(&self, file_path: &Path) -> bool {
        let key = file_path.to_string_lossy().to_string();
        let meta = match std::fs::metadata(file_path) {
            Ok(m)  => m,
            Err(_) => return true,
        };
        let Some(entry) = self.entries.get(&key) else { return true; };
        // Fast size pre-check avoids hashing unchanged large files
        if meta.len() != entry.size { return true; }
        match sha256_file(file_path) {
            Ok(h)  => h != entry.sha256,
            Err(_) => true,
        }
    }

    pub fn mark_indexed(&mut self, file_path: &Path) -> Result<()> {
        let key  = file_path.to_string_lossy().to_string();
        let meta = std::fs::metadata(file_path).context("stat failed")?;
        let hash = sha256_file(file_path)?;
        let now  = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.entries.insert(key, FileEntry { sha256: hash, indexed_at: now, size: meta.len() });
        Ok(())
    }

    pub fn remove(&mut self, file_path: &Path) {
        self.entries.remove(&file_path.to_string_lossy().to_string());
    }
}

/// Stream-hash a file in 64 KB chunks to avoid loading it entirely into RAM.
fn sha256_file(path: &Path) -> Result<String> {
    let file   = std::fs::File::open(path).context("open for hash")?;
    let mut r  = std::io::BufReader::with_capacity(65_536, file);
    let mut h  = Sha256::new();
    let mut buf = [0u8; 65_536];
    loop {
        let n = r.read(&mut buf).context("read for hash")?;
        if n == 0 { break; }
        h.update(&buf[..n]);
    }
    Ok(format!("{:x}", h.finalize()))
}
