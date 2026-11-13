//! Lists the games found on this PC: `cargo run -p pocketdeck-library --example scan`
fn main() {
    let t = std::time::Instant::now();
    for (source, scan) in pocketdeck_library::scan_all() {
        println!("{} (launcher found: {}): {} games", source.label(), scan.launcher_found, scan.games.len());
        for g in &scan.games {
            println!("  {:<40} {:<22} art: {}", g.name, g.key, if g.art.is_some() { "yes" } else { "no" });
            if std::env::var_os("SHOW_LAUNCH").is_some() { println!("      {:?}", g.launch); }
        }
    }
    println!("took {:?}", t.elapsed());
    // With an output folder, also write covers: `-- <dir>`
    if let Some(out) = std::env::args().nth(1) {
        let t = std::time::Instant::now();
        let mut n = 0;
        for (_, scan) in pocketdeck_library::scan_all() {
            for g in scan.games {
                if let Some(art) = &g.art {
                    if pocketdeck_library::covers::prepare(std::path::Path::new(&out), &g.key, art).is_some() { n += 1; }
                }
            }
        }
        println!("wrote {n} covers in {:?}", t.elapsed());
    }
}
