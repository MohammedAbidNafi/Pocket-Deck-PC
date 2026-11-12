//! Sunshine's Windows service: reading its state (no admin needed) and starting or restarting it
//! (one administrator prompt).

use windows_service::service::{ServiceAccess, ServiceState as WinState};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ServiceState {
    #[default]
    NotInstalled,
    Stopped,
    Starting,
    Stopping,
    Running,
    /// Paused, or a state we couldn't read.
    Unknown,
}

pub fn state(name: &str) -> ServiceState {
    let Ok(manager) = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT) else {
        return ServiceState::Unknown;
    };
    let Ok(service) = manager.open_service(name, ServiceAccess::QUERY_STATUS) else {
        return ServiceState::NotInstalled;
    };
    match service.query_status().map(|s| s.current_state) {
        Ok(WinState::Running) => ServiceState::Running,
        Ok(WinState::Stopped) => ServiceState::Stopped,
        Ok(WinState::StartPending | WinState::ContinuePending) => ServiceState::Starting,
        Ok(WinState::StopPending | WinState::PausePending) => ServiceState::Stopping,
        _ => ServiceState::Unknown,
    }
}

pub fn exists(name: &str) -> bool {
    !matches!(state(name), ServiceState::NotInstalled)
}

/// Starts the service, or restarts it when it is already running. Shows one administrator prompt.
pub fn restart(name: &str) -> crate::Result<()> {
    let command = if state(name) == ServiceState::Running {
        format!("Restart-Service -Name '{name}' -Force")
    } else {
        format!("Start-Service -Name '{name}'")
    };
    let code = crate::elevate::run(
        "powershell.exe",
        &format!("-NoProfile -NonInteractive -WindowStyle Hidden -Command \"{command}\""),
    )?;
    if code == 0 {
        Ok(())
    } else {
        Err(crate::Error::Failed(format!("Windows couldn't start Sunshine (code {code}).")))
    }
}
