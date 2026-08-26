//! Settings that outlive a session, in a small hand-written file.
//!
//! Until now every choice the app remembered died with the window — the sort
//! order deliberately so, because there was nowhere to write it down. There is
//! now: `%APPDATA%\imaginer\settings.txt`, a few `key = value` lines parsed by
//! hand. The format earns its keep by being boring — a dependency for nine
//! bytes of config would be the wrong trade, and a corrupt or half-written file
//! falls back to the defaults one key at a time rather than failing to start.

use std::path::{Path, PathBuf};

use crate::folder::{Order, SortKey};

/// The settings, with one default each. A missing, corrupt or partially
/// readable file yields these — per key, not all-or-nothing, so a typo in one
/// line does not throw away the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    /// How long a slideshow holds each image, in seconds.
    pub slideshow_secs: u32,
    /// What a newly opened folder is sorted by.
    pub order: Order,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            slideshow_secs: 4,
            order: Order::DEFAULT,
        }
    }
}

impl Settings {
    /// Read the settings from disk, or the defaults if there is nothing to read.
    pub fn load() -> Self {
        load()
    }

    /// Write these settings to disk. See [`save`] for the failure policy.
    pub fn save(&self) {
        save(self);
    }
}

/// Where the settings file lives: `%APPDATA%\imaginer`.
///
/// Roaming rather than local on purpose — it is a handful of bytes of pure
/// preference, exactly what roaming profiles exist to carry.
fn default_dir() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(appdata).join("imaginer"))
}

/// Read the settings from disk, or the defaults if there is nothing to read.
pub fn load() -> Settings {
    default_dir()
        .map(|dir| load_from(&dir.join("settings.txt")))
        .unwrap_or_default()
}

/// Write the settings to disk.
///
/// Called on change, which is rare and user-paced — a few bytes written when a
/// slider is let go is not a hot path. Best-effort like every other write this
/// app does to its own data directory: failing to remember a preference is a
/// worse outcome than crashing over one, and much rarer.
pub fn save(settings: &Settings) {
    if let Some(dir) = default_dir() {
        let _ = std::fs::create_dir_all(&dir);
        save_in(&dir.join("settings.txt"), settings);
    }
}

pub fn load_from(path: &Path) -> Settings {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Settings::default();
    };
    parse(&text)
}

pub fn save_in(path: &Path, settings: &Settings) {
    let text = render(settings);
    let _ = std::fs::write(path, text);
}

fn render(settings: &Settings) -> String {
    format!(
        "# imaginer settings — hand-edited at your own risk\n\
         slideshow_secs = {}\n\
         order = {}\n",
        settings.slideshow_secs,
        order_key(settings.order),
    )
}

/// `name-asc`, `date-desc`, … — the two independent choices spelled out rather
/// than numbered, so a hand-edited file stays readable.
fn order_key(order: Order) -> String {
    let key = match order.key {
        SortKey::Name => "name",
        SortKey::Modified => "date",
        SortKey::Size => "size",
    };
    format!("{key}-{}", if order.descending { "desc" } else { "asc" })
}

fn parse_order(value: &str) -> Option<Order> {
    let (key, direction) = value.split_once('-')?;
    let key = match key {
        "name" => SortKey::Name,
        "date" => SortKey::Modified,
        "size" => SortKey::Size,
        _ => return None,
    };
    Some(Order {
        key,
        descending: match direction {
            "asc" => false,
            "desc" => true,
            _ => return None,
        },
    })
}

fn parse(text: &str) -> Settings {
    let mut settings = Settings::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "slideshow_secs" => {
                if let Ok(secs) = value.trim().parse::<u32>() {
                    settings.slideshow_secs = secs.clamp(1, 120);
                }
            }
            "order" => {
                if let Some(order) = parse_order(value.trim()) {
                    settings.order = order;
                }
            }
            // A key from a newer or older build: ignore, keep the default.
            _ => {}
        }
    }
    settings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("imaginer-settings-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("settings.txt")
    }

    #[test]
    fn a_round_trip_preserves_every_setting() {
        let path = scratch("roundtrip");
        let settings = Settings {
            slideshow_secs: 9,
            order: Order {
                key: SortKey::Modified,
                descending: true,
            },
        };

        save_in(&path, &settings);
        assert_eq!(load_from(&path), settings);

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_missing_file_is_the_defaults() {
        let path = scratch("missing");
        assert_eq!(load_from(&path), Settings::default());
    }

    #[test]
    fn a_corrupt_file_falls_back_per_key() {
        let path = scratch("corrupt");
        std::fs::write(
            &path,
            "slideshow_secs = banana\norder = size-desc\nnonsense line\n",
        )
        .unwrap();

        let settings = load_from(&path);
        assert_eq!(settings.slideshow_secs, 4, "bad value keeps the default");
        assert_eq!(
            settings.order,
            Order {
                key: SortKey::Size,
                descending: true
            },
            "a good key survives a bad sibling"
        );

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn absurd_slideshow_lengths_are_clamped_into_sense() {
        let path = scratch("clamp");
        std::fs::write(&path, "slideshow_secs = 0\n").unwrap();
        assert_eq!(load_from(&path).slideshow_secs, 1);

        std::fs::write(&path, "slideshow_secs = 99999\n").unwrap();
        assert_eq!(load_from(&path).slideshow_secs, 120);

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn every_order_spells_and_parses_back() {
        for key in SortKey::ALL {
            for descending in [false, true] {
                let order = Order { key, descending };
                assert_eq!(parse_order(&order_key(order)), Some(order));
            }
        }
    }
}
