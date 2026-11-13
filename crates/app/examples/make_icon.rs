//! Regenerates `assets/icon.ico` and `assets/logo.png` from the mark drawn
//! in `src/brand_icon.rs`:
//!
//! `cargo run -p zest --example make_icon`

#[path = "../src/brand_icon.rs"]
#[allow(dead_code)]
mod brand_icon;

use image::codecs::ico::{IcoEncoder, IcoFrame};
use image::{ExtendedColorType, ImageEncoder};
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let assets = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets");
    std::fs::create_dir_all(&assets)?;

    let frames = [16u32, 20, 24, 32, 40, 48, 64, 128, 256]
        .iter()
        .map(|&size| {
            IcoFrame::as_png(
                &brand_icon::render(size),
                size,
                size,
                ExtendedColorType::Rgba8,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let ico = std::fs::File::create(assets.join("icon.ico"))?;
    IcoEncoder::new(ico).encode_images(&frames)?;

    let logo = std::fs::File::create(assets.join("logo.png"))?;
    image::codecs::png::PngEncoder::new(logo).write_image(
        &brand_icon::render(512),
        512,
        512,
        ExtendedColorType::Rgba8,
    )?;
    println!("wrote {}", assets.join("icon.ico").display());
    Ok(())
}
