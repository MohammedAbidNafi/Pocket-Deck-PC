//! The virtual display adapter as a Windows device: finding it, creating it, and switching it on
//! and off. Reading needs nothing special; every change needs administrator rights (the service
//! runs as SYSTEM, and setup runs elevated).
//!
//! Switched off is the resting state. A disabled device stays disabled across restarts, so
//! Windows never boots onto the virtual monitor or remembers it as a place to open windows.

use crate::{from_wide, wide};
use std::time::{Duration, Instant};
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_DEVNODE_STATUS_FLAGS, CM_Get_DevNode_Status, CM_PROB, CM_PROB_DISABLED, CR_SUCCESS, DICD_GENERATE_ID,
    DICS_DISABLE, DICS_ENABLE, DICS_FLAG_GLOBAL, DIF_PROPERTYCHANGE, DIF_REGISTERDEVICE, DIF_REMOVE, DIGCF_PRESENT,
    DN_HAS_PROBLEM, DN_STARTED, GUID_DEVCLASS_DISPLAY, HDEVINFO, SP_CLASSINSTALL_HEADER, SP_DEVINFO_DATA,
    SP_PROPCHANGE_PARAMS, SP_REMOVEDEVICE_PARAMS, SPDRP_HARDWAREID, SetupDiCallClassInstaller,
    SetupDiCreateDeviceInfoList, SetupDiCreateDeviceInfoW, SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo,
    SetupDiGetClassDevsW, SetupDiGetDeviceInstanceIdW, SetupDiGetDeviceRegistryPropertyW,
    SetupDiSetClassInstallParamsW, SetupDiSetDeviceRegistryPropertyW,
};
use windows_sys::Win32::Foundation::{GetLastError, INVALID_HANDLE_VALUE};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// No virtual display adapter on this PC.
    Missing,
    /// Present and switched off.
    Off,
    /// Switched on and working.
    On,
    /// Switched on but not working (yet, or with a problem code).
    Starting,
    Problem(u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// For example `ROOT\DISPLAY\0002`.
    pub instance_id: String,
    pub state: State,
}

/// Owns a device information set and frees it on drop.
struct DevInfo(HDEVINFO);

impl Drop for DevInfo {
    fn drop(&mut self) {
        // SAFETY: the set came from SetupDiGetClassDevsW / SetupDiCreateDeviceInfoList.
        unsafe { SetupDiDestroyDeviceInfoList(self.0) };
    }
}

fn display_devices() -> Result<DevInfo, String> {
    // SAFETY: plain call; the result is checked before use.
    let set = unsafe { SetupDiGetClassDevsW(&GUID_DEVCLASS_DISPLAY, std::ptr::null(), std::ptr::null_mut(), DIGCF_PRESENT) };
    if set == INVALID_HANDLE_VALUE as HDEVINFO {
        return Err(last_error("list display devices"));
    }
    Ok(DevInfo(set))
}

/// The virtual display adapter and the set it belongs to, when there is one.
fn locate() -> Result<Option<(DevInfo, SP_DEVINFO_DATA)>, String> {
    let set = display_devices()?;
    let mut index = 0;
    loop {
        let mut data = SP_DEVINFO_DATA { cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32, ..Default::default() };
        // SAFETY: data is sized; the set is valid.
        if unsafe { SetupDiEnumDeviceInfo(set.0, index, &mut data) } == 0 {
            return Ok(None);
        }
        if is_vdd(&hardware_ids(&set, &data)) {
            return Ok(Some((set, data)));
        }
        index += 1;
    }
}

/// True for the hardware IDs of the driver PocketDeck uses (`Root\MttVDD`, or plain `MttVDD`).
pub fn is_vdd(ids: &[String]) -> bool {
    ids.iter().any(|id| {
        let id = id.to_ascii_lowercase();
        id == "root\\mttvdd" || id == "mttvdd"
    })
}

fn hardware_ids(set: &DevInfo, data: &SP_DEVINFO_DATA) -> Vec<String> {
    let mut buf = vec![0u16; 512];
    let mut needed = 0u32;
    // SAFETY: the buffer is (len * 2) bytes long.
    let ok = unsafe {
        SetupDiGetDeviceRegistryPropertyW(
            set.0,
            data,
            SPDRP_HARDWAREID,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            (buf.len() * 2) as u32,
            &mut needed,
        )
    };
    if ok == 0 {
        return Vec::new();
    }
    parse_multi_sz(&buf[..(needed as usize / 2).min(buf.len())])
}

/// A REG_MULTI_SZ value as its strings.
pub fn parse_multi_sz(buf: &[u16]) -> Vec<String> {
    buf.split(|c| *c == 0).filter(|s| !s.is_empty()).map(String::from_utf16_lossy).collect()
}

fn instance_id(set: &DevInfo, data: &SP_DEVINFO_DATA) -> String {
    let mut buf = [0u16; 260];
    // SAFETY: buffer length in characters as documented.
    unsafe { SetupDiGetDeviceInstanceIdW(set.0, data, buf.as_mut_ptr(), buf.len() as u32, std::ptr::null_mut()) };
    from_wide(&buf)
}

fn state_of(dev_inst: u32) -> State {
    let mut status: CM_DEVNODE_STATUS_FLAGS = 0;
    let mut problem: CM_PROB = 0;
    // SAFETY: out-pointers to locals.
    if unsafe { CM_Get_DevNode_Status(&mut status, &mut problem, dev_inst, 0) } != CR_SUCCESS {
        return State::Missing;
    }
    if status & DN_HAS_PROBLEM != 0 {
        return if problem == CM_PROB_DISABLED { State::Off } else { State::Problem(problem) };
    }
    if status & DN_STARTED != 0 { State::On } else { State::Starting }
}

/// The virtual display adapter, if this PC has one.
pub fn find() -> Option<Device> {
    let (set, data) = locate().ok()??;
    Some(Device { instance_id: instance_id(&set, &data), state: state_of(data.DevInst) })
}

pub fn state() -> State {
    find().map(|d| d.state).unwrap_or(State::Missing)
}

/// Switches the adapter on or off. Administrator only.
pub fn set_enabled(on: bool) -> Result<(), String> {
    let (set, data) = locate()?.ok_or("The virtual display driver isn't installed.")?;
    let params = SP_PROPCHANGE_PARAMS {
        ClassInstallHeader: SP_CLASSINSTALL_HEADER {
            cbSize: std::mem::size_of::<SP_CLASSINSTALL_HEADER>() as u32,
            InstallFunction: DIF_PROPERTYCHANGE,
        },
        StateChange: if on { DICS_ENABLE } else { DICS_DISABLE },
        Scope: DICS_FLAG_GLOBAL,
        HwProfile: 0,
    };
    // SAFETY: params outlives both calls; sizes are exact.
    unsafe {
        if SetupDiSetClassInstallParamsW(set.0, &data, &params.ClassInstallHeader, std::mem::size_of::<SP_PROPCHANGE_PARAMS>() as u32) == 0 {
            return Err(last_error("prepare the display change"));
        }
        if SetupDiCallClassInstaller(DIF_PROPERTYCHANGE, set.0, &data) == 0 {
            return Err(last_error(if on { "switch the virtual display on" } else { "switch the virtual display off" }));
        }
    }
    Ok(())
}

/// Waits until the adapter reaches `want` (or `limit` passes). Returns the last state seen.
pub fn wait_for(want: State, limit: Duration) -> State {
    let start = Instant::now();
    loop {
        let s = state();
        if s == want || start.elapsed() >= limit {
            return s;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// Creates the root-enumerated device the driver's INF matches (`Root\MttVDD`), as the
/// driver's own installer does. The driver gets bound to it afterwards (`pnputil /add-driver
/// … /install`). Administrator only.
pub fn create() -> Result<(), String> {
    // SAFETY: the set is freed by DevInfo; every pointer passed lives across its call.
    unsafe {
        let set = SetupDiCreateDeviceInfoList(&GUID_DEVCLASS_DISPLAY, std::ptr::null_mut());
        if set == INVALID_HANDLE_VALUE as HDEVINFO {
            return Err(last_error("start creating the virtual display"));
        }
        let set = DevInfo(set);
        let mut data = SP_DEVINFO_DATA { cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32, ..Default::default() };
        let class = wide("Display");
        if SetupDiCreateDeviceInfoW(set.0, class.as_ptr(), &GUID_DEVCLASS_DISPLAY, std::ptr::null(), std::ptr::null_mut(), DICD_GENERATE_ID, &mut data) == 0 {
            return Err(last_error("create the virtual display"));
        }
        // REG_MULTI_SZ: the ID, then an empty string.
        let mut ids: Vec<u16> = crate::vdd::HARDWARE_ID.encode_utf16().collect();
        ids.extend([0, 0]);
        if SetupDiSetDeviceRegistryPropertyW(set.0, &mut data, SPDRP_HARDWAREID, ids.as_ptr().cast(), (ids.len() * 2) as u32) == 0 {
            return Err(last_error("name the virtual display"));
        }
        if SetupDiCallClassInstaller(DIF_REGISTERDEVICE, set.0, &data) == 0 {
            return Err(last_error("register the virtual display"));
        }
    }
    Ok(())
}

/// Removes the adapter (the driver package stays in Windows' store). Administrator only.
pub fn remove() -> Result<(), String> {
    let Some((set, data)) = locate()? else { return Ok(()) };
    let params = SP_REMOVEDEVICE_PARAMS {
        ClassInstallHeader: SP_CLASSINSTALL_HEADER {
            cbSize: std::mem::size_of::<SP_CLASSINSTALL_HEADER>() as u32,
            InstallFunction: DIF_REMOVE,
        },
        Scope: 1, // DI_REMOVEDEVICE_GLOBAL
        HwProfile: 0,
    };
    // SAFETY: as in set_enabled.
    unsafe {
        if SetupDiSetClassInstallParamsW(set.0, &data, &params.ClassInstallHeader, std::mem::size_of::<SP_REMOVEDEVICE_PARAMS>() as u32) == 0
            || SetupDiCallClassInstaller(DIF_REMOVE, set.0, &data) == 0
        {
            return Err(last_error("remove the virtual display"));
        }
    }
    Ok(())
}

fn last_error(what: &str) -> String {
    // SAFETY: reads the calling thread's last error.
    let code = unsafe { GetLastError() };
    format!("Windows couldn't {what} ({}).", std::io::Error::from_raw_os_error(code as i32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_the_driver() {
        assert!(is_vdd(&["ROOT\\MttVDD".into()]));
        assert!(is_vdd(&["something".into(), "MttVDD".into()]));
        assert!(!is_vdd(&["ROOT\\DISPLAY".into(), "Root\\ParsecVDA".into()]));
    }

    #[test]
    fn splits_multi_strings() {
        let raw: Vec<u16> = "Root\\MttVDD\0MttVDD\0\0".encode_utf16().collect();
        assert_eq!(parse_multi_sz(&raw), vec!["Root\\MttVDD".to_string(), "MttVDD".to_string()]);
    }

    #[test]
    fn reading_needs_no_admin() {
        // Must not fail or panic on a PC without the driver.
        let _ = find();
    }
}
