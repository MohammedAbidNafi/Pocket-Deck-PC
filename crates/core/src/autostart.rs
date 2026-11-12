//! Start with Windows, through the current user's `Run` registry key (no admin needed).

use std::io;
use winreg::RegKey;
use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "PocketDeck";

/// Command-line flag the app gets when Windows starts it: open in the tray, no window.
pub const TRAY_FLAG: &str = "--tray";

fn command_line() -> io::Result<String> {
    let exe = std::env::current_exe()?;
    Ok(format!("\"{}\" {TRAY_FLAG}", exe.display()))
}

/// When set, PocketDeck never touches the `Run` key (development builds).
pub const DEV_FLAG: &str = "POCKETDECK_DEV";

/// Turns start-with-Windows on or off for this copy of PocketDeck.
pub fn set_enabled(enabled: bool) -> io::Result<()> {
    if std::env::var_os(DEV_FLAG).is_some() {
        return Ok(());
    }
    let run = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(RUN_KEY, KEY_SET_VALUE)?;
    if enabled {
        run.set_value(VALUE_NAME, &command_line()?)
    } else {
        match run.delete_value(VALUE_NAME) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }
}

/// True when Windows will start this copy of PocketDeck at sign-in.
pub fn is_enabled() -> bool {
    if std::env::var_os(DEV_FLAG).is_some() {
        return true;
    }
    let Ok(run) = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(RUN_KEY, KEY_READ) else {
        return false;
    };
    let Ok(value) = run.get_value::<String, _>(VALUE_NAME) else {
        return false;
    };
    command_line().map(|expected| value.eq_ignore_ascii_case(&expected)).unwrap_or(false)
}
