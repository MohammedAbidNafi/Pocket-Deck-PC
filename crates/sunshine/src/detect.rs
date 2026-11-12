//! Finding an existing Sunshine install.
//!
//! Checked in order: the installer's uninstall entry, the Windows service's program path, and
//! the default install folder. Each candidate must actually contain `sunshine.exe`.

use std::path::{Component, Path, PathBuf};
use winreg::RegKey;
use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Install {
    /// Folder holding `sunshine.exe`.
    pub dir: PathBuf,
    /// Version from the uninstall entry, when there is one.
    pub version: Option<String>,
}

impl Install {
    pub fn exe(&self) -> PathBuf {
        self.dir.join("sunshine.exe")
    }

    pub fn config_dir(&self) -> PathBuf {
        self.dir.join("config")
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir().join("sunshine.conf")
    }

    pub fn log_file(&self) -> PathBuf {
        self.config_dir().join("sunshine.log")
    }
}

const UNINSTALL: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall";

pub fn find() -> Option<Install> {
    let entry = uninstall_entry();
    let version = entry.as_ref().and_then(|e| e.1.clone());

    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some((Some(location), _)) = &entry {
        candidates.push(location.clone());
    }
    if let Some(dir) = service_dir() {
        candidates.push(dir);
    }
    for var in ["ProgramFiles", "ProgramW6432"] {
        if let Some(pf) = std::env::var_os(var) {
            candidates.push(PathBuf::from(pf).join("Sunshine"));
        }
    }

    candidates
        .into_iter()
        .map(|d| normalize(&d))
        .find(|d| d.join("sunshine.exe").is_file())
        .map(|dir| Install { dir, version })
}

/// `(InstallLocation, DisplayVersion)` from the "Sunshine" uninstall entry.
fn uninstall_entry() -> Option<(Option<PathBuf>, Option<String>)> {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    for view in [KEY_WOW64_64KEY, KEY_WOW64_32KEY] {
        let Ok(root) = hklm.open_subkey_with_flags(UNINSTALL, KEY_READ | view) else { continue };
        for name in root.enum_keys().flatten() {
            let Ok(key) = root.open_subkey_with_flags(&name, KEY_READ | view) else { continue };
            let display: String = key.get_value("DisplayName").unwrap_or_default();
            if display.trim() != "Sunshine" {
                continue;
            }
            let location = key
                .get_value::<String, _>("InstallLocation")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .map(PathBuf::from);
            let version = key
                .get_value::<String, _>("DisplayVersion")
                .ok()
                .filter(|s| !s.trim().is_empty());
            return Some((location, version));
        }
    }
    None
}

/// Install folder derived from the service's program path (`<dir>\tools\sunshinesvc.exe`).
fn service_dir() -> Option<PathBuf> {
    let key = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(format!(r"SYSTEM\CurrentControlSet\Services\{}", crate::SERVICE_NAME), KEY_READ)
        .ok()?;
    let image: String = key.get_value("ImagePath").ok()?;
    let exe = PathBuf::from(executable_from_command(&image));
    let parent = exe.parent()?;
    let is_tools = parent.file_name().is_some_and(|n| n.eq_ignore_ascii_case("tools"));
    Some(if is_tools { parent.parent()?.to_path_buf() } else { parent.to_path_buf() })
}

/// The program part of a service command line: quoted, or up to the first space.
fn executable_from_command(cmd: &str) -> &str {
    let cmd = cmd.trim();
    if let Some(rest) = cmd.strip_prefix('"') {
        rest.split('"').next().unwrap_or(rest)
    } else {
        cmd.split(' ').next().unwrap_or(cmd)
    }
}

/// Collapses doubled separators (the installer writes `D:\\tools\...`).
fn normalize(p: &Path) -> PathBuf {
    p.components().filter(|c| !matches!(c, Component::CurDir)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_service_command_lines() {
        assert_eq!(executable_from_command(r#""D:\\tools\sunshinesvc.exe""#), r"D:\\tools\sunshinesvc.exe");
        assert_eq!(
            executable_from_command(r#""C:\Program Files\Sunshine\tools\sunshinesvc.exe" --flag"#),
            r"C:\Program Files\Sunshine\tools\sunshinesvc.exe"
        );
        assert_eq!(executable_from_command(r"C:\Sunshine\tools\sunshinesvc.exe -x"), r"C:\Sunshine\tools\sunshinesvc.exe");
    }

    #[test]
    fn normalizes_doubled_separators() {
        assert_eq!(normalize(Path::new(r"D:\\tools\sunshinesvc.exe")), PathBuf::from(r"D:\tools\sunshinesvc.exe"));
        let exe = normalize(Path::new(r"D:\\tools\sunshinesvc.exe"));
        assert_eq!(exe.parent().unwrap().parent().unwrap(), Path::new(r"D:\"));
    }
}
