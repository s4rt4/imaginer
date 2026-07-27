//! Startup instrumentation.
//!
//! Startup time is the one metric that justifies this project, so it is measured
//! from the first milestone rather than profiled at the end — that way a commit
//! that costs 40ms is caught when it lands, not months later.
//!
//! Driven by environment variables so the benchmark script can use the same binary
//! that ships:
//!   * `IMAGINER_TRACE_STARTUP=1` — print milestones to stderr as `startup: <ms>`
//!   * `IMAGINER_EXIT_AFTER_FIRST_FRAME=1` — close as soon as the first frame is
//!     painted, so a benchmark loop can measure without a human closing windows
//!   * `IMAGINER_TRACE_INIT=1` — additionally timestamp eframe's own debug logging,
//!     which is the only visibility into the phases between `run_native` and
//!     `context_ready`. Noisy, and the printing itself perturbs the numbers a
//!     little, so it is for investigation rather than routine benchmarking.

use std::time::Instant;

pub struct StartupTrace {
    launched_at: Instant,
    trace_enabled: bool,
    exit_after_first_frame: bool,
    first_frame_reported: bool,
    first_image_reported: bool,
}

impl StartupTrace {
    pub fn new(launched_at: Instant) -> Self {
        Self {
            launched_at,
            trace_enabled: flag("IMAGINER_TRACE_STARTUP"),
            exit_after_first_frame: flag("IMAGINER_EXIT_AFTER_FIRST_FRAME"),
            first_frame_reported: false,
            first_image_reported: false,
        }
    }

    pub fn should_exit_after_first_frame(&self) -> bool {
        self.exit_after_first_frame
    }

    /// Report a one-off milestone. Used to attribute startup cost to a phase —
    /// without intermediate marks, a slow launch is just one unhelpful number.
    pub fn mark(&self, milestone: &str) {
        self.report(milestone);
    }

    /// Report which GPU the GL context actually bound to.
    ///
    /// On a hybrid-GPU laptop this is the single most important fact about startup:
    /// binding the discrete adapter means loading the vendor's OpenGL ICD, which is
    /// where the bulk of the launch time is suspected to go. Without this line the
    /// timings cannot be attributed to an adapter at all.
    pub fn report_gl(&self, gl: Option<&eframe::glow::Context>) {
        if !self.trace_enabled {
            return;
        }
        let Some(gl) = gl else {
            eprintln!("startup: gl <no glow context — not the glow backend>");
            return;
        };

        use eframe::glow::HasContext as _;
        // SAFETY: called on the thread that owns the current GL context, from
        // `App::new`, which eframe runs after making the context current.
        let (vendor, renderer, version) = unsafe {
            (
                gl.get_parameter_string(eframe::glow::VENDOR),
                gl.get_parameter_string(eframe::glow::RENDERER),
                gl.get_parameter_string(eframe::glow::VERSION),
            )
        };
        eprintln!("startup: gl vendor={vendor} | renderer={renderer} | version={version}");
    }

    pub fn elapsed_ms(&self) -> f64 {
        self.launched_at.elapsed().as_secs_f64() * 1000.0
    }

    /// Called every frame; reports once.
    pub fn mark_first_frame(&mut self) {
        if !self.first_frame_reported {
            self.first_frame_reported = true;
            self.report("first_frame");
        }
    }

    /// Called when pixels from the image first reach the screen. This is the number
    /// that actually matters — `first_frame` can be an empty canvas.
    pub fn mark_first_image(&mut self) {
        if !self.first_image_reported {
            self.first_image_reported = true;
            self.report("first_image");
        }
    }

    fn report(&self, milestone: &str) {
        if self.trace_enabled {
            eprintln!("startup: {milestone} {:.1}ms", self.elapsed_ms());
        }
    }
}

fn flag(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| v != "0" && !v.is_empty())
}

/// Route eframe's `log` output into the startup trace, stamped from the same clock.
///
/// The gap between `run_native` and `context_ready` is all third-party code — winit,
/// glutin, the GPU driver — with no place to hang a milestone of our own. eframe
/// already logs around each phase of that setup; timestamping those lines turns one
/// opaque number into a breakdown, without a profiler or a probe binary.
///
/// Call before `run_native`. Does nothing unless `IMAGINER_TRACE_INIT` is set.
pub fn install_init_logger(launched_at: Instant) {
    if !flag("IMAGINER_TRACE_INIT") {
        return;
    }

    struct TraceLogger {
        launched_at: Instant,
    }

    impl log::Log for TraceLogger {
        fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
            metadata.level() <= log::Level::Debug
        }

        fn log(&self, record: &log::Record<'_>) {
            if !self.enabled(record.metadata()) {
                return;
            }
            let ms = self.launched_at.elapsed().as_secs_f64() * 1000.0;
            eprintln!(
                "startup: log {ms:.1}ms [{}] {}",
                record.target(),
                record.args()
            );
        }

        fn flush(&self) {}
    }

    // Leaks the logger, which is what the `log` crate's global-logger API requires;
    // it lives for the whole process anyway.
    if log::set_boxed_logger(Box::new(TraceLogger { launched_at })).is_ok() {
        log::set_max_level(log::LevelFilter::Debug);
    }
}
