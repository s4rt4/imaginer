//! The images sitting alongside the one on screen.
//!
//! Stepping to the next photo is the single most repeated action in a viewer, so
//! the list it steps through is built once when a file is opened rather than
//! re-scanned per keypress. It is also what the slideshow steps through and what
//! prefetch reads to decide which images to warm, which is why it lives in core
//! rather than next to the UI that happens to use it first.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::decode::is_supported;

/// What a listing is ordered by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey {
    /// The filename, case-insensitively.
    #[default]
    Name,
    /// When the file was last written.
    Modified,
    /// How many bytes it takes on disk.
    Size,
}

impl SortKey {
    /// Every key, in the order a menu should offer them: the one people want most
    /// often first.
    pub const ALL: [Self; 3] = [Self::Name, Self::Modified, Self::Size];

    pub fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Modified => "Date",
            Self::Size => "Size",
        }
    }
}

/// A sort key and a direction.
///
/// The direction is kept separate from the key rather than folded into it — six
/// variants for what is really two independent choices, and switching from date to
/// size would silently reverse the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Order {
    pub key: SortKey,
    /// Z first, newest first, largest first.
    pub descending: bool,
}

impl Order {
    /// How the app starts: by name, A to Z. What every file manager does, so it is
    /// the one order nobody has to be told about.
    pub const DEFAULT: Self = Self {
        key: SortKey::Name,
        descending: false,
    };

    pub fn is_default(self) -> bool {
        self == Self::DEFAULT
    }
}

/// One image in the listing, with what it takes to sort it.
///
/// The size and the timestamp are captured during the scan rather than looked up
/// when a sort asks for them: on Windows `FindNextFile` returns both alongside the
/// name, so a listing of thousands costs no syscalls beyond the walk itself, and
/// re-sorting later touches no disk at all.
#[derive(Debug)]
struct Entry {
    path: PathBuf,
    /// The filename, lowercased once here rather than on every comparison — a sort
    /// asks for its key O(n log n) times.
    sort_name: String,
    len: u64,
    /// `None` when the filesystem would not say. Sorts before every real timestamp,
    /// which puts the handful of odd files at one end instead of scattering them.
    modified: Option<SystemTime>,
}

/// The supported images in one directory, in display order, with a cursor.
#[derive(Debug, Default)]
pub struct Folder {
    entries: Vec<Entry>,
    /// Index into `entries`. `None` when the directory holds no images at all, or
    /// when the file that was opened is not in it — a path typed on the command
    /// line need not live anywhere in particular.
    current: Option<usize>,
    order: Order,
}

impl Folder {
    /// List the directory `path` lives in, positioned at `path` itself.
    pub fn containing(path: &Path, order: Order) -> Self {
        let Some(dir) = path.parent() else {
            return Self::default();
        };

        let entries = list(dir, order);
        let current = entries.iter().position(|entry| entry.path == path);
        Self {
            entries,
            current,
            order,
        }
    }

    /// List `dir`, positioned at its first image.
    pub fn of_directory(dir: &Path, order: Order) -> Self {
        let entries = list(dir, order);
        let current = (!entries.is_empty()).then_some(0);
        Self {
            entries,
            current,
            order,
        }
    }

    pub fn order(&self) -> Order {
        self.order
    }

    /// Re-order the listing, without moving off the image being looked at.
    ///
    /// Keeping the cursor on the same file is the whole behaviour: sorting answers
    /// "what comes next", not "show me something else", and a re-sort that jumped to
    /// whatever landed at the old index would lose the photograph you were reading
    /// the order for. Position and neighbours change; the picture does not.
    ///
    /// Re-sorts what is already in memory. Files added or removed by other programs
    /// since the scan are not picked up — that is a rescan, and this is not one.
    pub fn set_order(&mut self, order: Order) {
        if order == self.order {
            return;
        }
        self.order = order;

        let showing = self.current().map(Path::to_path_buf);
        sort(&mut self.entries, order);
        self.current =
            showing.and_then(|path| self.entries.iter().position(|entry| entry.path == path));
    }

    pub fn current(&self) -> Option<&Path> {
        Some(self.entries.get(self.current?)?.path.as_path())
    }

    /// Where the cursor sits, as the 1-based pair a status bar would show.
    pub fn position(&self) -> Option<(usize, usize)> {
        Some((self.current? + 1, self.entries.len()))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The images either side of the cursor, nearest first and forward before back.
    ///
    /// Forward first because that is the direction most sessions move in, and the
    /// prefetch queue is worked in order: the image most likely to be asked for next
    /// should not sit behind the one least likely.
    ///
    /// Wraps the way stepping does, so the last image in a folder still has a next.
    /// Never returns the current image, and never the same one twice — in a folder of
    /// two, forward and back are the same file, and decoding it twice would be work
    /// spent to learn nothing.
    ///
    /// Takes `&self`: this answers "what is nearby", which is not the same question
    /// as "take me there", and prefetching must not move the cursor.
    pub fn neighbours(&self, radius: usize) -> Vec<PathBuf> {
        let Some(current) = self.current else {
            return Vec::new();
        };
        let len = self.entries.len() as isize;

        // Seeded with the current index, so the image already on screen is excluded
        // by the same rule that excludes repeats.
        let mut indices = vec![current];
        for distance in 1..=radius as isize {
            for delta in [distance, -distance] {
                let index = (current as isize + delta).rem_euclid(len) as usize;
                if !indices.contains(&index) {
                    indices.push(index);
                }
            }
        }

        indices[1..]
            .iter()
            .map(|&index| self.entries[index].path.clone())
            .collect()
    }

    /// Step forward, wrapping at the end.
    ///
    /// Named `next_image` rather than `next` so it cannot be mistaken for an
    /// iterator: this advances a cursor that other calls also read, which is not
    /// what `Iterator::next` promises.
    ///
    /// Wrapping because the alternative is a keypress that silently does nothing at
    /// the last photo, which reads as the app having frozen. Returns an owned path
    /// so the caller can start a load without holding a borrow on the folder.
    pub fn next_image(&mut self) -> Option<PathBuf> {
        self.step(1)
    }

    /// Step back, wrapping at the start.
    pub fn prev_image(&mut self) -> Option<PathBuf> {
        self.step(-1)
    }

    fn step(&mut self, delta: isize) -> Option<PathBuf> {
        let len = self.entries.len();
        if len == 0 {
            return None;
        }

        let current = self.current? as isize;
        let next = (current + delta).rem_euclid(len as isize) as usize;
        self.current = Some(next);
        self.entries.get(next).map(|entry| entry.path.clone())
    }

    /// Drop `path` from the listing and return whatever should be shown in its
    /// place — the next image along, or `None` if that was the last one.
    ///
    /// Deleting is the one action that invalidates the listing mid-session, and
    /// re-scanning the directory to find that out would make a run of deletions
    /// quadratic in the size of the folder.
    pub fn remove(&mut self, path: &Path) -> Option<PathBuf> {
        let index = self.entries.iter().position(|entry| entry.path == path)?;
        self.entries.remove(index);

        if self.entries.is_empty() {
            self.current = None;
            return None;
        }

        // Staying at the same index lands on the image that shifted into the gap,
        // which is what "next" means after a delete. Past the end, wrap to the start.
        let next = index % self.entries.len();
        self.current = Some(next);
        self.entries.get(next).map(|entry| entry.path.clone())
    }
}

/// Every supported image directly inside `dir`, in `order`.
///
/// Subdirectories are not descended into: the folder you opened is the set you are
/// looking at, and recursing would silently turn one folder into thousands.
fn list(dir: &Path, order: Order) -> Vec<Entry> {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut entries: Vec<Entry> = read_dir
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            // Checked before anything is asked of the filesystem: most directories
            // hold more files this build cannot open than ones it can.
            if !is_supported(&path) {
                return None;
            }

            let metadata = match entry.metadata() {
                // A symlink's own metadata describes the link — nought bytes, and
                // the moment the link was made. Follow it for the image's own.
                Ok(metadata) if metadata.is_symlink() => std::fs::metadata(&path).ok()?,
                Ok(metadata) => metadata,
                Err(_) => return None,
            };
            if !metadata.is_file() {
                return None;
            }

            Some(Entry {
                sort_name: path
                    .file_name()
                    .map(|name| name.to_string_lossy().to_lowercase())
                    .unwrap_or_default(),
                len: metadata.len(),
                modified: metadata.modified().ok(),
                path,
            })
        })
        .collect();

    sort(&mut entries, order);
    entries
}

/// Put `entries` in `order`, breaking every tie the same way twice.
///
/// A folder full of images off one camera has files of the same size and timestamps
/// to the second; without a tiebreak their order would be whatever `read_dir`
/// happened to return, which is neither stable between runs nor the same on two
/// machines. Falling back to the name — and then to the whole path, which is unique
/// by definition — makes the order total, so stepping forward and back always
/// retraces the same list.
fn sort(entries: &mut [Entry], order: Order) {
    entries.sort_by(|a, b| {
        let by_key = match order.key {
            SortKey::Name => Ordering::Equal,
            SortKey::Modified => a.modified.cmp(&b.modified),
            SortKey::Size => a.len.cmp(&b.len),
        };

        let by_key = if order.descending {
            by_key.reverse()
        } else {
            by_key
        };

        // Case-insensitively, because a listing where `Zebra.jpg` sorts before
        // `apple.jpg` looks broken on Windows, where the shell has never ordered
        // files that way.
        let by_name = a.sort_name.cmp(&b.sort_name);
        let by_name = if order.descending && order.key == SortKey::Name {
            by_name.reverse()
        } else {
            by_name
        };

        by_key.then(by_name).then_with(|| a.path.cmp(&b.path))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory holding empty files with the given names.
    ///
    /// The contents never matter here — listing is extension-based by design, so
    /// zero-byte files exercise exactly the same code path as real photographs.
    fn scratch(name: &str, files: &[&str]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("imaginer-folder-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for file in files {
            std::fs::write(dir.join(file), []).unwrap();
        }
        dir
    }

    /// A directory whose files have the sizes and modification times given, so the
    /// non-name orders have something to actually sort on.
    ///
    /// The timestamps are *set*, not slept for: writing three files a millisecond
    /// apart and hoping the filesystem records three different times is how a test
    /// passes on one machine and fails on another.
    fn scratch_with(name: &str, files: &[(&str, u64, u64)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("imaginer-folder-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        for (file, len, age_secs) in files {
            let handle = std::fs::File::create(dir.join(file)).unwrap();
            handle.set_len(*len).unwrap();
            handle
                .set_modified(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(*age_secs))
                .unwrap();
        }
        dir
    }

    fn names(folder: &Folder) -> Vec<String> {
        folder
            .entries
            .iter()
            .map(|entry| {
                entry
                    .path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    }

    #[test]
    fn lists_only_supported_images_sorted_case_insensitively() {
        let dir = scratch(
            "listing",
            &["Beta.PNG", "alpha.jpg", "notes.txt", "gamma.webp"],
        );
        let folder = Folder::of_directory(&dir, Order::DEFAULT);

        assert_eq!(names(&folder), ["alpha.jpg", "Beta.PNG", "gamma.webp"]);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn name_order_reverses_into_z_first() {
        let dir = scratch("name-desc", &["Beta.PNG", "alpha.jpg", "gamma.webp"]);
        let folder = Folder::of_directory(
            &dir,
            Order {
                key: SortKey::Name,
                descending: true,
            },
        );

        assert_eq!(names(&folder), ["gamma.webp", "Beta.PNG", "alpha.jpg"]);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn size_order_runs_smallest_to_largest_and_back() {
        let dir = scratch_with(
            "size",
            &[
                ("big.png", 3000, 1),
                ("small.png", 10, 2),
                ("mid.png", 900, 3),
            ],
        );

        let up = Folder::of_directory(
            &dir,
            Order {
                key: SortKey::Size,
                descending: false,
            },
        );
        assert_eq!(names(&up), ["small.png", "mid.png", "big.png"]);

        let down = Folder::of_directory(
            &dir,
            Order {
                key: SortKey::Size,
                descending: true,
            },
        );
        assert_eq!(names(&down), ["big.png", "mid.png", "small.png"]);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn date_order_runs_oldest_to_newest_and_back() {
        let dir = scratch_with(
            "date",
            &[
                ("newest.png", 1, 30_000),
                ("oldest.png", 1, 10_000),
                ("middle.png", 1, 20_000),
            ],
        );

        let up = Folder::of_directory(
            &dir,
            Order {
                key: SortKey::Modified,
                descending: false,
            },
        );
        assert_eq!(names(&up), ["oldest.png", "middle.png", "newest.png"]);

        let down = Folder::of_directory(
            &dir,
            Order {
                key: SortKey::Modified,
                descending: true,
            },
        );
        assert_eq!(names(&down), ["newest.png", "middle.png", "oldest.png"]);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn files_that_tie_fall_back_to_the_name_rather_than_to_chance() {
        // Every file the same size and the same age, which is what a folder of
        // camera output looks like to a filesystem with second resolution.
        let dir = scratch_with(
            "ties",
            &[("c.png", 500, 99), ("a.png", 500, 99), ("b.png", 500, 99)],
        );

        for descending in [false, true] {
            for key in [SortKey::Modified, SortKey::Size] {
                let folder = Folder::of_directory(&dir, Order { key, descending });
                // Ascending by name even when the *key* is descending: within a run
                // of equal sizes, A before Z is what anyone reading the list expects.
                assert_eq!(
                    names(&folder),
                    ["a.png", "b.png", "c.png"],
                    "{key:?} descending={descending}"
                );
            }
        }

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn reordering_keeps_the_cursor_on_the_image_being_looked_at() {
        // By name this is a, b, c; by size it is c, b, a. Whichever way round, the
        // image on screen must not change.
        let dir = scratch_with(
            "reorder",
            &[("a.png", 3000, 1), ("b.png", 2000, 2), ("c.png", 100, 3)],
        );
        let mut folder = Folder::containing(&dir.join("a.png"), Order::DEFAULT);
        assert_eq!(folder.position(), Some((1, 3)));

        folder.set_order(Order {
            key: SortKey::Size,
            descending: false,
        });

        assert_eq!(folder.current(), Some(dir.join("a.png").as_path()));
        // Same image, new position: it is now the largest of three.
        assert_eq!(folder.position(), Some((3, 3)));
        // And stepping follows the new order.
        assert_eq!(folder.next_image(), Some(dir.join("c.png")));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn reordering_an_empty_listing_is_harmless() {
        let dir = scratch("reorder-empty", &["readme.txt"]);
        let mut folder = Folder::of_directory(&dir, Order::DEFAULT);

        folder.set_order(Order {
            key: SortKey::Size,
            descending: true,
        });

        assert!(folder.is_empty());
        assert_eq!(folder.current(), None);
        assert_eq!(folder.order().key, SortKey::Size);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn containing_positions_the_cursor_on_the_opened_file() {
        let dir = scratch("cursor", &["a.png", "b.png", "c.png"]);
        let folder = Folder::containing(&dir.join("b.png"), Order::DEFAULT);

        assert_eq!(folder.position(), Some((2, 3)));
        assert_eq!(folder.current(), Some(dir.join("b.png").as_path()));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stepping_wraps_at_both_ends() {
        let dir = scratch("wrap", &["a.png", "b.png"]);
        let mut folder = Folder::containing(&dir.join("a.png"), Order::DEFAULT);

        assert_eq!(folder.next_image(), Some(dir.join("b.png")));
        assert_eq!(folder.next_image(), Some(dir.join("a.png")));
        assert_eq!(folder.prev_image(), Some(dir.join("b.png")));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn removing_lands_on_the_image_that_took_its_place() {
        let dir = scratch("remove", &["a.png", "b.png", "c.png"]);
        let mut folder = Folder::containing(&dir.join("b.png"), Order::DEFAULT);

        assert_eq!(folder.remove(&dir.join("b.png")), Some(dir.join("c.png")));
        assert_eq!(folder.position(), Some((2, 2)));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn removing_the_last_image_leaves_nothing_to_show() {
        let dir = scratch("remove-last", &["only.png"]);
        let mut folder = Folder::containing(&dir.join("only.png"), Order::DEFAULT);

        assert_eq!(folder.remove(&dir.join("only.png")), None);
        assert!(folder.is_empty());
        assert_eq!(folder.current(), None);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn neighbours_come_back_nearest_first_and_forward_before_back() {
        let dir = scratch("neighbours", &["a.png", "b.png", "c.png", "d.png", "e.png"]);
        let folder = Folder::containing(&dir.join("c.png"), Order::DEFAULT);

        assert_eq!(
            folder.neighbours(2),
            [
                dir.join("d.png"),
                dir.join("b.png"),
                dir.join("e.png"),
                dir.join("a.png"),
            ]
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn neighbours_wrap_at_the_ends() {
        let dir = scratch("neighbours-wrap", &["a.png", "b.png", "c.png"]);
        let folder = Folder::containing(&dir.join("c.png"), Order::DEFAULT);

        assert_eq!(folder.neighbours(1), [dir.join("a.png"), dir.join("b.png")]);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn neighbours_never_repeat_an_image_or_name_the_current_one() {
        let dir = scratch("neighbours-small", &["a.png", "b.png"]);
        let folder = Folder::containing(&dir.join("a.png"), Order::DEFAULT);

        // Forward and back are the same file here, and a radius past the end of the
        // folder cannot conjure more images than there are.
        assert_eq!(folder.neighbours(1), [dir.join("b.png")]);
        assert_eq!(folder.neighbours(9), [dir.join("b.png")]);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_lone_image_has_no_neighbours() {
        let dir = scratch("neighbours-alone", &["only.png"]);
        let folder = Folder::containing(&dir.join("only.png"), Order::DEFAULT);

        assert!(folder.neighbours(2).is_empty());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_directory_with_no_images_has_no_cursor() {
        let dir = scratch("empty", &["readme.txt"]);
        let mut folder = Folder::of_directory(&dir, Order::DEFAULT);

        assert!(folder.is_empty());
        assert_eq!(folder.current(), None);
        assert_eq!(folder.next_image(), None);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
