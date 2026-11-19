//! Keeping the game on the virtual display: `--display follow <device>`.
//!
//! The virtual display is the primary screen while a phone streams, so new windows normally open
//! there. Some games don't: they remember the monitor they last ran on and go back to it (Don't
//! Starve Together does, full-screen). This helper runs in the signed-in session for as long as
//! the virtual display is on, and moves top-level windows of programs started during the stream
//! onto it. Each window is only handled for its first [`WATCH`], so a person who drags one back
//! to their own screen keeps it there. It ends by itself once the virtual display is off.
//!
//! A game in exclusive full-screen may ignore the move; borderless and windowed games follow.

use crate::{log, topology, wide};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, FILETIME, GetLastError, HWND, LPARAM, RECT, TRUE,
};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO, MONITORINFOEXW,
    MonitorFromWindow,
};
use windows_sys::Win32::System::Threading::{
    CreateMutexW, GetCurrentProcess, GetCurrentProcessId, GetProcessTimes, OpenProcess,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GWL_EXSTYLE, GetWindow, GetWindowLongW, GetWindowRect, GetWindowThreadProcessId, IsIconic,
    IsWindowVisible, IsZoomed, SW_MAXIMIZE, SW_RESTORE, SWP_NOACTIVATE, SWP_NOZORDER, SetWindowPos, ShowWindow,
    WS_EX_TOOLWINDOW,
};

/// How long after a window first shows up it's kept on the virtual display.
const WATCH: Duration = Duration::from_secs(20);
/// Moves per window: a game in exclusive full-screen that fights back is left alone after this.
const MAX_MOVES: u32 = 3;
const POLL: Duration = Duration::from_millis(500);
/// Programs started this long before the helper still count (the launch races the stream start).
const SLACK_100NS: u64 = 10 * 10_000_000;

struct Tracked {
    first_seen: Instant,
    moves: u32,
}

pub fn run(instance_id: &str) -> i32 {
    // One at a time: every prepare (and re-arrange) starts one.
    let name = wide("Global\\PocketDeckFollow");
    // SAFETY: a valid, NUL-terminated name. The handle stays open until the process ends.
    let mutex = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if mutex.is_null() || unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        return 0;
    }

    let since = creation_time(unsafe { GetCurrentProcess() }).unwrap_or(0).saturating_sub(SLACK_100NS);
    let own_pid = unsafe { GetCurrentProcessId() };
    let mut started: HashMap<u32, bool> = HashMap::new();
    let mut tracked: HashMap<isize, Tracked> = HashMap::new();

    loop {
        std::thread::sleep(POLL);
        let Some(gdi) = topology::active_gdi_name(instance_id) else { return 0 };
        let Some((target, area)) = monitor_named(&gdi) else { continue };

        for hwnd in top_level_windows() {
            let mut pid = 0u32;
            // SAFETY: hwnd came from EnumWindows; pid is a local.
            unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
            if pid == 0 || pid == own_pid {
                continue;
            }
            let new = *started.entry(pid).or_insert_with(|| process_created(pid).is_some_and(|t| t >= since));
            if !new {
                continue;
            }
            let t = tracked.entry(hwnd as isize).or_insert(Tracked { first_seen: Instant::now(), moves: 0 });
            if t.first_seen.elapsed() > WATCH || t.moves >= MAX_MOVES {
                continue;
            }
            // SAFETY: as above.
            let on = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
            if on.is_null() || on == target {
                continue;
            }
            t.moves += 1;
            move_onto(hwnd, on, area);
            log::line(format!("follow: moved a window of process {pid} onto {gdi}"));
        }
    }
}

/// Visible, unowned, non-tool top-level windows with a real size.
fn top_level_windows() -> Vec<HWND> {
    unsafe extern "system" fn collect(hwnd: HWND, out: LPARAM) -> i32 {
        // SAFETY: `out` is the Vec passed below, alive for the whole EnumWindows call.
        let out = unsafe { &mut *(out as *mut Vec<HWND>) };
        out.push(hwnd);
        TRUE
    }
    let mut all: Vec<HWND> = Vec::new();
    // SAFETY: the callback only pushes into `all`.
    unsafe { EnumWindows(Some(collect), &mut all as *mut Vec<HWND> as LPARAM) };
    all.into_iter()
        .filter(|&h| unsafe {
            IsWindowVisible(h) != 0
                && IsIconic(h) == 0
                && GetWindow(h, GW_OWNER).is_null()
                && (GetWindowLongW(h, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW) == 0
        })
        .filter(|&h| rect_of(h).is_some_and(|r| r.right - r.left >= 200 && r.bottom - r.top >= 150))
        .collect()
}

fn rect_of(hwnd: HWND) -> Option<RECT> {
    let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    // SAFETY: r is a local RECT.
    (unsafe { GetWindowRect(hwnd, &mut r) } != 0).then_some(r)
}

/// Moves the window onto `area`: full-screen windows (covering their monitor) take the whole of
/// it, maximized ones are maximized there, others keep their size (shrunk to fit) and are centred.
fn move_onto(hwnd: HWND, from: HMONITOR, area: RECT) {
    let Some(r) = rect_of(hwnd) else { return };
    let (aw, ah) = (area.right - area.left, area.bottom - area.top);
    let flags = SWP_NOZORDER | SWP_NOACTIVATE;
    // SAFETY: plain window calls on a window that existed a moment ago (they fail harmlessly if not).
    unsafe {
        if IsZoomed(hwnd) != 0 {
            ShowWindow(hwnd, SW_RESTORE);
            SetWindowPos(hwnd, std::ptr::null_mut(), area.left, area.top, aw.min(r.right - r.left), ah.min(r.bottom - r.top), flags);
            ShowWindow(hwnd, SW_MAXIMIZE);
            return;
        }
        if monitor_rect(from).is_some_and(|m| m.left == r.left && m.top == r.top && m.right == r.right && m.bottom == r.bottom) {
            SetWindowPos(hwnd, std::ptr::null_mut(), area.left, area.top, aw, ah, flags);
            return;
        }
        let (w, h) = ((r.right - r.left).min(aw), (r.bottom - r.top).min(ah));
        SetWindowPos(hwnd, std::ptr::null_mut(), area.left + (aw - w) / 2, area.top + (ah - h) / 2, w, h, flags);
    }
}

fn monitor_rect(m: HMONITOR) -> Option<RECT> {
    // SAFETY: a zeroed MONITORINFO with cbSize set.
    unsafe {
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        (GetMonitorInfoW(m, &mut info) != 0).then_some(info.rcMonitor)
    }
}

/// The monitor with GDI name `gdi` and its rectangle.
fn monitor_named(gdi: &str) -> Option<(HMONITOR, RECT)> {
    struct Search {
        name: String,
        found: Option<(HMONITOR, RECT)>,
    }
    unsafe extern "system" fn visit(m: HMONITOR, _dc: HDC, _r: *mut RECT, data: LPARAM) -> i32 {
        // SAFETY: `data` is the Search below, alive for the whole EnumDisplayMonitors call.
        let search = unsafe { &mut *(data as *mut Search) };
        // SAFETY: a zeroed MONITORINFOEXW with cbSize set.
        let mut info: MONITORINFOEXW = unsafe { std::mem::zeroed() };
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if unsafe { GetMonitorInfoW(m, &mut info as *mut MONITORINFOEXW as *mut MONITORINFO) } != 0
            && crate::from_wide(&info.szDevice).eq_ignore_ascii_case(&search.name)
        {
            search.found = Some((m, info.monitorInfo.rcMonitor));
            return 0;
        }
        TRUE
    }
    let mut search = Search { name: gdi.to_string(), found: None };
    // SAFETY: the callback only writes into `search`.
    unsafe { EnumDisplayMonitors(std::ptr::null_mut(), std::ptr::null(), Some(visit), &mut search as *mut Search as LPARAM) };
    search.found
}

fn process_created(pid: u32) -> Option<u64> {
    // SAFETY: the handle is closed below.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let t = creation_time(h);
        CloseHandle(h);
        t
    }
}

/// A process's creation time, in 100 ns units since 1601.
fn creation_time(process: windows_sys::Win32::Foundation::HANDLE) -> Option<u64> {
    let zero = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
    // SAFETY: four local FILETIMEs.
    let ok = unsafe { GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) };
    (ok != 0).then(|| (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
}
