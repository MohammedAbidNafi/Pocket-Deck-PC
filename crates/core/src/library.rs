//! The person's choices about their game library, stored in `%APPDATA%\PocketDeck\library.json`:
//! which games or launchers to leave off the phone, games they added by hand, and which
//! Sunshine apps PocketDeck created (so it only ever changes those).

use pocketdeck_library::{Game, Source};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ManualGame {
    pub exe: PathBuf,
    pub name: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct LibraryState {
    /// Game keys turned off.
    pub hidden: BTreeSet<String>,
    /// Launchers turned off as a whole.
    pub sources_off: BTreeSet<Source>,
    pub manual: Vec<ManualGame>,
    /// Game key → name of the Sunshine app PocketDeck made for it.
    pub managed: BTreeMap<String, String>,
}

impl LibraryState {
    pub fn path() -> PathBuf {
        crate::paths::config_dir().join("library.json")
    }

    pub fn load() -> Self {
        Self::load_from(&Self::path())
    }

    pub fn load_from(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> io::Result<()> {
        self.save_to(&Self::path())
    }

    pub fn save_to(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self).map_err(io::Error::other)?)?;
        std::fs::rename(&tmp, path)
    }

    /// Whether `game` should be on the phone.
    pub fn wants(&self, game: &Game) -> bool {
        !self.hidden.contains(&game.key) && !self.sources_off.contains(&game.source)
    }

    pub fn manual_games(&self) -> Vec<Game> {
        self.manual
            .iter()
            .map(|m| pocketdeck_library::manual_game(&m.exe, Some(&m.name)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pocketdeck_library::Launch;

    fn game(key: &str, source: Source) -> Game {
        Game { key: key.into(), source, name: key.into(), launch: Launch::Detached { uri: "x".into() }, art: None, art_url: None }
    }

    #[test]
    fn hides_games_and_launchers() {
        let mut s = LibraryState::default();
        assert!(s.wants(&game("steam:1", Source::Steam)));
        s.hidden.insert("steam:1".into());
        assert!(!s.wants(&game("steam:1", Source::Steam)));
        s.sources_off.insert(Source::Epic);
        assert!(!s.wants(&game("epic:a", Source::Epic)));
        assert!(s.wants(&game("steam:2", Source::Steam)));
    }

    #[test]
    fn round_trips() {
        let path = std::env::temp_dir().join(format!("pocketdeck-lib-{}", std::process::id())).join("library.json");
        let mut s = LibraryState::default();
        s.hidden.insert("steam:730".into());
        s.sources_off.insert(Source::Gog);
        s.manual.push(ManualGame { exe: r"C:\Games\x.exe".into(), name: "X".into() });
        s.managed.insert("steam:413150".into(), "Stardew Valley".into());
        s.save_to(&path).unwrap();
        assert_eq!(LibraryState::load_from(&path), s);
    }
}
