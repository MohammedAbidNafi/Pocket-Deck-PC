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
//! - [`pipe`]: the local control channel (`PocketDeck.exe --display on 1920 1080 60` and friends).
//! - [`setup`]: installing and removing all of it, with one administrator prompt.
//!
//! The contract with the phone is `docs/virtual-display.md`; keep the two in step.

pub mod cli;
pub mod device;
mod log;
pub mod pipe;
pub mod remote;
pub mod service;
mod session;
pub mod setup;
pub mod topology;
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
        Overview {
            installed: service != ServiceState::NotInstalled,
            version: setup::installed_version(),
            running,
            driver: device::find().is_some(),
            in_use,
        }
    }

    /// Set up by an older (or newer) PocketDeck: setup should run again.
    pub fn outdated(&self) -> bool {
        self.installed && self.version.as_deref() != Some(env!("CARGO_PKG_VERSION"))
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
