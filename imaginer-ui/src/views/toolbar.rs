//! Slim top bar. Kept to the actions that earn their pixels in a viewer.
//!
//! Icons rather than words, grouped by what they do to the image: open it, look at
//! it, do something with the file. Navigation is deliberately absent — it lives on
//! the canvas, where the eye already is, and keeping it out of here is what lets
//! this row stay short enough to read at a glance.

use eframe::egui;

use crate::icons::{self, Icon, Icons};
use crate::views::viewer::ViewState;

/// Actions the toolbar can request. Returned rather than applied so the app owns
/// all state transitions in one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Open,
    OpenFolder,
    CopyPath,
    Delete,
    ToggleFullscreen,
    ToggleSlideshow,
    ToggleSidebar,
}

/// What the toolbar needs to know about the rest of the app to draw itself.
#[derive(Debug, Clone, Copy)]
pub struct Bar {
    pub has_image: bool,
    pub fullscreen: bool,
    pub slideshow: bool,
    /// Whether there is anywhere to step to, which is what makes a slideshow
    /// worth offering at all.
    pub has_neighbours: bool,
    pub sidebar_open: bool,
}

/// Gap between buttons. Tighter than the app-wide spacing: a toolbar reads as
/// groups of related controls, and the groups are told apart by the separators
/// rather than by air between every pair of icons.
const BUTTON_SPACING: f32 = 2.0;

/// Width of the space a group separator occupies, line included.
const SEPARATOR_WIDTH: f32 = 13.0;

/// Height of the separator line itself — short of the full row, so it divides
/// without drawing attention to itself.
const SEPARATOR_HEIGHT: f32 = 16.0;

pub fn show(
    ui: &mut egui::Ui,
    icons: &mut Icons,
    view: &mut ViewState,
    bar: Bar,
) -> Option<Action> {
    let mut action = None;

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = BUTTON_SPACING;

        if icons::button(ui, icons, Icon::OpenImage, "Open an image (Ctrl+O)").clicked() {
            action = Some(Action::Open);
        }
        if icons::button(ui, icons, Icon::FolderOpen, "Open a folder (Ctrl+Shift+O)").clicked() {
            action = Some(Action::OpenFolder);
        }

        separator(ui);

        // Everything past this point acts on the image on screen, so it is dead
        // weight until there is one.
        ui.add_enabled_ui(bar.has_image, |ui| {
            ui.spacing_mut().item_spacing.x = BUTTON_SPACING;

            if icons::button(ui, icons, Icon::FitScreen, "Fit to window (F)").clicked() {
                view.reset();
            }
            // Text, because "actual size" has no glyph anyone reads unambiguously.
            if icons::text_button(ui, "100%", "Actual size (1)").clicked() {
                view.set_zoom(1.0);
            }
            // The icon shows what the button will do next, not which mode you are
            // in — a button that depicts the current state leaves you guessing what
            // pressing it does.
            let (icon, tooltip) = if bar.fullscreen {
                (Icon::FullScreenExit, "Leave fullscreen (F11 or Esc)")
            } else {
                (Icon::FullScreen, "Fullscreen (F11)")
            };
            if icons::button(ui, icons, icon, tooltip).clicked() {
                action = Some(Action::ToggleFullscreen);
            }

            ui.add_enabled_ui(bar.has_neighbours, |ui| {
                ui.spacing_mut().item_spacing.x = BUTTON_SPACING;
                let (icon, tooltip) = if bar.slideshow {
                    (Icon::Pause, "Stop the slideshow (Space)")
                } else {
                    (Icon::Play, "Play a slideshow (Space)")
                };
                if icons::button(ui, icons, icon, tooltip).clicked() {
                    action = Some(Action::ToggleSlideshow);
                }
            });
        });

        separator(ui);

        ui.add_enabled_ui(bar.has_image, |ui| {
            ui.spacing_mut().item_spacing.x = BUTTON_SPACING;

            if icons::button(ui, icons, Icon::Link, "Copy file path (Ctrl+Shift+C)").clicked() {
                action = Some(Action::CopyPath);
            }
            // Last in the row, as far as it can get from the buttons that are
            // clicked constantly.
            if icons::button(ui, icons, Icon::Trash2, "Move to Recycle Bin (Del)").clicked() {
                action = Some(Action::Delete);
            }
        });

        // Alone at the far end, because it opens a whole second surface rather than
        // doing one thing to the image — the other buttons are all instant.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_enabled_ui(bar.has_image, |ui| {
                let tooltip = if bar.sidebar_open {
                    "Close the edit sidebar (E)"
                } else {
                    "Edit this image (E)"
                };
                if icons::button(ui, icons, Icon::Edits, tooltip).clicked() {
                    action = Some(Action::ToggleSidebar);
                }
            });
        });
    });

    action
}

/// A hairline between two groups of buttons.
fn separator(ui: &mut egui::Ui) {
    let height = ui.spacing().interact_size.y;
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(SEPARATOR_WIDTH, height), egui::Sense::hover());

    let painter = ui.painter();
    // A one-pixel line lands on a pixel centre or it renders as two grey ones.
    let x = painter.round_to_pixel_center(rect.center().x);
    let half = SEPARATOR_HEIGHT * 0.5;
    painter.vline(
        x,
        (rect.center().y - half)..=(rect.center().y + half),
        ui.visuals().widgets.noninteractive.bg_stroke,
    );
}
