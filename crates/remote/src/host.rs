//! The PC end: accepts phones and forwards their streams and datagrams to Sunshine on
//! `127.0.0.1`.

use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use iroh::endpoint::{Connection, VarInt};
use iroh::{Endpoint, EndpointId};
use tokio::net::{TcpStream, UdpSocket};
use tokio::runtime::Runtime;
use tokio::task::JoinSet;

use crate::link::{self, Link, LinkState, Status};
use crate::wire::{self, CLOSE_NOT_ALLOWED, CLOSE_STOPPED, Ports, Tcp, Udp};
use crate::{Config, DIRECT_TIMEOUT, Error};

/// Decides whether a phone may connect, by its endpoint id. Called for every new connection, so
/// it can check the current pairing state.
pub type Allow = Arc<dyn Fn(EndpointId) -> bool + Send + Sync>;

type Conns = Arc<Mutex<Vec<(usize, Arc<LinkState>)>>>;

pub struct Host {
    rt: Option<Runtime>,
    endpoint: Endpoint,
    conns: Conns,
}

impl Host {
    /// Opens the endpoint and starts accepting phones in the background.
    pub fn start(config: Config, allow: Allow, ports: Ports) -> Result<Host, Error> {
        let rt = crate::runtime()?;
        let endpoint = rt.block_on(crate::bind(&config, true))?;
        let conns: Conns = Arc::default();
        rt.spawn(accept_loop(endpoint.clone(), allow, ports, config.sunshine_ip, conns.clone()));
        Ok(Host { rt: Some(rt), endpoint, conns })
    }

    /// This PC's endpoint id, which phones dial.
    pub fn id(&self) -> EndpointId {
        self.endpoint.id()
    }

    /// Waits up to `timeout` until the endpoint is connected to its relay. Returns whether it is.
    /// Always false without a relay.
    pub fn wait_online(&self, timeout: Duration) -> bool {
        let rt = self.rt.as_ref().expect("running");
        rt.block_on(async { tokio::time::timeout(timeout, self.endpoint.online()).await.is_ok() })
    }

    /// The best link among the connected phones, or `Connecting` (waiting for phones) when none
    /// is connected.
    pub fn status(&self) -> Status {
        let conns = self.conns.lock().unwrap();
        let mut best = Status { link: Link::Connecting, rtt: None, peers: conns.len() };
        let rank = |l: Link| match l {
            Link::Lan => 3,
            Link::Internet => 2,
            Link::Relayed => 1,
            _ => 0,
        };
        for (_, state) in conns.iter() {
            let (link, rtt) = state.get();
            if rank(link) > rank(best.link) {
                best.link = link;
                best.rtt = rtt;
            }
        }
        best
    }

    /// Closes every connection and the endpoint.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        if let Some(rt) = self.rt.take() {
            let endpoint = self.endpoint.clone();
            rt.block_on(async move {
                let _ = tokio::time::timeout(Duration::from_secs(2), endpoint.close()).await;
            });
            rt.shutdown_timeout(Duration::from_secs(1));
        }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.shutdown();
    }
}

async fn accept_loop(endpoint: Endpoint, allow: Allow, ports: Ports, sunshine: Ipv4Addr, conns: Conns) {
    while let Some(incoming) = endpoint.accept().await {
        let allow = allow.clone();
        let conns = conns.clone();
        let endpoint = endpoint.clone();
        tokio::spawn(async move {
            let Ok(conn) = incoming.await else { return };
            if !allow(conn.remote_id()) {
                conn.close(VarInt::from_u32(CLOSE_NOT_ALLOWED), b"not allowed");
                return;
            }
            serve(endpoint, conn, ports, sunshine, conns).await;
        });
    }
}

/// Runs one phone's tunnel until the connection closes.
async fn serve(endpoint: Endpoint, conn: Connection, ports: Ports, sunshine: Ipv4Addr, conns: Conns) {
    let hello = async {
        let (mut send, mut recv) = conn.accept_bi().await.map_err(|e| e.to_string())?;
        wire::hello_host(&mut send, &mut recv, ports).await
    };
    match tokio::time::timeout(DIRECT_TIMEOUT, hello).await {
        Ok(Ok(())) => {}
        _ => {
            conn.close(VarInt::from_u32(CLOSE_STOPPED), b"bad hello");
            return;
        }
    }

    let state = LinkState::new();
    let key = conn.stable_id();
    conns.lock().unwrap().push((key, state.clone()));
    let mut tasks = JoinSet::new();
    tasks.spawn(link::watch(endpoint, conn.clone(), state.clone()));

    // One local UDP socket per channel, "connected" to Sunshine's port: Sunshine answers the
    // address our packets come from, which is this socket.
    let mut sockets = Vec::new();
    for channel in Udp::ALL {
        let socket = match UdpSocket::bind((sunshine, 0)).await {
            Ok(s) => s,
            Err(_) => break,
        };
        if socket.connect((sunshine, ports.udp(channel))).await.is_err() {
            break;
        }
        sockets.push(Arc::new(socket));
    }
    if sockets.len() != Udp::ALL.len() {
        conn.close(VarInt::from_u32(CLOSE_STOPPED), b"udp bind failed");
        conns.lock().unwrap().retain(|(k, _)| *k != key);
        return;
    }

    // Sunshine -> phone.
    for (channel, socket) in Udp::ALL.into_iter().zip(sockets.iter().cloned()) {
        let conn = conn.clone();
        let state = state.clone();
        tasks.spawn(async move {
            let mut buf = vec![0u8; 64 * 1024];
            buf[0] = channel as u8;
            while let Ok(n) = socket.recv(&mut buf[1..]).await {
                if state.is_direct() {
                    let _ = conn.send_datagram(Bytes::copy_from_slice(&buf[..n + 1]));
                }
            }
        });
    }

    // Phone -> Sunshine.
    {
        let conn = conn.clone();
        let sockets = sockets.clone();
        tasks.spawn(async move {
            while let Ok(datagram) = conn.read_datagram().await {
                let Some((&tag, payload)) = datagram.split_first() else { continue };
                if let Some(channel) = Udp::from_tag(tag) {
                    let _ = sockets[channel as usize].send(payload).await;
                }
            }
        });
    }

    // TCP: one stream per connection, first byte says which port.
    while let Ok((send, mut recv)) = conn.accept_bi().await {
        tasks.spawn(async move {
            let mut tag = [0u8; 1];
            if recv.read_exact(&mut tag).await.is_err() {
                return;
            }
            let Some(port) = Tcp::from_tag(tag[0]) else { return };
            let Ok(tcp) = TcpStream::connect((sunshine, ports.tcp(port))).await else {
                return;
            };
            wire::splice(tcp, send, recv).await;
        });
    }

    conns.lock().unwrap().retain(|(k, _)| *k != key);
    tasks.abort_all();
}
