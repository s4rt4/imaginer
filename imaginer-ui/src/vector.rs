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
#[derive(Debug, Clone, Copy, PartialEq)]
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

/// Everything a request is decided from, worked out once per frame.
struct View {
    /// The image's native size, in source pixels.
    source: egui::Vec2,
    /// Screen pixels per source pixel.
    scale: f32,
    /// The largest patch this machine will take, per side.
    cap: f32,
    look: Look,
    /// Whether the view is the same as it was last frame.
    steady: bool,
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
    /// Set when a render came back an error, which stands the whole path down
    /// until another file arrives. Nothing about a failure changes what the next
    /// frame would ask for, so without this a rasteriser that cannot allocate
    /// its pixmap is asked again — and handed a fresh thread — sixty times a
    /// second, for as long as the picture is on screen.
    stalled: bool,
    /// The largest whole-artwork render already asked for.
    ///
    /// Not the same question as `side`, and the difference is what stops a loop:
    /// `side` is what reached the GPU, which on a driver whose texture limit is
    /// below [`svg::MAX_SIDE`] is *smaller* than what was drawn. Measuring the
    /// threshold against that is right — those are the texels being magnified —
    /// but asking again for a size already drawn would rasterise 67MB, watch it
    /// be scaled down to the same texture, and do it again forever.
    asked: u32,
    /// The geometry the previous frame reported, so a view still being moved can
    /// be told from one that has come to rest.
    previous: Option<Look>,
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
        self.stalled = false;
        self.asked = 0;
        self.previous = None;
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
        // And the raster resolution with them. Whatever is on the canvas during
        // an edit is the edit's output, and when the edit is undone the pipeline
        // re-uploads from the original pixels — so the sharp raster this had
        // recorded is gone, and remembering it would leave the artwork stuck
        // blurry with nothing to re-trigger a render.
        self.side = 0;
        self.asked = 0;
        // An edit is also the natural place to give a failed render another go:
        // whatever could not be allocated a minute ago may well fit now, and one
        // retry per edit is not a storm.
        self.stalled = false;
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
        if self.stalled {
            // A render failed. What is up stays up — it is still the right
            // picture, just not as sharp as it could be — but nothing more is
            // asked for until a different file arrives.
            return;
        }

        let source = texture.source_vec();
        if source.x <= 0.0 || source.y <= 0.0 {
            return;
        }
        let scale = look.zoom * look.ppp;
        let needed = source.max_elem() * scale;

        // A view that moved between this frame and the last is still moving, and
        // a patch takes long enough to draw that one started now would be stale
        // before it landed — a core burned per frame of a drag for pixels nobody
        // ever sees. The exception is having no patch at all: something sharp is
        // worth starting even mid-gesture, because until it arrives the screen
        // is magnified raster.
        let steady = self.previous == Some(look);
        self.previous = Some(look);
        // Ours is the smaller ceiling on any machine worth running this on, but
        // a patch goes to the GPU without passing through the downscale that
        // `texture::upload` does, so the driver's limit has to be honoured here.
        let view = View {
            source,
            scale,
            cap: PATCH_CAP.min(ctx.input(|i| i.max_texture_side) as f32),
            look,
            steady,
        };

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
            let started = self.request_patch(&svg, &view, trace);
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
                // What reached the GPU, which `upload_pixels` will have scaled
                // down if the driver's texture limit is below what was drawn.
                // Recording the rasterised size instead would have the sharpen
                // threshold measure against texels that are not there, and the
                // artwork would sit blurry with nothing able to trigger a render.
                self.side = texture.uploaded_size.0.max(texture.uploaded_size.1);
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
                self.stalled = true;
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
                self.stalled = true;
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
        if target <= self.asked {
            // Already drawn at this size. The texture being smaller than that
            // means the driver would not take it, which drawing it again will
            // not change.
            return;
        }
        self.asked = target;
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
    fn request_patch(&mut self, svg: &Arc<Svg>, view: &View, trace: bool) -> bool {
        if self.patch_rx.is_some() {
            return true;
        }
        if !view.steady && self.tile.is_some() {
            return false;
        }
        let Some(visible) = visible_region(view.look, view.source) else {
            return false;
        };
        let exact = plan(view.source, visible, view.scale, view.cap);
        let (width, height) = pixel_size(exact);
        if width == 0 || height == 0 {
            return false;
        }

        if let Some(tile) = self.tile.as_ref()
            && tile.patch.scale >= exact.scale * 0.99
            && covers(tile.patch, visible)
        {
            // The patch up is both sharp enough and wide enough. This is the
            // ordinary case while the user looks around inside one view, and it
            // is why panning a little does not re-render.
            return false;
        }

        // Source pixels are what the canvas thinks in; the artwork thinks in its
        // own units, and the first raster fixed the ratio between them.
        let (units_x, _) = svg.size();
        let per_pixel = units_x / view.source.x.max(1.0);
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
        // Only if the thread is real. A machine that cannot spawn one has not
        // started the render that matters more, so say so and let the caller
        // fall back to topping up the whole-artwork raster instead.
        self.patch_rx.is_some()
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

    // Settle on whole pixels here, and let the region follow them rather than the
    // other way round. A region rounded to fewer pixels than it covers is one the
    // freshness check below would find too small for the screen *every* frame,
    // which is a render storm rather than a blurry edge.
    //
    // Rounding up, except where that would reach past the artwork: pixels drawn
    // off the edge come back transparent, so they cost time to produce nothing.
    // What that shaves off is a fraction of one pixel at one edge, which is what
    // the slack in `covers` is for.
    let room = (whole.max - region.min) * scale;
    let pixels = (region.size() * scale)
        .ceil()
        .min(egui::Vec2::splat(cap))
        .min(egui::vec2(room.x.floor(), room.y.floor()));
    Patch {
        region: egui::Rect::from_min_size(region.min, pixels / scale),
        scale,
    }
}

/// Whether a patch still covers what the screen is showing.
///
/// With a screen pixel of slack: the cap above can shave a fraction off a patch
/// that is otherwise exactly right, and re-rendering the whole thing over half a
/// pixel at one edge is a worse answer than the half pixel.
fn covers(patch: Patch, visible: egui::Rect) -> bool {
    patch
        .region
        .expand(1.0 / patch.scale.max(f32::MIN_POSITIVE))
        .contains_rect(visible)
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

/// How many pixels a patch is, which [`plan`] has already made a whole number.
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
        // run off the image, and rendering empty space is wasted pixels. The
        // scale is deliberately one that does not divide the region evenly, so
        // rounding up to whole pixels is the thing pushing at the edge.
        let visible = rect(971.0, 971.0, 50.0, 50.0);
        let patch = plan(SOURCE, visible, 7.3, 8192.0);

        let whole = egui::Rect::from_min_size(egui::Pos2::ZERO, SOURCE);
        assert!(whole.contains_rect(patch.region), "{:?}", patch.region);
        assert!(covers(patch, visible), "{:?}", patch.region);

        // A whole number of pixels, to within the float round-trip through
        // `pixels / scale` and back — which is what keeps the rendered patch and
        // the region describing it from disagreeing enough to matter.
        let pixels = patch.region.size() * patch.scale;
        assert!(
            (pixels.x - pixels.x.round()).abs() < 0.01
                && (pixels.y - pixels.y.round()).abs() < 0.01,
            "a patch should be whole pixels, got {pixels:?}"
        );
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

    /// Settle whatever renders are in flight, with a bound so a hang is a
    /// failure rather than a test that never finishes.
    fn settle(zoom: &mut Zoom, ctx: &egui::Context, texture: &mut ImageTexture, look: Look) {
        // Generous, because these run in a debug build where a multi-megapixel
        // rasterise is seconds rather than milliseconds.
        let mut generation = 0;
        for _ in 0..1500 {
            zoom.poll(ctx, texture, &mut generation, look);
            if zoom.whole_rx.is_none() && zoom.patch_rx.is_none() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("renders never finished");
    }

    /// A driver that will not take the texture must not be asked twice.
    ///
    /// `upload_pixels` scales a render down to the texture limit, so on such a
    /// machine the raster on screen is permanently smaller than what was drawn.
    /// The sharpen threshold measures against the smaller number — correctly,
    /// those are the texels being magnified — which means without a memory of
    /// what has already been drawn it asks for the larger one again, forever.
    #[test]
    fn a_render_the_gpu_shrinks_is_not_asked_for_again() {
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(
            egui::RawInput {
                max_texture_side: Some(1024),
                ..Default::default()
            },
            |_| {},
        );

        let svg = Arc::new(Svg::parse(HALVES.as_bytes()).unwrap());
        let pixels = svg.render_longest(2048).unwrap();
        let mut texture = texture::upload_pixels(&ctx, "test", &pixels);
        assert_eq!(texture.uploaded_size, (1024, 1024), "the driver shrank it");

        let mut zoom = Zoom::default();
        zoom.adopt(
            Some(Arc::clone(&svg)),
            texture.uploaded_size.0.max(texture.uploaded_size.1),
        );

        // Enough zoom to want more texels than the driver took, and not enough
        // to leave the whole-artwork path for patches.
        let side = pixels.width() as f32;
        let look = Look {
            image_rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::splat(side)),
            canvas: rect(0.0, 0.0, 800.0, 600.0),
            zoom: 1.0,
            ppp: 1.0,
        };

        settle(&mut zoom, &ctx, &mut texture, look);
        let asked_once = zoom.asked;
        assert!(asked_once > 0, "it should have tried once");
        assert_eq!(zoom.side, 1024, "and got the driver's answer back");

        let mut generation = 0;
        for _ in 0..5 {
            zoom.poll(&ctx, &mut texture, &mut generation, look);
            assert!(
                zoom.whole_rx.is_none(),
                "the same render must not be started again"
            );
        }
        assert_eq!(zoom.asked, asked_once, "and nothing new was asked for");
    }

    /// A view still being dragged should not have renders started under it.
    #[test]
    fn a_moving_view_waits_but_a_first_patch_does_not() {
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |_| {});

        let svg = Arc::new(Svg::parse(HALVES.as_bytes()).unwrap());
        let pixels = svg.render_fit().unwrap();
        let side = pixels.width().max(pixels.height());
        let mut texture = texture::upload_pixels(&ctx, "test", &pixels);
        let mut zoom = Zoom::default();
        zoom.adopt(Some(Arc::clone(&svg)), side);

        let at = |offset: f32| Look {
            image_rect: egui::Rect::from_min_size(
                egui::pos2(-5000.0 + offset, -5000.0),
                egui::Vec2::splat(side as f32 * 20.0),
            ),
            canvas: rect(0.0, 0.0, 800.0, 600.0),
            zoom: 20.0,
            ppp: 1.0,
        };

        // Nothing on screen yet, so the first patch starts even though this is
        // the first frame at this geometry and nothing is steady.
        let mut generation = 0;
        for _ in 0..300 {
            zoom.poll(&ctx, &mut texture, &mut generation, at(0.0));
            if zoom.tile().is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(zoom.tile().is_some(), "a first patch should have been drawn");

        // Now drag: every frame a different view, none of them repeated.
        for step in 1..6 {
            zoom.poll(&ctx, &mut texture, &mut generation, at(step as f32 * 400.0));
            assert!(
                zoom.patch_rx.is_none(),
                "a patch was started under a view still moving"
            );
        }

        // And on the frame the drag stops, the same view twice, it starts.
        let rested = at(2400.0);
        zoom.poll(&ctx, &mut texture, &mut generation, rested);
        zoom.poll(&ctx, &mut texture, &mut generation, rested);
        assert!(
            zoom.patch_rx.is_some() || zoom.tile().is_some(),
            "a view at rest should get its patch"
        );
    }

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
