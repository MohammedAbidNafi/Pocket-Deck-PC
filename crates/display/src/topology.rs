//! Arranging the screens: the virtual display as the only active one, at the phone's mode, and
//! back to normal afterwards.
//!
//! These calls act on the signed-in session's desktop, so they can't run from the service
//! (session 0). The service starts `PocketDeckDisplay.exe --display arrange …` in the console
//! session for them instead (see `session.rs`). Changing the display layout needs no admin.
//!
//! Sunshine captures the primary display when no output is configured, and the only active
//! display is the primary one, so nothing in Sunshine's own settings has to change. When the
//! virtual display is switched off again, Windows puts back the layout it remembered for the
//! physical screens; [`restore`] asks for that explicitly as well.

use crate::{Mode, from_wide, wide};
use std::time::{Duration, Instant};
use windows_sys::Win32::Devices::Display::{
    DISPLAYCONFIG_ADAPTER_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_ADAPTER_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
    DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_MODE_INFO,
    DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME, DISPLAYCONFIG_TARGET_DEVICE_NAME,
    DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QDC_ALL_PATHS, QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
    SDC_ALLOW_CHANGES, SDC_ALLOW_PATH_ORDER_CHANGES, SDC_APPLY, SDC_SAVE_TO_DATABASE, SDC_TOPOLOGY_SUPPLIED,
    SDC_USE_DATABASE_CURRENT, SDC_USE_SUPPLIED_DISPLAY_CONFIG, SetDisplayConfig,
};
use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, LUID, POINTL};
use windows_sys::Win32::Graphics::Gdi::{
    CDS_UPDATEREGISTRY, ChangeDisplaySettingsExW, DEVMODEW, DISP_CHANGE_SUCCESSFUL, DISPLAYCONFIG_PATH_ACTIVE,
    DISPLAYCONFIG_PATH_MODE_IDX_INVALID, DM_DISPLAYFREQUENCY, DM_PELSHEIGHT, DM_PELSWIDTH, ENUM_CURRENT_SETTINGS,
    EnumDisplaySettingsW,
};

fn query(flags: u32) -> Result<(Vec<DISPLAYCONFIG_PATH_INFO>, Vec<DISPLAYCONFIG_MODE_INFO>), String> {
    // The layout can change between the two calls; try again when it grew.
    for _ in 0..5 {
        let (mut n_paths, mut n_modes) = (0u32, 0u32);
        // SAFETY: out-pointers to locals.
        let r = unsafe { GetDisplayConfigBufferSizes(flags, &mut n_paths, &mut n_modes) };
        if r != ERROR_SUCCESS {
            return Err(format!("Windows didn't list the displays ({r})."));
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); n_paths as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); n_modes as usize];
        // SAFETY: the buffers hold the counts passed in.
        let r = unsafe {
            QueryDisplayConfig(flags, &mut n_paths, paths.as_mut_ptr(), &mut n_modes, modes.as_mut_ptr(), std::ptr::null_mut())
        };
        if r == ERROR_INSUFFICIENT_BUFFER {
            continue;
        }
        if r != ERROR_SUCCESS {
            return Err(format!("Windows didn't list the displays ({r})."));
        }
        paths.truncate(n_paths as usize);
        modes.truncate(n_modes as usize);
        return Ok((paths, modes));
    }
    Err("The display layout kept changing.".into())
}

fn header(kind: i32, size: usize, adapter: LUID, id: u32) -> DISPLAYCONFIG_DEVICE_INFO_HEADER {
    DISPLAYCONFIG_DEVICE_INFO_HEADER { r#type: kind, size: size as u32, adapterId: adapter, id }
}

fn adapter_path(adapter: LUID) -> String {
    let mut info = DISPLAYCONFIG_ADAPTER_NAME {
        header: header(DISPLAYCONFIG_DEVICE_INFO_GET_ADAPTER_NAME, std::mem::size_of::<DISPLAYCONFIG_ADAPTER_NAME>(), adapter, 0),
        ..Default::default()
    };
    // SAFETY: the header's size matches the struct.
    unsafe { DisplayConfigGetDeviceInfo(&mut info.header) };
    from_wide(&info.adapterDevicePath)
}

fn target_path(adapter: LUID, id: u32) -> String {
    let mut info = DISPLAYCONFIG_TARGET_DEVICE_NAME {
        header: header(DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>(), adapter, id),
        ..Default::default()
    };
    // SAFETY: as above.
    unsafe { DisplayConfigGetDeviceInfo(&mut info.header) };
    from_wide(&info.monitorDevicePath)
}

fn source_gdi_name(adapter: LUID, id: u32) -> String {
    let mut info = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
        header: header(DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>(), adapter, id),
        ..Default::default()
    };
    // SAFETY: as above.
    unsafe { DisplayConfigGetDeviceInfo(&mut info.header) };
    from_wide(&info.viewGdiDeviceName)
}

/// True when a display path belongs to the virtual display adapter `instance_id`
/// (`ROOT\DISPLAY\0002` shows up in the adapter's path as `ROOT#DISPLAY#0002`).
pub fn belongs_to(instance_id: &str, adapter_path: &str, target_path: &str) -> bool {
    let needle = instance_id.replace('\\', "#").to_ascii_uppercase();
    let adapter = adapter_path.to_ascii_uppercase();
    // Whole components only: `\\?\ROOT#DISPLAY#0002#{…}` holds 0002 but not 000.
    let in_adapter = !needle.is_empty()
        && (adapter.contains(&format!("\\{needle}#")) || adapter.contains(&format!("#{needle}#")));
    // The driver's EDID names its monitor "MTT1337"; a fallback if the adapter path differs.
    in_adapter || target_path.to_ascii_uppercase().contains("#MTT1337#")
}

fn vdd_path(instance_id: &str, flags: u32) -> Result<Option<DISPLAYCONFIG_PATH_INFO>, String> {
    let (paths, _) = query(flags)?;
    Ok(paths.into_iter().find(|p| {
        p.targetInfo.targetAvailable != 0
            && belongs_to(instance_id, &adapter_path(p.targetInfo.adapterId), &target_path(p.targetInfo.adapterId, p.targetInfo.id))
    }))
}

/// Makes the virtual display the only active screen, at `mode`. Waits up to `limit` for it to
/// show up after being switched on. Returns its GDI name (`\\.\DISPLAY5`).
pub fn arrange(instance_id: &str, mode: Mode, limit: Duration) -> Result<String, String> {
    let start = Instant::now();
    let mut path = loop {
        if let Some(p) = vdd_path(instance_id, QDC_ALL_PATHS)? {
            break p;
        }
        if start.elapsed() >= limit {
            return Err("The virtual display didn't appear in time.".into());
        }
        std::thread::sleep(Duration::from_millis(200));
    };

    // Windows normally adds the new monitor to the desktop by itself. Then keep only its active
    // path (with its own modes, moved to 0,0 so it's primary). Otherwise, hand Windows the bare
    // topology and let it pick modes from its database.
    let mut done = false;
    for _ in 0..15 {
        if keep_only_active(instance_id)? {
            done = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    if !done {
        path.flags = DISPLAYCONFIG_PATH_ACTIVE;
        path.sourceInfo.Anonymous.modeInfoIdx = DISPLAYCONFIG_PATH_MODE_IDX_INVALID;
        path.targetInfo.Anonymous.modeInfoIdx = DISPLAYCONFIG_PATH_MODE_IDX_INVALID;
        // SAFETY: one valid path, no modes, as SDC_TOPOLOGY_SUPPLIED requires.
        let r = unsafe {
            SetDisplayConfig(1, &path, 0, std::ptr::null(), SDC_APPLY | SDC_TOPOLOGY_SUPPLIED | SDC_ALLOW_PATH_ORDER_CHANGES)
        };
        if r != ERROR_SUCCESS as i32 {
            return Err(format!("Windows wouldn't make the virtual display the only screen ({r})."));
        }
    }

    // Find it again among the active paths for its GDI name, then set the mode.
    let active = (0..25)
        .find_map(|_| match vdd_path(instance_id, QDC_ONLY_ACTIVE_PATHS) {
            Ok(Some(p)) => Some(p),
            _ => {
                std::thread::sleep(Duration::from_millis(200));
                None
            }
        })
        .ok_or("The virtual display didn't become active.")?;
    let name = source_gdi_name(active.sourceInfo.adapterId, active.sourceInfo.id);
    set_mode(&name, mode)?;
    Ok(name)
}

/// When the virtual display is among the active paths: applies a layout of that path alone, at
/// 0,0. `Ok(false)`: it isn't active (yet).
fn keep_only_active(instance_id: &str) -> Result<bool, String> {
    let (paths, modes) = query(QDC_ONLY_ACTIVE_PATHS)?;
    let Some(mut path) = paths.into_iter().find(|p| {
        belongs_to(instance_id, &adapter_path(p.targetInfo.adapterId), &target_path(p.targetInfo.adapterId, p.targetInfo.id))
    }) else {
        return Ok(false);
    };
    // Only this path's two modes, re-indexed.
    let mut kept: Vec<DISPLAYCONFIG_MODE_INFO> = Vec::new();
    // SAFETY: without QDC_VIRTUAL_MODE_AWARE the union holds plain mode indexes.
    let (source_idx, target_idx) = unsafe { (path.sourceInfo.Anonymous.modeInfoIdx, path.targetInfo.Anonymous.modeInfoIdx) };
    if let Some(m) = modes.get(source_idx as usize) {
        let mut m = *m;
        // A source mode's union holds sourceMode (writing a union field needs no unsafe).
        m.Anonymous.sourceMode.position = POINTL { x: 0, y: 0 };
        path.sourceInfo.Anonymous.modeInfoIdx = kept.len() as u32;
        kept.push(m);
    } else {
        path.sourceInfo.Anonymous.modeInfoIdx = DISPLAYCONFIG_PATH_MODE_IDX_INVALID;
    }
    if let Some(m) = modes.get(target_idx as usize) {
        path.targetInfo.Anonymous.modeInfoIdx = kept.len() as u32;
        kept.push(*m);
    } else {
        path.targetInfo.Anonymous.modeInfoIdx = DISPLAYCONFIG_PATH_MODE_IDX_INVALID;
    }
    path.flags = DISPLAYCONFIG_PATH_ACTIVE;
    // SAFETY: one path and the modes it points at.
    let r = unsafe {
        SetDisplayConfig(
            1,
            &path,
            kept.len() as u32,
            kept.as_ptr(),
            SDC_APPLY | SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_ALLOW_CHANGES | SDC_SAVE_TO_DATABASE,
        )
    };
    if r != ERROR_SUCCESS as i32 {
        return Err(format!("Windows wouldn't make the virtual display the only screen ({r})."));
    }
    Ok(true)
}

/// Sets `mode` on the display `gdi_name`, falling back to the resolution alone when the refresh
/// rate isn't on offer.
fn set_mode(gdi_name: &str, mode: Mode) -> Result<(), String> {
    let name = wide(gdi_name);
    let mut dm = DEVMODEW { dmSize: std::mem::size_of::<DEVMODEW>() as u16, ..unsafe { std::mem::zeroed() } };
    // SAFETY: dm is sized; name is NUL-terminated.
    unsafe { EnumDisplaySettingsW(name.as_ptr(), ENUM_CURRENT_SETTINGS, &mut dm) };
    if dm.dmPelsWidth == mode.width && dm.dmPelsHeight == mode.height && dm.dmDisplayFrequency == mode.fps {
        return Ok(());
    }
    dm.dmPelsWidth = mode.width;
    dm.dmPelsHeight = mode.height;
    dm.dmDisplayFrequency = mode.fps;
    // Remembered for next time if Windows lets us; for this session only otherwise.
    for flags in [CDS_UPDATEREGISTRY, 0] {
        for fields in [DM_PELSWIDTH | DM_PELSHEIGHT | DM_DISPLAYFREQUENCY, DM_PELSWIDTH | DM_PELSHEIGHT] {
            dm.dmFields = fields;
            // SAFETY: as above.
            let r = unsafe { ChangeDisplaySettingsExW(name.as_ptr(), &dm, std::ptr::null_mut(), flags, std::ptr::null()) };
            if r == DISP_CHANGE_SUCCESSFUL {
                return Ok(());
            }
        }
    }
    Err(format!("Windows wouldn't switch the virtual display to {mode}."))
}

/// Puts back the layout Windows remembers for the screens connected now.
pub fn restore() -> Result<(), String> {
    // SAFETY: no paths or modes, as SDC_USE_DATABASE_CURRENT requires.
    let r = unsafe { SetDisplayConfig(0, std::ptr::null(), 0, std::ptr::null(), SDC_APPLY | SDC_USE_DATABASE_CURRENT) };
    if r == ERROR_SUCCESS as i32 { Ok(()) } else { Err(format!("Windows didn't restore the display layout ({r}).")) }
}

/// Every display Windows can see right now, for `--display list`.
pub fn describe() -> Result<Vec<String>, String> {
    let (paths, _) = query(QDC_ALL_PATHS)?;
    let mut out = Vec::new();
    for p in paths.iter().filter(|p| p.targetInfo.targetAvailable != 0) {
        let line = format!(
            "{} source {} target {}: adapter {} monitor {}",
            if p.flags & DISPLAYCONFIG_PATH_ACTIVE != 0 { "active  " } else { "inactive" },
            p.sourceInfo.id,
            p.targetInfo.id,
            adapter_path(p.targetInfo.adapterId),
            target_path(p.targetInfo.adapterId, p.targetInfo.id),
        );
        if !out.contains(&line) {
            out.push(line);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_adapter_by_instance() {
        let adapter = r"\\?\ROOT#DISPLAY#0002#{5b45201d-f2f2-4f3b-85bb-30ff1f953599}";
        assert!(belongs_to(r"ROOT\DISPLAY\0002", adapter, ""));
        assert!(!belongs_to(r"ROOT\DISPLAY\0000", adapter, ""));
        // 0002 must not match 00021.
        assert!(!belongs_to(r"ROOT\DISPLAY\000", adapter, ""));
    }

    #[test]
    fn falls_back_to_the_monitor_name() {
        assert!(belongs_to(r"ROOT\DISPLAY\0009", "", r"\\?\DISPLAY#MTT1337#5&2a3b&0&UID256#{e6f07b5f}"));
    }

    #[test]
    fn listing_works_without_admin() {
        // The test runner is in a signed-in session; this must not fail there.
        assert!(describe().is_ok());
    }
}
