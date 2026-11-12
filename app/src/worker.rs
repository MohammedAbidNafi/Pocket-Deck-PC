//! Slow work (checking Sunshine, installing, restarting) runs on one background thread.
//! Results go back to the UI thread through `slint::invoke_from_event_loop`.

use pocketdeck_sunshine::{self as sunshine, Status, install};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

pub enum Command {
    Refresh,
    Install,
    Restart,
}

/// What the UI thread hears back.
pub enum Update {
    Status(Status),
    Task(TaskUpdate),
    Library(crate::library::Report),
    Pairing(crate::pairing::PairUpdate),
}

/// Work for the library thread.
pub enum LibraryCommand {
    Sync,
    Change(crate::library::Change),
    Login(pocketdeck_sunshine::api::Login),
    ResetLogin,
}

/// The game library has its own thread, so a slow sync never holds up status checks.
#[derive(Clone)]
pub struct LibraryWorker {
    tx: Sender<LibraryCommand>,
}

impl LibraryWorker {
    pub fn spawn(on_update: fn(Update)) -> LibraryWorker {
        let (tx, rx) = channel::<LibraryCommand>();
        std::thread::Builder::new()
            .name("library".into())
            .spawn(move || {
                let mut librarian = crate::library::Librarian::new();
                let report = |r: crate::library::Report| post(on_update, Update::Library(r));
                while let Ok(command) = rx.recv() {
                    // Apply every change queued meanwhile, then sync once.
                    let mut sync = false;
                    let mut next = Some(command);
                    while let Some(command) = next {
                        match command {
                            LibraryCommand::Sync => sync = true,
                            LibraryCommand::Change(c) => {
                                librarian.change(c);
                                sync = true;
                            }
                            LibraryCommand::Login(login) => sync |= librarian.try_login(login, &report),
                            LibraryCommand::ResetLogin => sync |= librarian.reset_login(&report),
                        }
                        next = rx.try_recv().ok();
                    }
                    if sync {
                        librarian.sync(&report);
                    }
                }
            })
            .expect("library thread starts");
        LibraryWorker { tx }
    }

    pub fn send(&self, command: LibraryCommand) {
        let _ = self.tx.send(command);
    }
}

#[derive(Clone, Default)]
pub struct TaskUpdate {
    pub active: bool,
    pub kind: &'static str,
    pub label: String,
    pub progress: f32,
    pub error: String,
}

#[derive(Clone)]
pub struct Worker {
    tx: Sender<Command>,
}

impl Worker {
    pub fn spawn(on_update: fn(Update)) -> Worker {
        let (tx, rx) = channel();
        std::thread::Builder::new()
            .name("sunshine".into())
            .spawn(move || run(rx, on_update))
            .expect("worker thread starts");
        Worker { tx }
    }

    pub fn send(&self, command: Command) {
        let _ = self.tx.send(command);
    }
}

fn post(on_update: fn(Update), update: Update) {
    let _ = slint::invoke_from_event_loop(move || on_update(update));
}

fn run(rx: Receiver<Command>, on_update: fn(Update)) {
    while let Ok(command) = rx.recv() {
        // Many refresh requests queued up while busy collapse into one.
        if matches!(command, Command::Refresh) {
            while let Ok(Command::Refresh) = rx.try_recv() {}
        }
        match command {
            Command::Refresh => post(on_update, Update::Status(Status::gather())),
            Command::Install => do_install(on_update),
            Command::Restart => do_restart(on_update),
        }
    }
}

fn task(on_update: fn(Update), kind: &'static str, label: impl Into<String>, progress: f32) {
    post(
        on_update,
        Update::Task(TaskUpdate { active: true, kind, label: label.into(), progress, error: String::new() }),
    );
}

fn finish(on_update: fn(Update), kind: &'static str, error: Option<String>) {
    post(
        on_update,
        Update::Task(TaskUpdate { active: false, kind, error: error.unwrap_or_default(), ..Default::default() }),
    );
    post(on_update, Update::Status(Status::gather()));
}

fn do_install(on_update: fn(Update)) {
    let dir = pocketdeck_core::paths::local_dir().join("downloads");
    let total_mb = install::PINNED.size as f32 / 1_000_000.0;
    task(on_update, "install", format!("Downloading Sunshine {}…", install::PINNED.version), 0.0);
    let mut last_percent = -1i32;
    let result = install::install(&dir, |p| match p {
        install::Progress::Downloading { done, total } => {
            let fraction = if total > 0 { done as f32 / total as f32 } else { 0.0 };
            let percent = (fraction * 100.0) as i32;
            if percent != last_percent {
                last_percent = percent;
                task(
                    on_update,
                    "install",
                    format!("Downloading… {:.0} of {:.0} MB", done as f32 / 1_000_000.0, total_mb),
                    fraction * 0.8,
                );
            }
        }
        install::Progress::Installing => {
            task(on_update, "install", "Installing. Windows will ask for permission.", -1.0)
        }
    });
    match result {
        Ok(()) => {
            task(on_update, "install", "Starting Sunshine…", -1.0);
            wait_until_answering(Duration::from_secs(20));
            finish(on_update, "install", None);
        }
        Err(sunshine::Error::Cancelled) => {
            finish(on_update, "install", Some("Install cancelled. Windows needs your permission to install Sunshine.".into()))
        }
        Err(e) => finish(on_update, "install", Some(e.to_string())),
    }
}

fn do_restart(on_update: fn(Update)) {
    task(on_update, "restart", "Starting Sunshine…", -1.0);
    match sunshine::service::restart(sunshine::SERVICE_NAME) {
        Ok(()) => {
            wait_until_answering(Duration::from_secs(15));
            finish(on_update, "restart", None);
        }
        Err(sunshine::Error::Cancelled) => {
            finish(on_update, "restart", Some("Cancelled. Starting Sunshine needs administrator permission.".into()))
        }
        Err(e) => finish(on_update, "restart", Some(e.to_string())),
    }
}

fn wait_until_answering(limit: Duration) {
    let start = Instant::now();
    while start.elapsed() < limit {
        if Status::gather().ready() {
            return;
        }
        std::thread::sleep(Duration::from_millis(700));
    }
}
