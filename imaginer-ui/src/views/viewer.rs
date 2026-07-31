//! The main canvas: pan, zoom, and the navigation chevrons that float over it.

use eframe::egui;
use imaginer_core::Adjust;

use crate::icons::{self, Icon, Icons};
use crate::shader::AdjustShader;
use crate::texture::ImageTexture;
use crate::theme;

const MIN_ZOOM: f32 = 0.02;
const MAX_ZOOM: f32 = 32.0;
/// How much of the image must stay on screen when panning, in points. Without a
/// clamp it is easy to fling the image out of view and be left staring at nothing.
const PAN_KEEP_VISIBLE: f32 = 48.0;

/// How the image is currently being looked at.
///
/// Deliberately without a rotation of its own. There used to be one, turning the
/// view without touching the file; it was dropped when the edit sidebar arrived,
/// because two buttons that look identical and differ only in whether the result
/// can be saved is a trap. Rotation is an edit now, and the pipeline being
/// non-destructive means straightening a crooked photo just to look at it still
/// costs nothing.
pub struct ViewState {
    /// Zoom used when not fitting. 1.0 means one image pixel per point.
    pub zoom: f32,
    /// Image centre relative to the canvas centre, in points.
    pub offset: egui::Vec2,
    /// While set, zoom is recomputed from the window size every frame.
    pub fit: bool,
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            offset: egui::Vec2::ZERO,
            fit: true,
        }
    }
}

impl ViewState {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Switch to a fixed zoom, centred.
    pub fn set_zoom(&mut self, zoom: f32) {
        self.fit = false;
        self.zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        self.offset = egui::Vec2::ZERO;
    }

    /// Multiply the current zoom, keeping the centre.
    pub fn zoom_by(&mut self, factor: f32, effective: f32) {
        self.fit = false;
        self.zoom = (effective * factor).clamp(MIN_ZOOM, MAX_ZOOM);
    }
}

/// What the canvas drew this frame, so anything painted over it can line up.
#[derive(Debug, Clone, Copy)]
pub struct Shown {
    /// Zoom actually used, which is what the status bar reports.
    pub zoom: f32,
    /// Where the image landed on screen.
    pub image_rect: egui::Rect,
}

/// How the image is to be coloured on the way to the screen.
pub struct Colour<'a> {
    /// The live slider values. At rest the image is drawn the ordinary way, so an
    /// unedited photograph never goes near the shader.
    pub adjust: Adjust,
    pub shader: &'a AdjustShader,
}

/// Draw the image and handle interaction.
///
/// `interactive` is false while cropping: the canvas belongs to the selection then,
/// and a drag that panned the image and resized the crop rectangle at the same time
/// would do neither well.
pub fn show(
    ui: &mut egui::Ui,
    texture: &ImageTexture,
    state: &mut ViewState,
    interactive: bool,
    colour: Colour<'_>,
) -> Shown {
    let sense = if interactive {
        egui::Sense::click_and_drag()
    } else {
        egui::Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(ui.available_size(), sense);
    ui.painter().rect_filled(rect, 0.0, theme::CANVAS_BG);

    let natural = texture.source_vec();
    if natural.x <= 0.0 || natural.y <= 0.0 {
        return Shown {
            zoom: 1.0,
            image_rect: rect,
        };
    }

    // Fit shrinks to the window but never enlarges — blowing a 32px icon up to
    // fill a 1080p window is not what "fit" means to anyone.
    let fit_zoom = (rect.width() / natural.x)
        .min(rect.height() / natural.y)
        .min(1.0)
        .clamp(MIN_ZOOM, MAX_ZOOM);

    let mut zoom = if state.fit { fit_zoom } else { state.zoom };

    if response.dragged() {
        state.fit = false;
        state.zoom = zoom;
        state.offset += response.drag_delta();
    }

    // Scroll wheel zooms about the cursor, so the pixel under the pointer stays
    // under the pointer — panning to chase your own zoom is miserable.
    let scroll = ui.input(|i| i.smooth_scroll_delta.y);
    if scroll != 0.0 && response.hovered() {
        let new_zoom = (zoom * (scroll * 0.0015).exp()).clamp(MIN_ZOOM, MAX_ZOOM);
        if let Some(pointer) = ui.input(|i| i.pointer.hover_pos()) {
            let centre = rect.center() + state.offset;
            let anchor = (pointer - centre) / zoom;
            state.offset = pointer - anchor * new_zoom - rect.center();
        }
        state.fit = false;
        state.zoom = new_zoom;
        zoom = new_zoom;
    }

    if response.double_clicked() {
        // Toggle between fit and 100%, the two views actually worth a shortcut.
        if state.fit {
            state.set_zoom(1.0);
        } else {
            state.reset();
        }
        zoom = if state.fit { fit_zoom } else { state.zoom };
    }

    let displayed = natural * zoom;
    state.offset = clamp_offset(state.offset, displayed, rect.size());

    let image_rect = egui::Rect::from_center_size(rect.center() + state.offset, displayed);
    let painter = ui.painter_at(rect);

    // The shader is only worth reaching for when it has something to do, and it is
    // skipped entirely if it could not be built — on hardware that cannot compile
    // it, the adjustment still reaches the saved file, it just is not previewed.
    if colour.adjust.is_none() || colour.shader.failed() {
        painter.image(
            texture.handle.id(),
            image_rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
    } else {
        painter.add(
            colour
                .shader
                .callback(rect, image_rect, texture.handle.id(), colour.adjust),
        );
    }

    Shown { zoom, image_rect }
}

/// Which way a canvas chevron was asking to go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Prev,
    Next,
}

/// Radius of a chevron's disc, which is also its hit target.
const CHEVRON_RADIUS: f32 = 21.0;

/// Gap between the disc and the edge of the canvas.
const CHEVRON_INSET: f32 = 14.0;

/// Drawn size of the arrow inside the disc.
const CHEVRON_ICON: f32 = 22.0;

/// Floating previous/next affordances at the canvas edges.
///
/// Here rather than in the toolbar because stepping through a folder is the single
/// most repeated action in a viewer, so it belongs where the eye already is — and
/// keeping it off the toolbar is precisely what lets that row stay short enough to
/// read at a glance. They come and go with the pointer so that looking at a
/// photograph is not looking at two buttons on top of it.
pub fn chevrons(
    ui: &mut egui::Ui,
    icons: &mut Icons,
    canvas: egui::Rect,
    visible: bool,
) -> Option<Step> {
    let middle = canvas.center().y;
    let offset = CHEVRON_INSET + CHEVRON_RADIUS;
    let sides = [
        (Step::Prev, Icon::ChevronLeft, canvas.left() + offset),
        (Step::Next, Icon::ChevronRight, canvas.right() - offset),
    ];
    let disc = |x: f32| {
        egui::Rect::from_center_size(
            egui::pos2(x, middle),
            egui::Vec2::splat(CHEVRON_RADIUS * 2.0),
        )
    };

    // A pointer resting on a chevron holds it open. Without this it disappears from
    // under the cursor, and the thing you were about to click stops existing.
    let pointer = ui.input(|i| i.pointer.hover_pos());
    let held_open = pointer.is_some_and(|at| sides.iter().any(|(_, _, x)| disc(*x).contains(at)));

    // Nothing is drawn when they are hidden, and nothing is claimed either: an
    // invisible chevron that still swallowed clicks would break panning near the
    // edges of the image.
    if !visible && !held_open {
        return None;
    }

    let mut step = None;
    for (direction, icon, x) in sides {
        let rect = disc(x);
        let response = ui.interact(rect, ui.id().with(direction as u8), egui::Sense::click());

        let backdrop = if response.hovered() {
            theme::PANEL_BG.gamma_multiply(0.92)
        } else {
            theme::PANEL_BG.gamma_multiply(0.66)
        };
        ui.painter()
            .circle_filled(rect.center(), CHEVRON_RADIUS, backdrop);
        icons::paint(
            ui,
            icons,
            icon,
            egui::Rect::from_center_size(rect.center(), egui::Vec2::splat(CHEVRON_ICON)),
            theme::TEXT_PRIMARY,
        );

        if response.clicked() {
            step = Some(direction);
        }
    }

    step
}

/// Keep at least a sliver of the image on screen.
fn clamp_offset(offset: egui::Vec2, displayed: egui::Vec2, canvas: egui::Vec2) -> egui::Vec2 {
    let limit = (displayed + canvas) * 0.5 - egui::Vec2::splat(PAN_KEEP_VISIBLE);
    egui::vec2(
        offset.x.clamp(-limit.x.max(0.0), limit.x.max(0.0)),
        offset.y.clamp(-limit.y.max(0.0), limit.y.max(0.0)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offset_is_clamped_so_the_image_cannot_leave_the_screen() {
        let displayed = egui::vec2(200.0, 200.0);
        let canvas = egui::vec2(400.0, 400.0);
        let clamped = clamp_offset(egui::vec2(10_000.0, -10_000.0), displayed, canvas);

        let limit = (200.0 + 400.0) / 2.0 - PAN_KEEP_VISIBLE;
        assert_eq!(clamped, egui::vec2(limit, -limit));
    }

    #[test]
    fn tiny_images_clamp_to_zero_rather_than_a_negative_range() {
        // Image and canvas both smaller than the keep-visible margin: the limit
        // would go negative, and `clamp` panics if min > max.
        let clamped = clamp_offset(
            egui::vec2(50.0, 50.0),
            egui::vec2(4.0, 4.0),
            egui::vec2(10.0, 10.0),
        );
        assert_eq!(clamped, egui::Vec2::ZERO);
    }
}
