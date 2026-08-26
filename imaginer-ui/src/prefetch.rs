//! Decoding the images either side of the one on screen, before they are asked for.
//!
//! Stepping through a folder is the most repeated thing anyone does in a viewer, and
//! the wait it involves is a decode the app could have started seconds earlier. One
//! background thread walks a list of neighbours and fills the cache; the viewer looks
//! there first and, on a hit, puts the image up in the same frame the arrow key
//! arrived in.
//!
//! One thread rather than a pool, deliberately. The work is a queue of two or three
//! images that is thrown away and rebuilt at every keypress, so more threads would
//! mostly decode positions the user has already moved past — and they would compete
//! with the viewer's own decode, which is the one somebody is actually waiting on.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use imaginer_core::cache::{ImageCache, Stamp};
use imaginer_core::{Decoded, decode_full_static};

/// One request: the images to warm, in the order they should be warmed.
type Job = Vec<PathBuf>;

/// The cache, and the thread that fills it.
pub struct Prefetcher {
    cache: Arc<Mutex<ImageCache>>,
    tx: Sender<Job>,
}

impl Prefetcher {
    pub fn with_budget(budget: usize) -> Self {
        let cache = Arc::new(Mutex::new(ImageCache::with_budget(budget)));
        let (tx, rx) = mpsc::channel();

        let worker = Arc::clone(&cache);
        std::thread::Builder::new()
            .name("prefetch".to_owned())
            .spawn(move || warm(&rx, &worker))
            .expect("failed to spawn prefetch thread");

        Self { cache, tx }
    }

    /// Ask for these images to be decoded, dropping whatever was queued before.
    ///
    /// Cheap enough to call on every navigation: it hands over a short list and
    /// returns. Everything after that happens on the worker.
    pub fn request(&self, paths: Job) {
        if paths.is_empty() {
            return;
        }
        // A dead worker is not worth reporting: it can only mean the thread failed
        // to start, and the viewer's own decode path still works without it.
        let _ = self.tx.send(paths);
    }

    /// The decoded image for `path`, if it is already in hand and still matches the
    /// file on disk.
    ///
    /// `want_animation` passes through to the cache: the viewer asking for frames
    /// must not be answered with a still that prefetch happened to store first.
    pub fn cached(&self, path: &Path, stamp: &Stamp, want_animation: bool) -> Option<Decoded> {
        self.lock().get(path, stamp, want_animation)
    }

    /// Keep a decode the viewer did itself, so stepping back to it is free.
    pub fn store(&self, path: PathBuf, stamp: Stamp, decoded: Decoded) {
        self.lock().insert(path, stamp, decoded);
    }

    /// Drop an image from the cache — deleting the file is what calls this.
    pub fn forget(&self, path: &Path) {
        self.lock().remove(path);
    }

    fn lock(&self) -> MutexGuard<'_, ImageCache> {
        lock(&self.cache)
    }
}

/// Take the cache lock, surviving a poisoned mutex.
///
/// Nothing slow ever runs under this lock — decoding happens outside it, and what is
/// left is map bookkeeping that cannot panic. So a poisoned lock would mean something
/// impossible happened, and taking the window down over it helps nobody: the worst a
/// recovered cache can be is wrong about its own byte count.
fn lock(cache: &Mutex<ImageCache>) -> MutexGuard<'_, ImageCache> {
    cache.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The worker loop: decode what was asked for, then sleep until asked again.
fn warm(rx: &Receiver<Job>, cache: &Mutex<ImageCache>) {
    // Blocking rather than spinning: a session that opens one image and never
    // navigates should not have a thread burning a core behind it.
    let Ok(mut job) = rx.recv() else {
        return;
    };

    loop {
        let mut superseded = None;

        // `take` so the job can be replaced from inside the loop it is driving.
        for path in std::mem::take(&mut job) {
            // Checked before each decode rather than after: stepping again means the
            // neighbours have moved, and the rest of this list points at where the
            // user no longer is. Finishing it would delay the images that now matter
            // by a full decode each.
            if let Ok(newer) = rx.try_recv() {
                superseded = Some(newer);
                break;
            }
            warm_one(&path, cache);
        }

        job = match superseded {
            Some(newer) => newer,
            None => match rx.recv() {
                Ok(job) => job,
                // The sender is gone, which means the window closed.
                Err(_) => return,
            },
        };

        // Only the newest request describes where the user actually is; anything
        // queued behind it was already stale when it arrived.
        while let Ok(newer) = rx.try_recv() {
            job = newer;
        }
    }
}

fn warm_one(path: &Path, cache: &Mutex<ImageCache>) {
    // No stamp means no file — it was deleted or renamed between the folder listing
    // and now, which is not worth reporting from a background thread.
    let Some(stamp) = Stamp::of(path) else {
        return;
    };
    if lock(cache).holds(path, &stamp, false) {
        return;
    }

    // Outside the lock, on purpose. This is the slow part, and holding the cache
    // across it would stall the UI thread on its own lookups — turning a prefetch
    // meant to remove a wait into one that causes it.
    //
    // Static decode: an animated neighbour is warmed as its first frame only.
    // Decoding every frame of every GIF beside the cursor would make prefetching
    // animations cost more than not prefetching them; when the viewer actually
    // lands on one it asks for the frame set itself and replaces this entry.
    let Ok(decoded) = decode_full_static(path) else {
        return;
    };

    // Re-checked implicitly by `insert`, which replaces rather than duplicates: the
    // viewer may well have decoded this very image itself while this ran.
    lock(cache).insert(path.to_owned(), stamp, decoded);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// A directory of real PNGs — real, because the point of these tests is that a
    /// separate thread actually decodes them.
    fn scratch(name: &str, files: &[&str]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("imaginer-prefetch-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for file in files {
            imaginer_core::image::RgbaImage::from_pixel(
                8,
                8,
                imaginer_core::image::Rgba([1, 2, 3, 255]),
            )
            .save(dir.join(file))
            .unwrap();
        }
        dir
    }

    /// Wait for `path` to turn up in the cache, or give up.
    ///
    /// Polled rather than signalled: the prefetcher deliberately has no "done" to
    /// report — the viewer only ever asks whether an image is there yet — so a test
    /// that waited on a completion signal would be testing something that does not
    /// exist. The timeout is generous because it only ever runs out on failure.
    fn cached_within(prefetch: &Prefetcher, path: &Path, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if Stamp::of(path).is_some_and(|stamp| prefetch.cached(path, &stamp, false).is_some()) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    #[test]
    fn a_requested_image_is_decoded_and_left_in_the_cache() {
        let dir = scratch("basic", &["a.png"]);
        let prefetch = Prefetcher::with_budget(1024 * 1024);
        let path = dir.join("a.png");

        prefetch.request(vec![path.clone()]);
        assert!(cached_within(&prefetch, &path, Duration::from_secs(5)));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_file_that_will_not_decode_does_not_take_the_thread_with_it() {
        let dir = scratch("broken", &["good.png"]);
        let broken = dir.join("broken.png");
        std::fs::write(&broken, b"not an image").unwrap();

        let prefetch = Prefetcher::with_budget(1024 * 1024);
        // The bad one first, so the good one can only arrive if the worker carried
        // on past it. A neighbour that turns out to be a renamed text file is not a
        // reason for the rest of the folder to stop being prefetched.
        prefetch.request(vec![broken.clone(), dir.join("good.png")]);

        assert!(cached_within(
            &prefetch,
            &dir.join("good.png"),
            Duration::from_secs(5)
        ));
        assert!(!cached_within(
            &prefetch,
            &broken,
            Duration::from_millis(50)
        ));

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
