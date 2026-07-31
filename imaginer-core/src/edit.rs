//! The non-destructive edit pipeline.
//!
//! Edits are an ordered list of operations, not mutable fields on the image. That
//! is what makes undo a matter of dropping the last entry rather than of keeping
//! copies of pixels around, and it is why the preview and the exported file cannot
//! disagree: both run this same list over the same decoded pixels.
//!
//! Nothing here touches the file on disk. The original is only ever read.

use std::borrow::Cow;

use image::RgbaImage;

use crate::adjust::Adjust;

/// One step in the pipeline.
///
/// The split that matters is not flip-versus-rotate but whether an op changes the
/// image's dimensions: a size-changing op means the canvas has to be re-fitted and
/// the zoom recomputed, while a size-preserving one can swap the texture underneath
/// an unchanged view. [`Op::changes_size`] is what the UI asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// Mirror the pixels left-to-right, in place.
    FlipHorizontal,
    /// Mirror the pixels top-to-bottom, in place.
    FlipVertical,
    /// A quarter turn clockwise.
    RotateCw,
    /// A quarter turn anticlockwise.
    RotateCcw,
    /// Place the image beside its own reflection, doubling the width.
    ///
    /// Not a flip: a flip turns the image over and leaves the canvas alone, while
    /// this keeps the original and adds the reflection next to it.
    MirrorHorizontal,
    /// Place the image below its own reflection, doubling the height.
    MirrorVertical,
    /// Keep a rectangle of the image and discard the rest.
    ///
    /// In pixels of the image *as the pipeline reaches this step*, not of the file
    /// on disk — a crop after a rotate is measured against the rotated image, which
    /// is the only reading that survives an undo of the step before it.
    Crop {
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    },
    /// Brightness, contrast and saturation, as absolute settings.
    ///
    /// Absolute, not a delta, because that is what a slider reports — which means
    /// two of these in a row must not compose, or dragging a slider twice would
    /// apply it twice. [`Edits::apply`] runs only the last one; see the note there.
    Adjust(Adjust),
}

impl Op {
    /// Whether applying this op changes the image's dimensions.
    pub fn changes_size(self) -> bool {
        !matches!(
            self,
            Self::FlipHorizontal | Self::FlipVertical | Self::Adjust(_)
        )
    }

    /// Dimensions this op produces from `size`.
    ///
    /// Kept separate from [`Op::apply`] so the export panel can report the output
    /// size without doing the work of producing it.
    pub fn size_after(self, size: (u32, u32)) -> (u32, u32) {
        let (w, h) = size;
        match self {
            Self::FlipHorizontal | Self::FlipVertical | Self::Adjust(_) => (w, h),
            Self::RotateCw | Self::RotateCcw => (h, w),
            Self::MirrorHorizontal => (w.saturating_mul(2), h),
            Self::MirrorVertical => (w, h.saturating_mul(2)),
            Self::Crop { .. } => {
                let (_, _, cw, ch) = self.clamped_crop(size);
                (cw, ch)
            }
        }
    }

    /// A crop rectangle trimmed to what actually exists in an image of `size`.
    ///
    /// The rectangle is stored as the user drew it, but the image under it can
    /// change afterwards — undoing the rotate that came before it, say. Rather than
    /// invalidating the crop, it is clamped: an op that survives editing of the ops
    /// before it is far less surprising than one that silently disappears, and a
    /// crop that fell entirely outside would panic the underlying view.
    fn clamped_crop(self, size: (u32, u32)) -> (u32, u32, u32, u32) {
        let Self::Crop {
            x,
            y,
            width,
            height,
        } = self
        else {
            return (0, 0, size.0, size.1);
        };

        let x = x.min(size.0);
        let y = y.min(size.1);
        (x, y, width.min(size.0 - x), height.min(size.1 - y))
    }

    fn apply(self, src: &RgbaImage) -> RgbaImage {
        use image::imageops;
        match self {
            Self::FlipHorizontal => imageops::flip_horizontal(src),
            Self::FlipVertical => imageops::flip_vertical(src),
            Self::RotateCw => imageops::rotate90(src),
            Self::RotateCcw => imageops::rotate270(src),
            Self::MirrorHorizontal => mirror(src, false),
            Self::MirrorVertical => mirror(src, true),
            Self::Crop { .. } => {
                let (x, y, width, height) = self.clamped_crop(src.dimensions());
                if width == 0 || height == 0 {
                    // Nothing left to keep. Handing back the original beats handing
                    // back an image with a zero dimension, which nothing downstream
                    // — texture upload, encoders — is prepared for.
                    return src.clone();
                }
                imageops::crop_imm(src, x, y, width, height).to_image()
            }
            Self::Adjust(adjust) => {
                let mut out = src.clone();
                adjust.apply(&mut out);
                out
            }
        }
    }
}

/// The smallest rectangle containing every pixel that is not fully transparent,
/// as `(x, y, width, height)`.
///
/// `None` when the image is entirely transparent, because there is then no
/// rectangle to keep and cropping to nothing is not an improvement. Returns the
/// whole image when nothing is transparent at all — the common case for a
/// photograph, and worth checking before pushing a crop that would do nothing.
///
/// Deliberately not an [`Op`]. Every op has to be able to report its output size
/// from an input size alone, so the export panel can predict dimensions without
/// doing the work; a trim's result depends on the pixels, not on the size. So the
/// caller measures once and pushes an ordinary [`Op::Crop`], which keeps the
/// pipeline predictable and makes the undo stack say what actually happened.
pub fn opaque_bounds(pixels: &RgbaImage) -> Option<(u32, u32, u32, u32)> {
    let (mut min_x, mut min_y) = (u32::MAX, u32::MAX);
    let (mut max_x, mut max_y) = (0u32, 0u32);
    let mut found = false;

    for (x, y, pixel) in pixels.enumerate_pixels() {
        if pixel.0[3] == 0 {
            continue;
        }
        found = true;
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
    }

    found.then(|| (min_x, min_y, max_x - min_x + 1, max_y - min_y + 1))
}

/// The image beside (or below) its own reflection.
fn mirror(src: &RgbaImage, vertical: bool) -> RgbaImage {
    let (w, h) = src.dimensions();
    let (out_w, out_h) = if vertical { (w, h * 2) } else { (w * 2, h) };

    let mut out = RgbaImage::new(out_w, out_h);
    image::imageops::replace(&mut out, src, 0, 0);

    let reflection = if vertical {
        image::imageops::flip_vertical(src)
    } else {
        image::imageops::flip_horizontal(src)
    };
    let (dx, dy) = if vertical {
        (0, h as i64)
    } else {
        (w as i64, 0)
    };
    image::imageops::replace(&mut out, &reflection, dx, dy);

    out
}

/// The ordered stack of edits, with undo and redo.
#[derive(Debug, Default, Clone)]
pub struct Edits {
    applied: Vec<Op>,
    /// Ops that were undone, newest last. Kept so redo is possible, and discarded
    /// the moment a new op is pushed — a redo of a branch you have left is a
    /// promise no editor keeps.
    undone: Vec<Op>,
}

impl Edits {
    pub fn push(&mut self, op: Op) {
        self.applied.push(op);
        self.undone.clear();
    }

    /// Move the last op onto the redo stack. Returns whether anything moved.
    pub fn undo(&mut self) -> bool {
        match self.applied.pop() {
            Some(op) => {
                self.undone.push(op);
                true
            }
            None => false,
        }
    }

    pub fn redo(&mut self) -> bool {
        match self.undone.pop() {
            Some(op) => {
                self.applied.push(op);
                true
            }
            None => false,
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.applied.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.undone.is_empty()
    }

    /// How many edits are pending, which is also how many changes are unsaved.
    pub fn len(&self) -> usize {
        self.applied.len()
    }

    pub fn is_empty(&self) -> bool {
        self.applied.is_empty()
    }

    pub fn ops(&self) -> &[Op] {
        &self.applied
    }

    /// The colour adjustment currently in force, which is the last one pushed.
    ///
    /// What the sliders read when an image is opened, and what they return to after
    /// an undo — the setting before the one just removed, not zero.
    pub fn adjust(&self) -> Adjust {
        self.applied
            .iter()
            .rev()
            .find_map(|op| match op {
                Op::Adjust(adjust) => Some(*adjust),
                _ => None,
            })
            .unwrap_or(Adjust::NONE)
    }

    /// This pipeline with `adjust` in place of whatever adjustment it holds.
    ///
    /// For previewing a slider mid-drag, which must not touch the undo stack — a
    /// drag from 0 to 40 passes through every value in between, and none of those
    /// are steps anybody wants to undo through.
    pub fn previewing(&self, adjust: Adjust) -> Self {
        let mut previewed = self.clone();
        // Simply pushing is enough: only the last adjustment runs.
        previewed.applied.push(Op::Adjust(adjust));
        previewed
    }

    /// The ops that actually run.
    ///
    /// Adjustments carry absolute slider values, so a stack holding three of them
    /// records that the sliders moved three times — not that the image should be
    /// adjusted three times over. Only the last one survives here, which is exactly
    /// what makes undo step back through the earlier settings instead of jumping
    /// straight to none. One that has been dragged back to zero is dropped as well,
    /// so an image returned to its original settings costs no work at all.
    ///
    /// Running the survivor where it sits rather than at the end of the list is safe
    /// because every other op only rearranges pixels: adjust-then-rotate and
    /// rotate-then-adjust produce the same picture.
    fn effective(&self) -> Vec<Op> {
        let last_adjust = self
            .applied
            .iter()
            .rposition(|op| matches!(op, Op::Adjust(_)));

        self.applied
            .iter()
            .enumerate()
            .filter(|(index, op)| match op {
                Op::Adjust(adjust) => Some(*index) == last_adjust && !adjust.is_none(),
                _ => true,
            })
            .map(|(_, op)| *op)
            .collect()
    }

    /// Forget everything, as after a save or a new image.
    pub fn clear(&mut self) {
        self.applied.clear();
        self.undone.clear();
    }

    /// Dimensions the pipeline produces from `size`.
    pub fn size_after(&self, size: (u32, u32)) -> (u32, u32) {
        self.applied
            .iter()
            .fold(size, |size, op| op.size_after(size))
    }

    /// Run the pipeline over `base`.
    ///
    /// Borrows when there is nothing to do, which is the common case — an image
    /// being looked at rather than edited must not pay for a copy of itself every
    /// time something asks for the current pixels.
    pub fn apply<'a>(&self, base: &'a RgbaImage) -> Cow<'a, RgbaImage> {
        run(&self.effective(), base)
    }

    /// Run only the steps that move pixels about, leaving colour alone.
    ///
    /// What the preview texture is built from. The colour adjustment is applied by a
    /// shader at draw time instead, because on a 24MP image it costs 400ms on the CPU
    /// and a slider being dragged has 16ms — measured, see `adjust::tests::adjust_costs`.
    /// Keeping it out of the texture also means undoing an adjustment changes a
    /// uniform rather than re-running the pipeline.
    ///
    /// Every path that writes a file uses [`Edits::apply`], which does include it.
    pub fn apply_geometry<'a>(&self, base: &'a RgbaImage) -> Cow<'a, RgbaImage> {
        let ops: Vec<Op> = self
            .applied
            .iter()
            .copied()
            .filter(|op| !matches!(op, Op::Adjust(_)))
            .collect();
        run(&ops, base)
    }
}

fn run<'a>(ops: &[Op], base: &'a RgbaImage) -> Cow<'a, RgbaImage> {
    let Some((first, rest)) = ops.split_first() else {
        return Cow::Borrowed(base);
    };

    let mut image = first.apply(base);
    for op in rest {
        image = op.apply(&image);
    }
    Cow::Owned(image)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small image with no symmetry, so any flip or rotation is detectable.
    fn asymmetric() -> RgbaImage {
        RgbaImage::from_fn(4, 2, |x, y| {
            image::Rgba([(x * 60) as u8, (y * 90) as u8, 7, 255])
        })
    }

    #[test]
    fn flipping_twice_returns_the_original() {
        let src = asymmetric();
        let mut edits = Edits::default();
        edits.push(Op::FlipHorizontal);
        edits.push(Op::FlipHorizontal);

        assert_eq!(*edits.apply(&src), src);
    }

    #[test]
    fn four_quarter_turns_return_the_original() {
        let src = asymmetric();
        let mut edits = Edits::default();
        for _ in 0..4 {
            edits.push(Op::RotateCw);
        }

        assert_eq!(*edits.apply(&src), src);
    }

    #[test]
    fn opposite_rotations_cancel() {
        let src = asymmetric();
        let mut edits = Edits::default();
        edits.push(Op::RotateCw);
        edits.push(Op::RotateCcw);

        assert_eq!(*edits.apply(&src), src);
    }

    #[test]
    fn mirroring_doubles_the_width_and_reflects_the_right_half() {
        let src = asymmetric();
        let mut edits = Edits::default();
        edits.push(Op::MirrorHorizontal);
        let out = edits.apply(&src).into_owned();

        assert_eq!(out.dimensions(), (8, 2));
        for y in 0..2 {
            for x in 0..4 {
                assert_eq!(
                    out.get_pixel(x, y),
                    src.get_pixel(x, y),
                    "left half at {x},{y}"
                );
                // The right half runs backwards: column 4 mirrors column 3.
                assert_eq!(
                    out.get_pixel(7 - x, y),
                    src.get_pixel(x, y),
                    "right half at {x},{y}"
                );
            }
        }
    }

    #[test]
    fn mirroring_vertically_doubles_the_height() {
        let src = asymmetric();
        let mut edits = Edits::default();
        edits.push(Op::MirrorVertical);
        let out = edits.apply(&src).into_owned();

        assert_eq!(out.dimensions(), (4, 4));
        assert_eq!(out.get_pixel(0, 3), src.get_pixel(0, 0));
    }

    #[test]
    fn predicted_size_matches_what_apply_produces() {
        let src = asymmetric();
        let mut edits = Edits::default();
        edits.push(Op::RotateCw);
        edits.push(Op::MirrorHorizontal);
        edits.push(Op::FlipVertical);

        let predicted = edits.size_after(src.dimensions());
        assert_eq!(edits.apply(&src).dimensions(), predicted);
    }

    #[test]
    fn an_empty_pipeline_borrows_rather_than_copying() {
        let src = asymmetric();
        assert!(matches!(Edits::default().apply(&src), Cow::Borrowed(_)));
    }

    #[test]
    fn undo_and_redo_walk_the_stack() {
        let mut edits = Edits::default();
        edits.push(Op::RotateCw);
        edits.push(Op::FlipVertical);

        assert!(edits.undo());
        assert_eq!(edits.ops(), [Op::RotateCw]);
        assert!(edits.redo());
        assert_eq!(edits.ops(), [Op::RotateCw, Op::FlipVertical]);

        assert!(edits.undo());
        assert!(edits.undo());
        assert!(!edits.undo());
        assert!(!edits.can_undo());
    }

    #[test]
    fn a_new_op_discards_the_redo_branch() {
        let mut edits = Edits::default();
        edits.push(Op::RotateCw);
        edits.undo();
        assert!(edits.can_redo());

        edits.push(Op::FlipHorizontal);
        assert!(!edits.can_redo());
        assert_eq!(edits.ops(), [Op::FlipHorizontal]);
    }

    #[test]
    fn cropping_keeps_the_rectangle_that_was_asked_for() {
        let src = asymmetric();
        let mut edits = Edits::default();
        edits.push(Op::Crop {
            x: 1,
            y: 0,
            width: 2,
            height: 2,
        });
        let out = edits.apply(&src).into_owned();

        assert_eq!(out.dimensions(), (2, 2));
        assert_eq!(out.get_pixel(0, 0), src.get_pixel(1, 0));
        assert_eq!(out.get_pixel(1, 1), src.get_pixel(2, 1));
    }

    #[test]
    fn a_crop_reaching_past_the_edge_is_trimmed_rather_than_fatal() {
        let src = asymmetric(); // 4x2
        let mut edits = Edits::default();
        edits.push(Op::Crop {
            x: 3,
            y: 1,
            width: 999,
            height: 999,
        });

        let predicted = edits.size_after(src.dimensions());
        let out = edits.apply(&src).into_owned();
        assert_eq!(out.dimensions(), (1, 1));
        assert_eq!(out.dimensions(), predicted);
    }

    #[test]
    fn a_crop_entirely_outside_the_image_leaves_it_alone() {
        // Reachable by undoing the op that made the image big enough for it.
        let src = asymmetric();
        let mut edits = Edits::default();
        edits.push(Op::Crop {
            x: 40,
            y: 40,
            width: 10,
            height: 10,
        });

        assert_eq!(*edits.apply(&src), src);
    }

    #[test]
    fn a_crop_is_measured_against_the_image_the_pipeline_reaches_it_with() {
        // 4x2 rotated clockwise is 2x4, so a crop 3 rows down is only in range
        // after the rotation — which is exactly the point.
        let src = asymmetric();
        let mut edits = Edits::default();
        edits.push(Op::RotateCw);
        edits.push(Op::Crop {
            x: 0,
            y: 3,
            width: 2,
            height: 1,
        });

        assert_eq!(edits.apply(&src).dimensions(), (2, 1));
    }

    #[test]
    fn an_opaque_image_has_nothing_to_trim() {
        let src = asymmetric();
        assert_eq!(opaque_bounds(&src), Some((0, 0, 4, 2)));
    }

    #[test]
    fn trimming_finds_the_content_inside_a_transparent_border() {
        // 6x6 of nothing, with a 2x3 opaque block at (2, 1).
        let mut src = RgbaImage::from_pixel(6, 6, image::Rgba([0, 0, 0, 0]));
        for y in 1..4 {
            for x in 2..4 {
                src.put_pixel(x, y, image::Rgba([255, 0, 0, 255]));
            }
        }

        assert_eq!(opaque_bounds(&src), Some((2, 1, 2, 3)));
    }

    #[test]
    fn a_fully_transparent_image_has_no_bounds_at_all() {
        let src = RgbaImage::from_pixel(4, 4, image::Rgba([9, 9, 9, 0]));
        assert_eq!(opaque_bounds(&src), None);
    }

    #[test]
    fn a_single_opaque_pixel_is_a_one_by_one_rectangle() {
        let mut src = RgbaImage::from_pixel(5, 5, image::Rgba([0, 0, 0, 0]));
        src.put_pixel(3, 4, image::Rgba([1, 2, 3, 1]));
        assert_eq!(opaque_bounds(&src), Some((3, 4, 1, 1)));
    }

    #[test]
    fn only_flips_leave_the_size_alone() {
        assert!(!Op::FlipHorizontal.changes_size());
        assert!(!Op::FlipVertical.changes_size());
        assert!(!Op::Adjust(Adjust::NONE).changes_size());
        assert!(Op::RotateCw.changes_size());
        assert!(Op::MirrorHorizontal.changes_size());
    }

    fn brighter(percent: i16) -> Op {
        Op::Adjust(Adjust {
            brightness: percent,
            ..Adjust::NONE
        })
    }

    #[test]
    fn only_the_last_adjustment_runs() {
        let src = asymmetric();

        // Two settings on the stack is a slider that moved twice, not an image to
        // brighten twice. Pushing +10 then +20 must look exactly like +20 alone.
        let mut twice = Edits::default();
        twice.push(brighter(10));
        twice.push(brighter(20));

        let mut once = Edits::default();
        once.push(brighter(20));

        assert_eq!(*twice.apply(&src), *once.apply(&src));
    }

    #[test]
    fn undo_returns_to_the_previous_setting_rather_than_to_none() {
        let src = asymmetric();
        let mut edits = Edits::default();
        edits.push(brighter(10));
        edits.push(brighter(20));
        edits.undo();

        assert_eq!(
            edits.adjust(),
            Adjust {
                brightness: 10,
                ..Adjust::NONE
            }
        );

        let mut only_ten = Edits::default();
        only_ten.push(brighter(10));
        assert_eq!(*edits.apply(&src), *only_ten.apply(&src));
    }

    #[test]
    fn an_adjustment_dragged_back_to_zero_costs_nothing() {
        let src = asymmetric();
        let mut edits = Edits::default();
        edits.push(brighter(30));
        edits.push(brighter(0));

        // Not merely equal pixels — no work at all, which is what keeps a slider
        // returned to its starting point from leaving a copy of the image behind.
        assert!(matches!(edits.apply(&src), Cow::Borrowed(_)));
    }

    #[test]
    fn an_adjustment_commutes_with_the_geometry_around_it() {
        // The claim `effective` rests on when it runs the surviving adjustment where
        // it sits rather than at the end: every other op only moves pixels about.
        let src = asymmetric();

        let mut first = Edits::default();
        first.push(brighter(25));
        first.push(Op::RotateCw);

        let mut second = Edits::default();
        second.push(Op::RotateCw);
        second.push(brighter(25));

        assert_eq!(*first.apply(&src), *second.apply(&src));
    }

    #[test]
    fn previewing_shows_a_setting_without_recording_it() {
        let src = asymmetric();
        let mut edits = Edits::default();
        edits.push(Op::RotateCw);

        let preview = edits.previewing(Adjust {
            brightness: 40,
            ..Adjust::NONE
        });

        let mut committed = Edits::default();
        committed.push(Op::RotateCw);
        committed.push(brighter(40));
        assert_eq!(*preview.apply(&src), *committed.apply(&src));

        // And the stack it came from is untouched, so nothing has to be undone.
        assert_eq!(edits.ops(), [Op::RotateCw]);
        assert_eq!(edits.adjust(), Adjust::NONE);
    }

    #[test]
    fn the_geometry_pass_leaves_colour_alone_but_keeps_every_move() {
        let src = asymmetric();
        let mut edits = Edits::default();
        edits.push(Op::RotateCw);
        edits.push(brighter(60));
        edits.push(Op::FlipVertical);

        let mut without_colour = Edits::default();
        without_colour.push(Op::RotateCw);
        without_colour.push(Op::FlipVertical);

        assert_eq!(
            *edits.apply_geometry(&src),
            *without_colour.apply(&src),
            "the texture must show every move and no colour change"
        );
        // And what gets saved is still the adjusted image.
        assert_ne!(*edits.apply(&src), *edits.apply_geometry(&src));
    }

    #[test]
    fn a_colour_only_pipeline_leaves_the_geometry_pass_borrowing() {
        let src = asymmetric();
        let mut edits = Edits::default();
        edits.push(brighter(60));

        assert!(matches!(edits.apply_geometry(&src), Cow::Borrowed(_)));
    }

    #[test]
    fn previewing_replaces_rather_than_compounds_an_existing_adjustment() {
        let src = asymmetric();
        let mut edits = Edits::default();
        edits.push(brighter(60));

        let preview = edits.previewing(Adjust {
            brightness: 5,
            ..Adjust::NONE
        });

        let mut only_five = Edits::default();
        only_five.push(brighter(5));
        assert_eq!(*preview.apply(&src), *only_five.apply(&src));
    }
}
