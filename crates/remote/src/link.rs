//! Watching which path a connection uses, and enforcing "direct only".

use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iroh::endpoint::{Connection, VarInt};
use iroh::{Endpoint, TransportAddr};

use crate::wire::CLOSE_NO_DIRECT;
use crate::{DIRECT_TIMEOUT, GRACE};

/// How a tunnel is connected right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    /// Not running.
    Off,
    /// Running, no connection yet (host: no phone connected; client: dialling or hole punching).
    Connecting,
    /// Direct over the local network.
    Lan,
    /// Direct over the internet (hole punched or port mapped).
    Internet,
    /// Only the relay path is up. Nothing is streamed in this state.
    Relayed,
    /// There was no direct path, or it was lost for longer than the grace period. The
    /// connection is closed.
    NoDirectPath,
}

impl Link {
    pub fn is_direct(self) -> bool {
        matches!(self, Link::Lan | Link::Internet)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Link::Off => "off",
            Link::Connecting => "connecting",
            Link::Lan => "lan",
            Link::Internet => "internet",
            Link::Relayed => "relayed",
            Link::NoDirectPath => "no-direct-path",
        }
    }
}

/// A snapshot of a tunnel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Status {
    pub link: Link,
    /// Round-trip time on the selected path.
    pub rtt: Option<Duration>,
    /// Host only: phones connected right now.
    pub peers: usize,
}

impl Status {
    pub const OFF: Status = Status { link: Link::Off, rtt: None, peers: 0 };

    /// One line, for the named pipe and logs: `lan 3`, `internet 34`, `connecting`, `off`.
    pub fn to_line(&self) -> String {
        match self.rtt {
            Some(rtt) if self.link.is_direct() => format!("{} {}", self.link.as_str(), rtt.as_millis()),
            _ => self.link.as_str().to_string(),
        }
    }
}

/// Whether an address belongs to a local network (or this machine).
pub fn is_lan(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_link_local() || v4.is_loopback(),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_lan(IpAddr::V4(v4));
            }
            let seg = v6.segments()[0];
            v6.is_loopback() || (seg & 0xfe00) == 0xfc00 || (seg & 0xffc0) == 0xfe80
        }
    }
}

/// Whether `remote` is on our own network although it isn't a private address: our own public
/// IPv4 (both ends behind the same NAT) or an IPv6 address in one of our /64s.
pub fn is_own_network(remote: IpAddr, own: &[IpAddr]) -> bool {
    own.iter().any(|ip| match (remote, ip) {
        (IpAddr::V4(r), IpAddr::V4(o)) => r == *o,
        (IpAddr::V6(r), IpAddr::V6(o)) => r.segments()[..4] == o.segments()[..4],
        _ => false,
    })
}

/// The link of a connection right now, from its selected path. `own` are this endpoint's own
/// addresses (local and as seen from outside).
pub fn classify(conn: &Connection, own: &[IpAddr]) -> (Link, Option<Duration>) {
    for path in conn.paths().iter() {
        if !path.is_selected() {
            continue;
        }
        let rtt = Some(path.rtt());
        return match path.remote_addr() {
            TransportAddr::Ip(addr) if is_lan(addr.ip()) || is_own_network(addr.ip(), own) => (Link::Lan, rtt),
            TransportAddr::Ip(_) => (Link::Internet, rtt),
            _ => (Link::Relayed, rtt),
        };
    }
    (Link::Connecting, None)
}

/// The live state of one connection, shared by its forwarders and the watcher.
pub struct LinkState {
    /// Datagrams are only forwarded while this is set.
    pub direct: AtomicBool,
    pub now: Mutex<(Link, Option<Duration>)>,
}

impl LinkState {
    pub fn new() -> Arc<LinkState> {
        Arc::new(LinkState { direct: AtomicBool::new(false), now: Mutex::new((Link::Connecting, None)) })
    }

    pub fn is_direct(&self) -> bool {
        self.direct.load(Ordering::Relaxed)
    }

    pub fn get(&self) -> (Link, Option<Duration>) {
        *self.now.lock().unwrap()
    }

    fn set(&self, link: Link, rtt: Option<Duration>) {
        self.direct.store(link.is_direct(), Ordering::Relaxed);
        *self.now.lock().unwrap() = (link, rtt);
    }
}

/// Polls the connection's paths until it closes. Closes it if it has no direct path for too
/// long: [`DIRECT_TIMEOUT`] + [`GRACE`] if it never had one, [`GRACE`] after losing one.
pub async fn watch(endpoint: Endpoint, conn: Connection, state: Arc<LinkState>) {
    let mut gave_up = false;
    let started = Instant::now();
    let mut ever_direct = false;
    let mut relayed_since: Option<Instant> = None;
    let mut tick = tokio::time::interval(Duration::from_millis(200));
    loop {
        tokio::select! {
            _ = conn.closed() => break,
            _ = tick.tick() => {}
        }
        let own: Vec<IpAddr> = endpoint.addr().ip_addrs().map(|a| a.ip()).collect();
        let (link, rtt) = classify(&conn, &own);
        state.set(link, rtt);
        if link.is_direct() {
            ever_direct = true;
            relayed_since = None;
            continue;
        }
        let since = *relayed_since.get_or_insert_with(Instant::now);
        let too_long = if ever_direct {
            since.elapsed() > GRACE
        } else {
            started.elapsed() > DIRECT_TIMEOUT + GRACE
        };
        if too_long {
            conn.close(VarInt::from_u32(CLOSE_NO_DIRECT), b"no direct path");
            gave_up = true;
            break;
        }
    }
    state.set(if gave_up { Link::NoDirectPath } else { Link::Off }, None);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lan_ranges() {
        for ip in ["192.168.1.5", "10.0.0.1", "172.20.1.1", "169.254.3.3", "127.0.0.1", "fd00::1", "fe80::1"] {
            assert!(is_lan(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["8.8.8.8", "100.64.0.1", "2001:db8::1"] {
            assert!(!is_lan(ip.parse().unwrap()), "{ip}");
        }
        let own: Vec<IpAddr> = ["203.0.113.7", "2001:db8:1:2::5"].iter().map(|s| s.parse().unwrap()).collect();
        assert!(is_own_network("203.0.113.7".parse().unwrap(), &own));
        assert!(is_own_network("2001:db8:1:2::99".parse().unwrap(), &own));
        assert!(!is_own_network("203.0.113.8".parse().unwrap(), &own));
        assert!(!is_own_network("2001:db8:1:3::5".parse().unwrap(), &own));
    }
}
