//! Slim top bar. Kept to the actions that earn their pixels in a viewer.
//!
//! Icons rather than words, grouped by what they do to the image: open it, look at
//! it, do something with the file. Navigation is deliberately absent — it lives on
//! the canvas, where the eye already is, and keeping it out of here is what lets
//! this row stay short enough to read at a glance.

use eframe::egui;
use imaginer_core::{Order, SortKey};

use crate::icons::{self, Icon, Icons};
use crate::views::viewer::ViewState;

/// Actions the toolbar can request. Returned rather than applied so the app owns
/// all state transitions in one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Open,
    OpenFolder,
    Sort(Order),
    ToggleInfo,
    CopyPath,
    Delete,
    ToggleFullscreen,
    ToggleSlideshow,
    ToggleSidebar,
    ToggleSettings,
}

/// What the toolbar needs to know about the rest of the app to draw itself.
#[derive(Debug, Clone, Copy)]
pub struct Bar {
    pub has_image: bool,
    /// The zoom the canvas actually drew at last frame, which is what the
    /// toolbar's zoom control reports.
    pub zoom: f32,
    pub fullscreen: bool,
    pub slideshow: bool,
    /// Whether there is anywhere to step to, which is what makes a slideshow
    /// worth offering at all.
    pub has_neighbours: bool,
    pub sidebar_open: bool,
    pub info_open: bool,
    pub settings_open: bool,
    /// What the folder listing is currently ordered by.
    pub order: Order,
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
        // Beside the folder button rather than with the view controls: it changes
        // what "next" means, which is a fact about the folder and not about how the
        // image on screen is being looked at.
        ui.add_enabled_ui(bar.has_neighbours, |ui| {
            if let Some(order) = sort_menu(ui, icons, bar.order) {
                action = Some(Action::Sort(order));
            }
        });

        separator(ui);

        // Everything past this point acts on the image on screen, so it is dead
        // weight until there is one.
        ui.add_enabled_ui(bar.has_image, |ui| {
            ui.spacing_mut().item_spacing.x = BUTTON_SPACING;

            if icons::button(ui, icons, Icon::FitScreen, "Fit to window (F)").clicked() {
                view.reset();
            }
            // The live zoom, and a menu of the ones worth a click. It used to be
            // a button labelled "100%" that set the zoom to 100% — which sat two
            // inches from a status bar reading "37%", so half the time it was
            // read as a display of the current zoom rather than as a control.
            // Showing the real number settles that, and the presets are what the
            // button did plus the four anyone actually wants.
            zoom_menu(ui, view, bar.zoom);
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

            // First of the group that acts on the file rather than on the view: what
            // this file *is* comes before anything done with it.
            let tooltip = if bar.info_open {
                "Close the info panel (I)"
            } else {
                "File and EXIF info (I)"
            };
            // Lit while its panel is up. Unlike fullscreen and the slideshow
            // above, this button cannot say so by swapping its icon: there is no
            // "close the info panel" glyph, and inventing one would be a second
            // symbol to learn for one action. The three panel buttons light
            // instead, which is the same answer to the same question.
            if icons::toggle(ui, icons, Icon::Info, tooltip, bar.info_open).clicked() {
                action = Some(Action::ToggleInfo);
            }

            // `P`, not the Ctrl+Shift+C anyone would guess at: egui swallows every
            // Ctrl+C combination before the app can see it. See `handle_shortcuts`.
            if icons::button(ui, icons, Icon::Link, "Copy file path (P)").clicked() {
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
            // Beside the edit toggle rather than with the file actions: it opens
            // a panel, like the edit sidebar does, and works with no image open.
            let tooltip = if bar.settings_open {
                "Close settings (S)"
            } else {
                "Settings and shortcuts (S)"
            };
            if icons::toggle(ui, icons, Icon::Setting, tooltip, bar.settings_open).clicked() {
                action = Some(Action::ToggleSettings);
            }
            ui.add_enabled_ui(bar.has_image, |ui| {
                let tooltip = if bar.sidebar_open {
                    "Close the edit sidebar (E)"
                } else {
                    "Edit this image (E)"
                };
                if icons::toggle(ui, icons, Icon::Edits, tooltip, bar.sidebar_open).clicked() {
                    action = Some(Action::ToggleSidebar);
                }
            });
        });
    });

    action
}

/// The sort button and the little menu it opens.
///
/// The menu deliberately does **not** close when something in it is picked. Order is
/// two choices, not one — "newest first" is Date and then Reverse — and a menu that
/// shut after each would make that two trips. The listing re-sorts behind it as each
/// is clicked, so the effect of a choice is visible while the next one is still
/// under the pointer. Clicking away, or the button again, closes it.
/// The current zoom, and the handful worth jumping straight to.
///
/// Percentages a viewer is asked for by name, rather than a continuous slider:
/// zooming by degrees is what the wheel is for, and what a menu adds is the
/// ability to land on a *number* — 100% to check focus, 400% to look at an edge,
/// fit to see the whole thing again.
fn zoom_menu(ui: &mut egui::Ui, view: &mut ViewState, zoom: f32) {
    /// Beyond 400% the wheel is a better tool than a list, and below 25% there
    /// is nothing to see that fit does not show.
    const PRESETS: [f32; 5] = [0.25, 0.5, 1.0, 2.0, 4.0];

    let label = format!("{:.0}%", zoom * 100.0);
    let button = icons::text_button(ui, &label, "Zoom");
    egui::Popup::menu(&button).show(|ui| {
        if ui.selectable_label(false, "Fit to window").clicked() {
            view.reset();
        }
        ui.separator();
        for preset in PRESETS {
            // Marked when the canvas is already there, within a hair: the zoom
            // arrived at by wheel is never exactly 1.0, and a menu that never
            // ticks anything is a menu that does not know what it is showing.
            let here = (zoom - preset).abs() < 0.005;
            if ui
                .selectable_label(here, format!("{:.0}%", preset * 100.0))
                .clicked()
            {
                view.set_zoom(preset);
            }
        }
    });
}

fn sort_menu(ui: &mut egui::Ui, icons: &mut Icons, order: Order) -> Option<Order> {
    let mut chosen = None;

    let button = icons::button(ui, icons, Icon::Sort, "Sort this folder");
    egui::Popup::menu(&button).show(|ui| {
        for key in SortKey::ALL {
            if ui.selectable_label(key == order.key, key.label()).clicked() {
                chosen = Some(Order { key, ..order });
            }
        }

        ui.separator();

        // A checkbox rather than an "ascending/descending" pair, because what
        // "ascending" means changes with the key — for dates most people want the
        // newest and could not tell you whether that is up or down.
        if ui.selectable_label(order.descending, "Reverse").clicked() {
            chosen = Some(Order {
                descending: !order.descending,
                ..order
            });
        }
    });

    chosen
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
