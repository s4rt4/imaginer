//! Application state and the per-frame update loop.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use eframe::egui;
use imaginer_core::image::RgbaImage;
use imaginer_core::{Edits, Folder, Op, Stage};

use crate::icons::Icons;
use crate::logo;
use crate::startup::StartupTrace;
use crate::texture::{self, ImageTexture};
use crate::views::{sidebar, statusbar, toolbar, viewer};
use crate::{LoadMessage, decode_into, theme, titlebar};

/// How long a status-bar notice stays up. Long enough to read in passing, short
/// enough that it never becomes part of the furniture.
const NOTICE_DURATION: Duration = Duration::from_secs(3);

/// Confirmation of something whose only other evidence is that it worked.
struct Notice {
    text: String,
    expires_at: Instant,
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
    file_size: Option<u64>,
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
    /// The pixels exactly as decoded, kept so every edit runs from the original
    /// rather than compounding on already-edited output. Shared with the worker
    /// thread, which is the only reason it is behind an `Arc`.
    source: Option<Arc<RgbaImage>>,
    edits: Edits,
    /// Results from the edit worker. Replaced per run, so a stale result from a
    /// pipeline the user has already moved past has nowhere to land.
    edit_rx: Receiver<RgbaImage>,
    /// True while the pipeline is running, which is what keeps frames coming until
    /// the new pixels arrive.
    applying: bool,
    sidebar_open: bool,
    export: imaginer_core::ExportSettings,
    /// Where the save worker reports back: the file it wrote, or why it could not.
    save_rx: Receiver<Result<PathBuf, String>>,
    saving: bool,
    /// Uploaded the first time the empty state is drawn, so launching with an image
    /// never pays for it.
    logotype: Option<egui::TextureHandle>,
    icons: Icons,
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
        // so its export format has to be picked up here too — otherwise launching
        // straight onto a JPEG offers to save it as a PNG.
        let export = export_defaults(path.as_deref());

        Self {
            trace,
            file_size: path.as_deref().and_then(file_size),
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
            source: None,
            edits: Edits::default(),
            // A channel with no sender: `applying` is false, so nothing reads it
            // until the first edit replaces it with a live one.
            edit_rx: std::sync::mpsc::channel().1,
            applying: false,
            sidebar_open: false,
            export,
            save_rx: std::sync::mpsc::channel().1,
            saving: false,
            logotype: None,
            icons: Icons::default(),
        }
    }

    /// Open a file the user picked, and forget the folder listing so it is rebuilt
    /// around the new location the next time navigation needs it.
    fn open(&mut self, ctx: &egui::Context, path: PathBuf) {
        self.folder = None;
        self.show(ctx, path);
    }

    /// The images beside the current one, scanned on first use.
    fn folder(&mut self) -> &mut Folder {
        if self.folder.is_none() {
            self.folder = Some(match self.path.as_deref() {
                Some(path) => Folder::containing(path),
                None => Folder::default(),
            });
        }
        self.folder.as_mut().expect("just filled in")
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

    fn show(&mut self, ctx: &egui::Context, path: PathBuf) {
        // A fresh channel per open, so results from a previous load that is still
        // in flight cannot land on top of the new one.
        let (tx, rx) = std::sync::mpsc::channel();
        self.rx = rx;

        self.file_size = file_size(&path);
        self.path = Some(path.clone());
        self.texture = None;
        self.stage = None;
        self.error = None;
        self.notice = None;
        self.view.reset();
        self.loading = true;
        self.texture_generation += 1;

        // Edits belong to the image they were made on. Carrying them across would
        // silently rotate the next photo because of something done to the last one.
        self.source = None;
        self.edits.clear();
        self.applying = false;

        self.export = export_defaults(Some(&path));

        std::thread::Builder::new()
            .name("decode".to_owned())
            .spawn(move || decode_into(path, &tx))
            .expect("failed to spawn decode thread");

        ctx.request_repaint();
    }

    fn poll_decode(&mut self, ctx: &egui::Context) {
        loop {
            match self.rx.try_recv() {
                Ok(LoadMessage::Loaded(decoded)) => {
                    let name = format!("image-{}", self.texture_generation);
                    let stage = decoded.stage;
                    self.stage = Some(stage);
                    self.texture = Some(texture::upload(ctx, &name, &decoded));
                    self.error = None;
                    // A preview means the full decode is still coming.
                    self.loading = stage == Stage::Preview;

                    // Editing needs the real pixels; a thumbnail stand-in would
                    // produce a preview at the wrong resolution and an export at
                    // the wrong one entirely.
                    if stage == Stage::Full {
                        self.source = Some(Arc::new(decoded.pixels));
                    }
                }
                Ok(LoadMessage::Failed(message)) => {
                    self.error = Some(message);
                    self.loading = false;
                }
                Err(TryRecvError::Empty) => break,
                // Sender gone with nothing more to say — the decode finished.
                Err(TryRecvError::Disconnected) => {
                    self.loading = false;
                    break;
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
        let edits = self.edits.clone();

        // Fresh channel per run: an earlier pipeline still finishing has nowhere to
        // deliver, so it cannot overwrite the newer result.
        let (tx, rx) = std::sync::mpsc::channel();
        self.edit_rx = rx;
        self.applying = true;
        self.texture_generation += 1;

        std::thread::Builder::new()
            .name("edit".to_owned())
            .spawn(move || {
                let _ = tx.send(edits.apply(&source).into_owned());
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

    /// Turn this frame's key presses into actions.
    ///
    /// Everything is read through `consume_key`, which matches against the modifiers
    /// recorded on the key event itself rather than the ones still held when the
    /// frame is assembled. Those differ more often than it sounds: a quick Ctrl+S can
    /// be pressed and fully released inside one frame, and `input.modifiers` — a
    /// snapshot of the end of that frame — then reports nothing held at all, so the
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
        let (open_folder, copy_path, redo, open, undo, save) = ctx.input_mut(|i| {
            (
                i.consume_key(CTRL_SHIFT, Key::O),
                // Ctrl+C is reserved for copying the image itself, which is what
                // anyone pressing it in a viewer expects; the path takes the
                // shifted variant.
                i.consume_key(CTRL_SHIFT, Key::C),
                // Bitwise, not short-circuiting: both spellings of redo have to be
                // consumed, or the unread one is left for something else to act on.
                i.consume_key(CTRL_SHIFT, Key::Z) | i.consume_key(CTRL, Key::Y),
                i.consume_key(CTRL, Key::O),
                i.consume_key(CTRL, Key::Z),
                i.consume_key(CTRL, Key::S),
            )
        });

        let (escape, fullscreen, delete, prev, next, rotate, sidebar) = ctx.input_mut(|i| {
            (
                i.consume_key(NONE, Key::Escape),
                i.consume_key(NONE, Key::F11),
                i.consume_key(NONE, Key::Delete),
                i.consume_key(NONE, Key::ArrowLeft),
                i.consume_key(NONE, Key::ArrowRight),
                i.consume_key(NONE, Key::R),
                i.consume_key(NONE, Key::E),
            )
        });

        if escape {
            // Escape means "back out of where I am", so it leaves fullscreen
            // before it closes the app.
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

        if self.texture.is_none() {
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
        if delete {
            self.delete_current(ctx);
        }
        if rotate {
            // The same op the sidebar's rotate button pushes — there is exactly one
            // rotation in this app, and it is an edit.
            self.push_op(ctx, Op::RotateCw);
        }
        if sidebar {
            self.sidebar_open = !self.sidebar_open;
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

    fn apply(&mut self, ctx: &egui::Context, action: toolbar::Action) {
        match action {
            toolbar::Action::Open => self.prompt_for_file(ctx),
            toolbar::Action::OpenFolder => self.prompt_for_folder(ctx),
            toolbar::Action::CopyPath => self.copy_path(),
            toolbar::Action::Delete => self.delete_current(ctx),
            toolbar::Action::ToggleFullscreen => self.set_fullscreen(ctx, !self.fullscreen),
            toolbar::Action::ToggleSidebar => self.sidebar_open = !self.sidebar_open,
        }
    }

    fn apply_sidebar(&mut self, ctx: &egui::Context, action: sidebar::Action) {
        match action {
            sidebar::Action::Close => self.sidebar_open = false,
            sidebar::Action::Apply(op) => self.push_op(ctx, op),
            sidebar::Action::Undo => self.undo(ctx),
            sidebar::Action::Redo => self.redo(ctx),
            sidebar::Action::Save => self.save(ctx),
        }
    }

    /// Write the edited pixels out.
    ///
    /// Save-as whenever the result would not simply take the original's place — a
    /// different format or a different size is a new file, and quietly replacing
    /// the original with it is how originals get lost.
    fn save(&mut self, ctx: &egui::Context) {
        let (Some(source), Some(path)) = (self.source.clone(), self.path.clone()) else {
            return;
        };

        let converting = imaginer_core::Format::from_path(&path) != Some(self.export.format);
        let resizing = self.export.scale_percent != 100;

        let target = if converting || resizing {
            let Some(target) = self.prompt_for_save_path(&path, converting) else {
                return;
            };
            target
        } else {
            path
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
        // not — the shell will still ask before overwriting, but a suggestion that
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
    /// of the action — the user asked for the folder, not for one file in it.
    fn prompt_for_folder(&mut self, ctx: &egui::Context) {
        let mut dialog = rfd::FileDialog::new();
        if let Some(dir) = self.path.as_deref().and_then(|p| p.parent()) {
            dialog = dialog.set_directory(dir);
        }

        let Some(dir) = dialog.pick_folder() else {
            return;
        };

        let folder = imaginer_core::Folder::of_directory(&dir);
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
        let text = path.display().to_string();

        match arboard::Clipboard::new().and_then(|mut clipboard| clipboard.set_text(text)) {
            Ok(()) => self.notify("Path copied"),
            Err(err) => self.error = Some(format!("Could not copy the path: {err}")),
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
                self.file_size = None;
                self.loading = false;
                self.view.reset();
            }
        }

        self.notify(format!("{name} moved to the Recycle Bin"));
    }

    fn set_fullscreen(&mut self, ctx: &egui::Context, fullscreen: bool) {
        self.fullscreen = fullscreen;
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(fullscreen));
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

        self.tick_notice(&ctx);
        self.handle_dropped_files(&ctx);
        self.handle_shortcuts(&ctx);

        let base_frame = egui::Frame::side_top_panel(ui.style());
        let toolbar_frame = base_frame.inner_margin(egui::Margin::symmetric(10, 7));
        let status_frame = base_frame.inner_margin(egui::Margin::symmetric(10, 5));

        let mut requested = None;
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
                        sidebar_open: self.sidebar_open,
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
                        file_size: self.file_size,
                        error: self.error.as_deref(),
                        notice: self.notice.as_ref().map(|n| n.text.as_str()),
                    },
                );
            });

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
                        },
                    );
                });
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(theme::CANVAS_BG))
            .show(ui, |ui| {
                if let Some(texture) = self.texture.as_ref() {
                    self.last_zoom = viewer::show(ui, texture, &mut self.view);
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

        self.trace.mark_first_frame();

        // Benchmark mode: leave as soon as there is something to measure.
        if self.trace.should_exit_after_first_frame()
            && (self.texture.is_some() || self.error.is_some() || self.path.is_none())
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

/// Drawn width of the wordmark. Present but quiet — this screen exists to be left,
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

fn file_size(path: &std::path::Path) -> Option<u64> {
    std::fs::metadata(path).ok().map(|m| m.len())
}

/// Export settings for a freshly opened file.
///
/// The format defaults to the one the file is already in, so Save means save rather
/// than convert. A format this build can read but not write falls back to PNG, the
/// lossless option — converting is then the honest description of what will happen.
fn export_defaults(path: Option<&Path>) -> imaginer_core::ExportSettings {
    imaginer_core::ExportSettings {
        format: path
            .and_then(imaginer_core::Format::from_path)
            .unwrap_or(imaginer_core::Format::Png),
        ..Default::default()
    }
}
