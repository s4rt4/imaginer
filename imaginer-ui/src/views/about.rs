//! What this is, what version of it, and what it can open.
//!
//! A modal rather than a panel: it is read once and dismissed, and it would be
//! the only thing in the right-hand strip that has nothing to do with the image
//! on screen. It is reached from the settings panel, which is where someone
//! already goes to ask the app about itself.
//!
//! The format list is read from [`imaginer_core::SUPPORTED_EXTENSIONS`] rather
//! than written out here. That list is the same one the open dialog filters by
//! and the folder listing walks, so this box cannot claim a format the app
//! would then refuse — which is the only way an About box has ever lied.

use eframe::egui;

use crate::theme;

/// Where the source lives, which is the one thing here a reader might want to
/// act on.
const REPOSITORY: &str = "https://github.com/s4rt4/imaginer";

/// Drawn every frame the box is open; `true` means the reader is done with it.
///
/// `logotype` is the texture the empty state already uploads, passed in rather
/// than loaded here so opening this never costs a second copy of the artwork.
pub fn show(ctx: &egui::Context, logotype: &egui::TextureHandle) -> bool {
    let mut close = false;

    let response = egui::Modal::new(egui::Id::new("about")).show(ctx, |ui| {
        ui.set_width(340.0);

        ui.vertical_centered(|ui| {
            // Half the wordmark's native size, which is what it was rasterised
            // for — a logo drawn larger than it was baked is the first thing an
            // about box gets wrong.
            let size = logotype.size_vec2() * 0.5;
            ui.add(egui::Image::new(logotype).fit_to_exact_size(size));
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new(concat!("Version ", env!("CARGO_PKG_VERSION")))
                    .color(theme::TEXT_MUTED),
            );
        });

        ui.add_space(12.0);
        ui.label("A fast-startup image viewer and light editor for Windows.");
        ui.add_space(10.0);

        ui.label(egui::RichText::new("Opens").strong());
        ui.add_space(2.0);
        ui.label(
            egui::RichText::new(formats())
                .color(theme::TEXT_MUTED)
                .small(),
        );

        ui.add_space(12.0);
        ui.hyperlink_to("Source and issues on GitHub", REPOSITORY);
        ui.add_space(2.0);
        ui.label(
            egui::RichText::new("MIT licensed. UI icons from Lucide, also MIT.")
                .color(theme::TEXT_MUTED)
                .small(),
        );

        ui.add_space(14.0);
        ui.vertical_centered(|ui| {
            if ui.button("Close").clicked() {
                close = true;
            }
        });
    });

    // Clicking the dimmed backdrop, or pressing Escape, closes it — both are
    // what a modal is expected to answer to, and neither should need a button.
    close || response.should_close()
}

/// The supported extensions as one readable line: `avif, bmp, gif, ...`.
///
/// Sorted, because the constant is in the order formats were added and a reader
/// scanning for "does it open X" wants alphabetical.
fn formats() -> String {
    let mut extensions: Vec<&str> = imaginer_core::SUPPORTED_EXTENSIONS.to_vec();
    extensions.sort_unstable();
    extensions.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_supported_format_is_named() {
        let listed = formats();
        for extension in imaginer_core::SUPPORTED_EXTENSIONS {
            assert!(
                listed.contains(extension),
                "{extension} is missing from the about box"
            );
        }
    }

    #[test]
    fn the_list_is_alphabetical() {
        let listed = formats();
        let mut names: Vec<&str> = listed.split(", ").collect();
        let given = names.clone();
        names.sort_unstable();
        assert_eq!(given, names);
    }
}
