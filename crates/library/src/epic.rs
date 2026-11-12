//! Epic Games: one JSON `.item` manifest per installed app, in the launcher's ProgramData folder.
//! Box art isn't stored locally, but the launcher's catalogue cache (`Catalog/catcache.bin`,
//! base64-encoded JSON) lists each game's art URLs, so covers can be fetched once.

use crate::{Game, Launch, Scan, Source, clean_name};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Deserialize, Debug)]
#[serde(rename_all = "PascalCase")]
struct Item {
    display_name: String,
    app_name: String,
    catalog_namespace: String,
    catalog_item_id: String,
    install_location: String,
    #[serde(default)]
    main_game_app_name: String,
    #[serde(default)]
    app_categories: Vec<String>,
    #[serde(rename = "bIsApplication", default = "yes")]
    is_application: bool,
    #[serde(rename = "bIsIncompleteInstall", default)]
    incomplete: bool,
}

fn yes() -> bool {
    true
}

fn data_dir() -> PathBuf {
    let data = std::env::var_os("ProgramData").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
    data.join(r"Epic\EpicGamesLauncher\Data")
}

pub fn scan() -> Scan {
    let data = data_dir();
    let Ok(entries) = std::fs::read_dir(data.join("Manifests")) else { return Scan::default() };
    let art = std::fs::read_to_string(data.join("Catalog").join("catcache.bin"))
        .map(|text| parse_catalog(&text))
        .unwrap_or_default();
    let mut games: Vec<Game> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("item")))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|text| parse_item(&text, |p| p.is_dir(), &art))
        .collect();
    games.sort_by_key(|g| g.name.to_lowercase());
    games.dedup_by(|a, b| a.key == b.key);
    Scan { games, launcher_found: true }
}

#[derive(Deserialize)]
struct CatalogEntry {
    id: String,
    #[serde(rename = "keyImages", default)]
    key_images: Vec<KeyImage>,
}

#[derive(Deserialize)]
struct KeyImage {
    #[serde(rename = "type")]
    kind: String,
    url: String,
}

/// Catalogue item id → best portrait art URL.
pub fn parse_catalog(base64_text: &str) -> HashMap<String, String> {
    let Some(bytes) = decode_base64(base64_text) else { return HashMap::new() };
    let Ok(entries) = serde_json::from_slice::<Vec<CatalogEntry>>(&bytes) else { return HashMap::new() };
    entries
        .into_iter()
        .filter_map(|e| {
            // Tall box art first (it's what the phone shows), then the store's square-ish art.
            let pick = ["DieselGameBoxTall", "OfferImageTall", "Thumbnail", "DieselGameBox"]
                .iter()
                .find_map(|kind| e.key_images.iter().find(|k| k.kind == *kind))?;
            Some((e.id, pick.url.clone()))
        })
        .collect()
}

/// One manifest to a game, or `None` for DLC, tools, unfinished or missing installs.
pub fn parse_item(text: &str, installed: impl Fn(&Path) -> bool, art: &HashMap<String, String>) -> Option<Game> {
    let item: Item = serde_json::from_str(text).ok()?;
    let is_dlc = !item.main_game_app_name.is_empty() && item.main_game_app_name != item.app_name;
    let is_game = item.app_categories.is_empty() || item.app_categories.iter().any(|c| c == "games");
    if !item.is_application || item.incomplete || is_dlc || !is_game || !installed(Path::new(&item.install_location)) {
        return None;
    }
    Some(Game {
        key: format!("epic:{}", item.app_name),
        source: Source::Epic,
        name: clean_name(&item.display_name),
        launch: Launch::Command {
            cmd: format!(
                "com.epicgames.launcher://apps/{}%3A{}%3A{}?action=launch&silent=true",
                item.catalog_namespace, item.catalog_item_id, item.app_name
            ),
            working_dir: None,
        },
        art: None,
        art_url: art.get(&item.catalog_item_id).cloned(),
    })
}

fn decode_base64(text: &str) -> Option<Vec<u8>> {
    fn value(c: u8) -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        } as u32)
    }
    let clean: Vec<u8> = text.bytes().filter(|b| !b.is_ascii_whitespace() && *b != b'=').collect();
    let mut out = Vec::with_capacity(clean.len() * 3 / 4);
    for chunk in clean.chunks(4) {
        let mut n = 0u32;
        for (i, c) in chunk.iter().enumerate() {
            n |= value(*c)? << (18 - 6 * i);
        }
        let bytes = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        out.extend_from_slice(&bytes[..chunk.len().saturating_sub(1)]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ITEM: &str = include_str!("../tests/fixtures/epic.item");

    fn no_art() -> HashMap<String, String> {
        HashMap::new()
    }

    #[test]
    fn reads_a_game() {
        let g = parse_item(ITEM, |_| true, &no_art()).unwrap();
        assert_eq!(g.key, "epic:Ovenbird");
        assert_eq!(g.name, "Surviving Mars");
        assert_eq!(g.art_url, None);
        assert_eq!(
            g.launch,
            Launch::Command {
                cmd: "com.epicgames.launcher://apps/d759128018124dcabb1fbee9bb28e178%3A20729b9176c241f0b617c5723e70ec2d%3AOvenbird?action=launch&silent=true".into(),
                working_dir: None
            }
        );
    }

    #[test]
    fn skips_dlc_missing_and_unfinished() {
        assert!(parse_item(ITEM, |_| false, &no_art()).is_none());
        let dlc = ITEM.replace("\"MainGameAppName\": \"Ovenbird\"", "\"MainGameAppName\": \"SomethingElse\"");
        assert!(parse_item(&dlc, |_| true, &no_art()).is_none());
        let partial = ITEM.replace("\"bIsIncompleteInstall\": false", "\"bIsIncompleteInstall\": true");
        assert!(parse_item(&partial, |_| true, &no_art()).is_none());
        let engine = ITEM.replace("\"games\",", "");
        assert!(parse_item(&engine, |_| true, &no_art()).is_none());
    }

    #[test]
    fn finds_art_in_the_catalogue_cache() {
        let json = r#"[{"id":"20729b9176c241f0b617c5723e70ec2d","keyImages":[
            {"type":"DieselGameBoxWide","url":"https://cdn1.epicgames.com/wide.jpg"},
            {"type":"DieselGameBoxTall","url":"https://cdn1.epicgames.com/tall.jpg"}]},
            {"id":"other","keyImages":[]}]"#;
        let catalog = parse_catalog(&base64(json.as_bytes()));
        assert_eq!(catalog.len(), 1);
        let g = parse_item(ITEM, |_| true, &catalog).unwrap();
        assert_eq!(g.art_url.as_deref(), Some("https://cdn1.epicgames.com/tall.jpg"));
    }

    #[test]
    fn decodes_base64() {
        assert_eq!(decode_base64("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(decode_base64("aGk\n").unwrap(), b"hi");
        assert!(decode_base64("@@@@").is_none());
    }

    fn base64(bytes: &[u8]) -> String {
        const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
            let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
            for i in 0..4 {
                out.push(if i <= chunk.len() { T[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
            }
        }
        out
    }
}
