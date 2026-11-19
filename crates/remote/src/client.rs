//! The phone end: dials the PC, and listens on `127.0.0.1` on Sunshine's port numbers so
//! Moonlight can treat the tunnel as the PC.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use iroh::endpoint::{Connection, ConnectionError, VarInt};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayUrl};
use tokio::net::{TcpListener, UdpSocket};
use tokio::runtime::Runtime;

use crate::link::{self, LinkState, Status};
use crate::wire::{self, CLOSE_NO_DIRECT, CLOSE_NOT_ALLOWED, CLOSE_STOPPED, Ports, Tcp, Udp};
use crate::{Config, DIRECT_TIMEOUT, Error, MIN_DATAGRAM};

pub struct Client {
    rt: Option<Runtime>,
    endpoint: Endpoint,
    conn: Connection,
    state: Arc<LinkState>,
    ports: Ports,
}

impl Client {
    /// Dials the PC, waits for a direct path, and opens the local ports. Returns once Moonlight
    /// can connect to `127.0.0.1`.
    pub fn start(config: Config, pc: EndpointId) -> Result<Client, Error> {
        let rt = crate::runtime()?;
        let started = rt.block_on(start(&config, pc));
        match started {
            Ok((endpoint, conn, state, ports)) => Ok(Client { rt: Some(rt), endpoint, conn, state, ports }),
            Err(e) => {
                rt.shutdown_timeout(Duration::from_secs(1));
                Err(e)
            }
        }
    }

    /// The PC's ports, which are also the ports listening on this device's `127.0.0.1`.
    pub fn ports(&self) -> Ports {
        self.ports
    }

    pub fn status(&self) -> Status {
        let (link, rtt) = self.state.get();
        Status { link, rtt, peers: 1 }
    }

    /// Whether the connection has ended (stopped, PC gone, or the direct path lost).
    pub fn is_closed(&self) -> bool {
        self.conn.close_reason().is_some()
    }

    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        if let Some(rt) = self.rt.take() {
            self.conn.close(VarInt::from_u32(CLOSE_STOPPED), b"stopped");
            let endpoint = self.endpoint.clone();
            rt.block_on(async move {
                let _ = tokio::time::timeout(Duration::from_secs(2), endpoint.close()).await;
            });
            rt.shutdown_timeout(Duration::from_secs(1));
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn closed_by_pc(e: &ConnectionError) -> Option<Error> {
    if let ConnectionError::ApplicationClosed(close) = e {
        if close.error_code == VarInt::from_u32(CLOSE_NOT_ALLOWED) {
            return Some(Error::Hello("this phone isn't paired with the PC any more; pair again at home".into()));
        }
        if close.error_code == VarInt::from_u32(CLOSE_NO_DIRECT) {
            return Some(Error::NoDirectPath);
        }
    }
    None
}

async fn start(config: &Config, pc: EndpointId) -> Result<(Endpoint, Connection, Arc<LinkState>, Ports), Error> {
    let endpoint = crate::bind(config, false).await?;
    let result = dial(config, &endpoint, pc).await;
    if result.is_err() {
        let _ = tokio::time::timeout(Duration::from_secs(2), endpoint.close()).await;
    }
    result.map(|(conn, state, ports)| (endpoint, conn, state, ports))
}

async fn dial(
    config: &Config,
    endpoint: &Endpoint,
    pc: EndpointId,
) -> Result<(Connection, Arc<LinkState>, Ports), Error> {
    let mut addr = EndpointAddr::new(pc);
    if let Some(relay) = &config.relay {
        let url: RelayUrl = relay.parse().map_err(|e| Error::Config(format!("bad relay URL {relay}: {e}")))?;
        addr = addr.with_relay_url(url);
    }
    let conn = tokio::time::timeout(DIRECT_TIMEOUT, endpoint.connect(addr, crate::ALPN))
        .await
        .map_err(|_| Error::Connect("timed out; is the PC on and online?".into()))?
        .map_err(|e| Error::Connect(e.to_string()))?;

    let hello = async {
        let (mut send, mut recv) = conn.open_bi().await.map_err(|e| e.to_string())?;
        wire::hello_client(&mut send, &mut recv).await
    };
    let ports = match tokio::time::timeout(DIRECT_TIMEOUT, hello).await {
        Ok(Ok(ports)) => ports,
        Ok(Err(e)) => {
            return Err(conn.close_reason().as_ref().and_then(closed_by_pc).unwrap_or(Error::Hello(e)));
        }
        Err(_) => return Err(Error::Hello("the PC didn't answer".into())),
    };

    let state = LinkState::new();
    tokio::spawn(link::watch(endpoint.clone(), conn.clone(), state.clone()));

    // Nothing is forwarded until there is a direct path.
    let deadline = Instant::now() + DIRECT_TIMEOUT;
    while !state.is_direct() {
        if Instant::now() > deadline || conn.close_reason().is_some() {
            conn.close(VarInt::from_u32(CLOSE_NO_DIRECT), b"no direct path");
            return Err(Error::NoDirectPath);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    match conn.max_datagram_size() {
        Some(n) if n >= MIN_DATAGRAM => {}
        other => {
            conn.close(VarInt::from_u32(CLOSE_STOPPED), b"datagrams too small");
            return Err(Error::DatagramsTooSmall(other.unwrap_or(0)));
        }
    }

    if let Err(e) = listen(&conn, &state, ports).await {
        conn.close(VarInt::from_u32(CLOSE_STOPPED), b"local bind failed");
        return Err(e);
    }
    Ok((conn, state, ports))
}

/// Opens the local ports and starts forwarding. Tasks end when the connection closes.
async fn listen(conn: &Connection, state: &Arc<LinkState>, ports: Ports) -> Result<(), Error> {
    let local = |port: u16| SocketAddr::from((Ipv4Addr::LOCALHOST, port));

    let mut listeners = Vec::new();
    for port in Tcp::ALL {
        let listener = TcpListener::bind(local(ports.tcp(port)))
            .await
            .map_err(|e| Error::Bind(format!("TCP {}: {e}", ports.tcp(port))))?;
        listeners.push((port, listener));
    }
    let mut sockets = Vec::new();
    for channel in Udp::ALL {
        let socket = UdpSocket::bind(local(ports.udp(channel)))
            .await
            .map_err(|e| Error::Bind(format!("UDP {}: {e}", ports.udp(channel))))?;
        sockets.push(Arc::new(socket));
    }

    for (port, listener) in listeners {
        let conn = conn.clone();
        tokio::spawn(async move {
            loop {
                let accepted = tokio::select! {
                    _ = conn.closed() => break,
                    accepted = listener.accept() => accepted,
                };
                let Ok((tcp, _)) = accepted else { continue };
                let conn = conn.clone();
                tokio::spawn(async move {
                    let Ok((mut send, recv)) = conn.open_bi().await else { return };
                    if send.write_all(&[port as u8]).await.is_err() {
                        return;
                    }
                    wire::splice(tcp, send, recv).await;
                });
            }
        });
    }

    // Moonlight's source port per channel, learned from its first packet.
    let peers: Arc<Mutex<[Option<SocketAddr>; 3]>> = Arc::default();

    for (channel, socket) in Udp::ALL.into_iter().zip(sockets.iter().cloned()) {
        let conn = conn.clone();
        let state = state.clone();
        let peers = peers.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 64 * 1024];
            buf[0] = channel as u8;
            loop {
                let received = tokio::select! {
                    _ = conn.closed() => break,
                    r = socket.recv_from(&mut buf[1..]) => r,
                };
                let Ok((n, from)) = received else { continue };
                peers.lock().unwrap()[channel as usize] = Some(from);
                if state.is_direct() {
                    let _ = conn.send_datagram(Bytes::copy_from_slice(&buf[..n + 1]));
                }
            }
        });
    }

    let conn = conn.clone();
    tokio::spawn(async move {
        while let Ok(datagram) = conn.read_datagram().await {
            let Some((&tag, payload)) = datagram.split_first() else { continue };
            let Some(channel) = Udp::from_tag(tag) else { continue };
            let to = peers.lock().unwrap()[channel as usize];
            if let Some(to) = to {
                let _ = sockets[channel as usize].send_to(payload, to).await;
            }
        }
    });
    Ok(())
}
