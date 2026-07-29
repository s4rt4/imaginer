//! The edit sidebar.
//!
//! Two sections, and the split between them is the point. **Transform** changes
//! pixels and previews live; **Export** changes how the file is written and touches
//! no pixel until Save. Putting "convert to PNG" next to "rotate" would imply an
//! edit that never happened, so it lives down in the Save panel instead — which is
//! also what gives Save an obvious home.

use eframe::egui;
use imaginer_core::{Edits, ExportSettings, Format, Op};

use crate::icons::{self, Icon, Icons};
use crate::theme;

/// Sidebar width. Wide enough for the export controls to breathe, narrow enough
/// that opening it does not shove the photograph off centre.
pub const WIDTH: f32 = 244.0;

/// What the sidebar can ask the app to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Close,
    Apply(Op),
    Undo,
    Redo,
    Save,
}

pub struct State<'a> {
    pub edits: &'a Edits,
    pub settings: &'a mut ExportSettings,
    /// Size of the edited image, before any export scaling.
    pub edited_size: Option<(u32, u32)>,
    /// Format of the file currently open, so Save can say whether it will
    /// overwrite that file or write a new one.
    pub source_format: Option<Format>,
    pub has_image: bool,
}

pub fn show(ui: &mut egui::Ui, icons: &mut Icons, state: &mut State<'_>) -> Option<Action> {
    let mut action = None;

    // Bottom-anchored, and declared before the content above it, which is how a
    // panel claims its space: Save stays put while the transform list grows.
    egui::Panel::bottom(egui::Id::new("sidebar_export"))
        .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(12, 12)))
        .show(ui, |ui| {
            if let Some(requested) = export(ui, state) {
                action = Some(requested);
            }
        });

    egui::Frame::NONE
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            if let Some(requested) = header(ui, icons) {
                action = Some(requested);
            }
            ui.add_space(8.0);

            if let Some(requested) = transform(ui, icons, state) {
                action = Some(requested);
            }
        });

    action
}

fn header(ui: &mut egui::Ui, icons: &mut Icons) -> Option<Action> {
    let mut action = None;

    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Edit").strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if icons::button(ui, icons, Icon::X, "Close the sidebar (E)").clicked() {
                action = Some(Action::Close);
            }
        });
    });

    action
}

fn transform(ui: &mut egui::Ui, icons: &mut Icons, state: &State<'_>) -> Option<Action> {
    let mut action = None;

    section(ui, "Transform");

    ui.add_enabled_ui(state.has_image, |ui| {
        egui::Grid::new("transform_ops")
            .num_columns(3)
            .spacing([6.0, 3.0])
            .show(ui, |ui| {
                let mut row = |label: &str, pair: [(Icon, &str, Op); 2]| {
                    ui.colored_label(theme::TEXT_MUTED, label);
                    for (icon, tooltip, op) in pair {
                        if icons::button(ui, icons, icon, tooltip).clicked() {
                            action = Some(Action::Apply(op));
                        }
                    }
                    ui.end_row();
                };

                row(
                    "Flip",
                    [
                        (
                            Icon::FlipHorizontal2,
                            "Flip horizontally",
                            Op::FlipHorizontal,
                        ),
                        (Icon::FlipVertical2, "Flip vertically", Op::FlipVertical),
                    ],
                );
                row(
                    "Rotate",
                    [
                        (Icon::RotateCcw, "Rotate 90° anticlockwise", Op::RotateCcw),
                        (Icon::RotateCw, "Rotate 90° clockwise (R)", Op::RotateCw),
                    ],
                );
                // Not a flip: this keeps the image and puts its reflection beside
                // it, so the canvas comes out twice the size.
                row(
                    "Mirror",
                    [
                        (
                            Icon::MirrorHorizontal,
                            "Mirror sideways — twice as wide",
                            Op::MirrorHorizontal,
                        ),
                        (
                            Icon::MirrorVertical,
                            "Mirror downwards — twice as tall",
                            Op::MirrorVertical,
                        ),
                    ],
                );
            });

        ui.add_space(10.0);

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;

            ui.add_enabled_ui(state.edits.can_undo(), |ui| {
                if icons::button(ui, icons, Icon::Undo2, "Undo (Ctrl+Z)").clicked() {
                    action = Some(Action::Undo);
                }
            });
            ui.add_enabled_ui(state.edits.can_redo(), |ui| {
                if icons::button(ui, icons, Icon::Redo2, "Redo (Ctrl+Y)").clicked() {
                    action = Some(Action::Redo);
                }
            });

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.colored_label(theme::TEXT_MUTED, changes_summary(state.edits));
            });
        });
    });

    action
}

fn export(ui: &mut egui::Ui, state: &mut State<'_>) -> Option<Action> {
    let mut action = None;

    section(ui, "Export");

    ui.add_enabled_ui(state.has_image, |ui| {
        egui::ComboBox::from_id_salt("export_format")
            .selected_text(state.settings.format.label())
            .width(ui.available_width())
            .show_ui(ui, |ui| {
                for format in Format::ALL {
                    ui.selectable_value(&mut state.settings.format, format, format.label());
                }
            });

        // Only where it means something: a quality slider on a lossless format is a
        // control that does nothing, which is worse than no control.
        if state.settings.format.is_lossy() {
            ui.add_space(6.0);
            ui.add(
                egui::Slider::new(&mut state.settings.quality, 1..=100)
                    .text("Quality")
                    .clamping(egui::SliderClamping::Always),
            );
        }

        ui.add_space(6.0);
        ui.add(
            egui::Slider::new(&mut state.settings.scale_percent, 5..=200)
                .text("Scale %")
                .clamping(egui::SliderClamping::Always),
        );

        if let Some(size) = state.edited_size {
            let (w, h) = state.settings.size_after(size);
            ui.add_space(2.0);
            ui.colored_label(theme::TEXT_MUTED, format!("{w} × {h} px"));
        }

        ui.add_space(10.0);

        // Say which of the two things the button will do. "Save" over a file it is
        // about to replace and "Save" into a file that does not exist yet are very
        // different promises.
        let converting = state.source_format != Some(state.settings.format);
        let resizing = state.settings.scale_percent != 100;
        let label = if converting || resizing {
            "Save as…"
        } else {
            "Save"
        };

        let button = egui::Button::new(egui::RichText::new(label).strong())
            .fill(theme::ACCENT_COLOR.gamma_multiply(0.9));
        if ui
            .add_sized([ui.available_width(), 30.0], button)
            .on_hover_text(if converting || resizing {
                "Write a new file (Ctrl+S)"
            } else {
                "Overwrite the original file (Ctrl+S)"
            })
            .clicked()
        {
            action = Some(Action::Save);
        }
    });

    action
}

/// A quiet heading, so the two halves of the sidebar read as two halves.
fn section(ui: &mut egui::Ui, title: &str) {
    ui.colored_label(theme::TEXT_MUTED, egui::RichText::new(title).small());
    ui.add_space(6.0);
}

fn changes_summary(edits: &Edits) -> String {
    match edits.len() {
        0 => "No changes".to_owned(),
        1 => "1 change".to_owned(),
        n => format!("{n} changes"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_change_count_reads_as_english() {
        let mut edits = Edits::default();
        assert_eq!(changes_summary(&edits), "No changes");

        edits.push(Op::RotateCw);
        assert_eq!(changes_summary(&edits), "1 change");

        edits.push(Op::FlipVertical);
        assert_eq!(changes_summary(&edits), "2 changes");
    }
}
