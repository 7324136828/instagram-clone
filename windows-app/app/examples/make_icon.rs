//! Generates the application icon: `assets/app.ico` (16, 32, 48 and 256 px, embedded into the
//! executable by `build.rs`) and `assets/icon.png` (256 px, the window icon).
//!
//! The glyph is the favicon of the reference web project (`index.html:11`): a rounded square, a
//! circle and a dot in the brand pink, on a transparent background. It is rasterised with the
//! same SVG renderer as the icons of the app. Run it again only when the glyph changes:
//!
//! ```text
//! cargo run -p instagram-windows-app --example make_icon
//! ```

use std::path::{Path, PathBuf};

use image::ImageEncoder as _;

/// The favicon, verbatim.
const FAVICON_SVG: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 24 24\" width=\"24\" height=\"24\" fill=\"none\" stroke=\"#e1306c\" stroke-width=\"2\">\
<rect x=\"2\" y=\"2\" width=\"20\" height=\"20\" rx=\"6\"/><circle cx=\"12\" cy=\"12\" r=\"4.5\"/><circle cx=\"17.5\" cy=\"6.5\" r=\"1.4\" fill=\"#e1306c\" stroke=\"none\"/></svg>";

/// The sizes of the .ico file, in pixels.
const ICON_SIZES: [u32; 4] = [16, 32, 48, 256];

/// Rasterises the glyph at `side` pixels and encodes it as a PNG with straight alpha.
fn png_of_size(side: u32) -> Result<Vec<u8>, String> {
    let hint = egui::load::SizeHint::Size { width: side, height: side, maintain_aspect_ratio: true };
    let image = egui_extras::image::load_svg_bytes_with_size(FAVICON_SVG.as_bytes(), hint, &Default::default())?;
    let mut rgba = Vec::with_capacity(image.pixels.len() * 4);
    for pixel in &image.pixels {
        rgba.extend_from_slice(&pixel.to_srgba_unmultiplied());
    }
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png).write_image(&rgba, side, side, image::ExtendedColorType::Rgba8).map_err(|error| error.to_string())?;
    Ok(png)
}

/// Builds an .ico container whose entries are PNG images (understood since Windows Vista).
fn ico_of(images: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut ico = Vec::new();
    ico.extend_from_slice(&0u16.to_le_bytes());
    ico.extend_from_slice(&1u16.to_le_bytes());
    ico.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len() as u32;
    for (side, png) in images {
        // A side of 256 is written as 0.
        let side_byte = if *side >= 256 { 0 } else { *side as u8 };
        ico.extend_from_slice(&[side_byte, side_byte, 0, 0]);
        ico.extend_from_slice(&1u16.to_le_bytes());
        ico.extend_from_slice(&32u16.to_le_bytes());
        ico.extend_from_slice(&(png.len() as u32).to_le_bytes());
        ico.extend_from_slice(&offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for (_, png) in images {
        ico.extend_from_slice(png);
    }
    ico
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(path, bytes).map_err(|error| format!("{} could not be written: {error}", path.display()))?;
    println!("wrote {} ({} bytes)", path.display(), bytes.len());
    Ok(())
}

fn run() -> Result<(), String> {
    let assets = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets");
    std::fs::create_dir_all(&assets).map_err(|error| format!("{} could not be created: {error}", assets.display()))?;
    let mut images = Vec::new();
    for side in ICON_SIZES {
        images.push((side, png_of_size(side)?));
    }
    write(&assets.join("app.ico"), &ico_of(&images))?;
    write(&assets.join("icon.png"), &png_of_size(256)?)
}

fn main() {
    if let Err(error) = run() {
        eprintln!("[make_icon] ERROR: {error}");
        std::process::exit(1);
    }
}
