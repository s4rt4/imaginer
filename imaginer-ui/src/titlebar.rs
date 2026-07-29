//! Dark window chrome.
//!
//! The title bar belongs to the shell, not to us, so it follows the *system*
//! theme — which on a light-mode machine leaves a white bar sitting on top of a
//! near-black app. DWM window attributes are the supported way to say otherwise
//! without going frameless and reimplementing drag, resize, snap layouts and
//! double-click-to-maximise ourselves.
//!
//! The obvious attribute for the job, `DWMWA_USE_IMMERSIVE_DARK_MODE`, is *not*
//! what this uses. Setting it made the canvas flicker black whenever the window
//! repainted — confirmed on 2026-07-29 by A/B'ing the call on this machine, and it
//! only showed with vsync on, so it is a compositor-level fight over presentation
//! rather than anything we draw. `DWMWA_CAPTION_COLOR` states the colour outright
//! instead of switching DWM into a different mode, and it also lets the chrome be
//! exactly the panel colour rather than the shell's generic dark grey.

use eframe::egui::Color32;
use raw_window_handle::HasWindowHandle;

/// Colour this window's title bar to match the app.
///
/// Best-effort by design: it is cosmetic, the attributes need Windows 11 22H2, and
/// every failure mode leaves a perfectly usable window with a system-coloured title
/// bar. Nothing here is worth interrupting startup for.
///
/// `IMAGINER_DARK_TITLEBAR=0` skips it. Touching a DWM attribute changes how the
/// compositor treats the window, which — see above — makes this the first thing to
/// rule out whenever presentation misbehaves, the same reason `IMAGINER_VSYNC` exists.
pub fn recolour(window: &impl HasWindowHandle, caption: Color32, text: Color32) {
    if matches!(
        std::env::var("IMAGINER_DARK_TITLEBAR").as_deref(),
        Ok("0") | Ok("false")
    ) {
        return;
    }

    #[cfg(windows)]
    windows::recolour(window, caption, text);

    #[cfg(not(windows))]
    let _ = (window, caption, text);
}

#[cfg(windows)]
mod windows {
    use eframe::egui::Color32;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::Graphics::Dwm::{
        DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR, DWMWINDOWATTRIBUTE,
        DwmSetWindowAttribute,
    };

    pub fn recolour(window: &impl HasWindowHandle, caption: Color32, text: Color32) {
        let Ok(handle) = window.window_handle() else {
            return;
        };
        let RawWindowHandle::Win32(win32) = handle.as_raw() else {
            return;
        };
        let hwnd = win32.hwnd.get() as *mut std::ffi::c_void;

        set_colour(hwnd, DWMWA_CAPTION_COLOR, caption);
        set_colour(hwnd, DWMWA_TEXT_COLOR, text);
        // Without this the border stays the system accent, which reads as a bright
        // hairline around an otherwise dark window.
        set_colour(hwnd, DWMWA_BORDER_COLOR, caption);
    }

    fn set_colour(hwnd: *mut std::ffi::c_void, attribute: DWMWINDOWATTRIBUTE, colour: Color32) {
        // COLORREF is 0x00BBGGRR — the reverse of every other colour on this side of
        // the codebase, and silently wrong rather than an error if you get it round
        // the wrong way.
        let colorref =
            u32::from(colour.r()) | (u32::from(colour.g()) << 8) | (u32::from(colour.b()) << 16);

        // SAFETY: `hwnd` comes from a live window handle borrowed for this call, and
        // the attribute buffer is a `COLORREF` described by its own size, which is
        // what all three colour attributes expect. A version of Windows too old to
        // know the attribute returns an error, which is nothing to act on.
        unsafe {
            DwmSetWindowAttribute(
                hwnd,
                attribute as u32,
                std::ptr::from_ref(&colorref).cast(),
                size_of::<u32>() as u32,
            );
        }
    }
}
