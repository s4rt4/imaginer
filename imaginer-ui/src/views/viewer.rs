//! The main canvas: pan, zoom, and the navigation chevrons that float over it.

use eframe::egui;
use imaginer_core::Adjust;

use crate::icons::{self, Icon, Icons};
use crate::shader::AdjustShader;
use crate::texture::{Backdrop, ImageTexture};
use crate::theme;
use crate::vector;

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
    backdrop: Option<&Backdrop>,
    tile: Option<&vector::Tile>,
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

    // The transparency grid, under a picture with see-through pixels. One draw
    // call: a two-by-two checker texture with wrapping UVs that run past 1.0, so
    // the GPU tiles it and the squares stay a fixed size on screen however the
    // picture is panned or zoomed — Photoshop's behaviour, not a pattern baked
    // into the image that would zoom with it.
    if texture.has_transparency
        && let Some(backdrop) = backdrop
    {
        let uv_max = image_rect.size() / backdrop.period;
        painter.image(
            backdrop.handle.id(),
            image_rect,
            egui::Rect::from_min_size(egui::Pos2::ZERO, uv_max),
            egui::Color32::WHITE,
        );
    }

    // The shader is only worth reaching for when it has something to do, and it is
    // skipped entirely if it could not be built — on hardware that cannot compile
    // it, the adjustment still reaches the saved file, it just is not previewed.
    // Taking the painter rather than closing over one: the bands around a vector
    // patch are each drawn through their own clipped painter, and that is the same
    // draw in every other respect.
    let draw = |painter: &egui::Painter, into: egui::Rect, id: egui::TextureId| {
        if colour.adjust.is_none() || colour.shader.failed() {
            painter.image(
                id,
                into,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        } else {
            painter.add(colour.shader.callback(rect, into, id, colour.adjust));
        }
    };

    // A vector patch replaces the raster over the part it covers, rather than
    // being laid on top of it. It has to: an SVG has transparent pixels, and at
    // these zooms the raster's antialiased edge is a row of half-see-through
    // blocks the size of a thumbnail. Drawn underneath, that edge would show
    // straight through the patch's transparency as a staircase ghost around
    // every sharp one. So the raster is drawn in the bands *around* the patch —
    // which is also what covers the gap between a pan and the patch catching up
    // with it, and is nothing at all once the patch covers the window.
    match tile {
        None => draw(&painter, image_rect, texture.handle.id()),
        Some(tile) => {
            let region = tile.patch.region;
            let at = |point: egui::Pos2| image_rect.min + point.to_vec2() * zoom;
            let patch_rect = egui::Rect::from_min_max(at(region.min), at(region.max));

            for band in around(rect, patch_rect) {
                draw(&ui.painter_at(band), image_rect, texture.handle.id());
            }

            draw(&painter, patch_rect, tile.texture.id());
        }
    }

    Shown { zoom, image_rect }
}

/// The logo's own two colours, read off `assets/imaginer_logoicon.svg`.
///
/// Not the theme's accent, which is a blue chosen to sit quietly behind
/// photographs. This one line is the app signing its name, so it wears the
/// logo — and it is the only place in the chrome that does.
const LOGO_RED: egui::Color32 = egui::Color32::from_rgb(0xdc, 0x23, 0x3d);
const LOGO_YELLOW: egui::Color32 = egui::Color32::from_rgb(0xf4, 0xcf, 0x48);

/// How thick the slideshow's progress line is, in points.
const PROGRESS_THICKNESS: f32 = 3.0;

/// Gap between two segments of that line, in points.
const PROGRESS_GAP: f32 = 2.0;

/// Most segments the line is ever cut into.
///
/// A one-second segment is the natural unit — the hold is set in seconds — but a
/// sixty-second slide would be sixty slivers on a bar a window wide, which reads
/// as a dotted line rather than as a count. Past this the segments cover two
/// seconds each, then three, and stay legible.
const PROGRESS_MAX_SEGMENTS: u32 = 24;

/// How many segments a hold of `seconds` is drawn as.
///
/// Public because the repaint schedule has to agree with the drawing: the app
/// asks for exactly one frame per segment, so if these two disagreed the line
/// would either sit stale or ask for frames nobody sees.
pub fn slideshow_segments(seconds: u32) -> u32 {
    seconds.clamp(1, PROGRESS_MAX_SEGMENTS)
}

/// A row of segments across the foot of the canvas, one lighting up per step.
///
/// **Deliberately not a sliding bar.** The first version filled continuously and
/// asked for thirty frames a second to do it, which was both visibly juddery and
/// exactly the "stream of back-to-back presents" that [`crate::idle`] records as
/// the cause of a black-canvas flicker on this machine. Stepping needs one frame
/// per segment — four frames for the default four-second hold — and a step that
/// lands in one jump cannot judder, because there is nothing between one
/// position and the next to be uneven about.
///
/// It also answers the two questions better than a smooth bar did: how long each
/// photo holds is the number of segments, and how long is left is how many are
/// still dark. A sliding bar only ever showed a proportion.
///
/// Drawn along the bottom edge rather than in the status bar because the status
/// bar hides itself in fullscreen, which is exactly where a slideshow is watched.
/// The colour runs from the logo's red to its yellow across the whole row, so a
/// glance at the colour of the last lit segment says how far in this is.
pub fn slideshow_progress(ui: &egui::Ui, canvas: egui::Rect, filled: u32, total: u32) {
    if total == 0 || canvas.width() <= 0.0 {
        return;
    }

    let painter = ui.painter_at(canvas);
    let span = canvas.width() / total as f32;
    let top = canvas.bottom() - PROGRESS_THICKNESS;

    for segment in 0..filled.min(total) {
        let left = canvas.left() + span * segment as f32;
        // The gap comes off the right of each segment, so the row starts flush
        // with the canvas edge and the last lit segment does not appear to float.
        let rect = egui::Rect::from_min_max(
            egui::pos2(left, top),
            egui::pos2(left + (span - PROGRESS_GAP).max(1.0), canvas.bottom()),
        );
        let through = if total > 1 {
            segment as f32 / (total - 1) as f32
        } else {
            0.0
        };
        painter.rect_filled(rect, 0.0, lerp_colour(LOGO_RED, LOGO_YELLOW, through));
    }
}

/// Mix two colours, `t` of the way from `from` to `to`.
fn lerp_colour(from: egui::Color32, to: egui::Color32, t: f32) -> egui::Color32 {
    let mix = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
    egui::Color32::from_rgb(
        mix(from.r(), to.r()),
        mix(from.g(), to.g()),
        mix(from.b(), to.b()),
    )
}

/// Say that letting go here will open the file being dragged.
///
/// Dropping already worked; nothing said so. A window that takes a file but
/// gives no sign it is willing to reads as a window that will not, so people
/// drop onto the taskbar icon instead, or give up and use the Open dialog.
///
/// Drawn over the picture rather than replacing it, and only while something is
/// actually hovering: the image stays visible underneath, which is what makes it
/// obvious *which* window is about to take the file.
pub fn drop_hint(ui: &egui::Ui, canvas: egui::Rect, name: Option<&str>) {
    let painter = ui.painter_at(canvas);
    painter.rect_filled(canvas, 0.0, egui::Color32::from_black_alpha(140));

    let inset = canvas.shrink(10.0);
    painter.rect_stroke(
        inset,
        6.0,
        egui::Stroke::new(2.0, LOGO_YELLOW),
        egui::StrokeKind::Inside,
    );

    // The file's own name when the shell gives us one, because a drag over a
    // viewer is nearly always "is this the right file" rather than "will this
    // work at all".
    let text = match name {
        Some(name) => format!("Drop to open {name}"),
        None => "Drop to open".to_owned(),
    };
    painter.text(
        canvas.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::TextStyle::Heading.resolve(ui.style()),
        theme::TEXT_PRIMARY,
    );
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

/// The parts of `outer` that `hole` does not cover, as up to four rectangles.
///
/// Empty when the hole covers everything, which is the common case while looking
/// at a vector patch and is what stops the raster being drawn at all.
fn around(outer: egui::Rect, hole: egui::Rect) -> Vec<egui::Rect> {
    let hole = hole.intersect(outer);
    if hole.width() <= 0.0 || hole.height() <= 0.0 {
        return vec![outer];
    }

    let bands = [
        egui::Rect::from_min_max(outer.min, egui::pos2(outer.right(), hole.top())),
        egui::Rect::from_min_max(egui::pos2(outer.left(), hole.bottom()), outer.max),
        egui::Rect::from_min_max(
            egui::pos2(outer.left(), hole.top()),
            egui::pos2(hole.left(), hole.bottom()),
        ),
        egui::Rect::from_min_max(
            egui::pos2(hole.right(), hole.top()),
            egui::pos2(outer.right(), hole.bottom()),
        ),
    ];

    bands
        .into_iter()
        .filter(|band| band.width() > 0.0 && band.height() > 0.0)
        .collect()
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
    fn a_patch_covering_the_window_leaves_no_raster_to_draw() {
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        assert!(around(canvas, canvas.expand(50.0)).is_empty());
    }

    #[test]
    fn a_patch_in_the_middle_leaves_four_bands_that_tile_the_rest() {
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let hole = egui::Rect::from_min_size(egui::pos2(200.0, 100.0), egui::vec2(300.0, 300.0));
        let bands = around(canvas, hole);

        assert_eq!(bands.len(), 4);
        let area: f32 = bands.iter().map(|b| b.width() * b.height()).sum();
        assert_eq!(
            area,
            canvas.width() * canvas.height() - hole.width() * hole.height(),
            "the bands should cover exactly what the patch does not"
        );
        for band in &bands {
            assert!(
                band.intersect(hole).width() <= 0.0 || band.intersect(hole).height() <= 0.0,
                "no band may overlap the patch: {band:?}"
            );
        }
    }

    #[test]
    fn a_patch_spanning_one_whole_side_leaves_a_single_band() {
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let hole = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(300.0, 600.0));
        assert_eq!(around(canvas, hole).len(), 1, "only the strip to its right");
    }

    #[test]
    fn a_patch_nowhere_near_the_canvas_leaves_it_whole() {
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let hole = egui::Rect::from_min_size(egui::pos2(2000.0, 2000.0), egui::vec2(10.0, 10.0));
        assert_eq!(around(canvas, hole), vec![canvas]);
    }

    #[test]
    fn a_hold_is_one_segment_per_second_until_that_stops_being_readable() {
        assert_eq!(slideshow_segments(4), 4, "the default hold, a segment a second");
        assert_eq!(slideshow_segments(1), 1);
        assert_eq!(slideshow_segments(24), 24);
        assert_eq!(
            slideshow_segments(60),
            PROGRESS_MAX_SEGMENTS,
            "a minute is not sixty slivers"
        );
        // Zero would divide by itself in the drawing; the setting cannot be zero
        // but the clamp is what makes that a fact rather than a hope.
        assert_eq!(slideshow_segments(0), 1);
    }

    #[test]
    fn the_progress_colour_runs_from_one_logo_colour_to_the_other() {
        assert_eq!(lerp_colour(LOGO_RED, LOGO_YELLOW, 0.0), LOGO_RED);
        assert_eq!(lerp_colour(LOGO_RED, LOGO_YELLOW, 1.0), LOGO_YELLOW);

        // And halfway is between them on every channel, which is what makes the
        // leading edge readable as "how far through the slide is this".
        let middle = lerp_colour(LOGO_RED, LOGO_YELLOW, 0.5);
        assert!(middle.r() > LOGO_RED.r() && middle.r() < LOGO_YELLOW.r());
        assert!(middle.g() > LOGO_RED.g() && middle.g() < LOGO_YELLOW.g());
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
