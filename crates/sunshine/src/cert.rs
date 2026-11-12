//! Sunshine's server certificate, as a phone sees it.
//!
//! The fingerprint goes into the pairing QR code so the phone can check that the PC it paired
//! with is the one on the screen. It's read off a TLS handshake with Sunshine's own HTTPS port,
//! so it is exactly what the phone will be shown, wherever Sunshine keeps the file.

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, ClientConnection, DigitallySignedStruct, SignatureScheme};
use sha2::{Digest, Sha256};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// SHA-256 of the DER certificate Sunshine presents on `port` (its GameStream HTTPS port),
/// lowercase hex.
pub fn fingerprint(port: u16) -> Result<String, String> {
    let der = server_cert(port)?;
    Ok(hex(&Sha256::digest(&der)))
}

pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 15) as usize] as char);
    }
    out
}

fn server_cert(port: u16) -> Result<Vec<u8>, String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let capture = Arc::new(Capture { cert: Mutex::new(None), provider: provider.clone() });
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(capture.clone())
        .with_no_client_auth();
    let name = ServerName::try_from("localhost").map_err(|e| e.to_string())?;
    let mut conn = ClientConnection::new(Arc::new(config), name).map_err(|e| e.to_string())?;

    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut sock = TcpStream::connect_timeout(&addr, Duration::from_secs(3)).map_err(|e| e.to_string())?;
    sock.set_read_timeout(Some(Duration::from_secs(3))).ok();
    sock.set_write_timeout(Some(Duration::from_secs(3))).ok();
    // Sunshine asks for a client certificate and may drop the connection when none comes, but
    // only after it has sent its own, which is all that's needed here.
    while conn.is_handshaking() {
        if conn.complete_io(&mut sock).is_err() {
            break;
        }
    }
    let cert = capture.cert.lock().ok().and_then(|mut c| c.take());
    cert.ok_or_else(|| format!("Sunshine didn't present a certificate on port {port}."))
}

#[derive(Debug)]
struct Capture {
    cert: Mutex<Option<Vec<u8>>>,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for Capture {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if let Ok(mut c) = self.cert.lock() {
            *c = Some(end_entity.as_ref().to_vec());
        }
        Ok(ServerCertVerified::assertion())
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

#[cfg(test)]
mod tests {
    #[test]
    fn hex_is_lowercase_and_padded() {
        assert_eq!(super::hex(&[0x00, 0x0f, 0xab, 0xff]), "000fabff");
    }
}
