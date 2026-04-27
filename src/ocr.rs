/// Tesseract OCR wrapper — extracts text from images locally.
///
/// Gracefully returns `None` if Tesseract is not installed.
/// Install Tesseract: https://tesseract-ocr.github.io/tessdoc/Installation.html

use std::path::Path;
use tracing::{info, warn};

/// Extract text from an image file using the system `tesseract` binary.
/// Returns `None` if Tesseract is unavailable or extraction produces no text.
pub fn extract_image_text(path: &Path) -> Option<String> {
    // Quick availability check
    if std::process::Command::new("tesseract")
        .arg("--version")
        .output()
        .is_err()
    {
        info!("Tesseract not on PATH — skipping OCR for {:?}", path);
        return None;
    }

    // Tesseract appends `.txt` to the output base name automatically
    let tmp_base = std::env::temp_dir().join("omitfs_ocr_out");
    let tmp_txt  = std::env::temp_dir().join("omitfs_ocr_out.txt");
    let _        = std::fs::remove_file(&tmp_txt); // clean stale output

    let result = std::process::Command::new("tesseract")
        .arg(path.as_os_str())
        .arg(&tmp_base)
        .arg("-l").arg("eng")
        .output();

    match result {
        Ok(out) if out.status.success() => {
            let text = std::fs::read_to_string(&tmp_txt).unwrap_or_default();
            let _    = std::fs::remove_file(&tmp_txt);
            if text.trim().is_empty() { None } else { Some(text) }
        }
        Ok(out) => {
            warn!(
                "Tesseract failed for {:?}: {}",
                path,
                String::from_utf8_lossy(&out.stderr)
            );
            None
        }
        Err(e) => {
            warn!("Tesseract spawn error: {}", e);
            None
        }
    }
}
