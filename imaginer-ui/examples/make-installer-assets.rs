//! Generate the installer's image assets from the SVG sources of truth.
//!
//! NSIS needs three binaries this repo does not commit: the `.ico` on the
//! installer and installed program, a 150x57 header strip and a 164x314
//! welcome-panel image. All three are drawn here from `assets/` through
//! `imaginer_core::Svg` and `export::write` — the same code path a user's own
//! files take — so editing the artwork and re-running this example refreshes
//! everything.
//!
//! Run: `cargo run --release -p imaginer-ui --example make-installer-assets`

use imaginer_core::{ExportSettings, Format, Svg};
use imaginer_core::image::{ImageFormat, Rgba, RgbaImage};

/// Repo layout the script is run from: `imaginer-ui/examples`.
fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("imaginer-ui has a parent")
        .to_path_buf()
}

/// The app's own palette (`imaginer-ui/src/theme.rs`), restated here so the
/// installer matches the chrome without imaginer-ui's eframe dependency.
const BG_DEEPEST: [u8; 3] = [0x0e, 0x0f, 0x11];
const BG_PANEL: [u8; 3] = [0x16, 0x18, 0x1c];

fn main() {
    let root = repo_root();
    let out = root.join("packaging");
    std::fs::create_dir_all(&out).expect("packaging/ can be created");

    let icon = parse(&root.join("assets/imaginer_logoicon.svg"));
    let logotype = parse(&root.join("assets/imaginer_logotype.svg"));

    let icon_art = icon.render_longest(512).expect("icon rasterises");
    let wordmark = logotype.render_longest(512).expect("logotype rasterises");

    // The icon: rendered well past the ladder's top rung so every size in the
    // .ico is a clean downsample. `export::write` builds the whole ladder, PNG
    // at 256 and raw BMP below, exactly as the executable's own resource does.
    let square = squarify(&icon_art);
    imaginer_core::export::write(
        &square,
        &out.join("imaginer.ico"),
        &ExportSettings {
            format: Format::Ico,
            ..Default::default()
        },
    )
    .expect("writing imaginer.ico failed");

    // Header strip: icon and wordmark on the panel colour, the same pair the
    // app's toolbar shows.
    let mut header = solid(150, 57, BG_PANEL);
    let mark = fit_square(&icon_art, 44);
    paste(&mut header, &mark, 6, 6);
    let word = fit_height(&wordmark, 20);
    paste(&mut header, &word, 58, 57 as i64 - word.height() as i64 / 2);
    save_bmp(&header, &out.join("header.bmp"));

    // Welcome panel: the artwork large on the deepest background, wordmark
    // beneath — what NSIS scales to fill the left column of the wizard.
    let mut sidebar = solid(164, 314, BG_DEEPEST);
    let big = fit_square(&icon_art, 104);
    paste(&mut sidebar, &big, 164 as i64 - big.width() as i64 / 2, 72);
    let word = fit_height(&wordmark, 44);
    paste(
        &mut sidebar,
        &word,
        164 as i64 - word.width() as i64 / 2,
        200,
    );
    save_bmp(&sidebar, &out.join("sidebar.bmp"));

    println!("wrote {}", out.display());
}

fn parse(path: &std::path::Path) -> Svg {
    let data = std::fs::read(path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
    Svg::parse(&data).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

/// Centre the artwork on a transparent square, so the icon's ladder entries
/// are square even though the source is only assumed to be roughly so.
fn squarify(art: &RgbaImage) -> RgbaImage {
    let side = art.width().max(art.height());
    let mut out = RgbaImage::new(side, side);
    let x = (side - art.width()) / 2;
    let y = (side - art.height()) / 2;
    paste(&mut out, art, x as i64, y as i64);
    out
}

/// Nearest-neighbour fit inside a square, preserving aspect with transparent
/// margins. Downsampling by integer-ish factors is fine at these sizes.
fn fit_square(art: &RgbaImage, side: u32) -> RgbaImage {
    let scale = side as f32 / art.width().max(art.height()) as f32;
    let w = ((art.width() as f32 * scale).round() as u32).max(1);
    let h = ((art.height() as f32 * scale).round() as u32).max(1);
    squarify(&resize(art, w, h))
}

fn fit_height(art: &RgbaImage, height: u32) -> RgbaImage {
    let scale = height as f32 / art.height() as f32;
    let w = ((art.width() as f32 * scale).round() as u32).max(1);
    resize(art, w, height)
}

fn resize(art: &RgbaImage, w: u32, h: u32) -> RgbaImage {
    imaginer_core::image::imageops::resize(art, w, h, imaginer_core::image::imageops::FilterType::Lanczos3)
}

fn solid(width: u32, height: u32, rgb: [u8; 3]) -> RgbaImage {
    RgbaImage::from_pixel(width, height, Rgba([rgb[0], rgb[1], rgb[2], 255]))
}

/// Alpha-composite `src` onto `dst`, clipping whatever falls outside.
fn paste(dst: &mut RgbaImage, src: &RgbaImage, x: i64, y: i64) {
    for sy in 0..src.height() as i64 {
        for sx in 0..src.width() as i64 {
            let (dx, dy) = (x + sx, y + sy);
            if dx < 0 || dy < 0 || dx >= dst.width() as i64 || dy >= dst.height() as i64 {
                continue;
            }
            let a = src.get_pixel(sx as u32, sy as u32).0[3] as f32 / 255.0;
            if a == 0.0 {
                continue;
            }
            let d = dst.get_pixel_mut(dx as u32, dy as u32);
            for c in 0..3 {
                d.0[c] = (src.get_pixel(sx as u32, sy as u32).0[c] as f32 * a
                    + d.0[c] as f32 * (1.0 - a))
                    .round() as u8;
            }
            d.0[3] = 255;
        }
    }
}

fn save_bmp(pixels: &RgbaImage, path: &std::path::Path) {
    // BMP carries no alpha; these panels are opaque backgrounds anyway.
    imaginer_core::image::DynamicImage::ImageRgba8(pixels.clone())
        .to_rgb8()
        .save_with_format(path, ImageFormat::Bmp)
        .unwrap_or_else(|err| panic!("{}: {err}", path.display()));
}
