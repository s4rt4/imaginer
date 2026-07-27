//! Slim top bar. Kept to the actions that earn their pixels in a viewer.

use eframe::egui;

use crate::views::viewer::ViewState;

/// Actions the toolbar can request. Returned rather than applied so the app owns
/// all state transitions in one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Open,
}

pub fn show(ui: &mut egui::Ui, state: &mut ViewState, has_image: bool) -> Option<Action> {
    let mut action = None;

    ui.horizontal(|ui| {
        if ui.button("Open").on_hover_text("Open an image (Ctrl+O)").clicked() {
            action = Some(Action::Open);
        }

        ui.add_enabled_ui(has_image, |ui| {
            ui.separator();

            if ui.button("Fit").on_hover_text("Fit to window (F)").clicked() {
                state.reset();
            }
            if ui.button("100%").on_hover_text("Actual size (1)").clicked() {
                state.set_zoom(1.0);
            }
            if ui.button("Rotate").on_hover_text("Rotate view 90° (R)").clicked() {
                state.rotate_clockwise();
            }
        });
    });

    action
}
