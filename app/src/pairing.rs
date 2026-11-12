//! Pairing phones by QR code, worker side, plus the paired-device list.
//! One thread: a pairing session polls Sunshine twice a second while the code is on screen.

use pocketdeck_core::pairing::{self, CODE_LIFETIME_SECS, PairCode};
use pocketdeck_core::paths;
use pocketdeck_sunshine::api::{Access, Api, ApiError, Client};
use pocketdeck_sunshine::{cert, conf, detect, login, probe};
use std::collections::HashSet;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant};

pub enum PairCommand {
    /// Show a fresh code (ends any session already running).
    Start,
    /// Take the code down.
    Stop,
    LoadDevices,
    Unpair(String),
}

pub enum PairUpdate {
    /// Getting a code ready.
    Preparing,
    Code { uri: String, pc_name: String, seconds_left: u32 },
    SecondsLeft(u32),
    /// The phone scanned the code; Sunshine is finishing the pairing.
    Pairing,
    Paired { name: String },
    Failed(String),
    Expired,
    Stopped,
    Devices(Result<Vec<Client>, String>),
}

#[derive(Clone)]
pub struct PairWorker {
    tx: Sender<PairCommand>,
}

impl PairWorker {
    pub fn spawn(on_update: fn(crate::worker::Update)) -> PairWorker {
        let (tx, rx) = channel();
        std::thread::Builder::new()
            .name("pairing".into())
            .spawn(move || {
                let post = move |u: PairUpdate| {
                    let _ = slint::invoke_from_event_loop(move || on_update(crate::worker::Update::Pairing(u)));
                };
                let mut next = rx.recv().ok();
                while let Some(command) = next.take() {
                    match command {
                        PairCommand::Start => next = session(&rx, &post),
                        PairCommand::Stop => post(PairUpdate::Stopped),
                        PairCommand::LoadDevices => post(PairUpdate::Devices(devices())),
                        PairCommand::Unpair(uuid) => {
                            if let Err(e) = connect().and_then(|(api, _)| api.unpair_client(&uuid).map_err(|e| e.to_string())) {
                                crate::log::line(format!("unpair: {e}"));
                            }
                            post(PairUpdate::Devices(devices()));
                        }
                    }
                    if next.is_none() {
                        next = rx.recv().ok();
                    }
                }
            })
            .expect("pairing thread starts");
        PairWorker { tx }
    }

    pub fn send(&self, command: PairCommand) {
        let _ = self.tx.send(command);
    }
}

fn connect() -> Result<(Api, pocketdeck_sunshine::Ports), String> {
    let install = detect::find().ok_or("The streaming engine isn't set up on this PC yet.")?;
    let ports = conf::read_ports(&install.config_file());
    let saved = login::load(&paths::config_dir());
    let api = Api::new(&ports, saved.as_ref());
    match api.access() {
        Access::Ok => Ok((api, ports)),
        Access::Unreachable => Err("The streaming engine isn't answering. Check it on Home.".into()),
        _ => Err("PocketDeck isn't signed in to the streaming engine. Sign in from Library first.".into()),
    }
}

fn devices() -> Result<Vec<Client>, String> {
    let (api, _) = connect()?;
    api.clients().map_err(|e| e.to_string())
}

fn ids<T>(items: &[T], id: impl Fn(&T) -> &str) -> HashSet<String> {
    items.iter().map(|i| id(i).to_string()).collect()
}

/// Shows one code until a phone pairs with it, it expires, or the person closes it. Returns a
/// command that arrived meanwhile and still needs handling.
fn session(rx: &Receiver<PairCommand>, post: &dyn Fn(PairUpdate)) -> Option<PairCommand> {
    post(PairUpdate::Preparing);
    let fail = |message: String| {
        crate::log::line(format!("pairing: {message}"));
        post(PairUpdate::Failed(message));
        None
    };

    let (api, ports) = match connect() {
        Ok(c) => c,
        Err(e) => return fail(e),
    };
    let fingerprint = match cert::fingerprint(ports.https) {
        Ok(f) => f,
        Err(e) => return fail(format!("Couldn't read the streaming engine's certificate: {e}")),
    };
    let addresses = pairing::lan_addresses();
    if addresses.is_empty() {
        return fail("This PC isn't connected to a network. Connect it to the same Wi-Fi or router as your phone.".into());
    }
    let clients_before = match api.clients() {
        Ok(c) => c,
        Err(e) => return fail(e.to_string()),
    };
    // Sunshine before 2026 has no list of waiting devices; the PIN then goes in on its own.
    let waiting_before = match api.pending_pairings() {
        Ok(p) => Some(ids(&p, |p| &p.id)),
        Err(ApiError::Rejected(_)) => None,
        Err(e) => return fail(e.to_string()),
    };
    let pc_name = probe::server_info(ports.http)
        .map(|i| i.hostname)
        .ok()
        .filter(|n| !n.is_empty())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .unwrap_or_else(|| "Gaming PC".into());
    let code = PairCode {
        name: pc_name.clone(),
        addresses,
        port: ports.http,
        pin: pocketdeck_sunshine::api::random_pin(),
        fingerprint,
    };
    let device_name = pairing::device_name(&clients_before);
    let clients_before = ids(&clients_before, |c| &c.uuid);
    post(PairUpdate::Code { uri: code.uri(), pc_name, seconds_left: CODE_LIFETIME_SECS as u32 });

    let shown = Instant::now();
    let lifetime = Duration::from_secs(CODE_LIFETIME_SECS);
    let mut last_left = CODE_LIFETIME_SECS as u32;
    // When the PIN went in.
    let mut entered: Option<Instant> = None;

    loop {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(PairCommand::Stop) => {
                post(PairUpdate::Stopped);
                return None;
            }
            Ok(other @ PairCommand::Start) => return Some(other),
            Ok(PairCommand::LoadDevices) => post(PairUpdate::Devices(devices())),
            Ok(other @ PairCommand::Unpair(_)) => return Some(other),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return None,
        }

        match entered {
            None => {
                if shown.elapsed() >= lifetime {
                    post(PairUpdate::Expired);
                    return None;
                }
                let left = (lifetime - shown.elapsed()).as_secs() as u32;
                if left != last_left {
                    last_left = left;
                    post(PairUpdate::SecondsLeft(left));
                }
                let accepted = match &waiting_before {
                    Some(before) => match api.pending_pairings() {
                        Ok(pending) => match pairing::our_request(&pending, before) {
                            Some(request) => {
                                crate::log::line(format!("pairing: request from {}", request.address));
                                api.submit_pin(&request.id, &code.pin, &device_name)
                            }
                            None => continue,
                        },
                        Err(e) => Err(e),
                    },
                    None => api.submit_pin_legacy(&code.pin, &device_name),
                };
                match accepted {
                    Ok(true) => {
                        entered = Some(Instant::now());
                        post(PairUpdate::Pairing);
                    }
                    // Legacy Sunshine says no while nobody is waiting yet.
                    Ok(false) if waiting_before.is_none() => {}
                    Ok(false) => return fail("The streaming engine turned the pairing down. Show a new code and scan again.".into()),
                    Err(e) => return fail(e.to_string()),
                }
            }
            Some(at) => {
                if let Ok(now) = api.clients() {
                    if let Some(c) = pairing::new_client(&now, &clients_before) {
                        post(PairUpdate::Paired { name: c.name.clone() });
                        return None;
                    }
                }
                if at.elapsed() > Duration::from_secs(20) {
                    return fail("Your phone didn't finish pairing. Show a new code and scan it again.".into());
                }
            }
        }
    }
}
