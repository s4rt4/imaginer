//! Custom look, applied once at startup.
//!
//! Done in the first milestone rather than bolted on later, so every screen built
//! afterwards inherits it instead of needing a retrofit.

use std::sync::Arc;

use eframe::egui::{self, Color32, CornerRadius, Stroke};

/// A near-black neutral. A viewer's chrome should recede — anything lighter
/// competes with the image and skews how its own colours read.
const BG_DEEPEST: Color32 = Color32::from_rgb(0x0e, 0x0f, 0x11);
const BG_PANEL: Color32 = Color32::from_rgb(0x16, 0x18, 0x1c);
const BG_ELEVATED: Color32 = Color32::from_rgb(0x1f, 0x22, 0x27);
const BG_HOVER: Color32 = Color32::from_rgb(0x2a, 0x2e, 0x35);
const BORDER: Color32 = Color32::from_rgb(0x2b, 0x30, 0x38);
const TEXT: Color32 = Color32::from_rgb(0xe6, 0xe8, 0xeb);
const TEXT_DIM: Color32 = Color32::from_rgb(0x8b, 0x93, 0x9e);
const ACCENT: Color32 = Color32::from_rgb(0x4c, 0x9a, 0xff);

/// Fonts to try, in order of preference. Shipping without `default_fonts` means
/// egui starts with no font at all, so one of these has to load.
const FONT_CANDIDATES: &[&str] = &["segoeui.ttf", "arial.ttf", "tahoma.ttf", "verdana.ttf"];

pub fn install(ctx: &egui::Context) {
    install_fonts(ctx);
    install_visuals(ctx);
}

/// Load a single UI font from the system.
///
/// eframe's `default_fonts` feature is off deliberately: it bundles Ubuntu, Hack
/// and an emoji font (~1-2MB) and builds an atlas for all of them during init,
/// which is measurable startup cost for glyphs a photo viewer never draws.
fn install_fonts(ctx: &egui::Context) {
    let Some((name, data)) = load_system_font() else {
        // Not fatal: images still display, only the chrome text goes missing.
        eprintln!("imaginer: no usable system font found; UI text will not render");
        return;
    };

    let mut fonts = egui::FontDefinitions::empty();
    fonts
        .font_data
        .insert(name.clone(), Arc::new(egui::FontData::from_owned(data)));

    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts.families.entry(family).or_default().push(name.clone());
    }

    ctx.set_fonts(fonts);
}

fn load_system_font() -> Option<(String, Vec<u8>)> {
    let dir = std::env::var_os("WINDIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Windows"))
        .join("Fonts");

    FONT_CANDIDATES.iter().find_map(|file| {
        std::fs::read(dir.join(file))
            .ok()
            .map(|data| ((*file).to_owned(), data))
    })
}

fn install_visuals(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();

    visuals.panel_fill = BG_PANEL;
    visuals.window_fill = BG_PANEL;
    visuals.extreme_bg_color = BG_DEEPEST;
    visuals.faint_bg_color = BG_ELEVATED;
    visuals.window_stroke = Stroke::new(1.0, BORDER);
    visuals.selection.bg_fill = ACCENT.gamma_multiply(0.35);
    visuals.selection.stroke = Stroke::new(1.0, ACCENT);
    visuals.hyperlink_color = ACCENT;

    let widgets = &mut visuals.widgets;
    widgets.noninteractive.bg_fill = BG_PANEL;
    widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_DIM);
    widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    widgets.inactive.bg_fill = BG_ELEVATED;
    widgets.inactive.weak_bg_fill = BG_ELEVATED;
    widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    widgets.hovered.bg_fill = BG_HOVER;
    widgets.hovered.weak_bg_fill = BG_HOVER;
    widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT);
    widgets.hovered.bg_stroke = Stroke::new(1.0, BORDER);
    widgets.active.bg_fill = ACCENT.gamma_multiply(0.45);
    widgets.active.weak_bg_fill = ACCENT.gamma_multiply(0.45);
    widgets.active.fg_stroke = Stroke::new(1.0, TEXT);

    for w in [
        &mut widgets.noninteractive,
        &mut widgets.inactive,
        &mut widgets.hovered,
        &mut widgets.active,
        &mut widgets.open,
    ] {
        w.corner_radius = CornerRadius::same(5);
    }

    // Pin the theme rather than using `set_visuals`, which writes into whichever
    // theme is currently active. At `App::new` time egui has not yet learned the
    // system preference, so on a light-mode machine these visuals landed in the
    // dark slot and were then discarded when the first frame switched to light.
    // A photo viewer wants dark chrome regardless of the system setting anyway.
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.set_visuals_of(egui::Theme::Dark, visuals);

    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(9.0, 5.0);
        style.spacing.interact_size.y = 26.0;
    });
}

/// Background behind the image itself — darker than the panels so the photo reads
/// as the foreground object.
pub const CANVAS_BG: Color32 = BG_DEEPEST;
pub const TEXT_MUTED: Color32 = TEXT_DIM;
pub const ACCENT_COLOR: Color32 = ACCENT;

/// Chrome background, for the panels that draw their own frame.
pub const PANEL_BG: Color32 = BG_PANEL;

/// Colours handed to the window manager for the title bar, so the chrome the shell
/// draws is the same colour as the toolbar directly below it.
pub const TITLEBAR_BG: Color32 = BG_PANEL;
pub const TITLEBAR_TEXT: Color32 = TEXT;
