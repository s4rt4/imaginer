//! The settings panel: the shortcuts, and the few choices worth remembering
//! between sessions.
//!
//! Two halves, in the order a newcomer needs them. **Shortcuts** is a reference
//! — every binding the app answers to, in one place, so nothing has to be
//! guessed or looked up in a readme. **Preferences** is the settings that
//! persist: small, stable choices like how long a slide holds, written to disk
//! the moment they change.
//!
//! It shares the right-hand strip with the edit sidebar and the info panel —
//! one at a time, the same rule the other two obey — and opens with `S` or the
//! toolbar's gear.

use eframe::egui;
use imaginer_core::{Order, SortKey};

use crate::icons::{self, Icon, Icons};

/// Wider than the edit sidebar it shares the strip with: the shortcut table
/// needs a monospace key column and its descriptions side by side, and at 244
/// both were truncated into nonsense.
pub const WIDTH: f32 = 300.0;

/// What the panel can ask the app to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Close,
    /// The slideshow slider moved. Applied live, saved when the drag ends.
    SetSlideshow(u32),
    /// The slider was let go (or stepped by keyboard) — write it down.
    CommitSlideshow,
    /// A sort choice was picked; applied and written down at once.
    SetOrder(Order),
}

/// Every binding the app answers to, as shown. Kept beside
/// `handle_shortcuts` by review, not by force — a test cannot read a human
/// label, but drift between the two halves of this table is at least a
/// two-minute read to spot.
const SHORTCUTS: &[(&str, &str)] = &[
    ("Ctrl+O", "Open an image"),
    ("Ctrl+Shift+O", "Open a folder"),
    ("← / →", "Previous / next image"),
    ("Space", "Slideshow / animation"),
    ("F11", "Fullscreen"),
    ("Esc", "Back out, or close"),
    ("F or 0", "Fit to window"),
    ("1", "Actual size"),
    ("+ / −", "Zoom in / out"),
    ("Ctrl+C", "Copy this image"),
    ("V", "Paste an image or path"),
    ("P", "Copy the file path"),
    ("Del", "Move to Recycle Bin"),
    ("R", "Rotate clockwise"),
    ("C", "Crop"),
    ("E", "Edit sidebar"),
    ("I", "File and EXIF info"),
    ("S", "This panel"),
    ("Ctrl+S", "Save"),
    ("Ctrl+Z / Ctrl+Y", "Undo / redo"),
];

pub struct State<'a> {
    /// Written through live while the slider drags; committed on release.
    pub slideshow_secs: &'a mut u32,
    /// The order new sessions (and the toolbar menu) start from.
    pub order: Order,
}

pub fn show(ui: &mut egui::Ui, icons: &mut Icons, state: &mut State<'_>) -> Option<Action> {
    let mut action = None;

    egui::Frame::NONE
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Settings").strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icons::button(ui, icons, Icon::X, "Close (S or Esc)").clicked() {
                        action = Some(Action::Close);
                    }
                });
            });
            ui.label(
                egui::RichText::new(concat!("Imaginer ", env!("CARGO_PKG_VERSION")))
                    .weak()
                    .small(),
            );
            ui.add_space(8.0);

            // The two halves together stand taller than the window: without the
            // scroll the last rows of the shortcut table were simply gone.
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if let Some(requested) = preferences(ui, state) {
                        action = Some(requested);
                    }
                    ui.add_space(14.0);
                    shortcuts(ui);
                });
        });

    action
}

fn preferences(ui: &mut egui::Ui, state: &mut State<'_>) -> Option<Action> {
    let mut action = None;

    ui.label(egui::RichText::new("Slideshow").strong());
    ui.add_space(4.0);
    let slider = egui::Slider::new(state.slideshow_secs, 1..=60).suffix(" s per image");
    let response = ui.add(slider);
    if response.changed() {
        action = Some(Action::SetSlideshow(*state.slideshow_secs));
    }
    if response.drag_stopped() {
        action = Some(Action::CommitSlideshow);
    }

    ui.add_space(10.0);
    ui.label(egui::RichText::new("Sort folders by").strong());
    ui.add_space(4.0);
    for key in SortKey::ALL {
        if ui.radio(state.order.key == key, key.label()).clicked() {
            action = Some(Action::SetOrder(Order { key, ..state.order }));
        }
    }
    // The same wording the toolbar's sort menu uses, because it is the same
    // choice and two words for it would be one more thing to keep straight.
    if ui
        .checkbox(&mut state.order.descending, "Reverse")
        .changed()
    {
        action = Some(Action::SetOrder(state.order));
    }

    action
}

fn shortcuts(ui: &mut egui::Ui) {
    ui.label(egui::RichText::new("Shortcuts").strong());
    ui.add_space(4.0);

    egui::Grid::new("shortcuts")
        .num_columns(2)
        .spacing([10.0, 3.0])
        .show(ui, |ui| {
            for (keys, description) in SHORTCUTS {
                ui.monospace(*keys);
                ui.label(*description);
                ui.end_row();
            }
        });
}
