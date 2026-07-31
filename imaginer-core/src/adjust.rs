//! Brightness, contrast and saturation.
//!
//! One struct rather than three ops, because they are one control panel: nobody sets
//! contrast without looking at what brightness is doing. Held as whole percentages
//! rather than floats so the whole edit stack stays `Eq` — an op list you can compare
//! is what lets the pipeline notice that nothing actually changed.
//!
//! The order is brightness, then contrast, then saturation, and it is not
//! interchangeable: brightening after a contrast stretch clips differently from
//! brightening before it. Anything that reimplements this — a shader, most likely —
//! has to apply them in the same order to produce the same picture.

use image::RgbaImage;

/// A colour adjustment, in percentages away from "leave it alone".
///
/// Each runs -100 to 100. The negative end of each is a real place to be: -100
/// brightness is black, -100 contrast is flat mid-grey, -100 saturation is greyscale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Adjust {
    pub brightness: i16,
    pub contrast: i16,
    pub saturation: i16,
}

/// The widest setting any of the three takes.
pub const LIMIT: i16 = 100;

/// Rec. 709 luma weights — the same ones every sRGB-era tool uses to decide what
/// "the same brightness, less colour" means.
const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

impl Adjust {
    pub const NONE: Self = Self {
        brightness: 0,
        contrast: 0,
        saturation: 0,
    };

    /// Whether this would leave every pixel exactly as it found it.
    pub fn is_none(self) -> bool {
        self == Self::NONE
    }

    /// Clamp every field into range, for values that came from outside.
    pub fn clamped(self) -> Self {
        Self {
            brightness: self.brightness.clamp(-LIMIT, LIMIT),
            contrast: self.contrast.clamp(-LIMIT, LIMIT),
            saturation: self.saturation.clamp(-LIMIT, LIMIT),
        }
    }

    /// Apply to every pixel. Alpha is never touched.
    ///
    /// Brightness and contrast are per-channel and depend on nothing but the channel
    /// itself, so they collapse into a 256-entry table computed once. Saturation
    /// cannot: it mixes the three channels, so it is the only part that runs per
    /// pixel — and when the saturation slider is at rest it does not run at all,
    /// which is the common case while the other two are being dragged.
    pub fn apply(self, pixels: &mut RgbaImage) {
        if self.is_none() {
            return;
        }
        let adjust = self.clamped();

        let saturation = 1.0 + adjust.saturation as f32 / 100.0;
        if adjust.saturation == 0 {
            let table = adjust.tone_table_u8();
            for pixel in pixels.pixels_mut() {
                pixel.0[0] = table[pixel.0[0] as usize];
                pixel.0[1] = table[pixel.0[1] as usize];
                pixel.0[2] = table[pixel.0[2] as usize];
            }
            return;
        }

        let table = adjust.tone_table();
        for pixel in pixels.pixels_mut() {
            let rgb = [
                table[pixel.0[0] as usize],
                table[pixel.0[1] as usize],
                table[pixel.0[2] as usize],
            ];
            let luma = LUMA[0] * rgb[0] + LUMA[1] * rgb[1] + LUMA[2] * rgb[2];

            for (channel, toned) in pixel.0.iter_mut().zip(rgb) {
                *channel = to_u8(luma + (toned - luma) * saturation);
            }
        }
    }

    /// Brightness and contrast for every possible channel value, as 0..1 floats.
    ///
    /// Left unclamped at the top end on purpose: clamping here and again after
    /// saturation would flatten highlights twice, so the single clamp lives at the
    /// point the value becomes a byte.
    fn tone_table(self) -> [f32; 256] {
        let brightness = self.brightness as f32 / 100.0;
        let contrast = 1.0 + self.contrast as f32 / 100.0;

        std::array::from_fn(|value| {
            let channel = value as f32 / 255.0 + brightness;
            // About mid grey, so raising contrast opens the image up from the middle
            // rather than sliding the whole thing towards white.
            (channel - 0.5) * contrast + 0.5
        })
    }

    /// The same table, already rounded to bytes, for when saturation is at rest.
    fn tone_table_u8(self) -> [u8; 256] {
        self.tone_table().map(to_u8)
    }
}

fn to_u8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(r: u8, g: u8, b: u8, a: u8) -> RgbaImage {
        RgbaImage::from_pixel(1, 1, image::Rgba([r, g, b, a]))
    }

    fn adjusted(adjust: Adjust, r: u8, g: u8, b: u8) -> [u8; 4] {
        let mut image = pixel(r, g, b, 128);
        adjust.apply(&mut image);
        image.get_pixel(0, 0).0
    }

    /// How long an adjustment takes on a photograph-sized image.
    ///
    /// Not an assertion — a number, printed on demand, because it is the input to a
    /// decision rather than a thing that can pass or fail. The plan says colour
    /// adjustment previews through a fragment shader on the grounds that the CPU
    /// cannot keep up with a dragging slider; whether that is true here is a
    /// measurement, and this is it. Run with:
    ///
    /// `cargo test --release -p imaginer-core -- --ignored --nocapture adjust_costs`
    ///
    /// Release only. A debug build measures the optimiser's absence, not the work.
    #[test]
    #[ignore = "a measurement, not an assertion"]
    fn adjust_costs() {
        use std::time::Instant;

        // 24MP, the size of a current phone or mid-range camera photograph.
        let base = RgbaImage::from_fn(6000, 4000, |x, y| {
            image::Rgba([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8, 255])
        });

        let cases = [
            (
                "brightness + contrast (table path)",
                Adjust {
                    brightness: 20,
                    contrast: 15,
                    saturation: 0,
                },
            ),
            (
                "all three (per-pixel path)",
                Adjust {
                    brightness: 20,
                    contrast: 15,
                    saturation: -30,
                },
            ),
        ];

        for (label, adjust) in cases {
            let mut pixels = base.clone();
            let started = Instant::now();
            adjust.apply(&mut pixels);
            let adjust_ms = started.elapsed().as_secs_f64() * 1000.0;

            // The preview path does not adjust in place: it copies the original
            // first, because every edit re-runs from untouched pixels. That copy is
            // part of what a slider drag costs, so it is part of the measurement.
            let started = Instant::now();
            let mut copy = base.clone();
            adjust.apply(&mut copy);
            let with_copy_ms = started.elapsed().as_secs_f64() * 1000.0;

            println!("{label}: {adjust_ms:.1}ms, {with_copy_ms:.1}ms including the copy");
        }
    }

    #[test]
    fn no_adjustment_changes_nothing() {
        let before = pixel(10, 120, 250, 77);
        let mut after = before.clone();
        Adjust::NONE.apply(&mut after);
        assert_eq!(after, before);
    }

    #[test]
    fn alpha_is_never_touched() {
        let out = adjusted(
            Adjust {
                brightness: 50,
                contrast: 40,
                saturation: -80,
            },
            10,
            120,
            250,
        );
        assert_eq!(out[3], 128);
    }

    #[test]
    fn full_brightness_reaches_white_and_full_darkness_reaches_black() {
        let up = Adjust {
            brightness: LIMIT,
            ..Adjust::NONE
        };
        assert_eq!(adjusted(up, 10, 120, 250)[..3], [255, 255, 255]);

        let down = Adjust {
            brightness: -LIMIT,
            ..Adjust::NONE
        };
        assert_eq!(adjusted(down, 10, 120, 250)[..3], [0, 0, 0]);
    }

    #[test]
    fn contrast_pivots_about_mid_grey() {
        let up = Adjust {
            contrast: 50,
            ..Adjust::NONE
        };
        // 128/255 is a hair above 0.5, so mid grey is the one value that barely
        // moves however hard contrast is pushed.
        let middle = adjusted(up, 128, 128, 128);
        assert!(
            middle[0].abs_diff(128) <= 1,
            "mid grey moved to {}",
            middle[0]
        );

        // Either side of it separates.
        assert!(adjusted(up, 200, 200, 200)[0] > 200);
        assert!(adjusted(up, 60, 60, 60)[0] < 60);
    }

    #[test]
    fn flattening_contrast_takes_everything_to_mid_grey() {
        let flat = Adjust {
            contrast: -LIMIT,
            ..Adjust::NONE
        };
        assert_eq!(adjusted(flat, 0, 128, 255)[..3], [128, 128, 128]);
    }

    #[test]
    fn removing_all_saturation_leaves_luma() {
        let grey = Adjust {
            saturation: -LIMIT,
            ..Adjust::NONE
        };
        let out = adjusted(grey, 255, 0, 0);

        // Every channel equal, and equal to the red weight — a greyscale conversion
        // that merely averaged the channels would give 85 here instead.
        assert_eq!(out[0], out[1]);
        assert_eq!(out[1], out[2]);
        assert_eq!(out[0], (LUMA[0] * 255.0).round() as u8);
    }

    #[test]
    fn saturation_leaves_a_grey_pixel_grey() {
        // Nothing to pull away from the luma it already is, in either direction.
        let up = Adjust {
            saturation: LIMIT,
            ..Adjust::NONE
        };
        assert_eq!(adjusted(up, 90, 90, 90)[..3], [90, 90, 90]);
    }

    #[test]
    fn values_from_outside_the_dial_are_clamped_rather_than_trusted() {
        let wild = Adjust {
            brightness: 9000,
            contrast: -9000,
            saturation: 9000,
        };
        assert_eq!(
            wild.clamped(),
            Adjust {
                brightness: LIMIT,
                contrast: -LIMIT,
                saturation: LIMIT,
            }
        );
        // And applying an out-of-range value cannot produce a wrapped byte.
        let out = adjusted(wild, 10, 120, 250);
        assert_eq!(out[..3], [128, 128, 128]);
    }

    #[test]
    fn the_saturation_shortcut_agrees_with_the_general_path() {
        // The u8 table is an optimisation for saturation == 0. If it ever disagreed
        // with the per-pixel path, dragging the saturation slider back to zero would
        // visibly change the image.
        let adjust = Adjust {
            brightness: 17,
            contrast: -23,
            saturation: 0,
        };

        let mut shortcut = RgbaImage::from_fn(16, 16, |x, y| {
            image::Rgba([(x * 16) as u8, (y * 16) as u8, 200, 255])
        });
        let mut general = shortcut.clone();
        adjust.apply(&mut shortcut);

        // The same maths, taken through the float path a saturation of 1.0 would use.
        let table = adjust.tone_table();
        for pixel in general.pixels_mut() {
            for channel in 0..3 {
                let toned = table[pixel.0[channel] as usize];
                let luma = toned; // saturation 1.0 leaves each channel where it is
                pixel.0[channel] = to_u8(luma);
            }
        }

        assert_eq!(shortcut, general);
    }
}
