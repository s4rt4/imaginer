//! Application state and the per-frame update loop.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use eframe::egui;
use imaginer_core::image::RgbaImage;
use imaginer_core::{Adjust, Decoded, Edits, Folder, Op, Order, Stage, Stamp};

use crate::icons::Icons;
use crate::idle;
use crate::logo;
use crate::prefetch::Prefetcher;
use crate::shader::AdjustShader;
use crate::startup::StartupTrace;
use crate::texture::{self, ImageTexture};
use crate::vector;
use crate::views::crop::CropState;
use crate::views::{crop, info, settings, sidebar, statusbar, timeline, toolbar, viewer};
use crate::{LoadMessage, clipboard, decode_into, theme, titlebar};

/// How long a status-bar notice stays up. Long enough to read in passing, short
/// enough that it never becomes part of the furniture.
const NOTICE_DURATION: Duration = Duration::from_secs(3);

/// How far either side of the current image to decode ahead.
///
/// One. Two would cover a second keypress arriving before the first prefetch
/// finished, but it also doubles the work thrown away every time the user changes
/// direction â€” and the image that matters, the very next one, would be finished no
/// sooner for it.
const PREFETCH_RADIUS: usize = 1;

/// What the clipboard worker has to report.
enum ClipboardResult {
    Copied,
    Pasted(clipboard::Pasted),
    Failed(String),
}

/// Confirmation of something whose only other evidence is that it worked.
struct Notice {
    text: String,
    expires_at: Instant,
}

/// Measurement of the action a folder session repeats most.
///
/// `IMAGINER_TRACE_NAV=1` prints a line per image shown â€” how long it took to reach
/// the screen, and whether the pixels came from the cache. Prefetch is only worth a
/// thread and 512MB if those two numbers are far apart, and this is what says whether
/// they are. Same idiom as the startup trace: off unless asked for, and reported by
/// the shipping binary rather than a special build.
struct NavTrace {
    enabled: bool,
    asked_at: Instant,
    from_cache: bool,
}

impl NavTrace {
    fn new() -> Self {
        Self {
            enabled: std::env::var_os("IMAGINER_TRACE_NAV").is_some_and(|v| v != "0"),
            asked_at: Instant::now(),
            from_cache: false,
        }
    }

    /// An image has been asked for. Starts the clock.
    fn asked(&mut self, from_cache: bool) {
        self.asked_at = Instant::now();
        self.from_cache = from_cache;
    }

    /// Full-resolution pixels are on screen. Reports what the wait was.
    ///
    /// Nothing to report without a path: pixels from the clipboard were not
    /// navigated to and were never decoded, so timing them against the last
    /// keypress would put a number in the log that measures nothing.
    fn arrived(&self, path: Option<&Path>) {
        if !self.enabled {
            return;
        }
        let Some(name) = path
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned())
        else {
            return;
        };
        let source = if self.from_cache { "cached" } else { "decoded" };
        eprintln!(
            "nav: {name} {source} {:.1}ms",
            self.asked_at.elapsed().as_secs_f64() * 1000.0
        );
    }
}

pub struct App {
    trace: StartupTrace,
    path: Option<PathBuf>,
    rx: Receiver<LoadMessage>,
    texture: Option<ImageTexture>,
    stage: Option<Stage>,
    error: Option<String>,
    notice: Option<Notice>,
    view: viewer::ViewState,
    /// What the current file looked like on disk when it was opened. Carries the
    /// size the status bar shows, and is what the cache is keyed on â€” so an image
    /// saved over is re-decoded rather than served from before the save.
    stamp: Option<Stamp>,
    /// True while a decode is in flight, which is also what drives repainting.
    loading: bool,
    /// Bumped per load so each texture gets a distinct name in egui's texture manager.
    texture_generation: u64,
    last_zoom: f32,
    /// Tracked rather than queried: the viewport command is a one-way instruction,
    /// so the toolbar's toggle needs its own idea of the current state.
    fullscreen: bool,
    /// The images sitting beside the current one, built on first use rather than at
    /// startup. Scanning a directory of thousands of files is real time, and a
    /// launch that only ever looks at the image it was given should not pay for it.
    folder: Option<Folder>,
    /// What to sort a listing by. Held here rather than only inside `folder`,
    /// because it has to outlive one: opening a different image throws the listing
    /// away, and the order the user picked is not a property of the folder they
    /// happened to be in when they picked it. Starts from the saved settings, and
    /// a change here is written back â€” see `set_order`.
    order: Order,
    /// What the settings panel can change, loaded once at startup and saved on
    /// change. The slideshow's hold time and the default sort order live here.
    settings: imaginer_core::Settings,
    /// Whether the settings panel is open. Shares the right-hand strip with the
    /// edit sidebar and the info panel â€” one at a time.
    settings_open: bool,
    /// The pixels exactly as decoded, kept so every edit runs from the original
    /// rather than compounding on already-edited output. Behind an `Arc` because it
    /// is shared: with the edit worker, and with the cache entry it came from â€” so
    /// an image evicted while it is still on screen costs nothing to keep showing.
    source: Option<Arc<RgbaImage>>,
    edits: Edits,
    /// What the colour sliders are showing, which is not always what the undo stack
    /// holds: mid-drag it runs ahead, and it is committed when the slider is let go.
    /// The texture never contains it â€” the shader applies it at draw time.
    adjust: Adjust,
    /// Draws the adjustment. Compiled on first use, not at startup.
    shader: AdjustShader,
    /// Results from the edit worker. Replaced per run, so a stale result from a
    /// pipeline the user has already moved past has nowhere to land.
    edit_rx: Receiver<RgbaImage>,
    /// True while the pipeline is running, which is what keeps frames coming until
    /// the new pixels arrive.
    applying: bool,
    /// When the slideshow should move on. `None` when it is not running.
    slideshow: Option<Instant>,
    /// The remaining frames of an animated image, `None` for a still. `pixels` of
    /// the current decode is this frame zero, so the first thing on screen is
    /// frame one whether or not more follow.
    animation: Option<Arc<imaginer_core::Animation>>,
    /// Which animation frame is on screen right now.
    anim_frame: usize,
    /// Whether frames advance on their own. An animation arrives playing; pause
    /// is what the user does to it.
    anim_playing: bool,
    /// When the frame on screen gives way to the next. `None` while paused â€”
    /// same shape as `slideshow`, and ticked the same way, so idle CPU stays at
    /// zero between frames of a paused or absent animation.
    anim_due: Option<Instant>,
    /// True while the timeline scrubber is being dragged, which holds playback.
    scrubbing: bool,
    /// What playback was doing when the drag started, so letting go resumes it
    /// rather than leaving it stopped because the user touched the slider.
    playing_before_scrub: bool,
    /// Set once a frame has been painted, which is when scanning the folder stops
    /// being something the user is waiting on.
    painted: bool,
    /// True between asking for an image and the first pixels of it arriving. What
    /// is on screen until then belongs to the previous one.
    awaiting_first_frame: bool,
    sidebar_open: bool,
    info_open: bool,
    /// What the EXIF block of the image on screen holds. Read when the info panel
    /// first asks for it and kept until the image changes â€” `info_read` is what
    /// tells "not looked at yet" apart from "looked, and there was none".
    info: imaginer_core::Info,
    info_read: bool,
    /// The crop being drawn, if the canvas is currently in that mode.
    crop: Option<CropState>,
    export: imaginer_core::ExportSettings,
    /// Where the save worker reports back: the file it wrote, or why it could not.
    save_rx: Receiver<Result<PathBuf, String>>,
    saving: bool,
    /// Where the trim worker reports the opaque bounds it measured.
    trim_rx: Receiver<Option<(u32, u32, u32, u32)>>,
    trimming: bool,
    /// Where the clipboard worker reports, in either direction.
    clipboard_rx: Receiver<ClipboardResult>,
    clipboard_busy: bool,
    /// Uploaded the first time the empty state is drawn, so launching with an image
    /// never pays for it.
    logotype: Option<egui::TextureHandle>,
    /// The transparency checker, uploaded the first time a picture with
    /// see-through pixels is shown. Most sessions never ask for it.
    checker: Option<egui::TextureHandle>,
    /// The vector artwork behind the image on screen, when it is an SVG, and the
    /// re-renders that keep it sharp as the view moves. Inert for every other
    /// format; `Decoded.svg` is what wakes it up.
    vector: vector::Zoom,
    /// What the canvas drew last frame, which is what `vector` reasons about.
    /// `None` until something has been on screen once.
    last_look: Option<vector::Look>,
    icons: Icons,
    /// Decoded images kept around, and the thread that decodes the ones nobody has
    /// asked for yet.
    prefetch: Prefetcher,
    nav: NavTrace,
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        trace: StartupTrace,
        path: Option<PathBuf>,
        rx: Receiver<LoadMessage>,
    ) -> Self {
        // Reaching here means the window and GL context exist, so this mark splits
        // startup into "platform setup" and "our own work".
        trace.mark("context_ready");
        trace.report_gl(cc.gl.as_deref());

        theme::install(&cc.egui_ctx);
        titlebar::recolour(cc, theme::TITLEBAR_BG, theme::TITLEBAR_TEXT);
        trace.mark("theme_ready");

        // The image handed to us on the command line never passes through `show`,
        // so its export format has to be picked up here too â€” otherwise launching
        // straight onto a JPEG offers to save it as a PNG.
        let export = export_defaults(path.as_deref());

        // Read before anything that consults it: the saved sort order is what
        // this session starts with. A file read of a few bytes, and it sits
        // after the decode spawn on the startup path â€” the decode thread is
        // already running by the time this line executes.
        let settings = imaginer_core::Settings::load();

        Self {
            trace,
            stamp: path.as_deref().and_then(Stamp::of),
            loading: path.is_some(),
            path,
            rx,
            texture: None,
            stage: None,
            error: None,
            notice: None,
            view: viewer::ViewState::default(),
            texture_generation: 0,
            last_zoom: 1.0,
            fullscreen: false,
            folder: None,
            order: settings.order,
            settings,
            settings_open: false,
            source: None,
            edits: Edits::default(),
            adjust: Adjust::NONE,
            shader: AdjustShader::default(),
            // A channel with no sender: `applying` is false, so nothing reads it
            // until the first edit replaces it with a live one.
            edit_rx: std::sync::mpsc::channel().1,
            applying: false,
            slideshow: None,
            animation: None,
            anim_frame: 0,
            anim_playing: false,
            anim_due: None,
            scrubbing: false,
            playing_before_scrub: false,
            painted: false,
            awaiting_first_frame: false,
            sidebar_open: false,
            info_open: false,
            info: imaginer_core::Info::default(),
            info_read: false,
            crop: None,
            export,
            save_rx: std::sync::mpsc::channel().1,
            saving: false,
            trim_rx: std::sync::mpsc::channel().1,
            trimming: false,
            clipboard_rx: std::sync::mpsc::channel().1,
            clipboard_busy: false,
            logotype: None,
            checker: None,
            vector: vector::Zoom::default(),
            last_look: None,
            icons: Icons::default(),
            prefetch: Prefetcher::with_budget(cache_budget()),
            nav: NavTrace::new(),
        }
    }

    /// Open a file the user picked, from somewhere the folder listing may not cover.
    fn open(&mut self, ctx: &egui::Context, path: PathBuf) {
        self.folder = None;
        self.show(ctx, path);

        // Rescanned straight away rather than left for the first arrow key. The scan
        // is kept off the *startup* path, which this is not â€” and prefetch cannot
        // warm neighbours nobody has told it about, so deferring it would mean the
        // first step after every Open was the slow kind.
        self.folder();
        self.warm_neighbours();
    }

    /// The images beside the current one, scanned on first use.
    fn folder(&mut self) -> &mut Folder {
        if self.folder.is_none() {
            self.folder = Some(match self.path.as_deref() {
                Some(path) => Folder::containing(path, self.order),
                None => Folder::default(),
            });
        }
        self.folder.as_mut().expect("just filled in")
    }

    /// Re-order the folder listing.
    ///
    /// The image on screen does not change â€” `Folder::set_order` keeps the cursor on
    /// it â€” but what comes next does, so the warmed neighbours are now the previous
    /// order's and have to be asked for again.
    ///
    /// The choice is also written to the settings file: it is what new sessions
    /// start from, which is the whole reason the settings exist. A few bytes per
    /// click, on a click nobody is waiting behind.
    fn set_order(&mut self, order: Order) {
        self.order = order;
        if let Some(folder) = self.folder.as_mut() {
            folder.set_order(order);
        }
        self.settings.order = order;
        self.settings.save();
        self.warm_neighbours();
    }

    fn step(&mut self, ctx: &egui::Context, forward: bool) {
        let next = if forward {
            self.folder().next_image()
        } else {
            self.folder().prev_image()
        };

        if let Some(path) = next {
            // Not `open`: stepping stays inside the listing that is already built,
            // and rescanning the directory per keypress is the thing to avoid.
            self.show(ctx, path);
        }
    }

    /// Clear out everything that belonged to the image being replaced.
    ///
    /// Shared by every route that puts a new image on screen â€” opened, stepped to,
    /// or pasted â€” because forgetting one of these is exactly how an edit stack or a
    /// stale decode leaks from one image into the next. `path` is `None` for pixels
    /// that came from the clipboard and have no file behind them.
    fn begin_showing(&mut self, path: Option<PathBuf>) {
        // A channel with no sender, so a load still in flight is orphaned rather
        // than left able to land on top of the new image. Whoever starts a decode
        // replaces it with a live one; whoever does not has nothing to hear from.
        self.rx = std::sync::mpsc::channel().1;

        self.stamp = path.as_deref().and_then(Stamp::of);
        self.export = export_defaults(path.as_deref());
        self.path = path;
        self.stage = None;
        self.error = None;
        self.notice = None;
        self.texture_generation += 1;

        // The outgoing image stays up until its replacement is ready. Clearing it
        // here instead would flash the empty state â€” logo, "drop an image here" â€”
        // between every pair of photographs, which is the same reason there is no
        // spinner: a window that empties and refills reads as jankier than one that
        // waits the extra 50ms. The view is refitted when the new pixels land, not
        // now, or the old image would visibly snap to fit on its way out.
        self.awaiting_first_frame = true;

        // Edits belong to the image they were made on. Carrying them across would
        // silently rotate the next photo because of something done to the last one.
        self.source = None;
        self.edits.clear();
        self.adjust = Adjust::NONE;
        self.applying = false;

        // So does an animation, and its playback with it: the next file starts
        // paused on its own first frame until its decode says otherwise.
        self.animation = None;
        self.anim_frame = 0;
        self.anim_playing = false;
        self.anim_due = None;
        self.scrubbing = false;
        self.playing_before_scrub = false;

        // A vector artwork belongs to its file, and so does the raster
        // resolution it was last drawn at.
        self.vector.clear();

        // The EXIF block describes the file it came out of, so it goes with it. Not
        // read here: the panel is usually shut, and opening a file a second time to
        // fill a panel nobody is looking at is work on the navigation path.
        self.info = imaginer_core::Info::default();
        self.info_read = false;

        // And so does a half-drawn crop: the selection is in image pixels, so left
        // alone it would hang over the next image at coordinates that mean nothing
        // there. Reachable from the slideshow, from a save that reloads, and from a
        // paste, none of which go through the modal's own exits.
        self.crop = None;
    }

    fn show(&mut self, ctx: &egui::Context, path: PathBuf) {
        self.begin_showing(Some(path.clone()));

        // Prefetched or stepped back to, this image may already be decoded â€” in
        // which case it goes up in this very frame. No thread, no second frame, and
        // none of the wait that makes walking a folder feel slow. Asking with the
        // frame set in mind: a still that prefetch stored for an animated file is
        // not good enough here, and the miss re-decodes it whole.
        let ready = self
            .stamp
            .as_ref()
            .and_then(|stamp| self.prefetch.cached(&path, stamp, true));

        self.nav.asked(ready.is_some());

        match ready {
            Some(decoded) => self.accept(ctx, decoded),
            None => {
                let (tx, rx) = std::sync::mpsc::channel();
                self.rx = rx;
                self.loading = true;
                std::thread::Builder::new()
                    .name("decode".to_owned())
                    .spawn(move || decode_into(path, &tx))
                    .expect("failed to spawn decode thread");
            }
        }

        // After the image itself is under way, never before: the neighbours are the
        // one thing here nobody is waiting on.
        self.warm_neighbours();

        ctx.request_repaint();
    }

    /// Show pixels that came from the clipboard rather than from a file.
    ///
    /// Having no path is a state the rest of the app has to respect rather than
    /// route around: Save becomes Save-as because there is no original to overwrite,
    /// Delete and Copy-path have nothing to act on, and the folder listing is empty
    /// because a pasted image sits beside nothing.
    fn show_pasted(&mut self, ctx: &egui::Context, pixels: RgbaImage) {
        self.begin_showing(None);
        // Not the previous image's listing, which belongs to a folder this image is
        // not in. Rebuilt empty on first use, since there is no path to build from.
        self.folder = None;

        let full_size = pixels.dimensions();
        // One alpha pass over clipboard pixels — a screenshot is megabytes, but
        // this lands on the UI thread only via paste, which is a deliberate,
        // rare action.
        let has_transparency = {
            let mut any = false;
            for px in pixels.as_raw().chunks_exact(4) {
                if px[3] != 255 {
                    any = true;
                    break;
                }
            }
            any
        };
        self.accept(
            ctx,
            imaginer_core::Decoded {
                pixels: Arc::new(pixels),
                stage: Stage::Full,
                // Clipboard pixels carry no EXIF: whatever put them there had already
                // resolved any rotation into the pixels themselves.
                orientation: imaginer_core::Orientation::Normal,
                full_size,
                animation: None,
                has_transparency,
                svg: None,
            },
        );

        ctx.request_repaint();
    }

    /// Ask the prefetch thread to decode the images either side of this one.
    ///
    /// Silently does nothing while the folder listing is still unbuilt, which is only
    /// ever the first frame â€” the scan is deliberately off the startup path, and
    /// forcing it here to warm a neighbour would put it back on.
    fn warm_neighbours(&self) {
        let Some(folder) = self.folder.as_ref() else {
            return;
        };
        self.prefetch.request(folder.neighbours(PREFETCH_RADIUS));
    }

    /// Put decoded pixels on screen, and keep them for the next time this image is
    /// asked for.
    ///
    /// Called once per decode message, which for an animated file is twice at full
    /// resolution: first the still â€” frame one â€” so something correct is up fast,
    /// then the whole frame set, which swaps in without touching layout because it
    /// has the very same canvas size.
    fn accept(&mut self, ctx: &egui::Context, decoded: Decoded) {
        let name = format!("image-{}", self.texture_generation);
        let stage = decoded.stage;
        let animation = decoded.animation.clone();
        let svg = decoded.svg.clone();
        let decoded_size = decoded.size();
        self.stage = Some(stage);
        self.texture = Some(texture::upload(ctx, &name, &decoded));
        self.error = None;

        // Fit the new image, once. Not on the full decode that follows a preview,
        // which would throw away a zoom set while it loaded.
        if self.awaiting_first_frame {
            self.awaiting_first_frame = false;
            self.view.reset();
        }

        // Editing needs the real pixels; a thumbnail stand-in would produce a
        // preview at the wrong resolution and an export at the wrong one entirely.
        if stage == Stage::Full {
            // The nav trace reports one line per image shown, and the frame set
            // that follows a still belongs to that same arrival â€” `source` being
            // set already says this image made it to the screen once.
            if self.source.is_none() {
                self.nav.arrived(self.path.as_deref());
            }
            self.source = Some(Arc::clone(&decoded.pixels));

            // Into the cache as well, so stepping back to it is as free as stepping
            // forward. Re-storing something that came from there is not wasted: it
            // renews the entry, which is what stops the image on screen being the
            // next one evicted.
            if let (Some(path), Some(stamp)) = (self.path.clone(), self.stamp.clone()) {
                self.prefetch.store(path, stamp, decoded);
            }
        }

        // An animated decode starts playing the moment it lands; anything else
        // leaves playback parked. Frame zero is what was just uploaded — `pixels`
        // *is* frame zero — so arming the deadline is all starting takes.
        self.animation = animation;
        self.anim_frame = 0;
        if let Some(anim) = self.animation.as_ref() {
            self.anim_playing = true;
            self.anim_due = Some(Instant::now() + anim.delays[0]);
        } else {
            self.anim_playing = false;
            self.anim_due = None;
        }

        // A vector file hands over its parsed artwork along with the first
        // rasterisation of it; everything else has nothing to re-render.
        let (width, height) = decoded_size;
        self.vector.adopt(svg, width.max(height));
    }

    fn poll_decode(&mut self, ctx: &egui::Context) {
        loop {
            match self.rx.try_recv() {
                Ok(LoadMessage::Loaded(decoded)) => self.accept(ctx, decoded),
                Ok(LoadMessage::Failed(message)) => {
                    self.error = Some(message);
                    self.loading = false;
                    // Now the previous image does have to go: leaving it up beside
                    // the new filename would claim to be a file that would not open.
                    if self.awaiting_first_frame {
                        self.awaiting_first_frame = false;
                        self.texture = None;
                        self.view.reset();
                    }
                }
                Err(TryRecvError::Empty) => break,
                // Sender gone with nothing more to say â€” the decode finished.
                // `loading` means "a decode channel is still alive", so it ends
                // here and only here: an animated file sends a second message
                // after its first full decode, and treating either message as the
                // last would strand the frame set in the channel.
                Err(TryRecvError::Disconnected) => {
                    self.loading = false;
                    break;
                }
            }
        }
    }

    /// Swap the texture over to animation frame `index` without re-fitting.
    ///
    /// Also moves `source` onto that frame, which is what makes edits, copy and
    /// save act on the picture actually on screen rather than on whichever frame
    /// was showing when the file opened.
    fn display_frame(&mut self, ctx: &egui::Context, index: usize) {
        let Some(anim) = self.animation.as_ref() else {
            return;
        };
        let index = index.min(anim.len() - 1);
        self.anim_frame = index;
        let frame = Arc::clone(&anim.frames[index]);

        if let Some(tex) = self.texture.as_mut() {
            texture::set_pixels(ctx, tex, &frame);
        }
        self.source = Some(frame);
    }

    /// Pause playback. The frame on screen stays until the user does something.
    fn pause_animation(&mut self) {
        self.anim_playing = false;
        self.anim_due = None;
    }

    /// Resume (or start) playback from the frame currently on screen.
    fn play_animation(&mut self, ctx: &egui::Context) {
        let Some(anim) = self.animation.as_ref() else {
            return;
        };
        if anim.len() < 2 {
            return;
        }
        self.anim_playing = true;
        let delay = anim.delays[self.anim_frame.min(anim.len() - 1)];
        self.anim_due = Some(Instant::now() + delay);
        ctx.request_repaint_after(delay);
    }

    /// Advance one frame when its time is up, and keep frames coming until it is.
    ///
    /// The same shape as `tick_slideshow`: schedule a repaint for the deadline and
    /// do the work when it fires, so a playing animation costs no frames beyond
    /// its own frame rate and a paused one costs none at all.
    fn tick_animation(&mut self, ctx: &egui::Context) {
        if !self.anim_playing || self.scrubbing {
            return;
        }
        let Some(due) = self.anim_due else {
            return;
        };

        let now = Instant::now();
        if now < due {
            ctx.request_repaint_after(due - now);
            return;
        }

        let Some(anim) = self.animation.as_ref() else {
            return;
        };
        let count = anim.len();
        let next = (self.anim_frame + 1) % count.max(1);
        let delay = anim.delays[next.min(count - 1)];
        self.display_frame(ctx, next);
        self.anim_due = Some(now + delay);
        ctx.request_repaint_after(delay);
    }

    fn apply_timeline(&mut self, ctx: &egui::Context, action: timeline::Action) {
        match action {
            timeline::Action::TogglePlay => {
                if self.anim_playing {
                    self.pause_animation();
                } else {
                    self.play_animation(ctx);
                }
            }
            timeline::Action::ScrubStart => {
                self.scrubbing = true;
                self.playing_before_scrub = self.anim_playing;
                self.pause_animation();
            }
            timeline::Action::Scrub(index) => {
                self.pause_animation();
                self.display_frame(ctx, index);
            }
            timeline::Action::ScrubEnd => {
                self.scrubbing = false;
                if self.playing_before_scrub {
                    self.play_animation(ctx);
                }
            }
        }
    }

    fn push_op(&mut self, ctx: &egui::Context, op: Op) {
        self.edits.push(op);
        self.reapply(ctx);
    }

    fn undo(&mut self, ctx: &egui::Context) {
        if self.edits.undo() {
            self.reapply(ctx);
        }
    }

    fn redo(&mut self, ctx: &egui::Context) {
        if self.edits.redo() {
            self.reapply(ctx);
        }
    }

    /// Record what the sliders now show, so it can be undone.
    ///
    /// Called when a slider is let go rather than as it moves. Nothing happens if the
    /// value is already the one on the stack â€” clicking a slider without moving it
    /// should not add a step to undo through.
    fn commit_adjust(&mut self, ctx: &egui::Context) {
        if self.adjust == self.edits.adjust() {
            return;
        }
        self.edits.push(Op::Adjust(self.adjust));

        // No `reapply`: the texture is deliberately free of colour, so a committed
        // adjustment changes nothing about it. This is only the undo stack catching
        // up with what the screen has been showing all along.
        ctx.request_repaint();
    }

    /// Re-run the whole pipeline from the original pixels, off the UI thread.
    ///
    /// From the original every time rather than incrementally, because that is what
    /// makes undo free: there is no inverse of an op to compute, only a shorter list
    /// to run. These are memcpy-class operations, so on a worker thread even a 24MP
    /// image reruns faster than the click that asked for it feels.
    fn reapply(&mut self, ctx: &egui::Context) {
        let Some(source) = self.source.clone() else {
            return;
        };

        // Geometry edits bake into the texture, which playback would overwrite
        // with unedited frames on its next advance â€” so an op pauses the film.
        // (Colour is different: the shader applies it at draw time, so sliders
        // keep working live over a playing animation.)
        self.pause_animation();
        let edits = self.edits.clone();

        // The sliders follow the stack, which is what makes undoing an adjustment
        // move them back rather than leave them describing an image that has changed
        // underneath them.
        self.adjust = edits.adjust();

        // Fresh channel per run: an earlier pipeline still finishing has nowhere to
        // deliver, so it cannot overwrite the newer result.
        let (tx, rx) = std::sync::mpsc::channel();
        self.edit_rx = rx;
        self.applying = true;
        self.texture_generation += 1;

        std::thread::Builder::new()
            .name("edit".to_owned())
            .spawn(move || {
                // Geometry only. Colour is the shader's job, every frame, for free.
                let _ = tx.send(edits.apply_geometry(&source).into_owned());
            })
            .expect("failed to spawn edit thread");

        ctx.request_repaint();
    }

    fn poll_edit(&mut self, ctx: &egui::Context) {
        match self.edit_rx.try_recv() {
            Ok(pixels) => {
                let name = format!("image-{}", self.texture_generation);
                let resized =
                    self.texture.as_ref().map(|t| t.source_size) != Some(pixels.dimensions());

                self.texture = Some(texture::upload_pixels(ctx, &name, &pixels));
                // Only when the canvas changed shape. Re-fitting after a flip would
                // throw away a zoom the user had set for no visible reason.
                if resized {
                    self.view.reset();
                }
                self.applying = false;
            }
            Err(TryRecvError::Empty) => {}
            // The worker vanished without sending; nothing is coming.
            Err(TryRecvError::Disconnected) => self.applying = false,
        }
    }

    /// The shortcuts that keep working while a widget holds the keyboard.
    ///
    /// Split out so the focused case cannot quietly drift from the unfocused one:
    /// both call this, and only the plain keys differ between them.
    #[allow(clippy::too_many_arguments)]
    fn apply_modified(
        &mut self,
        ctx: &egui::Context,
        open: bool,
        open_folder: bool,
        undo: bool,
        redo: bool,
        save: bool,
        copy_image: bool,
    ) {
        if open {
            self.prompt_for_file(ctx);
        }
        if open_folder {
            self.prompt_for_folder(ctx);
        }
        if undo {
            self.undo(ctx);
        }
        if redo {
            self.redo(ctx);
        }
        if save {
            self.save(ctx);
        }
        if copy_image && self.source.is_some() {
            self.copy_image(ctx);
        }
    }

    /// Turn this frame's key presses into actions.
    ///
    /// Everything is read through `consume_key`, which matches against the modifiers
    /// recorded on the key event itself rather than the ones still held when the
    /// frame is assembled. Those differ more often than it sounds: a quick Ctrl+S can
    /// be pressed and fully released inside one frame, and `input.modifiers` â€” a
    /// snapshot of the end of that frame â€” then reports nothing held at all, so the
    /// shortcut silently does nothing. Consuming also means a widget later in the
    /// frame cannot act on the same press a second time.
    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        use egui::{Key, Modifiers};

        const NONE: Modifiers = Modifiers::NONE;
        const CTRL: Modifiers = Modifiers::CTRL;
        const CTRL_SHIFT: Modifiers = Modifiers::CTRL.plus(Modifiers::SHIFT);

        // Order matters here: a pattern only demands the modifiers it names, so
        // Ctrl+O matches a Ctrl+Shift+O press too. Consuming the more specific
        // shortcut first is what keeps the two apart.
        let (open_folder, redo, open, undo, save) = ctx.input_mut(|i| {
            (
                i.consume_key(CTRL_SHIFT, Key::O),
                // Bitwise, not short-circuiting: both spellings of redo have to be
                // consumed, or the unread one is left for something else to act on.
                i.consume_key(CTRL_SHIFT, Key::Z) | i.consume_key(CTRL, Key::Y),
                i.consume_key(CTRL, Key::O),
                i.consume_key(CTRL, Key::Z),
                i.consume_key(CTRL, Key::S),
            )
        });

        // The clipboard shortcuts cannot be read the way everything above is, and
        // that is a property of the framework rather than a choice. egui-winit
        // intercepts them before any key event exists: Ctrl+C becomes `Event::Copy`
        // and returns, Ctrl+V becomes `Event::Paste` â€” but only when the clipboard
        // holds *text*, so an image on it produces no event at all. Its test is
        // `command && key == C`, with shift neither required nor excluded, which is
        // also why the `Ctrl+Shift+C` this app used to bind for copy-path could
        // never once have fired.
        //
        // So Ctrl+C arrives as `Event::Copy` and copies the image, which is what
        // Ctrl+C means in a viewer. Paste answers to `Event::Paste` when egui sends
        // one, and to a plain `V` always â€” the only spelling that survives, since
        // every Ctrl+V variant is swallowed whether or not anything comes back.
        // Copy-path moves to plain `P` for the same reason, beside its toolbar
        // button, which was doing all the work already.
        let (copy_image, pasted_text) = ctx.input(|i| {
            (
                i.events.iter().any(|e| matches!(e, egui::Event::Copy)),
                i.events.iter().any(|e| matches!(e, egui::Event::Paste(_))),
            )
        });

        // A focused widget owns the plain keys, so everything below this point waits.
        // The colour sliders are the first things in this app to take keyboard focus,
        // and without this an arrow key meant to nudge brightness would step to the
        // next image instead â€” this handler runs before any widget is drawn, so it
        // would win every time. Escape belongs to the widget too: egui uses it to
        // leave one, and closing the window out from under somebody tabbing through
        // sliders is not what they asked for.
        //
        // Modified shortcuts are unaffected. Ctrl+S means save wherever focus is.
        if ctx.memory(|memory| memory.focused().is_some()) {
            self.apply_modified(ctx, open, open_folder, undo, redo, save, copy_image);
            return;
        }

        let (copy_path, paste) = ctx.input_mut(|i| {
            (
                i.consume_key(NONE, Key::P),
                i.consume_key(NONE, Key::V) | pasted_text,
            )
        });

        let (escape, fullscreen, delete, prev, next, rotate, sidebar, slideshow, start_crop) = ctx
            .input_mut(|i| {
                (
                    i.consume_key(NONE, Key::Escape),
                    i.consume_key(NONE, Key::F11),
                    i.consume_key(NONE, Key::Delete),
                    i.consume_key(NONE, Key::ArrowLeft),
                    i.consume_key(NONE, Key::ArrowRight),
                    i.consume_key(NONE, Key::R),
                    i.consume_key(NONE, Key::E),
                    i.consume_key(NONE, Key::Space),
                    i.consume_key(NONE, Key::C),
                )
            });
        let (enter, info, settings_key) = ctx.input_mut(|i| {
            (
                i.consume_key(NONE, Key::Enter),
                i.consume_key(NONE, Key::I),
                i.consume_key(NONE, Key::S),
            )
        });

        if escape {
            // Escape means "back out of where I am", and cropping is the innermost
            // place to be.
            if self.crop.is_some() {
                self.crop = None;
                return;
            }
            if self.settings_open {
                self.settings_open = false;
                return;
            }
            if self.fullscreen {
                self.set_fullscreen(ctx, false);
            } else {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            return;
        }
        if open {
            self.prompt_for_file(ctx);
            return;
        }
        if open_folder {
            self.prompt_for_folder(ctx);
            return;
        }
        // Above the guard below: pasting is how an empty window stops being empty,
        // so it is one of the few things that has to work with nothing open.
        if paste {
            self.paste(ctx);
            return;
        }

        if self.texture.is_none() {
            return;
        }

        // Cropping is modal: the keys that would step to another image or push
        // another transform belong to the selection until it is committed.
        if self.crop.is_some() {
            if enter {
                self.apply_crop(ctx);
            }
            return;
        }

        if next {
            self.step(ctx, true);
        } else if prev {
            self.step(ctx, false);
        }

        if fullscreen {
            self.set_fullscreen(ctx, !self.fullscreen);
        }
        if copy_path {
            self.copy_path();
        }
        if copy_image {
            self.copy_image(ctx);
        }
        if delete {
            self.delete_current(ctx);
        }
        if rotate {
            // The same op the sidebar's rotate button pushes â€” there is exactly one
            // rotation in this app, and it is an edit.
            self.push_op(ctx, Op::RotateCw);
        }
        if sidebar {
            self.toggle_sidebar();
        }
        if info {
            self.toggle_info();
        }
        if settings_key {
            self.toggle_settings();
        }
        // Space means "stop/start the thing that is moving": an animation's
        // playback when there is one, otherwise the slideshow. The two never run
        // as one â€” stepping to a still mid-slideshow keeps the slideshow, and an
        // animated file plays whether or not it has neighbours.
        if slideshow {
            if self.animation.is_some() {
                if self.anim_playing {
                    self.pause_animation();
                } else {
                    self.play_animation(ctx);
                }
            } else if self.has_neighbours() {
                self.toggle_slideshow();
            }
        }
        if start_crop {
            self.start_crop();
        }
        if undo {
            self.undo(ctx);
        }
        if redo {
            self.redo(ctx);
        }
        if save {
            self.save(ctx);
        }

        ctx.input_mut(|i| {
            if i.consume_key(NONE, Key::F) || i.consume_key(NONE, Key::Num0) {
                self.view.reset();
            }
            if i.consume_key(NONE, Key::Num1) {
                self.view.set_zoom(1.0);
            }
            if i.consume_key(NONE, Key::Plus) || i.consume_key(NONE, Key::Equals) {
                self.view.zoom_by(1.25, self.last_zoom);
            }
            if i.consume_key(NONE, Key::Minus) {
                self.view.zoom_by(1.0 / 1.25, self.last_zoom);
            }
        });
    }

    /// Apply what the settings panel asked for.
    ///
    /// The slider updates live as it drags; only its release touches the disk,
    /// so a drag from 4 to 30 is one write rather than twenty-six. A sort pick
    /// is a single deliberate click and writes at once.
    fn apply_settings(&mut self, ctx: &egui::Context, action: settings::Action) {
        match action {
            settings::Action::Close => self.settings_open = false,
            settings::Action::SetSlideshow(secs) => self.settings.slideshow_secs = secs,
            settings::Action::CommitSlideshow => {
                self.settings.save();
                self.notify(format!(
                    "Slideshow set to {} seconds per image",
                    self.settings.slideshow_secs
                ));
            }
            settings::Action::SetOrder(order) => {
                // `set_order` re-sorts the listing, re-warms the prefetch and
                // writes the settings file â€” everything a sort pick means.
                self.set_order(order);
            }
        }
        let _ = ctx;
    }

    /// Apply what the toolbar asked for.
    fn apply(&mut self, ctx: &egui::Context, action: toolbar::Action) {
        match action {
            toolbar::Action::Open => self.prompt_for_file(ctx),
            toolbar::Action::OpenFolder => self.prompt_for_folder(ctx),
            toolbar::Action::Sort(order) => self.set_order(order),
            toolbar::Action::ToggleInfo => self.toggle_info(),
            toolbar::Action::CopyPath => self.copy_path(),
            toolbar::Action::Delete => self.delete_current(ctx),
            toolbar::Action::ToggleFullscreen => self.set_fullscreen(ctx, !self.fullscreen),
            toolbar::Action::ToggleSlideshow => self.toggle_slideshow(),
            toolbar::Action::ToggleSidebar => self.toggle_sidebar(),
            toolbar::Action::ToggleSettings => self.toggle_settings(),
        }
    }

    fn apply_sidebar(&mut self, ctx: &egui::Context, action: sidebar::Action) {
        match action {
            sidebar::Action::Close => self.sidebar_open = false,
            sidebar::Action::Apply(op) => self.push_op(ctx, op),
            sidebar::Action::Undo => self.undo(ctx),
            sidebar::Action::Redo => self.redo(ctx),
            sidebar::Action::Save => self.save(ctx),
            sidebar::Action::Trim => self.request_trim(ctx),
            sidebar::Action::StartCrop => self.start_crop(),
            sidebar::Action::SetAspect(aspect) => {
                // Read before the mutable borrow: `edited_size` is a method, so it
                // borrows all of `self`.
                let size = self.edited_size();
                if let (Some(crop), Some(size)) = (self.crop.as_mut(), size) {
                    crop.set_aspect(aspect, size);
                }
            }
            sidebar::Action::ApplyCrop => self.apply_crop(ctx),
            sidebar::Action::CancelCrop => self.crop = None,
            // Nothing to do: the slider has already written its new value into
            // `self.adjust`, and the shader reads that when the frame is drawn.
            // Which is the whole point â€” a slider that costs a repaint and nothing
            // else is one that can be dragged.
            sidebar::Action::Adjusting => {}
            sidebar::Action::CommitAdjust => self.commit_adjust(ctx),
            sidebar::Action::ResetAdjust => {
                self.adjust = Adjust::NONE;
                self.commit_adjust(ctx);
            }
        }
    }

    /// Open the edit sidebar, and put the info panel away.
    ///
    /// The two are the same strip of window. Both at once would leave the
    /// photograph a column down the middle, so opening either closes the other â€”
    /// and there is no arrangement of the pair worth the width it would cost.
    fn open_sidebar(&mut self) {
        self.sidebar_open = true;
        self.info_open = false;
        self.settings_open = false;
    }

    fn toggle_sidebar(&mut self) {
        match self.sidebar_open {
            true => self.sidebar_open = false,
            false => self.open_sidebar(),
        }
    }

    fn toggle_info(&mut self) {
        self.info_open = !self.info_open;
        if self.info_open {
            self.sidebar_open = false;
            self.settings_open = false;
        }
    }

    /// Same strip, same rule as the other two panels.
    fn toggle_settings(&mut self) {
        self.settings_open = !self.settings_open;
        if self.settings_open {
            self.sidebar_open = false;
            self.info_open = false;
        }
    }

    /// Read the EXIF block of the image on screen, once per image.
    ///
    /// Called from the frame that draws the panel rather than from the load, so a
    /// session that never opens it never pays. The read itself is a file open and a
    /// few kilobytes â€” noise beside the decode it sits next to, and nothing like
    /// enough work to be worth a thread and a channel to report back on.
    fn read_info(&mut self) {
        if self.info_read {
            return;
        }
        self.info_read = true;
        self.info = match self.path.as_deref() {
            Some(path) => imaginer_core::Info::read(path),
            // Pixels from the clipboard were never a file, so there is no block to
            // read. The panel still has the File section's shape to describe.
            None => imaginer_core::Info::default(),
        };
    }

    /// Size of the image as the pipeline currently produces it, which is what a
    /// crop rectangle is measured against.
    fn edited_size(&self) -> Option<(u32, u32)> {
        self.source
            .as_ref()
            .map(|source| self.edits.size_after(source.dimensions()))
    }

    fn start_crop(&mut self) {
        if self.source.is_none() {
            return;
        }

        // Fit first: a selection can only be drawn over what is on screen, so
        // starting a crop while zoomed into a corner would silently put most of the
        // image out of reach.
        self.view.reset();
        self.open_sidebar();
        self.crop = Some(CropState::new(
            self.crop
                .as_ref()
                .map_or_else(Default::default, |c| c.aspect),
        ));
    }

    /// Measure the transparent border, off the UI thread.
    ///
    /// The answer depends on the pixels rather than on the size, so it cannot be an
    /// op of its own â€” the export panel has to be able to predict output dimensions
    /// without running the pipeline. What comes back becomes an ordinary crop, which
    /// keeps the stack honest about what happened and undoes like anything else.
    fn request_trim(&mut self, ctx: &egui::Context) {
        let Some(source) = self.source.clone() else {
            return;
        };
        let edits = self.edits.clone();

        let (tx, rx) = std::sync::mpsc::channel();
        self.trim_rx = rx;
        self.trimming = true;

        std::thread::Builder::new()
            .name("trim".to_owned())
            .spawn(move || {
                let pixels = edits.apply(&source);
                let _ = tx.send(imaginer_core::edit::opaque_bounds(&pixels));
            })
            .expect("failed to spawn trim thread");

        ctx.request_repaint();
    }

    fn poll_trim(&mut self, ctx: &egui::Context) {
        let bounds = match self.trim_rx.try_recv() {
            Ok(bounds) => bounds,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                self.trimming = false;
                return;
            }
        };
        self.trimming = false;

        let current = self.edited_size();
        match bounds {
            // Nothing transparent to take away. Saying so beats pushing a crop that
            // changes nothing and still costs an undo to get rid of.
            Some((0, 0, w, h)) if Some((w, h)) == current => self.notify("Nothing to trim"),
            Some((x, y, width, height)) => self.push_op(
                ctx,
                Op::Crop {
                    x,
                    y,
                    width,
                    height,
                },
            ),
            None => self.notify("The whole image is transparent"),
        }
    }

    fn apply_crop(&mut self, ctx: &egui::Context) {
        let Some((x, y, width, height)) = self.crop.as_ref().and_then(CropState::rectangle) else {
            return;
        };

        self.crop = None;
        self.push_op(
            ctx,
            Op::Crop {
                x,
                y,
                width,
                height,
            },
        );
    }

    /// Write the edited pixels out.
    ///
    /// Save-as whenever the result would not simply take the original's place â€” a
    /// different format or a different size is a new file, and quietly replacing
    /// the original with it is how originals get lost.
    fn save(&mut self, ctx: &egui::Context) {
        let Some(source) = self.source.clone() else {
            return;
        };

        let target = match self.path.clone() {
            Some(path) => {
                let converting =
                    imaginer_core::Format::from_path(&path) != Some(self.export.format);
                let resizing = self.export.scale_percent != 100;

                if converting || resizing {
                    let Some(target) = self.prompt_for_save_path(&path, converting) else {
                        return;
                    };
                    target
                } else {
                    path
                }
            }
            // Pasted pixels: there is no original to take the place of, so this is
            // always a new file and always asks where to put it.
            None => {
                let Some(target) = self.prompt_for_new_file() else {
                    return;
                };
                target
            }
        };

        // Off the UI thread: re-running the pipeline and then encoding a 24MP PNG is
        // seconds of work, and a window that stops responding while it happens reads
        // as a crash.
        let edits = self.edits.clone();
        let settings = self.export;
        let (tx, rx) = std::sync::mpsc::channel();
        self.save_rx = rx;
        self.saving = true;

        std::thread::Builder::new()
            .name("save".to_owned())
            .spawn(move || {
                // The very same pipeline the preview ran, over the very same pixels,
                // which is what makes the file match what was on screen.
                let pixels = edits.apply(&source);
                let result = imaginer_core::export::write(&pixels, &target, &settings)
                    .map(|()| target)
                    .map_err(|err| err.to_string());
                let _ = tx.send(result);
            })
            .expect("failed to spawn save thread");

        ctx.request_repaint();
    }

    fn prompt_for_save_path(&self, original: &Path, converting: bool) -> Option<PathBuf> {
        let format = self.export.format;
        let stem = original
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "image".to_owned());

        // Converting produces a name that cannot collide with the original, so it
        // keeps the original's. Anything else would land on top of it, so it does
        // not â€” the shell will still ask before overwriting, but a suggestion that
        // aims at your own source file is a poor one.
        let suggested = if converting {
            format!("{stem}.{}", format.extension())
        } else {
            format!("{stem} (edited).{}", format.extension())
        };

        let mut dialog = rfd::FileDialog::new()
            .add_filter(format.label(), &[format.extension()])
            .set_file_name(suggested);
        if let Some(dir) = original.parent() {
            dialog = dialog.set_directory(dir);
        }

        dialog.save_file()
    }

    /// Where to write an image that has never been a file.
    ///
    /// No starting directory: there is no original to sit beside, and the dialog's
    /// own memory of where the user last saved something is a better guess than
    /// anything this app could invent.
    fn prompt_for_new_file(&self) -> Option<PathBuf> {
        let format = self.export.format;
        rfd::FileDialog::new()
            .add_filter(format.label(), &[format.extension()])
            .set_file_name(format!("pasted.{}", format.extension()))
            .save_file()
    }

    fn poll_save(&mut self, ctx: &egui::Context) {
        match self.save_rx.try_recv() {
            Ok(Ok(target)) => {
                self.saving = false;
                let name = target
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| target.display().to_string());

                // Saving over the image being viewed means the file now matches the
                // screen, so reload it: the edit stack is spent, and the size on
                // disk has changed.
                if self.path.as_deref() == Some(target.as_path()) {
                    self.show(ctx, target);
                }
                self.notify(format!("Saved {name}"));
            }
            Ok(Err(message)) => {
                self.saving = false;
                self.error = Some(message);
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.saving = false,
        }
    }

    /// Open the shell's own file dialog.
    ///
    /// It blocks the event loop while it is up, which is exactly what a modal
    /// dialog is: there is nothing to repaint behind it and nothing else to do.
    fn prompt_for_file(&mut self, ctx: &egui::Context) {
        let mut dialog =
            rfd::FileDialog::new().add_filter("Images", imaginer_core::SUPPORTED_EXTENSIONS);

        // Start where the current image lives; browsing usually continues from
        // wherever you already are.
        if let Some(dir) = self.path.as_deref().and_then(|p| p.parent()) {
            dialog = dialog.set_directory(dir);
        }

        if let Some(path) = dialog.pick_file() {
            self.open(ctx, path);
        }
    }

    /// Pick a folder and show the first image in it.
    ///
    /// The listing is built here rather than lazily, because it is the whole point
    /// of the action â€” the user asked for the folder, not for one file in it.
    fn prompt_for_folder(&mut self, ctx: &egui::Context) {
        let mut dialog = rfd::FileDialog::new();
        if let Some(dir) = self.path.as_deref().and_then(|p| p.parent()) {
            dialog = dialog.set_directory(dir);
        }

        let Some(dir) = dialog.pick_folder() else {
            return;
        };

        let folder = imaginer_core::Folder::of_directory(&dir, self.order);
        let Some(first) = folder.current().map(Path::to_path_buf) else {
            self.error = Some(format!(
                "No images this build can open in {}",
                dir.display()
            ));
            return;
        };

        self.folder = Some(folder);
        self.show(ctx, first);
    }

    fn copy_path(&mut self) {
        let Some(path) = self.path.as_deref() else {
            return;
        };

        match clipboard::copy_text(path.display().to_string()) {
            Ok(()) => self.notify("Path copied"),
            Err(message) => self.error = Some(message),
        }
    }

    /// Put the image as it now looks on the clipboard.
    ///
    /// The edited pixels, not the file on disk. What Ctrl+C means in a viewer is
    /// "the thing I am looking at" â€” rotating an image and then pasting the
    /// unrotated original elsewhere is the wrong kind of surprise.
    fn copy_image(&mut self, ctx: &egui::Context) {
        let Some(source) = self.source.clone() else {
            return;
        };
        let edits = self.edits.clone();
        let tx = self.start_clipboard_job();

        // Off the UI thread: the pipeline runs and then the whole buffer is rewritten
        // into the bitmap layout Windows wants, which for a 24MP photograph is a
        // couple of hundred milliseconds of a window that would otherwise not draw.
        std::thread::Builder::new()
            .name("clipboard".to_owned())
            .spawn(move || {
                let pixels = edits.apply(&source);
                let _ = tx.send(match clipboard::copy_image(&pixels) {
                    Ok(()) => ClipboardResult::Copied,
                    Err(message) => ClipboardResult::Failed(message),
                });
            })
            .expect("failed to spawn clipboard thread");

        ctx.request_repaint();
    }

    /// Show whatever is on the clipboard.
    fn paste(&mut self, ctx: &egui::Context) {
        let tx = self.start_clipboard_job();

        // Also off the UI thread, for the same reason in reverse: reading a
        // full-screen screenshot back is a copy of somebody else's megabytes.
        std::thread::Builder::new()
            .name("clipboard".to_owned())
            .spawn(move || {
                let _ = tx.send(match clipboard::paste() {
                    Ok(pasted) => ClipboardResult::Pasted(pasted),
                    Err(message) => ClipboardResult::Failed(message),
                });
            })
            .expect("failed to spawn clipboard thread");

        ctx.request_repaint();
    }

    /// A fresh channel per clipboard job, so one the user has already moved past has
    /// nowhere to deliver. Copy and paste share it: neither is worth waiting for
    /// while the other runs, and the newer request is always the one meant.
    fn start_clipboard_job(&mut self) -> std::sync::mpsc::Sender<ClipboardResult> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.clipboard_rx = rx;
        self.clipboard_busy = true;
        tx
    }

    fn poll_clipboard(&mut self, ctx: &egui::Context) {
        let result = match self.clipboard_rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            // The worker vanished without sending; nothing is coming.
            Err(TryRecvError::Disconnected) => {
                self.clipboard_busy = false;
                return;
            }
        };
        self.clipboard_busy = false;

        match result {
            ClipboardResult::Copied => self.notify("Image copied"),
            // A path goes through `open` rather than `show`: it names a file in some
            // folder, and the images beside it are as much a part of opening it as
            // the pixels are.
            ClipboardResult::Pasted(clipboard::Pasted::Path(path)) => self.open(ctx, path),
            ClipboardResult::Pasted(clipboard::Pasted::Image(pixels)) => {
                self.show_pasted(ctx, pixels)
            }
            ClipboardResult::Failed(message) => self.error = Some(message),
        }
    }

    /// Send the current file to the Recycle Bin.
    ///
    /// Not guarded by a confirmation dialog, on purpose: the Recycle Bin *is* the
    /// confirmation, and a prompt on every delete is what teaches people to click
    /// through prompts. The toolbar keeps the button away from the ones that get
    /// clicked constantly instead.
    fn delete_current(&mut self, ctx: &egui::Context) {
        let Some(path) = self.path.clone() else {
            return;
        };

        // Build the listing while the file is still on disk. Afterwards it would no
        // longer be found in its own directory, and the step to the next image
        // would have nothing to aim at.
        self.folder();

        if let Err(err) = trash::delete(&path) {
            self.error = Some(format!("Could not delete: {err}"));
            return;
        }

        // Hand the budget back. The stamp check would refuse to serve these pixels
        // anyway, but only once somebody asked â€” and nobody will, because the file
        // is gone. Left alone they would sit there evicting images that still exist.
        self.prefetch.forget(&path);

        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());

        // Straight on to the next image: clearing out a run of photos is a
        // sequence, and being dropped on an empty screen after each one breaks it.
        match self.folder().remove(&path) {
            Some(next) => self.show(ctx, next),
            None => {
                self.path = None;
                self.texture = None;
                self.stage = None;
                self.stamp = None;
                self.loading = false;
                self.animation = None;
                self.anim_playing = false;
                self.anim_due = None;
                self.view.reset();
            }
        }

        self.notify(format!("{name} moved to the Recycle Bin"));
    }

    fn set_fullscreen(&mut self, ctx: &egui::Context, fullscreen: bool) {
        self.fullscreen = fullscreen;
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(fullscreen));
    }

    /// Whether there is more than one image to move between.
    ///
    /// `false` while the listing is still unbuilt, which is only ever the first
    /// frame â€” chrome that offers to step somewhere there is nowhere to step is
    /// worse than chrome that appears a frame late.
    fn has_neighbours(&self) -> bool {
        self.folder.as_ref().is_some_and(|folder| folder.len() > 1)
    }

    fn toggle_slideshow(&mut self) {
        let duration = Duration::from_secs(u64::from(self.settings.slideshow_secs));
        self.slideshow = match self.slideshow {
            Some(_) => None,
            None => Some(Instant::now() + duration),
        };
    }

    /// Advance the slideshow when its time is up, and keep frames coming until it is.
    fn tick_slideshow(&mut self, ctx: &egui::Context) {
        let Some(due) = self.slideshow else {
            return;
        };

        let now = Instant::now();
        if now < due {
            ctx.request_repaint_after(due - now);
            return;
        }

        self.slideshow = Some(now + Duration::from_secs(u64::from(self.settings.slideshow_secs)));
        self.step(ctx, true);
    }

    /// Stop the slideshow the moment the user does anything deliberate.
    ///
    /// Keys and the scroll wheel, not pointer movement: a slideshow that stopped
    /// because the mouse was nudged would be unusable. Space is the exception,
    /// because it is the key that toggles the thing â€” letting it count as an
    /// interruption would stop the slideshow here and immediately restart it in the
    /// shortcut handler a few lines later.
    fn interrupt_slideshow(&mut self, ctx: &egui::Context) {
        if self.slideshow.is_none() {
            return;
        }

        let interrupted = ctx.input(|i| {
            i.smooth_scroll_delta != egui::Vec2::ZERO
                || i.events.iter().any(|event| {
                    matches!(
                        event,
                        egui::Event::Key { key, pressed: true, .. } if *key != egui::Key::Space
                    // Ctrl+C and Ctrl+V never arrive as key presses, so without
                    // these two a slideshow would carry on running underneath a
                    // copy â€” and the next slide would land before the worker had
                    // read the image the user meant.
                    ) || matches!(event, egui::Event::Copy | egui::Event::Paste(_))
                })
        });

        if interrupted {
            self.slideshow = None;
        }
    }

    fn notify(&mut self, text: impl Into<String>) {
        self.error = None;
        self.notice = Some(Notice {
            text: text.into(),
            expires_at: Instant::now() + NOTICE_DURATION,
        });
    }

    /// Drop an expired notice, and keep frames coming until it expires.
    ///
    /// egui is event-driven, so without the scheduled repaint a notice would sit
    /// there until the next mouse movement rather than fading on its own.
    fn tick_notice(&mut self, ctx: &egui::Context) {
        let Some(notice) = self.notice.as_ref() else {
            return;
        };

        let now = Instant::now();
        if now >= notice.expires_at {
            self.notice = None;
        } else {
            ctx.request_repaint_after(notice.expires_at - now);
        }
    }

    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped = ctx.input(|i| i.raw.dropped_files.iter().find_map(|f| f.path.clone()));

        if let Some(path) = dropped {
            self.open(ctx, path);
        }
    }
}

impl eframe::App for App {
    /// Hand the shader's GL objects back while the context is still alive.
    fn on_exit(&mut self, gl: Option<&eframe::glow::Context>) {
        if let Some(gl) = gl {
            self.shader.destroy(gl);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        if self.loading {
            self.poll_decode(&ctx);
            // egui is event-driven and would otherwise sit idle waiting for input,
            // never noticing the decode landed. Repainting continuously is scoped
            // to the load so idle CPU stays at zero the rest of the time.
            ctx.request_repaint();
        }
        if self.applying {
            self.poll_edit(&ctx);
            ctx.request_repaint();
        }
        if self.saving {
            self.poll_save(&ctx);
            ctx.request_repaint();
        }
        if self.trimming {
            self.poll_trim(&ctx);
            ctx.request_repaint();
        }
        if self.clipboard_busy {
            self.poll_clipboard(&ctx);
            ctx.request_repaint();
        }

        self.tick_notice(&ctx);
        // Before the shortcut handler, which consumes the very key presses that
        // ought to stop a slideshow.
        self.interrupt_slideshow(&ctx);
        self.handle_dropped_files(&ctx);
        self.handle_shortcuts(&ctx);
        // After the shortcuts: a Space that pauses must not be immediately
        // followed by a tick that plays again in the same frame.
        self.tick_animation(&ctx);
        self.tick_slideshow(&ctx);

        // Fullscreen means the photograph, not a photograph with a toolbar over it.
        // The chrome comes back the moment the pointer moves.
        let chrome = idle::visible(&ctx);
        let show_chrome = !self.fullscreen || chrome;

        let base_frame = egui::Frame::side_top_panel(ui.style());
        let toolbar_frame = base_frame.inner_margin(egui::Margin::symmetric(10, 7));
        let status_frame = base_frame.inner_margin(egui::Margin::symmetric(10, 5));

        // Read before the panels borrow `self.icons`: it is a method, so it borrows
        // all of `self`, which a disjoint field access would not.
        let has_neighbours = self.has_neighbours();

        let mut requested = None;
        if show_chrome {
            egui::Panel::top(egui::Id::new("toolbar"))
                .frame(toolbar_frame)
                .show(ui, |ui| {
                    requested = toolbar::show(
                        ui,
                        &mut self.icons,
                        &mut self.view,
                        toolbar::Bar {
                            has_image: self.texture.is_some(),
                            fullscreen: self.fullscreen,
                            slideshow: self.slideshow.is_some(),
                            has_neighbours,
                            sidebar_open: self.sidebar_open,
                            info_open: self.info_open,
                            settings_open: self.settings_open,
                            order: self.order,
                        },
                    );
                });

            egui::Panel::bottom(egui::Id::new("status"))
                .frame(status_frame)
                .show(ui, |ui| {
                    statusbar::show(
                        ui,
                        &statusbar::Status {
                            path: self.path.as_deref(),
                            texture: self.texture.as_ref(),
                            stage: self.stage,
                            zoom: self.last_zoom,
                            file_size: self.stamp.as_ref().map(Stamp::file_size),
                            position: self.folder.as_ref().and_then(Folder::position),
                            order: self.order,
                            error: self.error.as_deref(),
                            notice: self.notice.as_ref().map(|n| n.text.as_str()),
                        },
                    );
                });
        }

        // Only over an image: with nothing on screen every row would be blank, and
        // the panel would describe a file that is not open. Emptying the window â€”
        // deleting the last image in a folder â€” therefore puts it away by itself,
        // and opening another brings it back.
        let mut info_action = None;
        if self.info_open && self.texture.is_some() {
            // Before the panel borrows anything, and only while it is open.
            self.read_info();

            egui::Panel::right(egui::Id::new("info"))
                .exact_size(info::WIDTH)
                .resizable(false)
                .frame(egui::Frame::NONE.fill(theme::PANEL_BG))
                .show(ui, |ui| {
                    info_action = info::show(
                        ui,
                        &mut self.icons,
                        &info::State {
                            file: info::file_facts(
                                self.path.as_deref(),
                                self.stamp.as_ref().map(Stamp::file_size),
                                // The decoded original, not the texture: this
                                // section describes the file, and a rotate the user
                                // has not saved has not changed what is on disk.
                                self.source.as_ref().map(|source| source.dimensions()),
                            ),
                            exif: &self.info,
                            ready: !self.loading,
                        },
                    );
                });
        }

        // After the top and bottom panels so it sits between them, and before the
        // central panel so the canvas gets whatever is left.
        let mut sidebar_action = None;
        if self.sidebar_open {
            egui::Panel::right(egui::Id::new("sidebar"))
                .exact_size(sidebar::WIDTH)
                .resizable(false)
                .frame(egui::Frame::NONE.fill(theme::PANEL_BG))
                .show(ui, |ui| {
                    sidebar_action = sidebar::show(
                        ui,
                        &mut self.icons,
                        &mut sidebar::State {
                            edits: &self.edits,
                            adjust: &mut self.adjust,
                            settings: &mut self.export,
                            edited_size: self
                                .source
                                .as_ref()
                                .map(|source| self.edits.size_after(source.dimensions())),
                            source_format: self
                                .path
                                .as_deref()
                                .and_then(imaginer_core::Format::from_path),
                            has_image: self.source.is_some(),
                            cropping: self.crop.as_ref().map(|crop| sidebar::Cropping {
                                aspect: crop.aspect,
                                selection: crop.rectangle().map(|(_, _, w, h)| (w, h)),
                            }),
                        },
                    );
                });
        }

        // Unlike the info panel and the sidebar, the settings panel is useful with
        // nothing open â€” the shortcuts are a reference, the preferences are about
        // the app â€” so it does not wait for an image.
        let mut settings_action = None;
        if self.settings_open {
            egui::Panel::right(egui::Id::new("settings"))
                .exact_size(settings::WIDTH)
                .resizable(false)
                .frame(egui::Frame::NONE.fill(theme::PANEL_BG))
                .show(ui, |ui| {
                    settings_action = settings::show(
                        ui,
                        &mut self.icons,
                        &mut settings::State {
                            slideshow_secs: &mut self.settings.slideshow_secs,
                            order: self.order,
                        },
                    );
                });
        }

        let mut requested_step = None;
        let mut requested_timeline = None;
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(theme::CANVAS_BG))
            .show(ui, |ui| {
                if let Some(texture) = self.texture.as_ref() {
                    let canvas = ui.max_rect();
                    if texture.has_transparency {
                        self.checker
                            .get_or_insert_with(|| texture::checkerboard(&ctx));
                    }
                    let shown = viewer::show(
                        ui,
                        texture,
                        &mut self.view,
                        self.crop.is_none(),
                        viewer::Colour {
                            adjust: self.adjust,
                            shader: &self.shader,
                        },
                        self.checker.as_ref(),
                        self.vector.tile(),
                    );
                    self.last_zoom = shown.zoom;
                    self.last_look = Some(vector::Look {
                        image_rect: shown.image_rect,
                        canvas,
                        zoom: shown.zoom,
                        ppp: ctx.pixels_per_point(),
                    });

                    match self.crop.as_mut() {
                        Some(state) => {
                            crop::overlay(ui, state, texture.source_size, shown.image_rect, canvas)
                        }
                        // Chevrons would step to another image mid-crop, which is
                        // not something a half-drawn selection should survive.
                        None if has_neighbours => {
                            requested_step = viewer::chevrons(ui, &mut self.icons, canvas, chrome);
                        }
                        None => {}
                    }

                    // The playback bar belongs beside the chevrons: on the canvas,
                    // with the chrome. Cropping hides it along with everything else
                    // that acts on the image as a whole.
                    if self.crop.is_none() {
                        let count = self.animation.as_ref().map_or(0, |anim| anim.len());
                        requested_timeline = timeline::show(
                            ui,
                            &mut self.icons,
                            canvas,
                            timeline::Playback {
                                frame: self.anim_frame,
                                count,
                                playing: self.anim_playing,
                            },
                            show_chrome,
                        );
                    }
                    self.trace.mark_first_image();
                } else {
                    let logotype = self
                        .logotype
                        .get_or_insert_with(|| logo::logotype_texture(ui.ctx()));
                    empty_state(ui, logotype, self.error.as_deref());
                }
            });

        // Applied after the frame is laid out: an action can empty the window, and
        // doing that mid-frame would leave the panels above describing an image
        // that is no longer there.
        if let Some(action) = requested {
            self.apply(&ctx, action);
        }
        if let Some(action) = sidebar_action {
            self.apply_sidebar(&ctx, action);
        }
        if let Some(action) = settings_action {
            self.apply_settings(&ctx, action);
        }
        if info_action == Some(info::Action::Close) {
            self.info_open = false;
        }
        if let Some(step) = requested_step {
            self.step(&ctx, step == viewer::Step::Next);
        }
        if let Some(action) = requested_timeline {
            self.apply_timeline(&ctx, action);
        }

        // After the canvas, because everything the vector path decides comes from
        // what the canvas just drew. Resting rather than polling while an edit,
        // a crop or a decode is in flight: the picture on screen is then either
        // not the artwork's any more, or about to be replaced.
        let settled = !self.loading && self.edits.is_empty() && self.crop.is_none();
        match (settled, self.last_look, self.texture.as_mut()) {
            (true, Some(look), Some(texture)) => {
                self.vector
                    .poll(&ctx, texture, &mut self.texture_generation, look)
            }
            _ => self.vector.rest(),
        }

        self.trace.mark_first_frame();

        // Scan the folder once something is on screen. Doing it during startup would
        // put a directory walk on the critical path of an app whose whole point is
        // how fast it starts; doing it here costs a few milliseconds nobody is
        // waiting on, and means the position counter and the chevrons are there from
        // the second frame rather than from the first arrow key.
        if !self.painted {
            self.painted = true;
            if self.path.is_some() {
                self.folder();
                // Only now can prefetch know what the neighbours are. `show` asked
                // while the listing was still unbuilt and was told nothing.
                self.warm_neighbours();
                ctx.request_repaint();
            }
        }

        // Benchmark mode: leave as soon as there is something to measure.
        if self.trace.should_exit_after_first_frame()
            && (self.texture.is_some() || self.error.is_some() || self.path.is_none())
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

/// Drawn width of the wordmark. Present but quiet â€” this screen exists to be left,
/// so the logo should identify the app without turning into a splash screen.
const LOGOTYPE_DRAW_WIDTH: f32 = 210.0;

/// Gap between the wordmark and the line below it.
const LOGOTYPE_GAP: f32 = 22.0;

fn empty_state(ui: &mut egui::Ui, logotype: &egui::TextureHandle, error: Option<&str>) {
    let native = logotype.size_vec2();
    let drawn = egui::vec2(
        LOGOTYPE_DRAW_WIDTH,
        LOGOTYPE_DRAW_WIDTH * native.y / native.x,
    );

    ui.vertical_centered(|ui| {
        // Centre the block as a whole. `centered_and_justified` only centres a single
        // widget, and letting the layout stack from the top would leave the pair
        // clinging to the ceiling of a tall window.
        let block_height = drawn.y + LOGOTYPE_GAP + ui.text_style_height(&egui::TextStyle::Body);
        ui.add_space(((ui.available_height() - block_height) * 0.5).max(0.0));

        ui.add(egui::Image::new(logotype).fit_to_exact_size(drawn));
        ui.add_space(LOGOTYPE_GAP);

        match error {
            Some(error) => {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            None => {
                ui.colored_label(theme::TEXT_MUTED, "Drop an image here");
            }
        }
    });
}

/// How much memory decoded images may occupy, read from `IMAGINER_CACHE_MB`.
///
/// An environment variable rather than a setting, because there is no settings panel
/// to put it in yet and the default is the number that matters. Anything unparseable
/// falls back to the default rather than failing: a typo in a variable is no reason
/// to refuse to open a photograph.
fn cache_budget() -> usize {
    std::env::var("IMAGINER_CACHE_MB")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .map_or(imaginer_core::cache::DEFAULT_BUDGET, |mb| {
            mb.saturating_mul(1024 * 1024)
        })
}

/// Export settings for a freshly opened file.
///
/// The format defaults to the one the file is already in, so Save means save rather
/// than convert. A format this build can read but not write falls back to PNG, the
/// lossless option â€” converting is then the honest description of what will happen.
fn export_defaults(path: Option<&Path>) -> imaginer_core::ExportSettings {
    imaginer_core::ExportSettings {
        format: path
            .and_then(imaginer_core::Format::from_path)
            .unwrap_or(imaginer_core::Format::Png),
        ..Default::default()
    }
}
