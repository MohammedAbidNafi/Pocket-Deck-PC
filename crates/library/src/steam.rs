//! Steam: every library folder in `steamapps/libraryfolders.vdf`, every fully installed game in
//! its `appmanifest_*.acf` files, and box art from Steam's own `appcache/librarycache`.

use crate::{Game, Launch, Scan, Source, clean_name, vdf};
use std::path::{Path, PathBuf};
use winreg::RegKey;
use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY};

/// Steam's own tools that show up as installed apps but aren't games.
const NOT_GAMES: &[u32] = &[
    228980,  // Steamworks Common Redistributables
    250820,  // SteamVR
    1070560, // Steam Linux Runtime
    1391110, // Steam Linux Runtime - Soldier
    1628350, // Steam Linux Runtime - Sniper
    1493710, // Proton Experimental
    2180100, // Proton Hotfix
    1826330, // Proton EasyAntiCheat Runtime
    1161040, // Proton BattlEye Runtime
];
const NOT_GAME_PREFIXES: &[&str] = &["Proton ", "Steam Linux Runtime", "Steamworks ", "SteamVR"];

/// `StateFlags` bit meaning "fully installed".
const FULLY_INSTALLED: u32 = 4;

pub fn scan() -> Scan {
    let Some(root) = steam_root() else { return Scan::default() };
    let mut games = Vec::new();
    for library in libraries(&root) {
        let apps = library.join("steamapps");
        let Ok(entries) = std::fs::read_dir(&apps) else { continue };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            if !(name.starts_with("appmanifest_") && name.ends_with(".acf")) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(entry.path()) else { continue };
            let Some(m) = parse_manifest(&text) else { continue };
            if !m.is_game() || !apps.join("common").join(&m.install_dir).is_dir() {
                continue;
            }
            if games.iter().any(|g: &Game| g.key == m.key()) {
                continue;
            }
            games.push(Game {
                key: m.key(),
                source: Source::Steam,
                name: clean_name(&m.name),
                launch: Launch::Detached { uri: format!("steam://rungameid/{}", m.app_id) },
                art: box_art(&root, m.app_id),
                art_url: None,
            });
        }
    }
    games.sort_by_key(|g| g.name.to_lowercase());
    Scan { games, launcher_found: true }
}

fn steam_root() -> Option<PathBuf> {
    let from_user = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(r"Software\Valve\Steam", KEY_READ)
        .and_then(|k| k.get_value::<String, _>("SteamPath"))
        .ok();
    let from_machine = || {
        RegKey::predef(HKEY_LOCAL_MACHINE)
            .open_subkey_with_flags(r"SOFTWARE\Valve\Steam", KEY_READ | KEY_WOW64_32KEY)
            .and_then(|k| k.get_value::<String, _>("InstallPath"))
            .ok()
    };
    from_user
        .or_else(from_machine)
        .map(|p| PathBuf::from(p.replace('/', "\\")))
        .filter(|p| p.join("steamapps").is_dir())
}

/// The main Steam folder plus every extra library folder.
pub fn libraries(root: &Path) -> Vec<PathBuf> {
    let mut out = vec![root.to_path_buf()];
    if let Ok(text) = std::fs::read_to_string(root.join("steamapps").join("libraryfolders.vdf")) {
        for path in parse_library_folders(&text) {
            if !out.iter().any(|p| same_path(p, &path)) {
                out.push(path);
            }
        }
    }
    out
}

pub fn parse_library_folders(text: &str) -> Vec<PathBuf> {
    let doc = vdf::parse(text);
    let Some(root) = doc.block("libraryfolders") else { return Vec::new() };
    root.blocks()
        .filter_map(|(_, folder)| folder.text("path"))
        // Very old files use plain `"1" "D:\\SteamLibrary"` lines.
        .chain(root.texts().into_iter().filter(|(k, _)| k.parse::<u32>().is_ok()).map(|(_, v)| v))
        .map(PathBuf::from)
        .collect()
}

#[derive(Debug, PartialEq, Eq)]
pub struct Manifest {
    pub app_id: u32,
    pub name: String,
    pub install_dir: String,
    pub state_flags: u32,
}

impl Manifest {
    pub fn key(&self) -> String {
        format!("steam:{}", self.app_id)
    }

    pub fn is_game(&self) -> bool {
        self.state_flags & FULLY_INSTALLED != 0
            && !NOT_GAMES.contains(&self.app_id)
            && !NOT_GAME_PREFIXES.iter().any(|p| self.name.starts_with(p))
            && !self.name.trim().is_empty()
    }
}

pub fn parse_manifest(text: &str) -> Option<Manifest> {
    let doc = vdf::parse(text);
    let app = doc.block("AppState")?;
    Some(Manifest {
        app_id: app.text("appid")?.parse().ok()?,
        name: app.text("name")?.to_string(),
        install_dir: app.text("installdir")?.to_string(),
        state_flags: app.text("StateFlags").and_then(|s| s.parse().ok()).unwrap_or(0),
    })
}

/// Art from Steam's cache, best first: the portrait `library_600x900.jpg`, then the wide header
/// images (the cover maker turns those into a portrait cover). Files sit in
/// `librarycache/<id>/` (sometimes one folder deeper) or, in older Steam, `librarycache/<id>_<name>`.
pub fn box_art(root: &Path, app_id: u32) -> Option<PathBuf> {
    let cache = root.join("appcache").join("librarycache");
    let dir = cache.join(app_id.to_string());
    let mut dirs = vec![dir.clone()];
    if let Ok(entries) = std::fs::read_dir(&dir) {
        dirs.extend(entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()));
    }
    for name in ["library_600x900.jpg", "library_capsule.jpg", "library_header.jpg", "header.jpg"] {
        if let Some(found) = dirs.iter().map(|d| d.join(name)).find(|p| p.is_file()) {
            return Some(found);
        }
        let flat = cache.join(format!("{app_id}_{name}"));
        if flat.is_file() {
            return Some(flat);
        }
    }
    None
}

fn same_path(a: &Path, b: &Path) -> bool {
    a.to_string_lossy().trim_end_matches('\\').eq_ignore_ascii_case(b.to_string_lossy().trim_end_matches('\\'))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FOLDERS: &str = include_str!("../tests/fixtures/libraryfolders.vdf");
    const MANIFEST: &str = include_str!("../tests/fixtures/appmanifest_1086940.acf");

    #[test]
    fn reads_library_folders() {
        assert_eq!(
            parse_library_folders(FOLDERS),
            vec![PathBuf::from(r"C:\Program Files (x86)\Steam"), PathBuf::from(r"D:\SteamLibrary")]
        );
    }

    #[test]
    fn reads_app_manifests() {
        let m = parse_manifest(MANIFEST).unwrap();
        assert_eq!(
            m,
            Manifest { app_id: 1086940, name: "Baldur's Gate 3".into(), install_dir: "Baldurs Gate 3".into(), state_flags: 4 }
        );
        assert!(m.is_game());
        assert_eq!(m.key(), "steam:1086940");
    }

    #[test]
    fn skips_tools_and_unfinished_installs() {
        let tool = Manifest { app_id: 228980, name: "Steamworks Common Redistributables".into(), install_dir: "x".into(), state_flags: 4 };
        assert!(!tool.is_game());
        let proton = Manifest { app_id: 3658110, name: "Proton 10.0".into(), install_dir: "x".into(), state_flags: 4 };
        assert!(!proton.is_game());
        let downloading = Manifest { app_id: 1, name: "Some Game".into(), install_dir: "x".into(), state_flags: 1026 };
        assert!(!downloading.is_game());
        let updating = Manifest { app_id: 1, name: "Some Game".into(), install_dir: "x".into(), state_flags: 6 };
        assert!(updating.is_game());
    }
}
