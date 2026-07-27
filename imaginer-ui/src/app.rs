//! Application state and the per-frame update loop.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Instant;

use eframe::egui;
use imaginer_core::Stage;

use crate::startup::StartupTrace;
use crate::texture::{self, ImageTexture};
use crate::theme;
use crate::views::{statusbar, toolbar, viewer};
use crate::{LoadMessage, decode_into};

pub struct App {
    trace: StartupTrace,
    path: Option<PathBuf>,
    rx: Receiver<LoadMessage>,
    texture: Option<ImageTexture>,
    stage: Option<Stage>,
    error: Option<String>,
    view: viewer::ViewState,
    file_size: Option<u64>,
    /// True while a decode is in flight, which is also what drives repainting.
    loading: bool,
    /// Bumped per load so each texture gets a distinct name in egui's texture manager.
    texture_generation: u64,
    last_zoom: f32,
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        launched_at: Instant,
        path: Option<PathBuf>,
        rx: Receiver<LoadMessage>,
    ) -> Self {
        let trace = StartupTrace::new(launched_at);
        // Reaching here means the window and GL context exist, so this mark splits
        // startup into "platform setup" and "our own work".
        trace.mark("context_ready");

        theme::install(&cc.egui_ctx);
        trace.mark("theme_ready");

        Self {
            trace,
            file_size: path.as_deref().and_then(file_size),
            loading: path.is_some(),
            path,
            rx,
            texture: None,
            stage: None,
            error: None,
            view: viewer::ViewState::default(),
            texture_generation: 0,
            last_zoom: 1.0,
        }
    }

    fn open(&mut self, ctx: &egui::Context, path: PathBuf) {
        // A fresh channel per open, so results from a previous load that is still
        // in flight cannot land on top of the new one.
        let (tx, rx) = std::sync::mpsc::channel();
        self.rx = rx;

        self.file_size = file_size(&path);
        self.path = Some(path.clone());
        self.texture = None;
        self.stage = None;
        self.error = None;
        self.view.reset();
        self.loading = true;
        self.texture_generation += 1;

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
                    self.stage = Some(decoded.stage);
                    self.texture = Some(texture::upload(ctx, &name, &decoded));
                    self.error = None;
                    // A preview means the full decode is still coming.
                    self.loading = decoded.stage == Stage::Preview;
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

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let (open_requested, close_requested) = ctx.input(|i| {
            (
                i.modifiers.ctrl && i.key_pressed(egui::Key::O),
                i.key_pressed(egui::Key::Escape),
            )
        });

        if close_requested {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if open_requested {
            self.prompt_for_file(ctx);
            return;
        }

        if self.texture.is_none() {
            return;
        }

        ctx.input(|i| {
            if i.key_pressed(egui::Key::R) {
                self.view.rotate_clockwise();
            }
            if i.key_pressed(egui::Key::F) || i.key_pressed(egui::Key::Num0) {
                self.view.reset();
            }
            if i.key_pressed(egui::Key::Num1) {
                self.view.set_zoom(1.0);
            }
            if i.key_pressed(egui::Key::Plus) || i.key_pressed(egui::Key::Equals) {
                self.view.zoom_by(1.25, self.last_zoom);
            }
            if i.key_pressed(egui::Key::Minus) {
                self.view.zoom_by(1.0 / 1.25, self.last_zoom);
            }
        });
    }

    fn prompt_for_file(&mut self, _ctx: &egui::Context) {
        // Native file dialogs arrive with `rfd` in the next milestone; until then
        // files come in via the command line or drag-and-drop.
        self.error = Some("Open dialog not wired up yet — drag an image in, or pass a path on the command line".to_owned());
    }

    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .find_map(|f| f.path.clone())
        });

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

        self.handle_dropped_files(&ctx);
        self.handle_shortcuts(&ctx);

        let base_frame = egui::Frame::side_top_panel(ui.style());
        let toolbar_frame = base_frame.inner_margin(egui::Margin::symmetric(10, 7));
        let status_frame = base_frame.inner_margin(egui::Margin::symmetric(10, 5));

        let mut open_requested = false;
        egui::Panel::top(egui::Id::new("toolbar"))
            .frame(toolbar_frame)
            .show(ui, |ui| {
                open_requested = toolbar::show(ui, &mut self.view, self.texture.is_some())
                    == Some(toolbar::Action::Open);
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
                    },
                );
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(theme::CANVAS_BG))
            .show(ui, |ui| match self.texture.as_ref() {
                Some(texture) => {
                    self.last_zoom = viewer::show(ui, texture, &mut self.view);
                    self.trace.mark_first_image();
                }
                None => empty_state(ui, self.error.as_deref()),
            });

        if open_requested {
            self.prompt_for_file(&ctx);
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

fn empty_state(ui: &mut egui::Ui, error: Option<&str>) {
    ui.centered_and_justified(|ui| match error {
        Some(error) => {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        None => {
            ui.colored_label(theme::TEXT_MUTED, "Drop an image here");
        }
    });
}

fn file_size(path: &std::path::Path) -> Option<u64> {
    std::fs::metadata(path).ok().map(|m| m.len())
}
