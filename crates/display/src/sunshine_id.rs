//! The ID Sunshine knows a monitor by, so its `output_name` setting can point at the virtual
//! display.
//!
//! With several screens on, Sunshine doesn't capture the primary one: with `output_name` empty it
//! takes the first output of the first graphics adapter, which is a PC monitor, not the virtual
//! display (that sits on its own adapter). With `output_name` set to an ID that isn't connected
//! right now (the virtual display while it's off), Sunshine falls back to that same first output,
//! so naming the virtual display for good is safe.
//!
//! The ID is computed exactly as Sunshine's display library (libdisplaydevice,
//! `WinApiLayer::getDeviceId`) does: a name-based (SHA-1, v5) UUID with the nil namespace over the
//! monitor's EDID followed by the stable parts of its device instance ID, as UTF-16 including the
//! terminating NUL. `--display ids` prints it for every monitor, to check against Sunshine's log.

use crate::{from_wide, wide};
use sha1::{Digest, Sha1};
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    DICS_FLAG_GLOBAL, DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, DIREG_DEV, HDEVINFO, SP_DEVINFO_DATA,
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiGetClassDevsW, SetupDiGetDeviceInstanceIdW,
    SetupDiOpenDevRegKey,
};
use windows_sys::Win32::Devices::Display::GUID_DEVINTERFACE_MONITOR;
use windows_sys::Win32::Foundation::{ERROR_SUCCESS, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Registry::{KEY_READ, RegCloseKey, RegQueryValueExW};

/// The virtual display's monitor: `DISPLAY\MTT1337\…` (its EDID says manufacturer MTT, product
/// 1337).
pub const VDD_MONITOR_PREFIX: &str = r"DISPLAY\MTT1337\";

/// A name-based (v5) UUID with the nil namespace, as `{xxxxxxxx-xxxx-…}`.
pub fn uuid_v5_nil(data: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update([0u8; 16]);
    hasher.update(data);
    let hash = hasher.finalize();
    let mut b = [0u8; 16];
    b.copy_from_slice(&hash[..16]);
    b[6] = (b[6] & 0x0f) | 0x50;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{{{}-{}-{}-{}-{}}}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

/// The ID Sunshine gives a monitor with this EDID and instance ID (`instance_id` as SetupAPI
/// returns it, with its terminating NUL). `None` when the instance ID doesn't have the shape
/// Sunshine expects, in which case Sunshine falls back to an ID we can't predict.
pub fn device_id(edid: &[u8], instance_id: &[u16]) -> Option<String> {
    let amp = u16::from(b'&');
    let first = instance_id.iter().position(|c| *c == amp)?;
    let unstable = first + 1 + instance_id[first + 1..].iter().position(|c| *c == amp)?;
    let semi_stable = unstable + 1 + instance_id[unstable + 1..].iter().position(|c| *c == amp)?;
    let mut data = edid.to_vec();
    for c in instance_id[..unstable].iter().chain(&instance_id[semi_stable..]) {
        data.extend_from_slice(&c.to_le_bytes());
    }
    Some(uuid_v5_nil(&data))
}

/// A monitor Windows has right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Monitor {
    pub instance_id: String,
    /// Sunshine's ID for it, when it can be worked out.
    pub sunshine_id: Option<String>,
}

/// Every monitor that's present (switched-off devices aren't), with Sunshine's ID for each.
pub fn monitors() -> Vec<Monitor> {
    let mut out = Vec::new();
    // SAFETY: the set is destroyed below; each buffer is sized as passed.
    unsafe {
        let set: HDEVINFO =
            SetupDiGetClassDevsW(&GUID_DEVINTERFACE_MONITOR, std::ptr::null(), std::ptr::null_mut(), DIGCF_DEVICEINTERFACE | DIGCF_PRESENT);
        if set == INVALID_HANDLE_VALUE as HDEVINFO {
            return out;
        }
        let mut index = 0;
        loop {
            let mut data = SP_DEVINFO_DATA { cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32, ..Default::default() };
            if SetupDiEnumDeviceInfo(set, index, &mut data) == 0 {
                break;
            }
            index += 1;
            // Instance ID with its NUL, exactly as libdisplaydevice keeps it.
            let mut needed = 0u32;
            SetupDiGetDeviceInstanceIdW(set, &data, std::ptr::null_mut(), 0, &mut needed);
            if needed == 0 {
                continue;
            }
            let mut id = vec![0u16; needed as usize];
            if SetupDiGetDeviceInstanceIdW(set, &data, id.as_mut_ptr(), needed, std::ptr::null_mut()) == 0 {
                continue;
            }
            let sunshine_id = edid(set, &data).and_then(|e| device_id(&e, &id));
            out.push(Monitor { instance_id: from_wide(&id), sunshine_id });
        }
        SetupDiDestroyDeviceInfoList(set);
    }
    out
}

/// The monitor's EDID from its device registry key.
unsafe fn edid(set: HDEVINFO, data: &SP_DEVINFO_DATA) -> Option<Vec<u8>> {
    // SAFETY (caller): set and data are valid. The key is closed before returning.
    unsafe {
        let key = SetupDiOpenDevRegKey(set, data, DICS_FLAG_GLOBAL, 0, DIREG_DEV, KEY_READ);
        if std::ptr::eq(key, INVALID_HANDLE_VALUE) {
            return None;
        }
        let name = wide("EDID");
        let mut size = 0u32;
        let mut out = None;
        if RegQueryValueExW(key, name.as_ptr(), std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut(), &mut size) == ERROR_SUCCESS && size > 0 {
            let mut buf = vec![0u8; size as usize];
            if RegQueryValueExW(key, name.as_ptr(), std::ptr::null(), std::ptr::null_mut(), buf.as_mut_ptr(), &mut size) == ERROR_SUCCESS {
                buf.truncate(size as usize);
                out = Some(buf);
            }
        }
        RegCloseKey(key);
        out
    }
}

/// Sunshine's ID for the virtual display, while it's switched on.
pub fn vdd() -> Option<String> {
    monitors()
        .into_iter()
        .find(|m| m.instance_id.to_ascii_uppercase().starts_with(VDD_MONITOR_PREFIX))
        .and_then(|m| m.sunshine_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn makes_v5_uuids_with_the_nil_namespace() {
        // Python: uuid.uuid5(uuid.UUID(int=0), "hello")
        assert_eq!(uuid_v5_nil(b"hello"), "{b7502f40-1152-59f2-ba10-69aeed522cdf}");
    }

    #[test]
    fn drops_the_unstable_part_of_the_instance_id() {
        let with_nul = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
        // Only the 4th part (the rotating counter) differs: same ID.
        let a = device_id(b"edid", &with_nul(r"DISPLAY\MTT1337\1&33320f0&0&UID256"));
        let b = device_id(b"edid", &with_nul(r"DISPLAY\MTT1337\1&33320f0&7&UID256"));
        assert!(a.is_some());
        assert_eq!(a, b);
        // A different port (5th part) or EDID: a different ID.
        assert_ne!(a, device_id(b"edid", &with_nul(r"DISPLAY\MTT1337\1&33320f0&0&UID257")));
        assert_ne!(a, device_id(b"other", &with_nul(r"DISPLAY\MTT1337\1&33320f0&0&UID256")));
        // Not enough parts: Sunshine doesn't use this scheme either.
        assert_eq!(device_id(b"edid", &with_nul(r"DISPLAY\X\1&2")), None);
    }

    #[test]
    fn lists_monitors_without_admin() {
        let _ = monitors();
    }
}
