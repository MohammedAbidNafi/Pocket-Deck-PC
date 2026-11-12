//! Read-only check of what pairing by QR code needs: the certificate fingerprint, devices
//! waiting for a PIN and the paired devices (with PocketDeck's saved login).
use pocketdeck_sunshine::{api::Api, cert, conf, detect, login};

fn main() {
    let ports = detect::find().map(|i| conf::read_ports(&i.config_file())).unwrap_or_default();
    println!("fingerprint (port {}): {:?}", ports.https, cert::fingerprint(ports.https));
    let dir = std::path::PathBuf::from(std::env::var_os("APPDATA").unwrap_or_default()).join("PocketDeck");
    let api = Api::new(&ports, login::load(&dir).as_ref());
    println!("access: {:?}", api.access());
    println!("waiting: {:?}", api.pending_pairings());
    println!("paired: {:?}", api.clients());
}
