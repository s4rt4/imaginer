//! Rasterises the logo SVGs at build time.
//!
//! The alternative — rendering SVG at runtime — would mean carrying resvg in the
//! shipping binary and paying for it during startup, which is the one thing this
//! project is not willing to spend. Doing it here keeps the SVG as the source of
//! truth (edit the artwork, rebuild, done) while the binary only ever sees a flat
//! array of pixels it can hand straight to the window manager or the GPU.

use std::path::{Path, PathBuf};

use resvg::{tiny_skia, usvg};

/// Window icon edge, in pixels. Windows asks for anything from 16px in the title bar
/// to 256px in alt-tab; 128 downsamples cleanly to all of them without the weight of
/// a full 256px buffer.
const ICON_SIZE: u32 = 128;

/// Logotype width, in pixels. Drawn at roughly half this size, so the extra pixels
/// are headroom for high-DPI displays rather than waste.
const LOGOTYPE_WIDTH: u32 = 384;

fn main() {
    let assets = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("imaginer-ui has a parent directory")
        .join("assets");
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));

    let icon = render(&assets.join("imaginer_logoicon.svg"), ICON_SIZE);
    let logotype = render(&assets.join("imaginer_logotype.svg"), LOGOTYPE_WIDTH);

    std::fs::write(out_dir.join("logoicon.rgba"), &icon.rgba).expect("failed to write icon pixels");
    std::fs::write(out_dir.join("logotype.rgba"), &logotype.rgba)
        .expect("failed to write logotype pixels");

    // Emitted rather than hard-coded in both places, so the constants above stay the
    // only definition of the sizes.
    std::fs::write(
        out_dir.join("logo_dimensions.rs"),
        format!(
            "pub const ICON_SIZE: u32 = {};\n\
             pub const LOGOTYPE_SIZE: [usize; 2] = [{}, {}];\n",
            icon.width, logotype.width, logotype.height
        ),
    )
    .expect("failed to write logo dimensions");
}

struct Raster {
    width: u32,
    height: u32,
    /// Straight (non-premultiplied) RGBA, which is what both `egui::IconData` and
    /// `ColorImage::from_rgba_unmultiplied` expect.
    rgba: Vec<u8>,
}

fn render(svg_path: &Path, target_width: u32) -> Raster {
    println!("cargo:rerun-if-changed={}", svg_path.display());

    let svg = std::fs::read(svg_path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", svg_path.display()));
    let tree = usvg::Tree::from_data(&svg, &usvg::Options::default())
        .unwrap_or_else(|err| panic!("failed to parse {}: {err}", svg_path.display()));

    // Scale uniformly from the SVG's own size so the artwork keeps its aspect ratio
    // whatever the source viewBox happens to be.
    let scale = target_width as f32 / tree.size().width();
    let height = (tree.size().height() * scale).round().max(1.0) as u32;

    let mut pixmap = tiny_skia::Pixmap::new(target_width, height)
        .unwrap_or_else(|| panic!("invalid raster size {target_width}x{height}"));
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );

    // tiny-skia composites in premultiplied alpha; undo that here rather than in the
    // app, so the runtime never has to know how these pixels were produced.
    let mut rgba = Vec::with_capacity(pixmap.pixels().len() * 4);
    for pixel in pixmap.pixels() {
        let demultiplied = pixel.demultiply();
        rgba.extend_from_slice(&[
            demultiplied.red(),
            demultiplied.green(),
            demultiplied.blue(),
            demultiplied.alpha(),
        ]);
    }

    Raster {
        width: target_width,
        height,
        rgba,
    }
}
