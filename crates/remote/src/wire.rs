//! The tunnel's wire format. See `docs/remote-play.md`.

use iroh::endpoint::{RecvStream, SendStream};

/// The first bytes of the client's hello.
pub const MAGIC: [u8; 2] = *b"PD";

/// Protocol version. A host refuses clients with a different version.
pub const VERSION: u8 = 1;

/// Host's reply byte when the version is accepted.
pub const HELLO_OK: u8 = 0;

/// Host's reply byte when the version is not supported. Followed by the host's version.
pub const HELLO_BAD_VERSION: u8 = 1;

/// Connection close codes.
pub const CLOSE_NOT_ALLOWED: u32 = 1;
pub const CLOSE_NO_DIRECT: u32 = 2;
pub const CLOSE_STOPPED: u32 = 3;

/// The TCP ports, in tag order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Tcp {
    Https = 0,
    Http = 1,
    Rtsp = 2,
    Display = 3,
}

impl Tcp {
    pub const ALL: [Tcp; 4] = [Tcp::Https, Tcp::Http, Tcp::Rtsp, Tcp::Display];

    pub fn from_tag(tag: u8) -> Option<Tcp> {
        Tcp::ALL.get(tag as usize).copied()
    }
}

/// The UDP channels, in tag order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Udp {
    Video = 0,
    Control = 1,
    Audio = 2,
}

impl Udp {
    pub const ALL: [Udp; 3] = [Udp::Video, Udp::Control, Udp::Audio];

    pub fn from_tag(tag: u8) -> Option<Udp> {
        Udp::ALL.get(tag as usize).copied()
    }
}

/// Sunshine's ports on the PC. The phone listens on the same numbers on `127.0.0.1`, so the
/// addresses Sunshine puts in its replies (`rtsp://127.0.0.1:48010`, the UDP ports in RTSP
/// SETUP) are right on both ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ports {
    pub https: u16,
    pub http: u16,
    pub rtsp: u16,
    /// Pocket Deck's own display endpoint (`base + 30`).
    pub display: u16,
    pub video: u16,
    pub control: u16,
    pub audio: u16,
}

impl Ports {
    /// Sunshine's defaults with the display endpoint at base + 30.
    pub fn from_base(base: u16) -> Ports {
        let at = |offset: i32| (base as i32 + offset).clamp(1, u16::MAX as i32) as u16;
        Ports {
            https: at(-5),
            http: base,
            rtsp: at(21),
            display: at(30),
            video: at(9),
            control: at(10),
            audio: at(11),
        }
    }

    pub fn tcp(&self, port: Tcp) -> u16 {
        match port {
            Tcp::Https => self.https,
            Tcp::Http => self.http,
            Tcp::Rtsp => self.rtsp,
            Tcp::Display => self.display,
        }
    }

    pub fn udp(&self, channel: Udp) -> u16 {
        match channel {
            Udp::Video => self.video,
            Udp::Control => self.control,
            Udp::Audio => self.audio,
        }
    }

    fn to_bytes(self) -> [u8; 14] {
        let mut out = [0u8; 14];
        let all = [self.https, self.http, self.rtsp, self.display, self.video, self.control, self.audio];
        for (i, p) in all.iter().enumerate() {
            out[i * 2..i * 2 + 2].copy_from_slice(&p.to_be_bytes());
        }
        out
    }

    fn from_bytes(b: &[u8; 14]) -> Ports {
        let p = |i: usize| u16::from_be_bytes([b[i * 2], b[i * 2 + 1]]);
        Ports { https: p(0), http: p(1), rtsp: p(2), display: p(3), video: p(4), control: p(5), audio: p(6) }
    }
}

/// Client side of the hello: `"PD" version`, then reads `HELLO_OK ports` back.
pub async fn hello_client(send: &mut SendStream, recv: &mut RecvStream) -> Result<Ports, String> {
    send.write_all(&[MAGIC[0], MAGIC[1], VERSION]).await.map_err(|e| e.to_string())?;
    let _ = send.finish();
    let mut status = [0u8; 1];
    recv.read_exact(&mut status).await.map_err(|e| e.to_string())?;
    match status[0] {
        HELLO_OK => {
            let mut ports = [0u8; 14];
            recv.read_exact(&mut ports).await.map_err(|e| e.to_string())?;
            Ok(Ports::from_bytes(&ports))
        }
        HELLO_BAD_VERSION => {
            let mut v = [0u8; 1];
            let _ = recv.read_exact(&mut v).await;
            Err(format!("the PC speaks tunnel version {}, this app speaks {VERSION}; update both", v[0]))
        }
        other => Err(format!("unexpected hello reply {other}")),
    }
}

/// Host side of the hello. Returns whether the client was accepted.
pub async fn hello_host(send: &mut SendStream, recv: &mut RecvStream, ports: Ports) -> Result<(), String> {
    let mut hello = [0u8; 3];
    recv.read_exact(&mut hello).await.map_err(|e| e.to_string())?;
    if hello[..2] != MAGIC {
        return Err("not a Pocket Deck tunnel".into());
    }
    if hello[2] != VERSION {
        send.write_all(&[HELLO_BAD_VERSION, VERSION]).await.map_err(|e| e.to_string())?;
        let _ = send.finish();
        return Err(format!("client speaks version {}", hello[2]));
    }
    let mut reply = [0u8; 15];
    reply[0] = HELLO_OK;
    reply[1..].copy_from_slice(&ports.to_bytes());
    send.write_all(&reply).await.map_err(|e| e.to_string())?;
    let _ = send.finish();
    Ok(())
}

/// Copies bytes both ways between a TCP connection and a QUIC stream pair until both sides
/// have finished.
pub async fn splice(tcp: tokio::net::TcpStream, mut send: SendStream, mut recv: RecvStream) {
    let _ = tcp.set_nodelay(true);
    let (mut tcp_r, mut tcp_w) = tcp.into_split();
    let up = async {
        let _ = tokio::io::copy(&mut tcp_r, &mut send).await;
        let _ = send.finish();
    };
    let down = async {
        let _ = tokio::io::copy(&mut recv, &mut tcp_w).await;
        let _ = tokio::io::AsyncWriteExt::shutdown(&mut tcp_w).await;
    };
    tokio::join!(up, down);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ports_round_trip() {
        let p = Ports::from_base(47989);
        assert_eq!(p.https, 47984);
        assert_eq!(p.rtsp, 48010);
        assert_eq!(p.display, 48019);
        assert_eq!((p.video, p.control, p.audio), (47998, 47999, 48000));
        assert_eq!(Ports::from_bytes(&p.to_bytes()), p);
    }
}
