//! Keeping PocketDeck's Sunshine login, and resetting Sunshine's login when nobody knows it.
//!
//! The login is stored encrypted with Windows DPAPI, so only this Windows account on this PC
//! can read it back.

use crate::api::Login;
use std::path::{Path, PathBuf};
use windows_sys::Win32::Foundation::{HLOCAL, LocalFree};
use windows_sys::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};

const ENTROPY: &[u8] = b"PocketDeck Sunshine login v1";

pub fn path(config_dir: &Path) -> PathBuf {
    config_dir.join("sunshine-login.bin")
}

pub fn load(config_dir: &Path) -> Option<Login> {
    let sealed = std::fs::read(path(config_dir)).ok()?;
    let plain = unprotect(&sealed)?;
    let text = String::from_utf8(plain).ok()?;
    let (username, password) = text.split_once('\n')?;
    Some(Login { username: username.into(), password: password.into() })
}

pub fn save(config_dir: &Path, login: &Login) -> std::io::Result<()> {
    std::fs::create_dir_all(config_dir)?;
    let sealed = protect(format!("{}\n{}", login.username, login.password).as_bytes())
        .ok_or_else(|| std::io::Error::other("Windows couldn't encrypt the login"))?;
    let file = path(config_dir);
    let tmp = file.with_extension("tmp");
    std::fs::write(&tmp, sealed)?;
    std::fs::rename(&tmp, file)
}

pub fn forget(config_dir: &Path) {
    let _ = std::fs::remove_file(path(config_dir));
}

/// Gives Sunshine a new web login (Sunshine's own `--creds` command) and restarts it so the
/// login takes effect. One administrator prompt. Replaces whatever login Sunshine had.
pub fn reset(install: &crate::Install, login: &Login) -> crate::Result<()> {
    let exe = install.exe();
    // Single quotes: the password alphabet has no quotes, and PowerShell takes the rest literally.
    let script = format!(
        "& '{}' --creds '{}' '{}'; if ($LASTEXITCODE -ne 0) {{ exit 2 }}; Restart-Service -Name '{}' -Force",
        exe.display(),
        login.username,
        login.password,
        crate::SERVICE_NAME
    );
    let code = crate::elevate::run(
        "powershell.exe",
        &format!("-NoProfile -NonInteractive -WindowStyle Hidden -Command \"{script}\""),
    )?;
    if code == 0 {
        Ok(())
    } else {
        Err(crate::Error::Failed(format!("Sunshine couldn't save the new login (code {code}).")))
    }
}

fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
    CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 }
}

fn protect(data: &[u8]) -> Option<Vec<u8>> {
    let input = blob(data);
    let entropy = blob(ENTROPY);
    let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
    // SAFETY: input/entropy point at live slices; out is freed with LocalFree below.
    unsafe {
        if CryptProtectData(&input, std::ptr::null(), &entropy, std::ptr::null(), std::ptr::null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out) == 0 {
            return None;
        }
        let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        LocalFree(out.pbData as HLOCAL);
        Some(v)
    }
}

fn unprotect(data: &[u8]) -> Option<Vec<u8>> {
    let input = blob(data);
    let entropy = blob(ENTROPY);
    let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
    // SAFETY: as in `protect`.
    unsafe {
        if CryptUnprotectData(&input, std::ptr::null_mut(), &entropy, std::ptr::null(), std::ptr::null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out) == 0 {
            return None;
        }
        let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        LocalFree(out.pbData as HLOCAL);
        Some(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_encrypted() {
        let dir = std::env::temp_dir().join(format!("pocketdeck-login-{}", std::process::id()));
        let login = Login { username: "pocketdeck".into(), password: "s3cret pass".into() };
        save(&dir, &login).unwrap();
        let raw = std::fs::read(path(&dir)).unwrap();
        assert!(!raw.windows(6).any(|w| w == b"s3cret"), "stored in the clear");
        assert_eq!(load(&dir), Some(login));
        forget(&dir);
        assert_eq!(load(&dir), None);
        let _ = std::fs::remove_dir_all(dir);
    }
}
