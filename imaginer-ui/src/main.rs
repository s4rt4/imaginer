// Release builds are GUI apps — no console window flashing up when Explorer
// launches us. Debug builds keep the console so tracing output is visible.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod cli;
mod clipboard;
mod collector;
mod console;
mod icons;
mod idle;
mod logo;
mod prefetch;
mod shader;
mod startup;
mod texture;
mod theme;
mod titlebar;
mod vector;
mod views;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::mpsc;
use std::time::Instant;

use imaginer_core::{Decoded, decode_full, decode_full_static, decode_preview, decode_thumb};

/// What the decode thread sends back to the UI.
pub enum LoadMessage {
    Loaded(Decoded),
    Failed(String),
}

fn main() -> ExitCode {
    let launched_at = Instant::now();

    // Reading the arguments has to come first, because they decide whether a window
    // is wanted at all — `--convert` must not pay for one. It costs microseconds
    // over the `args_os().nth(1)` this replaced, so the decode spawn below is still
    // effectively the first thing that happens on a viewer launch.
    let launch = match cli::parse(std::env::args_os()) {
        Ok(launch) => launch,
        Err(message) => {
            console::attach_to_parent();
            return cli::report_usage_error(&message);
        }
    };

    let path = match launch {
        cli::Launch::Convert(request) => {
            // Borrow the launching terminal's console, if there is one, so the
            // per-file report is visible. From Explorer there is none and the run
            // stays silent unless something fails.
            console::attach_to_parent();
            return cli::run(request);
        }
        cli::Launch::Viewer(path) => path,
    };

    // Everything below this point is deliberate ordering, not style.
    //
    // The decode thread starts before the window exists. Creating the window and
    // initialising the GL context costs 50-150ms; running the decode alongside it
    // means the image is usually ready by the time we paint the first frame, so
    // there is no empty window and no spinner. Anything added to `main` above this
    // spawn directly delays the image appearing.
    let (tx, rx) = mpsc::channel();
    if let Some(path) = path.clone() {
        std::thread::Builder::new()
            .name("decode".to_owned())
            .spawn(move || decode_into(path, &tx))
            .expect("failed to spawn decode thread");
    }

    // Constructed here rather than at the top of `main` so it stays below the decode
    // spawn. Its marks are still measured from `launched_at`, so nothing is lost.
    let trace = startup::StartupTrace::new(launched_at);
    trace.mark("decode_spawned");
    startup::install_init_logger(launched_at);

    let options = eframe::NativeOptions {
        // The icon was A/B'd against no icon at all: ~20ms apart over 15 runs each,
        // inside the run-to-run spread. It is free.
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 720.0])
            .with_min_inner_size([420.0, 300.0])
            .with_title("Imaginer")
            .with_icon(logo::window_icon())
            .with_drag_and_drop(true),
        renderer: renderer_choice(),
        glow_options: eframe::egui_glow::GlowConfiguration {
            hardware_acceleration: hardware_acceleration_choice(),
            vsync: !flag_disabled("IMAGINER_VSYNC"),
            ..Default::default()
        },
        ..Default::default()
    };

    // Everything between this mark and `context_ready` is winit + glutin + the GPU
    // driver, with no code of ours running in it.
    trace.mark("run_native");

    let result = eframe::run_native(
        "Imaginer",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, trace, path, rx)))),
    );

    match result {
        Ok(()) => ExitCode::SUCCESS,
        // The window never opened, so there is nothing on screen to carry the
        // message and a GUI-subsystem build has no console either.
        Err(err) => cli::report_usage_error(&format!("could not start: {err}")),
    }
}

/// Which renderer to ask eframe for.
///
/// Creating the graphics context dominates startup — measured at roughly 860ms of
/// a 1030ms launch on the hybrid-GPU laptop this was built on — so the backend
/// choice is worth more than everything else combined. glow beat wgpu 1032ms vs
/// 2648ms there, hence the default. Build with `--features compare-renderers` and
/// set `IMAGINER_RENDERER=wgpu` to re-run that comparison on other hardware.
fn renderer_choice() -> eframe::Renderer {
    match std::env::var("IMAGINER_RENDERER").as_deref() {
        #[cfg(feature = "compare-renderers")]
        Ok("wgpu") => eframe::Renderer::Wgpu,
        _ => eframe::Renderer::Glow,
    }
}

/// Whether to ask for a hardware-accelerated GL context.
///
/// An investigation knob, not a setting anyone should need: `IMAGINER_HW_ACCEL=off`
/// selects a software context, which skips loading the vendor's OpenGL driver
/// entirely. Comparing that against the default is how the cost of context creation
/// gets attributed to the driver rather than to winit or glutin. Rendering through
/// it is far too slow to actually use.
fn hardware_acceleration_choice() -> eframe::egui_glow::HardwareAcceleration {
    use eframe::egui_glow::HardwareAcceleration;
    match std::env::var("IMAGINER_HW_ACCEL").as_deref() {
        Ok("off") => HardwareAcceleration::Off,
        Ok("required") => HardwareAcceleration::Required,
        _ => HardwareAcceleration::Preferred,
    }
}

/// True when `name` is set to an explicit "off" value. Distinct from unset, so that
/// the default stays on.
fn flag_disabled(name: &str) -> bool {
    matches!(std::env::var(name).as_deref(), Ok("0") | Ok("false"))
}

/// Decode in two passes: the embedded EXIF thumbnail first if there is one, then
/// the real image. The viewer shows whichever arrives first.
///
/// The EXIF thumbnail is not the only fast lane: files that carry none —
/// screenshots, PNGs, downloads — get one from the on-disk thumbnail cache,
/// written by an earlier session's decode of the very same file. And for an
/// animated file the full decode is itself split in two, for the same reason
/// the thumbnail is: a GIF's whole frame set can take far longer to decode than
/// its first frame, and a window that waits for all of it before painting
/// anything is exactly what this startup exists to avoid. So the still arrives
/// as an ordinary `Loaded` and plays nothing; the frame set follows as a second
/// message for the same image, which the viewer swaps in without touching
/// layout.
fn decode_into(path: PathBuf, tx: &mpsc::Sender<LoadMessage>) {
    if let Some(preview) = decode_preview(&path).or_else(|| decode_thumb(&path)) {
        // A dropped receiver just means the window closed; stop rather than
        // spending time on a full decode nobody will see.
        if tx.send(LoadMessage::Loaded(preview)).is_err() {
            return;
        }
    }

    let still = decode_full_static(&path);
    let message = match &still {
        Ok(decoded) => LoadMessage::Loaded(decoded.clone()),
        Err(err) => LoadMessage::Failed(err.to_string()),
    };
    if tx.send(message).is_err() {
        return;
    }

    // This decode is the moment a thumbnail gets written for next time: a
    // background thread is already holding the full pixels, and the marginal
    // cost is one 256px encode. The EXIF thumbnail of a camera file may already
    // have served; a cached one serves every file the same way.
    if let Ok(decoded) = &still {
        imaginer_core::thumbs::ensure(&path, &decoded.pixels);
    }

    if let Ok(animated) = decode_full(&path)
        && animated.animation.is_some()
    {
        let _ = tx.send(LoadMessage::Loaded(animated));
    }
}
