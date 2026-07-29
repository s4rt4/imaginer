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
}

impl Op {
    /// Whether applying this op changes the image's dimensions.
    pub fn changes_size(self) -> bool {
        !matches!(self, Self::FlipHorizontal | Self::FlipVertical)
    }

    /// Dimensions this op produces from `size`.
    ///
    /// Kept separate from [`Op::apply`] so the export panel can report the output
    /// size without doing the work of producing it.
    pub fn size_after(self, size: (u32, u32)) -> (u32, u32) {
        let (w, h) = size;
        match self {
            Self::FlipHorizontal | Self::FlipVertical => (w, h),
            Self::RotateCw | Self::RotateCcw => (h, w),
            Self::MirrorHorizontal => (w.saturating_mul(2), h),
            Self::MirrorVertical => (w, h.saturating_mul(2)),
        }
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
        }
    }
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
        let Some((first, rest)) = self.applied.split_first() else {
            return Cow::Borrowed(base);
        };

        let mut image = first.apply(base);
        for op in rest {
            image = op.apply(&image);
        }
        Cow::Owned(image)
    }
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
    fn only_flips_leave_the_size_alone() {
        assert!(!Op::FlipHorizontal.changes_size());
        assert!(!Op::FlipVertical.changes_size());
        assert!(Op::RotateCw.changes_size());
        assert!(Op::MirrorHorizontal.changes_size());
    }
}
