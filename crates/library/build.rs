//! Bakes the SteamGridDB API key into the build. It comes from the `STEAMGRIDDB_API_KEY`
//! environment variable (CI) or the workspace's git-ignored `.env` file (local builds). Without
//! one, PocketDeck simply skips SteamGridDB.

use std::path::Path;

const VAR: &str = "STEAMGRIDDB_API_KEY";

fn main() {
    println!("cargo:rerun-if-env-changed={VAR}");
    let env_file = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.env");
    println!("cargo:rerun-if-changed={}", env_file.display());

    let from_env = std::env::var(VAR).ok().filter(|v| !v.trim().is_empty());
    let key = from_env.or_else(|| {
        let text = std::fs::read_to_string(&env_file).ok()?;
        text.lines()
            .filter_map(|line| line.split_once('='))
            .find(|(name, _)| name.trim() == VAR)
            .map(|(_, value)| value.trim().trim_matches('"').trim_matches('\'').to_string())
            .filter(|v| !v.is_empty())
    });
    if let Some(key) = key {
        println!("cargo:rustc-env={VAR}={key}");
    }
}
