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
