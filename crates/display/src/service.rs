//! The Windows service that owns the virtual display.
//!
//! A phone *leases* the display: [`Controller::prepare`] switches it on at the phone's mode and
//! starts a lease of [`crate::LEASE_SECS`]; keep-alives renew it while the stream runs; a release
//! (or the lease running out because the phone vanished) switches it off again. Whatever happens,
//! the display is off whenever nobody holds a lease: at service start, at shutdown, and after
//! PocketDeck or the phone crashed mid-stream.

use crate::device::State;
use crate::{LEASE_SECS, Mode, SERVICE_NAME, log, vdd};
use serde_json::{Value, json};
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// What the service tells the phone and the local tools.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// On, arranged at the phone's mode. `note` says what didn't go to plan (the display is on
    /// but couldn't be made the main screen, say).
    Ready { mode: Mode, note: Option<String> },
    /// On under a lease, with this many seconds left.
    On { mode: Mode, seconds_left: u64 },
    Off,
    /// This PC can't do it (no driver, say). The phone streams the normal screen instead.
    Unavailable(String),
    Failed(String),
}

impl Reply {
    pub fn to_json(&self) -> Value {
        let version = env!("CARGO_PKG_VERSION");
        match self {
            Reply::Ready { mode, note } => json!({
                "state": "ready", "width": mode.width, "height": mode.height, "fps": mode.fps,
                "note": note, "leaseSeconds": LEASE_SECS, "version": version,
            }),
            Reply::On { mode, seconds_left } => json!({
                "state": "on", "width": mode.width, "height": mode.height, "fps": mode.fps,
                "leaseSeconds": seconds_left, "version": version,
            }),
            Reply::Off => json!({ "state": "off", "version": version }),
            Reply::Unavailable(m) => json!({ "state": "unavailable", "message": m, "version": version }),
            Reply::Failed(m) => json!({ "state": "failed", "message": m, "version": version }),
        }
    }

    pub fn http_status(&self) -> u16 {
        match self {
            Reply::Ready { .. } | Reply::On { .. } | Reply::Off => 200,
            Reply::Unavailable(_) => 503,
            Reply::Failed(_) => 500,
        }
    }
}

/// What the controller does to the PC. Real for the service; a stand-in for the tests.
pub trait Backend: Send + Sync {
    fn state(&self) -> State;
    /// Adds `mode` to the driver's list; `true` when the list changed.
    fn ensure_mode(&self, mode: Mode) -> Result<bool, String>;
    fn set_enabled(&self, on: bool) -> Result<(), String>;
    fn wait_for(&self, state: State, limit: Duration) -> State;
    /// Makes the display the main screen at `mode`, in the signed-in session.
    fn arrange(&self, mode: Mode) -> Result<(), String>;
    /// Puts the remembered layout back, in the signed-in session.
    fn restore(&self) -> Result<(), String>;
    /// Whether the person has the feature switched on (Settings in PocketDeck).
    fn allowed(&self) -> bool;
    fn set_allowed(&self, on: bool) -> Result<(), String>;
}

#[derive(Default)]
struct Lease {
    until: Option<Instant>,
    mode: Option<Mode>,
}

pub struct Controller<B: Backend> {
    backend: B,
    lease: Mutex<Lease>,
    lease_length: Duration,
}

impl<B: Backend> Controller<B> {
    pub fn new(backend: B) -> Self {
        Controller { backend, lease: Mutex::new(Lease::default()), lease_length: Duration::from_secs(LEASE_SECS) }
    }

    #[cfg(test)]
    fn with_lease(backend: B, lease_length: Duration) -> Self {
        Controller { backend, lease: Mutex::new(Lease::default()), lease_length }
    }

    fn lock(&self) -> MutexGuard<'_, Lease> {
        self.lease.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Switches the display on at `mode` (or re-arranges it if it's on already) and starts or
    /// renews the lease. Returns once the display is in place.
    pub fn prepare(&self, mode: Mode) -> Reply {
        if !mode.sane() {
            return Reply::Failed(format!("{mode} isn't a screen size PocketDeck can set up."));
        }
        let mut lease = self.lock();
        let state = self.backend.state();
        if state == State::Missing {
            return Reply::Unavailable("The virtual display isn't set up on this PC.".into());
        }
        if !self.backend.allowed() {
            return Reply::Unavailable("The virtual display is switched off in PocketDeck's settings on the PC.".into());
        }
        let added = match self.backend.ensure_mode(mode) {
            Ok(a) => a,
            Err(e) => return Reply::Failed(format!("Couldn't add {mode} to the virtual display: {e}")),
        };
        // The driver reads its modes when it starts, so a new one means a restart. So does a
        // display that's stuck with a problem.
        let restart = (state != State::Off && added) || matches!(state, State::Problem(_));
        if restart {
            if let Err(e) = self.backend.set_enabled(false) {
                return Reply::Failed(e);
            }
            self.backend.wait_for(State::Off, Duration::from_secs(5));
        }
        if (state == State::Off || restart)
            && let Err(e) = self.backend.set_enabled(true) {
                return Reply::Failed(e);
            }
        match self.backend.wait_for(State::On, Duration::from_secs(10)) {
            State::On => {}
            other => {
                let _ = self.backend.set_enabled(false);
                *lease = Lease::default();
                return Reply::Failed(match other {
                    State::Problem(code) => format!("The virtual display reported a problem (code {code}). Try setting it up again."),
                    _ => "The virtual display didn't start in time.".into(),
                });
            }
        }
        lease.until = Some(Instant::now() + self.lease_length);
        lease.mode = Some(mode);
        let note = self.backend.arrange(mode).err();
        if let Some(n) = &note {
            log::line(format!("arrange {mode}: {n}"));
        }
        log::line(format!("on at {mode}"));
        Reply::Ready { mode, note }
    }

    /// Renews the lease while the display is on.
    pub fn keepalive(&self) -> Reply {
        let mut lease = self.lock();
        match (lease.until, lease.mode) {
            (Some(_), Some(mode)) => {
                lease.until = Some(Instant::now() + self.lease_length);
                Reply::On { mode, seconds_left: self.lease_length.as_secs() }
            }
            _ => Reply::Off,
        }
    }

    /// Ends the lease and switches the display off.
    pub fn release(&self) -> Reply {
        let mut lease = self.lock();
        self.off(&mut lease, "released");
        Reply::Off
    }

    pub fn status(&self) -> Reply {
        let lease = self.lock();
        match (self.backend.state(), lease.until, lease.mode) {
            (State::Missing, ..) => Reply::Unavailable("The virtual display isn't set up on this PC.".into()),
            (_, Some(until), Some(mode)) => {
                Reply::On { mode, seconds_left: until.saturating_duration_since(Instant::now()).as_secs() }
            }
            _ => Reply::Off,
        }
    }

    /// Called every few seconds: switches the display off when its lease has run out.
    pub fn tick(&self) {
        let mut lease = self.lock();
        if lease.until.is_some_and(|u| Instant::now() >= u) {
            self.off(&mut lease, "lease ran out");
        }
    }

    /// At service start: whatever state the display was left in, it's off now.
    pub fn reset(&self) {
        let mut lease = self.lock();
        if !matches!(self.backend.state(), State::Off | State::Missing) {
            self.off(&mut lease, "service started");
        }
    }

    fn off(&self, lease: &mut Lease, why: &str) {
        let was_on = !matches!(self.backend.state(), State::Off | State::Missing);
        *lease = Lease::default();
        if !was_on {
            return;
        }
        if let Err(e) = self.backend.set_enabled(false) {
            log::line(format!("off ({why}): {e}"));
            return;
        }
        self.backend.wait_for(State::Off, Duration::from_secs(5));
        if let Err(e) = self.backend.restore() {
            log::line(format!("restore layout: {e}"));
        }
        log::line(format!("off ({why})"));
    }

    /// Switches the feature on or off. Off also hands back a display that's on right now.
    pub fn allow(&self, on: bool) -> Reply {
        if let Err(e) = self.backend.set_allowed(on) {
            return Reply::Failed(e);
        }
        log::line(if on { "feature switched on" } else { "feature switched off" });
        if on { self.status() } else { self.release() }
    }

    /// One line from the local pipe: `status`, `on <width> <height> <fps>`, `keepalive`, `off`,
    /// `allow`, `disallow`.
    pub fn command(&self, line: &str) -> Reply {
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.as_slice() {
            ["status"] => self.status(),
            ["keepalive"] => self.keepalive(),
            ["off"] => self.release(),
            ["allow"] => self.allow(true),
            ["disallow"] => self.allow(false),
            ["on", w, h, f] => match (w.parse(), h.parse(), f.parse()) {
                (Ok(width), Ok(height), Ok(fps)) => self.prepare(Mode { width, height, fps }),
                _ => Reply::Failed("Usage: on <width> <height> <fps>".into()),
            },
            _ => Reply::Failed("Commands: status, on <width> <height> <fps>, keepalive, off, allow, disallow".into()),
        }
    }
}

/// The real thing: the device, the driver's settings file, and the helper in the user session.
pub struct SystemBackend {
    exe: PathBuf,
}

impl SystemBackend {
    pub fn new() -> Self {
        SystemBackend { exe: std::env::current_exe().unwrap_or_else(|_| crate::service_exe()) }
    }

    fn helper(&self, args: &[String]) -> Result<(), String> {
        let mut all = vec![crate::FLAG.to_string()];
        all.extend_from_slice(args);
        match crate::session::run_in_console(&self.exe, &all, Duration::from_secs(30))? {
            0 => Ok(()),
            code => Err(format!("the display helper failed (code {code}); details in {}", crate::data_dir().join("display.log").display())),
        }
    }
}

impl Default for SystemBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl Backend for SystemBackend {
    fn state(&self) -> State {
        crate::device::state()
    }

    fn ensure_mode(&self, mode: Mode) -> Result<bool, String> {
        vdd::ensure_mode(mode).map_err(|e| e.to_string())
    }

    fn set_enabled(&self, on: bool) -> Result<(), String> {
        crate::device::set_enabled(on)
    }

    fn wait_for(&self, state: State, limit: Duration) -> State {
        crate::device::wait_for(state, limit)
    }

    fn arrange(&self, mode: Mode) -> Result<(), String> {
        let device = crate::device::find().ok_or("the virtual display disappeared")?;
        self.helper(&[
            "arrange".into(),
            device.instance_id.clone(),
            mode.width.to_string(),
            mode.height.to_string(),
            mode.fps.to_string(),
        ])?;
        // Games that remember another monitor get moved onto the virtual display. The helper
        // ends by itself when the display is switched off, and only one runs at a time.
        let args = [crate::FLAG.to_string(), "follow".into(), device.instance_id];
        if let Err(e) = crate::session::spawn_in_console(&self.exe, &args) {
            log::line(format!("follow: {e}"));
        }
        Ok(())
    }

    fn restore(&self) -> Result<(), String> {
        self.helper(&["restore".into()])
    }

    fn allowed(&self) -> bool {
        crate::allowed()
    }

    fn set_allowed(&self, on: bool) -> Result<(), String> {
        winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE)
            .create_subkey(crate::REG_KEY)
            .and_then(|(k, _)| k.set_value("Enabled", &u32::from(on)))
            .map_err(|e| format!("Couldn't save the setting: {e}"))
    }
}

// ---- The Windows service -----------------------------------------------------------------

windows_service::define_windows_service!(ffi_service_main, service_main);

/// Hands this process to the Service Control Manager. Only works when started as the service.
pub fn run_as_service() -> Result<(), String> {
    windows_service::service_dispatcher::start(SERVICE_NAME, ffi_service_main).map_err(|e| e.to_string())
}

fn service_main(_args: Vec<OsString>) {
    if let Err(e) = run_service() {
        log::line(format!("service: {e}"));
    }
}

fn run_service() -> windows_service::Result<()> {
    use windows_service::service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
    };
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};

    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
    let handler = move |event| match event {
        ServiceControl::Stop | ServiceControl::Shutdown | ServiceControl::Preshutdown => {
            let _ = stop_tx.send(());
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    };
    let handle = service_control_handler::register(SERVICE_NAME, handler)?;
    let status = |state, accept| ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: accept,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::from_secs(10),
        process_id: None,
    };
    handle.set_service_status(status(ServiceState::Running, ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN))?;
    log::line(format!("service {} started", env!("CARGO_PKG_VERSION")));

    let controller = Arc::new(Controller::new(SystemBackend::new()));
    controller.reset();
    let tunnel = crate::tunnel::Tunnel::new();
    {
        let t = tunnel.clone();
        std::thread::Builder::new().name("tunnel".into()).spawn(move || t.restart()).ok();
    }
    {
        let c = controller.clone();
        let t = tunnel.clone();
        std::thread::Builder::new()
            .name("pipe".into())
            .spawn(move || {
                crate::pipe::serve(move |line| {
                    let words: Vec<&str> = line.split_whitespace().collect();
                    match words.split_first() {
                        Some((&"remote", rest)) => t.command(rest).to_string(),
                        _ => c.command(line).to_json().to_string(),
                    }
                })
            })
            .ok();
    }
    {
        let c = controller.clone();
        std::thread::Builder::new().name("remote".into()).spawn(move || crate::remote::serve(c)).ok();
    }

    // Ticks every 5 s until told to stop (or the handler went away).
    while let Err(std::sync::mpsc::RecvTimeoutError::Timeout) = stop_rx.recv_timeout(Duration::from_secs(5)) {
        controller.tick();
        tunnel.tick();
    }
    handle.set_service_status(status(ServiceState::StopPending, ServiceControlAccept::empty()))?;
    controller.release();
    tunnel.stop();
    log::line("service stopped");
    handle.set_service_status(status(ServiceState::Stopped, ServiceControlAccept::empty()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pretend PC: records what was done to it.
    struct Fake {
        state: Mutex<State>,
        modes: Mutex<Vec<Mode>>,
        log: Mutex<Vec<String>>,
        arrange_fails: bool,
        never_starts: bool,
        allowed: Mutex<bool>,
    }

    impl Fake {
        fn new(state: State) -> Self {
            Fake {
                state: Mutex::new(state),
                modes: Mutex::new(vec![]),
                log: Mutex::new(vec![]),
                arrange_fails: false,
                never_starts: false,
                allowed: Mutex::new(true),
            }
        }
        fn did(&self) -> Vec<String> {
            self.log.lock().unwrap().clone()
        }
        fn note(&self, s: impl Into<String>) {
            self.log.lock().unwrap().push(s.into());
        }
    }

    impl Backend for Fake {
        fn state(&self) -> State {
            *self.state.lock().unwrap()
        }
        fn ensure_mode(&self, mode: Mode) -> Result<bool, String> {
            let mut m = self.modes.lock().unwrap();
            if m.contains(&mode) {
                return Ok(false);
            }
            m.push(mode);
            self.note(format!("add {mode}"));
            Ok(true)
        }
        fn set_enabled(&self, on: bool) -> Result<(), String> {
            *self.state.lock().unwrap() = if !on { State::Off } else if self.never_starts { State::Starting } else { State::On };
            self.note(if on { "enable" } else { "disable" });
            Ok(())
        }
        fn wait_for(&self, _: State, _: Duration) -> State {
            self.state()
        }
        fn arrange(&self, mode: Mode) -> Result<(), String> {
            self.note(format!("arrange {mode}"));
            if self.arrange_fails { Err("nobody signed in".into()) } else { Ok(()) }
        }
        fn restore(&self) -> Result<(), String> {
            self.note("restore");
            Ok(())
        }
        fn allowed(&self) -> bool {
            *self.allowed.lock().unwrap()
        }
        fn set_allowed(&self, on: bool) -> Result<(), String> {
            *self.allowed.lock().unwrap() = on;
            Ok(())
        }
    }

    #[test]
    fn switching_the_feature_off_refuses_phones_and_hands_the_display_back() {
        let c = Controller::new(Fake::new(State::Off));
        c.prepare(PHONE);
        assert_eq!(c.command("disallow"), Reply::Off);
        assert_eq!(c.backend.state(), State::Off);
        assert!(matches!(c.prepare(PHONE), Reply::Unavailable(_)));
        assert_eq!(c.backend.state(), State::Off);
        assert_eq!(c.command("allow"), Reply::Off);
        assert!(matches!(c.prepare(PHONE), Reply::Ready { .. }));
    }

    const PHONE: Mode = Mode { width: 2400, height: 1080, fps: 120 };

    #[test]
    fn prepare_switches_on_and_release_switches_off() {
        let c = Controller::new(Fake::new(State::Off));
        assert_eq!(c.prepare(PHONE), Reply::Ready { mode: PHONE, note: None });
        assert_eq!(c.backend.state(), State::On);
        assert!(matches!(c.status(), Reply::On { mode: PHONE, .. }));
        assert_eq!(c.release(), Reply::Off);
        assert_eq!(c.backend.state(), State::Off);
        assert_eq!(c.backend.did(), ["add 2400x1080 at 120 Hz", "enable", "arrange 2400x1080 at 120 Hz", "disable", "restore"]);
    }

    #[test]
    fn a_new_mode_on_a_running_display_restarts_it() {
        let c = Controller::new(Fake::new(State::Off));
        c.prepare(PHONE);
        let other = Mode { width: 1920, height: 1080, fps: 60 };
        c.prepare(other);
        assert_eq!(&c.backend.did()[3..], ["add 1920x1080 at 60 Hz", "disable", "enable", "arrange 1920x1080 at 60 Hz"]);
        // The same mode again: no restart, just arranged.
        c.prepare(other);
        assert_eq!(c.backend.did().last().unwrap(), "arrange 1920x1080 at 60 Hz");
        assert_eq!(c.backend.did().len(), 8);
    }

    #[test]
    fn the_lease_runs_out() {
        let c = Controller::with_lease(Fake::new(State::Off), Duration::from_millis(40));
        c.prepare(PHONE);
        c.tick();
        assert_eq!(c.backend.state(), State::On);
        std::thread::sleep(Duration::from_millis(60));
        c.tick();
        assert_eq!(c.backend.state(), State::Off);
        assert_eq!(c.keepalive(), Reply::Off);
    }

    #[test]
    fn keepalive_extends_the_lease() {
        let c = Controller::with_lease(Fake::new(State::Off), Duration::from_millis(80));
        c.prepare(PHONE);
        for _ in 0..4 {
            std::thread::sleep(Duration::from_millis(40));
            assert!(matches!(c.keepalive(), Reply::On { .. }));
            c.tick();
        }
        assert_eq!(c.backend.state(), State::On);
    }

    #[test]
    fn nothing_to_do_without_the_driver() {
        let c = Controller::new(Fake::new(State::Missing));
        assert!(matches!(c.prepare(PHONE), Reply::Unavailable(_)));
        assert!(c.backend.did().is_empty());
    }

    #[test]
    fn startup_switches_a_leftover_display_off() {
        let c = Controller::new(Fake::new(State::On));
        c.reset();
        assert_eq!(c.backend.state(), State::Off);
        let c = Controller::new(Fake::new(State::Off));
        c.reset();
        assert!(c.backend.did().is_empty());
    }

    #[test]
    fn arranging_can_fail_without_failing_the_stream() {
        let mut fake = Fake::new(State::Off);
        fake.arrange_fails = true;
        let c = Controller::new(fake);
        assert_eq!(c.prepare(PHONE), Reply::Ready { mode: PHONE, note: Some("nobody signed in".into()) });
    }

    #[test]
    fn a_display_that_never_starts_is_switched_back_off() {
        let mut fake = Fake::new(State::Off);
        fake.never_starts = true;
        let c = Controller::new(fake);
        assert!(matches!(c.prepare(PHONE), Reply::Failed(_)));
        assert_eq!(c.backend.state(), State::Off);
        assert_eq!(c.keepalive(), Reply::Off);
    }

    #[test]
    fn refuses_silly_modes() {
        let c = Controller::new(Fake::new(State::Off));
        assert!(matches!(c.prepare(Mode { width: 99999, height: 1, fps: 60 }), Reply::Failed(_)));
        assert!(c.backend.did().is_empty());
    }

    #[test]
    fn parses_pipe_commands() {
        let c = Controller::new(Fake::new(State::Off));
        assert!(matches!(c.command("on 2400 1080 120"), Reply::Ready { .. }));
        assert!(matches!(c.command("status"), Reply::On { .. }));
        assert_eq!(c.command("off"), Reply::Off);
        assert!(matches!(c.command("on wide tall"), Reply::Failed(_)));
    }

    #[test]
    fn replies_as_json() {
        let j = Reply::Ready { mode: PHONE, note: None }.to_json();
        assert_eq!(j["state"], "ready");
        assert_eq!(j["width"], 2400);
        assert_eq!(Reply::Unavailable("x".into()).http_status(), 503);
    }
}
