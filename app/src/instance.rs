//! One PocketDeck at a time. A second launch asks the running one to show its window, then exits.

use std::os::windows::ffi::OsStrExt;
use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows_sys::Win32::System::Threading::{
    CreateEventW, CreateMutexW, EVENT_MODIFY_STATE, INFINITE, OpenEventW, SetEvent, WaitForSingleObject,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow};

const MUTEX: &str = "Local\\PocketDeckPC.Instance";
const SHOW_EVENT: &str = "Local\\PocketDeckPC.Show";

fn wide(s: &str) -> Vec<u16> {
    std::ffi::OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

pub enum Instance {
    /// This is the only PocketDeck running. Keep the value alive for the life of the app.
    Primary(#[allow(dead_code)] Guard),
    /// Another PocketDeck is running and has been asked to show itself.
    Secondary,
}

pub struct Guard(#[allow(dead_code)] HANDLE);

/// Claims the single-instance lock. `on_show` runs (on a background thread) whenever a later
/// launch asks this one to show its window.
pub fn acquire(on_show: impl Fn() + Send + 'static) -> Instance {
    // SAFETY: plain Win32 calls with valid, NUL-terminated names.
    unsafe {
        let mutex = CreateMutexW(std::ptr::null(), 0, wide(MUTEX).as_ptr());
        if !mutex.is_null() && GetLastError() == ERROR_ALREADY_EXISTS {
            let event = OpenEventW(EVENT_MODIFY_STATE, 0, wide(SHOW_EVENT).as_ptr());
            if !event.is_null() {
                // Let the running copy bring its window to the front.
                AllowSetForegroundWindow(ASFW_ANY);
                SetEvent(event);
            }
            return Instance::Secondary;
        }
        let event = CreateEventW(std::ptr::null(), 0, 0, wide(SHOW_EVENT).as_ptr());
        if !event.is_null() {
            let raw = event as usize;
            std::thread::Builder::new()
                .name("show-requests".into())
                .spawn(move || {
                    loop {
                        if WaitForSingleObject(raw as HANDLE, INFINITE) != 0 {
                            break;
                        }
                        on_show();
                    }
                })
                .ok();
        }
        Instance::Primary(Guard(mutex))
    }
}
