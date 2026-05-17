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
///
/// Uses a unique per-call temporary directory so that concurrent Whisper
/// invocations cannot clobber each other's `{stem}.txt` output file.
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

    let stem = path.file_stem()?.to_string_lossy().to_string();

    // Create a unique output directory per call: PID + thread-id ensures no
    // two concurrent transcriptions share the same directory, even for files
    // with identical stems (e.g. notes.mp3 and backup/notes.mp4).
    let unique = format!(
        "omitfs_whisper_{}_{}",
        std::process::id(),
        format!("{:?}", std::thread::current().id())
            .replace(['(', ')', ' '], "_"),
    );
    let tmp_dir = std::env::temp_dir().join(&unique);

    if let Err(e) = std::fs::create_dir_all(&tmp_dir) {
        warn!("Failed to create Whisper tmp dir {:?}: {}", tmp_dir, e);
        return None;
    }

    // Whisper always writes `<stem>.txt` into the output directory.
    let whisper_out = tmp_dir.join(format!("{stem}.txt"));

    let out = std::process::Command::new(cmd)
        .arg(path.as_os_str())
        .arg("--output_format").arg("txt")
        .arg("--output_dir").arg(&tmp_dir)
        .arg("--model").arg("base") // base is fast & accurate enough for indexing
        .output();

    let result = match out {
        Ok(o) if o.status.success() => {
            let text = std::fs::read_to_string(&whisper_out).unwrap_or_default();
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
    };

    // Always clean up the unique tmp directory (ignore errors — OS will GC it).
    let _ = std::fs::remove_dir_all(&tmp_dir);
    result
}
