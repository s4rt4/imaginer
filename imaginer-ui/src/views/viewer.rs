//! The main canvas: pan, zoom, and view rotation.

use eframe::egui;

use crate::texture::ImageTexture;
use crate::theme;

const MIN_ZOOM: f32 = 0.02;
const MAX_ZOOM: f32 = 32.0;
/// How much of the image must stay on screen when panning, in points. Without a
/// clamp it is easy to fling the image out of view and be left staring at nothing.
const PAN_KEEP_VISIBLE: f32 = 48.0;

pub struct ViewState {
    /// Zoom used when not fitting. 1.0 means one image pixel per point.
    pub zoom: f32,
    /// Image centre relative to the canvas centre, in points.
    pub offset: egui::Vec2,
    /// While set, zoom is recomputed from the window size every frame.
    pub fit: bool,
    /// View-only rotation, 0-3 clockwise quarter turns. Does not touch pixels.
    pub quarter_turns: u8,
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            offset: egui::Vec2::ZERO,
            fit: true,
            quarter_turns: 0,
        }
    }
}

impl ViewState {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn rotate_clockwise(&mut self) {
        self.quarter_turns = (self.quarter_turns + 1) % 4;
        self.offset = egui::Vec2::ZERO;
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

    /// Size the image occupies on screen at 100%, accounting for view rotation.
    fn oriented_size(&self, size: egui::Vec2) -> egui::Vec2 {
        if self.quarter_turns % 2 == 1 {
            egui::vec2(size.y, size.x)
        } else {
            size
        }
    }
}

/// Draw the image and handle interaction. Returns the zoom actually used, so the
/// status bar can report it.
pub fn show(ui: &mut egui::Ui, texture: &ImageTexture, state: &mut ViewState) -> f32 {
    let (rect, response) =
        ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
    ui.painter().rect_filled(rect, 0.0, theme::CANVAS_BG);

    let natural = state.oriented_size(texture.source_vec());
    if natural.x <= 0.0 || natural.y <= 0.0 {
        return 1.0;
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
    paint_image(ui, texture, image_rect, rect, state.quarter_turns);

    zoom
}

/// Keep at least a sliver of the image on screen.
fn clamp_offset(offset: egui::Vec2, displayed: egui::Vec2, canvas: egui::Vec2) -> egui::Vec2 {
    let limit = (displayed + canvas) * 0.5 - egui::Vec2::splat(PAN_KEEP_VISIBLE);
    egui::vec2(
        offset.x.clamp(-limit.x.max(0.0), limit.x.max(0.0)),
        offset.y.clamp(-limit.y.max(0.0), limit.y.max(0.0)),
    )
}

fn paint_image(
    ui: &egui::Ui,
    texture: &ImageTexture,
    image_rect: egui::Rect,
    clip: egui::Rect,
    quarter_turns: u8,
) {
    let painter = ui.painter_at(clip);

    // Built by hand rather than with `add_rect_with_uv` because view rotation is
    // expressed by rotating which corner of the texture each vertex samples —
    // a UV `Rect` cannot represent that.
    let corners = [
        image_rect.left_top(),
        image_rect.right_top(),
        image_rect.right_bottom(),
        image_rect.left_bottom(),
    ];
    let base_uv = [
        egui::pos2(0.0, 0.0),
        egui::pos2(1.0, 0.0),
        egui::pos2(1.0, 1.0),
        egui::pos2(0.0, 1.0),
    ];

    let turns = (quarter_turns % 4) as usize;
    let mut mesh = egui::Mesh::with_texture(texture.handle.id());
    for (i, pos) in corners.iter().enumerate() {
        mesh.vertices.push(egui::epaint::Vertex {
            pos: *pos,
            uv: base_uv[(i + 4 - turns) % 4],
            color: egui::Color32::WHITE,
        });
    }
    mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);

    painter.add(egui::Shape::mesh(mesh));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quarter_turns_swap_the_oriented_size() {
        let landscape = egui::vec2(800.0, 600.0);
        let state_for = |quarter_turns| ViewState {
            quarter_turns,
            ..Default::default()
        };

        assert_eq!(state_for(0).oriented_size(landscape), landscape);
        assert_eq!(
            state_for(1).oriented_size(landscape),
            egui::vec2(600.0, 800.0)
        );
        assert_eq!(state_for(2).oriented_size(landscape), landscape);
        assert_eq!(
            state_for(3).oriented_size(landscape),
            egui::vec2(600.0, 800.0)
        );
    }

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

    #[test]
    fn rotating_four_times_returns_to_the_start() {
        let mut state = ViewState::default();
        for _ in 0..4 {
            state.rotate_clockwise();
        }
        assert_eq!(state.quarter_turns, 0);
    }
}
