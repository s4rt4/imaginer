//! Rasterises the logo and UI-icon SVGs at build time.
//!
//! Still done here now that the app can render SVG at runtime, and for the reason
//! that always applied: the chrome must not be *waiting* on a renderer. Doing it
//! here keeps the SVG as the source of truth — edit the artwork, rebuild, done —
//! while startup only ever sees a flat array of pixels to hand to the window
//! manager or the GPU.
//!
//! The rendering itself goes through `imaginer_core::Svg`, the same path a user's
//! own `.svg` takes. That is the rule the embedded `.ico` already follows, and it
//! also means resvg is compiled once for this workspace rather than twice with
//! different features.

use std::path::{Path, PathBuf};

/// Window icon edge, in pixels. Windows asks for anything from 16px in the title bar
/// to 256px in alt-tab; 128 downsamples cleanly to all of them without the weight of
/// a full 256px buffer.
const ICON_SIZE: u32 = 128;

/// Edge of the raster the executable's icon resource is built from.
///
/// 256 because that is the largest entry an `.ico` can hold and what the shell's
/// preview pane asks for. Rendered separately from [`ICON_SIZE`] rather than
/// sharing it: this one is written to a file at build time and never loaded at
/// runtime, so it costs nothing to make it as large as the format allows.
const APP_ICON_SIZE: u32 = 256;

/// Logotype width, in pixels. Drawn at roughly half this size, so the extra pixels
/// are headroom for high-DPI displays rather than waste.
const LOGOTYPE_WIDTH: u32 = 384;

/// Edge of a rasterised UI icon, in pixels. Drawn at ~18px, so this is 2.6x
/// oversampled: enough headroom for a 200% display, and minified everywhere else,
/// which is the case the mipmapped linear filter handles well.
const UI_ICON_SIZE: u32 = 48;

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

    rasterise_ui_icons(&assets.join("icons"), &out_dir);
    embed_executable_icon(&assets.join("imaginer_logoicon.svg"), &out_dir);
}

/// Attach the logo to the executable as a Win32 icon resource.
///
/// This is not the same thing as the window icon set through eframe, and one does
/// not stand in for the other: the runtime icon is handed to the window manager
/// after the process starts, so Explorer, shortcuts, the taskbar's pinned entry and
/// anything reading `"imaginer.exe,0"` — the shell verbs in
/// `scripts/install-shell-integration.ps1` among them — all see nothing without it.
///
/// The `.ico` is written here by the crate's own encoder rather than committed as a
/// binary asset, which keeps the SVG the single source of truth and means the icon
/// Explorer draws comes off exactly the code path the Convert menu writes files
/// with.
fn embed_executable_icon(svg_path: &Path, out_dir: &Path) {
    // Build scripts run on the host, so the host's own `cfg` says nothing about what
    // is being built. Only a Windows target has resources to attach.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let raster = render(svg_path, APP_ICON_SIZE);
    let pixels =
        imaginer_core::image::RgbaImage::from_raw(raster.width, raster.height, raster.rgba)
            .expect("the rasterised icon is RGBA8 and its buffer matches its dimensions");

    let ico = out_dir.join("imaginer.ico");
    imaginer_core::export::write(
        &pixels,
        &ico,
        &imaginer_core::ExportSettings {
            format: imaginer_core::Format::Ico,
            ..Default::default()
        },
    )
    .unwrap_or_else(|err| panic!("failed to write the executable icon: {err}"));

    #[cfg(windows)]
    winresource::WindowsResource::new()
        .set_icon(ico.to_str().expect("OUT_DIR is valid UTF-8"))
        // Without these the resource carries the crate name, so Explorer's
        // properties pane and the "Open with" list both offer "imaginer-ui".
        .set("ProductName", "Imaginer")
        .set("FileDescription", "Imaginer — image viewer and converter")
        .set("OriginalFilename", "imaginer.exe")
        .set("LegalCopyright", "MIT licensed — see LICENSE")
        .compile()
        .unwrap_or_else(|err| {
            panic!(
                "failed to compile the icon resource: {err}\n\
                 This needs rc.exe from the Windows SDK, which ships with the MSVC \
                 toolchain this crate already builds under."
            )
        });
}

/// Rasterise every icon in `dir` into one packed blob of alpha masks, plus the
/// generated enum that indexes it.
///
/// Only the alpha channel is kept. The icons are monochrome strokes, so the three
/// colour channels carry no information the runtime needs — it tints them from the
/// widget's own foreground colour instead, which is also what makes hover, disabled
/// and active states free rather than three more rasters. At 48px that is 2.25KB an
/// icon rather than 9KB.
fn rasterise_ui_icons(dir: &Path, out_dir: &Path) {
    // Directory-level, so adding or removing an icon rebuilds. `render` adds a
    // per-file watch on top, which is what catches edits to the artwork itself.
    println!("cargo:rerun-if-changed={}", dir.display());

    let mut icons: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", dir.display()))
        .map(|entry| {
            entry
                .expect("failed to read an icon directory entry")
                .path()
        })
        .filter(|path| path.extension().is_some_and(|ext| ext == "svg"))
        .collect();
    // read_dir order is filesystem-defined; sorting is what keeps the generated
    // enum's variant order — and therefore its discriminants — stable across builds.
    icons.sort();

    let mut masks = Vec::with_capacity(icons.len() * (UI_ICON_SIZE * UI_ICON_SIZE) as usize);
    let mut variants = String::new();

    for path in &icons {
        let raster = render(path, UI_ICON_SIZE);
        assert_eq!(
            raster.height,
            UI_ICON_SIZE,
            "{} is not square; the runtime assumes every icon mask is {UI_ICON_SIZE}x{UI_ICON_SIZE}",
            path.display()
        );
        masks.extend(raster.rgba.chunks_exact(4).map(|pixel| pixel[3]));

        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_else(|| panic!("icon name is not valid UTF-8: {}", path.display()));
        variants.push_str(&format!(
            "    /// `{stem}.svg`\n    {},\n",
            camel_case(stem)
        ));
    }

    std::fs::write(out_dir.join("icons.alpha"), &masks).expect("failed to write icon masks");
    std::fs::write(
        out_dir.join("icons.rs"),
        format!(
            "// @generated by build.rs — add or edit files in assets/icons/ instead.\n\
             \n\
             /// Edge of every rasterised icon mask, in pixels.\n\
             pub const ICON_PX: usize = {UI_ICON_SIZE};\n\
             \n\
             /// Number of icons in the packed mask blob.\n\
             pub const ICON_COUNT: usize = {};\n\
             \n\
             /// One variant per `assets/icons/*.svg`, in sorted filename order — the\n\
             /// discriminant doubles as the icon's slot in the blob.\n\
             //\n\
             // The whole set is generated whether or not the UI reaches for it yet:\n\
             // the artwork is a set, and an icon nothing draws costs 2.25KB in the\n\
             // binary and nothing at all at runtime, since uploads are lazy.\n\
             #[allow(dead_code)]\n\
             #[derive(Debug, Clone, Copy, PartialEq, Eq)]\n\
             pub enum Icon {{\n{variants}}}\n",
            icons.len(),
        ),
    )
    .expect("failed to write icon table");
}

/// `flip-horizontal-2` -> `FlipHorizontal2`.
fn camel_case(stem: &str) -> String {
    stem.split('-')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

struct Raster {
    width: u32,
    height: u32,
    /// Straight (non-premultiplied) RGBA, which is what both `egui::IconData` and
    /// `ColorImage::from_rgba_unmultiplied` expect.
    rgba: Vec<u8>,
}

/// Rasterise one SVG to an exact width, with the height following its aspect ratio.
///
/// `render_width` rather than the viewer's `render_fit`: the packing code below is
/// told a stride and every mask has to be exactly that wide.
///
/// Note that parsing loads the system fonts, since that is what the runtime path
/// needs. It costs the build a couple of hundred milliseconds once. It would also
/// make the output depend on the machine's fonts — but only for artwork containing
/// `<text>`, and everything in `assets/` is paths. Convert type to outlines before
/// adding artwork here.
fn render(svg_path: &Path, target_width: u32) -> Raster {
    println!("cargo:rerun-if-changed={}", svg_path.display());

    let data = std::fs::read(svg_path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", svg_path.display()));
    let pixels = imaginer_core::Svg::parse(&data)
        .and_then(|svg| svg.render_width(target_width))
        .unwrap_or_else(|err| panic!("failed to rasterise {}: {err}", svg_path.display()));

    Raster {
        width: pixels.width(),
        height: pixels.height(),
        rgba: pixels.into_raw(),
    }
}
