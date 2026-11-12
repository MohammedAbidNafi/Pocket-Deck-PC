//! Installing Sunshine when the PC doesn't have it: download one pinned, tested release from the
//! official GitHub releases, check its SHA-256, then run the MSI silently with one admin prompt.

use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub struct Release {
    pub version: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub size: u64,
}

/// The release PocketDeck installs. Bump only after testing a new one.
#[cfg(target_arch = "x86_64")]
pub const PINNED: Release = Release {
    version: "2026.914.233613",
    url: "https://github.com/SunshineStream/Sunshine/releases/download/v2026.914.233613/Sunshine-Windows-AMD64-installer.msi",
    sha256: "1d7fed8beecd5889dc7ff14cf9f42d6d38f37c3066c13c6c2a5f4e91847e0ccf",
    size: 33_710_080,
};

#[cfg(target_arch = "aarch64")]
pub const PINNED: Release = Release {
    version: "2026.914.233613",
    url: "https://github.com/SunshineStream/Sunshine/releases/download/v2026.914.233613/Sunshine-Windows-ARM64-installer.msi",
    sha256: "1e211404b924fcf421657c71e1aaa95cf1f76e172cacf8ff8430beacc0e30e4a",
    size: 24_541_615,
};

/// Progress of [`install`]: bytes downloaded so far and the total.
pub enum Progress {
    Downloading { done: u64, total: u64 },
    Installing,
}

/// Downloads (reusing a verified earlier download) and installs [`PINNED`].
pub fn install(download_dir: &Path, mut progress: impl FnMut(Progress)) -> crate::Result<()> {
    let msi = download(&PINNED, download_dir, &mut progress)?;
    progress(Progress::Installing);
    let log = download_dir.join("sunshine-install.log");
    let args = format!("/i \"{}\" /qn /norestart /l*v \"{}\"", msi.display(), log.display());
    match crate::elevate::run("msiexec.exe", &args)? {
        // 3010: success, a restart is needed to finish.
        0 | 3010 => Ok(()),
        code => Err(crate::Error::Failed(format!(
            "The Sunshine installer stopped with code {code}. Details are in {}.",
            log.display()
        ))),
    }
}

fn download(release: &Release, dir: &Path, progress: &mut impl FnMut(Progress)) -> crate::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let target = dir.join(format!("Sunshine-{}.msi", release.version));
    if target.is_file() && sha256_file(&target)? == release.sha256 {
        progress(Progress::Downloading { done: release.size, total: release.size });
        return Ok(target);
    }

    let partial = target.with_extension("msi.part");
    let mut response = ureq::get(release.url)
        .call()
        .map_err(|e| crate::Error::Download(e.to_string()))?;
    let total = response
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(release.size);
    let mut reader = response.body_mut().with_config().limit(release.size * 2).reader();
    let mut file = std::fs::File::create(&partial)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut done = 0u64;
    loop {
        let n = reader.read(&mut buf).map_err(|e| crate::Error::Download(e.to_string()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n])?;
        done += n as u64;
        progress(Progress::Downloading { done, total });
    }
    file.flush()?;
    drop(file);

    let actual = hex(&hasher.finalize());
    if actual != release.sha256 {
        let _ = std::fs::remove_file(&partial);
        return Err(crate::Error::Checksum { expected: release.sha256.into(), actual });
    }
    std::fs::rename(&partial, &target)?;
    Ok(target)
}

fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_files() {
        let path = std::env::temp_dir().join(format!("pocketdeck-sha-{}.bin", std::process::id()));
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(
            sha256_file(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn pinned_release_is_well_formed() {
        assert_eq!(PINNED.sha256.len(), 64);
        assert!(PINNED.url.starts_with("https://github.com/SunshineStream/Sunshine/releases/download/"));
        assert!(PINNED.url.contains(PINNED.version));
    }
}
