# Imaginer

Fast-startup image viewer and light editor for Windows. Rust + eframe/egui.
Personal tool, not a distributed product.

## Status

Usable as a daily viewer, with light editing. What works today:

**Viewing**

- Opens an image passed on the command line (so "Open With" / file association works),
  dropped onto the window, or picked through the native Open dialog
- PNG, JPEG, BMP, GIF (static), WebP (static)
- EXIF orientation applied on load, so phone photos are the right way up
- Two-stage decode: the embedded EXIF thumbnail appears first, the full image
  swaps in behind it
- Pan, zoom (scroll wheel, anchored at the cursor), fit-to-window
- Oversized images downscaled to the GPU's texture limit rather than failing
- Next/previous through the containing folder, from the arrow keys or from
  chevrons that fade in over the canvas and out again when the pointer goes still
- Fullscreen, where the toolbar and status bar get out of the way once you stop
  moving — otherwise "fullscreen" only means "big window"
- Slideshow over the folder; any keypress stops it
- Dark window chrome, coloured to match the toolbar rather than the shell's grey

**Editing** — non-destructive; the original is only ever read

- Flip, rotate, and mirror (the image beside its own reflection, so the canvas doubles)
- Undo/redo over the operation stack
- Export to PNG, JPEG or BMP with quality and scale; save-as whenever the result
  would not simply replace the original

**Housekeeping**

- Copy the file path, or send the file to the Recycle Bin — never `fs::remove_file`
- Startup benchmark harness

Not yet: crop, colour adjustment, filmstrip, prefetch and cache.

## Build and run

```powershell
cargo run --release -- "C:\path\to\image.jpg"
```

## Shortcuts

| Key | Action |
|---|---|
| `Ctrl+O` / `Ctrl+Shift+O` | Open an image / a folder |
| `←` / `→` | Previous / next image in the folder |
| `F` / `0` | Fit to window |
| `1` | Actual size (100%) |
| `+` / `-` | Zoom |
| Double-click | Toggle fit / 100% |
| `F11` | Fullscreen |
| `Space` | Start or stop the slideshow |
| `E` | Toggle the edit sidebar |
| `R` | Rotate 90° clockwise |
| `Ctrl+Z` / `Ctrl+Y` | Undo / redo |
| `Ctrl+S` | Save |
| `Ctrl+Shift+C` | Copy the file path |
| `Del` | Move to the Recycle Bin |
| `Esc` | Leave fullscreen, or close |

There is exactly one rotation, and it is an edit: the old view-only rotate was
dropped when the sidebar arrived. Two buttons that look identical and differ only
in whether the result can be saved is a trap. Because the pipeline is
non-destructive, straightening a crooked photo just to look at it still costs
nothing.

## Startup measurement

Startup is the point of this project, so it is measured continuously rather than
profiled at the end:

```powershell
.\scripts\bench-startup.ps1 -Image "C:\path\to\image.jpg" -Runs 20
```

The binary prints milestones to stderr under `IMAGINER_TRACE_STARTUP=1`, and
`IMAGINER_EXIT_AFTER_FIRST_FRAME=1` makes it close as soon as there is something
to measure.

**Only compare runs taken minutes apart.** These numbers drift with machine state —
a session of back-to-back LTO builds moved `first_image` by ~150ms with no code
change at all, which is larger than most changes worth measuring. A number from
yesterday is not a baseline. To judge a change, measure it and its alternative in
the same sitting; the numbers below are shape, not constants.

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
~1152ms to ~974ms. It stays on anyway. The app already starts faster than nomacs
with vsync on, so the saving buys nothing; tearing while panning and zooming would
be paid every session where the 180ms is paid once; and vsync is what caps the
repaint loop that runs during decode, which would otherwise spin at unbounded FPS.
The env var is kept for re-measuring, not for shipping with it off.

### Is it actually fast?

The original ~150ms target was not reachable and has been retired. The honest
yardstick is nomacs, on the same machine and image, warm:

| Viewer | Time to own a window |
|---|---|
| Imaginer | ~335ms |
| nomacs | ~1303ms |

Imaginer's window appears at ~335ms but is empty until ~1152ms, so the honest
comparison is ~1152ms against nomacs. nomacs cannot have painted the image before
it has a window, so its time-to-image is necessarily above ~1303ms: Imaginer wins
by at least ~150ms, with vsync on. The project's premise holds — it does start
faster — but by a modest margin rather than an order of magnitude. Compare with
`.\scripts\bench-vs-viewer.ps1`; that metric is approximate, see the script header.

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
assets/          logo artwork and the UI icon set, as SVG
imaginer-core/   decode, EXIF, folder listing, edit pipeline, export — no UI dependency
imaginer-ui/     eframe app: canvas, theme, toolbar, status bar, edit sidebar
scripts/         startup benchmarks
```

`imaginer-core` must never gain a UI dependency; that split is what keeps the
image logic testable without opening a window.

Nothing renders SVG at runtime. `imaginer-ui/build.rs` rasterises the logo with
resvg at build time, and the icons alongside it — icons are monochrome strokes, so
only the alpha channel is kept (~2KB each) and the colour arrives at draw time from
the widget's own foreground colour. That is what makes hover, disabled and active
states free, and it keeps an SVG renderer out of a binary whose whole point is how
fast it starts.

## Licence

MIT — see [LICENSE](LICENSE).

The UI icons are [Lucide](https://lucide.dev), also MIT. `chevron-left` and
`chevron-right` were drawn by hand to match the set.
