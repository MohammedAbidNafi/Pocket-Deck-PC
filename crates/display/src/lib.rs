//! A virtual display that exists only while a phone is streaming.
//!
//! Windows sees a real monitor at the phone's own resolution and refresh rate, so games render
//! for the phone's screen instead of the PC's. Kept off the rest of the time: a virtual monitor
//! that stays plugged in is what makes Windows boot onto it or open apps on it.
//!
//! The pieces:
//! - [`vdd`]: the driver release PocketDeck installs, and its resolution list.
//! - [`device`]: finding the virtual display adapter and switching it on and off (admin only).
//! - [`topology`]: making it the main screen at the phone's mode, beside the PC's own (runs in the signed-in session).
//! - [`service`]: the Windows service that owns all of that and hands out leases.
//! - [`remote`]: the LAN endpoint the phone calls before it streams.
//! - [`tunnel`]: play from anywhere, the PC end of the phone's iroh tunnel.
//! - [`pipe`]: the local control channel (`PocketDeck.exe --display on 1920 1080 60` and friends).
//! - [`setup`]: installing and removing all of it, with one administrator prompt.
//!
//! The contract with the phone is `docs/virtual-display.md`; keep the two in step.

pub mod cli;
pub mod device;
mod follow;
mod log;
pub mod pipe;
pub mod remote;
pub mod service;
mod session;
pub mod setup;
pub mod sunshine_id;
pub mod topology;
pub mod tunnel;
pub mod vdd;

pub use vdd::Mode;

use std::path::PathBuf;

/// The Windows service's name.
pub const SERVICE_NAME: &str = "PocketDeckDisplay";

/// Shown in Services and Task Manager.
pub const SERVICE_DISPLAY_NAME: &str = "PocketDeck virtual display";

/// The command-line switch that turns `PocketDeck.exe` into the display tool or service.
pub const FLAG: &str = "--display";

/// The phone's endpoint listens this many ports above Sunshine's base port (47989 + 30 = 48019).
/// Sunshine itself uses offsets -5 to +21.
pub const PORT_OFFSET: u16 = 30;

/// How long a phone's claim on the display lasts without a keep-alive. The phone renews it
/// every 20 seconds while streaming, so this survives a few lost requests.
pub const LEASE_SECS: u64 = 90;

/// Registry key (under HKLM) where setup records what it did, so removal can undo exactly that.
pub const REG_KEY: &str = r"SOFTWARE\PocketDeck\Display";

/// `%ProgramFiles%\PocketDeck`: the service's copy of the program, the driver files and the
/// driver's settings. Only administrators can write here, which matters because the service
/// runs as SYSTEM.
pub fn install_dir() -> PathBuf {
    let base = std::env::var_os("ProgramW6432")
        .or_else(|| std::env::var_os("ProgramFiles"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"));
    base.join("PocketDeck")
}

/// The service's copy of the program.
pub fn service_exe() -> PathBuf {
    install_dir().join("PocketDeckDisplay.exe")
}

/// `%ProgramData%\PocketDeck`: the service's log.
pub fn data_dir() -> PathBuf {
    std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
        .join("PocketDeck")
}

/// Whether the person has the feature switched on (`Enabled` under [`REG_KEY`]; on unless
/// switched off). Readable by anyone; only the service writes it.
pub fn allowed() -> bool {
    winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(REG_KEY, winreg::enums::KEY_READ)
        .and_then(|k| k.get_value::<u32, _>("Enabled"))
        .map(|v| v != 0)
        .unwrap_or(true)
}

/// What the app shows about the virtual display. Gathered without admin rights.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Overview {
    /// Set up by PocketDeck (the service is registered).
    pub installed: bool,
    /// The version of PocketDeck that set it up.
    pub version: Option<String>,
    pub running: bool,
    /// The virtual display adapter exists.
    pub driver: bool,
    /// A phone holds the display right now.
    pub in_use: bool,
    /// Switched on in Settings (phones may use it).
    pub allowed: bool,
    /// Play from anywhere is switched on.
    pub remote_enabled: bool,
    /// One line on how play from anywhere is doing, for the settings row.
    pub remote_line: String,
}

impl Overview {
    pub fn gather() -> Overview {
        use pocketdeck_sunshine::ServiceState;
        let service = pocketdeck_sunshine::service::state(SERVICE_NAME);
        let running = service == ServiceState::Running;
        let in_use = running
            && pipe::request("status")
                .ok()
                .and_then(|r| serde_json::from_str::<serde_json::Value>(&r).ok())
                .is_some_and(|v| v["state"] == "on");
        let remote = if running {
            pipe::request("remote status").ok().and_then(|r| serde_json::from_str::<serde_json::Value>(&r).ok())
        } else {
            None
        };
        Overview {
            remote_enabled: remote.as_ref().is_none_or(|v| v["enabled"] != false),
            remote_line: remote.as_ref().map(remote_line).unwrap_or_default(),
            installed: service != ServiceState::NotInstalled,
            version: setup::installed_version(),
            running,
            driver: device::find().is_some(),
            in_use,
            allowed: allowed(),
        }
    }

    /// Set up by an older (or newer) PocketDeck: setup should run again.
    pub fn outdated(&self) -> bool {
        self.installed && self.version.as_deref() != Some(env!("CARGO_PKG_VERSION"))
    }
}

/// The settings row's line for the service's `remote status` answer.
pub fn remote_line(v: &serde_json::Value) -> String {
    let host = |url: &str| url.split("://").nth(1).unwrap_or(url).trim_end_matches('/').to_string();
    let rtt = v["rttMs"].as_u64().map(|ms| format!(", {ms} ms")).unwrap_or_default();
    if let Some(m) = v["message"].as_str() {
        return m.to_string();
    }
    match v["state"].as_str().unwrap_or("") {
        "lan" => format!("Phone connected directly over your network{rtt}."),
        "internet" => format!("Phone connected directly over the internet{rtt}."),
        "relayed" => "A phone is connecting. Waiting for a direct path…".into(),
        "off" => "Starting…".into(),
        _ if v["enabled"] == false => "Off. Phones connect on this PC's network only.".into(),
        _ => match v["relay"].as_str() {
            Some(url) => format!("Ready via {}. Phones stream directly, at home or away.", host(url)),
            None => "Ready on this network. No relay is set up for playing away from home.".into(),
        },
    }
}

pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A NUL-terminated UTF-16 buffer as a `String`.
pub(crate) fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}
