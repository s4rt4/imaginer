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
        Self::from_name(ext)
    }

    /// The format an extension-like name refers to, if this build can write it.
    ///
    /// Takes a leading dot as well, so `--convert .webp` and `--convert webp` mean
    /// the same thing — nobody should have to remember which one the CLI wanted.
    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.strip_prefix('.').unwrap_or(name);
        Self::ALL.into_iter().find(|format| match format {
            // The one format whose usual extension is not its canonical one.
            Self::Jpeg => name.eq_ignore_ascii_case("jpg") || name.eq_ignore_ascii_case("jpeg"),
            other => name.eq_ignore_ascii_case(other.extension()),
        })
    }
}

/// Icon sizes written into an `.ico`, largest first.
///
/// This is the ladder Windows uses for its own icons, copied deliberately: 16 in a
/// menu, 32 on the desktop, 48 in the shell, 64 for large views, 256 in the preview
/// pane, and 20/40 because they are 125% and 150% of 16 and 32 — the sizes a
/// high-DPI display asks for and would otherwise get by blurry downscaling.
///
/// 128 is absent for the same reason Microsoft leaves it out: nothing asks for it,
/// and as an uncompressed entry (see [`LARGEST_BMP_ENTRY`]) it would cost 66KB —
/// more than every other size in the file put together.
///
/// Sizes above the source are skipped rather than upscaled — an icon invented out
/// of pixels that were never there is bigger and no sharper.
const ICON_SIZES: [u32; 8] = [256, 64, 48, 40, 32, 24, 20, 16];

/// The largest entry stored as an uncompressed BMP. Anything above it is PNG.
///
/// This is not a size optimisation — the opposite. PNG-compressing every entry
/// produces a much smaller file, and it was what this encoder did first, but GDI+
/// cannot read a PNG entry below 256: `System.Drawing.Icon` throws on the whole
/// file, so any .NET application, installer or older tool handed such an icon sees
/// nothing at all. Windows' own icons put PNG at 256 and raw BMP everywhere below,
/// and matching that is what makes the output actually usable as an icon.
const LARGEST_BMP_ENTRY: u32 = 64;

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
/// The 256 entry is PNG-compressed and the rest are raw BMP, which is exactly the
/// mix Windows' own icons use — see [`LARGEST_BMP_ENTRY`] for why the small ones
/// must not be PNG. That one large compressed entry is still what keeps the file
/// tens of kilobytes rather than the megabyte other tools produce, since a raw 256
/// would be 256KB on its own.
///
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

        let frame = if size > LARGEST_BMP_ENTRY {
            image::codecs::ico::IcoFrame::as_png(
                resized.as_raw(),
                size,
                size,
                image::ExtendedColorType::Rgba8,
            )
        } else {
            image::codecs::ico::IcoFrame::with_encoded(
                bmp_entry(&resized),
                size,
                size,
                image::ExtendedColorType::Rgba8,
            )
        }
        .map_err(|source| ExportError::at(path, source))?;
        frames.push(frame);
    }

    let file = std::fs::File::create(path).map_err(|source| ExportError::at(path, source))?;
    image::codecs::ico::IcoEncoder::new(std::io::BufWriter::new(file))
        .encode_images(&frames)
        .map_err(|source| ExportError::at(path, source))
}

/// One icon entry as an uncompressed 32-bit BMP, in the peculiar shape `.ico`
/// requires.
///
/// Three things make it not an ordinary BMP: there is no file header, only the
/// 40-byte `BITMAPINFOHEADER`; the declared height is doubled because a 1-bit AND
/// mask is stacked underneath the colour data; and rows run bottom-up in BGRA.
///
/// The mask is redundant for a 32-bit icon — Windows uses the alpha channel — but
/// it is part of the format and the older code paths that ignore alpha read it, so
/// it is filled from alpha rather than left blank.
fn bmp_entry(pixels: &RgbaImage) -> Vec<u8> {
    let (width, height) = pixels.dimensions();

    // Mask rows are one bit per pixel, padded out to a 4-byte boundary.
    let mask_stride = width.div_ceil(32) as usize * 4;
    let colour_len = (width * height * 4) as usize;
    let mask_len = mask_stride * height as usize;

    let mut out = Vec::with_capacity(BITMAPINFOHEADER_SIZE as usize + colour_len + mask_len);

    out.extend_from_slice(&BITMAPINFOHEADER_SIZE.to_le_bytes());
    out.extend_from_slice(&(width as i32).to_le_bytes());
    out.extend_from_slice(&((height * 2) as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // colour planes
    out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
    out.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB — uncompressed
    out.extend_from_slice(&((colour_len + mask_len) as u32).to_le_bytes());
    // Pixels-per-metre for both axes, then the palette counts: all meaningless for
    // a 32-bit uncompressed image, and all zero in Windows' own icons.
    out.extend_from_slice(&[0u8; 16]);

    for y in (0..height).rev() {
        for x in 0..width {
            let [r, g, b, a] = pixels.get_pixel(x, y).0;
            out.extend_from_slice(&[b, g, r, a]);
        }
    }

    for y in (0..height).rev() {
        let mut row = vec![0u8; mask_stride];
        for x in 0..width {
            // A set bit means "transparent here". Everything else stays clear, which
            // is what makes the colour pixel show through.
            if pixels.get_pixel(x, y).0[3] == 0 {
                row[(x / 8) as usize] |= 0x80 >> (x % 8);
            }
        }
        out.extend_from_slice(&row);
    }

    out
}

/// Size of a `BITMAPINFOHEADER`, which is also the value of its first field.
const BITMAPINFOHEADER_SIZE: u32 = 40;

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

    /// One entry of an `.ico` directory, as read back off disk.
    struct Entry {
        size: u32,
        bytes: usize,
        is_png: bool,
    }

    /// Parse the icon directory, so tests can assert on what was actually written
    /// rather than on what the encoder was asked for.
    fn entries(path: &Path) -> Vec<Entry> {
        let file = std::fs::read(path).unwrap();
        let count = u16::from_le_bytes([file[4], file[5]]) as usize;

        (0..count)
            .map(|i| {
                let at = 6 + i * 16;
                let field = |o: usize| {
                    u32::from_le_bytes(file[at + o..at + o + 4].try_into().unwrap()) as usize
                };
                let bytes = field(8);
                let offset = field(12);
                Entry {
                    // Stored as a single byte, where 0 means 256.
                    size: if file[at] == 0 {
                        256
                    } else {
                        u32::from(file[at])
                    },
                    bytes,
                    is_png: file[offset..offset + 4] == [0x89, b'P', b'N', b'G'],
                }
            })
            .collect()
    }

    fn write_icon(src: &RgbaImage, name: &str) -> std::path::PathBuf {
        let path = scratch(name);
        let settings = Settings {
            format: Format::Ico,
            ..Settings::default()
        };
        write(src, &path, &settings).unwrap();
        path
    }

    #[test]
    fn an_icon_holds_the_whole_ladder_and_stays_small() {
        let path = write_icon(&artwork(512, 512), "icon.ico");

        // Largest entry is the 256 one — what the Windows preview pane asks for.
        assert_eq!(image::image_dimensions(&path).unwrap(), (256, 256));

        let written: Vec<u32> = entries(&path).iter().map(|e| e.size).collect();
        assert_eq!(written, ICON_SIZES);

        // The whole reason this feature exists: the user's other tools produce 1MB+
        // here. Compressing the 256 entry is what avoids that — raw, it alone would
        // be 256KB.
        let size = std::fs::metadata(&path).unwrap().len();
        assert!(size < 128 * 1024, "icon is {size}B, which is not small");

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn only_the_largest_entry_is_png_because_gdi_cannot_read_the_others() {
        // The bug this pins down: PNG-compressing every entry gives a much smaller
        // file that `System.Drawing.Icon` refuses to open at all. Windows' own icons
        // draw the line at 64, and so do we.
        let path = write_icon(&artwork(512, 512), "mixed.ico");

        for entry in entries(&path) {
            assert_eq!(
                entry.is_png,
                entry.size > LARGEST_BMP_ENTRY,
                "the {}px entry is stored as {}",
                entry.size,
                if entry.is_png { "PNG" } else { "BMP" },
            );
        }

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn bmp_entries_are_byte_for_byte_the_size_windows_writes() {
        // Measured off Windows' own icons (VoiceAccessIcon.ico, Magnifier.ico): the
        // 40-byte header, the colour rows, and the AND mask padded to 4 bytes a row.
        // Matching these exactly is the evidence the layout is right — a mask stride
        // computed without the padding would be off by a few bytes a row and still
        // look plausible.
        let expected = [
            (16, 1_128),
            (20, 1_720),
            (24, 2_440),
            (32, 4_264),
            (40, 6_760),
            (48, 9_640),
            (64, 16_936),
        ];

        let path = write_icon(&artwork(512, 512), "sizes.ico");
        let written = entries(&path);

        for (size, bytes) in expected {
            let entry = written
                .iter()
                .find(|e| e.size == size)
                .unwrap_or_else(|| panic!("no {size}px entry"));
            assert_eq!(entry.bytes, bytes, "the {size}px entry is the wrong size");
        }

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_bmp_entry_declares_double_height_and_runs_bottom_up_in_bgra() {
        // A single opaque pixel, so the byte order is unambiguous.
        let mut src = RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 0]));
        src.put_pixel(0, 0, image::Rgba([10, 20, 30, 255]));
        let entry = bmp_entry(&src);

        assert_eq!(u32::from_le_bytes(entry[0..4].try_into().unwrap()), 40);
        assert_eq!(i32::from_le_bytes(entry[4..8].try_into().unwrap()), 1);
        // Doubled, to account for the mask stacked underneath.
        assert_eq!(i32::from_le_bytes(entry[8..12].try_into().unwrap()), 2);
        assert_eq!(u16::from_le_bytes(entry[14..16].try_into().unwrap()), 32);

        // BGRA, not RGBA.
        assert_eq!(entry[40..44], [30, 20, 10, 255]);

        // Then one mask row, padded to four bytes, clear because the pixel is opaque.
        assert_eq!(entry.len(), 40 + 4 + 4);
        assert_eq!(entry[44..48], [0, 0, 0, 0]);
    }

    #[test]
    fn the_mask_marks_transparent_pixels_for_the_paths_that_ignore_alpha() {
        // Two pixels: the left one transparent, the right one not.
        let mut src = RgbaImage::from_pixel(2, 1, image::Rgba([1, 2, 3, 255]));
        src.put_pixel(0, 0, image::Rgba([0, 0, 0, 0]));

        let entry = bmp_entry(&src);
        let mask = entry[entry.len() - 4..].to_vec();

        // Top bit set for pixel 0, clear for pixel 1.
        assert_eq!(mask[0], 0b1000_0000);
    }

    #[test]
    fn a_source_smaller_than_the_ladder_does_not_invent_pixels() {
        let path = write_icon(&artwork(20, 20), "small.ico");

        // 24 and up are skipped rather than upscaled; 20 and 16 are all it can
        // honestly fill.
        let written: Vec<u32> = entries(&path).iter().map(|e| e.size).collect();
        assert_eq!(written, [20, 16]);

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_source_smaller_than_every_rung_still_produces_an_icon() {
        let path = write_icon(&artwork(9, 9), "tiny.ico");

        // Nothing in the ladder fits, so it falls back to the source's own size
        // rather than writing an icon with no entries in it.
        let written: Vec<u32> = entries(&path).iter().map(|e| e.size).collect();
        assert_eq!(written, [9]);

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
