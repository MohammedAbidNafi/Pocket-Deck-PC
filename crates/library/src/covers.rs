//! Box art for Sunshine (which only accepts PNG) and small thumbnails for PocketDeck's own grid.
//!
//! Sunshine derives each app's ID from its name plus a hash of the cover file, and the phone
//! keys its library entries (favourites, play time) on that ID. So a cover, once written, is
//! never rewritten: the same game keeps the same bytes, and the same ID, from then on.

use image::imageops::FilterType;
use std::path::{Path, PathBuf};

/// Width of the cover handed to Sunshine (Steam's portrait art is 600x900).
const COVER_WIDTH: u32 = 600;
/// Width of the thumbnails PocketDeck shows in its own grid (kept small to save memory).
pub const THUMB_WIDTH: u32 = 180;

fn file_name(key: &str) -> String {
    let safe: String = key.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' }).collect();
    format!("{safe}.png")
}

pub fn cover_path(dir: &Path, key: &str) -> PathBuf {
    dir.join("covers").join(file_name(key))
}

pub fn thumb_path(dir: &Path, key: &str) -> PathBuf {
    dir.join("thumbs").join(file_name(key))
}

/// Writes the Sunshine cover and the thumbnail for `key` from `art`, unless they already exist.
/// Returns the cover path when there is one.
pub fn prepare(dir: &Path, key: &str, art: &Path) -> Option<PathBuf> {
    let cover = cover_path(dir, key);
    let thumb = thumb_path(dir, key);
    if cover.is_file() && thumb.is_file() {
        return Some(cover);
    }
    let mut img = image::open(art).ok()?;
    // Wide art (a store header) becomes a portrait cover: the art centred on a blurred,
    // darkened copy of itself.
    if img.width() * 3 > img.height() * 2 + img.width() / 10 {
        img = portrait_from_wide(&img);
    }
    if !cover.is_file() {
        let sized = if img.width() > COVER_WIDTH {
            img.resize(COVER_WIDTH, u32::MAX, FilterType::Lanczos3)
        } else {
            img.clone()
        };
        write_png(&sized, &cover)?;
    }
    if !thumb.is_file() {
        write_png(&img.resize(THUMB_WIDTH, u32::MAX, FilterType::Triangle), &thumb)?;
    }
    Some(cover)
}

/// Downloads art known only by URL, once, into `<dir>/art/`. Only Epic's and SteamGridDB's
/// image servers are allowed. Returns the local file (with an extension matching its real format).
pub fn fetch(dir: &Path, key: &str, url: &str) -> Option<PathBuf> {
    let host = url.strip_prefix("https://")?.split('/').next()?;
    let allowed = ["epicgames.com", "steamgriddb.com"];
    if !allowed.iter().any(|a| host == *a || host.ends_with(&format!(".{a}"))) {
        return None;
    }
    let base = dir.join("art").join(file_name(key)).with_extension("");
    for ext in ["jpg", "png", "webp"] {
        let existing = base.with_extension(ext);
        if existing.is_file() {
            return Some(existing);
        }
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(20)))
        .build()
        .into();
    let mut res = agent.get(url).call().ok()?;
    let bytes = res.body_mut().with_config().limit(25 * 1024 * 1024).read_to_vec().ok()?;
    let ext = match bytes.get(..4)? {
        [0x89, b'P', b'N', b'G'] => "png",
        [0xFF, 0xD8, ..] => "jpg",
        [b'R', b'I', b'F', b'F'] => "webp",
        _ => return None,
    };
    let path = base.with_extension(ext);
    std::fs::create_dir_all(path.parent()?).ok()?;
    let tmp = path.with_extension("part");
    std::fs::write(&tmp, &bytes).ok()?;
    std::fs::rename(&tmp, &path).ok()?;
    Some(path)
}

/// 600x900 portrait built from wide art.
fn portrait_from_wide(wide: &image::DynamicImage) -> image::DynamicImage {
    let (w, h) = (COVER_WIDTH, COVER_WIDTH * 3 / 2);
    // Background: the art scaled to fill, blurred and darkened.
    let fill = wide.resize_to_fill(w / 4, h / 4, FilterType::Triangle).blur(6.0).brighten(-60);
    let mut canvas = fill.resize_exact(w, h, FilterType::Triangle).to_rgb8();
    // Foreground: the art at full width, centred.
    let front = wide.resize(w, h, FilterType::Lanczos3).to_rgb8();
    let y = (h - front.height()) / 2;
    image::imageops::overlay(&mut canvas, &front, 0, y as i64);
    image::DynamicImage::ImageRgb8(canvas)
}

fn write_png(img: &image::DynamicImage, path: &Path) -> Option<()> {
    std::fs::create_dir_all(path.parent()?).ok()?;
    let tmp = path.with_extension("tmp");
    img.to_rgb8().save_with_format(&tmp, image::ImageFormat::Png).ok()?;
    std::fs::rename(&tmp, path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_once_and_keeps_bytes() {
        let dir = std::env::temp_dir().join(format!("pocketdeck-covers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("art.png");
        image::RgbImage::from_fn(900, 1350, |x, y| image::Rgb([(x % 256) as u8, (y % 256) as u8, 90])).save(&src).unwrap();

        let cover = prepare(&dir, "steam:42", &src).unwrap();
        assert_eq!(cover, dir.join("covers").join("steam-42.png"));
        let (w, h) = image::image_dimensions(&cover).unwrap();
        assert_eq!((w, h), (600, 900));
        assert_eq!(image::image_dimensions(thumb_path(&dir, "steam:42")).unwrap().0, THUMB_WIDTH);

        // A changed source must not change an existing cover (its hash is the Sunshine app ID).
        let before = std::fs::read(&cover).unwrap();
        image::RgbImage::new(900, 1350).save(&src).unwrap();
        prepare(&dir, "steam:42", &src).unwrap();
        assert_eq!(std::fs::read(&cover).unwrap(), before);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
