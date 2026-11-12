//! Prints the pairing URI this PC would show (read-only: nothing is sent to Sunshine but
//! a TLS handshake and a serverinfo request).
use pocketdeck_core::pairing::{PairCode, lan_addresses};
use pocketdeck_sunshine::{cert, conf, detect, probe};

fn main() {
    let ports = detect::find().map(|i| conf::read_ports(&i.config_file())).unwrap_or_default();
    let code = PairCode {
        name: probe::server_info(ports.http).map(|i| i.hostname).unwrap_or_default(),
        addresses: lan_addresses(),
        port: ports.http,
        pin: pocketdeck_sunshine::api::random_pin(),
        fingerprint: cert::fingerprint(ports.https).unwrap_or_default(),
    };
    let uri = code.uri();
    println!("{uri}\n{} characters", uri.len());
}
