//! Bottom bar: filename, dimensions, zoom, file size.

use std::path::Path;

use eframe::egui;
use imaginer_core::{Order, SortKey, Stage};

use crate::texture::ImageTexture;
use crate::theme;

pub struct Status<'a> {
    pub path: Option<&'a Path>,
    pub texture: Option<&'a ImageTexture>,
    pub stage: Option<Stage>,
    pub zoom: f32,
    pub file_size: Option<u64>,
    /// Where this image sits in its folder, 1-based, as `(position, total)`.
    pub position: Option<(usize, usize)>,
    /// What that position is counting. Shown only when it is not the default.
    pub order: Order,
    pub error: Option<&'a str>,
    /// Short-lived confirmation of an action, shown at the far end of the bar.
    pub notice: Option<&'a str>,
}

pub fn show(ui: &mut egui::Ui, status: &Status<'_>) {
    ui.horizontal(|ui| {
        facts(ui, status);

        // Right-aligned, so it never shifts the facts about the image sideways as
        // it comes and goes. This has to come last: the layout claims whatever
        // width is left over.
        if let Some(notice) = status.notice {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.colored_label(theme::ACCENT_COLOR, notice);
            });
        }
    });
}

fn facts(ui: &mut egui::Ui, status: &Status<'_>) {
    ui.horizontal(|ui| {
        if let Some(error) = status.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
            return;
        }

        let name = status
            .path
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            // No path and an image anyway means pixels from the clipboard. Saying
            // "No image" over a photograph would be a plain lie, and the thing worth
            // saying is the thing that is different about it: nothing on disk holds
            // this yet, so Save will ask where to put it.
            .unwrap_or_else(|| match status.texture {
                Some(_) => "Unsaved image".to_owned(),
                None => "No image".to_owned(),
            });
        ui.label(name);

        // Directly after the name, because it answers a question about the name:
        // which of these am I looking at, and how many are left.
        if let Some((at, total)) = status.position {
            ui.colored_label(theme::TEXT_MUTED, format!("{at} / {total}"));

            // Only when the order is not the plain A-to-Z the app starts in. A
            // status bar that restates the default is one people stop reading — and
            // "3 / 128" means something different under every other order, so that
            // is exactly when it is worth the pixels.
            if !status.order.is_default() {
                ui.colored_label(theme::TEXT_MUTED, order_summary(status.order));
            }
        }

        let Some(texture) = status.texture else {
            return;
        };

        separator(ui);
        let (w, h) = texture.source_size;
        ui.label(format!("{w} × {h}"));

        separator(ui);
        ui.label(format!("{:.0}%", status.zoom * 100.0));

        if let Some(bytes) = status.file_size {
            separator(ui);
            ui.label(human_size(bytes));
        }

        // Tell the user when they are not looking at the real pixels yet. Without
        // this, the low-res preview just looks like a broken decode.
        if status.stage == Some(Stage::Preview) {
            separator(ui);
            ui.colored_label(theme::ACCENT_COLOR, "preview…");
        }

        if texture.downscaled {
            separator(ui);
            ui.colored_label(theme::TEXT_MUTED, "downscaled to fit GPU limit")
                .on_hover_text(format!(
                    "Image is larger than this GPU's maximum texture size; \
                     displaying at {} × {}",
                    texture.uploaded_size.0, texture.uploaded_size.1
                ));
        }
    });
}

fn separator(ui: &mut egui::Ui) {
    ui.colored_label(theme::TEXT_MUTED, "·");
}

/// The order in words.
///
/// Words rather than an arrow beside the key: an arrow needs to be read as up or
/// down and then translated into older or newer, and half the time it is translated
/// wrong. It also needs a glyph, and this build loads exactly one system font.
fn order_summary(order: Order) -> &'static str {
    match (order.key, order.descending) {
        (SortKey::Name, false) => "by name",
        (SortKey::Name, true) => "by name, Z first",
        (SortKey::Modified, false) => "by date, oldest first",
        (SortKey::Modified, true) => "by date, newest first",
        (SortKey::Size, false) => "by size, smallest first",
        (SortKey::Size, true) => "by size, largest first",
    }
}

fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let bytes = bytes as f64;
    if bytes < KB {
        format!("{bytes:.0} B")
    } else if bytes < KB * KB {
        format!("{:.1} KB", bytes / KB)
    } else if bytes < KB * KB * KB {
        format!("{:.1} MB", bytes / (KB * KB))
    } else {
        format!("{:.2} GB", bytes / (KB * KB * KB))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_sizes_at_each_scale() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2.0 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(human_size(3 * 1024 * 1024 * 1024), "3.00 GB");
    }
}
