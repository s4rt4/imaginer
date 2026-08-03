//! Two-stage decoding.
//!
//! The whole point of Imaginer is that something correct appears on screen fast.
//! So decoding is split: [`decode_preview`] pulls the thumbnail that cameras and
//! phones already embed in the EXIF block (a few hundred microseconds — no full
//! decode at all), and [`decode_full`] does the real work afterwards. The viewer
//! shows whichever arrives first and swaps in the full image when it lands.

use std::borrow::Cow;
use std::fs::File;
use std::io::{BufReader, Read, Seek};
use std::path::Path;
use std::sync::Arc;

use crate::metadata::{self, Orientation};

/// File extensions this build can decode, lower-case and without the dot.
///
/// Mirrors the `image` feature list in `Cargo.toml` — a new format has to be added
/// in both places. Everything that needs to ask "is this an image?" reads it from
/// here: the open dialog's filter, the folder listing, and the shell integration's
/// idea of which files to offer a Convert menu on.
///
/// Longer than the list of formats that can be *written*, which is
/// [`crate::Format`]. That asymmetry is deliberate and is `image`'s: it ships more
/// decoders than encoders, and offering a save format that then fails is worse than
/// not offering it.
pub const SUPPORTED_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "bmp", "webp", "tif", "tiff", "ico", "ff", "svg", "svgz",
];

/// Whether `path` looks like something this build can open.
///
/// Extension-based on purpose: this is used to filter directory listings, where
/// sniffing the contents of every file would mean opening all of them.
pub fn is_supported(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            SUPPORTED_EXTENSIONS
                .iter()
                .any(|supported| ext.eq_ignore_ascii_case(supported))
        })
}

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("could not read {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("could not decode {path}: {source}")]
    Image {
        path: String,
        #[source]
        source: image::ImageError,
    },
    #[error("could not render {path}: {source}")]
    Svg {
        path: String,
        #[source]
        source: crate::svg::SvgError,
    },
}

/// Which of the two decode stages produced an image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// A low-resolution stand-in, shown while the full decode runs.
    Preview,
    /// The real thing, at native resolution.
    Full,
}

/// A decoded image, already rotated per EXIF and in the RGBA8 layout the GPU wants.
///
/// Cheap to clone, which is what lets the cache hand the same image to the viewer
/// twice without decoding it twice.
#[derive(Clone)]
pub struct Decoded {
    /// Behind an `Arc` because one decode is wanted in three places at once — the
    /// texture upload, the edit pipeline's untouched original, and the cache — and a
    /// 24MP photograph is 100MB of pixels. Copying that around to share it would
    /// undo the point of keeping it.
    pub pixels: Arc<image::RgbaImage>,
    pub stage: Stage,
    pub orientation: Orientation,
    /// Native size of the source image, oriented. Known even for a preview, so the
    /// viewer can lay out and set the zoom level correctly before the full decode
    /// arrives — no layout jump when it swaps in.
    pub full_size: (u32, u32),
}

impl Decoded {
    /// Size of the pixels actually held, which is smaller than `full_size` for a preview.
    pub fn size(&self) -> (u32, u32) {
        self.pixels.dimensions()
    }

    /// Raw RGBA8 bytes, row-major, straight (non-premultiplied) alpha.
    pub fn rgba_bytes(&self) -> &[u8] {
        self.pixels.as_raw()
    }

    /// How much memory the pixels occupy, which is what the cache budgets against.
    ///
    /// The buffer itself, not `size_of` the struct: everything else here is a
    /// handful of bytes beside it.
    pub fn byte_size(&self) -> usize {
        self.pixels.as_raw().len()
    }

    /// Pixels ready to hand to the GPU, downscaled if the image is larger than the
    /// driver's texture limit.
    ///
    /// Limits are commonly 16384 texels per side; panoramas and large scans exceed
    /// that, and an oversized upload fails outright rather than degrading, so it
    /// has to be handled. Borrows in the common case — the copy only happens when
    /// scaling is actually required.
    pub fn for_upload(&self, max_side: u32) -> (u32, u32, Cow<'_, [u8]>) {
        for_upload(&self.pixels, max_side)
    }
}

/// Pixels ready to hand to the GPU, downscaled if they are larger than the driver's
/// texture limit.
///
/// Limits are commonly 16384 texels per side; panoramas and large scans exceed that,
/// and an oversized upload fails outright rather than degrading, so it has to be
/// handled. Borrows in the common case — the copy only happens when scaling is
/// actually required.
///
/// Free-standing rather than a method, because edited pixels need exactly the same
/// treatment and they never belonged to a [`Decoded`].
pub fn for_upload(pixels: &image::RgbaImage, max_side: u32) -> (u32, u32, Cow<'_, [u8]>) {
    let (width, height) = pixels.dimensions();
    let Some(scale) = fit_scale(width, height, max_side) else {
        return (width, height, Cow::Borrowed(pixels.as_raw()));
    };

    let target_w = ((width as f64 * scale).round() as u32).max(1);
    let target_h = ((height as f64 * scale).round() as u32).max(1);
    let resized = image::imageops::resize(
        pixels,
        target_w,
        target_h,
        image::imageops::FilterType::Triangle,
    );
    (target_w, target_h, Cow::Owned(resized.into_raw()))
}

/// Scale factor needed to bring an image within `max_side`, or `None` if it fits.
fn fit_scale(width: u32, height: u32, max_side: u32) -> Option<f64> {
    let longest = width.max(height);
    (longest > max_side).then(|| max_side as f64 / longest as f64)
}

// Hand-written so debug output stays a single line — the derive would dump every
// pixel in the buffer.
impl std::fmt::Debug for Decoded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Decoded")
            .field("size", &self.size())
            .field("stage", &self.stage)
            .field("orientation", &self.orientation)
            .field("full_size", &self.full_size)
            .finish()
    }
}

/// Native dimensions of an image, read from the header without decoding pixels,
/// and corrected for EXIF orientation.
pub fn oriented_dimensions(path: &Path, orientation: Orientation) -> Option<(u32, u32)> {
    let (w, h) = image::image_dimensions(path).ok()?;
    Some(if orientation.swaps_axes() {
        (h, w)
    } else {
        (w, h)
    })
}

/// Try to produce an instant preview from the JPEG thumbnail embedded in EXIF.
///
/// Returns `None` whenever that isn't possible — no EXIF, no thumbnail, or a
/// corrupt one. That is an entirely normal outcome (screenshots and PNGs have no
/// thumbnail), so the caller should just wait for the full decode.
pub fn decode_preview(path: &Path) -> Option<Decoded> {
    let mut reader = BufReader::new(File::open(path).ok()?);
    let exif = exif::Reader::new().read_from_container(&mut reader).ok()?;
    let orientation = metadata::orientation_from_exif(&exif);

    let offset = thumbnail_field(&exif, exif::Tag::JPEGInterchangeFormat)?;
    let len = thumbnail_field(&exif, exif::Tag::JPEGInterchangeFormatLength)?;

    // Thumbnail offsets are relative to the start of the TIFF header, which is
    // exactly what `buf()` hands back.
    let bytes = exif.buf().get(offset..offset.checked_add(len)?)?;
    let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Jpeg).ok()?;

    // The thumbnail is stored in the same orientation as the main image.
    let img = orientation.apply(img);
    let full_size = oriented_dimensions(path, orientation)
        // If the header read fails, the thumbnail's own size is a poor but usable
        // stand-in — better than reporting nothing and blocking layout.
        .unwrap_or_else(|| (img.width(), img.height()));

    Some(Decoded {
        pixels: Arc::new(img.into_rgba8()),
        stage: Stage::Preview,
        orientation,
        full_size,
    })
}

fn thumbnail_field(exif: &exif::Exif, tag: exif::Tag) -> Option<usize> {
    exif.get_field(tag, exif::In::THUMBNAIL)?
        .value
        .get_uint(0)
        .map(|v| v as usize)
}

/// Fully decode an image at native resolution, corrected for EXIF orientation.
pub fn decode_full(path: &Path) -> Result<Decoded, DecodeError> {
    let io_err = |source| DecodeError::Io {
        path: path.display().to_string(),
        source,
    };
    let img_err = |source| DecodeError::Image {
        path: path.display().to_string(),
        source,
    };
    let svg_err = |source| DecodeError::Svg {
        path: path.display().to_string(),
        source,
    };

    let mut reader = BufReader::new(File::open(path).map_err(io_err)?);
    let orientation = metadata::read_orientation(&mut reader);
    reader.rewind().map_err(io_err)?;

    // Detect by content rather than extension, so a mislabelled `.png` that is
    // really a JPEG still opens.
    let probe = image::ImageReader::new(reader)
        .with_guessed_format()
        .map_err(io_err)?;

    let img = match probe.format() {
        Some(_) => probe.decode().map_err(img_err)?,
        // Nothing with a magic number. SVG is the only format here that has none —
        // it is XML, and text has nothing to sniff for — so it belongs at the end
        // as the fallback rather than as another guess in the queue. A file that is
        // neither comes back as an SVG parse error, which is the more useful
        // message anyway: `image` would only say it did not recognise the format.
        None => {
            let mut reader = probe.into_inner();
            reader.rewind().map_err(io_err)?;
            let mut data = Vec::new();
            reader.read_to_end(&mut data).map_err(io_err)?;

            let rendered = crate::svg::Svg::parse(&data)
                .and_then(|svg| svg.render_fit())
                .map_err(svg_err)?;
            image::DynamicImage::ImageRgba8(rendered)
        }
    };

    let img = orientation.apply(img);
    let full_size = (img.width(), img.height());

    Ok(Decoded {
        pixels: Arc::new(img.into_rgba8()),
        stage: Stage::Full,
        orientation,
        full_size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_path(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("imaginer-test-{name}"));
        let _ = std::fs::remove_file(&path);
        path
    }

    #[test]
    fn decodes_a_png_at_native_size() {
        let path = temp_path("basic.png");
        image::RgbaImage::from_pixel(7, 3, image::Rgba([10, 20, 30, 255]))
            .save(&path)
            .unwrap();

        let decoded = decode_full(&path).unwrap();
        assert_eq!(decoded.stage, Stage::Full);
        assert_eq!(decoded.size(), (7, 3));
        assert_eq!(decoded.full_size, (7, 3));
        assert_eq!(decoded.orientation, Orientation::Normal);
        assert_eq!(decoded.pixels.get_pixel(0, 0).0, [10, 20, 30, 255]);

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn no_exif_thumbnail_yields_no_preview() {
        let path = temp_path("nothumb.png");
        image::RgbaImage::from_pixel(4, 4, image::Rgba([0, 0, 0, 255]))
            .save(&path)
            .unwrap();

        assert!(decode_preview(&path).is_none());

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn missing_file_is_an_io_error_not_a_panic() {
        let err = decode_full(Path::new("definitely-not-here.png")).unwrap_err();
        assert!(matches!(err, DecodeError::Io { .. }));
    }

    #[test]
    fn garbage_bytes_are_a_decode_error() {
        let path = temp_path("garbage.png");
        File::create(&path)
            .unwrap()
            .write_all(b"not an image")
            .unwrap();

        assert!(decode_full(&path).is_err());

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn images_within_the_texture_limit_are_not_scaled() {
        assert_eq!(fit_scale(1920, 1080, 16384), None);
        assert_eq!(fit_scale(16384, 16384, 16384), None);
    }

    #[test]
    fn oversized_images_scale_by_their_longest_side() {
        let landscape = fit_scale(32768, 1000, 16384).unwrap();
        assert!((landscape - 0.5).abs() < 1e-9);

        // Portrait orientation is driven by height, not width.
        let portrait = fit_scale(1000, 32768, 16384).unwrap();
        assert!((portrait - 0.5).abs() < 1e-9);
    }

    #[test]
    fn for_upload_borrows_when_it_fits_and_scales_when_it_does_not() {
        let decoded = Decoded {
            pixels: Arc::new(image::RgbaImage::from_pixel(
                64,
                16,
                image::Rgba([1, 2, 3, 255]),
            )),
            stage: Stage::Full,
            orientation: Orientation::Normal,
            full_size: (64, 16),
        };

        let (w, h, bytes) = decoded.for_upload(16384);
        assert_eq!((w, h), (64, 16));
        assert!(matches!(bytes, Cow::Borrowed(_)));

        let (w, h, bytes) = decoded.for_upload(32);
        assert_eq!((w, h), (32, 8));
        assert!(matches!(bytes, Cow::Owned(_)));
        assert_eq!(bytes.len(), 32 * 8 * 4);
    }

    #[test]
    fn detects_format_by_content_not_extension() {
        // A JPEG wearing a .png extension should still open.
        let path = temp_path("liar.png");
        let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            8,
            8,
            image::Rgb([200, 100, 50]),
        ));
        let mut bytes = std::io::Cursor::new(Vec::new());
        img.write_to(&mut bytes, image::ImageFormat::Jpeg).unwrap();
        std::fs::write(&path, bytes.into_inner()).unwrap();

        let decoded = decode_full(&path).unwrap();
        assert_eq!(decoded.size(), (8, 8));

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn recognises_supported_extensions_whatever_their_case() {
        assert!(is_supported(Path::new("holiday.JPG")));
        assert!(is_supported(Path::new("scan.png")));
        assert!(is_supported(Path::new("scan.TIFF")));
        assert!(is_supported(Path::new("app.ico")));
        assert!(!is_supported(Path::new("notes.txt")));
        assert!(!is_supported(Path::new("no-extension")));
    }

    /// Encode `img` in `format` into a temp file and decode it back.
    fn round_trip(name: &str, format: image::ImageFormat, img: &image::DynamicImage) -> Decoded {
        let path = temp_path(name);
        let mut bytes = std::io::Cursor::new(Vec::new());
        img.write_to(&mut bytes, format).expect("encoding failed");
        std::fs::write(&path, bytes.into_inner()).unwrap();

        let decoded = decode_full(&path).expect("decoding failed");
        std::fs::remove_file(&path).unwrap();
        decoded
    }

    #[test]
    fn reads_tiff() {
        // What scanners and Photoshop write, and the reason this extension is on the
        // list at all.
        let src = image::DynamicImage::ImageRgba8(image::RgbaImage::from_fn(6, 4, |x, y| {
            image::Rgba([(x * 40) as u8, (y * 60) as u8, 90, 255])
        }));
        let decoded = round_trip("format.tiff", image::ImageFormat::Tiff, &src);

        assert_eq!(decoded.size(), (6, 4));
        assert_eq!(decoded.pixels.get_pixel(2, 1).0, [80, 60, 90, 255]);
    }

    #[test]
    fn reads_farbfeld() {
        // Farbfeld is 16 bits a channel and its encoder accepts nothing else, so the
        // round trip is the interesting part: `0xABAB` has to come back as `0xAB`
        // rather than as something a bit off.
        let src = image::DynamicImage::ImageRgba16(image::ImageBuffer::from_pixel(
            3,
            2,
            image::Rgba([0xabab, 0x1010, 0xffff, 0xffff]),
        ));
        let decoded = round_trip("format.ff", image::ImageFormat::Farbfeld, &src);

        assert_eq!(decoded.size(), (3, 2));
        assert_eq!(decoded.pixels.get_pixel(0, 0).0, [0xab, 0x10, 0xff, 0xff]);
    }

    #[test]
    fn reads_svg_and_draws_it_large_enough_to_look_at() {
        // The one format with no magic number, so this also checks the fallback
        // ordering: `image` finds nothing to recognise and hands the bytes on.
        let path = temp_path("vector.svg");
        std::fs::write(
            &path,
            br#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="12"
                viewBox="0 0 24 12"><rect width="24" height="12" fill="lime"/></svg>"#,
        )
        .unwrap();

        let decoded = decode_full(&path).expect("SVG should decode");
        // 24 units wide would be a smudge; the longest side comes up to the floor.
        assert_eq!(
            decoded.size(),
            (crate::svg::MIN_SIDE, crate::svg::MIN_SIDE / 2)
        );
        assert_eq!(decoded.pixels.get_pixel(100, 100).0, [0, 255, 0, 255]);

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_file_that_is_neither_raster_nor_svg_says_so() {
        // The SVG fallback must not turn "unrecognised" into a blank image.
        let path = temp_path("prose.svg");
        std::fs::write(&path, b"just some words, not markup at all").unwrap();

        assert!(matches!(
            decode_full(&path).unwrap_err(),
            DecodeError::Svg { .. }
        ));

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn reads_back_an_icon_this_crate_wrote() {
        // The loop worth closing: the exporter's `.ico` is the output this project
        // is measured on, and until now nothing here could open one. A file that
        // Windows reads but this app cannot would be a poor advertisement for it.
        // 256 square, so the file carries the one PNG-compressed entry as well as
        // the BMP ladder below it — and it is the PNG entry a decoder is likeliest
        // to choke on. A smaller source would skip it: the encoder never upscales.
        let path = temp_path("written.ico");
        let src =
            image::RgbaImage::from_fn(256, 256, |x, y| image::Rgba([x as u8, y as u8, 128, 255]));
        crate::export::write(
            &src,
            &path,
            &crate::export::Settings {
                format: crate::export::Format::Ico,
                ..Default::default()
            },
        )
        .expect("writing the icon failed");

        // The largest entry in the ladder, which is what a decoder should pick.
        let decoded = decode_full(&path).expect("decoding the icon failed");
        assert_eq!(decoded.size(), (256, 256));

        std::fs::remove_file(&path).unwrap();
    }
}
