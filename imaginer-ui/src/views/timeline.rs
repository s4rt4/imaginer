//! The floating playback bar for animated files: play/pause and a frame scrubber.
//!
//! It lives on the canvas rather than in a panel for the same reason the chevrons
//! do — playback belongs to the thing being watched, not to the furniture around
//! it — and it comes and goes with the chrome, so fullscreen stays just the
//! picture until the pointer moves.

use eframe::egui;

use crate::icons::{self, Icon, Icons};
use crate::theme;

/// What the bar needs to know to draw itself.
#[derive(Debug, Clone, Copy)]
pub struct Playback {
    /// Frame on screen right now, zero-based.
    pub frame: usize,
    pub count: usize,
    /// Whether frames are advancing on their own.
    pub playing: bool,
}

/// What the user did to the bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    TogglePlay,
    /// A drag of the slider began; the app should hold playback while it lasts.
    ScrubStart,
    /// The slider moved to this frame.
    Scrub(usize),
    /// The drag ended; the app decides whether to resume.
    ScrubEnd,
}

/// Gap between the bar and the bottom edge of the canvas.
const BOTTOM_INSET: f32 = 16.0;

/// Height of the whole bar, backdrop included.
const BAR_HEIGHT: f32 = 34.0;

/// Width of the play/pause button's square.
const BUTTON_SIDE: f32 = 26.0;

/// Width of the scrubber track at full window width; shrunk rather than wrapped
/// when the window cannot spare it.
const TRACK_WIDTH: f32 = 260.0;

pub fn show(
    ui: &mut egui::Ui,
    icons: &mut Icons,
    canvas: egui::Rect,
    state: Playback,
    visible: bool,
) -> Option<Action> {
    let mut action = None;

    // A one-frame animation has nothing to scrub and nothing to pause.
    if state.count < 2 {
        return None;
    }

    // Shrink the track before letting the bar leave its canvas.
    let chrome = BUTTON_SIDE + 12.0 + 70.0 + 24.0;
    let track = TRACK_WIDTH.min((canvas.width() - chrome).max(80.0));
    let width = BUTTON_SIDE + 12.0 + track + 70.0;
    let rect = egui::Rect::from_min_size(
        egui::pos2(
            canvas.center().x - width * 0.5,
            canvas.bottom() - BAR_HEIGHT - BOTTOM_INSET,
        ),
        egui::vec2(width, BAR_HEIGHT),
    );

    // A pointer resting where the bar was holds it open — the same contract as
    // the chevrons. Without it the bar would vanish from under the cursor, and
    // the control you were reaching for would stop existing.
    let pointer = ui.input(|i| i.pointer.hover_pos());
    if !visible && !pointer.is_some_and(|at| rect.contains(at)) {
        return None;
    }

    ui.painter().rect_filled(
        rect,
        rect.height() * 0.5,
        theme::PANEL_BG.gamma_multiply(0.85),
    );

    let mut bar = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    bar.style_mut().spacing.item_spacing.x = 8.0;
    // Indent past the rounded end of the backdrop.
    bar.add_space(6.0);

    let (icon, tooltip) = if state.playing {
        (Icon::Pause, "Pause (Space)")
    } else {
        (Icon::Play, "Play (Space)")
    };
    if icons::button(&mut bar, icons, icon, tooltip).clicked() {
        action = Some(Action::TogglePlay);
    }

    let mut frame = state.frame;
    let slider = egui::Slider::new(&mut frame, 0..=state.count - 1).show_value(false);
    let response = bar.add_sized(egui::vec2(track, bar.available_height()), slider);

    if response.drag_started() {
        action = Some(Action::ScrubStart);
    }
    if response.changed() {
        action = Some(Action::Scrub(frame));
    }
    if response.drag_stopped() {
        action = Some(Action::ScrubEnd);
    }

    // "12 / 48", so the position reads without doing arithmetic.
    bar.monospace(format!("{} / {}", state.frame + 1, state.count));

    action
}
