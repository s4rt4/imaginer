//! Rasterising SVG.
//!
//! The one format here with no natural pixel size. Everything else arrives as a
//! grid of samples and the only question is how to decode it; an SVG is a
//! description, and something has to decide how large to draw it.
//!
//! This is also what the logo and icon pipeline in `imaginer-ui/build.rs` calls, so
//! the artwork baked into the executable comes off the same code path a user's own
//! `.svg` does — the same reason the embedded `.ico` is written by this crate's own
//! encoder rather than a second one kept in step by hand.

use std::sync::{Arc, OnceLock};

use image::RgbaImage;
use resvg::{tiny_skia, usvg};

/// Smallest longest-side this will rasterise at.
///
/// A 16px icon drawn at 16px is unreadable on a screen that can show it at any size,
/// and the whole point of vector artwork is that there is no reason to.
pub const MIN_SIDE: u32 = 1024;

/// Largest longest-side this will rasterise at.
///
/// 4096 square is 64MB of RGBA, which is one large photograph. Artboards run to
/// tens of thousands of units and a literal reading of one would be gigabytes.
pub const MAX_SIDE: u32 = 4096;

#[derive(Debug, thiserror::Error)]
pub enum SvgError {
    #[error("not valid SVG: {0}")]
    Parse(#[from] usvg::Error),
    #[error("cannot rasterise at {width}x{height}")]
    Size { width: u32, height: u32 },
}

/// The system fonts, loaded at most once per process.
///
/// Enumerating every font on the machine takes long enough to be worth avoiding,
/// and a viewer that never opens an SVG should never pay for it — which is why this
/// is a `OnceLock` reached from the decode path rather than anything set up at
/// startup. The first SVG of a session pays; nothing else ever does.
///
/// Loaded unconditionally rather than only when the file contains text, because
/// that cannot be known until it has been parsed, and parsing is what needs them.
fn system_fonts() -> &'static Arc<usvg::fontdb::Database> {
    static FONTS: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    FONTS.get_or_init(|| {
        let mut db = usvg::fontdb::Database::new();
        db.load_system_fonts();
        Arc::new(db)
    })
}

/// A parsed SVG, ready to be drawn at whatever size the caller wants.
pub struct Svg {
    tree: usvg::Tree,
}

impl Svg {
    /// Parse SVG data. Gzipped `.svgz` is handled too — `usvg` looks for the gzip
    /// magic itself, so there is nothing to detect here.
    pub fn parse(data: &[u8]) -> Result<Self, SvgError> {
        let options = usvg::Options {
            fontdb: Arc::clone(system_fonts()),
            ..Default::default()
        };
        Ok(Self {
            tree: usvg::Tree::from_data(data, &options)?,
        })
    }

    /// The size the file asks to be drawn at, in SVG units.
    pub fn size(&self) -> (f32, f32) {
        let size = self.tree.size();
        (size.width(), size.height())
    }

    /// Rasterise at a size that suits looking at it.
    ///
    /// The file's own `width` and `height` are honoured when they say something
    /// reasonable, and both extremes are refused — see [`MIN_SIDE`] and
    /// [`MAX_SIDE`]. This is a judgement rather than a fact about the file, which
    /// is why it lives here and not in the parser: a viewer wants one answer and
    /// the executable's icon pipeline wants a different one.
    pub fn render_fit(&self) -> Result<RgbaImage, SvgError> {
        let (width, height) = self.size();
        let longest = width.max(height).max(1.0);
        let target = longest.clamp(MIN_SIDE as f32, MAX_SIDE as f32);
        self.rasterise(target / longest)
    }

    /// Rasterise so the longest side is exactly `target` pixels.
    ///
    /// The zoom-in path calls this: the first render is a guess at a viewing
    /// size, and when the user magnifies past it the viewer asks for exactly as
    /// many pixels as the screen will show, clamping to [`MAX_SIDE`] itself.
    pub fn render_longest(&self, target: u32) -> Result<RgbaImage, SvgError> {
        let (width, height) = self.size();
        let longest = width.max(height).max(1.0);
        self.rasterise(target as f32 / longest)
    }

    /// Rasterise to an exact pixel width, with the height following the aspect ratio.
    ///
    /// What the build script wants: an icon has to come out exactly the size the
    /// packing code was told to expect, not a pixel either side of it.
    pub fn render_width(&self, width: u32) -> Result<RgbaImage, SvgError> {
        let (natural_width, natural_height) = self.size();
        let scale = width as f32 / natural_width.max(1.0);
        let height = ((natural_height * scale).round() as u32).max(1);
        self.draw(width, height, scale)
    }

    fn rasterise(&self, scale: f32) -> Result<RgbaImage, SvgError> {
        let (width, height) = self.size();
        self.draw(
            ((width * scale).round() as u32).max(1),
            ((height * scale).round() as u32).max(1),
            scale,
        )
    }

    /// Rasterise one rectangle of the artwork, in SVG units, at an exact scale.
    ///
    /// The deep-zoom path calls this. Past a certain magnification a whole-artwork
    /// raster stops being possible — [`MAX_SIDE`] is there because the alternative
    /// is gigabytes — but the *screen* never grows, so what a viewer actually needs
    /// is the part it is showing, drawn at the resolution it is showing it. That is
    /// this: the pixmap stays the size of a window however far in the user goes, and
    /// the vector is re-drawn rather than magnified, which is the whole difference
    /// between this and a PNG.
    ///
    /// `x` and `y` are the top-left of the wanted region in SVG units; `width` and
    /// `height` are the output in pixels. Asking for a region that runs off the
    /// artwork is allowed and comes back transparent there, which saves every caller
    /// from clamping a rectangle it is about to clamp anyway.
    pub fn render_region(
        &self,
        scale: f32,
        x: f32,
        y: f32,
        width: u32,
        height: u32,
    ) -> Result<RgbaImage, SvgError> {
        let transform =
            tiny_skia::Transform::from_scale(scale, scale).post_translate(-x * scale, -y * scale);
        self.draw_with(width, height, transform)
    }

    fn draw(&self, width: u32, height: u32, scale: f32) -> Result<RgbaImage, SvgError> {
        // One scale for both axes: the pixel dimensions are rounded, so deriving a
        // separate y scale from them would stretch the artwork by up to half a pixel.
        self.draw_with(width, height, tiny_skia::Transform::from_scale(scale, scale))
    }

    fn draw_with(
        &self,
        width: u32,
        height: u32,
        transform: tiny_skia::Transform,
    ) -> Result<RgbaImage, SvgError> {
        let mut pixmap =
            tiny_skia::Pixmap::new(width, height).ok_or(SvgError::Size { width, height })?;

        resvg::render(&self.tree, transform, &mut pixmap.as_mut());

        // tiny-skia composites in premultiplied alpha and `RgbaImage` is straight,
        // so this is not a copy that could be skipped.
        let mut rgba = Vec::with_capacity(pixmap.pixels().len() * 4);
        for pixel in pixmap.pixels() {
            let straight = pixel.demultiply();
            rgba.extend_from_slice(&[
                straight.red(),
                straight.green(),
                straight.blue(),
                straight.alpha(),
            ]);
        }

        Ok(RgbaImage::from_raw(width, height, rgba)
            .expect("the buffer was built from these dimensions"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A red square filling a `size`-unit viewBox.
    fn square(size: u32) -> String {
        format!(
            concat!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" "#,
                r#"width="{size}" height="{size}" viewBox="0 0 {size} {size}">"#,
                r#"<rect width="{size}" height="{size}" fill="red"/></svg>"#
            ),
            size = size
        )
    }

    #[test]
    fn a_tiny_icon_is_drawn_large_enough_to_look_at() {
        // 16 units, which as 16 pixels would be a smudge in the middle of a window.
        let svg = Svg::parse(square(16).as_bytes()).unwrap();
        assert_eq!(svg.size(), (16.0, 16.0));

        let rendered = svg.render_fit().unwrap();
        assert_eq!(rendered.dimensions(), (MIN_SIDE, MIN_SIDE));
        assert_eq!(rendered.get_pixel(500, 500).0, [255, 0, 0, 255]);
    }

    #[test]
    fn an_enormous_artboard_is_capped_rather_than_believed() {
        let svg = Svg::parse(square(20_000).as_bytes()).unwrap();
        let rendered = svg.render_fit().unwrap();

        // 20000 square would be 1.6GB of pixels.
        assert_eq!(rendered.dimensions(), (MAX_SIDE, MAX_SIDE));
    }

    #[test]
    fn a_reasonable_size_is_taken_at_its_word() {
        let svg = Svg::parse(square(2000).as_bytes()).unwrap();
        assert_eq!(svg.render_fit().unwrap().dimensions(), (2000, 2000));
    }

    #[test]
    fn an_exact_width_comes_out_exactly_that_wide() {
        // The build script packs icon masks at a fixed stride, so one pixel either
        // side would corrupt every icon after it.
        let wide = r#"<svg xmlns="http://www.w3.org/2000/svg" width="30" height="10"
                      viewBox="0 0 30 10"><rect width="30" height="10" fill="blue"/></svg>"#;
        let svg = Svg::parse(wide.as_bytes()).unwrap();

        let rendered = svg.render_width(48).unwrap();
        assert_eq!(rendered.dimensions(), (48, 16));
    }

    #[test]
    fn transparency_survives_as_straight_alpha() {
        // A half-transparent red. Premultiplied it would be stored as (128, 0, 0),
        // and handing that to the GPU as straight alpha would draw it too dark.
        let translucent = r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"
                             viewBox="0 0 10 10"><rect width="10" height="10"
                             fill="red" fill-opacity="0.5"/></svg>"#;
        let rendered = Svg::parse(translucent.as_bytes())
            .unwrap()
            .render_width(4)
            .unwrap();

        let pixel = rendered.get_pixel(2, 2).0;
        assert_eq!(pixel[0], 255, "red should still be full strength");
        assert!(
            pixel[3].abs_diff(128) <= 1,
            "alpha should be about half, got {}",
            pixel[3]
        );
    }

    /// Left half red, right half blue, in a 100x100 viewBox.
    fn halves() -> &'static str {
        concat!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100" "#,
            r#"viewBox="0 0 100 100">"#,
            r#"<rect width="50" height="100" fill="red"/>"#,
            r#"<rect x="50" width="50" height="100" fill="blue"/></svg>"#
        )
    }

    #[test]
    fn a_region_draws_only_that_part_of_the_artwork() {
        let svg = Svg::parse(halves().as_bytes()).unwrap();

        // The right half, at four pixels per unit: nothing but blue.
        let right = svg.render_region(4.0, 50.0, 0.0, 200, 400).unwrap();
        assert_eq!(right.dimensions(), (200, 400));
        assert_eq!(right.get_pixel(0, 0).0, [0, 0, 255, 255]);
        assert_eq!(right.get_pixel(199, 399).0, [0, 0, 255, 255]);

        // And the left half is the other colour, at the same scale.
        let left = svg.render_region(4.0, 0.0, 0.0, 200, 400).unwrap();
        assert_eq!(left.get_pixel(199, 0).0, [255, 0, 0, 255]);
    }

    #[test]
    fn a_region_is_the_same_picture_as_the_whole_one_at_that_scale() {
        // The point of the deep-zoom path: what it draws has to line up with what
        // the full raster would have drawn, or panning would shift the image.
        let svg = Svg::parse(halves().as_bytes()).unwrap();
        let whole = svg.render_longest(200).unwrap();
        let patch = svg.render_region(2.0, 25.0, 25.0, 50, 50).unwrap();

        for y in 0..50 {
            for x in 0..50 {
                assert_eq!(
                    patch.get_pixel(x, y).0,
                    whole.get_pixel(x + 50, y + 50).0,
                    "patch pixel {x},{y} should match the whole render"
                );
            }
        }
    }

    #[test]
    fn a_region_past_the_edge_comes_back_transparent_rather_than_failing() {
        let svg = Svg::parse(halves().as_bytes()).unwrap();
        let over = svg.render_region(1.0, 90.0, 0.0, 20, 20).unwrap();

        assert_eq!(over.get_pixel(0, 0).0[3], 255, "inside is still drawn");
        assert_eq!(over.get_pixel(19, 0).0[3], 0, "past the edge is empty");
    }

    #[test]
    fn something_that_is_not_svg_is_an_error_rather_than_a_blank_image() {
        assert!(Svg::parse(b"certainly not svg").is_err());
    }
}
