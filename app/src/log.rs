//! A tiny log: `%LOCALAPPDATA%\PocketDeck\logs\pocketdeck.log`, plus stderr in debug builds.

use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_BYTES: u64 = 512 * 1024;

pub fn line(message: impl AsRef<str>) {
    let message = message.as_ref();
    if cfg!(debug_assertions) {
        eprintln!("{message}");
    }
    let dir = pocketdeck_core::paths::log_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join("pocketdeck.log");
    // Start over when the file gets big; this log is for recent trouble only.
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_BYTES) {
        let _ = std::fs::rename(&path, dir.join("pocketdeck.old.log"));
    }
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "[{secs}] {message}");
    }
}
