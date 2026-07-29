//! The images sitting alongside the one on screen.
//!
//! Stepping to the next photo is the single most repeated action in a viewer, so
//! the list it steps through is built once when a file is opened rather than
//! re-scanned per keypress. It is also what slideshow and prefetch will walk, which
//! is why it lives in core rather than next to the UI that happens to use it first.

use std::path::{Path, PathBuf};

use crate::decode::is_supported;

/// The supported images in one directory, in display order, with a cursor.
#[derive(Debug, Default)]
pub struct Folder {
    entries: Vec<PathBuf>,
    /// Index into `entries`. `None` when the directory holds no images at all, or
    /// when the file that was opened is not in it — a path typed on the command
    /// line need not live anywhere in particular.
    current: Option<usize>,
}

impl Folder {
    /// List the directory `path` lives in, positioned at `path` itself.
    pub fn containing(path: &Path) -> Self {
        let Some(dir) = path.parent() else {
            return Self::default();
        };

        let entries = list(dir);
        let current = entries.iter().position(|entry| entry == path);
        Self { entries, current }
    }

    /// List `dir`, positioned at its first image.
    pub fn of_directory(dir: &Path) -> Self {
        let entries = list(dir);
        let current = (!entries.is_empty()).then_some(0);
        Self { entries, current }
    }

    pub fn current(&self) -> Option<&Path> {
        Some(self.entries.get(self.current?)?.as_path())
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
        self.entries.get(next).cloned()
    }

    /// Drop `path` from the listing and return whatever should be shown in its
    /// place — the next image along, or `None` if that was the last one.
    ///
    /// Deleting is the one action that invalidates the listing mid-session, and
    /// re-scanning the directory to find that out would make a run of deletions
    /// quadratic in the size of the folder.
    pub fn remove(&mut self, path: &Path) -> Option<PathBuf> {
        let index = self.entries.iter().position(|entry| entry == path)?;
        self.entries.remove(index);

        if self.entries.is_empty() {
            self.current = None;
            return None;
        }

        // Staying at the same index lands on the image that shifted into the gap,
        // which is what "next" means after a delete. Past the end, wrap to the start.
        let next = index % self.entries.len();
        self.current = Some(next);
        self.entries.get(next).cloned()
    }
}

/// Every supported image directly inside `dir`, sorted by name.
///
/// Case-insensitively, because a listing where `Zebra.jpg` sorts before `apple.jpg`
/// looks broken on Windows, where the shell has never ordered files that way.
/// Subdirectories are not descended into: the folder you opened is the set you are
/// looking at, and recursing would silently turn one folder into thousands.
fn list(dir: &Path) -> Vec<PathBuf> {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut entries: Vec<PathBuf> = read_dir
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && is_supported(path))
        .collect();

    entries.sort_by_key(|path| {
        path.file_name()
            .map(|name| name.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    });
    entries
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

    #[test]
    fn lists_only_supported_images_sorted_case_insensitively() {
        let dir = scratch(
            "listing",
            &["Beta.PNG", "alpha.jpg", "notes.txt", "gamma.webp"],
        );
        let folder = Folder::of_directory(&dir);

        let names: Vec<String> = (0..folder.len())
            .map(|i| {
                folder.entries[i]
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(names, ["alpha.jpg", "Beta.PNG", "gamma.webp"]);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn containing_positions_the_cursor_on_the_opened_file() {
        let dir = scratch("cursor", &["a.png", "b.png", "c.png"]);
        let folder = Folder::containing(&dir.join("b.png"));

        assert_eq!(folder.position(), Some((2, 3)));
        assert_eq!(folder.current(), Some(dir.join("b.png").as_path()));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stepping_wraps_at_both_ends() {
        let dir = scratch("wrap", &["a.png", "b.png"]);
        let mut folder = Folder::containing(&dir.join("a.png"));

        assert_eq!(folder.next_image(), Some(dir.join("b.png")));
        assert_eq!(folder.next_image(), Some(dir.join("a.png")));
        assert_eq!(folder.prev_image(), Some(dir.join("b.png")));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn removing_lands_on_the_image_that_took_its_place() {
        let dir = scratch("remove", &["a.png", "b.png", "c.png"]);
        let mut folder = Folder::containing(&dir.join("b.png"));

        assert_eq!(folder.remove(&dir.join("b.png")), Some(dir.join("c.png")));
        assert_eq!(folder.position(), Some((2, 2)));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn removing_the_last_image_leaves_nothing_to_show() {
        let dir = scratch("remove-last", &["only.png"]);
        let mut folder = Folder::containing(&dir.join("only.png"));

        assert_eq!(folder.remove(&dir.join("only.png")), None);
        assert!(folder.is_empty());
        assert_eq!(folder.current(), None);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_directory_with_no_images_has_no_cursor() {
        let dir = scratch("empty", &["readme.txt"]);
        let mut folder = Folder::of_directory(&dir);

        assert!(folder.is_empty());
        assert_eq!(folder.current(), None);
        assert_eq!(folder.next_image(), None);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
