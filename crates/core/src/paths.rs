//! Where PocketDeck keeps its files.

use std::path::PathBuf;

const APP_DIR: &str = "PocketDeck";

/// Set to a folder to keep all of PocketDeck's files there instead (for development and tests).
pub const HOME_OVERRIDE: &str = "POCKETDECK_HOME";

/// Roaming settings folder: `%APPDATA%\PocketDeck`.
pub fn config_dir() -> PathBuf {
    home_override().unwrap_or_else(|| base("APPDATA").join(APP_DIR))
}

/// Machine-local cache and downloads: `%LOCALAPPDATA%\PocketDeck`.
pub fn local_dir() -> PathBuf {
    home_override().map(|h| h.join("local")).unwrap_or_else(|| base("LOCALAPPDATA").join(APP_DIR))
}

fn home_override() -> Option<PathBuf> {
    std::env::var_os(HOME_OVERRIDE).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// Log files: `%LOCALAPPDATA%\PocketDeck\logs`.
pub fn log_dir() -> PathBuf {
    local_dir().join("logs")
}

fn base(var: &str) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}
