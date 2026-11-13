//! Games installed some other way (EA app, Ubisoft Connect, Battle.net, Riot, itch, a standalone
//! installer…): every program in Windows' list of installed apps (the Uninstall registry keys)
//! is checked, and the ones that look like games are kept.
//!
//! "Looks like a game" means its folder holds files only games ship (a game engine's runtime,
//! a store SDK, upscaler libraries, packed game data), or it comes from a games publisher.
//! Launchers, anti-cheat, runtimes and tools are left out by name.

use crate::{Game, Launch, Scan, Source, clean_name, short_hash};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use winreg::RegKey;
use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY};

const UNINSTALL: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall";

/// Files (lower-case) that only games ship.
const GAME_FILES: &[&str] = &[
    "unityplayer.dll",
    "gameassembly.dll",
    "steam_api.dll",
    "steam_api64.dll",
    "eossdk-win32-shipping.dll",
    "eossdk-win64-shipping.dll",
    "galaxy.dll",
    "galaxy64.dll",
    "discord_game_sdk.dll",
    "bink2w64.dll",
    "bink2w32.dll",
    "binkw32.dll",
    "binkw64.dll",
    "fmod.dll",
    "fmod64.dll",
    "fmodstudio.dll",
    "fmodstudio64.dll",
    "physx3_x64.dll",
    "physx_64.dll",
    "nvngx_dlss.dll",
    "nvngx_dlssg.dll",
    "libxess.dll",
    "amd_fidelityfx_dx12.dll",
    "d3dx9_43.dll",
    "xinput1_3.dll",
    "data.win",
];
/// File extensions (lower-case) of packed game data: Unreal, Godot, Ren'Py, Bethesda, Ubisoft.
/// (Not `.pak` on its own: Chromium and Electron apps ship those too.)
const GAME_EXTENSIONS: &[&str] = &["pck", "rpa", "bsa", "ba2", "utoc", "ucas", "forge"];
/// Folders (lower-case) of game engines' layouts.
const GAME_DIRS: &[&str] = &["paks", "renpy", "streamingassets"];

/// Publishers (lower-case substrings) whose programs are games unless the name says otherwise.
const GAME_PUBLISHERS: &[&str] = &[
    "electronic arts",
    "ubisoft",
    "blizzard",
    "riot games",
    "rockstar games",
    "bethesda",
    "cd projekt",
    "square enix",
    "bandai namco",
    "capcom",
    "sega",
    "activision",
    "2k games",
    "take-two",
    "mojang",
    "paradox interactive",
    "devolver",
    "wargaming",
    "hoyoverse",
    "cognosphere",
    "mihoyo",
    "kuro games",
    "nexon",
    "krafton",
    "level infinite",
    "innersloth",
    "re-logic",
    "warner bros. games",
    "gearbox",
    "konami",
    "frontier developments",
    "larian",
    "fromsoftware",
    "grinding gear games",
    "digital extremes",
    "smilegate",
    "jagex",
    "pearl abyss",
];

/// Words (lower-case) in names of things that aren't games.
const NOT_GAME_WORDS: &[&str] = &[
    "launcher",
    "client",
    "ea app",
    "origin",
    "ubisoft connect",
    "uplay",
    "battle.net",
    "steam",
    "epic games",
    "gog galaxy",
    "xbox",
    "anti-cheat",
    "anticheat",
    "battleye",
    "vanguard",
    "punkbuster",
    "redistributable",
    "redist",
    "runtime",
    "directx",
    "vulkan",
    "physx",
    "driver",
    "sdk",
    "update",
    "updater",
    "uninstall",
    "editor",
    "server",
    "tool",
    "benchmark",
    "overlay",
    "companion",
    "mod manager",
    "visual c++",
    ".net",
    "unity hub",
    "unreal engine",
    "godot",
];

/// Exe names (lower-case substrings) that are never the game itself.
const NOT_GAME_EXES: &[&str] = &[
    "unins", "uninstall", "setup", "install", "redist", "crash", "report", "dxsetup", "vcredist", "vc_redist",
    "prereq", "helper", "update", "cefprocess", "webhelper", "unitycrashhandler", "easyanticheat", "battleye",
    "touchup", "cleanup", "dotnet", "ue4prereq", "ueprereq", "activation", "register", "config", "settings",
];

/// One program from Windows' list of installed apps.
#[derive(Debug, Clone, Default)]
pub struct Entry {
    pub key: String,
    pub name: String,
    pub publisher: String,
    pub install_location: String,
    pub display_icon: String,
    pub uninstall_string: String,
    pub system_component: bool,
    pub has_parent: bool,
}

/// What's in a folder, as far as telling a game goes.
#[derive(Debug, Default)]
pub struct Folder {
    pub game_files: bool,
    /// Exes found, with their sizes.
    pub exes: Vec<(PathBuf, u64)>,
}

/// Scans every installed program. `known` holds the names of games other scanners already found
/// (normalised with [`normalise`]), so the same game isn't listed twice.
pub fn scan(known: &HashSet<String>) -> Scan {
    let mut seen: HashSet<String> = known.clone();
    let mut games: Vec<Game> = Vec::new();
    for entry in entries() {
        if !seen.insert(normalise(&entry.name)) {
            continue;
        }
        if let Some(game) = to_game(&entry, look_into) {
            games.push(game);
        }
    }
    games.sort_by_key(|g| g.name.to_lowercase());
    // Windows' list of apps is always there; this source is "found" so it can be switched off.
    Scan { games, launcher_found: true }
}

/// Lower-case letters and digits only, for comparing names across launchers.
pub fn normalise(name: &str) -> String {
    clean_name(name).chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

fn entries() -> Vec<Entry> {
    let roots = [
        (HKEY_LOCAL_MACHINE, KEY_WOW64_64KEY),
        (HKEY_LOCAL_MACHINE, KEY_WOW64_32KEY),
        (HKEY_CURRENT_USER, 0),
    ];
    let mut out = Vec::new();
    for (hive, view) in roots {
        let Ok(list) = RegKey::predef(hive).open_subkey_with_flags(UNINSTALL, KEY_READ | view) else { continue };
        for key_name in list.enum_keys().flatten() {
            let Ok(key) = list.open_subkey_with_flags(&key_name, KEY_READ | view) else { continue };
            let text = |name: &str| key.get_value::<String, _>(name).unwrap_or_default();
            out.push(Entry {
                key: key_name.clone(),
                name: text("DisplayName"),
                publisher: text("Publisher"),
                install_location: text("InstallLocation"),
                display_icon: text("DisplayIcon"),
                uninstall_string: text("UninstallString"),
                system_component: key.get_value::<u32, _>("SystemComponent").is_ok_and(|v| v == 1),
                has_parent: key.get_value::<String, _>("ParentKeyName").is_ok(),
            });
        }
    }
    out
}

/// Decides whether an installed program is a game, and how to start it. `look` reads a folder.
pub fn to_game(e: &Entry, look: impl Fn(&Path) -> Folder) -> Option<Game> {
    let name = clean_name(&e.name);
    if name.is_empty() || e.system_component || e.has_parent {
        return None;
    }
    // Other scanners own these.
    let location = unquote(&e.install_location);
    let lower_location = location.to_lowercase();
    if e.key.starts_with("Steam App ")
        || lower_location.contains(r"\steamapps\")
        || lower_location.contains(r"\windowsapps\")
        || e.publisher.eq_ignore_ascii_case("GOG.com")
    {
        return None;
    }
    let lower_name = name.to_lowercase();
    if NOT_GAME_WORDS.iter().any(|w| contains_word(&lower_name, w)) {
        return None;
    }
    let lower_publisher = e.publisher.to_lowercase();
    if lower_publisher.contains("microsoft") || lower_publisher.contains("nvidia") || lower_publisher.contains("advanced micro devices") {
        return None;
    }

    let icon_exe = exe_from_icon(&e.display_icon).filter(|p| p.is_file() && is_game_exe(p));
    let dir = Some(PathBuf::from(&location))
        .filter(|p| !location.is_empty() && p.is_dir())
        .or_else(|| icon_exe.as_ref().and_then(|p| p.parent().map(Path::to_path_buf)))?;
    // A whole drive or Program Files isn't one game's folder.
    if dir.components().count() < 2 || is_shared_folder(&dir) {
        return None;
    }
    let folder = look(&dir);
    let publisher_says = GAME_PUBLISHERS.iter().any(|p| lower_publisher.contains(p));
    if !folder.game_files && !publisher_says {
        return None;
    }

    // Riot's games start through the Riot Client, which also uninstalls them.
    if let Some(launch) = riot_launch(&e.uninstall_string) {
        return Some(game(e, name, launch));
    }

    let exe = icon_exe.or_else(|| pick_exe(&folder.exes, &lower_name))?;
    let working_dir = exe.parent().map(|p| p.display().to_string());
    Some(game(e, name, Launch::Command { cmd: format!("\"{}\"", exe.display()), working_dir }))
}

fn game(e: &Entry, name: String, launch: Launch) -> Game {
    Game { key: format!("pc:{}", short_hash(&e.key)), source: Source::Other, name, launch, art: None, art_url: None }
}

/// `"C:\Riot Games\Riot Client\RiotClientServices.exe" --uninstall-product=valorant --uninstall-patchline=live`
fn riot_launch(uninstall: &str) -> Option<Launch> {
    let lower = uninstall.to_lowercase();
    if !lower.contains("riotclientservices.exe") || !lower.contains("--uninstall-product=") {
        return None;
    }
    let product = uninstall.split_whitespace().find_map(|a| a.strip_prefix("--uninstall-product="))?.trim_matches('"');
    let patchline = uninstall
        .split_whitespace()
        .find_map(|a| a.strip_prefix("--uninstall-patchline="))
        .map(|p| p.trim_matches('"'))
        .unwrap_or("live");
    let exe = &uninstall[..lower.find("riotclientservices.exe")? + "riotclientservices.exe".len()];
    let exe = exe.trim_start_matches('"');
    Some(Launch::Command {
        cmd: format!("\"{exe}\" --launch-product={product} --launch-patchline={patchline}"),
        working_dir: Path::new(exe).parent().map(|p| p.display().to_string()),
    })
}

fn unquote(s: &str) -> String {
    s.trim().trim_matches('"').replace('/', "\\").trim_end_matches('\\').to_string()
}

/// `"C:\Games\X\x.exe",0` → `C:\Games\X\x.exe`, when it's an exe.
fn exe_from_icon(icon: &str) -> Option<PathBuf> {
    let icon = icon.trim();
    let path = match icon.strip_prefix('"') {
        Some(rest) => rest.split('"').next()?,
        None => icon.rsplit_once(',').filter(|(_, n)| n.trim().parse::<i32>().is_ok()).map_or(icon, |(p, _)| p),
    };
    let path = PathBuf::from(path.trim());
    path.extension().is_some_and(|x| x.eq_ignore_ascii_case("exe")).then_some(path)
}

fn is_game_exe(path: &Path) -> bool {
    let name = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    !NOT_GAME_EXES.iter().any(|w| name.contains(w))
}

fn is_shared_folder(dir: &Path) -> bool {
    let name = dir.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    matches!(name.as_str(), "program files" | "program files (x86)" | "programdata" | "appdata" | "local" | "roaming" | "programs" | "games")
}

/// The likeliest game exe: one named like the game, else the biggest.
fn pick_exe(exes: &[(PathBuf, u64)], lower_name: &str) -> Option<PathBuf> {
    let wanted = normalise(lower_name);
    let candidates: Vec<&(PathBuf, u64)> = exes.iter().filter(|(p, _)| is_game_exe(p)).collect();
    let named = candidates.iter().filter(|(p, _)| {
        let stem = p.file_stem().map(|s| normalise(&s.to_string_lossy())).unwrap_or_default();
        !stem.is_empty() && !wanted.is_empty() && (wanted.contains(&stem) || stem.contains(&wanted))
    });
    named
        .max_by_key(|(_, size)| *size)
        .or_else(|| candidates.iter().max_by_key(|(_, size)| *size))
        .map(|(p, _)| p.clone())
}

/// Matches `word` in `text` on word boundaries (so "tool" doesn't hit "Toolbox Wars").
fn contains_word(text: &str, word: &str) -> bool {
    text.match_indices(word).any(|(i, _)| {
        let before = text[..i].chars().next_back();
        let after = text[i + word.len()..].chars().next();
        !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
    })
}

/// Reads a folder up to three levels deep (enough for Unreal's `Game\Binaries\Win64`), visiting
/// at most a few hundred folders so a huge install can't stall the scan.
pub fn look_into(dir: &Path) -> Folder {
    let mut folder = Folder::default();
    let mut queue = vec![(dir.to_path_buf(), 0)];
    let mut visited = 0;
    while let Some((path, depth)) = queue.pop() {
        visited += 1;
        if visited > 300 {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&path) else { continue };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                if GAME_DIRS.contains(&name.as_str()) || (name.ends_with("_data") && entry.path().join("globalgamemanagers").is_file()) {
                    folder.game_files = true;
                }
                if depth < 3 && !name.starts_with('.') && !matches!(name.as_str(), "redist" | "_commonredist" | "directx" | "support" | "__installer" | "logs") {
                    queue.push((entry.path(), depth + 1));
                }
            } else if GAME_FILES.contains(&name.as_str())
                || name.rsplit_once('.').is_some_and(|(_, ext)| GAME_EXTENSIONS.contains(&ext))
            {
                folder.game_files = true;
            } else if name.ends_with(".exe") {
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                folder.exes.push((entry.path(), size));
            }
        }
    }
    folder
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, publisher: &str, location: &str) -> Entry {
        Entry { key: name.into(), name: name.into(), publisher: publisher.into(), install_location: location.into(), ..Default::default() }
    }

    fn folder(game_files: bool, exes: &[(&str, u64)]) -> Folder {
        Folder { game_files, exes: exes.iter().map(|(p, s)| (PathBuf::from(p), *s)).collect() }
    }

    // Uses the temp folder as an install location that exists.
    fn here() -> String {
        std::env::temp_dir().display().to_string().trim_end_matches('\\').to_string()
    }

    #[test]
    fn finds_an_engine_game() {
        let dir = here();
        let e = entry("Hollow Knight", "Team Cherry", &dir);
        let exe = format!(r"{dir}\hollow_knight.exe");
        let g = to_game(&e, |_| folder(true, &[(&exe, 600_000), (r"C:\x\UnityCrashHandler64.exe", 1_200_000)])).unwrap();
        assert_eq!(g.source, Source::Other);
        assert_eq!(g.name, "Hollow Knight");
        assert_eq!(g.launch, Launch::Command { cmd: format!("\"{exe}\""), working_dir: Some(dir) });
    }

    #[test]
    fn skips_ordinary_apps_and_launchers() {
        let dir = here();
        assert!(to_game(&entry("7-Zip", "Igor Pavlov", &dir), |_| folder(false, &[("7zFM.exe", 1)])).is_none());
        assert!(to_game(&entry("EA app", "Electronic Arts", &dir), |_| folder(true, &[("EADesktop.exe", 1)])).is_none());
        assert!(to_game(&entry("Steam App 730", "Valve", &dir), |_| folder(true, &[("cs2.exe", 1)])).is_none());
        assert!(to_game(&entry("Riot Vanguard", "Riot Games, Inc", &dir), |_| folder(false, &[("vgc.exe", 1)])).is_none());
    }

    #[test]
    fn publisher_alone_counts() {
        let dir = here();
        let g = to_game(&entry("Assassin's Creed Mirage", "Ubisoft", &dir), |_| folder(false, &[("ACMirage.exe", 90_000_000), ("ACMirage_plus.exe", 1)])).unwrap();
        assert_eq!(g.launch, Launch::Command { cmd: "\"ACMirage.exe\"".into(), working_dir: Some(String::new()) });
    }

    #[test]
    fn starts_riot_games_through_the_client() {
        let dir = here();
        let mut e = entry("VALORANT", "Riot Games, Inc", &dir);
        e.uninstall_string = r#""C:\Riot Games\Riot Client\RiotClientServices.exe" --uninstall-product=valorant --uninstall-patchline=live"#.into();
        let g = to_game(&e, |_| folder(true, &[])).unwrap();
        assert_eq!(
            g.launch,
            Launch::Command {
                cmd: r#""C:\Riot Games\Riot Client\RiotClientServices.exe" --launch-product=valorant --launch-patchline=live"#.into(),
                working_dir: Some(r"C:\Riot Games\Riot Client".into())
            }
        );
    }

    #[test]
    fn reads_icons_and_words() {
        assert_eq!(exe_from_icon(r#""C:\G\game.exe",0"#), Some(PathBuf::from(r"C:\G\game.exe")));
        assert_eq!(exe_from_icon(r"C:\G\game.exe,0"), Some(PathBuf::from(r"C:\G\game.exe")));
        assert_eq!(exe_from_icon(r"C:\G\game.ico"), None);
        assert!(contains_word("ea app", "ea app"));
        assert!(!contains_word("toolbox wars", "tool"));
        assert_eq!(normalise("DOOM™ Eternal"), "doometernal");
    }
}
