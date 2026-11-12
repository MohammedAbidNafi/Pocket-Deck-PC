//! Developer tasks. `cargo run -p xtask -- icons` renders the logo into the Windows icon files.

use resvg::{tiny_skia, usvg};
use std::path::{Path, PathBuf};

/// Sizes Windows picks from: taskbar, tray, Explorer views, Alt-Tab, high-DPI screens.
const ICO_SIZES: [u32; 9] = [16, 20, 24, 32, 40, 48, 64, 128, 256];

fn main() {
    let task = std::env::args().nth(1).unwrap_or_default();
    match task.as_str() {
        "icons" => icons(),
        _ => {
            eprintln!("usage: cargo run -p xtask -- icons");
            std::process::exit(2);
        }
    }
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn render(svg: &Path, size: u32) -> tiny_skia::Pixmap {
    let data = std::fs::read(svg).unwrap_or_else(|e| panic!("{}: {e}", svg.display()));
    let tree = usvg::Tree::from_data(&data, &usvg::Options::default()).expect("valid SVG");
    let mut pixmap = tiny_skia::Pixmap::new(size, size).unwrap();
    let s = tree.size();
    let transform = tiny_skia::Transform::from_scale(size as f32 / s.width(), size as f32 / s.height());
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    pixmap
}

fn icons() {
    let root = root();
    let tile = root.join("app/ui/assets/logo-tile.svg");
    let out = root.join("app/assets");
    std::fs::create_dir_all(&out).unwrap();

    let mut dir = ico::IconDir::new(ico::ResourceType::Icon);
    for size in ICO_SIZES {
        let png = render(&tile, size).encode_png().unwrap();
        let image = ico::IconImage::read_png(std::io::Cursor::new(png)).unwrap();
        dir.add_entry(ico::IconDirEntry::encode(&image).unwrap());
    }
    let ico_path = out.join("app.ico");
    dir.write(std::fs::File::create(&ico_path).unwrap()).unwrap();

    let png_path = out.join("logo-512.png");
    std::fs::write(&png_path, render(&tile, 512).encode_png().unwrap()).unwrap();

    println!("wrote {} and {}", ico_path.display(), png_path.display());
}
