//! The endpoint the phone calls before it streams: HTTPS on Sunshine's base port + 30.
//!
//! No new pairing and no new secrets. The PC answers with Sunshine's own server certificate,
//! which the phone already pinned when it paired, and only phones whose client certificate is
//! on Sunshine's paired list get past the TLS handshake. The service runs as SYSTEM, so it can
//! read Sunshine's key; nothing else on the PC needs to.
//!
//! The contract is `docs/virtual-display.md`.

use crate::service::{Backend, Controller};
use crate::{Mode, PORT_OFFSET, log};
use rustls::client::danger::HandshakeSignatureValid;
use rustls::crypto::CryptoProvider;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, ServerConfig, ServerConnection, SignatureScheme, StreamOwned};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

pub const PATH_STATUS: &str = "/pocketdeck/v1/display";
pub const PATH_PREPARE: &str = "/pocketdeck/v1/display/prepare";
pub const PATH_KEEPALIVE: &str = "/pocketdeck/v1/display/keepalive";
pub const PATH_RELEASE: &str = "/pocketdeck/v1/display/release";

const MAX_HEAD: usize = 8 * 1024;
const MAX_BODY: usize = 4 * 1024;
const MAX_CONNECTIONS: usize = 8;

/// Where Sunshine keeps what the endpoint needs, and the port to listen on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sunshine {
    pub cert: PathBuf,
    pub key: PathBuf,
    pub state: PathBuf,
    pub port: u16,
}

/// Reads Sunshine's settings for its certificate, key and paired-devices file (its defaults
/// unless `cert`, `pkey` or `file_state` say otherwise) and its base port.
pub fn sunshine() -> Result<Sunshine, String> {
    let install = pocketdeck_sunshine::detect::find().ok_or("Sunshine isn't installed.")?;
    let dir = install.config_dir();
    let conf = std::fs::read_to_string(install.config_file()).map(|t| pocketdeck_sunshine::conf::parse(&t)).unwrap_or_default();
    let ports = pocketdeck_sunshine::conf::read_ports(&install.config_file());
    let path = |key: &str, default: &str| {
        let p = conf.get(key).map(|v| PathBuf::from(v.trim_matches('"'))).unwrap_or_else(|| PathBuf::from(default));
        if p.is_absolute() { p } else { dir.join(p) }
    };
    Ok(Sunshine {
        cert: path("cert", r"credentials\cacert.pem"),
        key: path("pkey", r"credentials\cakey.pem"),
        state: path("file_state", "sunshine_state.json"),
        port: ports.http.saturating_add(PORT_OFFSET),
    })
}

/// The DER client certificates of every paired device in Sunshine's state file.
pub fn paired_certs(state_file: &Path) -> Vec<Vec<u8>> {
    let Ok(text) = std::fs::read_to_string(state_file) else { return Vec::new() };
    let Ok(value) = serde_json::from_str::<Value>(&text) else { return Vec::new() };
    paired_certs_in(&value)
}

pub fn paired_certs_in(state: &Value) -> Vec<Vec<u8>> {
    let mut pems: Vec<&str> = Vec::new();
    // Current Sunshine: root.named_devices[].cert. Older: root.devices[].certs[].
    if let Some(list) = state.pointer("/root/named_devices").and_then(Value::as_array) {
        pems.extend(list.iter().filter_map(|d| d.get("cert").and_then(Value::as_str)));
    }
    if let Some(list) = state.pointer("/root/devices").and_then(Value::as_array) {
        for d in list {
            if let Some(certs) = d.get("certs").and_then(Value::as_array) {
                pems.extend(certs.iter().filter_map(Value::as_str));
            }
        }
    }
    pems.into_iter()
        .filter_map(|pem| CertificateDer::from_pem_slice(pem.as_bytes()).ok())
        .map(|der| der.as_ref().to_vec())
        .collect()
}

/// Lets in only certificates on Sunshine's paired list (they are self-signed, so there is no
/// chain to check: the certificate itself is the identity, exactly as Sunshine treats it).
struct PairedOnly {
    certs: Vec<Vec<u8>>,
    provider: Arc<CryptoProvider>,
}

impl std::fmt::Debug for PairedOnly {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PairedOnly({} devices)", self.certs.len())
    }
}

impl ClientCertVerifier for PairedOnly {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        if self.certs.iter().any(|c| c.as_slice() == end_entity.as_ref()) {
            Ok(ClientCertVerified::assertion())
        } else {
            Err(rustls::Error::General("this device isn't paired with Sunshine".into()))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

/// Built for every connection, so a phone paired a minute ago is let in straight away.
fn server_config(s: &Sunshine) -> Result<Arc<ServerConfig>, String> {
    let certs = CertificateDer::pem_file_iter(&s.cert)
        .and_then(|i| i.collect::<Result<Vec<_>, _>>())
        .map_err(|e| format!("Sunshine's certificate ({}): {e}", s.cert.display()))?;
    let key = PrivateKeyDer::from_pem_file(&s.key).map_err(|e| format!("Sunshine's key ({}): {e}", s.key.display()))?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Arc::new(PairedOnly { certs: paired_certs(&s.state), provider: provider.clone() });
    let config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_client_cert_verifier(verifier)
        .with_single_cert(certs, key)
        .map_err(|e| e.to_string())?;
    Ok(Arc::new(config))
}

/// Listens forever. Waits for Sunshine to be installed if it isn't yet.
pub fn serve<B: Backend + 'static>(controller: Arc<Controller<B>>) {
    let active = Arc::new(AtomicUsize::new(0));
    loop {
        let s = match sunshine() {
            Ok(s) => s,
            Err(e) => {
                log::line(format!("remote: {e} Trying again in a minute."));
                std::thread::sleep(Duration::from_secs(60));
                continue;
            }
        };
        let listener = match TcpListener::bind((Ipv4Addr::UNSPECIFIED, s.port)) {
            Ok(l) => l,
            Err(e) => {
                log::line(format!("remote: can't listen on port {}: {e}. Trying again in a minute.", s.port));
                std::thread::sleep(Duration::from_secs(60));
                continue;
            }
        };
        log::line(format!("remote: listening on port {}", s.port));
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            if active.load(Ordering::SeqCst) >= MAX_CONNECTIONS {
                continue;
            }
            active.fetch_add(1, Ordering::SeqCst);
            let (controller, s, active) = (controller.clone(), s.clone(), active.clone());
            std::thread::spawn(move || {
                if let Err(e) = handle(stream, &controller, &s) {
                    log::line(format!("remote: {e}"));
                }
                active.fetch_sub(1, Ordering::SeqCst);
            });
        }
    }
}

fn handle<B: Backend>(stream: TcpStream, controller: &Controller<B>, s: &Sunshine) -> Result<(), String> {
    let peer = stream.peer_addr().map(|a| a.to_string()).unwrap_or_default();
    stream.set_read_timeout(Some(Duration::from_secs(15))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(15))).ok();
    let config = server_config(s)?;
    let conn = ServerConnection::new(config).map_err(|e| e.to_string())?;
    let mut tls = StreamOwned::new(conn, stream);
    let request = read_request(&mut tls).map_err(|e| format!("{peer}: {e}"))?;
    let (status, body) = route(&request, controller);
    log::line(format!("remote: {peer} {} {} -> {status}", request.method, request.path));
    write_response(&mut tls, status, &body).map_err(|e| e.to_string())?;
    tls.conn.send_close_notify();
    let _ = tls.flush();
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub body: Vec<u8>,
}

/// One HTTP/1.1 request, with hard limits on its size.
pub fn read_request(r: &mut impl Read) -> Result<Request, String> {
    let mut head = Vec::with_capacity(512);
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() >= MAX_HEAD {
            return Err("request headers too long".into());
        }
        match r.read(&mut byte) {
            Ok(1) => head.push(byte[0]),
            Ok(_) => return Err("connection closed mid-request".into()),
            Err(e) => return Err(e.to_string()),
        }
    }
    let text = String::from_utf8(head).map_err(|_| "request headers aren't text")?;
    let mut lines = text.split("\r\n");
    let mut first = lines.next().unwrap_or("").split(' ');
    let (method, target) = (first.next().unwrap_or(""), first.next().unwrap_or(""));
    if method.is_empty() || !target.starts_with('/') {
        return Err("malformed request line".into());
    }
    let path = target.split('?').next().unwrap_or(target).to_string();
    let mut length = 0usize;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().map_err(|_| "bad Content-Length")?;
            }
        }
    }
    if length > MAX_BODY {
        return Err("request body too long".into());
    }
    let mut body = vec![0u8; length];
    r.read_exact(&mut body).map_err(|e| e.to_string())?;
    Ok(Request { method: method.to_string(), path, body })
}

/// The mode in a prepare request's JSON body.
pub fn parse_mode(body: &[u8]) -> Option<Mode> {
    let v: Value = serde_json::from_slice(body).ok()?;
    let n = |k: &str| v.get(k).and_then(Value::as_u64).and_then(|x| u32::try_from(x).ok());
    Some(Mode { width: n("width")?, height: n("height")?, fps: n("fps")? })
}

fn route<B: Backend>(req: &Request, controller: &Controller<B>) -> (u16, Value) {
    let reply = match (req.method.as_str(), req.path.as_str()) {
        ("GET", PATH_STATUS) => controller.status(),
        ("POST", PATH_PREPARE) => match parse_mode(&req.body) {
            Some(mode) => controller.prepare(mode),
            None => return (400, json!({ "state": "failed", "message": "Send {\"width\", \"height\", \"fps\"}." })),
        },
        ("POST", PATH_KEEPALIVE) => controller.keepalive(),
        ("POST", PATH_RELEASE) => controller.release(),
        _ => return (404, json!({ "state": "failed", "message": "Not found." })),
    };
    (reply.http_status(), reply.to_json())
}

fn write_response(w: &mut impl Write, status: u16, body: &Value) -> std::io::Result<()> {
    let body = body.to_string();
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        503 => "Service Unavailable",
        _ => "Internal Server Error",
    };
    write!(
        w,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_request() {
        let body = r#"{"width":2400,"height":1080,"fps":120}"#;
        let raw = format!("POST /pocketdeck/v1/display/prepare?x=1 HTTP/1.1\r\nHost: pc\r\ncontent-length: {}\r\n\r\n{body}", body.len());
        let req = read_request(&mut raw.as_bytes()).unwrap();
        assert_eq!(req.method, "POST");
        assert_eq!(req.path, PATH_PREPARE);
        assert_eq!(parse_mode(&req.body), Some(Mode { width: 2400, height: 1080, fps: 120 }));
    }

    #[test]
    fn refuses_oversized_requests() {
        let mut big = b"GET / HTTP/1.1\r\n".to_vec();
        big.extend(std::iter::repeat_n(b'a', MAX_HEAD));
        assert!(read_request(&mut &big[..]).is_err());
        let raw = b"POST / HTTP/1.1\r\nContent-Length: 999999\r\n\r\n";
        assert!(read_request(&mut &raw[..]).is_err());
        assert!(read_request(&mut &b"nonsense\r\n\r\n"[..]).is_err());
    }

    #[test]
    fn mode_needs_all_three_numbers() {
        assert_eq!(parse_mode(br#"{"width":1920,"height":1080}"#), None);
        assert_eq!(parse_mode(br#"{"width":-1,"height":1080,"fps":60}"#), None);
        assert_eq!(parse_mode(b"not json"), None);
    }

    #[test]
    fn writes_a_response() {
        let mut out = Vec::new();
        write_response(&mut out, 200, &json!({ "state": "off" })).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(text.ends_with("\r\n\r\n{\"state\":\"off\"}"));
        assert!(text.contains("Content-Length: 15\r\n"));
    }

    #[test]
    fn finds_paired_certificates_in_both_formats() {
        // A real self-signed certificate (Moonlight-style), PEM.
        let pem = include_str!("../tests/client.pem");
        let current = json!({ "root": { "named_devices": [{ "name": "PocketDeck phone", "cert": pem, "uuid": "A" }, { "name": "broken", "cert": "junk" }] } });
        let older = json!({ "root": { "devices": [{ "certs": [pem] }] } });
        let a = paired_certs_in(&current);
        assert_eq!(a.len(), 1);
        assert_eq!(a, paired_certs_in(&older));
        assert_eq!(a[0][0], 0x30, "DER starts with a SEQUENCE");
        assert!(paired_certs_in(&json!({})).is_empty());
    }
}
