//! Writing edited pixels back out to a file.
//!
//! Export is where a session of non-destructive edits finally becomes bytes. It
//! runs the same [`crate::edit::Edits`] pipeline the preview does — the caller
//! hands over pixels that have already been through it — so what is saved is what
//! was on screen.

use std::path::Path;

use image::RgbaImage;

/// The formats this build can write.
///
/// Shorter than the list it can read: `image` ships decoders for more than it
/// ships encoders, and offering a format that then fails at save time is worse
/// than not offering it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Png,
    Jpeg,
    WebP,
    Bmp,
    Ico,
}

impl Format {
    pub const ALL: [Self; 5] = [Self::Png, Self::Jpeg, Self::WebP, Self::Bmp, Self::Ico];

    /// Extension to write, without the dot.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::WebP => "webp",
            Self::Bmp => "bmp",
            Self::Ico => "ico",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Png => "PNG",
            Self::Jpeg => "JPEG",
            Self::WebP => "WebP",
            Self::Bmp => "BMP",
            Self::Ico => "ICO",
        }
    }

    /// Whether the encoder takes a quality setting, which is what decides if the
    /// quality slider is worth showing.
    pub fn is_lossy(self) -> bool {
        matches!(self, Self::Jpeg | Self::WebP)
    }

    /// Whether the encoder keeps an alpha channel. JPEG does not, so transparency
    /// has to be flattened before it is handed over.
    pub fn keeps_alpha(self) -> bool {
        self != Self::Jpeg
    }

    /// Whether the output size is decided by the format rather than by the export
    /// settings. An `.ico` holds a fixed ladder of icon sizes, so scaling it is not
    /// a thing the user gets to ask for.
    pub fn has_fixed_sizes(self) -> bool {
        self == Self::Ico
    }

    /// The format a path's extension names, if this build can write it.
    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?;
        Self::ALL.into_iter().find(|format| match format {
            // The one format whose usual extension is not its canonical one.
            Self::Jpeg => ext.eq_ignore_ascii_case("jpg") || ext.eq_ignore_ascii_case("jpeg"),
            other => ext.eq_ignore_ascii_case(other.extension()),
        })
    }
}

/// Icon sizes written into an `.ico`, largest first.
///
/// The full ladder Windows actually asks for: 16 in a menu, 32 on the desktop, 48
/// in the shell, 256 in the preview pane. Sizes above the source are skipped rather
/// than upscaled — an icon invented out of pixels that were never there is bigger
/// and no sharper.
const ICON_SIZES: [u32; 7] = [256, 128, 64, 48, 32, 24, 16];

/// How the file should be written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub format: Format,
    /// JPEG quality, 1-100. Ignored by the lossless formats.
    pub quality: u8,
    /// Output size as a percentage of the edited image. 100 writes it as-is.
    pub scale_percent: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            format: Format::Png,
            // The usual "visually lossless" mark: past this, file size climbs much
            // faster than anything anyone can see.
            quality: 90,
            scale_percent: 100,
        }
    }
}

impl Settings {
    /// Dimensions this would produce from `size`.
    pub fn size_after(&self, size: (u32, u32)) -> (u32, u32) {
        if self.scale_percent == 100 || self.format.has_fixed_sizes() {
            return size;
        }
        let scale = self.scale_percent as f64 / 100.0;
        (
            ((size.0 as f64 * scale).round() as u32).max(1),
            ((size.1 as f64 * scale).round() as u32).max(1),
        )
    }
}

/// Why a file could not be written.
///
/// One shape for every encoder, because the caller only ever shows the message —
/// and the three encoders in here fail in three unrelated type systems.
#[derive(Debug, thiserror::Error)]
#[error("could not write {path}: {reason}")]
pub struct ExportError {
    pub path: String,
    pub reason: String,
}

impl ExportError {
    fn at(path: &Path, reason: impl std::fmt::Display) -> Self {
        Self {
            path: path.display().to_string(),
            reason: reason.to_string(),
        }
    }
}

/// Write `pixels` to `path`.
pub fn write(pixels: &RgbaImage, path: &Path, settings: &Settings) -> Result<(), ExportError> {
    let scaled = scale(pixels, settings);

    match settings.format {
        Format::Ico => write_ico(&scaled, path),
        Format::WebP => write_webp(&scaled, path, settings.quality),
        Format::Jpeg => {
            // Flattened onto white rather than dropped: discarding alpha leaves
            // whatever colour happened to sit under a transparent pixel, which is
            // usually black and always a surprise.
            let flattened = flatten_onto_white(&scaled);
            write_jpeg(&flattened, path, settings.quality)
                .map_err(|source| ExportError::at(path, source))
        }
        Format::Png => scaled
            .save_with_format(path, image::ImageFormat::Png)
            .map_err(|source| ExportError::at(path, source)),
        Format::Bmp => scaled
            .save_with_format(path, image::ImageFormat::Bmp)
            .map_err(|source| ExportError::at(path, source)),
    }
}

/// Encode WebP through libwebp, which is the only reason this crate has a C
/// dependency.
///
/// `image` cannot write WebP at all, and the pure-Rust `image-webp` is lossless
/// only — which defeats the point. The reason to write WebP is a smaller file for
/// the web, and that means the quality dial.
fn write_webp(pixels: &RgbaImage, path: &Path, quality: u8) -> Result<(), ExportError> {
    let encoder = webp::Encoder::from_rgba(pixels.as_raw(), pixels.width(), pixels.height());

    // 100 means "lossy at maximum quality", which is still lossy and still larger
    // than it needs to be. Asking for the top of the dial is asking for no loss.
    let encoded = if quality >= 100 {
        encoder.encode_lossless()
    } else {
        encoder.encode(f32::from(quality.max(1)))
    };

    if encoded.is_empty() {
        return Err(ExportError::at(path, "libwebp produced no data"));
    }
    std::fs::write(path, &*encoded).map_err(|source| ExportError::at(path, source))
}

/// Write a Windows icon holding the whole ladder of sizes.
///
/// Each entry is PNG-compressed, which is what keeps the file small: the reason
/// other tools produce a megabyte here is a single uncompressed full-size bitmap.
/// Non-square sources are padded with transparency rather than stretched, because
/// an icon is square and a squashed logo is worse than a margin.
fn write_ico(pixels: &RgbaImage, path: &Path) -> Result<(), ExportError> {
    let square = square_canvas(pixels);
    let square: &RgbaImage = &square;
    let longest = square.width();

    let mut frames = Vec::new();
    for size in ICON_SIZES {
        // Skip sizes the source cannot fill, but never end up with nothing: a tiny
        // source still deserves an icon at its own size.
        if size > longest && !(frames.is_empty() && size == *ICON_SIZES.last().expect("non-empty"))
        {
            continue;
        }
        let size = size.min(longest.max(1));

        let resized =
            image::imageops::resize(square, size, size, image::imageops::FilterType::Lanczos3);
        let frame = image::codecs::ico::IcoFrame::as_png(
            resized.as_raw(),
            size,
            size,
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|source| ExportError::at(path, source))?;
        frames.push(frame);
    }

    let file = std::fs::File::create(path).map_err(|source| ExportError::at(path, source))?;
    image::codecs::ico::IcoEncoder::new(std::io::BufWriter::new(file))
        .encode_images(&frames)
        .map_err(|source| ExportError::at(path, source))
}

/// The image centred on a transparent square, or itself if it already is one.
fn square_canvas(pixels: &RgbaImage) -> std::borrow::Cow<'_, RgbaImage> {
    let (w, h) = pixels.dimensions();
    if w == h {
        return std::borrow::Cow::Borrowed(pixels);
    }

    let side = w.max(h);
    let mut square = RgbaImage::from_pixel(side, side, image::Rgba([0, 0, 0, 0]));
    image::imageops::replace(
        &mut square,
        pixels,
        i64::from(side - w) / 2,
        i64::from(side - h) / 2,
    );
    std::borrow::Cow::Owned(square)
}

/// Resize for export, if asked.
///
/// Lanczos3 rather than the Triangle filter the GPU upload path uses: that one runs
/// on every image just to fit the texture limit and has to be quick, while this one
/// runs once, on purpose, and the result is what gets kept.
fn scale<'a>(pixels: &'a RgbaImage, settings: &Settings) -> std::borrow::Cow<'a, RgbaImage> {
    let (width, height) = settings.size_after(pixels.dimensions());
    if (width, height) == pixels.dimensions() {
        return std::borrow::Cow::Borrowed(pixels);
    }

    std::borrow::Cow::Owned(image::imageops::resize(
        pixels,
        width,
        height,
        image::imageops::FilterType::Lanczos3,
    ))
}

fn flatten_onto_white(pixels: &RgbaImage) -> image::RgbImage {
    image::RgbImage::from_fn(pixels.width(), pixels.height(), |x, y| {
        let [r, g, b, a] = pixels.get_pixel(x, y).0;
        let a = a as u32;
        let over = |c: u8| ((c as u32 * a + 255 * (255 - a)) / 255) as u8;
        image::Rgb([over(r), over(g), over(b)])
    })
}

fn write_jpeg(pixels: &image::RgbImage, path: &Path, quality: u8) -> image::ImageResult<()> {
    let file = std::fs::File::create(path).map_err(image::ImageError::IoError)?;
    let mut writer = std::io::BufWriter::new(file);
    // The plain `save` path gives no way to set quality, and quality is the whole
    // reason anyone picks JPEG.
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut writer, quality.clamp(1, 100))
        .encode_image(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("imaginer-export-{name}"));
        let _ = std::fs::remove_file(&path);
        path
    }

    fn opaque(w: u32, h: u32) -> RgbaImage {
        RgbaImage::from_pixel(w, h, image::Rgba([12, 200, 90, 255]))
    }

    /// A gradient with a little structure — closer to real artwork than flat
    /// colour, which any encoder flatters, or noise, which none of them can.
    fn artwork(w: u32, h: u32) -> RgbaImage {
        RgbaImage::from_fn(w, h, |x, y| {
            let r = (x * 255 / w.max(1)) as u8;
            let g = (y * 255 / h.max(1)) as u8;
            image::Rgba([r, g, r ^ g, 255])
        })
    }

    #[test]
    fn recognises_the_formats_it_can_write() {
        assert_eq!(Format::from_path(Path::new("a.PNG")), Some(Format::Png));
        assert_eq!(Format::from_path(Path::new("a.jpeg")), Some(Format::Jpeg));
        assert_eq!(Format::from_path(Path::new("a.jpg")), Some(Format::Jpeg));
        assert_eq!(Format::from_path(Path::new("a.WEBP")), Some(Format::WebP));
        assert_eq!(Format::from_path(Path::new("a.ico")), Some(Format::Ico));
        assert_eq!(Format::from_path(Path::new("a.tiff")), None);
    }

    #[test]
    fn webp_quality_is_a_dial_that_actually_moves_the_file_size() {
        let src = artwork(200, 150);
        let low = scratch("low.webp");
        let high = scratch("high.webp");

        for (path, quality) in [(&low, 20), (&high, 95)] {
            let settings = Settings {
                format: Format::WebP,
                quality,
                ..Settings::default()
            };
            write(&src, path, &settings).unwrap();
        }

        let (small, large) = (
            std::fs::metadata(&low).unwrap().len(),
            std::fs::metadata(&high).unwrap().len(),
        );
        assert!(
            small < large,
            "quality 20 gave {small}B, quality 95 gave {large}B"
        );

        // And it is a real WebP that reads back at the right size.
        assert_eq!(image::image_dimensions(&high).unwrap(), (200, 150));

        std::fs::remove_file(&low).unwrap();
        std::fs::remove_file(&high).unwrap();
    }

    #[test]
    fn webp_at_full_quality_is_lossless_rather_than_merely_expensive() {
        let src = artwork(64, 64);
        let path = scratch("lossless.webp");
        let settings = Settings {
            format: Format::WebP,
            quality: 100,
            ..Settings::default()
        };
        write(&src, &path, &settings).unwrap();

        assert_eq!(image::open(&path).unwrap().to_rgba8(), src);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn an_icon_holds_the_whole_ladder_and_stays_small() {
        let src = artwork(512, 512);
        let path = scratch("icon.ico");
        let settings = Settings {
            format: Format::Ico,
            ..Settings::default()
        };
        write(&src, &path, &settings).unwrap();

        // Largest entry is the 256 one — what the Windows preview pane asks for.
        assert_eq!(image::image_dimensions(&path).unwrap(), (256, 256));

        // The point of the whole feature: entries are PNG-compressed, not stored
        // raw. Uncompressed, this ladder would be ~352KB before any headers, and
        // that is how other tools end up shipping a megabyte.
        let raw: u64 = ICON_SIZES.iter().map(|s| u64::from(s * s * 4)).sum();
        let written = std::fs::metadata(&path).unwrap().len();
        assert!(
            written < raw / 2,
            "icon is {written}B against {raw}B raw — entries are not being compressed"
        );

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_source_smaller_than_the_ladder_does_not_invent_pixels() {
        let src = artwork(20, 20);
        let path = scratch("small.ico");
        write(
            &src,
            &path,
            &Settings {
                format: Format::Ico,
                ..Settings::default()
            },
        )
        .unwrap();

        // 24 and up are skipped; 16 is the only size it can honestly fill.
        assert_eq!(image::image_dimensions(&path).unwrap(), (16, 16));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_non_square_source_is_padded_rather_than_squashed() {
        let src = artwork(40, 10);
        let square = square_canvas(&src);

        assert_eq!(square.dimensions(), (40, 40));
        // The original rows sit in the middle, untouched.
        assert_eq!(square.get_pixel(0, 15), src.get_pixel(0, 0));
        // And the padding is transparent, not black.
        assert_eq!(square.get_pixel(0, 0).0[3], 0);
    }

    #[test]
    fn an_icon_ignores_the_scale_slider() {
        // Its sizes come from the format, so reporting a scaled size would be a lie.
        let settings = Settings {
            format: Format::Ico,
            scale_percent: 25,
            ..Settings::default()
        };
        assert_eq!(settings.size_after((512, 512)), (512, 512));
    }

    #[test]
    fn writes_a_png_at_the_original_size() {
        let path = scratch("plain.png");
        write(&opaque(6, 4), &path, &Settings::default()).unwrap();

        assert_eq!(image::image_dimensions(&path).unwrap(), (6, 4));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn scaling_is_applied_on_the_way_out() {
        let path = scratch("scaled.png");
        let settings = Settings {
            scale_percent: 50,
            ..Settings::default()
        };
        write(&opaque(10, 8), &path, &settings).unwrap();

        assert_eq!(image::image_dimensions(&path).unwrap(), (5, 4));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_transparent_pixel_lands_on_white_in_a_jpeg() {
        let path = scratch("flat.jpg");
        let mut pixels = opaque(8, 8);
        pixels.put_pixel(0, 0, image::Rgba([0, 0, 0, 0]));

        let settings = Settings {
            format: Format::Jpeg,
            ..Settings::default()
        };
        write(&pixels, &path, &settings).unwrap();

        let written = image::open(&path).unwrap().to_rgb8();
        let [r, g, b] = written.get_pixel(0, 0).0;
        // JPEG is lossy, so this is "near white" rather than exactly white.
        assert!(r > 200 && g > 200 && b > 200, "got {r},{g},{b}");

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn scale_of_one_hundred_percent_changes_nothing() {
        let settings = Settings::default();
        assert_eq!(settings.size_after((37, 11)), (37, 11));
    }

    #[test]
    fn scaling_never_rounds_a_dimension_away() {
        let settings = Settings {
            scale_percent: 1,
            ..Settings::default()
        };
        assert_eq!(settings.size_after((10, 10)), (1, 1));
    }
}
