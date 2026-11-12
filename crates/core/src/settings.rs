//! User settings, stored as JSON in `%APPDATA%\PocketDeck\settings.json`.
//!
//! Names and values mirror the Android app's settings where both apps have the same setting,
//! so the phone and the PC can share them later.

use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

/// Tile shape, as on the phone (`TileShape` in `SettingsRepository.kt`).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TileShape {
    Round,
    Square,
    #[default]
    Rectangle,
}

/// Tile size, as on the phone (`TileSize` in `SettingsRepository.kt`).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TileSize {
    Small,
    #[default]
    Medium,
    Large,
}

impl TileShape {
    pub const ALL: [TileShape; 3] = [TileShape::Round, TileShape::Square, TileShape::Rectangle];

    pub fn index(self) -> i32 {
        Self::ALL.iter().position(|s| *s == self).unwrap_or(2) as i32
    }

    pub fn from_index(i: i32) -> Self {
        Self::ALL.get(i as usize).copied().unwrap_or_default()
    }
}

impl TileSize {
    pub const ALL: [TileSize; 3] = [TileSize::Small, TileSize::Medium, TileSize::Large];

    pub fn index(self) -> i32 {
        Self::ALL.iter().position(|s| *s == self).unwrap_or(1) as i32
    }

    pub fn from_index(i: i32) -> Self {
        Self::ALL.get(i as usize).copied().unwrap_or_default()
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    /// First-run setup has been completed.
    pub onboarding_done: bool,
    /// Setup step to resume at if PocketDeck was closed during setup.
    pub onboarding_step: u8,
    /// Index into [`crate::ACCENTS`].
    pub accent_index: u8,
    pub tile_shape: TileShape,
    pub tile_size: TileSize,
    /// Start PocketDeck (in the tray) when you sign in to Windows.
    pub start_with_windows: bool,
    /// Closing the window keeps PocketDeck running in the tray, so your phone can still connect.
    pub close_to_tray: bool,
    /// Console-style UI sounds and the startup chime, as on the phone (`uiSounds` there).
    pub ui_sounds: bool,
    /// PocketDeck installed the streaming engine (Sunshine) itself, so the UI never names it.
    pub engine_by_pocketdeck: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            onboarding_done: false,
            onboarding_step: 0,
            accent_index: 0,
            tile_shape: TileShape::default(),
            tile_size: TileSize::default(),
            start_with_windows: true,
            close_to_tray: true,
            ui_sounds: true,
            engine_by_pocketdeck: false,
        }
    }
}

impl Settings {
    pub fn path() -> PathBuf {
        crate::paths::config_dir().join("settings.json")
    }

    /// Loads settings, falling back to defaults when the file is missing or unreadable.
    pub fn load() -> Self {
        Self::load_from(&Self::path())
    }

    pub fn load_from(path: &Path) -> Self {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice::<Settings>(&bytes)
                .map(Settings::sanitized)
                .unwrap_or_default(),
            Err(_) => Settings::default(),
        }
    }

    pub fn save(&self) -> io::Result<()> {
        self.save_to(&Self::path())
    }

    /// Writes to a temporary file first, then swaps it in, so a crash never leaves half a file.
    pub fn save_to(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        let json = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, path)
    }

    fn sanitized(mut self) -> Self {
        if self.accent_index as usize >= crate::ACCENTS.len() {
            self.accent_index = 0;
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pocketdeck-test-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("settings.json")
    }

    #[test]
    fn round_trips() {
        let path = temp_file("roundtrip");
        let s = Settings {
            onboarding_done: true,
            onboarding_step: 2,
            accent_index: 3,
            tile_shape: TileShape::Round,
            tile_size: TileSize::Large,
            start_with_windows: false,
            close_to_tray: false,
            ui_sounds: false,
            engine_by_pocketdeck: true,
        };
        s.save_to(&path).unwrap();
        assert_eq!(Settings::load_from(&path), s);
    }

    #[test]
    fn missing_file_gives_defaults() {
        let path = temp_file("missing").with_file_name("nope.json");
        assert_eq!(Settings::load_from(&path), Settings::default());
    }

    #[test]
    fn unknown_and_missing_fields_are_tolerated() {
        let path = temp_file("partial");
        std::fs::write(&path, br#"{"accentIndex": 2, "tileShape": "SQUARE", "somethingNew": 1}"#).unwrap();
        let s = Settings::load_from(&path);
        assert_eq!(s.accent_index, 2);
        assert_eq!(s.tile_shape, TileShape::Square);
        assert_eq!(s.tile_size, TileSize::Medium);
        assert!(s.close_to_tray);
    }

    #[test]
    fn out_of_range_accent_resets() {
        let path = temp_file("accent");
        std::fs::write(&path, br#"{"accentIndex": 40}"#).unwrap();
        assert_eq!(Settings::load_from(&path).accent_index, 0);
    }

    #[test]
    fn enum_names_match_android() {
        assert_eq!(serde_json::to_string(&TileShape::Rectangle).unwrap(), "\"RECTANGLE\"");
        assert_eq!(serde_json::to_string(&TileSize::Small).unwrap(), "\"SMALL\"");
        assert_eq!(TileShape::from_index(TileShape::Round.index()), TileShape::Round);
        assert_eq!(TileSize::from_index(9), TileSize::Medium);
    }
}
