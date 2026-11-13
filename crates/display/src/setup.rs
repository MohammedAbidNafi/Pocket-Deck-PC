//! Installing and removing the virtual display, run elevated (one administrator prompt):
//! `PocketDeck.exe --display setup <driver zip>` and `PocketDeck.exe --display remove`.
//!
//! Setup copies the program to `%ProgramFiles%\PocketDeck` (the service runs as SYSTEM, so its
//! program must not be writable by ordinary accounts), installs the pinned driver unless the PC
//! already has it, switches the display off, registers and starts the service, and opens the
//! firewall for the phone's endpoint. Everything it did is recorded under [`crate::REG_KEY`]
//! so removal undoes exactly that and nothing more.

use crate::device::{self, State};
use crate::{Mode, REG_KEY, SERVICE_DISPLAY_NAME, SERVICE_NAME, install_dir, log, service_exe, vdd};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use windows_service::service::{
    ServiceAccess, ServiceAction, ServiceActionType, ServiceErrorControl, ServiceFailureActions,
    ServiceFailureResetPeriod, ServiceInfo, ServiceStartType, ServiceState, ServiceType,
};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
use winreg::RegKey;
use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_ALL_ACCESS, KEY_READ};

const FIREWALL_RULE: &str = "PocketDeck virtual display";

/// Exit codes of the elevated commands, so the app can explain what went wrong.
pub mod exit {
    pub const OK: i32 = 0;
    pub const USAGE: i32 = 2;
    pub const CHECKSUM: i32 = 3;
    pub const DRIVER: i32 = 4;
    pub const SERVICE: i32 = 5;
    pub const OTHER: i32 = 6;
}

/// Why an elevated command failed, with the exit code that says so.
pub struct Failure {
    pub code: i32,
    pub message: String,
}

fn fail(code: i32, message: impl Into<String>) -> Failure {
    Failure { code, message: message.into() }
}

fn system32(program: &str) -> PathBuf {
    let root = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    root.join("System32").join(program)
}

fn run(program: &str, args: &[&str]) -> Result<i32, String> {
    let out = Command::new(system32(program)).args(args).output().map_err(|e| format!("{program}: {e}"))?;
    let code = out.status.code().unwrap_or(-1);
    if code != 0 {
        log::line(format!(
            "{program} {}: exit {code}: {} {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stdout).trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(code)
}

fn record() -> Result<RegKey, String> {
    RegKey::predef(HKEY_LOCAL_MACHINE)
        .create_subkey(REG_KEY)
        .map(|(k, _)| k)
        .map_err(|e| format!("Couldn't save the setup record: {e}"))
}

/// The version of PocketDeck that set up the service, if it's set up.
pub fn installed_version() -> Option<String> {
    RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(REG_KEY, KEY_READ)
        .ok()?
        .get_value::<String, _>("Version")
        .ok()
}

/// Installs everything (elevated). `zip` is the driver release, already downloaded.
pub fn install(zip: &Path) -> Result<(), Failure> {
    log::line(format!("setup {} from {}", env!("CARGO_PKG_VERSION"), zip.display()));
    let dir = install_dir();
    std::fs::create_dir_all(&dir).map_err(|e| fail(exit::OTHER, format!("Couldn't create {}: {e}", dir.display())))?;

    // Check the download again here: the copy in the user's folder could have changed since
    // the unelevated app checked it.
    let staged = dir.join("driver.zip");
    std::fs::copy(zip, &staged).map_err(|e| fail(exit::OTHER, format!("Couldn't copy the driver: {e}")))?;
    let sum = pocketdeck_sunshine::install::sha256_file(&staged).map_err(|e| fail(exit::OTHER, e.to_string()))?;
    if sum != vdd::PINNED.sha256 {
        let _ = std::fs::remove_file(&staged);
        return Err(fail(exit::CHECKSUM, "The driver download was damaged or changed."));
    }

    stop_service();
    copy_program().map_err(|e| fail(exit::OTHER, e))?;

    let key = record().map_err(|e| fail(exit::OTHER, e))?;
    let own_device: Option<u32> = key.get_value("OwnDevice").ok();
    match device::find() {
        None => {
            install_driver(&staged, &dir.join("driver")).map_err(|e| fail(exit::DRIVER, e))?;
            let _ = key.set_value("OwnDevice", &1u32);
        }
        Some(d) if own_device.is_none() => {
            // The PC had the driver before PocketDeck: remember how it was, to leave it that way.
            let _ = key.set_value("OwnDevice", &0u32);
            let _ = key.set_value("WasEnabled", &u32::from(d.state != State::Off));
        }
        Some(_) => {}
    }
    let _ = std::fs::remove_file(&staged);

    // Our own settings folder, unless the driver already has one in use.
    let default_file = Path::new(vdd::DEFAULT_SETTINGS_DIR).join(vdd::SETTINGS_FILE);
    if vdd::configured_settings_dir().is_none() && !default_file.exists() {
        let settings = dir.join("vdd");
        let (driver_key, _) = RegKey::predef(HKEY_LOCAL_MACHINE)
            .create_subkey(vdd::DRIVER_REG_KEY)
            .map_err(|e| fail(exit::DRIVER, format!("Couldn't point the driver at its settings: {e}")))?;
        driver_key
            .set_value("VDDPATH", &settings.display().to_string())
            .map_err(|e| fail(exit::DRIVER, format!("Couldn't point the driver at its settings: {e}")))?;
        let _ = key.set_value("OwnSettings", &1u32);
    }
    vdd::ensure_mode(Mode { width: 1920, height: 1080, fps: 60 })
        .map_err(|e| fail(exit::DRIVER, format!("Couldn't write the driver's settings: {e}")))?;

    // Off until a phone asks for it.
    if !matches!(device::state(), State::Off | State::Missing) {
        if let Err(e) = device::set_enabled(false) {
            log::line(format!("setup: switch off: {e}"));
        }
        device::wait_for(State::Off, Duration::from_secs(5));
    }

    register_service().map_err(|e| fail(exit::SERVICE, e))?;
    open_firewall();
    let _ = key.set_value("Version", &env!("CARGO_PKG_VERSION"));
    log::line("setup done");
    Ok(())
}

fn copy_program() -> Result<(), String> {
    let from = std::env::current_exe().map_err(|e| e.to_string())?;
    let to = service_exe();
    if from == to {
        return Ok(());
    }
    // The old service may take a moment to let go of the file.
    let mut last = String::new();
    for _ in 0..20 {
        match std::fs::copy(&from, &to) {
            Ok(_) => return Ok(()),
            Err(e) => last = e.to_string(),
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Err(format!("Couldn't copy PocketDeck to {}: {last}", to.display()))
}

/// Unpacks the release with Windows' own `tar`, creates the device and binds the driver to it.
fn install_driver(zip: &Path, into: &Path) -> Result<(), String> {
    let _ = std::fs::remove_dir_all(into);
    std::fs::create_dir_all(into).map_err(|e| e.to_string())?;
    let (zip_s, into_s) = (zip.display().to_string(), into.display().to_string());
    if run("tar.exe", &["-xf", &zip_s, "-C", &into_s])? != 0 {
        return Err("Couldn't unpack the driver.".into());
    }
    let inf = into.join(vdd::INF_PATH);
    if !inf.is_file() {
        return Err(format!("The driver package has no {}.", vdd::INF_PATH));
    }
    device::create()?;
    let inf_s = inf.display().to_string();
    // 0: done; 3010: done, restart suggested.
    match run("pnputil.exe", &["/add-driver", &inf_s, "/install"])? {
        0 | 3010 => {}
        code => {
            let _ = device::remove();
            return Err(format!("Windows didn't install the virtual display driver (pnputil exit {code})."));
        }
    }
    if device::wait_for(State::On, Duration::from_secs(15)) == State::Missing {
        return Err("The virtual display didn't appear after installing the driver.".into());
    }
    Ok(())
}

fn manager(access: ServiceManagerAccess) -> Result<ServiceManager, String> {
    ServiceManager::local_computer(None::<&str>, access).map_err(|e| format!("Couldn't open the service manager: {e}"))
}

fn register_service() -> Result<(), String> {
    let manager = manager(ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE)?;
    let info = ServiceInfo {
        name: OsString::from(SERVICE_NAME),
        display_name: OsString::from(SERVICE_DISPLAY_NAME),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: service_exe(),
        launch_arguments: vec![OsString::from(crate::FLAG), OsString::from("service")],
        dependencies: vec![],
        account_name: None, // LocalSystem
        account_password: None,
    };
    let access = ServiceAccess::QUERY_STATUS | ServiceAccess::START | ServiceAccess::STOP | ServiceAccess::CHANGE_CONFIG;
    let service = match manager.open_service(SERVICE_NAME, access) {
        Ok(s) => {
            s.change_config(&info).map_err(|e| format!("Couldn't update the service: {e}"))?;
            s
        }
        Err(_) => manager.create_service(&info, access).map_err(|e| format!("Couldn't create the service: {e}"))?,
    };
    let _ = service.set_description(
        "Switches PocketDeck's virtual display on while your phone streams and off afterwards.",
    );
    let restart = ServiceAction { action_type: ServiceActionType::Restart, delay: Duration::from_secs(5) };
    let _ = service.update_failure_actions(ServiceFailureActions {
        reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(24 * 3600)),
        reboot_msg: None,
        command: None,
        actions: Some(vec![restart.clone(), restart.clone(), restart]),
    });
    service.start(&[] as &[&str]).map_err(|e| format!("Couldn't start the service: {e}"))?;
    Ok(())
}

fn stop_service() {
    let Ok(manager) = manager(ServiceManagerAccess::CONNECT) else { return };
    let Ok(service) = manager.open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS | ServiceAccess::STOP) else { return };
    let _ = service.stop();
    for _ in 0..40 {
        match service.query_status() {
            Ok(s) if s.current_state != ServiceState::Stopped => std::thread::sleep(Duration::from_millis(250)),
            _ => break,
        }
    }
}

fn open_firewall() {
    let program = format!("program={}", service_exe().display());
    let name = format!("name={FIREWALL_RULE}");
    let _ = run("netsh.exe", &["advfirewall", "firewall", "delete", "rule", &name]);
    let _ = run(
        "netsh.exe",
        &["advfirewall", "firewall", "add", "rule", &name, "dir=in", "action=allow", &program, "protocol=TCP", "enable=yes", "profile=any"],
    );
}

/// Removes everything setup did (elevated), and leaves a driver that was there before as it was.
pub fn remove() -> Result<(), Failure> {
    log::line("remove");
    stop_service();
    if let Ok(manager) = manager(ServiceManagerAccess::CONNECT) {
        if let Ok(service) = manager.open_service(SERVICE_NAME, ServiceAccess::DELETE) {
            service.delete().map_err(|e| fail(exit::SERVICE, format!("Couldn't remove the service: {e}")))?;
        }
    }
    let _ = run("netsh.exe", &["advfirewall", "firewall", "delete", "rule", &format!("name={FIREWALL_RULE}")]);

    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let key = hklm.open_subkey_with_flags(REG_KEY, KEY_READ).ok();
    let get = |name: &str| key.as_ref().and_then(|k| k.get_value::<u32, _>(name).ok());
    match get("OwnDevice") {
        Some(1) => {
            if let Err(e) = device::remove() {
                log::line(format!("remove device: {e}"));
            }
        }
        Some(0) if get("WasEnabled") == Some(1) => {
            let _ = device::set_enabled(true);
        }
        _ => {}
    }
    if get("OwnSettings") == Some(1) {
        if let Ok(k) = hklm.open_subkey_with_flags(vdd::DRIVER_REG_KEY, KEY_ALL_ACCESS) {
            let _ = k.delete_value("VDDPATH");
        }
    }
    let _ = hklm.delete_subkey_all(REG_KEY);
    let _ = std::fs::remove_dir_all(install_dir());
    log::line("removed");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_windows_tools() {
        assert!(system32("tar.exe").is_file(), "tar.exe ships with Windows 10 1803 and later");
        assert!(system32("pnputil.exe").is_file());
        assert!(system32("netsh.exe").is_file());
    }
}
