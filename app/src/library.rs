//! The game library, worker side: scanning, covers, the Sunshine login and the sync.
//! Runs on the worker thread and owns the library state; the UI hears back via [`Report`].

use pocketdeck_core::library::{LibraryState, ManualGame};
use pocketdeck_core::paths;
use pocketdeck_core::sync::{self, Wanted};
use pocketdeck_library::{Game, Source, covers, steamgriddb};
use pocketdeck_sunshine::api::{Access, Api, Login, random_password};
use pocketdeck_sunshine::{conf, detect, login};
use std::path::PathBuf;

/// A change the person made in the UI.
pub enum Change {
    ToggleGame(String),
    ToggleSource(Source),
    AddManual(PathBuf),
}

#[derive(Clone)]
pub struct GameRow {
    pub key: String,
    pub name: String,
    pub source: Source,
    pub thumb: Option<PathBuf>,
    pub on: bool,
    pub note: String,
}

#[derive(Clone)]
pub struct SourceRow {
    pub source: Source,
    pub found: bool,
    pub count: usize,
    pub on: bool,
}

#[derive(Clone)]
pub struct Report {
    pub games: Vec<GameRow>,
    pub sources: Vec<SourceRow>,
    pub access: Access,
    pub message: String,
    pub error: String,
    pub busy: bool,
    /// 0..1 while busy, or below 0 when there's no telling.
    pub progress: f32,
    pub on_phone: usize,
}

impl Default for Report {
    fn default() -> Self {
        Report {
            games: Vec::new(),
            sources: Vec::new(),
            access: Access::Unreachable,
            message: "Looking for your games…".into(),
            error: String::new(),
            busy: true,
            progress: -1.0,
            on_phone: 0,
        }
    }
}

pub struct Librarian {
    state: LibraryState,
    last: Report,
}

const USERNAME: &str = "pocketdeck";

impl Librarian {
    pub fn new() -> Self {
        Librarian { state: LibraryState::load(), last: Report::default() }
    }

    pub fn change(&mut self, change: Change) {
        match change {
            Change::ToggleGame(key) => {
                if !self.state.hidden.remove(&key) {
                    self.state.hidden.insert(key);
                }
            }
            Change::ToggleSource(source) => {
                if !self.state.sources_off.remove(&source) {
                    self.state.sources_off.insert(source);
                }
            }
            Change::AddManual(exe) => {
                let game = pocketdeck_library::manual_game(&exe, None);
                if !self.state.manual.iter().any(|m| m.exe == exe) {
                    self.state.manual.push(ManualGame { exe, name: game.name });
                }
                self.state.hidden.remove(&game.key);
            }
        }
        let _ = self.state.save();
    }

    /// Scans, prepares covers and syncs, posting progress along the way so the screen is never
    /// blank: the list appears as soon as the scan is done, covers fill in as they're made.
    pub fn sync(&mut self, post: &dyn Fn(Report)) {
        // Name the engine only for people who installed Sunshine themselves.
        let own = detect::find().is_some() && !pocketdeck_core::Settings::load().engine_by_pocketdeck;
        let engine = if own { "Sunshine" } else { "the streaming engine" };

        let progress = |me: &mut Self, message: String, value: f32| {
            me.last.busy = true;
            me.last.message = message;
            me.last.progress = value;
            post(me.last.clone());
        };
        self.last.error.clear();
        progress(self, "Looking for your games…".into(), -1.0);

        // 1. What's installed.
        let scans = pocketdeck_library::scan_all();
        let mut games: Vec<Game> = scans.iter().flat_map(|(_, s)| s.games.clone()).collect();
        games.extend(self.state.manual_games());
        games.sort_by_key(|g| g.name.to_lowercase());
        self.last.sources = scans
            .iter()
            .map(|(source, scan)| SourceRow {
                source: *source,
                found: scan.launcher_found,
                count: scan.games.len(),
                on: !self.state.sources_off.contains(source),
            })
            .collect();

        let art_dir = paths::local_dir();
        self.last.games = games
            .iter()
            .map(|g| GameRow {
                key: g.key.clone(),
                name: g.name.clone(),
                source: g.source,
                thumb: Some(covers::thumb_path(&art_dir, &g.key)).filter(|p| p.is_file()),
                on: self.state.wants(g),
                note: String::new(),
            })
            .collect();
        let found = match games.len() {
            1 => "Found 1 game".to_string(),
            n => format!("Found {n} games"),
        };
        progress(self, found, 0.1);

        // 2. Covers (written once, then reused as-is). Posted in batches so the grid fills in.
        // With SteamGridDB, every game can get a cover.
        let sgdb = steamgriddb::available();
        let with_art = games.iter().filter(|g| sgdb || g.art.is_some() || g.art_url.is_some()).count().max(1);
        let mut covers: Vec<Option<PathBuf>> = Vec::with_capacity(games.len());
        let mut done = 0;
        let mut last_post = std::time::Instant::now();
        for (i, g) in games.iter().enumerate() {
            let existed = covers::cover_path(&art_dir, &g.key).is_file();
            // Local art, or (Epic) art fetched once from the launcher's CDN, or failing both
            // (games found among installed programs, games added by hand, missing launcher art)
            // a cover from SteamGridDB.
            let art = g.art.clone().or_else(|| {
                if existed && covers::thumb_path(&art_dir, &g.key).is_file() {
                    // Already made; no need to touch the network.
                    Some(covers::cover_path(&art_dir, &g.key))
                } else {
                    g.art_url.as_ref().and_then(|url| covers::fetch(&art_dir, &g.key, url)).or_else(|| {
                        steamgriddb::cover_url(&art_dir, g).and_then(|url| covers::fetch(&art_dir, &g.key, &url))
                    })
                }
            });
            let cover = art.as_ref().and_then(|art| {
                let c = covers::prepare(&art_dir, &g.key, art);
                if !existed {
                    self.last.games[i].thumb = Some(covers::thumb_path(&art_dir, &g.key)).filter(|p| p.is_file());
                }
                c
            });
            if sgdb || g.art.is_some() || g.art_url.is_some() {
                done += 1;
            }
            covers.push(cover);
            if last_post.elapsed().as_millis() > 150 {
                last_post = std::time::Instant::now();
                let value = 0.1 + 0.7 * done as f32 / with_art as f32;
                progress(self, format!("Getting the artwork ready… {done} of {with_art}"), value);
            }
        }

        // 3. The engine's app list.
        progress(self, "Updating your phone's list…".into(), 0.85);
        let (access, api) = self.connect();
        let wanted: Vec<Wanted> = games
            .iter()
            .zip(&covers)
            .filter(|(g, _)| self.state.wants(g))
            .map(|(g, cover)| Wanted { key: g.key.clone(), app: sync::app_json(g, cover.as_deref()) })
            .collect();

        let mut error = String::new();
        let mut on_phone = 0;
        let already = if own { "Already in Sunshine" } else { "Already added" };
        let message = match (access, api) {
            (Access::Ok, Some(api)) => match sync::apply(&api, &wanted, &mut self.state) {
                Ok(outcome) => {
                    let _ = self.state.save();
                    for row in self.last.games.iter_mut().filter(|r| outcome.skipped.contains(&r.key)) {
                        row.note = already.into();
                    }
                    on_phone = wanted.len() - outcome.skipped.len();
                    let mut m = match on_phone {
                        0 => "No games on your phone yet. Click one to add it.".to_string(),
                        1 => "1 game on your phone".to_string(),
                        n => format!("{n} games on your phone"),
                    };
                    if !outcome.skipped.is_empty() && own {
                        m.push_str(&format!(" · {} already in Sunshine", outcome.skipped.len()));
                    }
                    m
                }
                Err(e) => {
                    error = e.to_string();
                    format!("Couldn't update {engine}'s game list.")
                }
            },
            (Access::NeedsLogin, _) => "Connect to Sunshine to put these games on your phone.".into(),
            (Access::NoLoginYet, _) => format!("Couldn't get {engine} ready for your games."),
            _ if detect::find().is_none() => "Set up the streaming engine on Home to put these games on your phone.".into(),
            _ => format!("{} isn't answering, so your phone's list wasn't updated. Check Home.", capitalise(engine)),
        };

        self.last.access = access;
        self.last.message = message;
        self.last.error = error;
        self.last.busy = false;
        self.last.progress = 1.0;
        self.last.on_phone = on_phone;
        post(self.last.clone());
    }

    /// The API, logged in when possible. Creates a login on a Sunshine that has none.
    fn connect(&mut self) -> (Access, Option<Api>) {
        let Some(install) = detect::find() else { return (Access::Unreachable, None) };
        let ports = conf::read_ports(&install.config_file());
        let saved = login::load(&paths::config_dir());
        let api = Api::new(&ports, saved.as_ref());
        match api.access() {
            Access::NoLoginYet => {
                let fresh = Login { username: USERNAME.into(), password: random_password() };
                if Api::new(&ports, None).create_login(&fresh).is_ok() && login::save(&paths::config_dir(), &fresh).is_ok() {
                    let api = Api::new(&ports, Some(&fresh));
                    let access = api.access();
                    (access, Some(api))
                } else {
                    (Access::NoLoginYet, None)
                }
            }
            Access::NeedsLogin => {
                if saved.is_some() {
                    // The login we kept no longer works (changed in Sunshine's web UI).
                    login::forget(&paths::config_dir());
                }
                (Access::NeedsLogin, None)
            }
            access => (access, Some(api)),
        }
    }

    /// Tries a login the person typed; keeps it when Sunshine accepts it.
    pub fn try_login(&mut self, login: Login, post: &dyn Fn(Report)) -> bool {
        let Some(install) = detect::find() else { return false };
        self.last.busy = true;
        self.last.progress = -1.0;
        self.last.error.clear();
        self.last.message = "Connecting to Sunshine…".into();
        post(self.last.clone());
        let ports = conf::read_ports(&install.config_file());
        if Api::new(&ports, Some(&login)).access() == Access::Ok {
            let _ = login::save(&paths::config_dir(), &login);
            true
        } else {
            self.last.error = "Sunshine didn't accept that username and password.".into();
            self.last.busy = false;
            post(self.last.clone());
            false
        }
    }

    /// Gives Sunshine a new login PocketDeck keeps (one administrator prompt).
    pub fn reset_login(&mut self, post: &dyn Fn(Report)) -> bool {
        let Some(install) = detect::find() else { return false };
        self.last.busy = true;
        self.last.error.clear();
        self.last.message = "Setting a new Sunshine login. Windows will ask for permission.".into();
        post(self.last.clone());
        let fresh = Login { username: USERNAME.into(), password: random_password() };
        let result = login::reset(&install, &fresh);
        match result {
            Ok(()) => {
                let _ = login::save(&paths::config_dir(), &fresh);
                // Sunshine restarts to load the new login; give its web API a moment.
                let ports = conf::read_ports(&install.config_file());
                let api = Api::new(&ports, Some(&fresh));
                for _ in 0..30 {
                    if api.access() == Access::Ok {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
                true
            }
            Err(e) => {
                self.last.busy = false;
                self.last.error = match e {
                    pocketdeck_sunshine::Error::Cancelled => "Cancelled. Resetting the login needs administrator permission.".into(),
                    other => other.to_string(),
                };
                post(self.last.clone());
                false
            }
        }
    }
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}
