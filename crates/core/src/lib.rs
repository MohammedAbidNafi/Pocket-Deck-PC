//! Shared, UI-free pieces of PocketDeck for PC: settings, app folders and start-with-Windows.

pub mod autostart;
pub mod library;
pub mod pairing;
pub mod paths;
pub mod settings;
pub mod sync;

pub use settings::{Settings, TileShape, TileSize};

/// Accent colours, in the same order and with the same names as the Android app
/// (`Palette.accents` in `ui/theme/Theme.kt`), so an accent index means the same colour on both.
pub const ACCENTS: [(&str, u32); 6] = [
    ("Steam blue", 0x1A9FFF),
    ("Neon red", 0xFF3D57),
    ("Electric purple", 0x9B6BFF),
    ("Mint", 0x2EE6A6),
    ("Amber", 0xFFB020),
    ("Pink", 0xFF5FC8),
];
