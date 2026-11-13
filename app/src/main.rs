// PocketDeck for PC: sets up and looks after Sunshine, and (soon) lets PocketDeck on your phone
// reach this PC from anywhere. Lives in the tray; the window is only built while it is open.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod instance;
mod library;
mod log;
mod pairing;
mod sound;
mod tray;
mod worker;

slint::include_modules!();

use pocketdeck_core::{Settings, TileShape, TileSize, autostart};
use pocketdeck_sunshine::{self as sunshine, ServiceState, Status};
use slint::ComponentHandle;
use pocketdeck_library::Source;
use slint::{ModelRc, VecModel};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use worker::{Command, LibraryCommand, Update};

struct Shell {
    settings: Settings,
    engine: EngineStatus,
    task: Task,
    /// Only exists while the window is open; dropped when it closes to the tray to free memory.
    window: Option<AppWindow>,
    tray: Option<tray::Tray>,
    worker: worker::Worker,
    library_worker: worker::LibraryWorker,
    library: Option<library::Report>,
    pair_worker: pairing::PairWorker,
    pairing: PairView,
    /// Cover thumbnails, decoded only while the window is open.
    thumbs: HashMap<String, slint::Image>,
    ticks: u32,
}

/// What the Devices page shows: the pairing code and the paired phones.
#[derive(Default)]
struct PairView {
    state: &'static str,
    /// The current code, drawn once per code.
    code: Option<slint::Image>,
    pc_name: String,
    seconds_left: u32,
    message: String,
    devices: Option<Result<Vec<sunshine::api::Client>, String>>,
}

thread_local! {
    static SHELL: RefCell<Option<Shell>> = const { RefCell::new(None) };
}

/// Runs `f` with the app state. Only ever called on the UI thread.
fn with<R>(f: impl FnOnce(&mut Shell) -> R) -> Option<R> {
    SHELL.with(|s| s.borrow_mut().as_mut().map(f))
}

fn main() {
    let start_in_tray = std::env::args().any(|a| a == autostart::TRAY_FLAG);
    let _instance = match instance::acquire(|| {
        let _ = slint::invoke_from_event_loop(show_window);
    }) {
        instance::Instance::Secondary => return,
        primary => primary,
    };

    select_renderer();

    let settings = Settings::load();
    // Keep the Run entry in step with the setting (it also follows the exe if it moved).
    // Nothing is registered until setup is finished.
    if settings.onboarding_done && settings.start_with_windows != autostart::is_enabled() {
        if let Err(e) = autostart::set_enabled(settings.start_with_windows) {
            log::line(format!("start with Windows: {e}"));
        }
    }

    sound::set_enabled(settings.ui_sounds);
    let worker = worker::Worker::spawn(on_update);
    let library_worker = worker::LibraryWorker::spawn(on_update);
    let pair_worker = pairing::PairWorker::spawn(on_update);
    let tray = match tray::Tray::new(on_tray) {
        Ok(t) => Some(t),
        Err(e) => {
            log::line(format!("tray icon: {e}"));
            None
        }
    };
    let open_window = !start_in_tray || !settings.onboarding_done || tray.is_none();

    SHELL.with(|s| {
        *s.borrow_mut() = Some(Shell {
            settings,
            engine: EngineStatus::default(),
            task: Task::default(),
            window: None,
            tray,
            worker: worker.clone(),
            library_worker: library_worker.clone(),
            library: None,
            pair_worker,
            pairing: PairView { state: "idle", ..Default::default() },
            thumbs: HashMap::new(),
            ticks: 0,
        })
    });
    worker.send(Command::Refresh);
    library_worker.send(LibraryCommand::Sync);
    if open_window {
        show_window();
        start_intro();
    }
    // Write the sound files in the background (the intro first; it's needed straight away).
    std::thread::Builder::new()
        .name("sounds".into())
        .spawn(|| {
            sound::prepare(sound::Sound::Intro);
            let _ = slint::invoke_from_event_loop(|| begin_intro(true));
            sound::prepare_all();
        })
        .ok();

    // Keep the status fresh: every 5 s while the window is open, every 60 s in the tray.
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, Duration::from_secs(5), || {
        with(|s| {
            s.ticks += 1;
            if s.window.is_some() || s.ticks % 12 == 0 {
                s.worker.send(Command::Refresh);
            }
            // New and removed games are picked up every 10 minutes.
            if s.ticks % 120 == 0 {
                s.library_worker.send(LibraryCommand::Sync);
            }
        });
    });

    if let Err(e) = slint::run_event_loop_until_quit() {
        log::line(format!("event loop: {e}"));
    }
    SHELL.with(|s| s.borrow_mut().take());
}

/// The software renderer by default: measured at ~31 MB with the window open and ~28 MB in the
/// tray, against ~100 MB and ~85 MB for femtovg, which keeps the OpenGL driver loaded for good.
/// `POCKETDECK_RENDERER=femtovg` switches to the GPU renderer.
fn select_renderer() {
    let renderer = std::env::var("POCKETDECK_RENDERER").unwrap_or_else(|_| "software".into());
    if let Err(e) = slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name(renderer.clone())
        .select()
    {
        log::line(format!("renderer {renderer}: {e}"));
    }
}

// ---- Startup intro ---------------------------------------------------------------------------

static INTRO_BEGUN: AtomicBool = AtomicBool::new(false);
static INTRO_SHOWN: AtomicBool = AtomicBool::new(false);

/// Puts the intro on screen, paused on the splash colour until its sound is ready.
fn start_intro() {
    INTRO_SHOWN.store(true, Ordering::Relaxed);
    with(|s| {
        if let Some(w) = &s.window {
            let app = w.global::<App>();
            // Development only: POCKETDECK_INTRO_SPEED=0.2 plays the intro in slow motion.
            if let Some(speed) = std::env::var("POCKETDECK_INTRO_SPEED").ok().and_then(|v| v.parse::<f32>().ok()) {
                app.set_intro_speed(speed.clamp(0.05, 4.0));
            }
            app.set_show_intro(true);
        }
    });
    // Like the phone: wait briefly for the chime so sound and motion line up, then go anyway.
    slint::Timer::single_shot(Duration::from_millis(450), || begin_intro(false));
}

/// Starts the intro animation, with its sound when `with_sound` (the sound file is ready).
fn begin_intro(with_sound: bool) {
    if !INTRO_SHOWN.load(Ordering::Relaxed) || INTRO_BEGUN.swap(true, Ordering::Relaxed) {
        return;
    }
    if with_sound {
        sound::play(sound::Sound::Intro);
    }
    let speed = with(|s| {
        s.window.as_ref().map(|w| {
            let app = w.global::<App>();
            app.set_intro_running(true);
            app.get_intro_speed()
        })
    })
    .flatten()
    .unwrap_or(1.0)
    .max(0.05);
    // Belt and braces: take the intro down even if its own end-of-fade callback never comes.
    let total = Duration::from_secs_f32(1.9 / speed + 0.5);
    slint::Timer::single_shot(total, end_intro);
}

fn end_intro() {
    with(|s| {
        if let Some(w) = &s.window {
            w.global::<App>().set_show_intro(false);
        }
    });
}

// ---- Window ---------------------------------------------------------------------------------

fn show_window() {
    let window = with(|shell| {
        if shell.window.is_none() {
            match AppWindow::new() {
                Ok(w) => {
                    bind(&w);
                    shell.window = Some(w);
                }
                Err(e) => {
                    log::line(format!("window: {e}"));
                    return None;
                }
            }
        }
        let w = shell.window.as_ref()?.clone_strong();
        push_everything(shell, &w);
        push_library(shell);
        push_pairing(shell);
        shell.worker.send(Command::Refresh);
        Some(w)
    })
    .flatten();

    if let Some(w) = window {
        if let Err(e) = w.show() {
            log::line(format!("show window: {e}"));
        }
        bring_to_front(&w);
    }
}

/// Drops the window and everything it holds; the app keeps running in the tray.
fn release_window() {
    let window = with(|s| {
        s.thumbs.clear();
        // Nobody can scan a code that isn't on screen.
        if s.pairing.state != "idle" {
            s.pair_worker.send(pairing::PairCommand::Stop);
        }
        s.pairing.code = None;
        s.window.take()
    })
    .flatten();
    drop(window);
}

fn hwnd(w: &AppWindow) -> Option<windows_sys::Win32::Foundation::HWND> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = w.window().window_handle();
    let handle = handle.window_handle().ok()?;
    match handle.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get() as windows_sys::Win32::Foundation::HWND),
        _ => None,
    }
}

fn bring_to_front(w: &AppWindow) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{IsIconic, SW_RESTORE, SetForegroundWindow, ShowWindow};
    let Some(hwnd) = hwnd(w) else { return };
    // SAFETY: hwnd is the live window Slint just showed.
    unsafe {
        if IsIconic(hwnd) != 0 {
            ShowWindow(hwnd, SW_RESTORE);
        }
        SetForegroundWindow(hwnd);
    }
}

/// The standard Windows "Open" dialog, for picking a game's `.exe`.
fn pick_exe(owner: Option<windows_sys::Win32::Foundation::HWND>) -> Option<std::path::PathBuf> {
    use windows_sys::Win32::UI::Controls::Dialogs::{
        GetOpenFileNameW, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST, OPENFILENAMEW,
    };
    let filter: Vec<u16> = "Games and programs (*.exe)\0*.exe\0\0".encode_utf16().collect();
    let title: Vec<u16> = "Add a game to PocketDeck\0".encode_utf16().collect();
    let mut file = vec![0u16; 2048];
    // SAFETY: every pointer refers to a live buffer for the duration of the modal call.
    unsafe {
        let mut ofn: OPENFILENAMEW = std::mem::zeroed();
        ofn.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
        ofn.hwndOwner = owner.unwrap_or(std::ptr::null_mut());
        ofn.lpstrFilter = filter.as_ptr();
        ofn.lpstrFile = file.as_mut_ptr();
        ofn.nMaxFile = file.len() as u32;
        ofn.lpstrTitle = title.as_ptr();
        ofn.Flags = OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR;
        if GetOpenFileNameW(&mut ofn) == 0 {
            return None;
        }
    }
    let len = file.iter().position(|c| *c == 0).unwrap_or(file.len());
    Some(std::path::PathBuf::from(String::from_utf16_lossy(&file[..len])))
}

fn push_everything(shell: &Shell, w: &AppWindow) {
    let s = &shell.settings;
    let theme = w.global::<Theme>();
    theme.set_accent_index(s.accent_index as i32);
    theme.set_tile_shape(s.tile_shape.index());
    theme.set_tile_size(s.tile_size.index());

    let app = w.global::<App>();
    app.set_version(env!("CARGO_PKG_VERSION").into());
    app.set_onboarding(!s.onboarding_done);
    app.set_onboarding_step(s.onboarding_step as i32);
    app.set_start_with_windows(s.start_with_windows);
    app.set_close_to_tray(s.close_to_tray);
    app.set_ui_sounds(s.ui_sounds);

    let engine = w.global::<Engine>();
    engine.set_pinned_version(sunshine::install::PINNED.version.into());
    engine.set_own_sunshine(shell.engine.installed && !shell.settings.engine_by_pocketdeck);
    engine.set_status(shell.engine.clone());
    engine.set_task(shell.task.clone());
}

fn bind(w: &AppWindow) {
    let weak = w.as_weak();
    w.window().on_close_requested(move || {
        let to_tray = with(|s| s.settings.close_to_tray && s.tray.is_some()).unwrap_or(false);
        if to_tray {
            slint::Timer::single_shot(Duration::ZERO, release_window);
        } else {
            let _ = slint::quit_event_loop();
        }
        slint::CloseRequestResponse::HideWindow
    });

    let app = w.global::<App>();

    app.on_look_changed({
        let weak = weak.clone();
        move || {
            let Some(w) = weak.upgrade() else { return };
            let theme = w.global::<Theme>();
            with(|s| {
                s.settings.accent_index = theme.get_accent_index().clamp(0, 5) as u8;
                s.settings.tile_shape = TileShape::from_index(theme.get_tile_shape());
                s.settings.tile_size = TileSize::from_index(theme.get_tile_size());
                save(&s.settings);
            });
        }
    });

    app.on_general_changed({
        let weak = weak.clone();
        move || {
            let Some(w) = weak.upgrade() else { return };
            let app = w.global::<App>();
            with(|s| {
                s.settings.close_to_tray = app.get_close_to_tray();
                s.settings.ui_sounds = app.get_ui_sounds();
                sound::set_enabled(s.settings.ui_sounds);
                let start = app.get_start_with_windows();
                if start != s.settings.start_with_windows {
                    s.settings.start_with_windows = start;
                    if let Err(e) = autostart::set_enabled(start) {
                        log::line(format!("start with Windows: {e}"));
                    }
                }
                save(&s.settings);
            });
        }
    });

    app.on_step_changed({
        let weak = weak.clone();
        move || {
            let Some(w) = weak.upgrade() else { return };
            let step = w.global::<App>().get_onboarding_step().clamp(0, 10) as u8;
            with(|s| {
                s.settings.onboarding_step = step;
                save(&s.settings);
            });
        }
    });

    app.on_finish_onboarding({
        let weak = weak.clone();
        move || {
            let Some(w) = weak.upgrade() else { return };
            let app = w.global::<App>();
            app.set_tab(0);
            app.set_onboarding(false);
            app.set_finishing(false);
            app.set_onboarding_step(0);
            with(|s| {
                s.settings.onboarding_done = true;
                s.settings.onboarding_step = 0;
                if let Err(e) = autostart::set_enabled(s.settings.start_with_windows) {
                    log::line(format!("start with Windows: {e}"));
                }
                save(&s.settings);
                s.worker.send(Command::Refresh);
            });
        }
    });

    app.on_run_setup_again({
        let weak = weak.clone();
        move || {
            let Some(w) = weak.upgrade() else { return };
            let app = w.global::<App>();
            app.set_onboarding_step(0);
            app.set_finishing(false);
            app.set_onboarding(true);
        }
    });

    app.on_open_url(|url| open_url(&url));

    w.global::<Sfx>().on_play(|name| {
        if let Some(s) = sound::Sound::from_name(&name) {
            sound::play(s);
        }
    });

    let lib = w.global::<Library>();
    lib.on_refresh(|| {
        with(|s| s.library_worker.send(LibraryCommand::Sync));
    });
    lib.on_filter_changed(|| {
        with(push_library);
    });
    lib.on_toggle_game(|key| {
        with(|s| {
            if let Some(row) = s.library.as_mut().and_then(|r| r.games.iter_mut().find(|g| g.key == key.as_str())) {
                row.on = !row.on;
            }
            push_library(s);
            s.library_worker.send(LibraryCommand::Change(library::Change::ToggleGame(key.to_string())));
        });
    });
    lib.on_toggle_source(|id| {
        let Some(source) = source_from_id(&id) else { return };
        with(|s| {
            if let Some(row) = s.library.as_mut().and_then(|r| r.sources.iter_mut().find(|x| x.source == source)) {
                row.on = !row.on;
            }
            push_library(s);
            s.library_worker.send(LibraryCommand::Change(library::Change::ToggleSource(source)));
        });
    });
    lib.on_add_game({
        let weak = weak.clone();
        move || {
            let owner = weak.upgrade().and_then(|w| hwnd(&w));
            if let Some(exe) = pick_exe(owner) {
                with(|s| s.library_worker.send(LibraryCommand::Change(library::Change::AddManual(exe))));
            }
        }
    });
    lib.on_login(|user, pass| {
        let login = pocketdeck_sunshine::api::Login { username: user.trim().to_string(), password: pass.to_string() };
        with(|s| s.library_worker.send(LibraryCommand::Login(login)));
    });
    lib.on_reset_login(|| {
        with(|s| s.library_worker.send(LibraryCommand::ResetLogin));
    });

    let pair = w.global::<Pairing>();
    pair.on_start(|| {
        with(|s| {
            s.pairing.state = "preparing";
            s.pairing.code = None;
            push_pairing(s);
            s.pair_worker.send(pairing::PairCommand::Start);
        });
    });
    pair.on_stop(|| {
        with(|s| {
            s.pairing.state = "idle";
            s.pairing.code = None;
            push_pairing(s);
            s.pair_worker.send(pairing::PairCommand::Stop);
        });
    });
    pair.on_load_devices(|| {
        with(|s| s.pair_worker.send(pairing::PairCommand::LoadDevices));
    });
    pair.on_unpair(|uuid| {
        with(|s| s.pair_worker.send(pairing::PairCommand::Unpair(uuid.to_string())));
    });

    let engine = w.global::<Engine>();
    engine.on_refresh(|| {
        with(|s| s.worker.send(Command::Refresh));
    });
    engine.on_install(|| start_task("install", Command::Install));
    engine.on_restart(|| start_task("restart", Command::Restart));
    engine.on_open_driver_page(|| open_url(sunshine::CONTROLLER_DRIVER_URL));
}

/// Marks a task as running straight away (so buttons disable at once), then hands it to the worker.
fn start_task(kind: &'static str, command: Command) {
    with(|s| {
        if s.task.active {
            return;
        }
        s.task = Task { active: true, kind: kind.into(), progress: -1.0, ..Default::default() };
        push_engine(s);
        s.worker.send(command);
    });
}

fn save(settings: &Settings) {
    if let Err(e) = settings.save() {
        log::line(format!("save settings: {e}"));
    }
}

fn open_url(url: &str) {
    if !url.starts_with("https://") {
        return;
    }
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let wide = |s: &str| -> Vec<u16> { std::ffi::OsStr::new(s).encode_wide().chain(Some(0)).collect() };
    let (verb, target) = (wide("open"), wide(url));
    // SAFETY: both strings are NUL-terminated and outlive the call.
    unsafe {
        ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), target.as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL);
    }
}

// ---- Updates from the worker and the tray -----------------------------------------------------

fn on_update(update: Update) {
    with(|s| {
        match update {
            Update::Status(status) => {
                s.engine = engine_status(&status);
                if let Some(tray) = &s.tray {
                    tray.set_status(tray_text(&status, !s.settings.engine_by_pocketdeck));
                }
            }
            Update::Pairing(u) => {
                on_pairing(s, u);
                return;
            }
            Update::Library(report) => {
                s.library = Some(report);
                push_library(s);
                return;
            }
            Update::Task(t) => {
                // A freshly installed Sunshine: put the games on it straight away.
                if !t.active && t.kind == "install" && t.error.is_empty() {
                    s.settings.engine_by_pocketdeck = true;
                    save(&s.settings);
                    s.library_worker.send(LibraryCommand::Sync);
                }
                s.task = Task {
                    active: t.active,
                    kind: t.kind.into(),
                    label: t.label.into(),
                    progress: t.progress,
                    error: t.error.into(),
                };
            }
        }
        push_engine(s);
    });
}

fn push_engine(s: &Shell) {
    if let Some(w) = &s.window {
        let engine = w.global::<Engine>();
        engine.set_status(s.engine.clone());
        engine.set_task(s.task.clone());
        engine.set_own_sunshine(s.engine.installed && !s.settings.engine_by_pocketdeck);
    }
}

fn on_pairing(s: &mut Shell, update: pairing::PairUpdate) {
    use pairing::PairUpdate as P;
    let view = &mut s.pairing;
    // A late update from a session the person already closed changes nothing.
    let closed = view.state == "idle";
    match update {
        P::Devices(d) => view.devices = Some(d),
        P::Stopped => {}
        _ if closed => return,
        P::Preparing => view.state = "preparing",
        P::Code { uri, pc_name, seconds_left } => match qr_image(&uri) {
            Some(img) => {
                view.state = "showing";
                view.code = Some(img);
                view.pc_name = pc_name;
                view.seconds_left = seconds_left;
            }
            None => {
                view.state = "failed";
                view.message = "Couldn't draw the code. Try again.".into();
                s.pair_worker.send(pairing::PairCommand::Stop);
            }
        },
        P::SecondsLeft(n) => view.seconds_left = n,
        P::Pairing => view.state = "pairing",
        P::Paired { name } => {
            view.state = "paired";
            view.message = name;
            sound::play(sound::Sound::Select);
            s.pair_worker.send(pairing::PairCommand::LoadDevices);
            // The new phone wants the game list straight away.
            s.library_worker.send(LibraryCommand::Sync);
        }
        P::Failed(message) => {
            view.state = "failed";
            view.message = message;
            sound::play(sound::Sound::Back);
        }
        P::Expired => view.state = "expired",
    }
    if !matches!(view.state, "showing" | "pairing") {
        view.code = None;
    }
    push_pairing(s);
}

/// The pairing code as an image: dark modules on white with the standard 4-module quiet
/// zone, one pixel per module. The UI scales it up with pixelated sampling.
fn qr_image(uri: &str) -> Option<slint::Image> {
    use qrcode::{Color, EcLevel, QrCode};
    let code = QrCode::with_error_correction_level(uri.as_bytes(), EcLevel::M).ok()?;
    let modules = code.width();
    let colors = code.to_colors();
    let quiet = 4;
    let size = (modules + quiet * 2) as u32;
    let mut buffer = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(size, size);
    let width = size as usize;
    for (i, px) in buffer.make_mut_slice().iter_mut().enumerate() {
        let (x, y) = (i % width, i / width);
        let dark = x >= quiet
            && y >= quiet
            && x < quiet + modules
            && y < quiet + modules
            && colors[(y - quiet) * modules + (x - quiet)] == Color::Dark;
        let v = if dark { 0 } else { 255 };
        *px = slint::Rgb8Pixel { r: v, g: v, b: v };
    }
    Some(slint::Image::from_rgb8(buffer))
}

fn push_pairing(s: &Shell) {
    let Some(w) = &s.window else { return };
    let p = w.global::<Pairing>();
    let v = &s.pairing;
    p.set_state(v.state.into());
    p.set_code(v.code.clone().unwrap_or_default());
    p.set_pc_name(v.pc_name.clone().into());
    p.set_seconds_left(v.seconds_left as i32);
    p.set_message(v.message.clone().into());
    match &v.devices {
        None => p.set_devices_loaded(false),
        Some(Ok(list)) => {
            let items: Vec<PairedDevice> =
                list.iter().map(|c| PairedDevice { uuid: c.uuid.clone().into(), name: c.name.clone().into() }).collect();
            p.set_devices(ModelRc::new(VecModel::from(items)));
            p.set_devices_error("".into());
            p.set_devices_loaded(true);
        }
        Some(Err(e)) => {
            p.set_devices(ModelRc::default());
            p.set_devices_error(e.clone().into());
            p.set_devices_loaded(true);
        }
    }
}

fn source_id(source: Source) -> &'static str {
    match source {
        Source::Steam => "steam",
        Source::Epic => "epic",
        Source::Gog => "gog",
        Source::Xbox => "xbox",
        Source::Other => "other",
        Source::Manual => "manual",
    }
}

fn source_from_id(id: &str) -> Option<Source> {
    Source::ALL.into_iter().find(|s| source_id(*s) == id)
}

/// Sends the latest library report to the window, under the current filter.
fn push_library(s: &mut Shell) {
    let Shell { window, library, thumbs, .. } = s;
    let (Some(w), Some(report)) = (window.as_ref(), library.as_ref()) else { return };
    let lib = w.global::<Library>();

    let mut filter = lib.get_filter().to_string();
    let count = |src: Source| report.games.iter().filter(|g| g.source == src).count();
    let mut filters =
        vec![SourceItem { id: "all".into(), label: "All".into(), found: true, count: report.games.len() as i32, on: true }];
    for src in Source::ALL {
        let n = count(src);
        if n > 0 {
            filters.push(SourceItem { id: source_id(src).into(), label: src.label().into(), found: true, count: n as i32, on: true });
        }
    }
    if !filters.iter().any(|f| f.id == filter.as_str()) {
        filter = "all".into();
        lib.set_filter(filter.clone().into());
    }

    // Decode every cover once (the grid and the Home shelf share them).
    for g in &report.games {
        if let Some(path) = &g.thumb {
            if !thumbs.contains_key(&g.key) {
                if let Ok(img) = slint::Image::load_from_path(path) {
                    thumbs.insert(g.key.clone(), img);
                }
            }
        }
    }

    let games: Vec<GameItem> = report
        .games
        .iter()
        .filter(|g| filter == "all" || source_id(g.source) == filter)
        .map(|g| {
            let thumb = g.thumb.as_ref().map(|path| {
                thumbs
                    .entry(g.key.clone())
                    .or_insert_with(|| slint::Image::load_from_path(path).unwrap_or_default())
                    .clone()
            });
            GameItem {
                key: g.key.clone().into(),
                name: g.name.clone().into(),
                source: source_id(g.source).into(),
                has_thumb: thumb.is_some(),
                thumb: thumb.unwrap_or_default(),
                on: g.on,
                note: g.note.clone().into(),
            }
        })
        .collect();
    let sources: Vec<SourceItem> = report
        .sources
        .iter()
        .map(|x| SourceItem {
            id: source_id(x.source).into(),
            label: x.source.label().into(),
            found: x.found,
            count: x.count as i32,
            on: x.on,
        })
        .collect();

    let shelf: Vec<GameItem> = report
        .games
        .iter()
        .filter(|g| g.on)
        .map(|g| GameItem {
            key: g.key.clone().into(),
            name: g.name.clone().into(),
            source: source_id(g.source).into(),
            has_thumb: g.thumb.is_some() && thumbs.contains_key(&g.key),
            thumb: thumbs.get(&g.key).cloned().unwrap_or_default(),
            on: true,
            note: g.note.clone().into(),
        })
        .collect();
    lib.set_games(ModelRc::new(VecModel::from(games)));
    lib.set_shelf(ModelRc::new(VecModel::from(shelf)));
    lib.set_filters(ModelRc::new(VecModel::from(filters)));
    lib.set_sources(ModelRc::new(VecModel::from(sources)));
    lib.set_busy(report.busy);
    lib.set_progress(report.progress);
    lib.set_access(
        match report.access {
            pocketdeck_sunshine::api::Access::Ok => "ok",
            pocketdeck_sunshine::api::Access::NeedsLogin => "needs-login",
            _ => "unreachable",
        }
        .into(),
    );
    lib.set_message(report.message.clone().into());
    lib.set_error(report.error.clone().into());
    lib.set_on_phone(report.on_phone as i32);
}

fn on_tray(action: tray::Action) {
    match action {
        tray::Action::Open => show_window(),
        tray::Action::Quit => {
            let _ = slint::quit_event_loop();
        }
    }
}

fn engine_status(s: &Status) -> EngineStatus {
    let install = s.install.as_ref();
    let server = s.server.as_ref();
    EngineStatus {
        checked: true,
        installed: install.is_some(),
        version: install.and_then(|i| i.version.clone()).unwrap_or_default().into(),
        location: install.map(|i| i.dir.display().to_string()).unwrap_or_default().into(),
        outdated: s.outdated(),
        running: s.service == ServiceState::Running,
        changing: matches!(s.service, ServiceState::Starting | ServiceState::Stopping),
        answering: server.is_some(),
        host_name: server.map(|i| i.hostname.clone()).unwrap_or_default().into(),
        busy: server.is_some_and(|i| i.busy),
        port: s.ports.http as i32,
        controller: s.controller_driver,
    }
}

fn tray_text(s: &Status, own_sunshine: bool) -> &'static str {
    match (&s.install, s.service, &s.server) {
        (None, _, _) => "Streaming engine isn't set up",
        (_, _, Some(info)) if info.busy => "Streaming now",
        (_, _, Some(_)) => "Ready to stream",
        (_, ServiceState::Running, None) if own_sunshine => "Sunshine isn't answering",
        (_, ServiceState::Running, None) => "Streaming engine isn't answering",
        _ if own_sunshine => "Sunshine isn't running",
        _ => "Streaming engine isn't running",
    }
}
