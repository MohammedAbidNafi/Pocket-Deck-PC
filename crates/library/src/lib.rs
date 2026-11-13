//! Finding the games installed on this PC: Steam, Epic Games, GOG, the Xbox app and Microsoft
//! Store, other games found among the installed programs, plus games the person adds by hand. Each scanner reads the launcher's own files and registry entries, so nothing
//! needs the launcher to be running, and nothing goes online.

pub mod covers;
pub mod epic;
pub mod gog;
pub mod installed;
pub mod steam;
pub mod steamgriddb;
pub mod vdf;
pub mod xbox;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Steam,
    Epic,
    Gog,
    Xbox,
    /// Games found among the installed programs, from no particular launcher.
    Other,
    Manual,
}

impl Source {
    pub const LAUNCHERS: [Source; 5] = [Source::Steam, Source::Epic, Source::Gog, Source::Xbox, Source::Other];
    pub const ALL: [Source; 6] = [Source::Steam, Source::Epic, Source::Gog, Source::Xbox, Source::Other, Source::Manual];

    pub fn label(self) -> &'static str {
        match self {
            Source::Steam => "Steam",
            Source::Epic => "Epic Games",
            Source::Gog => "GOG",
            Source::Xbox => "Xbox & Microsoft Store",
            Source::Other => "Other games on this PC",
            Source::Manual => "Added by you",
        }
    }
}

/// How Sunshine should start the game.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Launch {
    /// A launcher URI (`steam://…`), started detached because the launcher process outlives it.
    Detached { uri: String },
    /// A command run directly, optionally from a working folder.
    Command { cmd: String, working_dir: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Game {
    /// Stable identity: `steam:<appid>`, `epic:<AppName>`, `gog:<id>`, `xbox:<package family>`,
    /// `pc:<hash>`, `manual:<hash>`.
    pub key: String,
    pub source: Source,
    pub name: String,
    pub launch: Launch,
    /// Local box art the launcher already has, if any.
    pub art: Option<PathBuf>,
    /// Box art the launcher only knows online (Epic); downloaded once when covers are made.
    pub art_url: Option<String>,
}

/// What one launcher scan found.
#[derive(Debug, Clone, Default)]
pub struct Scan {
    pub games: Vec<Game>,
    /// The launcher itself is installed.
    pub launcher_found: bool,
}

/// Scans every launcher, then the installed programs for any other games. Takes well under a
/// second; run it off the UI thread.
pub fn scan_all() -> Vec<(Source, Scan)> {
    let mut scans = vec![
        (Source::Steam, steam::scan()),
        (Source::Epic, epic::scan()),
        (Source::Gog, gog::scan()),
        (Source::Xbox, xbox::scan()),
    ];
    let known = scans.iter().flat_map(|(_, s)| s.games.iter().map(|g| installed::normalise(&g.name))).collect();
    scans.push((Source::Other, installed::scan(&known)));
    scans
}

/// A clean display name: trims, drops trademark signs, collapses spaces.
pub fn clean_name(name: &str) -> String {
    let stripped: String = name.chars().filter(|c| !matches!(c, '™' | '®' | '©')).collect();
    stripped.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// FNV-1a, for short stable keys derived from paths.
pub fn short_hash(s: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.to_lowercase().bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

/// A game the person picked themselves (an `.exe`).
pub fn manual_game(exe: &std::path::Path, name: Option<&str>) -> Game {
    let fallback = exe.file_stem().map(|s| s.to_string_lossy().replace(['_', '-'], " ")).unwrap_or_default();
    let name = clean_name(name.unwrap_or(&fallback));
    Game {
        key: format!("manual:{}", short_hash(&exe.to_string_lossy())),
        source: Source::Manual,
        name,
        launch: Launch::Command {
            cmd: format!("\"{}\"", exe.display()),
            working_dir: exe.parent().map(|p| p.display().to_string()),
        },
        art: None,
        art_url: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_names() {
        assert_eq!(clean_name("  DOOM™   Eternal "), "DOOM Eternal");
        assert_eq!(clean_name("Tom Clancy's® Rainbow Six"), "Tom Clancy's Rainbow Six");
    }

    #[test]
    fn manual_games_are_stable() {
        let a = manual_game(std::path::Path::new(r"C:\Games\Cool_Game\cool-game.exe"), None);
        let b = manual_game(std::path::Path::new(r"c:\games\cool_game\COOL-GAME.exe"), None);
        assert_eq!(a.key, b.key);
        assert_eq!(a.name, "cool game");
        assert_eq!(
            a.launch,
            Launch::Command { cmd: r#""C:\Games\Cool_Game\cool-game.exe""#.into(), working_dir: Some(r"C:\Games\Cool_Game".into()) }
        );
    }
}
