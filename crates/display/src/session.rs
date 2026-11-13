//! Running the display-layout step in the signed-in session.
//!
//! The service lives in session 0, which has no access to the desktop's display layout. Like
//! Sunshine's own service, it starts a copy of itself in the console session with its own
//! SYSTEM token (moved to that session) and waits for it. When this process already is in the
//! console session (testing from a terminal) it just runs the command directly.

use crate::wide;
use std::path::Path;
use std::time::Duration;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::Security::{
    DuplicateTokenEx, SecurityImpersonation, SetTokenInformation, TOKEN_ALL_ACCESS, TokenPrimary, TokenSessionId,
};
use windows_sys::Win32::System::RemoteDesktop::{ProcessIdToSessionId, WTSGetActiveConsoleSessionId};
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CreateProcessAsUserW, GetCurrentProcess, GetCurrentProcessId, GetExitCodeProcess,
    OpenProcessToken, PROCESS_INFORMATION, STARTUPINFOW, TerminateProcess, WaitForSingleObject,
};

struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: an open handle we own.
            unsafe { CloseHandle(self.0) };
        }
    }
}

/// Runs `exe args` in the console session and returns its exit code.
pub fn run_in_console(exe: &Path, args: &[String], limit: Duration) -> Result<u32, String> {
    // SAFETY: plain calls with out-pointers to locals.
    let (console, ours) = unsafe {
        let console = WTSGetActiveConsoleSessionId();
        let mut ours = u32::MAX;
        ProcessIdToSessionId(GetCurrentProcessId(), &mut ours);
        (console, ours)
    };
    if console == u32::MAX {
        return Err("Nobody is signed in to Windows.".into());
    }
    if console == ours {
        let status = std::process::Command::new(exe).args(args).status().map_err(|e| e.to_string())?;
        return Ok(status.code().unwrap_or(1) as u32);
    }

    let mut command_line = wide(&command_line(exe, args));
    let mut desktop = wide("winsta0\\default");
    // SAFETY: every handle is wrapped and closed; buffers outlive the calls that use them.
    unsafe {
        let mut own = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_ALL_ACCESS, &mut own) == 0 {
            return Err(os_error("open the service's token"));
        }
        let own = Handle(own);
        let mut token = std::ptr::null_mut();
        if DuplicateTokenEx(own.0, TOKEN_ALL_ACCESS, std::ptr::null(), SecurityImpersonation, TokenPrimary, &mut token) == 0 {
            return Err(os_error("copy the service's token"));
        }
        let token = Handle(token);
        if SetTokenInformation(token.0, TokenSessionId, (&console as *const u32).cast(), 4) == 0 {
            return Err(os_error("move to the signed-in session"));
        }
        let mut startup: STARTUPINFOW = std::mem::zeroed();
        startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        startup.lpDesktop = desktop.as_mut_ptr();
        let mut info: PROCESS_INFORMATION = std::mem::zeroed();
        if CreateProcessAsUserW(
            token.0,
            std::ptr::null(),
            command_line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            CREATE_NO_WINDOW,
            std::ptr::null(),
            std::ptr::null(),
            &startup,
            &mut info,
        ) == 0
        {
            return Err(os_error("start the display helper"));
        }
        let process = Handle(info.hProcess);
        let _thread = Handle(info.hThread);
        match WaitForSingleObject(process.0, limit.as_millis().min(u32::MAX as u128) as u32) {
            WAIT_OBJECT_0 => {}
            WAIT_TIMEOUT => {
                TerminateProcess(process.0, 1);
                return Err("The display helper took too long.".into());
            }
            _ => return Err(os_error("wait for the display helper")),
        }
        let mut code = 1u32;
        GetExitCodeProcess(process.0, &mut code);
        Ok(code)
    }
}

/// A Windows command line: the program and each argument quoted when they need it.
pub fn command_line(exe: &Path, args: &[String]) -> String {
    let mut out = quote(&exe.display().to_string());
    for a in args {
        out.push(' ');
        out.push_str(&quote(a));
    }
    out
}

fn quote(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.to_string();
    }
    // Arguments here are paths, numbers and device IDs: no quotes inside to escape.
    format!("\"{}\"", arg.replace('"', ""))
}

fn os_error(what: &str) -> String {
    format!("Couldn't {what} ({}).", std::io::Error::last_os_error())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_when_needed() {
        let exe = Path::new(r"C:\Program Files\PocketDeck\PocketDeckDisplay.exe");
        let args = vec!["--display".to_string(), "arrange".into(), r"ROOT\DISPLAY\0002".into(), "1920".into()];
        assert_eq!(
            command_line(exe, &args),
            r#""C:\Program Files\PocketDeck\PocketDeckDisplay.exe" --display arrange ROOT\DISPLAY\0002 1920"#
        );
    }
}
