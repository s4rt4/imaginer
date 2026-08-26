//! Verifies that the plain texture path — the one the viewer uses whenever no
//! colour adjustment is active — puts the file's own bytes on the screen.
//!
//! This was the last unverified assumption in the render pipeline. egui decides
//! the texture's internal GL format for us, and if it ever chose an sRGB format
//! while the framebuffer stayed linear (or the reverse), every image would
//! darken or lighten by a gamma curve — slowly, uniformly, and never enough to
//! look broken, only enough to look subtly wrong next to the same file in
//! another viewer. The adjustment shader's verification already exercises this
//! texture upload, but through the shader's own maths, which could in principle
//! cancel or compound a format surprise. This checks the unadjusted path alone.
//!
//! The acceptance test is the strictest useful one: a ramp covering the full
//! 0-255 range of every channel, uploaded through the app's real texture code
//! (`texture.rs`, included as source, with its real `TextureOptions` — nearest
//! magnification, linear minification with mipmaps), drawn the way
//! `viewer::show` draws an unadjusted image, read back with `glReadPixels`, and
//! compared byte for byte. At a 1:1 draw scale no filtering runs, so anything
//! above a rounding difference is a format problem.
//!
//! Deliberately not a `#[test]`: it needs a GL context. A window opens for well
//! under a second.
//!
//! `cargo run --release -p imaginer-ui --example verify-srgb`
//!
//! Exit status is 0 only if every case passed, so it can be a build step later.

// The module under test, included as source. `imaginer-ui` is a binary crate
// with no library target, so an example cannot `use` its modules — and a copy
// of the uploader would test the copy. The example only calls one of its
// functions, hence the allow.
#[allow(dead_code)]
#[path = "../src/texture.rs"]
mod texture;

use std::process::ExitCode;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use eframe::egui;
use eframe::glow::{self, HasContext as _};
use imaginer_core::image::{Rgba, RgbaImage};

use texture::ImageTexture;

/// Texels a side in the test image. 16 gives every channel a 17-step ramp that
/// reaches exactly 255 — the whole byte range, including the ends where a gamma
/// error shows most.
const GRID: u32 = 16;

/// Points each texel is drawn as. Exactly 1:1, which is what makes the check
/// sharp: no filtering runs at native scale, so each read texel must be the
/// uploaded one, not an average of its neighbours.
const BLOCK: f32 = 8.0;

/// Where the image sits inside the canvas — deliberately not the origin, so a
/// quad positioned from the viewport instead of its rectangle cannot hide.
const INSET: egui::Vec2 = egui::vec2(37.0, 23.0);

/// What the canvas is painted with before the image is drawn. Nothing outside
/// the image rectangle may be anything else.
const BACKDROP: egui::Color32 = egui::Color32::from_rgb(9, 11, 13);

/// A channel may differ by this much. Identity through an unfiltered 1:1 draw
/// should be exact; 1 leaves room for the last bit of a float conversion.
const TOLERANCE: u8 = 1;

/// The image under test: red runs with the column, green with the row, blue is
/// `x ^ y` — so a flipped, mirrored or shifted quad cannot match, and every one
/// of the 256 ramp steps of red and green is visited.
fn source() -> RgbaImage {
    RgbaImage::from_fn(GRID, GRID, |x, y| {
        Rgba([(x * 17) as u8, (y * 17) as u8, ((x ^ y) * 17) as u8, 255])
    })
}

fn image_rect(canvas: egui::Rect) -> egui::Rect {
    egui::Rect::from_min_size(canvas.min + INSET, egui::Vec2::splat(GRID as f32 * BLOCK))
}

/// One frame of the window, as the GPU wrote it.
struct Readback {
    /// RGBA8, bottom row first, exactly the viewport the callback was given.
    pixels: Vec<u8>,
    width: i32,
    height: i32,
    pixels_per_point: f32,
    canvas: egui::Rect,
}

impl Readback {
    /// The pixel covering a point given in egui's screen points.
    fn at(&self, point: egui::Pos2) -> Option<[u8; 4]> {
        let scale = |value: f32| (self.pixels_per_point * value).round() as i32;
        let x = scale(point.x) - scale(self.canvas.left());
        let from_top = scale(point.y) - scale(self.canvas.top());

        if x < 0 || x >= self.width || from_top < 0 || from_top >= self.height {
            return None;
        }

        // glReadPixels hands back the bottom row first.
        let row = self.height - 1 - from_top;
        let start = ((row * self.width + x) * 4) as usize;
        Some([
            self.pixels[start],
            self.pixels[start + 1],
            self.pixels[start + 2],
            self.pixels[start + 3],
        ])
    }
}

/// A callback, queued straight after the image draw, that copies the
/// framebuffer out while the frame is still being built.
fn readback(canvas: egui::Rect, slot: Arc<Mutex<Option<Readback>>>) -> egui::PaintCallback {
    egui::PaintCallback {
        rect: canvas,
        callback: Arc::new(eframe::egui_glow::CallbackFn::new(move |info, painter| {
            let gl = painter.gl();
            let viewport = info.viewport_in_pixels();
            if viewport.width_px <= 0 || viewport.height_px <= 0 {
                return;
            }

            let mut pixels = vec![0u8; (viewport.width_px * viewport.height_px * 4) as usize];
            // SAFETY: the painting thread, with the context current.
            unsafe {
                gl.pixel_store_i32(glow::PACK_ALIGNMENT, 1);
                gl.read_pixels(
                    viewport.left_px,
                    viewport.from_bottom_px,
                    viewport.width_px,
                    viewport.height_px,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelPackData::Slice(Some(&mut pixels)),
                );
                // Put back the default, so nothing downstream inherits our setting.
                gl.pixel_store_i32(glow::PACK_ALIGNMENT, 4);
            }

            *lock(&slot) = Some(Readback {
                pixels,
                width: viewport.width_px,
                height: viewport.height_px,
                pixels_per_point: info.pixels_per_point,
                canvas,
            });
        })),
    }
}

fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Default)]
struct Report {
    /// The largest difference from the source across every channel of every
    /// texel, and the first out-of-tolerance finding, if any.
    worst: u8,
    failure: Option<String>,
    fatal: Option<String>,
    /// False if the window closed before a frame was measured, which would
    /// otherwise read exactly like a pass.
    judged: bool,
}

struct Verify {
    uploaded: Option<ImageTexture>,
    source: RgbaImage,
    slot: Arc<Mutex<Option<Readback>>>,
    report: Arc<Mutex<Report>>,
    /// Frames to let the window settle before anything is measured. The first
    /// frame of a freshly created context is not a good thing to trust.
    warmup: u32,
    judged: bool,
}

impl Verify {
    fn new(cc: &eframe::CreationContext<'_>, report: Arc<Mutex<Report>>) -> Self {
        let source = source();
        // Through the app's own uploader — `upload_pixels`, the route an edit
        // result takes, which is also a plain `ColorImage` upload like the
        // decode path's.
        let uploaded = texture::upload_pixels(&cc.egui_ctx, "srgb-source", &source);

        Self {
            uploaded: Some(uploaded),
            source,
            slot: Arc::new(Mutex::new(None)),
            report,
            warmup: 2,
            judged: false,
        }
    }

    fn judge(&self, frame: &Readback) {
        let rect = image_rect(frame.canvas);
        let mut worst = 0u8;
        let mut failure = None;

        // Outside the image rectangle nothing may have been written.
        for (where_, point) in [
            ("above left of it", frame.canvas.min + egui::vec2(4.0, 4.0)),
            (
                "just past its right edge",
                egui::pos2(rect.right() + 4.0, rect.center().y),
            ),
            (
                "just below it",
                egui::pos2(rect.center().x, rect.bottom() + 4.0),
            ),
        ] {
            let Some(got) = frame.at(point) else {
                continue;
            };
            let backdrop = BACKDROP.to_array();
            if got[..3] != backdrop[..3] {
                failure.get_or_insert(format!(
                    "something painted outside the image rectangle, {where_}: \
                     expected the backdrop {:?}, read {:?}",
                    &backdrop[..3],
                    &got[..3]
                ));
            }
        }

        for y in 0..GRID {
            for x in 0..GRID {
                let point =
                    rect.min + egui::vec2((x as f32 + 0.5) * BLOCK, (y as f32 + 0.5) * BLOCK);
                let Some(got) = frame.at(point) else {
                    failure.get_or_insert(format!("texel {x},{y} landed outside the canvas"));
                    continue;
                };
                let want = self.source.get_pixel(x, y).0;

                for channel in 0..3 {
                    let difference = got[channel].abs_diff(want[channel]);
                    worst = worst.max(difference);
                    if difference > TOLERANCE {
                        failure.get_or_insert(format!(
                            "texel {x},{y}: uploaded {:?}, the screen holds {:?} — \
                             the texture path is not identity",
                            &want[..3],
                            &got[..3]
                        ));
                    }
                }
            }
        }

        let mut report = lock(&self.report);
        report.worst = worst;
        report.failure = failure;
        report.judged = true;
    }

    fn finish(&self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

impl eframe::App for Verify {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        ctx.request_repaint();

        if self.warmup > 0 {
            self.warmup -= 1;
            return;
        }

        // The capture happens during painting, after this function returned —
        // so the frame in the slot is the previous frame's, and judging it here
        // is safe.
        if let Some(frame) = lock(&self.slot).take() {
            self.judge(&frame);
            self.judged = true;
            self.finish(&ctx);
            return;
        }

        let Some(uploaded) = self.uploaded.as_ref() else {
            return;
        };

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let canvas = ui.available_rect_before_wrap();
                ui.painter().rect_filled(canvas, 0.0, BACKDROP);

                // Exactly how `viewer::show` draws an unadjusted image.
                ui.painter().image(
                    uploaded.handle.id(),
                    image_rect(canvas),
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
                ui.painter().add(readback(canvas, Arc::clone(&self.slot)));
            });
    }
}

fn main() -> ExitCode {
    let report = Arc::new(Mutex::new(Report::default()));

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([420.0, 300.0])
            .with_title("Imaginer — verifying the texture path")
            .with_resizable(false),
        // No multisampling, or the default framebuffer could not be read back.
        multisampling: 0,
        glow_options: eframe::egui_glow::GlowConfiguration {
            vsync: false,
            ..Default::default()
        },
        ..Default::default()
    };

    let started = eframe::run_native(
        "verify-srgb",
        options,
        Box::new({
            let report = Arc::clone(&report);
            move |cc| Ok(Box::new(Verify::new(cc, report)))
        }),
    );

    if let Err(err) = started {
        eprintln!("could not open a window: {err}");
        return ExitCode::FAILURE;
    }

    let report = lock(&report);
    if let Some(fatal) = &report.fatal {
        println!("  FATAL {fatal}");
        return ExitCode::FAILURE;
    }
    if !report.judged {
        println!("  FATAL nothing was measured; the window closed early");
        return ExitCode::FAILURE;
    }

    match &report.failure {
        None => {
            println!(
                "  ok    plain texture path    worst channel difference {} across {} texels",
                report.worst,
                GRID * GRID
            );
            println!();
            println!(
                "the screen holds the file's own bytes: the default texture path \
                 is identity, no gamma conversion anywhere"
            );
            ExitCode::SUCCESS
        }
        Some(reason) => {
            println!(
                "  FAIL  plain texture path    worst channel difference {}",
                report.worst
            );
            println!("          {reason}");
            ExitCode::FAILURE
        }
    }
}
