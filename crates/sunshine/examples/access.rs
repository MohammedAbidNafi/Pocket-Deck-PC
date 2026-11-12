//! Asks Sunshine's web API whether PocketDeck could use it (no login sent): read-only.
fn main() {
    let api = pocketdeck_sunshine::api::Api::new(&pocketdeck_sunshine::Ports::default(), None);
    println!("{:?}", api.access());
}
