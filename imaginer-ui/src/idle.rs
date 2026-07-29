//! When floating chrome should be on screen.
//!
//! The canvas chevrons and the fullscreen toolbar both answer the same question —
//! has the pointer moved recently? — so they ask it in one place and cannot drift
//! into disagreeing about it.

use std::time::Duration;

use eframe::egui;

/// How long the pointer sits still before floating chrome starts to go.
///
/// Long enough to reach for a control after stopping, short enough that it is not
/// still sitting over the photograph while you look at it.
const HOLD: f32 = 1.6;

/// How long the fade itself takes.
const FADE: f32 = 0.4;

/// How visible floating chrome should be this frame, from 0 to 1.
///
/// This also schedules the repaints the fade needs. egui is event-driven, so
/// without them the chrome would stop at whatever opacity the last mouse movement
/// left it at and only finish fading when something unrelated caused a frame.
pub fn opacity(ctx: &egui::Context) -> f32 {
    let still_for = ctx.input(|i| i.pointer.time_since_last_movement());

    // Infinite before the pointer has ever moved — an image opened from Explorer
    // and left alone should show no chrome at all.
    if !still_for.is_finite() {
        return 0.0;
    }

    if still_for <= HOLD {
        // Wake up exactly when the fade is due, so it starts on time instead of
        // waiting for the next thing that happens to cause a frame.
        ctx.request_repaint_after(Duration::from_secs_f32(HOLD - still_for));
        return 1.0;
    }

    let gone = ((still_for - HOLD) / FADE).clamp(0.0, 1.0);
    if gone < 1.0 {
        // Mid-fade: this is an animation, and it wants every frame it can get.
        ctx.request_repaint();
    }

    1.0 - gone
}
