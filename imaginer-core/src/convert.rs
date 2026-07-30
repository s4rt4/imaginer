//! Converting files without opening them.
//!
//! The viewer's export path starts from pixels that are already on screen. This
//! one starts from a path: decode, then encode, with no window and no edit stack
//! in between. It exists because the user's actual conversion workflow is
//! right-click in Explorer, not open-then-save — see the CLI in the UI crate and
//! the shell integration it feeds.
//!
//! Naming and collision policy were decided by the user rather than inferred:
//! the output keeps the source's stem and takes the new format's extension, and
//! an existing file at that path is overwritten. The destination directory is
//! always supplied by the caller — this module never guesses one.

use std::path::{Path, PathBuf};

use crate::decode::decode_full;
use crate::export::{self, Format, Settings};

/// What happened to one file.
#[derive(Debug, Clone)]
pub enum Outcome {
    Converted {
        source: PathBuf,
        destination: PathBuf,
        /// Size of the file written, for the "1.2MB → 84KB" line that is the whole
        /// reason this feature exists.
        bytes: u64,
        /// Size of the source, for the same reason.
        source_bytes: u64,
    },
    Failed {
        source: PathBuf,
        reason: String,
    },
}

impl Outcome {
    pub fn source(&self) -> &Path {
        match self {
            Self::Converted { source, .. } | Self::Failed { source, .. } => source,
        }
    }

    pub fn is_failure(&self) -> bool {
        matches!(self, Self::Failed { .. })
    }
}

/// Where `source` would be written, converted to `format` inside `directory`.
///
/// Same stem, new extension. A source with no filename at all (a bare `..`, say)
/// falls back to a fixed name rather than producing a path that is a directory.
///
/// The name is assembled by hand rather than with `Path::with_extension`, which
/// replaces everything after the *last* dot: that turns `logo.v2.png` into
/// `logo.ico` and quietly aims two different sources at one destination — which,
/// under an overwrite policy, means losing one of them.
pub fn destination_for(source: &Path, directory: &Path, format: Format) -> PathBuf {
    let stem = source
        .file_stem()
        .filter(|stem| !stem.is_empty())
        .unwrap_or_else(|| std::ffi::OsStr::new("converted"));

    let mut name = stem.to_os_string();
    name.push(".");
    name.push(format.extension());
    directory.join(name)
}

/// Decode `source` and write it into `directory` in the settings' format.
///
/// Returns the path written. The decode is a full one, so EXIF orientation is
/// applied — a converted photo comes out the way up it was displayed, which is
/// the only behaviour that would not read as a bug.
pub fn convert_file(
    source: &Path,
    directory: &Path,
    settings: &Settings,
) -> Result<PathBuf, String> {
    let decoded = decode_full(source).map_err(|err| err.to_string())?;
    let destination = destination_for(source, directory, settings.format);

    // Creating it here rather than once per batch keeps the function usable on its
    // own, and `create_dir_all` on a directory that exists is a cheap no-op.
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("could not create {}: {err}", parent.display()))?;
    }

    export::write(&decoded.pixels, &destination, settings).map_err(|err| err.to_string())?;
    Ok(destination)
}

/// Convert every file in `sources` into `directory`.
///
/// One failure does not stop the batch: a folder of images where the third is
/// corrupt should still convert the other nine, and the caller reports what was
/// skipped at the end. `progress` is called after each file so a long batch can
/// say something while it runs.
pub fn convert_all(
    sources: &[PathBuf],
    directory: &Path,
    settings: &Settings,
    mut progress: impl FnMut(usize, usize, &Outcome),
) -> Vec<Outcome> {
    let total = sources.len();
    let mut outcomes = Vec::with_capacity(total);

    for (index, source) in sources.iter().enumerate() {
        let source_bytes = std::fs::metadata(source)
            .map(|meta| meta.len())
            .unwrap_or(0);

        let outcome = match convert_file(source, directory, settings) {
            Ok(destination) => {
                let bytes = std::fs::metadata(&destination)
                    .map(|meta| meta.len())
                    .unwrap_or(0);
                Outcome::Converted {
                    source: source.clone(),
                    destination,
                    bytes,
                    source_bytes,
                }
            }
            Err(reason) => Outcome::Failed {
                source: source.clone(),
                reason,
            },
        };

        progress(index + 1, total, &outcome);
        outcomes.push(outcome);
    }

    outcomes
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A gradient rather than flat colour, so encoders are not flattered into
    /// producing sizes that say nothing.
    fn artwork(w: u32, h: u32) -> image::RgbaImage {
        image::RgbaImage::from_fn(w, h, |x, y| {
            let r = (x * 255 / w.max(1)) as u8;
            let g = (y * 255 / h.max(1)) as u8;
            image::Rgba([r, g, r ^ g, 255])
        })
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("imaginer-convert-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn settings(format: Format) -> Settings {
        Settings {
            format,
            ..Settings::default()
        }
    }

    #[test]
    fn the_destination_keeps_the_stem_and_takes_the_new_extension() {
        let out = destination_for(
            Path::new(r"C:\photos\holiday.jpeg"),
            Path::new(r"D:\web"),
            Format::WebP,
        );
        assert_eq!(out.file_name().unwrap(), "holiday.webp");
        assert_eq!(out.parent().unwrap(), Path::new(r"D:\web"));
    }

    #[test]
    fn a_dotted_stem_is_not_mistaken_for_an_extension() {
        // `with_extension` replaces only the last component, so the version number
        // survives — which is what anyone naming a file this way expects.
        let out = destination_for(Path::new("logo.v2.png"), Path::new("out"), Format::Ico);
        assert_eq!(out.file_name().unwrap(), "logo.v2.ico");
    }

    #[test]
    fn converts_a_file_into_the_chosen_directory() {
        let dir = scratch("basic");
        let source = dir.join("source.png");
        artwork(48, 32).save(&source).unwrap();

        let out_dir = dir.join("out");
        let written = convert_file(&source, &out_dir, &settings(Format::WebP)).unwrap();

        assert_eq!(written, out_dir.join("source.webp"));
        assert_eq!(image::image_dimensions(&written).unwrap(), (48, 32));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_existing_destination_is_overwritten_rather_than_skipped() {
        let dir = scratch("overwrite");
        let source = dir.join("source.png");
        artwork(64, 64).save(&source).unwrap();

        // Something else is already sitting at the destination, at a size that will
        // give it away if it survives.
        let destination = dir.join("source.bmp");
        artwork(8, 8)
            .save_with_format(&destination, image::ImageFormat::Bmp)
            .unwrap();

        convert_file(&source, &dir, &settings(Format::Bmp)).unwrap();

        assert_eq!(image::image_dimensions(&destination).unwrap(), (64, 64));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn one_bad_file_does_not_abandon_the_rest_of_the_batch() {
        let dir = scratch("batch");
        let good_one = dir.join("one.png");
        let good_two = dir.join("two.png");
        let bad = dir.join("broken.png");
        artwork(20, 20).save(&good_one).unwrap();
        artwork(20, 20).save(&good_two).unwrap();
        std::fs::write(&bad, b"not an image at all").unwrap();

        let out_dir = dir.join("out");
        let sources = vec![good_one, bad.clone(), good_two];

        let mut seen = 0;
        let outcomes = convert_all(
            &sources,
            &out_dir,
            &settings(Format::Bmp),
            |done, total, _| {
                seen += 1;
                assert_eq!(total, 3);
                assert_eq!(done, seen);
            },
        );

        assert_eq!(seen, 3);
        assert!(!outcomes[0].is_failure());
        assert!(outcomes[1].is_failure());
        assert_eq!(outcomes[1].source(), bad);
        assert!(!outcomes[2].is_failure());
        assert!(out_dir.join("one.bmp").exists());
        assert!(out_dir.join("two.bmp").exists());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_outcome_carries_both_sizes_so_the_saving_can_be_reported() {
        let dir = scratch("sizes");
        let source = dir.join("big.bmp");
        // BMP is uncompressed, so the source is reliably the larger of the two and
        // the assertion below is not at the mercy of an encoder's mood.
        artwork(128, 128)
            .save_with_format(&source, image::ImageFormat::Bmp)
            .unwrap();

        let outcomes = convert_all(&[source], &dir, &settings(Format::WebP), |_, _, _| {});

        match &outcomes[0] {
            Outcome::Converted {
                bytes,
                source_bytes,
                ..
            } => {
                assert!(*source_bytes > 0 && *bytes > 0);
                assert!(
                    bytes < source_bytes,
                    "{bytes}B was not under {source_bytes}B"
                );
            }
            other => panic!("expected a conversion, got {other:?}"),
        }

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_source_fails_that_file_and_names_it() {
        let dir = scratch("missing");
        let missing = dir.join("nope.png");

        let outcomes = convert_all(
            std::slice::from_ref(&missing),
            &dir,
            &settings(Format::Png),
            |_, _, _| {},
        );

        match &outcomes[0] {
            Outcome::Failed { source, reason } => {
                assert_eq!(source, &missing);
                assert!(reason.contains("nope.png"), "unhelpful reason: {reason}");
            }
            other => panic!("expected a failure, got {other:?}"),
        }

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
