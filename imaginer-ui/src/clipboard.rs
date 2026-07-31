//! The system clipboard, in both directions.
//!
//! Everything that touches `arboard` lives here, so the rest of the app never has to
//! know what a DIBV5 is. The pixel format works out exactly: arboard's `ImageData`
//! is straight RGBA8, row-major, which is the layout `RgbaImage` already holds and
//! the one the decoder hands back — so a copy is a borrow of the buffer we have, and
//! a paste is a buffer we can wrap without touching a byte.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use imaginer_core::image::RgbaImage;

/// What came off the clipboard.
pub enum Pasted {
    /// Pixels, from a screenshot tool or another image app.
    Image(RgbaImage),
    /// A file on disk, named by a path somebody copied as text.
    Path(PathBuf),
}

/// Put text on the clipboard.
///
/// Synchronous, unlike [`copy_image`]: the payload is a path, a few hundred bytes,
/// and the round trip is over before a frame would have been drawn.
pub fn copy_text(text: String) -> Result<(), String> {
    clipboard()?
        .set_text(text)
        .map_err(|err| format!("Could not copy the path: {err}"))
}

/// Put pixels on the clipboard.
///
/// Belongs on a worker thread: the conversion allocates and rewrites the whole
/// buffer, which for a 24MP photograph is 100MB of work.
pub fn copy_image(pixels: &RgbaImage) -> Result<(), String> {
    let (width, height) = pixels.dimensions();
    let image = arboard::ImageData {
        width: width as usize,
        height: height as usize,
        // Borrowed: arboard builds its own bitmap from this, so handing it a copy
        // would mean the pixels existed three times over for no reason.
        bytes: Cow::Borrowed(pixels.as_raw()),
    };

    clipboard()?
        .set_image(image)
        .map_err(|err| format!("Could not copy the image: {err}"))
}

/// Take whatever the clipboard holds that this app can show.
pub fn paste() -> Result<Pasted, String> {
    let mut clipboard = clipboard()?;

    // Pixels first: an image on the clipboard is unambiguous, where text has to be
    // guessed at.
    if let Ok(image) = clipboard.get_image() {
        let (width, height) = (image.width as u32, image.height as u32);
        let pixels =
            RgbaImage::from_raw(width, height, image.bytes.into_owned()).ok_or_else(|| {
                "The clipboard image did not hold as many pixels as it claimed".to_owned()
            })?;
        return Ok(Pasted::Image(pixels));
    }

    // Otherwise a path, which is what Explorer's "Copy as path" leaves and what any
    // pasted filename looks like.
    //
    // NOTE: a plain Copy in Explorer puts CF_HDROP on the clipboard, not text, and
    // arboard cannot read that format at all — so copying a file in Explorer and
    // pressing Ctrl+V here does nothing. Reading CF_HDROP means going to the Win32
    // clipboard directly, which is a job of its own; dropping the file on the window
    // works today and is the same gesture.
    let text = clipboard
        .get_text()
        .map_err(|_| NOTHING_USABLE.to_owned())?;
    path_from_text(&text)
        .map(Pasted::Path)
        .ok_or_else(|| NOTHING_USABLE.to_owned())
}

const NOTHING_USABLE: &str = "There is no image on the clipboard";

/// Read clipboard text as a path to an image this build can open.
///
/// Quotes because that is how Windows hands out a path — "Copy as path" wraps it in
/// them, and so does anything that has been through a command line. Checked against
/// the disk as well as the extension: text that merely looks like a filename is far
/// more common than text that is one.
fn path_from_text(text: &str) -> Option<PathBuf> {
    let trimmed = text.trim().trim_matches('"');
    if trimmed.is_empty() {
        return None;
    }

    let path = Path::new(trimmed);
    (imaginer_core::is_supported(path) && path.is_file()).then(|| path.to_path_buf())
}

fn clipboard() -> Result<arboard::Clipboard, String> {
    arboard::Clipboard::new().map_err(|err| format!("Could not reach the clipboard: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // The clipboard itself is deliberately not exercised here: a test that wrote to
    // it would throw away whatever the person running the tests had copied, and a
    // test that read from it would assert against something it does not control.
    // What is testable is the guess this module makes about text, which is where the
    // surprises actually live.

    fn scratch(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("imaginer-clipboard-{name}"));
        std::fs::write(&path, []).unwrap();
        path
    }

    #[test]
    fn a_quoted_windows_path_is_unwrapped() {
        let path = scratch("quoted.png");
        let quoted = format!("  \"{}\"  ", path.display());

        assert_eq!(path_from_text(&quoted), Some(path.clone()));

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn text_that_is_not_a_file_is_not_a_path() {
        assert_eq!(path_from_text("hello, world"), None);
        assert_eq!(path_from_text(""), None);
        assert_eq!(path_from_text("   "), None);
        assert_eq!(path_from_text(r"C:\definitely\not\here.png"), None);
    }

    /// The one test that does touch the real clipboard, which is why it has to be
    /// asked for: `cargo test -p imaginer-ui -- --ignored --test-threads=1`.
    ///
    /// Worth having despite that, because it is the only thing that checks the claim
    /// this whole module rests on — that what arboard hands back is the same layout
    /// that went in. A red channel that came back as blue, or a row order flipped on
    /// the way through the bitmap, would look like a plausible image on screen and be
    /// wrong in a way no amount of reading the code catches.
    #[test]
    #[ignore = "round-trips through the real clipboard, throwing away whatever was on it"]
    fn pixels_survive_the_round_trip_through_the_clipboard() {
        // Asymmetric on every axis: a solid colour would pass even if rows, columns
        // and channels were all scrambled.
        let mut original = RgbaImage::new(4, 3);
        for (x, y, pixel) in original.enumerate_pixels_mut() {
            *pixel = imaginer_core::image::Rgba([
                (x * 60) as u8,
                (y * 80) as u8,
                if x == 0 && y == 0 { 255 } else { 7 },
                255,
            ]);
        }

        copy_image(&original).unwrap();
        let Pasted::Image(returned) = paste().unwrap() else {
            panic!("the clipboard gave back a path after being handed pixels");
        };

        assert_eq!(returned.dimensions(), original.dimensions());
        assert_eq!(returned.as_raw(), original.as_raw());
    }

    #[test]
    fn a_real_file_this_build_cannot_open_is_refused() {
        // On disk, and named like a path, but nothing this app could show — which is
        // the case that would otherwise open an empty window and an error.
        let path = scratch("notes.txt");

        assert_eq!(path_from_text(&path.display().to_string()), None);

        std::fs::remove_file(&path).unwrap();
    }
}
