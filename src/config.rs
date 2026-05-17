use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// User-editable configuration at `~/.omitfs_data/config.toml`.
/// All fields have sensible defaults — a missing file is fine.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Config {
    /// Max distinct files returned per search.
    #[serde(default = "default_max_results")]
    pub max_results: usize,

    /// Over-fetch multiplier for chunk deduplication.
    #[serde(default = "default_overfetch_factor")]
    pub overfetch_factor: usize,

    /// Words per embedding chunk (keep ≤ 350 for BERT 512-token safety).
    #[serde(default = "default_chunk_words")]
    pub chunk_words: usize,

    /// Overlap between consecutive chunks in words.
    #[serde(default = "default_overlap_words")]
    pub overlap_words: usize,

    /// Local Ollama base URL (no trailing slash).
    #[serde(default = "default_ollama_url")]
    pub ollama_url: String,

    /// Default Ollama model for the `ask` and `serve` commands.
    #[serde(default = "default_ollama_model")]
    pub ollama_model: String,

    /// Port for `omitfs serve`.
    #[serde(default = "default_serve_port")]
    pub serve_port: u16,

    /// Encrypt chunk text in LanceDB with AES-256-GCM.
    #[serde(default)]
    pub encryption_enabled: bool,

    /// Extract text from images via Tesseract OCR (requires tesseract on PATH).
    #[serde(default = "default_true")]
    pub ocr_enabled: bool,

    /// Transcribe audio/video via Whisper CLI (requires whisper on PATH).
    #[serde(default = "default_true")]
    pub whisper_enabled: bool,
}

fn default_max_results()      -> usize  { 10 }
fn default_overfetch_factor() -> usize  { 5  }
fn default_chunk_words()      -> usize  { 200 }
fn default_overlap_words()    -> usize  { 50  }
fn default_ollama_url()       -> String { "http://localhost:11434".into() }
fn default_ollama_model()     -> String { "llama3".into() }
fn default_serve_port()       -> u16    { 3030 }
fn default_true()             -> bool   { true }

impl Default for Config {
    fn default() -> Self {
        Self {
            max_results:        default_max_results(),
            overfetch_factor:   default_overfetch_factor(),
            chunk_words:        default_chunk_words(),
            overlap_words:      default_overlap_words(),
            ollama_url:         default_ollama_url(),
            ollama_model:       default_ollama_model(),
            serve_port:         default_serve_port(),
            encryption_enabled: false,
            ocr_enabled:        true,
            whisper_enabled:    true,
        }
    }
}

impl Config {
    pub fn load(data_dir: &Path) -> Result<Self> {
        let path = Self::path(data_dir);
        if !path.exists() {
            let cfg = Config::default();
            cfg.save(data_dir)?;
            return Ok(cfg);
        }
        let raw = std::fs::read_to_string(&path)
            .context("Failed to read config.toml")?;
        let cfg: Config = toml::from_str(&raw)
            .context("Failed to parse config.toml — delete it to regenerate defaults")?;
        Ok(cfg)
    }

    pub fn save(&self, data_dir: &Path) -> Result<()> {
        let toml_str = toml::to_string_pretty(self)
            .context("Failed to serialize config")?;
        std::fs::write(Self::path(data_dir), toml_str)
            .context("Failed to write config.toml")?;
        Ok(())
    }

    fn path(data_dir: &Path) -> PathBuf {
        data_dir.join("config.toml")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir() -> PathBuf {
        let p = std::env::temp_dir()
            .join(format!("omitfs_config_test_{}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn test_defaults_on_missing_file() {
        let dir = scratch_dir();
        let cfg = Config::load(&dir).unwrap();
        assert_eq!(cfg.max_results,      10);
        assert_eq!(cfg.overfetch_factor, 5);
        assert_eq!(cfg.chunk_words,      200);
        assert_eq!(cfg.serve_port,       3030);
        assert!(!cfg.encryption_enabled);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_save_and_reload_roundtrip() {
        let dir = scratch_dir();
        let mut cfg = Config::default();
        cfg.max_results        = 42;
        cfg.encryption_enabled = true;
        cfg.ollama_model       = "mistral".into();
        cfg.save(&dir).unwrap();

        let loaded = Config::load(&dir).unwrap();
        assert_eq!(loaded.max_results,        42);
        assert!(loaded.encryption_enabled);
        assert_eq!(loaded.ollama_model,        "mistral");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_corrupt_toml_returns_error() {
        let dir = scratch_dir();
        std::fs::write(dir.join("config.toml"), b"[[not_valid_toml").unwrap();
        assert!(Config::load(&dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
