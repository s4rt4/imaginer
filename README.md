# Imaginer

Fast-startup image viewer and light editor for Windows. Rust + eframe/egui.
Personal tool, not a distributed product.

## Status

Phase 1 in progress. What works today:

- Opens an image passed on the command line (so "Open With" / file association works)
  or dropped onto the window
- PNG, JPEG, BMP, GIF (static), WebP (static)
- EXIF orientation applied on load, so phone photos are the right way up
- Two-stage decode: the embedded EXIF thumbnail appears first, the full image
  swaps in behind it
- Pan, zoom (scroll wheel, anchored at the cursor), fit-to-window, 90° view rotation
- Oversized images downscaled to the GPU's texture limit rather than failing
- Custom dark theme, toolbar, status bar
- Startup benchmark harness

Not yet: folder navigation, filmstrip, prefetch, cache, editing, the Open dialog.
See the roadmap for the full list.

## Build and run

```powershell
cargo run --release -- "C:\path\to\image.jpg"
```

## Shortcuts

| Key | Action |
|---|---|
| `F` / `0` | Fit to window |
| `1` | Actual size (100%) |
| `R` | Rotate view 90° |
| `+` / `-` | Zoom |
| Double-click | Toggle fit / 100% |
| `Esc` | Close |

## Startup measurement

Startup is the point of this project, so it is measured continuously rather than
profiled at the end:

```powershell
.\scripts\bench-startup.ps1 -Image "C:\path\to\image.jpg" -Runs 20
```

The binary prints milestones to stderr under `IMAGINER_TRACE_STARTUP=1`, and
`IMAGINER_EXIT_AFTER_FIRST_FRAME=1` makes it close as soon as there is something
to measure.

### Where the time actually goes

Measured on the development machine (Ryzen laptop, hybrid AMD iGPU + RTX 3050),
warm start, median of 15 runs:

| Milestone | Median |
|---|---|
| `run_native` (our code done, handing off to eframe) | ~7ms |
| `context_ready` (window + GL context exist) | ~914ms |
| `theme_ready` (font loaded, style applied) | ~927ms |
| `first_image` (image on screen) | ~1152ms |

**Roughly 80% of startup is spent creating the graphics context, before any of our
own code runs.** Decode and upload of a 1.6MB JPEG cost ~45ms by comparison —
launching with no image at all still takes ~1058ms to first frame.

`IMAGINER_TRACE_INIT=1` timestamps eframe's own debug logging, which is the only
way to see inside that ~910ms. It breaks down as:

| Phase | Cost |
|---|---|
| glutin WGL display creation + pixel-format enumeration | ~470ms |
| winit event loop + window creation (to `Event::Resumed`) | ~265ms |
| `make_current` + egui_glow painter init | ~220ms |
| shader compilation + VAO setup | ~85ms |

All of it is inside winit, glutin and the GPU driver. There is no code of ours in
that window to optimise.

### What was ruled out

The original suspicion — that this was Optimus loading the NVIDIA OpenGL driver —
is **wrong**. The context already binds the AMD integrated GPU. Forcing the
adapter with the Windows per-app graphics preference confirms it (`first_image`,
median of 15):

| Preference | Median |
|---|---|
| none set (default) | 1152ms |
| `GpuPreference=1` (power saving / iGPU) | 1144ms |
| `GpuPreference=2` (high performance / dGPU) | 1444ms |

Forcing the discrete GPU is the only setting that changes anything, and it makes
startup ~300ms worse. Re-run with `.\scripts\bench-gpu-preference.ps1`.
`HardwareAcceleration::Required` (`IMAGINER_HW_ACCEL=required`) was also ~50ms
worse. **~900ms is the floor for eframe + glow on this machine.**

The one measurable win left is vsync: `IMAGINER_VSYNC=0` takes `first_image` from
~1152ms to ~974ms. It is still on by default, because turning it off can tear
while panning and zooming and that tradeoff has not been eyeballed yet.

### Is it actually fast?

The original ~150ms target was not reachable and has been retired. The honest
yardstick is nomacs, on the same machine and image, warm:

| Viewer | Time to own a window |
|---|---|
| Imaginer | ~335ms |
| nomacs | ~1303ms |

Imaginer's window appears at ~335ms but is empty until ~1152ms, so the fair
comparison is ~1152ms against nomacs' ~1303ms-plus-paint. The project's premise
holds — it does start faster — but by a modest margin rather than an order of
magnitude. Compare with `.\scripts\bench-vs-viewer.ps1`; that metric is
approximate, see the script header.

Renderer comparison on the same machine (`first_image`, median of 10):

| Renderer | Median |
|---|---|
| glow (OpenGL) | 1032ms |
| wgpu | 2648ms |

glow is the default because of this. To re-run the comparison on other hardware:

```powershell
cargo build --release --features compare-renderers
$env:IMAGINER_RENDERER = "wgpu"
```

## Layout

```
assets/          logo artwork (SVG); not wired into the app yet
imaginer-core/   decode, EXIF, (later) cache and edit ops — no UI dependency
imaginer-ui/     eframe app: viewer canvas, theme, toolbar, status bar
scripts/         startup benchmarks
```

`imaginer-core` must never gain a UI dependency; that split is what keeps the
image logic testable without opening a window.
