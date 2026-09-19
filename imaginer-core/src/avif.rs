//! Opening AVIF.
//!
//! This was rejected once, on 2026-08-26, and the reason has since expired. The
//! only decoder then was `avif-native`, which builds dav1d from C and wants
//! pkg-config and a system library, or nasm and meson — a toolchain no personal
//! viewer should make someone install. `rav1d` is dav1d transliterated into
//! Rust, and with its `asm` feature off it builds from `cargo build` alone:
//! measured at 40 seconds cold on this machine, no nasm, no C compiler.
//!
//! Two crates rather than one, because an AVIF is two problems. The container is
//! HEIF — boxes, items, a primary item, an auxiliary item holding alpha, and
//! properties that crop and rotate — and `gamut-avif` reads all of that in safe
//! Rust, then hands the coded picture to whatever AV1 decoder the caller
//! supplies. That seam is this file: everything below turns [`rav1d`]'s C-shaped
//! API into the one trait method `gamut-avif` asks for, and `gamut-avif` does
//! the rest, alpha and colour conversion included.
//!
//! Measured on `bbb_alpha.avif`, 3840x2160 with an alpha item, on this machine:
//! ~200ms in rav1d across both items, ~140ms in the colour pipeline, ~20ms in
//! the plane copy below. A 1-2MP picture off the web is 40-80ms. That is several
//! times a JPEG of the same size and perfectly affordable for a format nobody
//! has a folder full of — and it is the price of no assembly, which is the
//! trade this file exists to make.

use std::mem::MaybeUninit;
use std::ptr::NonNull;
use std::time::{Duration, Instant};

use gamut_avif::{Av1Config, Av1StillDecoder, AvifContainer, ChromaFormat, DecodedFrame};
use gamut_core::Error as GamutError;
use image::RgbaImage;
use rav1d::include::dav1d::data::Dav1dData;
use rav1d::include::dav1d::dav1d::{Dav1dContext, Dav1dSettings};
use rav1d::include::dav1d::picture::Dav1dPicture;

/// The C API's "not yet, ask again".
const EAGAIN: i32 = -11;

/// How long to wait for the worker threads to produce the one frame.
///
/// A still image is a single frame and the decoder either has it or has failed,
/// so this is not a budget, it is a guarantee of termination: a decode thread
/// that spun forever on a file that never completes would be a hung viewer with
/// no way back.
const PATIENCE: Duration = Duration::from_secs(20);

/// Most threads rav1d is given.
///
/// The prefetcher decodes neighbours while this runs, so a context that took
/// every core would be three of them fighting over the machine. Four is enough
/// to carry a 4K frame — it is where the measured gain flattened — and leaves
/// the rest of the app responsive.
const MAX_THREADS: u32 = 4;

#[derive(Debug, thiserror::Error)]
pub enum AvifError {
    #[error("not valid AVIF: {0}")]
    Container(String),
    #[error("cannot decode the AV1 picture: {0}")]
    Codec(String),
    #[error("the AV1 decoder could not be started")]
    Unavailable,
    #[error("the AV1 decoder did not finish within {}s", PATIENCE.as_secs())]
    TimedOut,
    #[error("decoded to {width}x{height}, which is not an image")]
    Size { width: u32, height: u32 },
}

/// Decode an AVIF to straight RGBA.
///
/// Whole-file rather than streaming: the container is a box structure that is
/// read by seeking around it, and every caller here already has the bytes.
pub fn decode(data: &[u8]) -> Result<RgbaImage, AvifError> {
    let container = AvifContainer::parse(data).map_err(|err| AvifError::Container(err.to_string()))?;
    let mut decoder = Decoder::new()?;
    let image = container.decode_primary_rgba8(&mut decoder).map_err(|err| {
        if decoder.timed_out {
            AvifError::TimedOut
        } else {
            AvifError::Codec(err.to_string())
        }
    })?;

    let (width, height) = (image.width(), image.height());
    RgbaImage::from_raw(width, height, image.as_ref().as_samples().to_vec())
        .ok_or(AvifError::Size { width, height })
}

/// rav1d, behind the one method `gamut-avif` needs.
struct Decoder {
    context: Dav1dContext,
    /// Set when [`PATIENCE`] ran out. The trait's error type belongs to
    /// `gamut-avif` and cannot carry our own, so the one failure that is this
    /// machine's fault rather than the file's is recorded here and read back
    /// after the call — a viewer saying "this file is malformed" about a decode
    /// that merely gave up would be telling the user something untrue.
    timed_out: bool,
}

impl Decoder {
    fn new() -> Result<Self, AvifError> {
        let threads = std::thread::available_parallelism()
            .map_or(1, |n| n.get() as u32)
            .min(MAX_THREADS);

        // SAFETY: both calls take pointers to stack values that outlive them,
        // and `dav1d_default_settings` is what fills a zeroed `Dav1dSettings`
        // in. `context` is written only on success, which the result reports.
        unsafe {
            let mut settings = MaybeUninit::<Dav1dSettings>::zeroed();
            rav1d::src::lib::dav1d_default_settings(NonNull::new_unchecked(settings.as_mut_ptr()));
            let mut settings = settings.assume_init();
            settings.n_threads = threads as i32;

            let mut context: Option<Dav1dContext> = None;
            let opened = rav1d::src::lib::dav1d_open(
                Some(NonNull::new_unchecked(&mut context)),
                Some(NonNull::new_unchecked(&mut settings)),
            );
            if opened.0 < 0 {
                return Err(AvifError::Unavailable);
            }
            context
                .map(|context| Self {
                    context,
                    timed_out: false,
                })
                .ok_or(AvifError::Unavailable)
        }
    }
}

impl Av1StillDecoder for Decoder {
    fn decode_still(
        &mut self,
        config: &Av1Config,
        payload: &[u8],
    ) -> Result<DecodedFrame, GamutError> {
        // One self-contained stream — temporal delimiter, the `configOBUs`, then
        // the payload, every OBU sized — which is the shape a software decoder
        // wants. `gamut-avif` assembles it; the alternative shape, `av1C` and
        // sample data passed separately, is for hardware decode APIs.
        let mut stream = Vec::new();
        config.full_stream(payload, &mut stream)?;

        // SAFETY: every pointer below is to a stack value that outlives the
        // call. `dav1d_data_create` hands back a buffer of exactly `stream.len()`
        // bytes for `data`, which `send_data` then takes ownership of; the
        // picture is unreffed before this returns, on both paths out.
        unsafe {
            let mut data = MaybeUninit::<Dav1dData>::zeroed().assume_init();
            let buffer =
                rav1d::src::lib::dav1d_data_create(Some(NonNull::new_unchecked(&mut data)), stream.len());
            if buffer.is_null() {
                return Err(codec("could not allocate the AV1 input buffer"));
            }
            std::ptr::copy_nonoverlapping(stream.as_ptr(), buffer, stream.len());

            let sent =
                rav1d::src::lib::dav1d_send_data(Some(self.context), Some(NonNull::new_unchecked(&mut data)));
            if sent.0 < 0 {
                rav1d::src::lib::dav1d_data_unref(Some(NonNull::new_unchecked(&mut data)));
                return Err(codec("the AV1 decoder refused the picture"));
            }

            let mut picture = MaybeUninit::<Dav1dPicture>::zeroed().assume_init();
            let deadline = Instant::now() + PATIENCE;
            loop {
                let got = rav1d::src::lib::dav1d_get_picture(
                    Some(self.context),
                    Some(NonNull::new_unchecked(&mut picture)),
                );
                if got.0 >= 0 {
                    break;
                }
                if got.0 != EAGAIN {
                    return Err(codec("the AV1 picture is malformed"));
                }
                // With worker threads the frame is not ready the moment the data
                // is in. Yielding rather than spinning: this runs on the decode
                // thread of an app whose other threads have a window to draw.
                if Instant::now() > deadline {
                    self.timed_out = true;
                    return Err(GamutError::Unsupported("AVIF: the AV1 decoder did not finish"));
                }
                std::thread::yield_now();
            }

            let frame = planes(&picture);
            rav1d::src::lib::dav1d_picture_unref(Some(NonNull::new_unchecked(&mut picture)));
            frame
        }
    }
}

/// Copy rav1d's planes into the plain vectors `gamut-avif` wants.
///
/// A copy rather than a borrow because the decoder owns its buffers until the
/// picture is unreffed, and the frame outlives that. Row by row: the planes are
/// strided — padded to whatever alignment the decoder chose — so the pixels of
/// one row are not next to the pixels of the next.
///
/// **Ten and twelve bit pictures are brought down to eight here**, and that is
/// not a shortcut around an edge case: ravif — and so most of what encodes AVIF
/// — uses ten bits even for eight-bit input, because AV1 codes it better. Two
/// reasons to do it at this exact point. `gamut-avif`'s RGBA surface takes
/// eight-bit planes only, and everything downstream of it in this app is
/// `RgbaImage`, so the extra bits have nowhere to go; and doing it on the planes
/// costs one shift per sample rather than a second pass over the picture.
/// Rounded rather than truncated — a plain shift darkens every sample by up to
/// half a level, which on a smooth gradient is visible banding.
///
/// # Safety
///
/// `picture` must be a picture rav1d has filled in and not yet unreffed.
unsafe fn planes(picture: &Dav1dPicture) -> Result<DecodedFrame, GamutError> {
    let width = picture.p.w.max(0) as u32;
    let height = picture.p.h.max(0) as u32;
    let coded_depth = picture.p.bpc.clamp(0, u8::MAX as i32) as u8;
    let shift = coded_depth.saturating_sub(8);
    let chroma = match picture.p.layout as i32 {
        0 => ChromaFormat::Monochrome,
        1 => ChromaFormat::Yuv420,
        2 => ChromaFormat::Yuv422,
        _ => ChromaFormat::Yuv444,
    };
    let (chroma_width, chroma_height) = chroma.chroma_dimensions(width, height);

    // SAFETY: the caller's guarantee. Each plane is read within the width and
    // height the decoder itself reported, along the stride it itself set.
    let plane = |index: usize, width: u32, height: u32| -> Vec<u16> {
        let Some(base) = picture.data[index] else {
            return Vec::new();
        };
        let base = base.as_ptr() as *const u8;
        let stride = picture.stride[usize::from(index > 0)];
        let mut out = Vec::with_capacity((width as usize) * (height as usize));

        let half = 1u16 << shift.saturating_sub(1);
        for row in 0..height as isize {
            let start = unsafe { base.offset(row * stride) };
            if coded_depth > 8 {
                // Read element by element: a row start is only guaranteed to sit
                // on the stride, which is a byte count and need not be even.
                for column in 0..width as isize {
                    let sample = unsafe { (start as *const u16).offset(column).read_unaligned() };
                    out.push((sample.saturating_add(half) >> shift).min(u8::MAX as u16));
                }
            } else {
                let row = unsafe { std::slice::from_raw_parts(start, width as usize) };
                out.extend(row.iter().map(|&sample| u16::from(sample)));
            }
        }
        out
    };

    let luma = plane(0, width, height);
    let (cb, cr) = if chroma == ChromaFormat::Monochrome {
        (Vec::new(), Vec::new())
    } else {
        (
            plane(1, chroma_width, chroma_height),
            plane(2, chroma_width, chroma_height),
        )
    };

    DecodedFrame::new(width, height, 8, chroma, luma, cb, cr)
}

impl Drop for Decoder {
    fn drop(&mut self) {
        // SAFETY: the context was opened here and is closed exactly once, with a
        // pointer to a stack value that outlives the call.
        unsafe {
            let mut context = Some(self.context);
            rav1d::src::lib::dav1d_close(Some(NonNull::new_unchecked(&mut context)));
        }
    }
}

fn codec(message: &'static str) -> GamutError {
    GamutError::Unsupported(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 32x32 colour wedge at eight bits.
    const OPAQUE: &[u8] = include_bytes!("../tests/data/opaque32.avif");
    /// A 64x64 disc fading out, at ten bits — both the alpha auxiliary item and
    /// the bit depth most encoders actually emit, in one file.
    const ALPHA: &[u8] = include_bytes!("../tests/data/alpha64.avif");

    #[test]
    fn a_plain_picture_decodes_to_its_own_size_and_colours() {
        let image = decode(OPAQUE).expect("opaque32 should decode");
        assert_eq!(image.dimensions(), (32, 32));

        // The wedge runs red across and green down, blue flat at 128. Lossy at
        // quality 80, so this asks the colour to be about right rather than
        // exact — enough to catch planes swapped, chroma upsampled wrong, or a
        // picture that came out grey.
        let corner = image.get_pixel(1, 1).0;
        let far = image.get_pixel(30, 30).0;
        assert!(corner[0] < 40 && corner[1] < 40, "top-left is dark: {corner:?}");
        assert!(far[0] > 200 && far[1] > 200, "bottom-right is bright: {far:?}");
        assert!(
            image.pixels().all(|px| px.0[3] == 255),
            "nothing here is see-through"
        );
    }

    #[test]
    fn an_alpha_auxiliary_item_reaches_the_pixels() {
        // Two things at once, both of which a plausible implementation gets
        // wrong silently. Alpha is a second coded picture beside the first, and
        // dropping it turns every transparent AVIF opaque; ten bits is what the
        // file is actually coded at, and refusing it would fail on most AVIF in
        // the wild rather than on a rarity.
        let image = decode(ALPHA).expect("alpha64 should decode");
        assert_eq!(image.dimensions(), (64, 64));

        // The fixture's alpha is a cone: 255 at the centre, falling by 9 a pixel
        // with distance. Testing the *shape* rather than one value — a plane
        // read at the wrong stride, or alpha taken from the colour item, still
        // produces a plausible single number. Tolerances are wide because alpha
        // is coded lossily and arrives through a ten-to-eight bit conversion:
        // measured here the centre comes back 248 and the ramp within one.
        let alpha_at = |x, y| i32::from(image.get_pixel(x, y).0[3]);
        assert!(alpha_at(32, 32) > 240, "centre solid: {}", alpha_at(32, 32));
        assert_eq!(alpha_at(1, 1), 0, "corner clear");
        assert_eq!(alpha_at(63, 63), 0, "and the far corner too");
        for (y, expected) in [(10, 58), (5, 12), (20, 148)] {
            let got = alpha_at(32, y);
            assert!(
                (got - expected).abs() <= 8,
                "at (32,{y}) the ramp should be about {expected}, got {got}"
            );
        }
    }

    #[test]
    fn something_that_is_not_avif_is_an_error_rather_than_a_panic() {
        assert!(decode(b"certainly not avif").is_err());
        // A JPEG that someone renamed `.avif`, which is a real thing found on
        // this machine while this was being written.
        assert!(decode(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 16, b'J', b'F', b'I', b'F']).is_err());
    }

    #[test]
    fn a_truncated_file_is_an_error_rather_than_a_panic() {
        for cut in [4, 16, 64, OPAQUE.len() - 1] {
            assert!(decode(&OPAQUE[..cut]).is_err(), "cut at {cut} should fail");
        }
    }
}
