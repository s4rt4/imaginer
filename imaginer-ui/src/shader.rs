//! The colour adjustment, done on the GPU while a slider is moving.
//!
//! The CPU implementation in `imaginer_core::adjust` is the one that writes files,
//! and it stays the definition of what an adjustment *is*. It is simply too slow to
//! watch: measured on this machine, a 24MP image takes 47ms for brightness and
//! contrast and **387ms once saturation is in play** (`cargo test --release -p
//! imaginer-core -- --ignored adjust_costs`). A slider has 16ms. So the preview
//! texture holds the image with the geometry applied and the colour left alone, and
//! this shader colours it at draw time, every frame, for free.
//!
//! The two implementations must agree, and the reason they can is that egui's own
//! fragment shader does nothing clever: it samples the texture and multiplies by the
//! vertex colour, both in gamma space, with no colour conversion anywhere. So the
//! numbers this shader works on are the same 0..1 values the CPU path derives from
//! the same bytes. The one difference that has to be undone is premultiplied alpha,
//! which egui applies on upload and core knows nothing about.
//!
//! Two things check that, and they check different things:
//!   * `tests::the_shader_formula_matches_core` transcribes the fragment maths into
//!     Rust and compares against core over a grid. It runs in `cargo test` and needs
//!     no GPU, but it never executes GLSL — it pins the *formula*, not this source.
//!   * `examples/verify-adjust-shader.rs` runs this shader on the GPU through
//!     `callback` and reads the framebuffer back. That one catches what the other
//!     cannot: a typo in the source, a uniform never written, a quad in the wrong
//!     place, a flipped `v_tc`. It needs a window, so it is a `cargo run --example`
//!     rather than a test. Run it after touching anything below.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use eframe::egui;
use eframe::glow::{self, HasContext as _};
use imaginer_core::Adjust;

/// A quad on the rectangle the image occupies, positioned from a uniform rather than
/// from the viewport.
///
/// Positioning it by uniform is what keeps the callback's viewport equal to the
/// canvas: zoomed to 32x, an image rectangle is hundreds of thousands of pixels wide
/// and would run past `GL_MAX_VIEWPORT_DIMS`, where behaviour stops being defined.
/// No vertex buffer — four corners are cheaper to derive from `gl_VertexID` than to
/// upload.
const VERTEX_SOURCE: &str = r#"#version 330
uniform vec4 u_rect;
out vec2 v_tc;
void main() {
    vec2 corner = vec2(float(gl_VertexID & 1), float((gl_VertexID >> 1) & 1));
    gl_Position = vec4(mix(u_rect.xy, u_rect.zw, corner), 0.0, 1.0);
    // egui uploads the first row of the image first, so texture v runs downwards
    // while clip-space y runs up.
    v_tc = vec2(corner.x, 1.0 - corner.y);
}
"#;

/// Brightness, then contrast, then saturation — the same order, and the same single
/// clamp at the end, as `imaginer_core::adjust`.
const FRAGMENT_SOURCE: &str = r#"#version 330
uniform sampler2D u_image;
uniform float u_brightness;
uniform float u_contrast;
uniform float u_saturation;
in vec2 v_tc;
out vec4 f_color;

const vec3 LUMA = vec3(0.2126, 0.7152, 0.0722);

void main() {
    vec4 texel = texture(u_image, v_tc);

    // egui stores colours premultiplied; the adjustment is defined on straight
    // colour, which is what core works on. Undo it, adjust, put it back.
    vec3 colour = texel.a > 0.0 ? texel.rgb / texel.a : texel.rgb;

    colour = colour + u_brightness;
    colour = (colour - 0.5) * u_contrast + 0.5;
    float luma = dot(colour, LUMA);
    colour = luma + (colour - luma) * u_saturation;

    f_color = vec4(clamp(colour, 0.0, 1.0) * texel.a, texel.a);
}
"#;

#[derive(Clone, Copy)]
struct Built {
    program: glow::Program,
    /// Empty, but a core profile refuses to draw without one bound.
    vao: glow::VertexArray,
    rect: Option<glow::UniformLocation>,
    brightness: Option<glow::UniformLocation>,
    contrast: Option<glow::UniformLocation>,
    saturation: Option<glow::UniformLocation>,
}

enum State {
    /// Nothing built yet. Compiling costs a few milliseconds, and an image that is
    /// only ever looked at should not pay them — so it happens the first time an
    /// adjustment is actually asked for, on the render thread that owns the context.
    Unbuilt,
    Ready(Built),
    /// Compilation failed. Recorded rather than retried every frame, and read by the
    /// viewer so it can go back to drawing the image the ordinary way.
    Failed,
}

/// The adjustment shader, and the handle the paint callback holds.
///
/// Behind an `Arc<Mutex<_>>` because egui runs paint callbacks later and elsewhere:
/// the closure has to own everything it touches and be `Send + Sync`.
#[derive(Clone)]
pub struct AdjustShader {
    state: Arc<Mutex<State>>,
}

impl Default for AdjustShader {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(State::Unbuilt)),
        }
    }
}

impl AdjustShader {
    /// Whether the shader has given up, in which case the caller should draw the
    /// image without it and adjust on the CPU instead.
    pub fn failed(&self) -> bool {
        matches!(*lock(&self.state), State::Failed)
    }

    /// A callback that draws `texture` into `image_rect`, adjusted.
    ///
    /// `canvas` becomes the callback's viewport and so the frame of reference for
    /// the rectangle; both are in points, and both come straight from the viewer.
    pub fn callback(
        &self,
        canvas: egui::Rect,
        image_rect: egui::Rect,
        texture: egui::TextureId,
        adjust: Adjust,
    ) -> egui::PaintCallback {
        let rect = ndc_rect(canvas, image_rect);
        let adjust = adjust.clamped();
        let state = Arc::clone(&self.state);

        egui::PaintCallback {
            rect: canvas,
            callback: Arc::new(eframe::egui_glow::CallbackFn::new(move |_info, painter| {
                let gl = painter.gl();
                let Some(texture) = painter.texture(texture) else {
                    return;
                };

                let mut current = lock(&state);
                if matches!(*current, State::Unbuilt) {
                    // SAFETY: runs on the thread egui paints from, with the context
                    // current — the only place `painter.gl()` is handed out.
                    *current = match unsafe { build(gl) } {
                        Some(built) => State::Ready(built),
                        None => State::Failed,
                    };
                }
                let State::Ready(built) = *current else {
                    return;
                };
                drop(current);

                // SAFETY: as above. egui restores its own pipeline state after the
                // callback returns, so nothing here has to be put back.
                unsafe { draw(gl, built, texture, rect, adjust) };
            })),
        }
    }

    /// Give the GL objects back, at shutdown.
    ///
    /// The driver would reclaim them when the context dies anyway; doing it properly
    /// means a leak that ever does matter shows up as one.
    pub fn destroy(&self, gl: &glow::Context) {
        let mut state = lock(&self.state);
        if let State::Ready(built) = *state {
            // SAFETY: called from `eframe::App::on_exit`, which hands over the
            // context while it is still current.
            unsafe {
                gl.delete_program(built.program);
                gl.delete_vertex_array(built.vao);
            }
        }
        *state = State::Unbuilt;
    }
}

/// Where `image_rect` sits inside `canvas`, in clip space.
///
/// `(x0, y0, x1, y1)` with y0 the *bottom* edge, because clip space counts upwards
/// and screen points count down.
fn ndc_rect(canvas: egui::Rect, image_rect: egui::Rect) -> [f32; 4] {
    let to_x = |x: f32| ((x - canvas.left()) / canvas.width()) * 2.0 - 1.0;
    let to_y = |y: f32| 1.0 - ((y - canvas.top()) / canvas.height()) * 2.0;

    [
        to_x(image_rect.left()),
        to_y(image_rect.bottom()),
        to_x(image_rect.right()),
        to_y(image_rect.top()),
    ]
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    // Nothing under this lock can panic on its own; recovering beats taking the
    // window down over a poisoned mutex holding two integers.
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

unsafe fn build(gl: &glow::Context) -> Option<Built> {
    unsafe {
        let vertex = compile(gl, glow::VERTEX_SHADER, VERTEX_SOURCE)?;
        let fragment = compile(gl, glow::FRAGMENT_SHADER, FRAGMENT_SOURCE)?;

        let program = gl.create_program().ok()?;
        gl.attach_shader(program, vertex);
        gl.attach_shader(program, fragment);
        gl.link_program(program);

        // Attached shaders are only needed for the link; the program keeps what it
        // compiled from them.
        gl.detach_shader(program, vertex);
        gl.detach_shader(program, fragment);
        gl.delete_shader(vertex);
        gl.delete_shader(fragment);

        if !gl.get_program_link_status(program) {
            eprintln!(
                "adjust shader: link failed: {}",
                gl.get_program_info_log(program)
            );
            gl.delete_program(program);
            return None;
        }

        let vao = gl.create_vertex_array().ok()?;
        Some(Built {
            rect: gl.get_uniform_location(program, "u_rect"),
            brightness: gl.get_uniform_location(program, "u_brightness"),
            contrast: gl.get_uniform_location(program, "u_contrast"),
            saturation: gl.get_uniform_location(program, "u_saturation"),
            program,
            vao,
        })
    }
}

unsafe fn compile(gl: &glow::Context, kind: u32, source: &str) -> Option<glow::Shader> {
    unsafe {
        let shader = gl.create_shader(kind).ok()?;
        gl.shader_source(shader, source);
        gl.compile_shader(shader);

        if !gl.get_shader_compile_status(shader) {
            eprintln!(
                "adjust shader: compile failed: {}",
                gl.get_shader_info_log(shader)
            );
            gl.delete_shader(shader);
            return None;
        }
        Some(shader)
    }
}

unsafe fn draw(
    gl: &glow::Context,
    built: Built,
    texture: glow::Texture,
    rect: [f32; 4],
    adjust: Adjust,
) {
    unsafe {
        gl.use_program(Some(built.program));
        gl.bind_vertex_array(Some(built.vao));

        gl.active_texture(glow::TEXTURE0);
        gl.bind_texture(glow::TEXTURE_2D, Some(texture));

        gl.uniform_4_f32(built.rect.as_ref(), rect[0], rect[1], rect[2], rect[3]);
        gl.uniform_1_f32(built.brightness.as_ref(), adjust.brightness as f32 / 100.0);
        gl.uniform_1_f32(
            built.contrast.as_ref(),
            1.0 + adjust.contrast as f32 / 100.0,
        );
        gl.uniform_1_f32(
            built.saturation.as_ref(),
            1.0 + adjust.saturation as f32 / 100.0,
        );

        // Four corners as a strip. The sampler defaults to texture unit 0, which is
        // the one bound above, so it needs no uniform of its own.
        gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
        gl.bind_vertex_array(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use imaginer_core::image::{Rgba, RgbaImage};

    /// The fragment shader's arithmetic, transcribed line for line.
    ///
    /// Deliberately a transcription rather than a tidy rewrite: it is only worth
    /// anything if a reader can hold it beside `FRAGMENT_SOURCE` and see that they
    /// say the same thing. What proves the GPU agrees is
    /// `examples/verify-adjust-shader.rs`, not this.
    fn fragment(colour: [f32; 3], alpha: f32, adjust: Adjust) -> [f32; 3] {
        const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

        let brightness = adjust.brightness as f32 / 100.0;
        let contrast = 1.0 + adjust.contrast as f32 / 100.0;
        let saturation = 1.0 + adjust.saturation as f32 / 100.0;

        // The premultiply round trip the shader does, so a non-opaque pixel is
        // compared through the same arithmetic it would meet on the GPU.
        let mut c = if alpha > 0.0 {
            [colour[0] / alpha, colour[1] / alpha, colour[2] / alpha]
        } else {
            colour
        };

        for channel in &mut c {
            *channel += brightness;
            *channel = (*channel - 0.5) * contrast + 0.5;
        }

        let luma = LUMA[0] * c[0] + LUMA[1] * c[1] + LUMA[2] * c[2];
        for channel in &mut c {
            *channel = luma + (*channel - luma) * saturation;
        }

        c.map(|channel| channel.clamp(0.0, 1.0))
    }

    #[test]
    fn the_shader_formula_matches_core() {
        let settings = [
            Adjust::NONE,
            Adjust {
                brightness: 25,
                contrast: 0,
                saturation: 0,
            },
            Adjust {
                brightness: -40,
                contrast: 0,
                saturation: 0,
            },
            Adjust {
                brightness: 0,
                contrast: 60,
                saturation: 0,
            },
            Adjust {
                brightness: 0,
                contrast: -100,
                saturation: 0,
            },
            Adjust {
                brightness: 0,
                contrast: 0,
                saturation: -100,
            },
            Adjust {
                brightness: 0,
                contrast: 0,
                saturation: 100,
            },
            Adjust {
                brightness: 15,
                contrast: -20,
                saturation: 45,
            },
            Adjust {
                brightness: -30,
                contrast: 70,
                saturation: -55,
            },
        ];

        // Opaque only. Premultiplication throws away precision that no tolerance can
        // honestly paper over — an alpha of 1/255 leaves each channel with eight
        // distinguishable values — and every path that saves a file works on straight
        // colour anyway. What the two implementations must agree on is the picture.
        let colours = [
            [0u8, 0, 0],
            [255, 255, 255],
            [128, 128, 128],
            [255, 0, 0],
            [0, 255, 0],
            [0, 0, 255],
            [10, 120, 250],
            [200, 60, 90],
            [37, 37, 200],
        ];

        for adjust in settings {
            for colour in colours {
                let mut image =
                    RgbaImage::from_pixel(1, 1, Rgba([colour[0], colour[1], colour[2], 255]));
                adjust.apply(&mut image);
                let cpu = image.get_pixel(0, 0).0;

                let gpu = fragment(colour.map(|c| c as f32 / 255.0), 1.0, adjust)
                    .map(|c| (c * 255.0).round() as u8);

                for channel in 0..3 {
                    assert!(
                        cpu[channel].abs_diff(gpu[channel]) <= 1,
                        "{adjust:?} on {colour:?}: core gave {cpu:?}, the shader formula {gpu:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_rectangle_filling_the_canvas_is_the_whole_of_clip_space() {
        let canvas = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(100.0, 50.0));
        let ndc = ndc_rect(canvas, canvas);

        assert_eq!(ndc, [-1.0, -1.0, 1.0, 1.0]);
    }

    #[test]
    fn clip_space_y_is_flipped_relative_to_screen_points() {
        let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0));
        // The top half of the canvas.
        let image = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 50.0));
        let [_, bottom, _, top] = ndc_rect(canvas, image);

        // Its bottom edge lands in the middle, its top edge at the top.
        assert_eq!(bottom, 0.0);
        assert_eq!(top, 1.0);
    }

    #[test]
    fn an_image_hanging_off_the_canvas_reaches_past_clip_space() {
        // Zoomed in, so most of the image is outside the window. The scissor egui
        // sets is what hides the excess; these coordinates are meant to run past the
        // edges rather than be clamped to them.
        let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0));
        let image = egui::Rect::from_min_size(egui::pos2(-100.0, -100.0), egui::vec2(400.0, 400.0));
        let [x0, y0, x1, y1] = ndc_rect(canvas, image);

        assert_eq!([x0, x1], [-3.0, 5.0]);
        assert_eq!([y0, y1], [-5.0, 3.0]);
    }
}
