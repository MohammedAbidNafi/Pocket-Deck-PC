//! Play from anywhere, PC side: the service runs a `pocketdeck_remote::Host` all the time, so a
//! phone always reaches Sunshine through the tunnel, at home (found over mDNS) and away (found
//! through the relay, then hole punched). Streams are only ever direct; see `docs/remote-play.md`.
//!
//! - The PC's iroh key: `%ProgramData%\PocketDeck\remote-key.bin`, SYSTEM and administrators only.
//! - Registered phones: `%ProgramData%\PocketDeck\remote-peers.json`, phone endpoint id →
//!   SHA-256 of the phone's Sunshine client certificate. A phone registers once, on the LAN,
//!   through the mutual-TLS endpoint in [`crate::remote`].
//! - A phone is let in only while that certificate is still on Sunshine's paired list, so
//!   unpairing it in Sunshine also ends remote access.
//! - "Play from anywhere" (`RemoteEnabled` under [`crate::REG_KEY`], on unless switched off)
//!   only decides whether the host uses the relay. Without it the tunnel still works on the LAN.

use crate::{PORT_OFFSET, REG_KEY, log, remote};
use pocketdeck_remote::{Config, EndpointId, Host, Ports, Status};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The relay baked in at build time (`POCKETDECK_RELAY_URL`), overridable with the `RelayUrl`
/// value under [`REG_KEY`].
pub const BUILT_IN_RELAY: Option<&str> = option_env!("POCKETDECK_RELAY_URL");

fn key_file() -> PathBuf {
    crate::data_dir().join("remote-key.bin")
}

fn peers_file() -> PathBuf {
    crate::data_dir().join("remote-peers.json")
}

fn reg() -> Option<winreg::RegKey> {
    winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(REG_KEY, winreg::enums::KEY_READ)
        .ok()
}

/// The relay phones and this PC meet at, if any.
pub fn relay_url() -> Option<String> {
    reg()
        .and_then(|k| k.get_value::<String, _>("RelayUrl").ok())
        .filter(|s| !s.trim().is_empty())
        .or_else(|| BUILT_IN_RELAY.map(str::to_string))
}

/// "Play from anywhere" is on (the default).
pub fn enabled() -> bool {
    reg().and_then(|k| k.get_value::<u32, _>("RemoteEnabled").ok()).map(|v| v != 0).unwrap_or(true)
}

fn set_enabled(on: bool) -> Result<(), String> {
    winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE)
        .create_subkey(REG_KEY)
        .and_then(|(k, _)| k.set_value("RemoteEnabled", &u32::from(on)))
        .map_err(|e| format!("Couldn't save the setting: {e}"))
}

/// The PC's secret key, created on first use. Only SYSTEM and administrators can read it.
pub fn secret() -> Result<[u8; 32], String> {
    let path = key_file();
    if let Ok(bytes) = std::fs::read(&path) {
        if let Ok(key) = <[u8; 32]>::try_from(bytes.as_slice()) {
            return Ok(key);
        }
        log::line("tunnel: the key file is damaged; making a new one (phones must register again)");
    }
    let key = pocketdeck_remote::generate_secret();
    std::fs::create_dir_all(crate::data_dir()).map_err(|e| e.to_string())?;
    std::fs::write(&path, key).map_err(|e| format!("Couldn't save the tunnel key: {e}"))?;
    restrict(&path);
    Ok(key)
}

/// Leaves only SYSTEM and Administrators on a file's access list.
fn restrict(path: &std::path::Path) {
    let icacls = PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into()))
        .join("System32")
        .join("icacls.exe");
    let out = std::process::Command::new(icacls)
        .arg(path)
        .args(["/inheritance:r", "/grant:r", "*S-1-5-18:F", "*S-1-5-32-544:F"])
        .output();
    if !out.is_ok_and(|o| o.status.success()) {
        log::line(format!("tunnel: couldn't restrict {}", path.display()));
    }
}

/// This PC's endpoint id.
pub fn pc_id() -> Result<EndpointId, String> {
    secret().map(|s| pocketdeck_remote::endpoint_id(&s))
}

pub fn cert_hash(der: &[u8]) -> String {
    Sha256::digest(der).iter().map(|b| format!("{b:02x}")).collect()
}

fn read_peers() -> BTreeMap<String, String> {
    std::fs::read_to_string(peers_file()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// Remembers that the phone with this endpoint id holds this (paired) client certificate.
pub fn register(phone: EndpointId, client_cert: &[u8]) -> Result<(), String> {
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock().unwrap();
    let mut peers = read_peers();
    let hash = cert_hash(client_cert);
    // One endpoint id per certificate: a reinstalled phone replaces its old id.
    peers.retain(|_, h| *h != hash);
    peers.insert(phone.to_string(), hash);
    let text = serde_json::to_string_pretty(&peers).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(crate::data_dir()).map_err(|e| e.to_string())?;
    let path = peers_file();
    let fresh = !path.exists();
    std::fs::write(&path, text).map_err(|e| format!("Couldn't save the phone list: {e}"))?;
    if fresh {
        restrict(&path);
    }
    Ok(())
}

/// Whether a phone may use the tunnel right now: registered, and its certificate still paired.
pub fn allowed(phone: EndpointId) -> bool {
    let Some(hash) = read_peers().remove(&phone.to_string()) else { return false };
    let Ok(s) = remote::sunshine() else { return false };
    remote::paired_certs(&s.state).iter().any(|der| cert_hash(der) == hash)
}

/// Sunshine's ports as the tunnel sees them, plus our display endpoint.
pub fn sunshine_ports() -> Result<Ports, String> {
    let install = pocketdeck_sunshine::detect::find().ok_or("Sunshine isn't installed.")?;
    let p = pocketdeck_sunshine::conf::read_ports(&install.config_file());
    Ok(Ports {
        https: p.https,
        http: p.http,
        rtsp: p.rtsp,
        display: p.http.saturating_add(PORT_OFFSET),
        video: p.video,
        control: p.control,
        audio: p.audio,
    })
}

/// The host as the service runs it.
pub struct Tunnel {
    host: Mutex<Option<(Host, Ports, bool)>>,
    error: Mutex<Option<String>>,
    last_try: Mutex<Option<Instant>>,
}

impl Tunnel {
    pub fn new() -> Arc<Tunnel> {
        Arc::new(Tunnel { host: Mutex::new(None), error: Mutex::new(None), last_try: Mutex::new(None) })
    }

    /// (Re)starts the host with the current settings.
    pub fn restart(&self) {
        *self.last_try.lock().unwrap() = Some(Instant::now());
        let mut host = self.host.lock().unwrap();
        if let Some((old, _, _)) = host.take() {
            old.stop();
        }
        match start() {
            Ok((h, ports, relayed)) => {
                log::line(format!(
                    "tunnel: ready as {} ({})",
                    h.id(),
                    if relayed { relay_url().unwrap_or_default() } else { "local network only".into() }
                ));
                *host = Some((h, ports, relayed));
                *self.error.lock().unwrap() = None;
            }
            Err(e) => {
                log::line(format!("tunnel: {e}"));
                *self.error.lock().unwrap() = Some(e);
            }
        }
    }

    /// Every few seconds: retries a failed start once a minute, and restarts after Sunshine's
    /// ports changed.
    pub fn tick(&self) {
        // Busy (starting or stopping): look again next tick rather than hold up the service loop.
        let Ok(host) = self.host.try_lock() else { return };
        let running_ports = host.as_ref().map(|(_, p, _)| *p);
        drop(host);
        match running_ports {
            Some(ports) => {
                if sunshine_ports().is_ok_and(|now| now != ports) {
                    log::line("tunnel: Sunshine's ports changed; restarting");
                    self.restart();
                }
            }
            None => {
                let due = self.last_try.lock().unwrap().is_none_or(|t| t.elapsed() > Duration::from_secs(60));
                if due {
                    self.restart();
                }
            }
        }
    }

    pub fn stop(&self) {
        if let Some((h, _, _)) = self.host.lock().unwrap().take() {
            h.stop();
        }
    }

    pub fn status(&self) -> Value {
        let host = self.host.lock().unwrap();
        let status = host.as_ref().map(|(h, _, _)| h.status()).unwrap_or(Status::OFF);
        json!({
            "state": status.link.as_str(),
            "rttMs": status.rtt.map(|d| d.as_millis() as u64),
            "peers": status.peers,
            "enabled": enabled(),
            "relay": host.as_ref().and_then(|(_, _, relayed)| relayed.then(relay_url).flatten()),
            "id": host.as_ref().map(|(h, _, _)| h.id().to_string()),
            "message": *self.error.lock().unwrap(),
        })
    }

    /// `remote status`, `remote on`, `remote off` from the pipe.
    pub fn command(&self, words: &[&str]) -> Value {
        match words {
            ["status"] => self.status(),
            [on @ ("on" | "off")] => match set_enabled(*on == "on") {
                Ok(()) => {
                    self.restart();
                    self.status()
                }
                Err(e) => json!({ "state": "failed", "message": e }),
            },
            _ => json!({ "state": "failed", "message": "Commands: remote status, remote on, remote off" }),
        }
    }
}

fn start() -> Result<(Host, Ports, bool), String> {
    let ports = sunshine_ports()?;
    let relay = if enabled() { relay_url() } else { None };
    let relayed = relay.is_some();
    let host = Host::start(Config::new(secret()?, relay), Arc::new(allowed), ports).map_err(|e| e.to_string())?;
    Ok((host, ports, relayed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_certificates_as_hex() {
        assert_eq!(cert_hash(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
