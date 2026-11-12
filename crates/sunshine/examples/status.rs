//! Prints what PocketDeck sees of Sunshine on this PC: `cargo run -p pocketdeck-sunshine --example status`
fn main() {
    let t = std::time::Instant::now();
    let s = pocketdeck_sunshine::Status::gather();
    println!("{s:#?}");
    println!("ready: {}  outdated: {}  took: {:?}", s.ready(), s.outdated(), t.elapsed());
}
