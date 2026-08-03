//! The info panel: what this file is, and what the camera recorded about it.
//!
//! Read-only, and that is what separates it from the edit sidebar rather than any
//! difference in where it sits. Both are right-hand surfaces and only one can be
//! open at a time — two would leave the photograph a strip down the middle.
//!
//! Every row here comes out of the file. Nothing is computed, nothing is guessed,
//! and a tag the file does not carry produces no row at all: a panel of "unknown"
//! against every label reads as a broken reader rather than as an unlabelled photo.

use std::path::Path;

use eframe::egui;
use imaginer_core::metadata::Info;

use crate::icons::{self, Icon, Icons};
use crate::theme;
use crate::views::statusbar::human_size;

/// Panel width. Wider than the edit sidebar, because this one holds prose — lens
/// names and file paths — rather than rows of buttons sized to their icons.
pub const WIDTH: f32 = 268.0;

/// Width of the label column. Fixed, so the values line up into a column of their
/// own instead of starting at a different place on every row.
const LABEL_WIDTH: f32 = 92.0;

/// What the panel can ask the app to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Close,
}

pub struct State<'a> {
    /// Facts about the file that the app already knows, so the panel does not go
    /// back to disk for anything the status bar has read.
    pub file: Vec<(&'static str, String)>,
    /// What the EXIF block held, or an empty `Info` for a file with none.
    pub exif: &'a Info,
    /// False while the image is still decoding, which is the one case where an
    /// empty panel means "not yet" rather than "nothing here".
    pub ready: bool,
}

pub fn show(ui: &mut egui::Ui, icons: &mut Icons, state: &State<'_>) -> Option<Action> {
    let mut action = None;

    egui::Frame::NONE
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Info").strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icons::button(ui, icons, Icon::X, "Close the info panel (I)").clicked() {
                        action = Some(Action::Close);
                    }
                });
            });
            ui.add_space(8.0);

            // Scrolled, because a raw file off a modern camera fills this several
            // times over and the panel cannot grow past the window.
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| body(ui, state));
        });

    action
}

fn body(ui: &mut egui::Ui, state: &State<'_>) {
    if !state.file.is_empty() {
        rows(ui, "File", &state.file);
    }

    for section in &state.exif.sections {
        ui.add_space(14.0);
        rows(ui, section.title, &section.rows);
    }

    // Said once, at the bottom, rather than as a section of its own: most files
    // this app opens are screenshots and exports, and EXIF is the exception rather
    // than the thing that is missing.
    if state.ready && state.exif.is_empty() {
        ui.add_space(14.0);
        ui.colored_label(theme::TEXT_MUTED, "No EXIF metadata in this file");
    }
}

fn rows(ui: &mut egui::Ui, title: &str, rows: &[(&'static str, String)]) {
    ui.colored_label(theme::TEXT_MUTED, egui::RichText::new(title).small());
    ui.add_space(4.0);

    // A grid rather than two labels per line, so the values share one left edge.
    // `striped` is deliberately off: the rows are short and the banding would put
    // more contrast into the panel than the photograph beside it has.
    egui::Grid::new(format!("info_{title}"))
        .num_columns(2)
        .spacing([10.0, 5.0])
        .min_col_width(LABEL_WIDTH)
        .max_col_width(LABEL_WIDTH)
        .show(ui, |ui| {
            for (label, value) in rows {
                ui.colored_label(theme::TEXT_MUTED, *label);
                // Wrapped rather than truncated, and selectable, because the long
                // ones — a folder path, a lens name, a coordinate pair — are exactly
                // the ones somebody opens this panel to copy.
                ui.add(egui::Label::new(value).wrap());
                ui.end_row();
            }
        });
}

/// The File section: what the app already knows without reading anything again.
///
/// Built here rather than in the app so it can be tested, and so the rules about
/// what to leave out live beside the panel that would otherwise show the gaps. A
/// pasted image has no path at all, which is a state to describe rather than to
/// paper over with an invented filename.
pub fn file_facts(
    path: Option<&Path>,
    file_size: Option<u64>,
    dimensions: Option<(u32, u32)>,
) -> Vec<(&'static str, String)> {
    let mut facts = Vec::new();

    match path {
        Some(path) => {
            if let Some(name) = path.file_name() {
                facts.push(("Name", name.to_string_lossy().into_owned()));
            }
            // The folder, not the whole path: the name is already on the row above,
            // and repeating it makes the longest row in the panel longer still.
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                facts.push(("Folder", parent.display().to_string()));
            }
        }
        // The same words the status bar uses for it, so the two never disagree
        // about what is on screen.
        None => facts.push(("Name", "Unsaved image".to_owned())),
    }

    // Both are absent until there is something to report — no file on disk means no
    // size, and no decode yet means no dimensions.
    if let Some(bytes) = file_size {
        facts.push(("Size", format!("{} ({bytes} bytes)", human_size(bytes))));
    }
    if let Some((w, h)) = dimensions {
        facts.push(("Dimensions", format!("{w} × {h} px")));
    }

    facts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(facts: &[(&'static str, String)]) -> Vec<&'static str> {
        facts.iter().map(|(label, _)| *label).collect()
    }

    #[test]
    fn a_file_is_described_by_its_name_folder_size_and_shape() {
        let facts = file_facts(
            Some(Path::new(r"C:\photos\holiday.jpg")),
            Some(2048),
            Some((4000, 3000)),
        );

        assert_eq!(labels(&facts), ["Name", "Folder", "Size", "Dimensions"]);
        assert_eq!(facts[0].1, "holiday.jpg");
        assert_eq!(facts[1].1, r"C:\photos");
        // Both spellings: the human one to read, the exact one to compare against
        // whatever wrote the file.
        assert_eq!(facts[2].1, "2.0 KB (2048 bytes)");
        assert_eq!(facts[3].1, "4000 × 3000 px");
    }

    #[test]
    fn pixels_from_the_clipboard_have_no_file_to_describe() {
        let facts = file_facts(None, None, Some((16, 16)));

        assert_eq!(labels(&facts), ["Name", "Dimensions"]);
        assert_eq!(facts[0].1, "Unsaved image");
    }

    #[test]
    fn nothing_is_claimed_before_the_decode_lands() {
        let facts = file_facts(Some(Path::new("bare.png")), None, None);

        // A relative path has a parent, but it is empty — a "Folder" row saying
        // nothing is worse than no row.
        assert_eq!(labels(&facts), ["Name"]);
    }
}
