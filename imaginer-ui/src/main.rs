// Release builds are GUI apps — no console window flashing up when Explorer
// launches us. Debug builds keep the console so tracing output is visible.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod startup;
mod texture;
mod theme;
mod views;

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Instant;

use imaginer_core::{Decoded, decode_full, decode_preview};

/// What the decode thread sends back to the UI.
pub enum LoadMessage {
    Loaded(Decoded),
    Failed(String),
}

fn main() -> eframe::Result {
    let launched_at = Instant::now();

    // Everything below this point is deliberate ordering, not style.
    //
    // The decode thread starts before the window exists. Creating the window and
    // initialising the GL context costs 50-150ms; running the decode alongside it
    // means the image is usually ready by the time we paint the first frame, so
    // there is no empty window and no spinner. Anything added to `main` above this
    // spawn directly delays the image appearing.
    let path = std::env::args_os().nth(1).map(PathBuf::from);
    let (tx, rx) = mpsc::channel();
    if let Some(path) = path.clone() {
        std::thread::Builder::new()
            .name("decode".to_owned())
            .spawn(move || decode_into(path, &tx))
            .expect("failed to spawn decode thread");
    }

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 720.0])
            .with_min_inner_size([420.0, 300.0])
            .with_title("Imaginer")
            .with_drag_and_drop(true),
        renderer: renderer_choice(),
        ..Default::default()
    };

    eframe::run_native(
        "Imaginer",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, launched_at, path, rx)))),
    )
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

/// Decode in two passes: the embedded EXIF thumbnail first if there is one, then
/// the real image. The viewer shows whichever arrives first.
fn decode_into(path: PathBuf, tx: &mpsc::Sender<LoadMessage>) {
    if let Some(preview) = decode_preview(&path) {
        // A dropped receiver just means the window closed; stop rather than
        // spending time on a full decode nobody will see.
        if tx.send(LoadMessage::Loaded(preview)).is_err() {
            return;
        }
    }

    let message = match decode_full(&path) {
        Ok(decoded) => LoadMessage::Loaded(decoded),
        Err(err) => LoadMessage::Failed(err.to_string()),
    };
    let _ = tx.send(message);
}
