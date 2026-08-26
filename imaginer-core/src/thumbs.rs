//! A cache of small preview images on disk, one file per thumbnail.
//!
//! Cameras embed a thumbnail in their EXIF block, which is what makes the first
//! paint fast — but screenshots, PNGs and downloads have none, and without one
//! every step back to such an image pays the full decode just to show a
//! half-megapixel stand-in. This cache gives every file an EXIF-style thumbnail
//! whether its format has one or not.
//!
//! One *file* per thumbnail, deliberately, rather than one database file for all
//! of them: two Imaginer instances can then write concurrently without anybody
//! taking a lock, a crash mid-write costs one thumbnail rather than the whole
//! cache, and reading is an open call with no index to load. The version key is
//! baked into the file name — path plus size plus mtime — so a saved-over image
//! simply lands in a new file and the stale one stops being asked for.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use image::RgbaImage;

/// Longest side of a stored thumbnail, in pixels.
///
/// The preview's job is layout and "something correct" while the full decode
/// runs — a quarter-megapixel picture does that at any window size this app can
/// have. Bigger would cost decode time and disk for nothing the eye sees.
pub const MAX_SIDE: u32 = 256;

/// When the directory holds more than this many thumbnails, the oldest are
/// deleted until it holds [`PRUNE_KEEP`].
const PRUNE_WHEN: usize = 4096;
const PRUNE_KEEP: usize = 3600;

/// The root of the thumbnail cache.
///
/// Under `LOCALAPPDATA` because that is what it is for: data this program made
/// that no user should have to see. `IMAGINER_THUMB_DIR` overrides it — an
/// investigation knob like `IMAGINER_CACHE_MB`, and what the tests use to work
/// in a scratch directory instead of the real cache.
pub fn cache_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("IMAGINER_THUMB_DIR") {
        return Some(PathBuf::from(dir));
    }
    let local = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(local).join("imaginer").join("thumbs"))
}

/// FNV-1a: unimpressive as hashes go, and deliberately so. The requirements are
/// only that the same file always maps to the same name and that two different
/// ones almost never collide within a cache of a few thousand entries — a
/// cryptographic hash would buy nothing and cost a dependency.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The cache file name for `path` as it looks right now.
///
/// `None` when the file cannot be stamped — deleted, unreadable — which is also
/// the answer "should we read or write a thumbnail for this?" deserves then.
fn entry_name(path: &Path) -> Option<String> {
    let stamp = crate::cache::Stamp::of(path)?;

    let mut mixed = Vec::with_capacity(path.as_os_str().len() + 24);
    mixed.extend_from_slice(path.as_os_str().to_string_lossy().as_bytes());
    // Fixed-width little-endian, so the hash does not depend on Debug formats.
    mixed.extend_from_slice(&stamp.file_size().to_le_bytes());
    let since_epoch = stamp
        .modified()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    mixed.extend_from_slice(&since_epoch.as_secs().to_le_bytes());
    mixed.extend_from_slice(&since_epoch.subsec_nanos().to_le_bytes());

    Some(format!("{:016x}.png", fnv1a(&mixed)))
}

/// The cached thumbnail for `path`, if there is one and it decodes.
///
/// Anything wrong here — missing, truncated, corrupt — is answered with `None`
/// and nothing more. A thumbnail is an optimisation; every failure mode short of
/// the source file being gone has an ordinary fallback waiting behind it.
pub fn load(path: &Path) -> Option<RgbaImage> {
    load_from(&cache_dir()?, path)
}

/// [`load`] against an explicit directory, which is what the tests use to work
/// in a scratch directory rather than the machine's real cache.
fn load_from(dir: &Path, path: &Path) -> Option<RgbaImage> {
    let name = entry_name(path)?;
    let img = image::open(dir.join(name)).ok()?;
    // Stored pixels are already oriented, exactly like an EXIF thumbnail.
    Some(img.into_rgba8())
}

/// Make sure a thumbnail of `source` exists, generating it if not.
///
/// Fire-and-forget by design: called from background threads (the decoder and
/// the prefetcher), it treats a full disk or an unwritable directory the way a
/// memoising cache treats any miss it cannot fill — silently. The next open of
/// this image falls back to the plain decode, which always works.
pub fn ensure(source: &Path, pixels: &RgbaImage) {
    if let Some(dir) = cache_dir() {
        ensure_in(&dir, source, pixels);
    }
}

/// [`ensure`] against an explicit directory; see [`load_from`].
fn ensure_in(dir: &Path, source: &Path, pixels: &RgbaImage) {
    let Some(name) = entry_name(source) else {
        return;
    };
    let target = dir.join(&name);
    if target.exists() {
        return;
    }
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }

    // Via a temp name beside the target: a reader that arrives mid-write would
    // otherwise decode a half-written PNG. Same volume, so the rename is atomic.
    let temp = dir.join(format!("{name}.tmp-{}", std::process::id()));
    let written = File::create(&temp).map(|file| {
        image::DynamicImage::ImageRgba8(scaled(pixels))
            .write_to(&mut std::io::BufWriter::new(file), image::ImageFormat::Png)
    });
    match written {
        // The BufWriter is dropped above, so the handle is closed and Windows
        // will let the rename through.
        Ok(Ok(())) => {
            let _ = std::fs::rename(&temp, &target);
        }
        _ => {
            let _ = std::fs::remove_file(&temp);
        }
    }

    prune(dir);
}

/// Downscale to [`MAX_SIDE`] preserving aspect, never upscaling.
fn scaled(pixels: &RgbaImage) -> RgbaImage {
    let (w, h) = pixels.dimensions();
    let longest = w.max(h);
    if longest <= MAX_SIDE || longest == 0 {
        return pixels.clone();
    }
    let scale = f64::from(MAX_SIDE) / f64::from(longest);
    let tw = ((f64::from(w) * scale).round() as u32).max(1);
    let th = ((f64::from(h) * scale).round() as u32).max(1);
    image::imageops::resize(pixels, tw, th, image::imageops::FilterType::Triangle)
}

/// Delete the oldest thumbnails until the directory holds at most
/// [`PRUNE_KEEP`] files.
///
/// Called after each store, off the UI thread. With one file per thumbnail the
/// directory is the whole index, so pruning is a listing and a sort — a few
/// thousand entries, done rarely enough that nobody waits on it.
fn prune(dir: &Path) {
    prune_dir(dir, PRUNE_WHEN, PRUNE_KEEP);
}

fn prune_dir(dir: &Path, when: usize, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(SystemTime, PathBuf)> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((modified, entry.path()))
        })
        .collect();

    if files.len() <= when {
        return;
    }
    let excess = files.len() - keep;
    files.sort_by_key(|(modified, _)| *modified);
    for (_, path) in files.into_iter().take(excess) {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("imaginer-thumbs-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A real image on disk, so `entry_name` can stamp it.
    fn source_image(dir: &Path, name: &str, colour: [u8; 4]) -> PathBuf {
        let path = dir.join(name);
        RgbaImage::from_pixel(32, 16, image::Rgba(colour))
            .save(&path)
            .unwrap();
        path
    }

    #[test]
    fn a_stored_thumbnail_round_trips() {
        let dir = scratch("roundtrip");
        let source = source_image(&dir, "img.png", [10, 20, 30, 255]);

        let big = RgbaImage::from_pixel(800, 400, image::Rgba([10, 20, 30, 255]));
        ensure_in(&dir, &source, &big);
        let loaded = load_from(&dir, &source).expect("thumbnail should be cached");

        // Downscaled to the cap, aspect kept.
        assert_eq!(loaded.dimensions(), (MAX_SIDE, MAX_SIDE / 2));
        assert_eq!(loaded.get_pixel(0, 0).0, [10, 20, 30, 255]);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn small_images_are_stored_at_their_own_size() {
        let dir = scratch("small");
        let source = source_image(&dir, "tiny.png", [1, 2, 3, 255]);
        ensure_in(
            &dir,
            &source,
            &RgbaImage::from_pixel(24, 12, image::Rgba([1, 2, 3, 255])),
        );

        assert_eq!(load_from(&dir, &source).unwrap().dimensions(), (24, 12));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_changed_file_maps_to_a_new_entry() {
        let dir = scratch("changed");
        let source = source_image(&dir, "img.png", [10, 20, 30, 255]);
        ensure_in(
            &dir,
            &source,
            &RgbaImage::from_pixel(64, 64, image::Rgba([9, 9, 9, 255])),
        );
        assert!(load_from(&dir, &source).is_some());

        // Saved over: different length, therefore a different stamp, therefore
        // a different name — the old thumbnail must stop being served even
        // before anything new has been written.
        std::fs::write(&source, vec![7u8; 5000]).unwrap();
        assert!(
            load_from(&dir, &source).is_none(),
            "stale thumbnail was served"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_corrupt_thumbnail_is_not_a_crash() {
        let dir = scratch("corrupt");
        let source = source_image(&dir, "img.png", [1, 1, 1, 255]);

        let name = entry_name(&source).unwrap();
        std::fs::write(dir.join(name), b"not a png at all").unwrap();

        assert!(load_from(&dir, &source).is_none());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_unstampable_source_has_no_entry_name() {
        assert!(entry_name(Path::new("definitely-not-here.png")).is_none());
    }

    #[test]
    fn pruning_keeps_the_newest_and_drops_the_oldest() {
        let dir = scratch("prune");
        // Written one after another, so each file's mtime lands after the last
        // one's — NTFS timestamps have far finer resolution than this loop.
        for i in 0..5u64 {
            std::fs::write(dir.join(format!("{i}.png")), b"x").unwrap();
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        prune_dir(&dir, 4, 3);

        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, vec!["2.png", "3.png", "4.png"]);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
