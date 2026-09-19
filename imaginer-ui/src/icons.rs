//! The UI icon set, rasterised into alpha masks by `build.rs`.
//!
//! Icons are stored as coverage, not colour: the artwork is monochrome strokes, so
//! one byte per pixel says everything the runtime needs and the whole set costs
//! ~60KB in the binary. Colour arrives at draw time from the widget's own
//! `fg_stroke`, which is what makes hover, active and disabled free — no state
//! needs a second raster, and an icon can never drift from the theme.
//!
//! Uploads are lazy. A launch that only ever draws the toolbar pays for the
//! toolbar's icons and nothing else.

use eframe::egui;
use eframe::egui::emath::GuiRounding as _;

include!(concat!(env!("OUT_DIR"), "/icons.rs"));

const MASKS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/icons.alpha"));
const MASK_LEN: usize = ICON_PX * ICON_PX;

/// Drawn edge of an icon, in points. Well under the 48px raster, so icons are
/// always minified — the case linear filtering plus a mipmap handles cleanly.
const DRAW_SIZE: f32 = 18.0;

/// Hit box around an icon. Wider than tall: a row of these reads as a toolbar,
/// while a square would read as a grid of tiles.
const BUTTON_SIZE: egui::Vec2 = egui::vec2(30.0, 26.0);

/// Horizontal padding inside a text button, so it sits at the same rhythm as the
/// icon buttons beside it.
const TEXT_PADDING: f32 = 8.0;

/// Lazily uploaded icon textures, one slot per [`Icon`] variant.
pub struct Icons {
    uploaded: Vec<Option<egui::TextureHandle>>,
}

impl Default for Icons {
    fn default() -> Self {
        Self {
            uploaded: vec![None; ICON_COUNT],
        }
    }
}

impl Icons {
    fn texture(&mut self, ctx: &egui::Context, icon: Icon) -> egui::TextureId {
        self.uploaded[icon as usize]
            .get_or_insert_with(|| upload(ctx, icon))
            .id()
    }
}

fn upload(ctx: &egui::Context, icon: Icon) -> egui::TextureHandle {
    let slot = icon as usize;
    let mask = &MASKS[slot * MASK_LEN..(slot + 1) * MASK_LEN];

    // egui composites in premultiplied alpha, so a white texel at coverage `a` is
    // literally `[a, a, a, a]`. Multiplying that by an opaque tint gives correctly
    // premultiplied coloured pixels, which is why tinting works without a shader.
    let image = egui::ColorImage::new(
        [ICON_PX, ICON_PX],
        mask.iter()
            .map(|&a| egui::Color32::from_white_alpha(a))
            .collect(),
    );

    ctx.load_texture(
        format!("icon-{icon:?}"),
        image,
        egui::TextureOptions::LINEAR.with_mipmap_mode(Some(egui::TextureFilter::Linear)),
    )
}

/// Draw an icon into `rect`, tinted, with no interaction of its own.
///
/// For chrome that is not a toolbar button — the canvas chevrons own their hit
/// area and their own fade, so they need the drawing without the widget around it.
pub fn paint(ui: &egui::Ui, icons: &mut Icons, icon: Icon, rect: egui::Rect, tint: egui::Color32) {
    let texture = icons.texture(ui.ctx(), icon);
    ui.painter().image(
        texture,
        rect.round_to_pixels(ui.pixels_per_point()),
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        tint,
    );
}

/// An icon button: transparent at rest, filling in only under the pointer.
///
/// Chrome that is quiet until you reach for it is what lets a toolbar sit above a
/// photograph without competing with it.
pub fn button(ui: &mut egui::Ui, icons: &mut Icons, icon: Icon, tooltip: &str) -> egui::Response {
    toggle(ui, icons, icon, tooltip, false)
}

/// The same button, drawn lit when `on`.
///
/// For the buttons that turn something on and leave it on — the slideshow, the
/// sidebar, fullscreen. Without this they look identical running and stopped,
/// so the only way to know whether the slideshow is going is to watch for the
/// next picture. Lit is a filled backdrop in the accent, not a different icon:
/// the icon says what the button does, and changing it would mean learning two
/// glyphs for one action.
pub fn toggle(
    ui: &mut egui::Ui,
    icons: &mut Icons,
    icon: Icon,
    tooltip: &str,
    on: bool,
) -> egui::Response {
    let (rect, response, mut visuals) = flat_button(ui, BUTTON_SIZE);

    if on {
        ui.painter().rect_filled(
            rect,
            visuals.corner_radius,
            crate::theme::ACCENT_COLOR.gamma_multiply(0.30),
        );
        // The icon takes the accent too, so a lit button reads at a glance
        // rather than only under the pointer.
        visuals.fg_stroke.color = crate::theme::ACCENT_COLOR;
    }

    if ui.is_rect_visible(rect) {
        let icon_rect = egui::Rect::from_center_size(rect.center(), egui::Vec2::splat(DRAW_SIZE))
            .round_to_pixels(ui.pixels_per_point());
        let texture = icons.texture(ui.ctx(), icon);
        ui.painter().image(
            texture,
            icon_rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            visuals.fg_stroke.color,
        );
    }

    response.on_hover_text(tooltip)
}

/// The same button in text form, for the one control with no unambiguous glyph.
pub fn text_button(ui: &mut egui::Ui, label: &str, tooltip: &str) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(
        label.to_owned(),
        egui::TextStyle::Button.resolve(ui.style()),
        // Resolved when the galley is painted, so the colour can come from the
        // interaction state that only exists after the button has been allocated.
        egui::Color32::PLACEHOLDER,
    );

    let size = egui::vec2(galley.size().x + 2.0 * TEXT_PADDING, BUTTON_SIZE.y);
    let (rect, response, visuals) = flat_button(ui, size);

    if ui.is_rect_visible(rect) {
        let pos = (rect.center() - 0.5 * galley.size()).round_to_pixels(ui.pixels_per_point());
        ui.painter().galley(pos, galley, visuals.fg_stroke.color);
    }

    response.on_hover_text(tooltip)
}

/// Allocate a button-shaped area and paint its background, returning the visuals so
/// the caller can draw the content in a colour that matches the interaction state.
///
/// Inside a disabled `Ui` the painter fades everything it is given, so neither
/// caller needs to know about the disabled state at all.
fn flat_button(
    ui: &mut egui::Ui,
    size: egui::Vec2,
) -> (egui::Rect, egui::Response, egui::style::WidgetVisuals) {
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let visuals = *ui.style().interact(&response);

    if ui.is_rect_visible(rect) && (response.hovered() || response.is_pointer_button_down_on()) {
        ui.painter()
            .rect_filled(rect, visuals.corner_radius, visuals.weak_bg_fill);
    }

    (rect, response, visuals)
}
