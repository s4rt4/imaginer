//! Runs the real adjustment shader on the real GPU and checks the pixels it produces
//! against `imaginer_core::adjust`.
//!
//! `shader::tests::the_shader_formula_matches_core` transcribes the fragment maths
//! into Rust and compares that against core. It is worth having, but it never
//! executes a line of GLSL: a typo inside `FRAGMENT_SOURCE`, a uniform that is never
//! written, a quad in the wrong place or a texture that is never sampled all pass it.
//! This closes that gap the only way it can be closed — by drawing with the shader
//! and reading the framebuffer back.
//!
//! What it actually proves, in order of how easy each is to get wrong:
//!   * The shader compiles and links on this driver, and `AdjustShader::callback`
//!     really draws. If it did not, the image region would still be the backdrop.
//!   * Every texel comes out the colour core says it should, for nine settings
//!     spanning both ends of all three sliders.
//!   * The quad lands where `u_rect` says. The image is drawn at a deliberate offset
//!     from the canvas origin, and the pixels outside it must be untouched.
//!   * `v_tc` is not flipped. The test grid's green channel varies with the row, so
//!     an upside-down quad fails every row but the middle one.
//!
//! Deliberately not a `#[test]`: it needs a GL context, and the only one that is the
//! *app's* context comes from running eframe. A window opens for well under a second.
//!
//! `cargo run --release -p imaginer-ui --example verify-adjust-shader`
//!
//! Exit status is 0 only if every case passed, so it can be a build step later.

// The module under test, included as source. `imaginer-ui` is a binary crate with no
// library target, so an example cannot `use` its modules — and a copy of the shader
// would test the copy.
#[path = "../src/shader.rs"]
mod shader;

use std::process::ExitCode;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use eframe::egui;
use eframe::glow::{self, HasContext as _};
use imaginer_core::Adjust;
use imaginer_core::image::{Rgba, RgbaImage};

use shader::AdjustShader;

/// Texels a side in the test image. 16 gives every channel a 17-step ramp, which
/// reaches exactly 255 at the top.
const GRID: u32 = 16;

/// Points each texel is drawn as. Magnified with nearest sampling, so one texel
/// becomes one flat block and the middle of a block is nowhere near a boundary —
/// no sampling or rounding argument can be had about which texel a sample came from.
const BLOCK: f32 = 8.0;

/// Where the image sits inside the canvas. Not the origin and not the centre: a quad
/// positioned from the viewport instead of from `u_rect` would still cover the middle.
const INSET: egui::Vec2 = egui::vec2(37.0, 23.0);

/// What the canvas is painted with before the shader runs. Nothing outside the image
/// rectangle may be anything else.
const BACKDROP: egui::Color32 = egui::Color32::from_rgb(9, 11, 13);

/// A channel may differ from core by this much. Both sides do the same arithmetic in
/// f32 and then round to a byte, so anything above 1 means a real disagreement rather
/// than the last bit of a float — the report prints what was actually seen.
const TOLERANCE: u8 = 2;

const CASES: &[(&str, Adjust)] = &[
    ("no adjustment", Adjust::NONE),
    ("brightness +25", adjust(25, 0, 0)),
    ("brightness -40", adjust(-40, 0, 0)),
    ("contrast +60", adjust(0, 60, 0)),
    ("contrast -100 (flat grey)", adjust(0, -100, 0)),
    ("saturation -100 (greyscale)", adjust(0, 0, -100)),
    ("saturation +100", adjust(0, 0, 100)),
    ("all three, mixed signs", adjust(15, -20, 45)),
    ("all three, the other way", adjust(-30, 70, -55)),
];

const fn adjust(brightness: i16, contrast: i16, saturation: i16) -> Adjust {
    Adjust {
        brightness,
        contrast,
        saturation,
    }
}

/// The image the shader is asked to colour.
///
/// Red runs with the column and green with the row, so a quad that is mirrored or
/// flipped cannot match; blue is `x ^ y`, which puts a pattern through the middle
/// that a uniformly wrong sample coordinate would not reproduce.
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
    ///
    /// Rounded the same way `ViewportInPixels::from_points` rounds the viewport
    /// origin, so the two cannot drift apart by a pixel at a fractional scale factor.
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

/// A second callback, queued straight after the shader's, that copies the framebuffer
/// out while the frame is still being built.
///
/// Callbacks run in the order they were added to the painter, so by the time this one
/// executes the adjusted quad is already in the buffer it reads.
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
            // SAFETY: the painting thread, with the context current — the same
            // guarantee the shader's own callback relies on.
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

/// What one case came to.
struct Verdict {
    label: &'static str,
    /// The largest difference from core across every channel of every texel.
    worst: u8,
    /// The first texel that was out of tolerance, if any.
    failure: Option<String>,
}

#[derive(Default)]
struct Report {
    verdicts: Vec<Verdict>,
    /// A reason the run could not answer the question at all, as opposed to an answer
    /// of "no".
    fatal: Option<String>,
}

struct Verify {
    shader: AdjustShader,
    texture: Option<egui::TextureHandle>,
    source: RgbaImage,
    slot: Arc<Mutex<Option<Readback>>>,
    report: Arc<Mutex<Report>>,
    /// Frames to let the window settle before anything is measured. The first frame
    /// of a freshly created context is not a good thing to trust.
    warmup: u32,
    case: usize,
}

impl Verify {
    fn new(cc: &eframe::CreationContext<'_>, report: Arc<Mutex<Report>>) -> Self {
        let source = source();
        let image = egui::ColorImage::from_rgba_unmultiplied(
            [source.width() as usize, source.height() as usize],
            source.as_raw(),
        );

        Self {
            shader: AdjustShader::default(),
            // Nearest in both directions: the shader is being asked what it does to a
            // colour, and an interpolated texel would be a different question.
            texture: Some(cc.egui_ctx.load_texture(
                "adjust-source",
                image,
                egui::TextureOptions::NEAREST,
            )),
            source,
            slot: Arc::new(Mutex::new(None)),
            report,
            warmup: 2,
            case: 0,
        }
    }

    /// Compare one captured frame against what core says the image should look like.
    fn judge(&self, frame: &Readback, label: &'static str, adjust: Adjust) -> Verdict {
        let mut expected = self.source.clone();
        adjust.apply(&mut expected);

        let rect = image_rect(frame.canvas);
        let mut worst = 0u8;
        let mut failure = None;

        // Outside the image rectangle nothing may have been written. A quad that
        // covered the whole viewport would pass every colour check and fail this.
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
                    "the shader painted outside its rectangle, {where_}: \
                     expected the backdrop {:?}, read {:?}",
                    &backdrop[..3],
                    &got[..3]
                ));
            }
        }

        for y in 0..GRID {
            for x in 0..GRID {
                // The middle of the block this texel was magnified into.
                let point =
                    rect.min + egui::vec2((x as f32 + 0.5) * BLOCK, (y as f32 + 0.5) * BLOCK);
                let Some(got) = frame.at(point) else {
                    failure.get_or_insert(format!("texel {x},{y} landed outside the canvas"));
                    continue;
                };
                let want = expected.get_pixel(x, y).0;

                for channel in 0..3 {
                    let difference = got[channel].abs_diff(want[channel]);
                    worst = worst.max(difference);
                    if difference > TOLERANCE {
                        failure.get_or_insert(format!(
                            "texel {x},{y} (source {:?}): core says {:?}, the GPU drew {:?}",
                            self.source.get_pixel(x, y).0,
                            &want[..3],
                            &got[..3]
                        ));
                    }
                }
            }
        }

        Verdict {
            label,
            worst,
            failure,
        }
    }

    fn finish(&self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

impl eframe::App for Verify {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // Every frame is a measurement, so none of them may be skipped for idleness.
        ctx.request_repaint();

        if self.warmup > 0 {
            self.warmup -= 1;
            return;
        }

        // A frame captured last time round is the previous case's answer: the capture
        // happens during painting, which is after this function returned.
        if let Some(frame) = lock(&self.slot).take() {
            let (label, adjust) = CASES[self.case];
            let verdict = self.judge(&frame, label, adjust);
            lock(&self.report).verdicts.push(verdict);
            self.case += 1;
        }

        if self.case >= CASES.len() {
            self.finish(&ctx);
            return;
        }

        // Recorded rather than left to show up as every colour being wrong: on
        // hardware that cannot compile the shader the app falls back to drawing the
        // image plainly, and that is a different result from a wrong shader.
        if self.shader.failed() {
            lock(&self.report).fatal =
                Some("the shader failed to compile or link — see the message above".to_owned());
            self.finish(&ctx);
            return;
        }

        let Some(texture) = self.texture.clone() else {
            return;
        };
        let (_, adjust) = CASES[self.case];

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let canvas = ui.available_rect_before_wrap();
                ui.painter().rect_filled(canvas, 0.0, BACKDROP);

                // The same two arguments the viewer passes, in the same order.
                ui.painter().add(self.shader.callback(
                    canvas,
                    image_rect(canvas),
                    texture.id(),
                    adjust,
                ));
                ui.painter().add(readback(canvas, Arc::clone(&self.slot)));
            });
    }

    fn on_exit(&mut self, gl: Option<&glow::Context>) {
        if let Some(gl) = gl {
            self.shader.destroy(gl);
        }
    }
}

fn main() -> ExitCode {
    let report = Arc::new(Mutex::new(Report::default()));

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([420.0, 300.0])
            .with_title("Imaginer — verifying the adjustment shader")
            .with_resizable(false),
        // No multisampling, or the default framebuffer could not be read back at all.
        multisampling: 0,
        // The run is a fixed number of frames; waiting for the display to catch up
        // just makes it take longer.
        glow_options: eframe::egui_glow::GlowConfiguration {
            vsync: false,
            ..Default::default()
        },
        ..Default::default()
    };

    let started = eframe::run_native(
        "verify-adjust-shader",
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
    let mut failed = false;

    println!();
    for verdict in &report.verdicts {
        match &verdict.failure {
            None => println!(
                "  ok    {:<28} worst channel difference {}",
                verdict.label, verdict.worst
            ),
            Some(reason) => {
                failed = true;
                println!(
                    "  FAIL  {:<28} worst channel difference {}\n          {reason}",
                    verdict.label, verdict.worst
                );
            }
        }
    }

    if let Some(fatal) = &report.fatal {
        println!("  FATAL {fatal}");
        failed = true;
    }

    // A window closed early — by hand, or by anything else — leaves cases unanswered,
    // and silence about them would read exactly like a pass.
    if report.verdicts.len() < CASES.len() && report.fatal.is_none() {
        println!(
            "  FATAL only {} of {} cases ran; the window closed early",
            report.verdicts.len(),
            CASES.len()
        );
        failed = true;
    }

    println!();
    if failed {
        println!("the adjustment shader does NOT agree with imaginer_core::adjust");
        ExitCode::FAILURE
    } else {
        println!(
            "the adjustment shader draws, and agrees with imaginer_core::adjust \
             across {} settings and {} texels each",
            CASES.len(),
            GRID * GRID
        );
        ExitCode::SUCCESS
    }
}
