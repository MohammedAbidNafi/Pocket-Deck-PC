//! Play from anywhere: carries Sunshine's ports between the PC and the phone over one iroh
//! QUIC connection, so Moonlight on the phone always talks to `127.0.0.1`.
//!
//! - TCP ports (https, http, rtsp, display) ride QUIC bidirectional streams, one stream per TCP
//!   connection, each starting with a one-byte port tag.
//! - UDP ports (video, control, audio) ride QUIC datagrams, each starting with a one-byte channel
//!   tag.
//!
//! The relay is only for finding each other and hole punching. Datagrams are never sent while
//! the connection's selected path goes through the relay, the client doesn't hand out ports
//! until a direct path exists, and a connection that loses its direct path for longer than
//! [`GRACE`] is closed. See `docs/remote-play.md` for the wire format.
//!
//! The API is blocking: each [`Host`] and [`Client`] owns a small tokio runtime.

mod client;
mod host;
mod link;
mod wire;

#[cfg(test)]
mod tests;

pub use client::Client;
pub use host::{Allow, Host};
pub use iroh::EndpointId;
pub use link::{Link, Status};
pub use wire::Ports;

use std::time::Duration;

use iroh::endpoint::{QuicTransportConfig, presets};
use iroh::{Endpoint, RelayMode, RelayUrl, SecretKey};

/// The QUIC ALPN both ends must agree on.
pub const ALPN: &[u8] = b"pocketdeck/tunnel/1";

/// How long the client waits for hole punching to give a direct path before giving up.
pub const DIRECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a connection may run without a direct path (for example while the phone moves from
/// Wi-Fi to mobile data) before it is closed.
pub const GRACE: Duration = Duration::from_secs(5);

/// The smallest datagram the tunnel accepts: Moonlight is told to use 1024-byte video packets,
/// plus RTP and Sunshine's own headers, plus our one-byte tag.
pub const MIN_DATAGRAM: usize = 1100;

/// The mDNS service name both ends use on the local network.
const MDNS_SERVICE: &str = "pocketdeck";

/// Settings shared by [`Host`] and [`Client`].
#[derive(Clone, Debug)]
pub struct Config {
    /// This device's iroh secret key. Keep it private; its public half is the endpoint id.
    pub secret: [u8; 32],
    /// The relay used to find the other end over the internet, for example
    /// `https://relay.example.com`. `None` means local network only (mDNS).
    pub relay: Option<String>,
    /// Tests only: accept the local test relay's self-signed certificate.
    #[doc(hidden)]
    pub insecure_relay_tls: bool,
    /// Tests only: no UDP sockets, so only the relay path exists.
    #[doc(hidden)]
    pub relay_only: bool,
    /// Host only: where Sunshine listens. Always `127.0.0.1` outside tests.
    #[doc(hidden)]
    pub sunshine_ip: std::net::Ipv4Addr,
}

impl Config {
    pub fn new(secret: [u8; 32], relay: Option<String>) -> Config {
        Config { secret, relay, insecure_relay_tls: false, relay_only: false, sunshine_ip: std::net::Ipv4Addr::LOCALHOST }
    }
}

/// A fresh random secret key.
pub fn generate_secret() -> [u8; 32] {
    SecretKey::generate().to_bytes()
}

/// The endpoint id (public key) belonging to a secret key.
pub fn endpoint_id(secret: &[u8; 32]) -> EndpointId {
    SecretKey::from_bytes(secret).public()
}

/// Parses an endpoint id as printed by its `Display` impl.
pub fn parse_id(text: &str) -> Result<EndpointId, Error> {
    text.trim().parse().map_err(|e| Error::Config(format!("bad endpoint id: {e}")))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Bad settings (relay URL, endpoint id).
    Config(String),
    /// The local endpoint or a local port couldn't be opened.
    Bind(String),
    /// The other end couldn't be reached at all.
    Connect(String),
    /// The other end answered but the hello failed (old version, refused, closed).
    Hello(String),
    /// The other end was reached, but only through the relay. Streaming needs a direct path.
    NoDirectPath,
    /// The direct path can't carry a full video packet.
    DatagramsTooSmall(usize),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Config(e) => write!(f, "{e}"),
            Error::Bind(e) => write!(f, "couldn't open the tunnel: {e}"),
            Error::Connect(e) => write!(f, "couldn't reach the PC: {e}"),
            Error::Hello(e) => write!(f, "the PC refused the tunnel: {e}"),
            Error::NoDirectPath => write!(f, "can't reach the PC directly on this network"),
            Error::DatagramsTooSmall(n) => {
                write!(f, "this network only carries {n}-byte packets; streaming needs {MIN_DATAGRAM}")
            }
        }
    }
}

impl std::error::Error for Error {}

fn runtime() -> Result<tokio::runtime::Runtime, Error> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("pocketdeck-tunnel")
        .enable_all()
        .build()
        .map_err(|e| Error::Bind(e.to_string()))
}

/// Opens an iroh endpoint with our settings: only our relay (never n0's), no n0 address lookup,
/// mDNS on the local network, the port mapper on, and big datagram buffers for video bursts.
async fn bind(config: &Config, accept: bool) -> Result<Endpoint, Error> {
    let relay_mode = match &config.relay {
        Some(url) => {
            let url: RelayUrl =
                url.parse().map_err(|e| Error::Config(format!("bad relay URL {url}: {e}")))?;
            RelayMode::custom([url])
        }
        None => RelayMode::Disabled,
    };
    let transport = QuicTransportConfig::builder()
        .datagram_send_buffer_size(4 << 20)
        .datagram_receive_buffer_size(Some(4 << 20))
        .keep_alive_interval(Duration::from_secs(5))
        .build();
    let mdns = iroh_mdns_address_lookup::MdnsAddressLookup::builder()
        .service_name(MDNS_SERVICE)
        .advertise(accept);
    let mut builder = Endpoint::builder(presets::Minimal)
        .secret_key(SecretKey::from_bytes(&config.secret))
        .relay_mode(relay_mode)
        .transport_config(transport);
    if !config.relay_only {
        builder = builder.address_lookup(mdns);
    }
    if accept {
        builder = builder.alpns(vec![ALPN.to_vec()]);
    }
    #[cfg(test)]
    if config.insecure_relay_tls {
        builder = builder.ca_tls_config(iroh::tls::CaTlsConfig::insecure_skip_verify());
    }
    if config.relay_only {
        builder = builder.clear_ip_transports();
    }
    builder.bind().await.map_err(|e| Error::Bind(e.to_string()))
}
