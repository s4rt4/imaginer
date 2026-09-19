# Imaginer

Fast-startup image viewer and light editor for Windows. Rust + eframe/egui.
Personal tool, not a distributed product.

## Install

Grab the installer from [Releases](https://github.com/s4rt4/imaginer/releases) and
run it. It puts Imaginer in Program Files, adds Start-menu and desktop shortcuts,
and registers the file types it opens so "Open with > Imaginer" is there. The
uninstaller takes all of it back out, per-extension registrations included.

To register a build you made yourself instead, without an installer:

```powershell
.\scripts\associate.ps1              # -Uninstall to undo
```

## What it does

**Viewing**

- Opens a file from the command line (so "Open with" and file associations work),
  dropped onto the window, or through the Open dialog
- Two-stage decode: the embedded EXIF thumbnail appears first, the full image
  swaps in behind it. EXIF orientation is applied, so phone photos are upright
- Neighbouring images are decoded ahead of time into a byte-budgeted cache, so
  stepping through a folder is instant after the first step
- Thumbnails are cached on disk, keyed by path and mtime
- Pan, zoom anchored at the cursor, fit-to-window, actual size
- Oversized images are downscaled to the GPU's texture limit rather than failing
- Next/previous through the folder, sorted by name, date or size, from the arrow
  keys or from chevrons that fade in over the canvas and out again
- Animated GIF and WebP play, with a timeline to scrub
- **SVG is drawn from the vector at any zoom** — past the point where a
  whole-artwork raster stops being possible, the visible part is re-rendered at
  screen resolution, the way Illustrator and Inkscape never run out of detail
- Transparency shows as a checkerboard, or as diagonal bands when the file is
  vector — which is the difference between a PNG and the SVG it came from, at a
  glance
- EXIF panel: camera, lens, exposure, and what the file itself is
- Fullscreen and slideshow, both of which get the chrome out of the way once the
  pointer goes still
- Dark window chrome, coloured to match the toolbar rather than the shell's grey

**Editing** — non-destructive; the original is only ever read

- Flip, rotate, and mirror (the image beside its own reflection, so the canvas
  doubles)
- Crop: drag a rectangle, move it, drag its corners, with aspect presets and a
  thirds guide
- Trim the fully transparent border off a PNG in one click
- Resize to a scale or to exact dimensions
- Brightness, contrast and saturation, previewed on the GPU while the slider
  moves and applied on the CPU on the way to the file — a test pins the two
  implementations to the same numbers
- Undo/redo over the operation stack
- Save, or save-as with format, quality and scale

**Converting** — the reason this exists beside a viewer

- Right-click any image in Explorer for a Convert submenu; a multi-file selection
  is reassembled into one batch and asks for a destination once
- Or from a prompt, with no window at all:

```powershell
imaginer --convert webp --quality 82 --out D:\out *.png
```

- `.ico` output writes the whole size ladder into one file — the reason this was
  built, since the alternative was a rate-limited website

**Housekeeping**

- Copy the image to the clipboard, or paste one in — including a path copied as
  text. A pasted image has no file, and the app says so rather than pretending
- Copy the file path; move the file to the Recycle Bin, never `fs::remove_file`
- Settings panel with every shortcut the app answers to, and the few choices
  that outlive a session

## Formats

| | Formats |
|---|---|
| Opens | PNG, JPEG, GIF, BMP, WebP, TIFF, ICO, farbfeld, SVG/SVGZ, PSD, JPEG XL, AVIF |
| Animates | GIF, WebP |
| Saves | PNG, JPEG, WebP, BMP, ICO |

PSD opens the flattened composite — layer compositing is a different program.
AVIF decodes through `rav1d` and `gamut-avif`, both pure Rust, so there is no C
toolchain anywhere in the build. HEIC does not open: there is no pure-Rust
decoder, and the list of formats is a promise rather than an aspiration.

## Shortcuts

| Key | Action |
|---|---|
| `Ctrl+O` / `Ctrl+Shift+O` | Open an image / a folder |
| `←` / `→` | Previous / next image |
| `Space` | Slideshow, or play the animation |
| `F11` | Fullscreen |
| `Esc` | Back out, or close |
| `F` / `0` | Fit to window |
| `1` | Actual size |
| `+` / `−` | Zoom in / out |
| Double-click | Toggle fit / 100% |
| `Ctrl+C` | Copy this image |
| `V` | Paste an image or a path |
| `P` | Copy the file path |
| `Del` | Move to the Recycle Bin |
| `R` | Rotate clockwise |
| `C` | Crop — then `Enter` to apply, `Esc` to cancel |
| `E` | Edit sidebar |
| `I` | File and EXIF info |
| `S` | Settings |
| `Ctrl+S` | Save |
| `Ctrl+Z` / `Ctrl+Y` | Undo / redo |

Copy and paste are odd on purpose, and not ours: egui-winit consumes `Ctrl+C` and
`Ctrl+V` before a key event exists, so copy-path moved to plain `P` and pasting is
`V`. The `Ctrl+Shift+C` this app once documented had never fired.

There is exactly one rotation, and it is an edit: the old view-only rotate was
dropped when the sidebar arrived. Two buttons that look identical and differ only
in whether the result can be saved is a trap. Because the pipeline is
non-destructive, straightening a crooked photo just to look at it still costs
nothing.

## Build and run

```powershell
cargo run --release -- "C:\path\to\image.jpg"
```

The installer is built from `packaging/`, with its images generated from the SVG
sources first:

```powershell
cargo run --release -p imaginer-ui --example make-installer-assets
makensis packaging\installer.nsi        # writes dist\Imaginer-<version>-setup.exe
```

## Startup measurement

Startup is the point of this project, so it is measured continuously rather than
profiled at the end:

```powershell
.\scripts\bench-startup.ps1 -Image "C:\path\to\image.jpg" -Runs 20
```

The binary prints milestones to stderr under `IMAGINER_TRACE_STARTUP=1`, and
`IMAGINER_EXIT_AFTER_FIRST_FRAME=1` makes it close as soon as there is something
to measure.

**Only same-sitting comparisons count.** These numbers drift by 150ms and more
between sessions with machine state alone — more than most changes worth
measuring. A number from yesterday is not a baseline: to judge a change, measure
it and its alternative back to back. Everything below is shape, not constants.

### Where the time actually goes

Development machine (Ryzen laptop, AMD iGPU), warm start, median of 12, measured
2026-09-19:

| Milestone | Median |
|---|---|
| `run_native` (our code done, handing off to eframe) | ~8ms |
| `context_ready` (window + GL context exist) | ~1066ms |
| `theme_ready` (font loaded, style applied) | ~1089ms |
| `first_image` (image on screen) | ~1334ms |

**Roughly 80% of startup is spent creating the graphics context, before any of our
own code runs.** `IMAGINER_TRACE_INIT=1` timestamps eframe's own logging, which is
the only way to see inside it: ~470ms of glutin WGL display and pixel-format
enumeration, ~265ms of winit event loop and window, ~220ms of `make_current` and
painter init, ~85ms of shaders and VAO. All of it is inside winit, glutin and the
driver. There is no code of ours in that window to optimise.

### What was ruled out

The original suspicion — Optimus loading the NVIDIA OpenGL driver — is **wrong**:
the context already binds the AMD integrated GPU. Forcing the adapter with the
Windows per-app graphics preference confirms it (`first_image`, median of 15):

| Preference | Median |
|---|---|
| none set (default) | 1152ms |
| `GpuPreference=1` (power saving / iGPU) | 1144ms |
| `GpuPreference=2` (high performance / dGPU) | 1444ms |

Forcing the discrete GPU is the only setting that changes anything, and it makes
startup ~300ms worse. Re-run with `.\scripts\bench-gpu-preference.ps1`.
`HardwareAcceleration::Required` was also ~50ms worse. **~900ms is the floor for
eframe + glow on this machine.**

Renderer choice, same machine, median of 10: glow 1032ms, wgpu 2648ms. glow is the
default because of that.

The one measurable win left is vsync: `IMAGINER_VSYNC=0` saves ~180ms. It stays on
anyway. Tearing while panning would be paid every session where the 180ms is paid
once, and vsync is what caps the repaint loop that runs during decode. The env var
is kept for re-measuring, not for shipping with it off.

### Is it actually fast?

The original ~150ms target was not reachable and has been retired. The honest
yardstick is nomacs, on the same machine and image, warm, median of 8, measured
2026-09-19:

| Viewer | Time to own a window |
|---|---|
| Imaginer | ~309ms |
| nomacs | ~1189ms |

Imaginer's window appears at ~309ms but is empty until ~1334ms, so the honest
comparison is ~1334ms against nomacs. nomacs cannot have painted before it has a
window, so its time-to-image is necessarily above ~1189ms: Imaginer wins, but by a
modest margin rather than an order of magnitude. Compare with
`.\scripts\bench-vs-viewer.ps1`; that metric is approximate — see the script
header.

## Layout

```
assets/          logo artwork and the UI icon set, as SVG
imaginer-core/   decode, EXIF, folder listing, edit pipeline, export, convert — no UI dependency
imaginer-ui/     eframe app: canvas, theme, toolbar, status bar, panels, clipboard
packaging/       the NSIS installer, and the images it is built from
scripts/         benchmarks, and the Explorer and Open-with registration
```

`imaginer-core` must never gain a UI dependency; that split is what keeps the
image logic testable without opening a window.

The icons and the logo are rasterised at build time by `imaginer-ui/build.rs`, so
the chrome costs a memcpy rather than a render — icons are monochrome strokes, so
only the alpha channel is kept (~2KB each) and the colour arrives at draw time
from the widget's own foreground colour, which is what makes hover, disabled and
active states free. SVG *files* are a different matter: those are rendered at
runtime by resvg, and re-rendered as you zoom into them.

## Licence

MIT — see [LICENSE](LICENSE).

The UI icons are [Lucide](https://lucide.dev), also MIT. `chevron-left` and
`chevron-right` were drawn by hand to match the set. AVIF decoding uses
[rav1d](https://github.com/memorysafety/rav1d) (BSD-2-Clause) and
[gamut-avif](https://crates.io/crates/gamut-avif) (MIT/Apache-2.0).
