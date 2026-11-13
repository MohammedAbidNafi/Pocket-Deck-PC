//! Fetches SteamGridDB covers for the games that have no art of their own:
//! `cargo run -p pocketdeck-library --example sgdb -- <out dir>`
fn main() {
    let out = std::path::PathBuf::from(std::env::args().nth(1).expect("an output folder"));
    println!("API key: {}", if pocketdeck_library::steamgriddb::available() { "yes" } else { "no" });
    for (_, scan) in pocketdeck_library::scan_all() {
        for g in scan.games.iter().filter(|g| g.art.is_none() && g.art_url.is_none()) {
            let url = pocketdeck_library::steamgriddb::cover_url(&out, g);
            let file = url.as_ref().and_then(|u| pocketdeck_library::covers::fetch(&out, &g.key, u));
            let cover = file.as_ref().and_then(|f| pocketdeck_library::covers::prepare(&out, &g.key, f));
            println!("{:<36} {}", g.name, cover.map(|c| c.display().to_string()).unwrap_or_else(|| "no cover".into()));
        }
    }
}
