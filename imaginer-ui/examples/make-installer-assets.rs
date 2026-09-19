//! Generate the installer's image assets from the SVG sources of truth.
//!
//! NSIS needs three binaries this repo does not commit: the `.ico` on the
//! installer and installed program, a header strip and a welcome-panel image.
//! All three are drawn here from `assets/` through `imaginer_core::Svg` — the
//! same code path a user's own files take — so editing the artwork and
//! re-running this example refreshes everything.
//!
//! Run: `cargo run --release -p imaginer-ui --example make-installer-assets`
//!
//! Two things about the panels that are easy to get wrong, and were:
//!
//! **They are stretched, so they must be drawn big.** MUI2 defaults both
//! bitmaps to `FitControl` — the image is scaled to whatever the control
//! measures — and the control is sized in dialog units, which grow with the
//! display's DPI. At 150% a 150x57 bitmap is blown up to 225x86 by the nearest
//! thing to a nearest-neighbour blit, which is exactly as bad as it sounds. So
//! everything here is drawn at [`SCALE`] times the recommended size, in the
//! recommended *proportion*: stretching then lands somewhere between a mild
//! downsample and no resampling at all.
//!
//! **Fitting is a box, not a scale.** The version of this file that shipped
//! computed `canvas - art / 2` where it meant `(canvas - art) / 2`, in three
//! places, which put the artwork half off the right edge of the panel and the
//! wordmark half off the bottom of the header. Nothing here multiplies a
//! measurement by hand any more: [`fit`] renders the vector into a box and
//! [`centre_in`] places it.

use imaginer_core::image::{ImageFormat, Rgba, RgbaImage};
use imaginer_core::{ExportSettings, Format, Svg};

/// How many times the recommended size everything is drawn at.
///
/// Three covers a 300% display, which is past anything Windows offers as a
/// standard scale. The cost is a megabyte or so of bitmap before compression,
/// and these are flat colour behind line art — LZMA eats them.
const SCALE: u32 = 3;

/// Header strip, in the size MUI2 documents. Drawn at [`SCALE`] times this.
const HEADER: (u32, u32) = (150, 57);
/// Welcome and finish panel, likewise.
const SIDEBAR: (u32, u32) = (164, 314);

/// The app's own palette (`imaginer-ui/src/theme.rs`), restated here so the
/// installer matches the chrome without imaginer-ui's eframe dependency.
const BG_DEEPEST: [u8; 3] = [0x0e, 0x0f, 0x11];

/// What the wizard's header actually is behind the bitmap.
///
/// White, because that is the colour Windows paints the strip the bitmap sits
/// in. The panel colour from the app's theme was used here once and it read as
/// a black slab dropped into a white band — the one place in this installer
/// where matching the app's chrome is the wrong instinct.
const HEADER_BG: [u8; 3] = [0xff, 0xff, 0xff];

/// Repo layout the script is run from: `imaginer-ui/examples`.
fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("imaginer-ui has a parent")
        .to_path_buf()
}

fn main() {
    let root = repo_root();
    let out = root.join("packaging");
    std::fs::create_dir_all(&out).expect("packaging/ can be created");

    let icon = parse(&root.join("assets/imaginer_logoicon.svg"));
    let logotype = parse(&root.join("assets/imaginer_logotype.svg"));

    // The icon: rendered well past the ladder's top rung so every size in the
    // .ico is a clean downsample. `export::write` builds the whole ladder, PNG
    // at 256 and raw BMP below, exactly as the executable's own resource does.
    let square = squarify(&icon.render_longest(512).expect("icon rasterises"));
    imaginer_core::export::write(
        &square,
        &out.join("imaginer.ico"),
        &ExportSettings {
            format: Format::Ico,
            ..Default::default()
        },
    )
    .expect("writing imaginer.ico failed");

    // Header strip: the mark alone. The page beside it already says "Imaginer
    // 0.1.0 Setup" in bold, so a wordmark here would be the app's name printed
    // twice, six pixels apart, at two different sizes — and squeezing one into
    // what is left of 150 points after the mark is what made it illegible.
    let mut header = solid(HEADER, HEADER_BG);
    let mark = fit(&icon, (40, 40));
    let at = centre_in(&header, &mark);
    paste(&mut header, &mark, at);
    save_bmp(&header, &out.join("header.bmp"));

    // Welcome panel: the mark large over the wordmark, on the app's darkest
    // background — this one is a panel of its own beside the wizard's text, not
    // a patch inside a white band, and the dark is what makes it read as the
    // app's rather than as Windows'.
    let mut sidebar = solid(SIDEBAR, BG_DEEPEST);
    let mark = fit(&icon, (96, 96));
    let (mark_x, _) = centre_in(&sidebar, &mark);
    paste(&mut sidebar, &mark, (mark_x, up(86)));

    let word = fit(&logotype, (120, 34));
    let (word_x, _) = centre_in(&sidebar, &word);
    paste(&mut sidebar, &word, (word_x, up(206)));
    save_bmp(&sidebar, &out.join("sidebar.bmp"));

    println!(
        "wrote {} at {SCALE}x: header {}x{}, sidebar {}x{}",
        out.display(),
        HEADER.0 * SCALE,
        HEADER.1 * SCALE,
        SIDEBAR.0 * SCALE,
        SIDEBAR.1 * SCALE
    );
}

/// A measurement in the documented size, scaled up to what is actually drawn.
fn up(measure: u32) -> i64 {
    i64::from(measure * SCALE)
}

fn parse(path: &std::path::Path) -> Svg {
    let data = std::fs::read(path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
    Svg::parse(&data).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

/// Render `art` as large as it goes inside `box_size` without being cropped or
/// stretched, at [`SCALE`].
///
/// From the vector each time rather than resampling one big raster: the whole
/// reason the artwork is SVG is that there is an exactly-right rasterisation
/// for every size, and reaching for it costs a millisecond.
fn fit(art: &Svg, box_size: (u32, u32)) -> RgbaImage {
    let (box_width, box_height) = (box_size.0 * SCALE, box_size.1 * SCALE);
    let (width, height) = art.size();
    // Whichever side runs out first decides, which is what stops a wide
    // wordmark from being sized by its height and running off the panel.
    let scale = (box_width as f32 / width).min(box_height as f32 / height);
    let longest = (width.max(height) * scale).round().max(1.0) as u32;
    art.render_longest(longest)
        .expect("artwork rasterises at a size that fits a panel")
}

/// Where to paste `art` so it sits in the middle of `canvas`.
///
/// `(canvas - art) / 2`. Spelled out once, here, because the same expression
/// written by hand at three call sites is where this file's cropping came from.
fn centre_in(canvas: &RgbaImage, art: &RgbaImage) -> (i64, i64) {
    (
        (i64::from(canvas.width()) - i64::from(art.width())) / 2,
        (i64::from(canvas.height()) - i64::from(art.height())) / 2,
    )
}

/// Centre the artwork on a transparent square, so the icon's ladder entries are
/// square even though the source is only assumed to be roughly so.
fn squarify(art: &RgbaImage) -> RgbaImage {
    let side = art.width().max(art.height());
    let mut out = RgbaImage::new(side, side);
    let at = centre_in(&out, art);
    paste(&mut out, art, at);
    out
}

fn solid(size: (u32, u32), rgb: [u8; 3]) -> RgbaImage {
    RgbaImage::from_pixel(
        size.0 * SCALE,
        size.1 * SCALE,
        Rgba([rgb[0], rgb[1], rgb[2], 255]),
    )
}

/// Alpha-composite `src` onto `dst`, clipping whatever falls outside.
fn paste(dst: &mut RgbaImage, src: &RgbaImage, at: (i64, i64)) {
    for sy in 0..src.height() as i64 {
        for sx in 0..src.width() as i64 {
            let (dx, dy) = (at.0 + sx, at.1 + sy);
            if dx < 0 || dy < 0 || dx >= dst.width() as i64 || dy >= dst.height() as i64 {
                continue;
            }
            let source = src.get_pixel(sx as u32, sy as u32).0;
            let alpha = f32::from(source[3]) / 255.0;
            if alpha == 0.0 {
                continue;
            }
            let target = dst.get_pixel_mut(dx as u32, dy as u32);
            for channel in 0..3 {
                target.0[channel] = (f32::from(source[channel]) * alpha
                    + f32::from(target.0[channel]) * (1.0 - alpha))
                    .round() as u8;
            }
            target.0[3] = 255;
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
