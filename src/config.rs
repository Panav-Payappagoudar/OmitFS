use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// User-editable configuration stored in `~/.omitfs_data/config.toml`.
/// All fields have sensible defaults so a missing file is fine.
#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    /// Maximum number of distinct files returned by a semantic search.
    #[serde(default = "default_max_results")]
    pub max_results: usize,

    /// Over-fetch multiplier for deduplication (max_results × factor raw rows pulled).
    #[serde(default = "default_overfetch_factor")]
    pub overfetch_factor: usize,

    /// Word count per text chunk sent to the embedding model (≤ 350 is safe for BERT 512-token limit).
    #[serde(default = "default_chunk_words")]
    pub chunk_words: usize,

    /// Overlap between consecutive chunks in words.
    #[serde(default = "default_overlap_words")]
    pub overlap_words: usize,
}

fn default_max_results()      -> usize { 10 }
fn default_overfetch_factor() -> usize { 5  }
fn default_chunk_words()      -> usize { 200 }
fn default_overlap_words()    -> usize { 50  }

impl Default for Config {
    fn default() -> Self {
        Self {
            max_results:      default_max_results(),
            overfetch_factor: default_overfetch_factor(),
            chunk_words:      default_chunk_words(),
            overlap_words:    default_overlap_words(),
        }
    }
}

impl Config {
    /// Load config from `data_dir/config.toml`. Falls back to defaults if missing.
    pub fn load(data_dir: &Path) -> Result<Self> {
        let path = Self::path(data_dir);
        if !path.exists() {
            let cfg = Config::default();
            cfg.save(data_dir)?;   // write defaults on first run
            return Ok(cfg);
        }
        let raw = std::fs::read_to_string(&path)
            .context("Failed to read config.toml")?;
        let cfg: Config = toml::from_str(&raw)
            .context("Failed to parse config.toml — delete it to regenerate defaults")?;
        Ok(cfg)
    }

    /// Persist the current config to disk.
    pub fn save(&self, data_dir: &Path) -> Result<()> {
        let path = Self::path(data_dir);
        let toml_str = toml::to_string_pretty(self)
            .context("Failed to serialize config")?;
        std::fs::write(&path, toml_str)
            .context("Failed to write config.toml")?;
        Ok(())
    }

    fn path(data_dir: &Path) -> PathBuf {
        data_dir.join("config.toml")
    }
}
