//! Pairing a phone by QR code: what goes in the code, and the small decisions around it.
//! The contract with the phone is `docs/pair-with-scan.md`; keep the two in step.
//!
//! The PC picks the PIN and shows it in the code. The phone scans it and starts an ordinary
//! Moonlight pairing with that PIN; the PC then enters the PIN into Sunshine for it.

use pocketdeck_sunshine::api::{Client, PendingPairing};
use std::collections::HashSet;
use std::net::{Ipv4Addr, UdpSocket};

/// Version of the QR contract.
pub const VERSION: u32 = 1;

/// How long one code stays good for.
pub const CODE_LIFETIME_SECS: u64 = 120;

/// What Sunshine calls a phone paired this way (numbered when there are several).
pub const DEVICE_NAME: &str = "PocketDeck phone";

/// Everything the phone needs to find this PC and pair with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairCode {
    /// The PC's name, as the phone should show it.
    pub name: String,
    /// LAN addresses, best first.
    pub addresses: Vec<Ipv4Addr>,
    /// Sunshine's base (GameStream HTTP) port.
    pub port: u16,
    pub pin: String,
    /// SHA-256 of Sunshine's certificate, lowercase hex.
    pub fingerprint: String,
}

impl PairCode {
    /// The URI the QR code holds: `pocketdeck://pair?v=1&n=…&a=…&p=…&pin=…&fp=…`.
    pub fn uri(&self) -> String {
        let addresses: Vec<String> = self.addresses.iter().map(Ipv4Addr::to_string).collect();
        format!(
            "pocketdeck://pair?v={VERSION}&n={}&a={}&p={}&pin={}&fp={}",
            encode(&self.name),
            addresses.join(","),
            self.port,
            self.pin,
            self.fingerprint
        )
    }
}

/// Percent-encodes everything but RFC 3986's unreserved characters.
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// What to do with the requests waiting in Sunshine.
#[derive(Debug, PartialEq, Eq)]
pub struct Choice<'a> {
    /// The request to enter the PIN for.
    pub answer: Option<&'a PendingPairing>,
    /// Older requests from the same phone, to turn down so the new one goes first.
    pub cancel: Vec<&'a PendingPairing>,
}

/// Picks the request that belongs to the phone that scanned the code: the newest one that wasn't
/// already waiting when the code went up. Sunshine keeps requests listed after the phone gives
/// up, so the same phone may have older ones waiting (from before the code, or from an attempt
/// it retried); those are turned down. Requests from other devices are left alone (they may be
/// someone using Sunshine's own PIN page).
///
/// `seen` holds request IDs in the order they were first seen. Sunshine's list has no order of
/// its own, so "newest" means last to appear there.
pub fn choose<'a>(pending: &'a [PendingPairing], waiting_before: &HashSet<String>, seen: &[String]) -> Choice<'a> {
    let order = |p: &PendingPairing| seen.iter().position(|id| *id == p.id).unwrap_or(usize::MAX);
    let answer = pending.iter().filter(|p| !waiting_before.contains(&p.id)).max_by_key(|p| order(p));
    let cancel = match answer {
        Some(a) => pending.iter().filter(|p| p.id != a.id && p.address == a.address).collect(),
        None => Vec::new(),
    };
    Choice { answer, cancel }
}

/// Adds the IDs in `pending` that haven't been seen yet to `seen`, in the order given.
pub fn note_seen(pending: &[PendingPairing], seen: &mut Vec<String>) {
    for p in pending {
        if !seen.contains(&p.id) {
            seen.push(p.id.clone());
        }
    }
}

/// A device in `now` that wasn't in `before`: the phone that just paired.
pub fn new_client<'a>(now: &'a [Client], before: &HashSet<String>) -> Option<&'a Client> {
    now.iter().find(|c| !before.contains(&c.uuid))
}

/// [`DEVICE_NAME`], numbered so it doesn't clash with a device that's already paired.
pub fn device_name(existing: &[Client]) -> String {
    let taken = |name: &str| existing.iter().any(|c| c.name.eq_ignore_ascii_case(name));
    if !taken(DEVICE_NAME) {
        return DEVICE_NAME.to_string();
    }
    (2..).map(|n| format!("{DEVICE_NAME} {n}")).find(|n| !taken(n)).unwrap_or_default()
}

/// This PC's addresses on its local networks, best first: the one the default route goes out
/// of, then any other adapter that is up and has a gateway (virtual adapters for WSL, Hyper-V
/// and the like have none, so they're left out).
pub fn lan_addresses() -> Vec<Ipv4Addr> {
    let mut out: Vec<Ipv4Addr> = default_route_address().into_iter().collect();
    for ip in adapter_addresses() {
        if !out.contains(&ip) {
            out.push(ip);
        }
    }
    out.retain(|ip| usable(*ip));
    out
}

fn usable(ip: Ipv4Addr) -> bool {
    !ip.is_loopback() && !ip.is_link_local() && !ip.is_unspecified() && !ip.is_multicast() && !ip.is_broadcast()
}

/// The local address the OS would use to reach the internet. A UDP `connect` only picks a
/// route; nothing is sent.
fn default_route_address() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).ok()?;
    match socket.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(ip) => Some(ip),
        _ => None,
    }
}

fn adapter_addresses() -> Vec<Ipv4Addr> {
    use windows_sys::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, NO_ERROR};
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GAA_FLAG_INCLUDE_GATEWAYS, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST,
        GetAdaptersAddresses, IF_TYPE_SOFTWARE_LOOPBACK, IP_ADAPTER_ADDRESSES_LH,
    };
    use windows_sys::Win32::NetworkManagement::Ndis::IfOperStatusUp;
    use windows_sys::Win32::Networking::WinSock::{AF_INET, SOCKADDR_IN};

    let flags = GAA_FLAG_INCLUDE_GATEWAYS | GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
    // u64 elements keep the buffer aligned for the structs Windows writes into it.
    let mut size: u32 = 16 * 1024;
    let mut buf: Vec<u64>;
    loop {
        buf = vec![0u64; (size as usize).div_ceil(8)];
        // SAFETY: the buffer is `size` bytes long and suitably aligned.
        let ret = unsafe {
            GetAdaptersAddresses(AF_INET as u32, flags, std::ptr::null(), buf.as_mut_ptr().cast(), &mut size)
        };
        match ret {
            NO_ERROR => break,
            ERROR_BUFFER_OVERFLOW => continue,
            _ => return Vec::new(),
        }
    }

    let mut out = Vec::new();
    let mut adapter = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
    // SAFETY: Windows filled the buffer with a linked list that lives as long as `buf`.
    unsafe {
        while let Some(a) = adapter.as_ref() {
            if a.OperStatus == IfOperStatusUp && a.IfType != IF_TYPE_SOFTWARE_LOOPBACK && !a.FirstGatewayAddress.is_null() {
                let mut unicast = a.FirstUnicastAddress;
                while let Some(u) = unicast.as_ref() {
                    if let Some(sa) = u.Address.lpSockaddr.as_ref() {
                        if sa.sa_family == AF_INET {
                            let sin = &*(u.Address.lpSockaddr as *const SOCKADDR_IN);
                            out.push(Ipv4Addr::from(u32::from_be(sin.sin_addr.S_un.S_addr)));
                        }
                    }
                    unicast = u.Next;
                }
            }
            adapter = a.Next;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(id: &str) -> PendingPairing {
        PendingPairing { id: id.into(), address: "192.168.1.20".into(), name: "roth".into() }
    }

    fn client(uuid: &str, name: &str) -> Client {
        Client { uuid: uuid.into(), name: name.into() }
    }

    #[test]
    fn builds_the_contract_uri() {
        let code = PairCode {
            name: "Gaming PC & Co".into(),
            addresses: vec![Ipv4Addr::new(192, 168, 1, 10), Ipv4Addr::new(10, 0, 0, 5)],
            port: 47989,
            pin: "0427".into(),
            fingerprint: "ab".repeat(32),
        };
        assert_eq!(
            code.uri(),
            format!("pocketdeck://pair?v=1&n=Gaming%20PC%20%26%20Co&a=192.168.1.10,10.0.0.5&p=47989&pin=0427&fp={}", "ab".repeat(32))
        );
    }

    #[test]
    fn encodes_non_ascii_names() {
        assert_eq!(encode("Zoë-PC_1.~"), "Zo%C3%AB-PC_1.~");
    }

    fn pending_from(id: &str, address: &str) -> PendingPairing {
        PendingPairing { id: id.into(), address: address.into(), name: "roth".into() }
    }

    fn ids(list: &[&PendingPairing]) -> Vec<String> {
        list.iter().map(|p| p.id.clone()).collect()
    }

    #[test]
    fn waits_while_only_old_requests_are_there() {
        let before: HashSet<String> = ["old".to_string()].into();
        let list = [pending("old")];
        assert_eq!(choose(&list, &before, &["old".into()]), Choice { answer: None, cancel: vec![] });
    }

    #[test]
    fn a_new_request_goes_first_and_the_same_phones_old_one_is_turned_down() {
        let before: HashSet<String> = ["old".to_string()].into();
        let list = [pending("new"), pending("old")];
        let c = choose(&list, &before, &["old".into(), "new".into()]);
        assert_eq!(c.answer.map(|p| p.id.as_str()), Some("new"));
        assert_eq!(ids(&c.cancel), ["old"]);
    }

    #[test]
    fn a_retry_beats_the_attempt_it_replaces() {
        // Both appeared after the code went up; "second" was seen later.
        let list = [pending("second"), pending("first")];
        let c = choose(&list, &HashSet::new(), &["first".into(), "second".into()]);
        assert_eq!(c.answer.map(|p| p.id.as_str()), Some("second"));
        assert_eq!(ids(&c.cancel), ["first"]);
    }

    #[test]
    fn other_devices_are_left_alone() {
        let before: HashSet<String> = ["laptop".to_string()].into();
        let list = [pending_from("laptop", "192.168.1.50"), pending_from("phone", "192.168.1.20")];
        let c = choose(&list, &before, &["laptop".into(), "phone".into()]);
        assert_eq!(c.answer.map(|p| p.id.as_str()), Some("phone"));
        assert!(c.cancel.is_empty());
    }

    #[test]
    fn remembers_first_seen_order() {
        let mut seen = vec!["a".to_string()];
        note_seen(&[pending("b"), pending("a"), pending("c")], &mut seen);
        assert_eq!(seen, ["a", "b", "c"]);
    }

    #[test]
    fn spots_the_new_client() {
        let before: HashSet<String> = ["A".to_string()].into();
        assert_eq!(new_client(&[client("A", "Pixel")], &before), None);
        assert_eq!(new_client(&[client("A", "Pixel"), client("B", "x")], &before).map(|c| c.uuid.as_str()), Some("B"));
    }

    #[test]
    fn numbers_device_names() {
        assert_eq!(device_name(&[client("A", "Pixel")]), "PocketDeck phone");
        assert_eq!(device_name(&[client("A", "PocketDeck phone"), client("B", "PocketDeck phone 2")]), "PocketDeck phone 3");
    }

    #[test]
    fn keeps_only_usable_addresses() {
        assert!(usable(Ipv4Addr::new(192, 168, 1, 4)));
        assert!(!usable(Ipv4Addr::new(169, 254, 3, 4)));
        assert!(!usable(Ipv4Addr::LOCALHOST));
    }
}
