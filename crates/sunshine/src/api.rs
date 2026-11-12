//! Sunshine's local web API (`https://127.0.0.1:<port+1>/api/...`), as used by its own web UI.
//!
//! Requests carry HTTP Basic auth and no `Origin` header, which Sunshine treats as a non-browser
//! client and exempts from its CSRF check. Its certificate is self-signed, so verification is
//! off; the connection never leaves this PC.

use crate::Ports;
use serde_json::{Value, json};
use std::time::Duration;

#[derive(Clone, PartialEq, Eq)]
pub struct Login {
    pub username: String,
    pub password: String,
}

impl std::fmt::Debug for Login {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Login {{ username: {:?}, password: <hidden> }}", self.username)
    }
}

/// Whether PocketDeck can use the API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Logged in.
    Ok,
    /// Sunshine has no web login yet; PocketDeck can create one.
    NoLoginYet,
    /// Sunshine has a login and ours is missing or wrong.
    NeedsLogin,
    /// The web API isn't answering.
    Unreachable,
}

#[derive(Debug)]
pub enum ApiError {
    Unreachable(String),
    Unauthorized,
    Rejected(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Unreachable(e) => write!(f, "Sunshine's web API isn't answering ({e})."),
            ApiError::Unauthorized => write!(f, "Sunshine didn't accept the login."),
            ApiError::Rejected(e) => write!(f, "Sunshine refused the change: {e}"),
        }
    }
}

impl std::error::Error for ApiError {}

/// One entry of Sunshine's app list, with its position (the API addresses apps by index).
#[derive(Debug, Clone)]
pub struct App {
    pub index: usize,
    pub json: Value,
}

impl App {
    pub fn name(&self) -> &str {
        self.json.get("name").and_then(Value::as_str).unwrap_or("")
    }

    pub fn image_path(&self) -> &str {
        self.json.get("image-path").and_then(Value::as_str).unwrap_or("")
    }
}

pub struct Api {
    base: String,
    agent: ureq::Agent,
    auth: Option<String>,
}

impl Api {
    pub fn new(ports: &Ports, login: Option<&Login>) -> Api {
        let tls = ureq::tls::TlsConfig::builder().disable_verification(true).build();
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .tls_config(tls)
            .max_redirects(0)
            .max_redirects_will_error(false)
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(8)))
            .build()
            .into();
        Api {
            base: format!("https://127.0.0.1:{}", ports.web_ui),
            agent,
            auth: login.map(|l| format!("Basic {}", base64(&format!("{}:{}", l.username, l.password)))),
        }
    }

    fn get(&self, path: &str) -> Result<(u16, String, String), ApiError> {
        let mut req = self.agent.get(format!("{}{path}", self.base));
        if let Some(a) = &self.auth {
            req = req.header("Authorization", a);
        }
        let mut res = req.call().map_err(|e| ApiError::Unreachable(e.to_string()))?;
        let status = res.status().as_u16();
        let location = res.headers().get("location").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        let body = res.body_mut().read_to_string().unwrap_or_default();
        Ok((status, location, body))
    }

    fn send(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value, ApiError> {
        let url = format!("{}{path}", self.base);
        let mut res = match method {
            "DELETE" => {
                let mut req = self.agent.delete(&url);
                if let Some(a) = &self.auth {
                    req = req.header("Authorization", a);
                }
                match body {
                    Some(b) => req.force_send_body().send_json(b),
                    None => req.call(),
                }
            }
            _ => {
                let mut req = self.agent.post(&url);
                if let Some(a) = &self.auth {
                    req = req.header("Authorization", a);
                }
                req.send_json(body.cloned().unwrap_or(Value::Null))
            }
        }
        .map_err(|e| ApiError::Unreachable(e.to_string()))?;
        let status = res.status().as_u16();
        let text = res.body_mut().read_to_string().unwrap_or_default();
        let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        match status {
            200..=299 => Ok(value),
            401 => Err(ApiError::Unauthorized),
            _ => Err(ApiError::Rejected(
                value.get("error").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| format!("HTTP {status}")),
            )),
        }
    }

    pub fn access(&self) -> Access {
        match self.get("/api/apps") {
            Ok((200, _, _)) => Access::Ok,
            Ok((300..=399, location, _)) if location.contains("welcome") => Access::NoLoginYet,
            Ok((401, _, _)) | Ok((403, _, _)) => Access::NeedsLogin,
            Ok((_, location, _)) if location.contains("welcome") => Access::NoLoginYet,
            Ok(_) => Access::NeedsLogin,
            Err(_) => Access::Unreachable,
        }
    }

    /// Sets the first web login on a Sunshine that has none (only allowed in that state).
    pub fn create_login(&self, login: &Login) -> Result<(), ApiError> {
        self.send(
            "POST",
            "/api/password",
            Some(&json!({
                "newUsername": login.username,
                "newPassword": login.password,
                "confirmNewPassword": login.password,
            })),
        )
        .map(|_| ())
    }

    pub fn apps(&self) -> Result<Vec<App>, ApiError> {
        let (status, _, body) = self.get("/api/apps")?;
        match status {
            200 => {}
            401 | 403 => return Err(ApiError::Unauthorized),
            s => return Err(ApiError::Rejected(format!("HTTP {s}"))),
        }
        let value: Value = serde_json::from_str(&body).map_err(|e| ApiError::Rejected(e.to_string()))?;
        Ok(value
            .get("apps")
            .and_then(Value::as_array)
            .map(|apps| apps.iter().cloned().enumerate().map(|(index, json)| App { index, json }).collect())
            .unwrap_or_default())
    }

    /// Adds an app (`index: None`) or replaces the one at `index`. Sunshine re-sorts the list
    /// afterwards, so indexes from an earlier `apps()` call are stale after this.
    pub fn save_app(&self, index: Option<usize>, app: &Value) -> Result<(), ApiError> {
        let mut body = app.clone();
        body["index"] = json!(index.map(|i| i as i64).unwrap_or(-1));
        self.send("POST", "/api/apps", Some(&body)).map(|_| ())
    }

    /// Removes the app at `index`. Indexes shift afterwards.
    pub fn delete_app(&self, index: usize) -> Result<(), ApiError> {
        self.send("DELETE", &format!("/api/apps/{index}"), None).map(|_| ())
    }

    /// Devices waiting for a PIN right now (Sunshine 2026 and later; the PIN tab polls this).
    pub fn pending_pairings(&self) -> Result<Vec<PendingPairing>, ApiError> {
        let (status, _, body) = self.get("/api/pin")?;
        match status {
            200 => {}
            401 | 403 => return Err(ApiError::Unauthorized),
            s => return Err(ApiError::Rejected(format!("HTTP {s}"))),
        }
        let value: Value = serde_json::from_str(&body).map_err(|e| ApiError::Rejected(e.to_string()))?;
        Ok(parse_pending(&value))
    }

    /// Enters the PIN for one waiting device, as the PIN tab of Sunshine's web page does, and
    /// names it `device_name` in Sunshine's list. `Ok(false)`: Sunshine turned it down.
    pub fn submit_pin(&self, pairing_id: &str, pin: &str, device_name: &str) -> Result<bool, ApiError> {
        let body = json!({ "pairing_id": pairing_id, "pin": pin, "name": device_name });
        let value = self.send("POST", "/api/pin", Some(&body))?;
        Ok(truthy(value.get("status")))
    }

    /// Sunshine before 2026: the PIN goes to whichever device is waiting, and `Ok(false)`
    /// means none is.
    pub fn submit_pin_legacy(&self, pin: &str, device_name: &str) -> Result<bool, ApiError> {
        let value = self.send("POST", "/api/pin", Some(&json!({ "pin": pin, "name": device_name })))?;
        Ok(truthy(value.get("status")))
    }

    /// Turns down a waiting device's pairing request.
    pub fn cancel_pairing(&self, pairing_id: &str) -> Result<(), ApiError> {
        self.send("DELETE", "/api/pin", Some(&json!({ "pairing_id": pairing_id }))).map(|_| ())
    }

    /// The devices paired with Sunshine.
    pub fn clients(&self) -> Result<Vec<Client>, ApiError> {
        let (status, _, body) = self.get("/api/clients/list")?;
        match status {
            200 => {}
            401 | 403 => return Err(ApiError::Unauthorized),
            s => return Err(ApiError::Rejected(format!("HTTP {s}"))),
        }
        let value: Value = serde_json::from_str(&body).map_err(|e| ApiError::Rejected(e.to_string()))?;
        Ok(parse_clients(&value))
    }

    /// Unpairs one device. It has to pair again before it can stream.
    pub fn unpair_client(&self, uuid: &str) -> Result<(), ApiError> {
        self.send("POST", "/api/clients/unpair", Some(&json!({ "uuid": uuid }))).map(|_| ())
    }
}

/// A device paired with Sunshine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Client {
    pub uuid: String,
    pub name: String,
}

/// A device that has asked to pair and is waiting for its PIN.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingPairing {
    pub id: String,
    /// The device's IP address as Sunshine saw it.
    pub address: String,
    /// What the device called itself. Moonlight-based clients all send "roth".
    pub name: String,
}

fn parse_pending(value: &Value) -> Vec<PendingPairing> {
    let text = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    value
        .get("pairings")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter(|p| !text(p, "id").is_empty())
                .map(|p| PendingPairing { id: text(p, "id"), address: text(p, "address"), name: text(p, "name") })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_clients(value: &Value) -> Vec<Client> {
    value
        .get("named_certs")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|c| {
                    let uuid = c.get("uuid").and_then(Value::as_str)?.to_string();
                    let name = c.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                    Some(Client { uuid, name })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Sunshine writes booleans as JSON booleans in newer releases and as `"true"` in older ones.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s.eq_ignore_ascii_case("true"),
        _ => false,
    }
}

/// A random 4-digit pairing PIN from the OS generator.
pub fn random_pin() -> String {
    let mut buf = [0u8; 4];
    fill_random(&mut buf);
    format!("{:04}", u32::from_le_bytes(buf) % 10_000)
}

fn fill_random(buf: &mut [u8]) {
    use windows_sys::Win32::Security::Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom};
    // SAFETY: valid buffer and length; the system RNG needs no algorithm handle with this flag.
    unsafe {
        BCryptGenRandom(std::ptr::null_mut(), buf.as_mut_ptr(), buf.len() as u32, BCRYPT_USE_SYSTEM_PREFERRED_RNG);
    }
}

fn base64(input: &str) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// A random password from the OS generator, for logins PocketDeck creates itself.
pub fn random_password() -> String {
    let mut buf = [0u8; 24];
    fill_random(&mut buf);
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";
    buf.iter().map(|b| ALPHABET[*b as usize % ALPHABET.len()] as char).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_basic_auth() {
        assert_eq!(base64("user:pass"), "dXNlcjpwYXNz");
        assert_eq!(base64("a"), "YQ==");
        assert_eq!(base64("ab"), "YWI=");
        assert_eq!(base64(""), "");
    }

    #[test]
    fn pins_are_four_digits() {
        for _ in 0..50 {
            let pin = random_pin();
            assert_eq!(pin.len(), 4);
            assert!(pin.bytes().all(|b| b.is_ascii_digit()));
        }
    }

    #[test]
    fn reads_clients_and_status() {
        let v = json!({ "named_certs": [{ "name": "Pixel", "uuid": "A1" }, { "name": "x" }], "status": "true" });
        assert_eq!(parse_clients(&v), vec![Client { uuid: "A1".into(), name: "Pixel".into() }]);
        assert!(truthy(v.get("status")));
        assert!(truthy(Some(&json!(true))));
        assert!(!truthy(Some(&json!("false"))));
        assert!(!truthy(None));
    }

    #[test]
    fn reads_pending_pairings() {
        // As Sunshine 2026.914 returns it.
        let v = json!({ "pairings": [{ "address": "192.168.1.20", "id": "8bdd534f276f3337f8722f2567381162", "name": "roth" }, { "name": "no id" }] });
        assert_eq!(
            parse_pending(&v),
            vec![PendingPairing { id: "8bdd534f276f3337f8722f2567381162".into(), address: "192.168.1.20".into(), name: "roth".into() }]
        );
        assert!(parse_pending(&json!({ "pairings": [] })).is_empty());
    }

    #[test]
    fn random_passwords_differ() {
        let (a, b) = (random_password(), random_password());
        assert_eq!(a.len(), 24);
        assert_ne!(a, b);
    }
}
