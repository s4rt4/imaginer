//! Keeping vector artwork sharp, however far in the user goes.
//!
//! An SVG arrives on screen as a raster like everything else — the canvas draws
//! pixels — so the question is only ever *which* pixels, and the answer changes
//! every time the zoom does. There are two answers here, and which one is in play
//! is decided by one number: how many screen pixels the artwork's longest side
//! covers.
//!
//! Below [`svg::MAX_SIDE`], re-render the whole thing bigger. Simple, and it has
//! the property that panning is free afterwards — the sharp pixels for anywhere
//! the user might scroll to already exist.
//!
//! Past it, that stops being possible: 32x into a 4096-unit artboard is 130,000
//! pixels a side, which is not a texture, it is a hard drive. But the *screen*
//! never grows. So the second answer is to draw only the part being looked at, at
//! exactly the resolution it is being looked at — a patch a window wide, re-drawn
//! from the vector each time the view moves far enough to matter. That is what
//! Illustrator and Inkscape do, and it is why zooming in them never runs out of
//! detail: there is no stored image to run out of.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

use eframe::egui;
use imaginer_core::image::RgbaImage;
use imaginer_core::svg::{self, Svg};

use crate::texture::{self, ImageTexture};

/// How much slack the SVG raster on screen must lose before a sharper one is
/// asked for. Re-rendering at every wheel notch would burn CPU for half the
/// zooms nobody squints at; 50% headroom means each render serves a real span.
const SHARPEN_FACTOR: f32 = 1.5;

/// How far past what the screen shows a fresh whole-artwork raster aims, so
/// ordinary zoom adjustments inside the same view never trigger a second render.
const OVERRENDER: f32 = 1.25;

/// How far past the visible rectangle a patch is drawn, as a fraction of it.
///
/// Pure margin for panning: a drag that stays inside it costs nothing, and only
/// one that leaves it asks for a new patch. Too small and every nudge re-renders;
/// too large and the patch is mostly pixels nobody will look at, which is time
/// spent before the sharp ones appear.
const PATCH_MARGIN: f32 = 0.35;

/// Largest patch this will rasterise, per side.
///
/// A patch is meant to be about the size of a window, and this is the ceiling on
/// how far the margin above may push that. It also keeps the upload inside what
/// any GPU will take.
const PATCH_CAP: f32 = 4096.0;

/// Where a patch of re-drawn artwork sits, and how finely it was drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Patch {
    /// The part of the image it covers, in source pixels — the same coordinates
    /// the canvas lays the whole image out in, so drawing it is a scale away.
    pub region: egui::Rect,
    /// Screen pixels per source pixel it was drawn at.
    pub scale: f32,
}

/// A patch that has made it to the GPU.
pub struct Tile {
    pub texture: egui::TextureHandle,
    pub patch: Patch,
}

/// What the canvas did this frame, which is all the geometry this needs.
#[derive(Debug, Clone, Copy)]
pub struct Look {
    /// Where the whole image landed on screen, in points.
    pub image_rect: egui::Rect,
    /// The canvas it is being looked at through.
    pub canvas: egui::Rect,
    /// Points per source pixel.
    pub zoom: f32,
    /// Physical pixels per point.
    pub ppp: f32,
}

/// The vector behind the picture on screen, and the re-renders it is feeding.
#[derive(Default)]
pub struct Zoom {
    /// The parsed artwork, when the file on screen is one. `None` for every
    /// other format, which is what switches all of this off.
    svg: Option<Arc<Svg>>,
    /// Longest side, in texels, of the whole-artwork raster currently up.
    side: u32,
    /// One channel per request, so a render the user has already zoomed past has
    /// nowhere to land.
    whole_rx: Option<Receiver<Result<RgbaImage, String>>>,
    patch_rx: Option<Receiver<Result<(RgbaImage, Patch), String>>>,
    tile: Option<Tile>,
}

impl Zoom {
    /// Take on the artwork behind a freshly decoded image, with the longest side
    /// of the raster that came with it.
    pub fn adopt(&mut self, svg: Option<Arc<Svg>>, side: u32) {
        self.clear();
        self.side = if svg.is_some() { side } else { 0 };
        self.svg = svg;
    }

    /// Forget everything: a different file is coming.
    pub fn clear(&mut self) {
        self.svg = None;
        self.side = 0;
        self.whole_rx = None;
        self.patch_rx = None;
        self.tile = None;
    }

    /// Stand down without forgetting the artwork.
    ///
    /// For the states where re-rendering would be wrong rather than merely
    /// unnecessary: a geometry edit replaces the canvas with pixels no longer
    /// ruled by the artwork, and re-rendering past a crop would erase the crop.
    /// In-flight renders are dropped rather than parked, because by the time
    /// editing ends they describe a view that has moved on.
    pub fn rest(&mut self) {
        self.whole_rx = None;
        self.patch_rx = None;
        self.tile = None;
    }

    /// The patch to draw over the image, if there is one.
    pub fn tile(&self) -> Option<&Tile> {
        self.tile.as_ref()
    }

    /// Collect finished renders and start whichever one the current view wants.
    ///
    /// Called after the canvas has drawn, because everything it decides comes
    /// from what the canvas just did.
    pub fn poll(
        &mut self,
        ctx: &egui::Context,
        texture: &mut ImageTexture,
        generation: &mut u64,
        look: Look,
    ) {
        let Some(svg) = self.svg.clone() else {
            return;
        };
        let trace = std::env::var_os("IMAGINER_TRACE_SVG").is_some_and(|v| v != "0");

        self.collect_whole(ctx, texture, generation, trace);
        self.collect_patch(ctx, generation, trace);

        let source = texture.source_vec();
        if source.x <= 0.0 || source.y <= 0.0 {
            return;
        }
        let scale = look.zoom * look.ppp;
        let needed = source.max_elem() * scale;

        if needed <= svg::MAX_SIDE as f32 {
            // A whole-artwork raster still reaches this far, and it is the better
            // answer while it does: once it lands, panning needs nothing. The
            // patch is held until that raster is actually sharp enough to take
            // over, so zooming back out does not flash a soft frame on the way.
            if self.side as f32 >= needed {
                self.tile = None;
            }
            self.request_whole(&svg, needed, trace);
        } else {
            // Past the ceiling. The patch comes first — it is what the user is
            // actually looking at — and the whole raster is only topped up to its
            // maximum afterwards, as the backdrop a patch is laid over.
            let started = self.request_patch(&svg, source, scale, look, trace);
            if !started {
                self.request_whole(&svg, needed, trace);
            }
        }

        if self.whole_rx.is_some() || self.patch_rx.is_some() {
            // Nothing to paint yet; wake again rather than idling until the next
            // input event happens to arrive.
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }

    /// Swap in a whole-artwork raster, keeping the layout it replaces.
    ///
    /// `source_size` is restored deliberately: more detail arrives inside the very
    /// same rectangle, so nothing refits and the view does not jump.
    fn collect_whole(
        &mut self,
        ctx: &egui::Context,
        texture: &mut ImageTexture,
        generation: &mut u64,
        trace: bool,
    ) {
        let Some(rx) = self.whole_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(pixels)) => {
                let source_size = texture.source_size;
                *generation += 1;
                let name = format!("image-{generation}");
                *texture = texture::upload_pixels(ctx, &name, &pixels);
                texture.source_size = source_size;
                self.side = pixels.width().max(pixels.height());
                if trace {
                    eprintln!("svg: sharpened raster in, side={}", self.side);
                }
                self.whole_rx = None;
                ctx.request_repaint();
            }
            Ok(Err(err)) => {
                if trace {
                    eprintln!("svg: raster failed: {err}");
                }
                self.whole_rx = None;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.whole_rx = None,
        }
    }

    fn collect_patch(&mut self, ctx: &egui::Context, generation: &mut u64, trace: bool) {
        let Some(rx) = self.patch_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok((pixels, patch))) => {
                *generation += 1;
                let image = egui::ColorImage::from_rgba_unmultiplied(
                    [pixels.width() as usize, pixels.height() as usize],
                    pixels.as_raw(),
                );
                if trace {
                    eprintln!(
                        "svg: patch in, {}x{} at scale {:.2}",
                        pixels.width(),
                        pixels.height(),
                        patch.scale
                    );
                }
                self.tile = Some(Tile {
                    texture: ctx.load_texture(format!("svg-patch-{generation}"), image, PATCH),
                    patch,
                });
                self.patch_rx = None;
                ctx.request_repaint();
            }
            Ok(Err(err)) => {
                if trace {
                    eprintln!("svg: patch failed: {err}");
                }
                self.patch_rx = None;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.patch_rx = None,
        }
    }

    /// Ask for a bigger whole-artwork raster, if the one up has run short.
    fn request_whole(&mut self, svg: &Arc<Svg>, needed: f32, trace: bool) {
        if self.whole_rx.is_some() {
            return;
        }
        let target = (needed * OVERRENDER).min(svg::MAX_SIDE as f32).ceil() as u32;
        if needed <= self.side as f32 * SHARPEN_FACTOR || target <= self.side {
            return;
        }
        if trace {
            eprintln!(
                "svg: raster side={} needs {needed:.0}px, asking for {target}",
                self.side
            );
        }
        let svg = Arc::clone(svg);
        self.whole_rx = spawn("svg-sharpen", move || {
            svg.render_longest(target).map_err(|err| err.to_string())
        });
    }

    /// Ask for the visible part of the artwork, drawn at screen resolution.
    ///
    /// Returns whether a render was started, which is the caller's cue that the
    /// worker is busy with the thing that matters more.
    fn request_patch(
        &mut self,
        svg: &Arc<Svg>,
        source: egui::Vec2,
        scale: f32,
        look: Look,
        trace: bool,
    ) -> bool {
        if self.patch_rx.is_some() {
            return true;
        }
        let Some(visible) = visible_region(look, source) else {
            return false;
        };
        let want = plan(source, visible, scale, PATCH_CAP);
        let (width, height) = pixel_size(want);
        if width == 0 || height == 0 {
            return false;
        }

        // What will be drawn, to the pixel: the rounding above decides the region
        // as much as the request did, and a region that disagreed with its own
        // pixels by half of one would drift the patch against the image under it.
        let exact = Patch {
            region: egui::Rect::from_min_size(
                want.region.min,
                egui::vec2(width as f32, height as f32) / want.scale,
            ),
            scale: want.scale,
        };

        if let Some(tile) = self.tile.as_ref()
            && tile.patch.scale >= exact.scale * 0.99
            && tile.patch.region.contains_rect(visible)
        {
            // The patch up is both sharp enough and wide enough. This is the
            // ordinary case while the user looks around inside one view, and it
            // is why panning a little does not re-render.
            return false;
        }

        // Source pixels are what the canvas thinks in; the artwork thinks in its
        // own units, and the first raster fixed the ratio between them.
        let (units_x, _) = svg.size();
        let per_pixel = units_x / source.x.max(1.0);
        let draw_scale = exact.scale / per_pixel.max(f32::MIN_POSITIVE);
        let (x, y) = (
            exact.region.min.x * per_pixel,
            exact.region.min.y * per_pixel,
        );

        if trace {
            eprintln!(
                "svg: patch {width}x{height} at scale {:.2}, region {:?}",
                exact.scale, exact.region
            );
        }
        let svg = Arc::clone(svg);
        self.patch_rx = spawn("svg-patch", move || {
            svg.render_region(draw_scale, x, y, width, height)
                .map(|pixels| (pixels, exact))
                .map_err(|err| err.to_string())
        });
        true
    }
}

/// How a patch is sampled.
///
/// Linear both ways, unlike the image texture: a patch is drawn at the size it
/// will be shown, so magnification only ever happens in the moments between the
/// view moving and the next patch arriving — and a slightly soft vector edge for
/// those few frames reads far better than a blocky one.
const PATCH: egui::TextureOptions = egui::TextureOptions {
    magnification: egui::TextureFilter::Linear,
    minification: egui::TextureFilter::Linear,
    wrap_mode: egui::TextureWrapMode::ClampToEdge,
    mipmap_mode: None,
};

fn spawn<T: Send + 'static>(
    name: &str,
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Option<Receiver<Result<T, String>>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let _ = tx.send(work());
        })
        .ok()
        .map(|_| rx)
}

/// The part of the image the window is showing, in source pixels.
///
/// `None` when that is nothing at all, which the pan clamp makes unreachable but
/// a zero-sized canvas during a layout pass does not.
fn visible_region(look: Look, source: egui::Vec2) -> Option<egui::Rect> {
    let seen = look.canvas.intersect(look.image_rect);
    if seen.width() <= 0.0 || seen.height() <= 0.0 || look.zoom <= 0.0 {
        return None;
    }
    let min = (seen.min - look.image_rect.min) / look.zoom;
    let max = (seen.max - look.image_rect.min) / look.zoom;
    let whole = egui::Rect::from_min_size(egui::Pos2::ZERO, source);
    let region = egui::Rect::from_min_max(min.to_pos2(), max.to_pos2()).intersect(whole);
    (region.width() > 0.0 && region.height() > 0.0).then_some(region)
}

/// Decide the patch to draw for a given view.
///
/// Margin first, then the cap: when the two disagree the margin gives way, since
/// it only buys future panning while the cap is what keeps the render a render.
/// Only if the visible rectangle *alone* is over the cap does the scale drop —
/// a screen that big is the one case where something has to be softer than
/// perfect, and being slightly soft beats not drawing.
fn plan(source: egui::Vec2, visible: egui::Rect, scale: f32, cap: f32) -> Patch {
    let whole = egui::Rect::from_min_size(egui::Pos2::ZERO, source);
    let margin = visible.size() * PATCH_MARGIN;
    let mut region = visible.expand2(margin).intersect(whole);
    let mut scale = scale;

    let over = |region: egui::Rect, scale: f32| {
        let pixels = region.size() * scale;
        (pixels.x / cap).max(pixels.y / cap)
    };

    if over(region, scale) > 1.0 {
        // Shrink back towards what is actually on screen, around its centre.
        let shrunk = (region.size() / over(region, scale)).max(visible.size());
        region = egui::Rect::from_center_size(visible.center(), shrunk)
            .translate(nudge_inside(visible.center(), shrunk, whole))
            .intersect(whole);

        // A hair over is rounding, not a real overflow — dividing the scale by
        // 1.000001 would cost a re-render's worth of sharpness for nothing.
        let still = over(region, scale);
        if still > 1.001 {
            scale /= still;
        }
    }

    Patch { region, scale }
}

/// How far a rectangle of `size` centred at `centre` has to move to sit inside
/// `whole`, when it fits at all.
fn nudge_inside(centre: egui::Pos2, size: egui::Vec2, whole: egui::Rect) -> egui::Vec2 {
    let rect = egui::Rect::from_center_size(centre, size);
    egui::vec2(
        (whole.left() - rect.left()).max(0.0) + (whole.right() - rect.right()).min(0.0),
        (whole.top() - rect.top()).max(0.0) + (whole.bottom() - rect.bottom()).min(0.0),
    )
}

fn pixel_size(patch: Patch) -> (u32, u32) {
    let pixels = patch.region.size() * patch.scale;
    (
        pixels.x.round().max(0.0) as u32,
        pixels.y.round().max(0.0) as u32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: egui::Vec2 = egui::vec2(1024.0, 1024.0);

    fn rect(x: f32, y: f32, w: f32, h: f32) -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, h))
    }

    #[test]
    fn a_patch_covers_more_than_the_screen_so_small_pans_are_free() {
        let visible = rect(400.0, 400.0, 100.0, 100.0);
        let patch = plan(SOURCE, visible, 20.0, 8192.0);

        assert!(
            patch.region.contains_rect(visible),
            "the visible part must be inside the patch"
        );
        assert!(
            patch.region.width() > visible.width(),
            "and there should be margin around it"
        );
        assert_eq!(patch.scale, 20.0, "nothing here needed a softer render");
    }

    #[test]
    fn the_margin_is_what_gives_way_when_the_cap_is_reached() {
        // 100 source pixels at 40x is 4000 across; the margin would take it past
        // a 4096 cap on its own.
        let visible = rect(400.0, 400.0, 100.0, 100.0);
        let patch = plan(SOURCE, visible, 40.0, 4096.0);

        let pixels = patch.region.size() * patch.scale;
        assert!(pixels.x <= 4096.5 && pixels.y <= 4096.5, "capped: {pixels:?}");
        assert!(patch.region.contains_rect(visible), "still covers the screen");
        assert_eq!(patch.scale, 40.0, "the scale is the last thing to give");
    }

    #[test]
    fn a_screen_too_large_for_the_cap_is_drawn_softer_rather_than_not_at_all() {
        let visible = rect(0.0, 0.0, 1024.0, 1024.0);
        let patch = plan(SOURCE, visible, 8.0, 4096.0);

        assert!(patch.scale < 8.0, "the scale had to drop");
        let pixels = patch.region.size() * patch.scale;
        assert!(pixels.x <= 4096.5 && pixels.y <= 4096.5, "capped: {pixels:?}");
        assert!(patch.region.contains_rect(visible));
    }

    #[test]
    fn a_patch_never_reaches_outside_the_artwork() {
        // Hard against the bottom-right corner: expanding by the margin would
        // run off the image, and rendering empty space is wasted pixels.
        let visible = rect(974.0, 974.0, 50.0, 50.0);
        let patch = plan(SOURCE, visible, 10.0, 8192.0);

        let whole = egui::Rect::from_min_size(egui::Pos2::ZERO, SOURCE);
        assert!(whole.contains_rect(patch.region), "{:?}", patch.region);
        assert!(patch.region.contains_rect(visible));
    }

    #[test]
    fn a_capped_patch_against_an_edge_stays_inside_the_artwork() {
        let visible = rect(0.0, 0.0, 100.0, 100.0);
        let patch = plan(SOURCE, visible, 40.0, 4096.0);

        let whole = egui::Rect::from_min_size(egui::Pos2::ZERO, SOURCE);
        assert!(whole.contains_rect(patch.region), "{:?}", patch.region);
        assert!(patch.region.contains_rect(visible));
    }

    #[test]
    fn the_visible_region_is_the_screen_expressed_in_source_pixels() {
        // A 1024-pixel image drawn at 4x, with only the middle of it on a
        // 512-point canvas: a quarter of the image, starting at 384.
        let look = Look {
            image_rect: egui::Rect::from_min_size(egui::pos2(-1792.0, -1792.0), SOURCE * 4.0),
            canvas: rect(0.0, 0.0, 512.0, 512.0),
            zoom: 4.0,
            ppp: 1.0,
        };

        let region = visible_region(look, SOURCE).expect("some of it is on screen");
        assert_eq!(region.min, egui::pos2(448.0, 448.0));
        assert_eq!(region.size(), egui::vec2(128.0, 128.0));
    }

    /// Left half red, right half blue, in a 100x100 viewBox.
    const HALVES: &str = concat!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100" "#,
        r#"viewBox="0 0 100 100">"#,
        r#"<rect width="50" height="100" fill="red"/>"#,
        r#"<rect x="50" width="50" height="100" fill="blue"/></svg>"#
    );

    /// The whole path, without a window: threshold, request, worker, arrival.
    ///
    /// The geometry above is arithmetic and the rendering is `imaginer-core`'s;
    /// what is left to get wrong is the wiring between them, and that is what
    /// this runs. An `egui::Context` is happy off-screen — its texture manager
    /// is CPU-side — so there is no GPU here and no window either.
    #[test]
    fn zooming_past_the_ceiling_produces_a_patch_at_screen_resolution() {
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |_| {});

        let svg = Arc::new(Svg::parse(HALVES.as_bytes()).unwrap());
        let pixels = svg.render_fit().unwrap();
        let side = pixels.width().max(pixels.height());
        let mut texture = texture::upload_pixels(&ctx, "test", &pixels);
        let mut generation = 0;

        let mut zoom = Zoom::default();
        zoom.adopt(Some(Arc::clone(&svg)), side);

        // 20x into a 1024-pixel raster wants 20,480 pixels a side — five times
        // what a whole-artwork render is allowed to be.
        let look = Look {
            image_rect: egui::Rect::from_min_size(
                egui::pos2(-5000.0, -5000.0),
                egui::Vec2::splat(side as f32 * 20.0),
            ),
            canvas: rect(0.0, 0.0, 800.0, 600.0),
            zoom: 20.0,
            ppp: 1.0,
        };

        let mut waited = 0;
        while zoom.tile().is_none() && waited < 200 {
            zoom.poll(&ctx, &mut texture, &mut generation, look);
            std::thread::sleep(std::time::Duration::from_millis(10));
            waited += 1;
        }

        let tile = zoom.tile().expect("a patch should have arrived");
        assert_eq!(
            tile.patch.scale, 20.0,
            "it should be drawn at exactly what the screen shows"
        );

        let visible = visible_region(look, texture.source_vec()).unwrap();
        assert!(
            tile.patch.region.contains_rect(visible),
            "the patch {:?} must cover what is on screen {visible:?}",
            tile.patch.region
        );
        let expected = pixel_size(tile.patch);
        assert_eq!(
            tile.texture.size(),
            [expected.0 as usize, expected.1 as usize],
            "the texture should be exactly as many pixels as the region asks for"
        );

        // And zooming back out hands the job back to the whole-artwork raster.
        let close = Look {
            image_rect: egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::Vec2::splat(side as f32),
            ),
            zoom: 1.0,
            ..look
        };
        zoom.poll(&ctx, &mut texture, &mut generation, close);
        assert!(zoom.tile().is_none(), "no patch is needed at 100%");
    }

    #[test]
    fn a_pixel_size_follows_the_region_and_the_scale() {
        let patch = Patch {
            region: rect(0.0, 0.0, 100.0, 50.0),
            scale: 3.5,
        };
        assert_eq!(pixel_size(patch), (350, 175));
    }
}
