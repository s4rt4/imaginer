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
    Bmp,
}

impl Format {
    pub const ALL: [Self; 3] = [Self::Png, Self::Jpeg, Self::Bmp];

    /// Extension to write, without the dot.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Bmp => "bmp",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Png => "PNG",
            Self::Jpeg => "JPEG",
            Self::Bmp => "BMP",
        }
    }

    /// Whether the encoder takes a quality setting, which is what decides if the
    /// quality slider is worth showing.
    pub fn is_lossy(self) -> bool {
        self == Self::Jpeg
    }

    /// Whether the encoder keeps an alpha channel. JPEG does not, so transparency
    /// has to be flattened before it is handed over.
    pub fn keeps_alpha(self) -> bool {
        self != Self::Jpeg
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
        if self.scale_percent == 100 {
            return size;
        }
        let scale = self.scale_percent as f64 / 100.0;
        (
            ((size.0 as f64 * scale).round() as u32).max(1),
            ((size.1 as f64 * scale).round() as u32).max(1),
        )
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("could not write {path}: {source}")]
    Write {
        path: String,
        #[source]
        source: image::ImageError,
    },
}

/// Write `pixels` to `path`.
pub fn write(pixels: &RgbaImage, path: &Path, settings: &Settings) -> Result<(), ExportError> {
    let scaled = scale(pixels, settings);

    let result = if settings.format.keeps_alpha() {
        scaled.save_with_format(path, image_format(settings.format))
    } else {
        // Flattened onto white rather than dropped: discarding alpha leaves whatever
        // colour happened to sit under a transparent pixel, which is usually black
        // and always a surprise.
        let flattened = flatten_onto_white(&scaled);
        write_jpeg(&flattened, path, settings.quality)
    };

    result.map_err(|source| ExportError::Write {
        path: path.display().to_string(),
        source,
    })
}

fn image_format(format: Format) -> image::ImageFormat {
    match format {
        Format::Png => image::ImageFormat::Png,
        Format::Jpeg => image::ImageFormat::Jpeg,
        Format::Bmp => image::ImageFormat::Bmp,
    }
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

    #[test]
    fn recognises_the_formats_it_can_write() {
        assert_eq!(Format::from_path(Path::new("a.PNG")), Some(Format::Png));
        assert_eq!(Format::from_path(Path::new("a.jpeg")), Some(Format::Jpeg));
        assert_eq!(Format::from_path(Path::new("a.jpg")), Some(Format::Jpeg));
        // Readable, but not writable — so not a format the export panel offers.
        assert_eq!(Format::from_path(Path::new("a.webp")), None);
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
