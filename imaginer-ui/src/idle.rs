//! When floating chrome should be on screen.
//!
//! The canvas chevrons and the fullscreen toolbar both answer the same question —
//! has the pointer moved recently? — so they ask it in one place and cannot drift
//! into disagreeing about it.
//!
//! **This deliberately does not fade.** The first version animated the chrome out
//! over 0.4s, which meant a burst of ~24 frames every time the pointer stopped, and
//! on this machine that brought back the black canvas flicker that
//! [`crate::titlebar`] describes: a window whose caption DWM is colouring does not
//! survive a stream of back-to-back presents. A/B'd on 2026-07-29 — dark title bar
//! off, no flicker; fade off, no flicker; both on, flicker. The fade was the
//! smaller loss, so it went. What is left asks for exactly one frame per
//! transition. Do not reintroduce an animation here without re-testing that.

use std::time::Duration;

use eframe::egui;

/// How long the pointer sits still before floating chrome goes.
///
/// Long enough to reach for a control after stopping, short enough that it is not
/// still sitting over the photograph while you look at it.
const HOLD: f32 = 1.6;

/// Whether floating chrome should be on screen this frame.
///
/// Also schedules the single repaint that hides it. egui is event-driven, so
/// without that the chrome would stay up until something unrelated happened to
/// cause a frame.
pub fn visible(ctx: &egui::Context) -> bool {
    let still_for = ctx.input(|i| i.pointer.time_since_last_movement());

    // Infinite before the pointer has ever moved — an image opened from Explorer
    // and left alone should show no chrome at all.
    if !still_for.is_finite() {
        return false;
    }

    if still_for <= HOLD {
        // Wake up exactly when it is due to go, rather than waiting for the next
        // thing that happens to cause a frame.
        ctx.request_repaint_after(Duration::from_secs_f32(HOLD - still_for));
        return true;
    }

    false
}
