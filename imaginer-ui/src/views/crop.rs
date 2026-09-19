//! Choosing a crop rectangle on the canvas.
//!
//! The selection is kept in *image pixels*, not screen points. Screen positions
//! change with every zoom, resize and window move, so storing them would mean the
//! crop drifting whenever the view did; pixels are what the op is finally expressed
//! in anyway.

use eframe::egui;

use crate::theme;

/// Ratio the selection is locked to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Aspect {
    #[default]
    Free,
    Square,
    FourThree,
    ThreeTwo,
    SixteenNine,
}

impl Aspect {
    pub const ALL: [Self; 5] = [
        Self::Free,
        Self::Square,
        Self::FourThree,
        Self::ThreeTwo,
        Self::SixteenNine,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Free => "Free",
            Self::Square => "1:1",
            Self::FourThree => "4:3",
            Self::ThreeTwo => "3:2",
            Self::SixteenNine => "16:9",
        }
    }

    /// Width divided by height, or `None` when the selection is unconstrained.
    pub fn ratio(self) -> Option<f32> {
        match self {
            Self::Free => None,
            Self::Square => Some(1.0),
            Self::FourThree => Some(4.0 / 3.0),
            Self::ThreeTwo => Some(3.0 / 2.0),
            Self::SixteenNine => Some(16.0 / 9.0),
        }
    }
}

/// What the pointer took hold of when the drag began.
#[derive(Debug, Clone, Copy)]
enum Grab {
    /// Pulling one corner away from the opposite one, which stays put.
    Corner { anchor: egui::Pos2 },
    /// Sliding the whole rectangle; the pointer's offset from its top-left.
    Move { offset: egui::Vec2 },
}

/// Everything the crop interaction remembers between frames.
#[derive(Debug, Default)]
pub struct CropState {
    /// The selection, in image pixels. `None` until one has been drawn.
    pub selection: Option<egui::Rect>,
    pub aspect: Aspect,
    grab: Option<Grab>,
}

impl CropState {
    pub fn new(aspect: Aspect) -> Self {
        Self {
            aspect,
            ..Self::default()
        }
    }

    /// The selection rounded to whole pixels, if it is big enough to be one.
    pub fn rectangle(&self) -> Option<(u32, u32, u32, u32)> {
        let rect = self.selection?;
        let (w, h) = (rect.width().round() as u32, rect.height().round() as u32);
        (w >= MIN_SIZE && h >= MIN_SIZE)
            .then(|| (rect.min.x.round() as u32, rect.min.y.round() as u32, w, h))
    }

    pub fn set_aspect(&mut self, aspect: Aspect, image: (u32, u32)) {
        self.aspect = aspect;
        // Re-cut the existing selection rather than dropping it: picking 16:9 after
        // roughly framing a shot should keep the framing, not start again.
        if let (Some(rect), Some(ratio)) = (self.selection, aspect.ratio()) {
            let bounds = bounds_of(image);
            self.selection = Some(fit_ratio(rect, ratio, bounds));
        }
    }
}

/// Shortest side of a usable selection, in image pixels.
const MIN_SIZE: u32 = 8;

/// Size of a corner handle, in screen points.
const HANDLE: f32 = 12.0;

/// Draw the selection over the image and let the pointer edit it.
///
/// `image` is the size of the image being cropped, in pixels; `image_rect` is where
/// the canvas put it on screen.
pub fn overlay(
    ui: &mut egui::Ui,
    state: &mut CropState,
    image: (u32, u32),
    image_rect: egui::Rect,
    canvas: egui::Rect,
) {
    let bounds = bounds_of(image);
    let scale = if image.0 > 0 {
        image_rect.width() / image.0 as f32
    } else {
        1.0
    };
    let to_screen = |p: egui::Pos2| image_rect.min + (p.to_vec2() * scale);
    let to_image = |p: egui::Pos2| {
        egui::pos2(
            (p.x - image_rect.min.x) / scale,
            (p.y - image_rect.min.y) / scale,
        )
    };

    let response = ui.interact(
        canvas,
        ui.id().with("crop_overlay"),
        egui::Sense::click_and_drag(),
    );

    if let Some(at) = response.interact_pointer_pos() {
        let at_image = to_image(at);

        if response.drag_started() {
            state.grab = Some(match state.selection {
                Some(rect) => match corner_anchor(rect, at_image, scale) {
                    Some(anchor) => Grab::Corner { anchor },
                    None if rect.contains(at_image) => Grab::Move {
                        offset: at_image - rect.min,
                    },
                    // Outside the current selection: start a new one from here.
                    None => Grab::Corner { anchor: at_image },
                },
                None => Grab::Corner { anchor: at_image },
            });
        }

        match state.grab {
            Some(Grab::Corner { anchor }) => {
                state.selection = Some(drawn(anchor, at_image, state.aspect.ratio(), bounds));
            }
            Some(Grab::Move { offset }) => {
                if let Some(rect) = state.selection {
                    state.selection = Some(shifted(rect, at_image - offset, bounds));
                }
            }
            None => {}
        }
    }

    if response.drag_stopped() {
        state.grab = None;
        // A stray click leaves a one-pixel rectangle behind; treat anything too
        // small to be meant as nothing at all.
        if state.rectangle().is_none() {
            state.selection = None;
        }
    }

    paint(ui, state, canvas, to_screen);
}

fn paint(
    ui: &egui::Ui,
    state: &CropState,
    canvas: egui::Rect,
    to_screen: impl Fn(egui::Pos2) -> egui::Pos2,
) {
    let painter = ui.painter_at(canvas);
    let shade = egui::Color32::from_black_alpha(140);

    let Some(selection) = state.selection else {
        // Nothing selected yet: dim everything, so it is visibly a mode waiting for
        // a rectangle rather than a canvas that has stopped responding.
        painter.rect_filled(canvas, 0.0, egui::Color32::from_black_alpha(90));
        return;
    };

    let rect = egui::Rect::from_min_max(to_screen(selection.min), to_screen(selection.max));

    // Four bands around the selection rather than one shape with a hole, which is
    // what a painter without even-odd fill can actually express.
    for band in [
        egui::Rect::from_min_max(canvas.left_top(), egui::pos2(canvas.right(), rect.top())),
        egui::Rect::from_min_max(
            egui::pos2(canvas.left(), rect.bottom()),
            canvas.right_bottom(),
        ),
        egui::Rect::from_min_max(egui::pos2(canvas.left(), rect.top()), rect.left_bottom()),
        egui::Rect::from_min_max(rect.right_top(), egui::pos2(canvas.right(), rect.bottom())),
    ] {
        if band.is_positive() {
            painter.rect_filled(band, 0.0, shade);
        }
    }

    painter.rect_stroke(
        rect,
        0.0,
        egui::Stroke::new(1.0, theme::TEXT_PRIMARY),
        egui::StrokeKind::Inside,
    );

    // Thirds, the one guide that earns its ink: it is what people are actually
    // lining up against when they reframe a photograph.
    let thirds = egui::Stroke::new(1.0, theme::TEXT_PRIMARY.gamma_multiply(0.28));
    for i in 1..3 {
        let t = i as f32 / 3.0;
        let x = rect.left() + rect.width() * t;
        let y = rect.top() + rect.height() * t;
        painter.vline(x, rect.y_range(), thirds);
        painter.hline(rect.x_range(), y, thirds);
    }

    // The size in pixels, which is the number anyone cropping to a target is
    // working towards and the one thing the overlay could not tell them. In
    // image pixels, not screen points: a crop is a promise about the file, and
    // the same selection at 50% zoom and at 200% would otherwise read as two
    // different sizes.
    size_readout(&painter, ui, rect, selection);

    for corner in [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
    ] {
        painter.rect_filled(
            egui::Rect::from_center_size(corner, egui::Vec2::splat(HANDLE)),
            2.0,
            theme::TEXT_PRIMARY,
        );
    }
}

/// Draw `w × h` beside the selection, in image pixels.
///
/// Above the top-left corner, and inside the selection when there is no room
/// above — a crop dragged to the top of the canvas should not push its own
/// label off the screen.
fn size_readout(
    painter: &egui::Painter,
    ui: &egui::Ui,
    rect: egui::Rect,
    selection: egui::Rect,
) {
    const PAD: egui::Vec2 = egui::vec2(6.0, 3.0);
    const MARGIN: f32 = 6.0;

    let text = format!(
        "{} × {}",
        selection.width().round() as u32,
        selection.height().round() as u32
    );
    let galley = painter.layout_no_wrap(
        text,
        egui::TextStyle::Small.resolve(ui.style()),
        theme::TEXT_PRIMARY,
    );

    let size = galley.size() + PAD * 2.0;
    let above = rect.top() - MARGIN - size.y;
    let top_left = if above >= painter.clip_rect().top() {
        egui::pos2(rect.left(), above)
    } else {
        egui::pos2(rect.left() + MARGIN, rect.top() + MARGIN)
    };

    // A backdrop, because this sits over a photograph and white text on a white
    // sky is not a readout.
    let panel = egui::Rect::from_min_size(top_left, size);
    painter.rect_filled(panel, 3.0, egui::Color32::from_black_alpha(160));
    painter.galley(panel.min + PAD, galley, theme::TEXT_PRIMARY);
}

fn bounds_of(image: (u32, u32)) -> egui::Rect {
    egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(image.0 as f32, image.1 as f32))
}

/// Which corner is being grabbed, expressed as the corner that stays put.
fn corner_anchor(rect: egui::Rect, at: egui::Pos2, scale: f32) -> Option<egui::Pos2> {
    // The handle is drawn in screen points, so its reach in image pixels depends on
    // the zoom: on a downscaled 24MP photo one handle covers a lot of pixels.
    let reach = (HANDLE / scale.max(f32::EPSILON)).max(1.0);

    let corners = [
        (rect.left_top(), rect.right_bottom()),
        (rect.right_top(), rect.left_bottom()),
        (rect.right_bottom(), rect.left_top()),
        (rect.left_bottom(), rect.right_top()),
    ];
    corners
        .into_iter()
        .find(|(corner, _)| corner.distance(at) <= reach)
        .map(|(_, opposite)| opposite)
}

/// The rectangle a drag from `anchor` to `corner` describes.
fn drawn(
    anchor: egui::Pos2,
    corner: egui::Pos2,
    ratio: Option<f32>,
    bounds: egui::Rect,
) -> egui::Rect {
    let anchor = clamp_pos(anchor, bounds);
    let corner = clamp_pos(corner, bounds);

    let Some(ratio) = ratio else {
        return egui::Rect::from_two_pos(anchor, corner);
    };

    // Take whichever axis the pointer pulled further, so the locked ratio follows
    // the gesture rather than fighting it.
    let (dx, dy) = (corner.x - anchor.x, corner.y - anchor.y);
    let mut width = dx.abs().max(dy.abs() * ratio);
    let mut height = width / ratio;

    // Then shrink until it fits, keeping the anchor where it is.
    let room_x = if dx < 0.0 {
        anchor.x - bounds.left()
    } else {
        bounds.right() - anchor.x
    };
    let room_y = if dy < 0.0 {
        anchor.y - bounds.top()
    } else {
        bounds.bottom() - anchor.y
    };
    let fit = (room_x / width.max(f32::EPSILON))
        .min(room_y / height.max(f32::EPSILON))
        .min(1.0);
    width *= fit;
    height *= fit;

    let sign = |d: f32| if d < 0.0 { -1.0 } else { 1.0 };
    egui::Rect::from_two_pos(
        anchor,
        anchor + egui::vec2(width * sign(dx), height * sign(dy)),
    )
}

/// Move `rect` so its top-left sits at `to`, without leaving `bounds`.
fn shifted(rect: egui::Rect, to: egui::Pos2, bounds: egui::Rect) -> egui::Rect {
    let size = rect.size();
    let min = egui::pos2(
        to.x.clamp(bounds.left(), (bounds.right() - size.x).max(bounds.left())),
        to.y.clamp(bounds.top(), (bounds.bottom() - size.y).max(bounds.top())),
    );
    egui::Rect::from_min_size(min, size)
}

/// Re-cut an existing selection to `ratio`, keeping its centre where possible.
fn fit_ratio(rect: egui::Rect, ratio: f32, bounds: egui::Rect) -> egui::Rect {
    let (mut width, mut height) = (rect.width(), rect.height());
    if width / height > ratio {
        width = height * ratio;
    } else {
        height = width / ratio;
    }

    let fit = (bounds.width() / width.max(f32::EPSILON))
        .min(bounds.height() / height.max(f32::EPSILON))
        .min(1.0);
    let size = egui::vec2(width * fit, height * fit);

    shifted(
        egui::Rect::from_min_size(egui::Pos2::ZERO, size),
        rect.center() - size * 0.5,
        bounds,
    )
}

fn clamp_pos(p: egui::Pos2, bounds: egui::Rect) -> egui::Pos2 {
    egui::pos2(
        p.x.clamp(bounds.left(), bounds.right()),
        p.y.clamp(bounds.top(), bounds.bottom()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds() -> egui::Rect {
        bounds_of((100, 80))
    }

    #[test]
    fn a_free_drag_is_the_rectangle_between_the_two_corners() {
        let rect = drawn(
            egui::pos2(10.0, 10.0),
            egui::pos2(40.0, 30.0),
            None,
            bounds(),
        );
        assert_eq!(
            rect,
            egui::Rect::from_min_max(egui::pos2(10.0, 10.0), egui::pos2(40.0, 30.0))
        );
    }

    #[test]
    fn dragging_backwards_still_gives_a_positive_rectangle() {
        let rect = drawn(
            egui::pos2(40.0, 30.0),
            egui::pos2(10.0, 10.0),
            None,
            bounds(),
        );
        assert_eq!(rect.min, egui::pos2(10.0, 10.0));
        assert_eq!(rect.max, egui::pos2(40.0, 30.0));
    }

    #[test]
    fn a_locked_ratio_is_honoured_whichever_way_the_pointer_went() {
        let rect = drawn(
            egui::pos2(0.0, 0.0),
            egui::pos2(60.0, 5.0),
            Some(16.0 / 9.0),
            bounds(),
        );
        assert!((rect.width() / rect.height() - 16.0 / 9.0).abs() < 0.01);
        // Followed the axis that was pulled further.
        assert!((rect.width() - 60.0).abs() < 0.01);
    }

    #[test]
    fn a_locked_ratio_shrinks_rather_than_leaving_the_image() {
        // 16:9 from x=60 would want 160 wide; only 40 is left.
        let rect = drawn(
            egui::pos2(60.0, 0.0),
            egui::pos2(220.0, 90.0),
            Some(16.0 / 9.0),
            bounds(),
        );
        assert!(bounds().contains_rect(rect), "{rect:?} left the image");
        assert!((rect.width() / rect.height() - 16.0 / 9.0).abs() < 0.01);
    }

    #[test]
    fn a_drag_beyond_the_edge_is_clamped_to_the_image() {
        let rect = drawn(
            egui::pos2(90.0, 70.0),
            egui::pos2(500.0, 500.0),
            None,
            bounds(),
        );
        assert_eq!(rect.max, egui::pos2(100.0, 80.0));
    }

    #[test]
    fn moving_a_selection_stops_at_the_edge() {
        let rect = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(20.0, 20.0));
        let moved = shifted(rect, egui::pos2(95.0, 75.0), bounds());
        assert_eq!(moved.size(), rect.size());
        assert_eq!(moved.max, egui::pos2(100.0, 80.0));
    }

    #[test]
    fn grabbing_near_a_corner_anchors_the_opposite_one() {
        let rect = egui::Rect::from_min_max(egui::pos2(10.0, 10.0), egui::pos2(50.0, 40.0));
        // Near the top-left, so the bottom-right should stay put.
        let anchor = corner_anchor(rect, egui::pos2(11.0, 11.0), 1.0);
        assert_eq!(anchor, Some(egui::pos2(50.0, 40.0)));
        // Nowhere near any corner.
        assert_eq!(corner_anchor(rect, egui::pos2(30.0, 25.0), 1.0), None);
    }

    #[test]
    fn switching_to_a_ratio_keeps_the_selection_centred() {
        let rect = egui::Rect::from_min_size(egui::pos2(20.0, 20.0), egui::vec2(40.0, 40.0));
        let cut = fit_ratio(rect, 2.0, bounds());
        assert!((cut.width() / cut.height() - 2.0).abs() < 0.01);
        assert!((cut.center().x - rect.center().x).abs() < 0.01);
    }

    fn selecting(rect: egui::Rect) -> CropState {
        CropState {
            selection: Some(rect),
            ..CropState::default()
        }
    }

    #[test]
    fn a_selection_smaller_than_a_stray_click_is_not_a_rectangle() {
        let tiny = selecting(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(2.0, 2.0),
        ));
        assert_eq!(tiny.rectangle(), None);

        let real = selecting(egui::Rect::from_min_size(
            egui::pos2(3.0, 4.0),
            egui::vec2(20.0, 10.0),
        ));
        assert_eq!(real.rectangle(), Some((3, 4, 20, 10)));
    }
}
