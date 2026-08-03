//! EXIF extraction.
//!
//! Orientation had to be handled from day one — without it every photo shot on a
//! phone renders sideways — and [`Info`] is the rest of the block, read only when
//! somebody asks to see it.

use std::fs::File;
use std::io::{BufRead, BufReader, Seek};
use std::path::Path;

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

/// A group of related facts, in the order they should be listed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub title: &'static str,
    /// Label and value, already formatted. Only tags the file actually carries.
    pub rows: Vec<(&'static str, String)>,
}

/// Everything worth showing from a file's EXIF block.
///
/// Built as formatted text rather than as typed quantities, because every consumer
/// of this is a display: a shutter speed is wanted as `1/250 s`, not as a rational
/// somebody has to render. Absent tags are simply left out — a panel of empty rows
/// says less than a short panel does.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Info {
    pub sections: Vec<Section>,
}

/// Tags that describe the equipment.
const CAMERA: &[(exif::Tag, &str)] = &[
    (exif::Tag::Make, "Make"),
    (exif::Tag::Model, "Model"),
    (exif::Tag::LensModel, "Lens"),
];

/// Tags that describe how the exposure was made.
const EXPOSURE: &[(exif::Tag, &str)] = &[
    (exif::Tag::ExposureTime, "Shutter"),
    (exif::Tag::FNumber, "Aperture"),
    (exif::Tag::PhotographicSensitivity, "ISO"),
    (exif::Tag::ExposureBiasValue, "Exposure bias"),
    (exif::Tag::FocalLength, "Focal length"),
    (exif::Tag::FocalLengthIn35mmFilm, "35mm equivalent"),
    (exif::Tag::ExposureProgram, "Program"),
    (exif::Tag::MeteringMode, "Metering"),
    (exif::Tag::WhiteBalance, "White balance"),
    (exif::Tag::Flash, "Flash"),
];

/// Tags about the picture itself, and about who made it.
const IMAGE: &[(exif::Tag, &str)] = &[
    (exif::Tag::DateTimeOriginal, "Taken"),
    (exif::Tag::DateTime, "Modified"),
    (exif::Tag::ColorSpace, "Colour space"),
    (exif::Tag::ImageDescription, "Description"),
    (exif::Tag::Artist, "Artist"),
    (exif::Tag::Copyright, "Copyright"),
    (exif::Tag::Software, "Software"),
];

impl Info {
    /// Read the EXIF block of a file.
    ///
    /// A file with no EXIF is not an error — most PNGs have none — so this returns
    /// an empty `Info` rather than a failure. The caller has a panel to fill either
    /// way, and "no EXIF here" is a perfectly good thing for it to say.
    pub fn read(path: &Path) -> Self {
        let Ok(file) = File::open(path) else {
            return Self::default();
        };
        let mut reader = BufReader::new(file);
        match exif::Reader::new().read_from_container(&mut reader) {
            Ok(exif) => Self::from_exif(&exif),
            Err(_) => Self::default(),
        }
    }

    /// Pull the fields out of an already-parsed EXIF block.
    pub fn from_exif(exif: &exif::Exif) -> Self {
        let sections = [
            section(exif, "Camera", CAMERA),
            section(exif, "Exposure", EXPOSURE),
            section(exif, "Image", IMAGE),
            location(exif),
        ]
        .into_iter()
        .flatten()
        .collect();

        Self { sections }
    }

    pub fn is_empty(&self) -> bool {
        self.sections.is_empty()
    }
}

/// One section, or `None` if the file carries none of its tags.
fn section(
    exif: &exif::Exif,
    title: &'static str,
    tags: &[(exif::Tag, &'static str)],
) -> Option<Section> {
    let rows: Vec<(&'static str, String)> = tags
        .iter()
        .filter_map(|&(tag, label)| Some((label, value_of(exif, tag)?)))
        .collect();

    (!rows.is_empty()).then_some(Section { title, rows })
}

/// One tag, formatted for reading.
///
/// Everything except text goes through the library's own display, which knows that
/// `ExposureTime` is `1/250 s` and that `MeteringMode` 5 is `pattern`. Reimplementing
/// that here would be a second, worse table of the same knowledge.
fn value_of(exif: &exif::Exif, tag: exif::Tag) -> Option<String> {
    let field = exif.get_field(tag, exif::In::PRIMARY)?;

    let text = match &field.value {
        // Text is the exception, and it has to be: `display_value` quotes ASCII, and
        // the strings themselves arrive padded — camera makers fill `Make` out to a
        // fixed width with spaces and NULs, which would be shown as-is.
        exif::Value::Ascii(parts) => parts
            .iter()
            .map(|part| String::from_utf8_lossy(part))
            .collect::<Vec<_>>()
            .join(" "),
        _ => field.display_value().with_unit(exif).to_string(),
    };

    let text = text.trim_matches(|c: char| c.is_whitespace() || c == '\0');
    (!text.is_empty()).then(|| text.to_owned())
}

/// Where the photograph was taken, as a decimal pair.
///
/// Decimal rather than the degrees-minutes-seconds EXIF stores and the library
/// displays: `-6.175392, 106.827153` can be pasted straight into a map, and
/// `6 deg 10 min 31.4 sec S` cannot.
fn location(exif: &exif::Exif) -> Option<Section> {
    let mut rows = Vec::new();

    let latitude = coordinate(exif, exif::Tag::GPSLatitude, exif::Tag::GPSLatitudeRef);
    let longitude = coordinate(exif, exif::Tag::GPSLongitude, exif::Tag::GPSLongitudeRef);
    if let (Some(lat), Some(lon)) = (latitude, longitude) {
        // Six places is about 0.1m, which is finer than any camera's fix.
        rows.push(("Coordinates", format!("{lat:.6}, {lon:.6}")));
    }

    if let Some(altitude) = value_of(exif, exif::Tag::GPSAltitude) {
        rows.push(("Altitude", altitude));
    }

    (!rows.is_empty()).then_some(Section {
        title: "Location",
        rows,
    })
}

/// One GPS coordinate in signed decimal degrees.
fn coordinate(exif: &exif::Exif, value: exif::Tag, reference: exif::Tag) -> Option<f64> {
    let field = exif.get_field(value, exif::In::PRIMARY)?;
    let exif::Value::Rational(parts) = &field.value else {
        return None;
    };
    let [degrees, minutes, seconds, ..] = parts.as_slice() else {
        return None;
    };

    // A zero denominator is a malformed file, and `to_f64` would hand back an
    // infinity that formats as `inf` in the panel. Refusing is the honest answer.
    if [degrees, minutes, seconds].iter().any(|r| r.denom == 0) {
        return None;
    }

    // The hemisphere lives in its own tag, so a coordinate without one is only half
    // a coordinate — north and south are the same number apart.
    let hemisphere = value_of(exif, reference)?;

    Some(decimal_degrees(
        [degrees.to_f64(), minutes.to_f64(), seconds.to_f64()],
        &hemisphere,
    ))
}

/// Degrees, minutes and seconds, plus a hemisphere letter, as one signed number.
///
/// Split out from the field reading so the arithmetic can be tested without
/// assembling a GPS IFD by hand.
fn decimal_degrees([degrees, minutes, seconds]: [f64; 3], hemisphere: &str) -> f64 {
    let magnitude = degrees + minutes / 60.0 + seconds / 3600.0;
    // South and west are the negative halves.
    let sign = if hemisphere.starts_with(['S', 's', 'W', 'w']) {
        -1.0
    } else {
        1.0
    };
    magnitude * sign
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal EXIF block, assembled by hand so the tests do not need a photograph
    /// checked into the repo.
    ///
    /// Little-endian TIFF header then one IFD. Values are ASCII and kept to four
    /// bytes including the terminator, which is the width that fits inside the
    /// entry itself — so nothing here has to compute an offset. Entries must be
    /// written in ascending tag order, which is what the spec requires and what the
    /// reader relies on.
    fn exif_with_text(fields: &[(u16, &[u8; 4])]) -> exif::Exif {
        let mut buf = b"II*\0\x08\0\0\0".to_vec();
        buf.extend_from_slice(&(fields.len() as u16).to_le_bytes());
        for &(tag, value) in fields {
            buf.extend_from_slice(&tag.to_le_bytes());
            // Type 2 (ASCII), four bytes of it, stored inline.
            buf.extend_from_slice(&2u16.to_le_bytes());
            buf.extend_from_slice(&4u32.to_le_bytes());
            buf.extend_from_slice(value);
        }
        // No IFD after this one.
        buf.extend_from_slice(&0u32.to_le_bytes());

        exif::Reader::new()
            .read_raw(buf)
            .expect("hand-built EXIF should parse")
    }

    /// The same fields, wrapped in a real JPEG.
    ///
    /// `Info::read` has to find the EXIF block inside a container before any of the
    /// parsing above happens, and hand-built IFDs skip that entirely. An APP1
    /// segment goes straight after SOI, which is where cameras put it.
    fn jpeg_with_text(fields: &[(u16, &[u8; 4])]) -> Vec<u8> {
        let mut tiff = b"II*\0\x08\0\0\0".to_vec();
        tiff.extend_from_slice(&(fields.len() as u16).to_le_bytes());
        for &(tag, value) in fields {
            tiff.extend_from_slice(&tag.to_le_bytes());
            tiff.extend_from_slice(&2u16.to_le_bytes());
            tiff.extend_from_slice(&4u32.to_le_bytes());
            tiff.extend_from_slice(value);
        }
        tiff.extend_from_slice(&0u32.to_le_bytes());

        let mut payload = b"Exif\0\0".to_vec();
        payload.extend_from_slice(&tiff);

        let mut jpeg = Vec::new();
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            8,
            8,
            image::Rgb([90, 120, 160]),
        ))
        .write_to(
            &mut std::io::Cursor::new(&mut jpeg),
            image::ImageFormat::Jpeg,
        )
        .expect("encoding a solid 8x8 JPEG cannot fail");

        // The segment length counts itself but not the marker.
        let mut segment = vec![0xff, 0xe1];
        segment.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
        segment.extend_from_slice(&payload);

        let (soi, rest) = jpeg.split_at(2);
        [soi, &segment, rest].concat()
    }

    /// Tag numbers, as they appear in an IFD. Named here rather than taken from
    /// `exif::Tag`, because the point is to write the bytes a camera would.
    const MAKE: u16 = 0x010f;
    const MODEL: u16 = 0x0110;
    const SOFTWARE: u16 = 0x0131;

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

    #[test]
    fn a_file_with_no_exif_has_nothing_to_show() {
        assert!(Info::default().is_empty());
        // A path that is not an image at all takes the same route as a PNG without
        // EXIF: nothing to show, and nothing to report as an error either.
        assert!(Info::read(std::path::Path::new("definitely-not-here.jpg")).is_empty());
    }

    #[test]
    fn reads_text_fields_into_the_section_they_belong_to() {
        let exif = exif_with_text(&[(MAKE, b"AB\0\0"), (MODEL, b"CD\0\0")]);
        let info = Info::from_exif(&exif);

        assert_eq!(info.sections.len(), 1, "only Camera has any tags present");
        let camera = &info.sections[0];
        assert_eq!(camera.title, "Camera");
        assert_eq!(
            camera.rows,
            vec![("Make", "AB".to_owned()), ("Model", "CD".to_owned())]
        );
    }

    #[test]
    fn sections_come_out_in_a_fixed_order_whatever_the_file_holds() {
        // Software is an Image tag and Make a Camera one, so this asserts the order
        // is the table's rather than the file's.
        let exif = exif_with_text(&[(MAKE, b"AB\0\0"), (SOFTWARE, b"XY\0\0")]);
        let titles: Vec<_> = Info::from_exif(&exif)
            .sections
            .iter()
            .map(|s| s.title)
            .collect();

        assert_eq!(titles, vec!["Camera", "Image"]);
    }

    #[test]
    fn text_is_stripped_of_the_padding_cameras_write() {
        // Makers pad these out to a fixed width with spaces and NULs. Left alone it
        // would be shown, and a value that is nothing but padding would be shown as
        // an empty row.
        let exif = exif_with_text(&[(MAKE, b"AB \0"), (MODEL, b"   \0")]);
        let camera = &Info::from_exif(&exif).sections[0];

        assert_eq!(camera.rows, vec![("Make", "AB".to_owned())]);
    }

    #[test]
    fn finds_the_exif_block_inside_a_real_jpeg() {
        let path = std::env::temp_dir().join("imaginer-test-info.jpg");
        std::fs::write(&path, jpeg_with_text(&[(MAKE, b"AB\0\0")])).expect("temp dir is writable");

        let info = Info::read(&path);
        assert_eq!(info.sections.len(), 1);
        assert_eq!(info.sections[0].rows, vec![("Make", "AB".to_owned())]);

        // And the same file decodes, which is what says the segment was spliced in
        // where a decoder expects it rather than somewhere only the EXIF reader
        // would tolerate.
        assert_eq!(crate::decode::decode_full(&path).unwrap().size(), (8, 8));

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn coordinates_are_signed_by_their_hemisphere() {
        // Jakarta, to six places.
        let south = decimal_degrees([6.0, 10.0, 31.4112], "S");
        assert!((south - -6.175392).abs() < 1e-6, "got {south}");

        let east = decimal_degrees([106.0, 49.0, 37.7508], "E");
        assert!((east - 106.827153).abs() < 1e-6, "got {east}");

        // The same magnitude, mirrored.
        assert_eq!(
            decimal_degrees([12.0, 30.0, 0.0], "N"),
            -decimal_degrees([12.0, 30.0, 0.0], "S")
        );
    }
}
