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
| `context_ready` (window + GL context exist) | ~873ms |
| `theme_ready` (font loaded, style applied) | ~880ms |
| `first_image` (image on screen) | ~1094ms |

**Roughly 80% of startup is spent creating the graphics context, before any of our
own code runs.** Decode and upload of a 1.6MB JPEG cost ~45ms by comparison —
launching with no image at all still takes ~1058ms to first frame.

This is the single fact that should drive the next round of work. The original
target of ~150ms is not reachable on this hardware without attacking GL context
creation itself; optimising decode further would be pointless.

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
imaginer-core/   decode, EXIF, (later) cache and edit ops — no UI dependency
imaginer-ui/     eframe app: viewer canvas, theme, toolbar, status bar
scripts/         startup benchmark
```

`imaginer-core` must never gain a UI dependency; that split is what keeps the
image logic testable without opening a window.
