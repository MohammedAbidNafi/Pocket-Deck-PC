//! GOG: installed games register themselves under `HKLM\SOFTWARE\WOW6432Node\GOG.com\Games`.

use crate::{Game, Launch, Scan, Source, clean_name};
use std::collections::HashMap;
use std::path::Path;
use winreg::RegKey;
use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY};

pub fn scan() -> Scan {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let launcher_found = hklm.open_subkey_with_flags(r"SOFTWARE\GOG.com", KEY_READ | KEY_WOW64_32KEY).is_ok();
    let Ok(root) = hklm.open_subkey_with_flags(r"SOFTWARE\GOG.com\Games", KEY_READ | KEY_WOW64_32KEY) else {
        return Scan { games: Vec::new(), launcher_found };
    };
    let mut games: Vec<Game> = root
        .enum_keys()
        .flatten()
        .filter_map(|id| {
            let key = root.open_subkey_with_flags(&id, KEY_READ | KEY_WOW64_32KEY).ok()?;
            let values: HashMap<String, String> = key
                .enum_values()
                .flatten()
                .map(|(name, v)| (name.to_lowercase(), v.to_string().trim_matches('"').to_string()))
                .collect();
            from_values(&id, &values, |p| p.is_file())
        })
        .collect();
    games.sort_by_key(|g| g.name.to_lowercase());
    Scan { games, launcher_found: true }
}

/// One registry entry (value names lower-cased) to a game; `None` for DLC or a missing exe.
pub fn from_values(id: &str, v: &HashMap<String, String>, exists: impl Fn(&Path) -> bool) -> Option<Game> {
    let name = v.get("gamename").filter(|s| !s.trim().is_empty())?;
    let exe = v.get("exe").filter(|s| !s.trim().is_empty())?;
    if v.get("dependson").is_some_and(|d| !d.trim().is_empty()) || !exists(Path::new(exe)) {
        return None;
    }
    let working_dir = v
        .get("workingdir")
        .or_else(|| v.get("path"))
        .filter(|s| !s.trim().is_empty())
        .cloned();
    Some(Game {
        key: format!("gog:{}", v.get("gameid").map(String::as_str).unwrap_or(id)),
        source: Source::Gog,
        name: clean_name(name),
        launch: Launch::Command { cmd: format!("\"{exe}\""), working_dir },
        art: None,
        art_url: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_lowercase(), v.to_string())).collect()
    }

    #[test]
    fn reads_a_game() {
        let v = entry(&[
            ("gameID", "1207658924"),
            ("gameName", "The Witcher® 3: Wild Hunt"),
            ("exe", r"D:\GOG Games\The Witcher 3\bin\x64\witcher3.exe"),
            ("workingDir", r"D:\GOG Games\The Witcher 3\bin\x64"),
            ("path", r"D:\GOG Games\The Witcher 3"),
        ]);
        let g = from_values("1207658924", &v, |_| true).unwrap();
        assert_eq!(g.key, "gog:1207658924");
        assert_eq!(g.name, "The Witcher 3: Wild Hunt");
        assert_eq!(
            g.launch,
            Launch::Command {
                cmd: r#""D:\GOG Games\The Witcher 3\bin\x64\witcher3.exe""#.into(),
                working_dir: Some(r"D:\GOG Games\The Witcher 3\bin\x64".into())
            }
        );
    }

    #[test]
    fn skips_dlc_and_missing() {
        let v = entry(&[("gameName", "Extra"), ("exe", r"C:\x.exe"), ("dependsOn", "1207658924")]);
        assert!(from_values("1", &v, |_| true).is_none());
        let v = entry(&[("gameName", "Gone"), ("exe", r"C:\gone.exe")]);
        assert!(from_values("2", &v, |_| false).is_none());
    }
}
