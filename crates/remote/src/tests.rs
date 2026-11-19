//! Loopback tests: a local test relay, a host forwarding to echo servers that stand in for
//! Sunshine on 127.0.0.2, and a client listening on 127.0.0.1 on the same port numbers.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream, UdpSocket};
use std::sync::Arc;
use std::time::Duration;

use super::*;

const SUNSHINE: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 2);

struct Relay {
    url: String,
    _server: iroh_relay::server::Server,
    _rt: tokio::runtime::Runtime,
}

fn relay() -> Relay {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (_map, url, server) = rt.block_on(iroh::test_utils::run_relay_server()).unwrap();
    Relay { url: url.to_string(), _server: server, _rt: rt }
}

fn config(relay: &Relay) -> Config {
    let mut c = Config::new(generate_secret(), Some(relay.url.clone()));
    c.insecure_relay_tls = true;
    c.sunshine_ip = SUNSHINE;
    c
}

/// Echo servers on 127.0.0.2 for every TCP port and UDP channel. Returns their ports.
fn fake_sunshine() -> Ports {
    let tcp = || {
        let l = TcpListener::bind((SUNSHINE, 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for s in l.incoming() {
                let Ok(mut s) = s else { continue };
                std::thread::spawn(move || {
                    let mut buf = [0u8; 4096];
                    while let Ok(n) = s.read(&mut buf) {
                        if n == 0 || s.write_all(&buf[..n]).is_err() {
                            break;
                        }
                    }
                });
            }
        });
        port
    };
    let udp = || {
        let s = UdpSocket::bind((SUNSHINE, 0)).unwrap();
        let port = s.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let mut buf = [0u8; 65536];
            while let Ok((n, from)) = s.recv_from(&mut buf) {
                let _ = s.send_to(&buf[..n], from);
            }
        });
        port
    };
    Ports { https: tcp(), http: tcp(), rtsp: tcp(), display: tcp(), video: udp(), control: udp(), audio: udp() }
}

#[test]
fn tcp_and_udp_echo_direct() {
    let relay = relay();
    let ports = fake_sunshine();
    let host = Host::start(config(&relay), Arc::new(|_| true), ports).unwrap();
    assert!(host.wait_online(Duration::from_secs(10)));

    let client = Client::start(config(&relay), host.id()).unwrap();
    assert_eq!(client.ports(), ports);
    assert!(client.status().link.is_direct(), "{:?}", client.status());
    assert_eq!(client.status().link, Link::Lan, "both ends are on this machine");

    for port in [ports.https, ports.http, ports.rtsp, ports.display] {
        let mut s = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let msg = format!("hello {port}");
        s.write_all(msg.as_bytes()).unwrap();
        let mut buf = vec![0u8; msg.len()];
        s.read_exact(&mut buf).unwrap();
        assert_eq!(buf, msg.as_bytes());
    }

    let moonlight = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    // Datagrams may be lost (Moonlight copes with that), so try a few times.
    moonlight.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
    for port in [ports.video, ports.control, ports.audio] {
        let payload = vec![port as u8; 1050];
        let mut buf = [0u8; 2048];
        let echoed = (0..10).find_map(|_| {
            moonlight.send_to(&payload, (Ipv4Addr::LOCALHOST, port)).unwrap();
            moonlight.recv_from(&mut buf).ok()
        });
        let (n, from) = echoed.unwrap_or_else(|| panic!("no echo on UDP {port}"));
        assert_eq!(from.port(), port);
        assert_eq!(&buf[..n], &payload[..]);
    }

    assert_eq!(host.status().peers, 1);
    client.stop();
    host.stop();
}

#[test]
fn peer_not_allowed_is_refused() {
    let relay = relay();
    let ports = fake_sunshine();
    let host = Host::start(config(&relay), Arc::new(|_| false), ports).unwrap();
    assert!(host.wait_online(Duration::from_secs(10)));
    match Client::start(config(&relay), host.id()) {
        Err(Error::Hello(e)) => assert!(e.contains("paired"), "{e}"),
        Err(e) => panic!("unexpected error {e:?}"),
        Ok(_) => panic!("connected although not allowed"),
    }
}

#[test]
fn relay_only_is_refused() {
    let relay = relay();
    let ports = fake_sunshine();
    let host = Host::start(config(&relay), Arc::new(|_| true), ports).unwrap();
    assert!(host.wait_online(Duration::from_secs(10)));
    let mut c = config(&relay);
    c.relay_only = true;
    match Client::start(c, host.id()) {
        Err(Error::NoDirectPath) => {}
        Err(e) => panic!("unexpected error {e:?}"),
        Ok(_) => panic!("streamed without a direct path"),
    }
    // The local ports were never opened.
    assert!(TcpStream::connect_timeout(&(Ipv4Addr::LOCALHOST, ports.http).into(), Duration::from_millis(300)).is_err());
}
