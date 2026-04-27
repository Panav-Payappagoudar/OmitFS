/// Whisper CLI wrapper — transcribes audio/video to text locally.
///
/// Works with openai-whisper (Python) or whisper.cpp.
/// Install whisper: `pip install openai-whisper`
/// Install whisper.cpp: https://github.com/ggerganov/whisper.cpp
///
/// Gracefully returns None if neither is installed.

use std::path::Path;
use tracing::{info, warn};

/// Transcribe an audio or video file. Returns raw transcript text or None.
pub fn transcribe(path: &Path) -> Option<String> {
    // Find whichever whisper binary is available
    let cmd = ["whisper", "whisper-cpp", "main"]
        .iter()
        .find(|&&c| std::process::Command::new(c).arg("--help").output().is_ok())
        .copied();

    let Some(cmd) = cmd else {
        info!("Whisper not installed — skipping transcription for {:?}", path);
        return None;
    };

    let tmp_dir  = std::env::temp_dir();
    let stem     = path.file_stem()?.to_string_lossy().to_string();
    let txt_path = tmp_dir.join(format!("{stem}.txt"));
    let _ = std::fs::remove_file(&txt_path);

    let out = std::process::Command::new(cmd)
        .arg(path.as_os_str())
        .arg("--output_format").arg("txt")
        .arg("--output_dir").arg(&tmp_dir)
        .arg("--model").arg("base") // base is fast & accurate enough for indexing
        .output();

    match out {
        Ok(o) if o.status.success() => {
            let text = std::fs::read_to_string(&txt_path).unwrap_or_default();
            let _ = std::fs::remove_file(&txt_path);
            if text.trim().is_empty() { None } else { Some(text) }
        }
        Ok(o) => {
            warn!("Whisper error for {:?}: {}", path, String::from_utf8_lossy(&o.stderr));
            None
        }
        Err(e) => {
            warn!("Whisper spawn failed: {}", e);
            None
        }
    }
}
