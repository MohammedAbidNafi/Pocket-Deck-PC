//! Fetches and prepares Epic covers into a folder: `cargo run -p pocketdeck-library --example epic_art -- <dir>`
fn main() {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).expect("output folder"));
    for g in pocketdeck_library::epic::scan().games {
        let art = g.art_url.as_ref().and_then(|u| pocketdeck_library::covers::fetch(&dir, &g.key, u));
        let cover = art.as_ref().and_then(|a| pocketdeck_library::covers::prepare(&dir, &g.key, a));
        println!("{:<20} url: {}  cover: {:?}", g.name, g.art_url.is_some(), cover);
    }
}
