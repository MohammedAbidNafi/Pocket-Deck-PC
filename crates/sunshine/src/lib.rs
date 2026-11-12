//! Everything PocketDeck needs to know about, and do with, Sunshine on this PC:
//! finding an existing install, reading its ports, checking its Windows service, asking the
//! server whether it is answering, and installing a pinned release when there is none.

pub mod api;
pub mod cert;
pub mod conf;
pub mod detect;
pub mod elevate;
pub mod install;
pub mod login;
pub mod probe;
pub mod service;
pub mod version;

pub use conf::Ports;
pub use detect::Install;
pub use probe::ServerInfo;
pub use service::ServiceState;

use std::fmt;

#[derive(Debug)]
pub enum Error {
    /// The person said no to the Windows administrator prompt.
    Cancelled,
    Io(std::io::Error),
    Download(String),
    /// The downloaded file isn't the one we expected.
    Checksum { expected: String, actual: String },
    /// A command ran but reported failure.
    Failed(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Cancelled => write!(f, "The administrator prompt was cancelled."),
            Error::Io(e) => write!(f, "{e}"),
            Error::Download(e) => write!(f, "Download failed: {e}"),
            Error::Checksum { .. } => write!(f, "The download was damaged or changed. Try again."),
            Error::Failed(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Name of the Windows service the Sunshine installer registers.
pub const SERVICE_NAME: &str = "SunshineService";

/// Name of the virtual controller driver's service (ViGEmBus), which Sunshine uses for gamepads.
pub const CONTROLLER_DRIVER_SERVICE: &str = "ViGEmBus";

/// Where to get the controller driver when it is missing.
pub const CONTROLLER_DRIVER_URL: &str = "https://github.com/nefarius/ViGEmBus/releases/latest";

/// Oldest Sunshine release PocketDeck accepts without suggesting an update.
pub const MIN_VERSION: &str = "2025.0.0";

/// One snapshot of Sunshine's state, gathered off the UI thread.
#[derive(Debug, Clone, Default)]
pub struct Status {
    pub install: Option<Install>,
    pub service: ServiceState,
    pub controller_driver: bool,
    pub ports: Ports,
    /// What the server said when asked; `None` when it didn't answer.
    pub server: Option<ServerInfo>,
}

impl Status {
    /// Checks everything. Takes up to about a second when Sunshine isn't answering.
    pub fn gather() -> Status {
        let install = detect::find();
        let ports = install
            .as_ref()
            .map(|i| conf::read_ports(&i.config_file()))
            .unwrap_or_default();
        let service = service::state(SERVICE_NAME);
        let controller_driver = service::exists(CONTROLLER_DRIVER_SERVICE);
        let server = if service == ServiceState::Running || install.is_some() {
            probe::server_info(ports.http).ok()
        } else {
            None
        };
        Status { install, service, controller_driver, ports, server }
    }

    /// True when an install was found but is older than [`MIN_VERSION`].
    pub fn outdated(&self) -> bool {
        self.install
            .as_ref()
            .and_then(|i| i.version.as_deref())
            .is_some_and(|v| version::older_than(v, MIN_VERSION))
    }

    /// Installed, running and answering: ready to stream.
    pub fn ready(&self) -> bool {
        self.install.is_some() && self.service == ServiceState::Running && self.server.is_some()
    }
}
