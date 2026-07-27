//! EXIF extraction. For now this is just orientation, which has to be handled from
//! day one — without it every photo shot on a phone renders sideways.

use std::io::{BufRead, Seek};

/// The eight EXIF orientation states (tag 0x0112), as flip/rotate operations to
/// apply to the decoded pixels before display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Orientation {
    #[default]
    Normal,
    FlipH,
    Rotate180,
    FlipV,
    /// Transpose: mirror across the top-left/bottom-right diagonal.
    Transpose,
    Rotate90,
    /// Transverse: mirror across the top-right/bottom-left diagonal.
    Transverse,
    Rotate270,
}

impl Orientation {
    fn from_exif_value(value: u32) -> Self {
        match value {
            2 => Self::FlipH,
            3 => Self::Rotate180,
            4 => Self::FlipV,
            5 => Self::Transpose,
            6 => Self::Rotate90,
            7 => Self::Transverse,
            8 => Self::Rotate270,
            // 1, and anything out of range, means "as stored".
            _ => Self::Normal,
        }
    }

    /// Whether applying this orientation swaps width and height.
    pub fn swaps_axes(self) -> bool {
        matches!(
            self,
            Self::Transpose | Self::Rotate90 | Self::Transverse | Self::Rotate270
        )
    }

    /// Apply the orientation to a decoded image.
    ///
    /// `Normal` returns the image untouched, which is the common case and costs
    /// nothing — the other variants each copy the buffer once.
    pub fn apply(self, img: image::DynamicImage) -> image::DynamicImage {
        use image::DynamicImage as D;
        match self {
            Self::Normal => img,
            Self::FlipH => D::fliph(&img),
            Self::Rotate180 => D::rotate180(&img),
            Self::FlipV => D::flipv(&img),
            Self::Transpose => D::rotate90(&D::fliph(&img)),
            Self::Rotate90 => D::rotate90(&img),
            Self::Transverse => D::rotate270(&D::fliph(&img)),
            Self::Rotate270 => D::rotate270(&img),
        }
    }
}

/// Pull the orientation out of an already-parsed EXIF block.
pub fn orientation_from_exif(exif: &exif::Exif) -> Orientation {
    exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY)
        .and_then(|field| field.value.get_uint(0))
        .map(Orientation::from_exif_value)
        .unwrap_or_default()
}

/// Read the EXIF orientation tag from an already-open file.
///
/// A missing or malformed EXIF block is not an error — most PNGs have none — so
/// this falls back to `Normal` rather than propagating a failure.
pub fn read_orientation<R: BufRead + Seek>(reader: &mut R) -> Orientation {
    match exif::Reader::new().read_from_container(reader) {
        Ok(exif) => orientation_from_exif(&exif),
        Err(_) => Orientation::Normal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_known_exif_values() {
        assert_eq!(Orientation::from_exif_value(1), Orientation::Normal);
        assert_eq!(Orientation::from_exif_value(6), Orientation::Rotate90);
        assert_eq!(Orientation::from_exif_value(8), Orientation::Rotate270);
    }

    #[test]
    fn out_of_range_orientation_falls_back_to_normal() {
        assert_eq!(Orientation::from_exif_value(0), Orientation::Normal);
        assert_eq!(Orientation::from_exif_value(99), Orientation::Normal);
    }

    #[test]
    fn quarter_turns_swap_axes() {
        assert!(Orientation::Rotate90.swaps_axes());
        assert!(Orientation::Rotate270.swaps_axes());
        assert!(!Orientation::Rotate180.swaps_axes());
        assert!(!Orientation::FlipH.swaps_axes());
    }

    #[test]
    fn rotate90_transposes_dimensions() {
        let img = image::DynamicImage::new_rgba8(4, 2);
        let rotated = Orientation::Rotate90.apply(img);
        assert_eq!((rotated.width(), rotated.height()), (2, 4));
    }
}
