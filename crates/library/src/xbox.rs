//! Xbox app and Microsoft Store games.
//!
//! Two places name them, and both are read:
//! - Gaming Services keeps the packages the Xbox app manages in
//!   `HKLM\SOFTWARE\Microsoft\GamingServices\PackageRepository\Package` (value names are package
//!   full names). This also covers older Store (UWP) games such as Solitaire.
//! - Every PC Game Pass / Xbox title (MSIXVC) carries a `MicrosoftGame.config` at its package
//!   root, so any installed package with one is a game too.
//!
//! Where a package lives, and its app ids, come from the per-user AppModel repository in the
//! registry. Games are started through the shell (`shell:AppsFolder\<family>!<app>`), the same
//! way the Start menu does, which is the only supported way to start a packaged app.

use crate::{Game, Launch, Scan, Source, clean_name};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use winreg::RegKey;
use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ};

const GAMING_SERVICES: &str = r"SOFTWARE\Microsoft\GamingServices";
const APPMODEL_PACKAGES: &str =
    r"Software\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\AppModel\Repository\Packages";

pub fn scan() -> Scan {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let gaming = hklm.open_subkey_with_flags(GAMING_SERVICES, KEY_READ).ok();
    let launcher_found = gaming.is_some();

    // Packages the Xbox app manages.
    let mut managed: BTreeSet<String> = BTreeSet::new();
    if let Some(list) = gaming.as_ref().and_then(|g| g.open_subkey_with_flags(r"PackageRepository\Package", KEY_READ).ok()) {
        managed.extend(list.enum_values().flatten().map(|(name, _)| name));
    }

    let Ok(packages) = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(APPMODEL_PACKAGES, KEY_READ) else {
        return Scan { games: Vec::new(), launcher_found };
    };
    let mut games: Vec<Game> = Vec::new();
    for full_name in packages.enum_keys().flatten() {
        let Ok(key) = packages.open_subkey_with_flags(&full_name, KEY_READ) else { continue };
        let Ok(root) = key.get_value::<String, _>("PackageRootFolder") else { continue };
        let root = PathBuf::from(root);
        let is_managed = managed.iter().any(|m| m.eq_ignore_ascii_case(&full_name));
        if !is_managed && !root.join("MicrosoftGame.config").is_file() {
            continue;
        }
        let registry_name: Option<String> = key.get_value("DisplayName").ok();
        if let Some(game) = read_package(&full_name, &root, registry_name.as_deref()) {
            // One entry per package family (several versions can be registered at once).
            if !games.iter().any(|g| g.key == game.key) {
                games.push(game);
            }
        }
    }
    games.sort_by_key(|g| g.name.to_lowercase());
    let launcher_found = launcher_found || !games.is_empty();
    Scan { games, launcher_found }
}

/// `Name_Version_Arch_ResourceId_PublisherId` → `Name_PublisherId`.
pub fn family_name(full_name: &str) -> Option<String> {
    let parts: Vec<&str> = full_name.split('_').collect();
    if parts.len() != 5 || parts[0].is_empty() || parts[4].is_empty() {
        return None;
    }
    Some(format!("{}_{}", parts[0], parts[4]))
}

/// What a package's manifests say about it.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Manifest {
    pub identity: String,
    pub display_name: Option<String>,
    pub app_id: Option<String>,
    /// Logos, best first, relative to the package root.
    pub logos: Vec<String>,
    pub framework: bool,
}

/// Reads `AppxManifest.xml`.
pub fn parse_appx(xml: &str) -> Option<Manifest> {
    let doc = roxmltree::Document::parse(xml).ok()?;
    let find = |name: &str| doc.descendants().find(|n| n.is_element() && n.tag_name().name() == name);
    let mut m = Manifest { identity: find("Identity")?.attribute("Name")?.to_string(), ..Default::default() };
    if let Some(app) = find("Application") {
        m.app_id = app.attribute("Id").map(str::to_string);
        if let Some(visuals) = app.children().find(|n| n.tag_name().name() == "VisualElements") {
            // Bigger art first.
            for attr in ["Square310x310Logo", "Square150x150Logo", "Wide310x150Logo", "Square44x44Logo"] {
                m.logos.extend(visuals.attribute(attr).map(str::to_string));
            }
            m.display_name = visuals.attribute("DisplayName").map(str::to_string);
        }
    }
    if let Some(props) = find("Properties") {
        let child = |name: &str| {
            props
                .children()
                .find(|n| n.tag_name().name() == name)
                .and_then(|n| n.text())
                .map(|t| t.trim().to_string())
        };
        // The package's name reads better than the tile's, which is often shortened.
        if let Some(name) = child("DisplayName").filter(|s| !s.is_empty()) {
            m.display_name = Some(name);
        }
        m.framework = child("Framework").is_some_and(|f| f.eq_ignore_ascii_case("true"));
        m.logos.extend(child("Logo"));
    }
    Some(m)
}

/// Reads `MicrosoftGame.config` (PC Game Pass / Xbox titles), which has plain display names and
/// larger logos than the AppX manifest.
pub fn parse_game_config(xml: &str) -> Option<Manifest> {
    let doc = roxmltree::Document::parse(xml).ok()?;
    let find = |name: &str| doc.descendants().find(|n| n.is_element() && n.tag_name().name() == name);
    let mut m = Manifest { identity: find("Identity")?.attribute("Name")?.to_string(), ..Default::default() };
    if let Some(visuals) = find("ShellVisuals") {
        m.display_name = visuals.attribute("DefaultDisplayName").map(str::to_string);
        for attr in ["Square480x480Logo", "Square150x150Logo", "StoreLogo", "Square44x44Logo"] {
            if let Some(logo) = visuals.attribute(attr) {
                m.logos.push(logo.to_string());
            }
        }
    }
    m.app_id = doc
        .descendants()
        .find(|n| n.is_element() && n.tag_name().name() == "Executable" && n.attribute("Id").is_some())
        .and_then(|n| n.attribute("Id"))
        .map(str::to_string);
    Some(m)
}

fn read_package(full_name: &str, root: &Path, registry_name: Option<&str>) -> Option<Game> {
    let family = family_name(full_name)?;
    let appx = std::fs::read_to_string(root.join("AppxManifest.xml")).ok().and_then(|x| parse_appx(&x));
    let config = std::fs::read_to_string(root.join("MicrosoftGame.config")).ok().and_then(|x| parse_game_config(&x));
    if appx.as_ref().is_some_and(|a| a.framework) {
        return None;
    }
    let identity = appx.as_ref().or(config.as_ref()).map(|m| m.identity.clone()).unwrap_or_default();
    let app_id = appx.as_ref().and_then(|m| m.app_id.clone()).or_else(|| config.as_ref().and_then(|m| m.app_id.clone()))?;

    let name = [
        config.as_ref().and_then(|m| m.display_name.clone()),
        appx.as_ref().and_then(|m| m.display_name.clone()),
        registry_name.map(str::to_string),
    ]
    .into_iter()
    .flatten()
    .find_map(|n| resolve_name(&n, full_name, &identity))?;

    let logos = config.iter().chain(appx.iter()).flat_map(|m| m.logos.iter());
    let art = logos.filter_map(|l| find_logo(root, l)).next();

    Some(Game {
        key: format!("xbox:{family}"),
        source: Source::Xbox,
        name: clean_name(&name),
        launch: Launch::Detached { uri: format!(r"explorer.exe shell:AppsFolder\{family}!{app_id}") },
        art,
        art_url: None,
    })
}

/// A plain name as-is; an `ms-resource:` name looked up in the package's resources.
fn resolve_name(name: &str, full_name: &str, identity: &str) -> Option<String> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let Some(rest) = name.strip_prefix("ms-resource:") else { return Some(name.to_string()) };
    let uri = if rest.starts_with("//") {
        format!("ms-resource:{rest}")
    } else if let Some(abs) = rest.strip_prefix('/') {
        format!("ms-resource://{identity}/{abs}")
    } else if rest.contains('/') {
        format!("ms-resource://{identity}/{rest}")
    } else {
        format!("ms-resource://{identity}/resources/{rest}")
    };
    load_indirect(&format!("@{{{full_name}?{uri}}}")).filter(|s| !s.trim().is_empty() && !s.starts_with("ms-resource:"))
}

fn load_indirect(source: &str) -> Option<String> {
    use windows_sys::Win32::UI::Shell::SHLoadIndirectString;
    let wide: Vec<u16> = source.encode_utf16().chain(std::iter::once(0)).collect();
    let mut buf = [0u16; 512];
    // SAFETY: `wide` is NUL-terminated and `buf` is a writable buffer of the length passed.
    let hr = unsafe { SHLoadIndirectString(wide.as_ptr(), buf.as_mut_ptr(), buf.len() as u32, std::ptr::null_mut()) };
    if hr < 0 {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

/// A logo path from a manifest. Packaged apps store scaled variants (`StoreLogo.scale-200.png`,
/// `Logo.targetsize-256.png`) in place of the named file, so the biggest variant wins.
fn find_logo(root: &Path, relative: &str) -> Option<PathBuf> {
    let path = root.join(relative.replace('/', "\\"));
    let dir = path.parent()?;
    let stem = path.file_stem()?.to_string_lossy().to_lowercase();
    let ext = path.extension()?.to_string_lossy().to_lowercase();
    let mut best: Option<(u32, PathBuf)> = None;
    let mut consider = |p: PathBuf| {
        if let Ok((w, h)) = image::image_dimensions(&p) {
            let size = w.min(h);
            if best.as_ref().is_none_or(|(s, _)| size > *s) {
                best = Some((size, p));
            }
        }
    };
    if path.is_file() {
        consider(path.clone());
    }
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().to_lowercase();
        if name.starts_with(&format!("{stem}.")) && name.ends_with(&format!(".{ext}")) && !name.contains("contrast") {
            consider(entry.path());
        }
    }
    // Tiny icons make poor covers.
    best.filter(|(size, _)| *size >= 100).map(|(_, p)| p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_names() {
        assert_eq!(
            family_name("Microsoft.MicrosoftSolitaireCollection_4.27.9181.0_x64__8wekyb3d8bbwe").as_deref(),
            Some("Microsoft.MicrosoftSolitaireCollection_8wekyb3d8bbwe")
        );
        assert_eq!(family_name("nonsense"), None);
    }

    #[test]
    fn reads_appx_manifest() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<Package xmlns="http://schemas.microsoft.com/appx/manifest/foundation/windows10" xmlns:uap="http://schemas.microsoft.com/appx/manifest/uap/windows10">
  <Identity Name="Microsoft.MicrosoftSolitaireCollection" Publisher="CN=Microsoft" Version="4.27.9181.0" />
  <Properties>
    <DisplayName>Solitaire &amp; Casual Games</DisplayName>
    <Logo>Assets\StoreLogo.png</Logo>
  </Properties>
  <Applications>
    <Application Id="App" Executable="Solitaire.exe">
      <uap:VisualElements DisplayName="ms-resource:AppName" Square150x150Logo="Assets\Medium.png" Square44x44Logo="Assets\Small.png" />
    </Application>
  </Applications>
</Package>"#;
        let m = parse_appx(xml).unwrap();
        assert_eq!(m.identity, "Microsoft.MicrosoftSolitaireCollection");
        assert_eq!(m.display_name.as_deref(), Some("Solitaire & Casual Games"));
        assert_eq!(m.app_id.as_deref(), Some("App"));
        assert_eq!(m.logos[0], r"Assets\Medium.png");
        assert!(!m.framework);
    }

    #[test]
    fn reads_game_config() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<Game configVersion="1">
  <Identity Name="Microsoft.624F8B84B80" Publisher="CN=Microsoft" Version="1.0.0.0" />
  <ExecutableList>
    <Executable Name="ForzaHorizon5.exe" TargetDeviceFamily="PC" Id="Game" />
  </ExecutableList>
  <ShellVisuals DefaultDisplayName="Forza Horizon 5" PublisherDisplayName="Microsoft Studios"
    StoreLogo="StoreLogo.png" Square150x150Logo="Logo.png" Square44x44Logo="SmallLogo.png"
    Square480x480Logo="Logo480.png" />
</Game>"#;
        let m = parse_game_config(xml).unwrap();
        assert_eq!(m.display_name.as_deref(), Some("Forza Horizon 5"));
        assert_eq!(m.app_id.as_deref(), Some("Game"));
        assert_eq!(m.logos[0], "Logo480.png");
    }
}
