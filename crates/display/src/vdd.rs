//! The virtual display driver: Virtual Display Driver (VDD) by MikeTheTech and the VirtualDrivers
//! contributors, MIT licence, signed through SignPath Foundation.
//! <https://github.com/VirtualDrivers/Virtual-Display-Driver>
//!
//! PocketDeck downloads one pinned release, checks its SHA-256 and installs it unchanged
//! (changing it would break the signature). The one thing it edits is the driver's settings
//! file, `vdd_settings.xml`, which lists the modes the virtual monitor offers: the driver reads
//! it each time the device starts, so a phone's mode is added before the display is switched on.

use pocketdeck_sunshine::install::Release;
use std::path::{Path, PathBuf};
use winreg::RegKey;
use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ};

/// The release PocketDeck installs. Bump only after testing a new one.
#[cfg(target_arch = "x86_64")]
pub const PINNED: Release = Release {
    version: "25.7.23",
    // Despite "x86" in the name this is the amd64 driver (its INF installs on NTamd64).
    url: "https://github.com/VirtualDrivers/Virtual-Display-Driver/releases/download/25.7.23/VirtualDisplayDriver-x86.Driver.Only.zip",
    sha256: "e24210692b442b39af763536330ce78b423f19342b7a7792c26de3944e418b3a",
    size: 132_118,
};

#[cfg(target_arch = "aarch64")]
pub const PINNED: Release = Release {
    version: "25.7.23",
    url: "https://github.com/VirtualDrivers/Virtual-Display-Driver/releases/download/25.7.23/VirtualDisplayDriver-ARM64.Driver.Only.zip",
    sha256: "dcd487161469c726bab60f174a7e7f70ee02afd437cad0d5c0e6755953de5a25",
    size: 129_547,
};

/// The hardware ID the driver's INF matches.
pub const HARDWARE_ID: &str = r"Root\MttVDD";

/// The INF inside the release zip.
pub const INF_PATH: &str = r"VirtualDisplayDriver\MttVDD.inf";

/// Where the driver looks for its settings folder (value `VDDPATH`).
pub const DRIVER_REG_KEY: &str = r"SOFTWARE\MikeTheTech\VirtualDisplayDriver";

/// The driver's settings folder when `VDDPATH` isn't set.
pub const DEFAULT_SETTINGS_DIR: &str = r"C:\VirtualDisplayDriver";

pub const SETTINGS_FILE: &str = "vdd_settings.xml";

/// A display mode, as the phone asks for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mode {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

impl Mode {
    /// Within what a phone could reasonably ask for. Everything else is refused before it
    /// reaches the driver's settings.
    pub fn sane(&self) -> bool {
        (320..=7680).contains(&self.width)
            && (240..=4320).contains(&self.height)
            && (24..=480).contains(&self.fps)
    }
}

impl std::fmt::Display for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}x{} at {} Hz", self.width, self.height, self.fps)
    }
}

/// The driver's settings folder: `VDDPATH` when something set it, else the driver's default.
pub fn settings_dir() -> PathBuf {
    configured_settings_dir().unwrap_or_else(|| PathBuf::from(DEFAULT_SETTINGS_DIR))
}

/// `VDDPATH`, when set.
pub fn configured_settings_dir() -> Option<PathBuf> {
    RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(DRIVER_REG_KEY, KEY_READ)
        .ok()?
        .get_value::<String, _>("VDDPATH")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
}

pub fn settings_file() -> PathBuf {
    settings_dir().join(SETTINGS_FILE)
}

/// Makes sure the settings file offers `mode`. Returns `true` when the file changed, which
/// means a running display has to be restarted to offer it.
pub fn ensure_mode(mode: Mode) -> std::io::Result<bool> {
    ensure_mode_in(&settings_file(), mode)
}

pub fn ensure_mode_in(path: &Path, mode: Mode) -> std::io::Result<bool> {
    let current = std::fs::read_to_string(path).ok();
    let updated = match current.as_deref().map(|text| with_mode(text, mode)) {
        Some(Edit::Unchanged) => return Ok(false),
        Some(Edit::Changed(t)) => t,
        // Missing, or unreadable to us: start over with ours rather than leave the phone
        // without its mode.
        Some(Edit::NoList) | None => template(mode),
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("xml.tmp");
    std::fs::write(&tmp, updated)?;
    std::fs::rename(&tmp, path)?;
    Ok(true)
}

/// The outcome of adding a mode to the settings text.
#[derive(Debug, PartialEq, Eq)]
pub enum Edit {
    /// The mode was already there.
    Unchanged,
    /// The new text.
    Changed(String),
    /// The text has no `<resolutions>` list to add to.
    NoList,
}

/// The settings text with `mode` added.
pub fn with_mode(xml: &str, mode: Mode) -> Edit {
    if resolutions(xml).contains(&mode) {
        return Edit::Unchanged;
    }
    let Some(end) = xml.find("</resolutions>") else { return Edit::NoList };
    // Indent like the line the closing tag is on.
    let line_start = xml[..end].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let indent: String = xml[line_start..end].chars().take_while(|c| c.is_whitespace()).collect();
    let entry = resolution_entry(mode, &format!("{indent}    "));
    let mut out = String::with_capacity(xml.len() + entry.len());
    out.push_str(&xml[..line_start]);
    out.push_str(&entry);
    out.push_str(&xml[line_start..]);
    Edit::Changed(out)
}

fn resolution_entry(mode: Mode, indent: &str) -> String {
    format!(
        "{indent}<resolution>\n{indent}    <width>{}</width>\n{indent}    <height>{}</height>\n{indent}    <refresh_rate>{}</refresh_rate>\n{indent}</resolution>\n",
        mode.width, mode.height, mode.fps
    )
}

/// Every `<resolution>` in the text, as modes (refresh rates rounded).
pub fn resolutions(xml: &str) -> Vec<Mode> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<resolution>") {
        let after = &rest[start + "<resolution>".len()..];
        let Some(end) = after.find("</resolution>") else { break };
        let block = &after[..end];
        let number = |tag: &str| -> Option<f32> {
            let open = format!("<{tag}>");
            let close = format!("</{tag}>");
            let s = block.find(&open)? + open.len();
            let e = block[s..].find(&close)? + s;
            block[s..e].trim().parse::<f32>().ok()
        };
        if let (Some(w), Some(h), Some(r)) = (number("width"), number("height"), number("refresh_rate")) {
            out.push(Mode { width: w as u32, height: h as u32, fps: r.round() as u32 });
        }
        rest = &after[end..];
    }
    out
}

/// A settings file of our own: one monitor, common modes, the phone's mode.
pub fn template(mode: Mode) -> String {
    let mut modes = vec![
        Mode { width: 1280, height: 720, fps: 60 },
        Mode { width: 1920, height: 1080, fps: 60 },
        Mode { width: 2560, height: 1440, fps: 60 },
    ];
    if !modes.contains(&mode) {
        modes.push(mode);
    }
    let entries: String = modes.iter().map(|m| resolution_entry(*m, "        ")).collect();
    format!(
        r#"<?xml version='1.0' encoding='utf-8'?>
<!-- Written by PocketDeck. It adds each phone's screen mode below before switching the display on. -->
<vdd_settings>
    <monitors>
        <count>1</count>
    </monitors>
    <gpu>
        <friendlyname>default</friendlyname>
    </gpu>
    <global>
        <g_refresh_rate>60</g_refresh_rate>
        <g_refresh_rate>90</g_refresh_rate>
        <g_refresh_rate>120</g_refresh_rate>
        <g_refresh_rate>144</g_refresh_rate>
    </global>
    <resolutions>
{entries}    </resolutions>
    <options>
        <CustomEdid>false</CustomEdid>
        <PreventSpoof>false</PreventSpoof>
        <EdidCeaOverride>false</EdidCeaOverride>
        <HardwareCursor>true</HardwareCursor>
        <SDR10bit>false</SDR10bit>
        <HDRPlus>false</HDRPlus>
        <logging>false</logging>
        <debuglogging>false</debuglogging>
    </options>
</vdd_settings>
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHIPPED: &str = "<vdd_settings>\n    <resolutions>\n        <resolution>\n            <width>1920</width>\n            <height>1080</height>\n            <refresh_rate>30</refresh_rate>\n        </resolution>\n    \n    </resolutions>\n</vdd_settings>";

    #[test]
    fn reads_resolutions() {
        assert_eq!(resolutions(SHIPPED), vec![Mode { width: 1920, height: 1080, fps: 30 }]);
        assert_eq!(resolutions("<resolution><width>800</width><height>600</height><refresh_rate>59.94</refresh_rate></resolution>")[0].fps, 60);
    }

    #[test]
    fn adds_a_missing_mode_once() {
        let phone = Mode { width: 2400, height: 1080, fps: 120 };
        let Edit::Changed(added) = with_mode(SHIPPED, phone) else { panic!("not added") };
        assert!(resolutions(&added).contains(&phone));
        assert_eq!(resolutions(&added).len(), 2);
        assert!(added.trim_end().ends_with("</vdd_settings>"));
        assert_eq!(with_mode(&added, phone), Edit::Unchanged);
    }

    #[test]
    fn refuses_text_without_a_list() {
        assert_eq!(with_mode("<vdd_settings/>", Mode { width: 1280, height: 720, fps: 60 }), Edit::NoList);
    }

    #[test]
    fn template_holds_the_mode() {
        let phone = Mode { width: 2340, height: 1080, fps: 90 };
        let t = template(phone);
        assert!(resolutions(&t).contains(&phone));
        assert_eq!(with_mode(&t, phone), Edit::Unchanged);
    }

    #[test]
    fn writes_the_file() {
        let dir = std::env::temp_dir().join(format!("pocketdeck-vdd-{}", std::process::id()));
        let path = dir.join(SETTINGS_FILE);
        let phone = Mode { width: 1920, height: 1080, fps: 120 };
        assert!(ensure_mode_in(&path, phone).unwrap());
        assert!(!ensure_mode_in(&path, phone).unwrap());
        let other = Mode { width: 1280, height: 800, fps: 60 };
        assert!(ensure_mode_in(&path, other).unwrap());
        let modes = resolutions(&std::fs::read_to_string(&path).unwrap());
        assert!(modes.contains(&phone) && modes.contains(&other));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn sanity_limits() {
        assert!(Mode { width: 2400, height: 1080, fps: 120 }.sane());
        assert!(!Mode { width: 0, height: 1080, fps: 60 }.sane());
        assert!(!Mode { width: 1920, height: 1080, fps: 1000 }.sane());
    }

    #[test]
    fn pinned_release_is_well_formed() {
        assert_eq!(PINNED.sha256.len(), 64);
        assert!(PINNED.url.starts_with("https://github.com/VirtualDrivers/Virtual-Display-Driver/releases/download/"));
        assert!(PINNED.url.contains(PINNED.version));
    }
}
