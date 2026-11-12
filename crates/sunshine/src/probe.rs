//! Asking the local Sunshine server who it is, over its GameStream HTTP port.
//!
//! `GET /serverinfo` works without pairing, which makes it a cheap "is it answering" check.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerInfo {
    pub hostname: String,
    pub https_port: Option<u16>,
    /// A stream is running right now.
    pub busy: bool,
}

const TIMEOUT: Duration = Duration::from_millis(800);

pub fn server_info(http_port: u16) -> std::io::Result<ServerInfo> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, http_port));
    let mut stream = TcpStream::connect_timeout(&addr, TIMEOUT)?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.set_write_timeout(Some(TIMEOUT))?;
    write!(stream, "GET /serverinfo HTTP/1.0\r\nHost: 127.0.0.1:{http_port}\r\nConnection: close\r\n\r\n")?;
    let mut body = Vec::with_capacity(2048);
    stream.take(64 * 1024).read_to_end(&mut body)?;
    parse(&String::from_utf8_lossy(&body))
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "not a GameStream server"))
}

fn tag<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find("</")? + start;
    Some(xml[start..end].trim())
}

pub fn parse(response: &str) -> Option<ServerInfo> {
    let hostname = tag(response, "hostname")?.to_string();
    Some(ServerInfo {
        hostname,
        https_port: tag(response, "HttpsPort").and_then(|p| p.parse().ok()),
        busy: tag(response, "state").is_some_and(|s| s.ends_with("_BUSY")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_serverinfo() {
        let r = "HTTP/1.1 200 OK\r\n\r\n<?xml version=\"1.0\"?><root status_code=\"200\"><hostname>GAMING-PC</hostname>\
                 <HttpsPort>47984</HttpsPort><state>SUNSHINE_SERVER_FREE</state></root>";
        assert_eq!(
            parse(r),
            Some(ServerInfo { hostname: "GAMING-PC".into(), https_port: Some(47984), busy: false })
        );
        assert!(parse(&r.replace("FREE", "BUSY")).unwrap().busy);
        assert_eq!(parse("<html>nope</html>"), None);
    }
}
