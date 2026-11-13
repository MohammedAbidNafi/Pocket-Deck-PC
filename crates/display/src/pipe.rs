//! The local control channel to the service: one line in, one JSON line out, then the pipe
//! closes. Local only (remote clients are refused); any signed-in account may use it, since all
//! it can do is switch the virtual display on and off.

use crate::wide;
use std::io::{Read, Write};
use std::os::windows::io::FromRawHandle;
use std::time::Duration;
use windows_sys::Win32::Foundation::{ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, GetLastError, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Security::Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows_sys::Win32::Storage::FileSystem::{FlushFileBuffers, PIPE_ACCESS_DUPLEX};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};

pub const PIPE_NAME: &str = r"\\.\pipe\PocketDeckDisplay";

/// SYSTEM and administrators: everything; signed-in accounts: read and write.
const SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;AU)";

const MAX_LINE: usize = 1024;

/// Answers requests forever, one thread per connection.
pub fn serve(handler: impl Fn(&str) -> String + Send + Sync + 'static) {
    let handler = std::sync::Arc::new(handler);
    let name = wide(PIPE_NAME);
    let sddl = wide(SDDL);
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: out-pointer to a local. The descriptor is never freed: the server runs for good.
    if unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), SDDL_REVISION_1, &mut descriptor, std::ptr::null_mut()) } == 0 {
        crate::log::line(format!("pipe: security descriptor: {}", std::io::Error::last_os_error()));
        return;
    }
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    loop {
        // SAFETY: name and attributes outlive the call; the handle is owned by a File below.
        let pipe = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                4096,
                4096,
                0,
                &attributes,
            )
        };
        if pipe == INVALID_HANDLE_VALUE {
            crate::log::line(format!("pipe: create: {}", std::io::Error::last_os_error()));
            std::thread::sleep(Duration::from_secs(5));
            continue;
        }
        // SAFETY: a valid pipe handle; ERROR_PIPE_CONNECTED means a client got in first, which is fine.
        let connected = unsafe { ConnectNamedPipe(pipe, std::ptr::null_mut()) != 0 || GetLastError() == ERROR_PIPE_CONNECTED };
        // SAFETY: the File takes ownership and closes the handle.
        let file = unsafe { std::fs::File::from_raw_handle(pipe) };
        if !connected {
            continue;
        }
        let handler = handler.clone();
        std::thread::spawn(move || answer(file, &*handler));
    }
}

fn answer(mut file: std::fs::File, handler: &dyn Fn(&str) -> String) {
    let Some(line) = read_line(&mut file) else { return };
    let reply = handler(line.trim());
    let _ = file.write_all(reply.as_bytes());
    let _ = file.write_all(b"\n");
    // SAFETY: the handle belongs to `file`, which is still open.
    unsafe {
        use std::os::windows::io::AsRawHandle;
        FlushFileBuffers(file.as_raw_handle());
        DisconnectNamedPipe(file.as_raw_handle());
    }
}

fn read_line(file: &mut impl Read) -> Option<String> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while buf.len() < MAX_LINE {
        match file.read(&mut byte) {
            Ok(1) if byte[0] == b'\n' => break,
            Ok(1) => buf.push(byte[0]),
            _ => break,
        }
    }
    String::from_utf8(buf).ok()
}

/// Sends one request to the service and returns its answer.
pub fn request(line: &str) -> Result<String, String> {
    let mut tries = 0;
    let mut file = loop {
        match std::fs::OpenOptions::new().read(true).write(true).open(PIPE_NAME) {
            Ok(f) => break f,
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) && tries < 40 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err("The PocketDeck virtual display service isn't running.".into());
            }
            Err(e) => return Err(e.to_string()),
        }
    };
    file.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
    file.write_all(b"\n").map_err(|e| e.to_string())?;
    let mut out = String::new();
    // The service closes its end after answering; a broken pipe there is the normal ending.
    let _ = file.read_to_string(&mut out);
    let out = out.trim().to_string();
    if out.is_empty() { Err("The service didn't answer.".into()) } else { Ok(out) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_one_bounded_line() {
        assert_eq!(read_line(&mut &b"status\nextra"[..]).as_deref(), Some("status"));
        assert_eq!(read_line(&mut &vec![b'a'; 5000][..]).map(|s| s.len()), Some(MAX_LINE));
    }
}
