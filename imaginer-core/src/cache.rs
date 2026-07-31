//! Keeping decoded images around, inside a fixed memory budget.
//!
//! Decoding is the expensive part of putting a photograph on screen — hundreds of
//! milliseconds for a 24MP JPEG, against a few for the upload that follows. Stepping
//! back to an image that was on screen a moment ago should not pay that a second
//! time, and neither should stepping forward to one a background thread has already
//! prepared.
//!
//! Budgeted in bytes rather than in entries, because entries are not comparable: a
//! count that holds twenty phone snaps comfortably will hold twenty 100MP scans
//! straight into swap. A decoded image is always width x height x 4, whatever it
//! cost on disk, so the budget is measured against something knowable.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::decode::Decoded;

/// How much memory decoded images may occupy by default.
///
/// 512MB is around a dozen 24MP photographs at RGBA8 — comfortably more than the
/// neighbourhood of any one position in a folder, which is all this has to hold to
/// do its job.
pub const DEFAULT_BUDGET: usize = 512 * 1024 * 1024;

/// What a file looked like on disk when it was decoded.
///
/// A cache keyed on the path alone would be wrong in the one case that matters:
/// saving over the image being viewed leaves the path identical and the pixels
/// completely different. Length and modification time together catch that, and both
/// come out of the single `metadata` call the viewer already makes for the status bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stamp {
    file_size: u64,
    modified: Option<SystemTime>,
}

impl Stamp {
    /// Stamp `path` as it is right now, or `None` if it cannot be read at all.
    pub fn of(path: &Path) -> Option<Self> {
        let metadata = std::fs::metadata(path).ok()?;
        Some(Self {
            file_size: metadata.len(),
            // Not every filesystem reports one. Missing on both sides compares
            // equal, which leaves the length doing the work alone — weaker, but the
            // alternative is refusing to cache at all there.
            modified: metadata.modified().ok(),
        })
    }

    /// Size of the file on disk, in bytes.
    pub fn file_size(&self) -> u64 {
        self.file_size
    }
}

struct Entry {
    decoded: Decoded,
    stamp: Stamp,
    /// Held rather than recomputed, so that eviction subtracts exactly what
    /// insertion added even if the entry is replaced.
    bytes: usize,
}

/// Decoded images, held until the budget says otherwise.
pub struct ImageCache {
    budget: usize,
    used: usize,
    entries: HashMap<PathBuf, Entry>,
    /// Paths in use order, least recently used first.
    ///
    /// A `Vec` rather than an intrusive list, because at 512MB this holds a few
    /// dozen entries at most: the linear scans are over a handful of pointers, and
    /// the order is readable in a debugger.
    order: Vec<PathBuf>,
}

impl Default for ImageCache {
    fn default() -> Self {
        Self::with_budget(DEFAULT_BUDGET)
    }
}

impl ImageCache {
    pub fn with_budget(budget: usize) -> Self {
        Self {
            budget,
            used: 0,
            entries: HashMap::new(),
            order: Vec::new(),
        }
    }

    pub fn budget(&self) -> usize {
        self.budget
    }

    /// Bytes of decoded pixels currently held.
    pub fn used(&self) -> usize {
        self.used
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether this exact file is already held — a peek, which does not count as use.
    ///
    /// This is what the prefetch thread asks before deciding to decode. Renewing an
    /// entry because a background thread looked at it would let prefetching keep
    /// images alive that the user has walked away from.
    pub fn holds(&self, path: &Path, stamp: &Stamp) -> bool {
        self.entries
            .get(path)
            .is_some_and(|entry| entry.stamp == *stamp)
    }

    /// Take the decoded image for `path`, if it is held and still matches the file.
    ///
    /// Counts as use, so what the viewer actually shows is what survives eviction.
    pub fn get(&mut self, path: &Path, stamp: &Stamp) -> Option<Decoded> {
        let entry = self.entries.get(path)?;

        // Same path, different file. Saving over the image on screen is how this
        // happens in practice, and serving the pixels from before the save would
        // show the user something their own file no longer contains.
        if entry.stamp != *stamp {
            self.remove(path);
            return None;
        }

        let decoded = entry.decoded.clone();
        self.touch(path);
        Some(decoded)
    }

    pub fn insert(&mut self, path: PathBuf, stamp: Stamp, decoded: Decoded) {
        // Through `remove` first: re-inserting a path is normal — it is what
        // happens when the viewer re-decodes a file that changed — and adding the
        // new bytes without subtracting the old ones would leak budget for the rest
        // of the session.
        self.remove(&path);

        let bytes = decoded.byte_size();
        self.used += bytes;
        self.entries.insert(
            path.clone(),
            Entry {
                decoded,
                stamp,
                bytes,
            },
        );
        self.order.push(path);
        self.evict();
    }

    /// Drop `path`, whatever state it is in. Deleting a file is the caller for this.
    pub fn remove(&mut self, path: &Path) {
        self.order.retain(|held| held != path);
        if let Some(entry) = self.entries.remove(path) {
            self.used = self.used.saturating_sub(entry.bytes);
        }
    }

    /// Move `path` to the most-recently-used end.
    fn touch(&mut self, path: &Path) {
        if let Some(index) = self.order.iter().position(|held| held == path) {
            let path = self.order.remove(index);
            self.order.push(path);
        }
    }

    fn evict(&mut self) {
        // Down to one entry, never to none. An image larger than the entire budget
        // is still an image the viewer is about to show, and evicting it on the way
        // in would mean the last insert silently did nothing.
        while self.used > self.budget && self.order.len() > 1 {
            let oldest = self.order.remove(0);
            if let Some(entry) = self.entries.remove(&oldest) {
                self.used = self.used.saturating_sub(entry.bytes);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::Stage;
    use crate::metadata::Orientation;
    use std::sync::Arc;

    /// A square decoded image, `side * side * 4` bytes of pixels.
    fn decoded(side: u32) -> Decoded {
        Decoded {
            pixels: Arc::new(image::RgbaImage::new(side, side)),
            stage: Stage::Full,
            orientation: Orientation::Normal,
            full_size: (side, side),
        }
    }

    /// A stamp that is not tied to any file on disk, so the tests exercise the
    /// bookkeeping without touching a filesystem.
    fn stamp(file_size: u64) -> Stamp {
        Stamp {
            file_size,
            modified: None,
        }
    }

    /// 16x16 RGBA8 — one kilobyte exactly, which makes the budgets below readable.
    const KB: usize = 16 * 16 * 4;

    fn fill(cache: &mut ImageCache, name: &str) {
        cache.insert(PathBuf::from(name), stamp(1), decoded(16));
    }

    #[test]
    fn evicts_the_least_recently_used_first() {
        let mut cache = ImageCache::with_budget(2 * KB);
        fill(&mut cache, "a.png");
        fill(&mut cache, "b.png");
        fill(&mut cache, "c.png");

        assert_eq!(cache.len(), 2);
        assert_eq!(cache.used(), 2 * KB);
        assert!(!cache.holds(Path::new("a.png"), &stamp(1)));
        assert!(cache.holds(Path::new("c.png"), &stamp(1)));
    }

    #[test]
    fn a_hit_renews_an_entry_so_the_next_eviction_takes_another() {
        let mut cache = ImageCache::with_budget(2 * KB);
        fill(&mut cache, "a.png");
        fill(&mut cache, "b.png");

        // `a` is the oldest until it is looked at, which is the whole point of an
        // LRU: stepping back to an image is what should keep it.
        assert!(cache.get(Path::new("a.png"), &stamp(1)).is_some());
        fill(&mut cache, "c.png");

        assert!(cache.holds(Path::new("a.png"), &stamp(1)));
        assert!(!cache.holds(Path::new("b.png"), &stamp(1)));
    }

    #[test]
    fn a_peek_does_not_renew() {
        let mut cache = ImageCache::with_budget(2 * KB);
        fill(&mut cache, "a.png");
        fill(&mut cache, "b.png");

        assert!(cache.holds(Path::new("a.png"), &stamp(1)));
        fill(&mut cache, "c.png");

        assert!(!cache.holds(Path::new("a.png"), &stamp(1)));
    }

    #[test]
    fn a_file_that_changed_on_disk_is_not_served_and_is_dropped() {
        let mut cache = ImageCache::with_budget(8 * KB);
        fill(&mut cache, "a.png");

        assert!(cache.get(Path::new("a.png"), &stamp(2)).is_none());
        // Not merely refused: the pixels are stale for good, so holding them would
        // be spending the budget on something that can never be served.
        assert!(cache.is_empty());
        assert_eq!(cache.used(), 0);
    }

    #[test]
    fn an_image_larger_than_the_whole_budget_is_still_held() {
        let mut cache = ImageCache::with_budget(KB);
        cache.insert(PathBuf::from("huge.png"), stamp(1), decoded(64));

        assert_eq!(cache.len(), 1);
        assert!(cache.used() > cache.budget());
    }

    #[test]
    fn re_inserting_a_path_does_not_double_count_its_bytes() {
        let mut cache = ImageCache::with_budget(8 * KB);
        cache.insert(PathBuf::from("a.png"), stamp(1), decoded(16));
        cache.insert(PathBuf::from("a.png"), stamp(2), decoded(16));

        assert_eq!(cache.len(), 1);
        assert_eq!(cache.used(), KB);
        // The newer stamp is the one held; the older file is gone.
        assert!(cache.holds(Path::new("a.png"), &stamp(2)));
    }

    #[test]
    fn removing_frees_the_budget_it_was_using() {
        let mut cache = ImageCache::with_budget(8 * KB);
        fill(&mut cache, "a.png");
        cache.remove(Path::new("a.png"));

        assert!(cache.is_empty());
        assert_eq!(cache.used(), 0);
        // Removing something that was never there is not an error — deleting a file
        // the cache never saw is an ordinary thing to do.
        cache.remove(Path::new("a.png"));
        assert_eq!(cache.used(), 0);
    }

    #[test]
    fn a_rewritten_file_stamps_differently() {
        let path = std::env::temp_dir().join("imaginer-stamp-test.bin");
        std::fs::write(&path, b"one").unwrap();
        let before = Stamp::of(&path).unwrap();

        // A different length, deliberately: modification times can land in the same
        // filesystem tick when a test writes twice in a row, and the assertion has
        // to hold every run.
        std::fs::write(&path, b"different").unwrap();
        assert_ne!(Stamp::of(&path).unwrap(), before);

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn an_unreadable_path_has_no_stamp() {
        assert!(Stamp::of(Path::new("definitely-not-here.png")).is_none());
    }
}
