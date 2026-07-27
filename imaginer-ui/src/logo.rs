//! The logo artwork, rasterised at build time by `build.rs`.
//!
//! Both assets are flat RGBA arrays baked into the binary, so using them costs a
//! memcpy rather than a decode.

use eframe::egui;

include!(concat!(env!("OUT_DIR"), "/logo_dimensions.rs"));

const ICON_RGBA: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/logoicon.rgba"));
const LOGOTYPE_RGBA: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/logotype.rgba"));

/// Icon for the title bar, the taskbar and alt-tab.
///
/// This runs before the window exists, so it is on the critical path — but it is
/// only a copy of an already-decoded buffer, which does not register against the
/// ~900ms the graphics context costs.
pub fn window_icon() -> egui::IconData {
    egui::IconData {
        rgba: ICON_RGBA.to_vec(),
        width: ICON_SIZE,
        height: ICON_SIZE,
    }
}

/// Upload the wordmark, for the empty state.
///
/// Only called when there is no image to show, so a normal launch never pays for it.
pub fn logotype_texture(ctx: &egui::Context) -> egui::TextureHandle {
    let image = egui::ColorImage::from_rgba_unmultiplied(LOGOTYPE_SIZE, LOGOTYPE_RGBA);
    ctx.load_texture(
        "logotype",
        image,
        // Drawn well below its native size, so it is always minified: linear
        // filtering with a mipmap is what keeps the wordmark's thin strokes from
        // breaking up.
        egui::TextureOptions::LINEAR.with_mipmap_mode(Some(egui::TextureFilter::Linear)),
    )
}
