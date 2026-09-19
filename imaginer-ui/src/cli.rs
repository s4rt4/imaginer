//! The command line, which exists so conversion does not require a window.
//!
//! ```text
//! imaginer <file>                                   open the viewer
//! imaginer --convert <format> [options] <files...>  convert and exit
//!
//!   --quality <1-100>   for the lossy formats; ignored by the others
//!   --out <directory>   skip the folder dialog, for scripts and benchmarks
//!   --collect           merge with sibling processes started at the same time
//! ```
//!
//! The conversion path never constructs an eframe window: it decodes, encodes and
//! exits. That is the point — the Explorer context menu is a caller of this, and
//! flashing a viewer up to convert a file would defeat it.
//!
//! Where the output goes and what it is called was settled with the user: a folder
//! dialog once per run, the source's stem with the new extension, and an existing
//! file overwritten without asking.

use std::path::PathBuf;
use std::process::ExitCode;

use imaginer_core::convert::{self, Outcome};
use imaginer_core::{ExportSettings, Format};

use crate::collector;

/// What the arguments asked for.
#[derive(Debug)]
pub enum Launch {
    /// Open the viewer, on the given file if there was one.
    Viewer(Option<PathBuf>),
    Convert(Request),
}

#[derive(Debug)]
pub struct Request {
    pub format: Format,
    pub quality: u8,
    /// Where to write. `None` means ask, which is the default and what the context
    /// menu will hit.
    pub out_dir: Option<PathBuf>,
    pub files: Vec<PathBuf>,
    /// Whether to merge with sibling processes before converting.
    ///
    /// Only the shell integration sets this: Explorer launches a classic verb once
    /// per selected file, and the batch has to be reassembled. A conversion typed
    /// at a prompt already has every file on its command line, and would only be
    /// paying half a second to discover that.
    pub collect: bool,
}

/// Read the process arguments.
///
/// Errors are for arguments that are wrong rather than missing — an unknown format
/// or a quality outside the dial. Anything the CLI does not recognise as a flag is
/// a file, so a plain `imaginer photo.png` still opens the viewer.
pub fn parse(args: impl IntoIterator<Item = std::ffi::OsString>) -> Result<Launch, String> {
    let mut args = args.into_iter().skip(1).peekable();

    let mut format = None;
    let mut quality = None;
    let mut out_dir = None;
    let mut collect = false;
    let mut files = Vec::new();

    while let Some(arg) = args.next() {
        // Flags are ASCII, so a non-UTF-8 argument can only ever be a path — and
        // paths must survive intact, which is why they stay `OsString`.
        match arg.to_str() {
            Some("--convert") => {
                let name = args
                    .next()
                    .ok_or("--convert needs a format, one of: png jpg webp bmp ico")?;
                let name = name.to_string_lossy().into_owned();
                format =
                    Some(Format::from_name(&name).ok_or_else(|| {
                        format!("cannot write {name:?}; try png jpg webp bmp ico")
                    })?);
            }
            Some("--quality") => {
                let value = args
                    .next()
                    .ok_or("--quality needs a number from 1 to 100")?;
                let value = value.to_string_lossy().into_owned();
                let parsed: u8 = value.parse().map_err(|_| {
                    format!("--quality wants a number from 1 to 100, not {value:?}")
                })?;
                if !(1..=100).contains(&parsed) {
                    return Err(format!("--quality is 1 to 100, not {parsed}"));
                }
                quality = Some(parsed);
            }
            Some("--out") => {
                let dir = args.next().ok_or("--out needs a directory")?;
                out_dir = Some(PathBuf::from(dir));
            }
            Some("--collect") => collect = true,
            Some(other) if other.starts_with("--") => {
                return Err(format!("unknown option {other}"));
            }
            _ => files.push(PathBuf::from(arg)),
        }
    }

    let Some(format) = format else {
        // No --convert: the first path, if any, is the image to open. Extra paths
        // are ignored rather than rejected — multi-window is a deferred decision,
        // and refusing to start is a worse answer than opening the first one.
        return Ok(Launch::Viewer(files.into_iter().next()));
    };

    if files.is_empty() {
        return Err("--convert needs at least one file".to_owned());
    }

    Ok(Launch::Convert(Request {
        format,
        quality: quality.unwrap_or(ExportSettings::default().quality),
        out_dir,
        files,
        collect,
    }))
}

/// Convert, report, and produce the process exit code.
pub fn run(mut request: Request) -> ExitCode {
    let settings = ExportSettings {
        format: request.format,
        quality: request.quality,
        scale_percent: 100,
    };

    // Gathering comes before the dialog, so the one folder question covers the whole
    // selection rather than the first file of it.
    if request.collect {
        match collector::join(&batch_key(&request), std::mem::take(&mut request.files)) {
            collector::Role::Collector(files) => request.files = files,
            // Another process owns this batch and now has our paths. Exiting quietly
            // is the entire contribution.
            collector::Role::HandedOver => return ExitCode::SUCCESS,
        }
    }

    let Some(directory) = destination_directory(&request) else {
        // The folder dialog was dismissed. Cancelling is not an error, and saying
        // so in a second dialog would be noise.
        return ExitCode::SUCCESS;
    };

    let total = request.files.len();
    println!(
        "Converting {total} file{} to {}",
        if total == 1 { "" } else { "s" },
        settings.format.label()
    );

    let outcomes = convert::convert_all(
        &request.files,
        &directory,
        &settings,
        |done, total, outcome| match outcome {
            Outcome::Converted {
                destination,
                bytes,
                source_bytes,
                ..
            } => println!(
                "[{done}/{total}] {} — {} to {}{}",
                destination.display(),
                human_bytes(*source_bytes),
                human_bytes(*bytes),
                saving(*source_bytes, *bytes),
            ),
            Outcome::Failed { source, reason } => {
                eprintln!("[{done}/{total}] {} — FAILED: {reason}", source.display());
            }
        },
    );

    report(&outcomes, &directory)
}

/// What makes two processes part of the same batch.
///
/// Format and quality, because those are the whole of what the verb decided. Two
/// menu picks in quick succession — WebP for one selection, ICO for another — must
/// not merge into one conversion, and they would if the key were fixed.
fn batch_key(request: &Request) -> String {
    format!("{}-{}", request.format.extension(), request.quality)
}

/// Where to write: what `--out` said, or whatever the folder dialog returns.
///
/// The dialog opens on the first file's own folder, which is nearly always where
/// the output is wanted and saves the user navigating back to where they started.
fn destination_directory(request: &Request) -> Option<PathBuf> {
    if let Some(dir) = &request.out_dir {
        return Some(dir.clone());
    }

    // Said before the dialog opens, not after. Launched from Explorer nobody is
    // reading this; typed at a prompt it is the difference between a command that
    // is waiting for you and one that has hung, and the dialog can easily come up
    // behind the terminal that started it.
    //
    // Flushed by hand, which is the whole point of it: Rust buffers stdout by
    // block when it is not a terminal, so redirected to a file or a pipe this
    // line would sit unwritten for exactly as long as the dialog blocks — which
    // is precisely the stretch it exists to explain.
    println!("Choose a destination folder for the converted file(s)...");
    let _ = std::io::Write::flush(&mut std::io::stdout());

    let mut dialog = rfd::FileDialog::new().set_title(format!(
        "Convert {} file{} to {} — choose a destination",
        request.files.len(),
        if request.files.len() == 1 { "" } else { "s" },
        request.format.label(),
    ));
    if let Some(parent) = request
        .files
        .first()
        .and_then(|file| file.parent())
        .filter(|parent| parent.is_dir())
    {
        dialog = dialog.set_directory(parent);
    }
    dialog.pick_folder()
}

/// Print the summary and decide the exit code.
///
/// Failures also raise a dialog, because launched from Explorer there is no console
/// to print to and a conversion that silently did nothing is the worst outcome
/// available. A clean run stays quiet: the written files are the evidence.
fn report(outcomes: &[Outcome], directory: &std::path::Path) -> ExitCode {
    let failures: Vec<&Outcome> = outcomes.iter().filter(|o| o.is_failure()).collect();
    let converted = outcomes.len() - failures.len();

    println!("{converted} written to {}", directory.display());

    if failures.is_empty() {
        return ExitCode::SUCCESS;
    }

    let mut message = format!(
        "{converted} of {} converted. These failed:\n",
        outcomes.len()
    );
    for outcome in &failures {
        if let Outcome::Failed { source, reason } = outcome {
            let name = source
                .file_name()
                .unwrap_or(source.as_os_str())
                .to_string_lossy();
            message.push_str(&format!("\n{name} — {reason}"));
        }
    }
    eprintln!("{message}");

    rfd::MessageDialog::new()
        .set_title("Imaginer — conversion")
        .set_description(&message)
        .set_level(rfd::MessageLevel::Warning)
        .show();

    ExitCode::FAILURE
}

/// Show a usage error the same way, whether or not there is a console to see it.
pub fn report_usage_error(message: &str) -> ExitCode {
    eprintln!("imaginer: {message}");
    rfd::MessageDialog::new()
        .set_title("Imaginer")
        .set_description(message)
        .set_level(rfd::MessageLevel::Error)
        .show();
    ExitCode::FAILURE
}

fn human_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let bytes = bytes as f64;
    if bytes < KB {
        format!("{bytes:.0}B")
    } else if bytes < KB * KB {
        format!("{:.1}KB", bytes / KB)
    } else {
        format!("{:.2}MB", bytes / (KB * KB))
    }
}

/// The percentage saved, or nothing when the file grew — a "-40% larger" reads
/// worse than simply not claiming a saving.
fn saving(before: u64, after: u64) -> String {
    if before == 0 || after >= before {
        return String::new();
    }
    let percent = 100.0 - (after as f64 / before as f64) * 100.0;
    format!(" ({percent:.0}% smaller)")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_args(args: &[&str]) -> Result<Launch, String> {
        // The real `args_os` includes the executable, which `parse` skips.
        let full = std::iter::once("imaginer")
            .chain(args.iter().copied())
            .map(std::ffi::OsString::from);
        parse(full)
    }

    fn convert_request(args: &[&str]) -> Request {
        match parse_args(args).unwrap() {
            Launch::Convert(request) => request,
            Launch::Viewer(_) => panic!("expected a conversion, got the viewer"),
        }
    }

    #[test]
    fn no_arguments_opens_an_empty_viewer() {
        assert!(matches!(parse_args(&[]).unwrap(), Launch::Viewer(None)));
    }

    #[test]
    fn a_bare_path_still_opens_the_viewer() {
        // This is the file-association and "Open With" path, and it must not
        // regress just because the CLI grew options.
        match parse_args(&[r"C:\photos\a.png"]).unwrap() {
            Launch::Viewer(Some(path)) => assert_eq!(path, PathBuf::from(r"C:\photos\a.png")),
            _ => panic!("expected the viewer on a file"),
        }
    }

    #[test]
    fn convert_collects_every_file_and_defaults_the_quality() {
        let request = convert_request(&["--convert", "webp", "a.png", "b.jpg"]);

        assert_eq!(request.format, Format::WebP);
        assert_eq!(request.quality, ExportSettings::default().quality);
        assert!(
            request.out_dir.is_none(),
            "the dialog should be the default"
        );
        assert_eq!(
            request.files,
            [PathBuf::from("a.png"), PathBuf::from("b.jpg")]
        );
    }

    #[test]
    fn options_are_accepted_after_the_files_as_well_as_before() {
        // Explorer builds command lines by appending, so this ordering is not
        // hypothetical.
        let request = convert_request(&["--convert", "ico", "a.png", "--quality", "80"]);
        assert_eq!(request.quality, 80);
        assert_eq!(request.files, [PathBuf::from("a.png")]);
    }

    #[test]
    fn a_format_may_be_written_with_or_without_its_dot() {
        assert_eq!(
            convert_request(&["--convert", ".WEBP", "a.png"]).format,
            Format::WebP
        );
        assert_eq!(
            convert_request(&["--convert", "jpeg", "a.png"]).format,
            Format::Jpeg
        );
    }

    #[test]
    fn out_skips_the_dialog() {
        let request = convert_request(&["--convert", "png", "--out", r"D:\web", "a.bmp"]);
        assert_eq!(request.out_dir, Some(PathBuf::from(r"D:\web")));
    }

    #[test]
    fn a_format_this_build_cannot_write_is_rejected_up_front() {
        // Rejected at parse time rather than after decoding a hundred files.
        let err = parse_args(&["--convert", "tiff", "a.png"]).unwrap_err();
        assert!(err.contains("tiff"), "unhelpful message: {err}");
    }

    #[test]
    fn quality_outside_the_dial_is_rejected() {
        assert!(parse_args(&["--convert", "webp", "--quality", "0", "a.png"]).is_err());
        assert!(parse_args(&["--convert", "webp", "--quality", "101", "a.png"]).is_err());
        assert!(parse_args(&["--convert", "webp", "--quality", "eighty", "a.png"]).is_err());
    }

    #[test]
    fn convert_without_a_file_is_an_error_rather_than_a_silent_success() {
        assert!(parse_args(&["--convert", "webp"]).is_err());
    }

    #[test]
    fn an_option_missing_its_value_says_so() {
        assert!(parse_args(&["--convert"]).is_err());
        assert!(parse_args(&["--convert", "webp", "a.png", "--out"]).is_err());
    }

    #[test]
    fn an_unknown_option_is_not_mistaken_for_a_filename() {
        let err = parse_args(&["--convert", "webp", "--lossless", "a.png"]).unwrap_err();
        assert!(err.contains("--lossless"), "unhelpful message: {err}");
    }

    #[test]
    fn byte_sizes_read_the_way_a_person_would_write_them() {
        assert_eq!(human_bytes(512), "512B");
        assert_eq!(human_bytes(1024), "1.0KB");
        assert_eq!(human_bytes(1024 * 1024 + 1024 * 512), "1.50MB");
    }

    #[test]
    fn a_saving_is_only_claimed_when_the_file_actually_shrank() {
        assert_eq!(saving(1000, 250), " (75% smaller)");
        assert_eq!(saving(1000, 1000), "");
        assert_eq!(saving(1000, 2000), "");
        assert_eq!(saving(0, 100), "");
    }
}
