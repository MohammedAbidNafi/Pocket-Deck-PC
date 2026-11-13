//! Cover art from SteamGridDB (<https://www.steamgriddb.com>), for games whose launcher has none:
//! games found among the installed programs, games added by hand, and any launcher game whose
//! own art is missing.
//!
//! Steam games are looked up by app id, everything else by name. A game with no match is
//! remembered for a week so a sync doesn't ask again every time.

use crate::{Game, Source};
use serde::Deserialize;
use std::path::Path;
use std::time::{Duration, SystemTime};

const API: &str = "https://www.steamgriddb.com/api/v2";
/// Portrait sizes SteamGridDB uses for library covers (all 2:3-ish).
const DIMENSIONS: &str = "600x900,342x482,660x930";
const RETRY_AFTER: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// The API key: `STEAMGRIDDB_API_KEY` at run time, else the one baked in at build time.
pub fn api_key() -> Option<String> {
    std::env::var("STEAMGRIDDB_API_KEY")
        .ok()
        .or_else(|| option_env!("STEAMGRIDDB_API_KEY").map(str::to_string))
        .filter(|k| !k.trim().is_empty())
}

/// Whether SteamGridDB can be used at all.
pub fn available() -> bool {
    api_key().is_some()
}

#[derive(Deserialize)]
struct Response<T> {
    success: bool,
    #[serde(default = "Vec::new")]
    data: Vec<T>,
}

#[derive(Deserialize)]
struct SearchHit {
    id: u64,
    name: String,
}

#[derive(Deserialize, Debug, Clone)]
pub struct Grid {
    pub url: String,
    #[serde(default)]
    pub score: i64,
    #[serde(default)]
    pub upvotes: i64,
    #[serde(default)]
    pub downvotes: i64,
    #[serde(default)]
    pub nsfw: bool,
    #[serde(default)]
    pub epilepsy: bool,
}

/// The URL of the best portrait cover for `game`, or `None`. Misses are remembered in
/// `<dir>/art/` for a week.
pub fn cover_url(dir: &Path, game: &Game) -> Option<String> {
    let key = api_key()?;
    let miss = miss_marker(dir, &game.key);
    if recently(&miss) {
        return None;
    }
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(15))).build().into();
    let get = |url: &str| -> Option<String> {
        let mut res = agent.get(url).header("Authorization", &format!("Bearer {key}")).call().ok()?;
        res.body_mut().with_config().limit(4 * 1024 * 1024).read_to_string().ok()
    };
    let grids_query = format!("dimensions={DIMENSIONS}&types=static&mimes=image/png,image/jpeg&nsfw=false&humor=false");

    let grids = match (game.source, game.key.strip_prefix("steam:")) {
        (Source::Steam, Some(app_id)) => get(&format!("{API}/grids/steam/{app_id}?{grids_query}")),
        _ => {
            let term = search_term(&game.name);
            let hits = get(&format!("{API}/search/autocomplete/{}", encode(&term)))
                .and_then(|body| serde_json::from_str::<Response<SearchHit>>(&body).ok())
                .filter(|r| r.success)
                .map(|r| r.data)
                .unwrap_or_default();
            best_match(&term, &hits).and_then(|id| get(&format!("{API}/grids/game/{id}?{grids_query}")))
        }
    };
    let url = grids
        .and_then(|body| serde_json::from_str::<Response<Grid>>(&body).ok())
        .filter(|r| r.success)
        .and_then(|r| best_grid(&r.data));
    if url.is_none() {
        if let Some(parent) = miss.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&miss, b"");
    }
    url
}

fn miss_marker(dir: &Path, key: &str) -> std::path::PathBuf {
    let safe: String = key.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' }).collect();
    dir.join("art").join(format!("{safe}.sgdb-miss"))
}

fn recently(path: &Path) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age < RETRY_AFTER)
}

/// Install folder names make poor search terms: `WATCH_DOGS2` → `WATCH DOGS2`.
fn search_term(name: &str) -> String {
    crate::clean_name(&name.replace(['_', '.'], " "))
}

/// The hit whose name matches exactly (ignoring case and punctuation), else the first one.
fn best_match(term: &str, hits: &[SearchHit]) -> Option<u64> {
    let wanted = crate::installed::normalise(term);
    hits.iter()
        .find(|h| crate::installed::normalise(&h.name) == wanted)
        .or_else(|| hits.first())
        .map(|h| h.id)
}

/// The best-rated cover, skipping ones flagged as unsafe.
pub fn best_grid(grids: &[Grid]) -> Option<String> {
    grids
        .iter()
        .filter(|g| !g.nsfw && !g.epilepsy)
        .enumerate()
        // Higher score, then net votes; the API's own order breaks ties.
        .max_by_key(|(i, g)| (g.score, g.upvotes - g.downvotes, std::cmp::Reverse(*i)))
        .map(|(_, g)| g.url.clone())
}

/// Percent-encodes a path segment.
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(url: &str, score: i64, up: i64, down: i64) -> Grid {
        Grid { url: url.into(), score, upvotes: up, downvotes: down, nsfw: false, epilepsy: false }
    }

    #[test]
    fn picks_the_best_grid() {
        let grids = vec![grid("a", 0, 0, 0), grid("b", 0, 5, 1), grid("c", 0, 5, 1), Grid { nsfw: true, ..grid("d", 9, 9, 0) }];
        assert_eq!(best_grid(&grids).as_deref(), Some("b"));
        assert_eq!(best_grid(&[]), None);
    }

    #[test]
    fn prefers_an_exact_name() {
        let hits = vec![
            SearchHit { id: 1, name: "Ghost of Tsushima".into() },
            SearchHit { id: 2, name: "Ghost of Tsushima: Director's Cut".into() },
        ];
        assert_eq!(best_match("Ghost of Tsushima Director's Cut", &hits), Some(2));
        assert_eq!(best_match("Ghost of Tsushima DC", &hits), Some(1));
        assert_eq!(best_match("x", &[]), None);
    }

    #[test]
    fn cleans_and_encodes_terms() {
        assert_eq!(search_term("WATCH_DOGS2"), "WATCH DOGS2");
        assert_eq!(encode("Tom Clancy's: R6"), "Tom%20Clancy%27s%3A%20R6");
    }
}
