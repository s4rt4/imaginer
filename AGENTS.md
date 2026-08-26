# Imaginer — project memory

Imaginer = fast-startup image viewer + light editor for Windows, Rust + eframe/egui.
Personal tool (not distributed). Goal: beat nomacs cold-start while matching its
format coverage and basic edits. Cargo workspace: `imaginer-core` (no UI deps) +
`imaginer-ui` (eframe).

Full-detail source memories live at
`C:\Users\Sarta\.claude\projects\C--laragon-www-imaginer\memory\` (MEMORY.md,
imaginer-roadmap.md, imaginer-tech-decisions.md, imaginer-conversion-requirements.md,
user-language-indonesian.md, powershell-addtype-triggers-av.md).

## Language

User writes Indonesian; reply in Indonesian by default ("pake bahasa indonesia").
Technical terms stay in English (startup, commit, vsync, build, empty state).
Code, commit messages, comments, README stay English.

## Machine constraint

Never use `Add-Type @"...C#..."@` (inline C#) in PowerShell here — Bitdefender
quarantines it mid-write (`Access denied ... Temp\<random>.dll`). `Add-Type -AssemblyName`
(framework assemblies) is fine. For screenshots use `System.Windows.Forms` +
`CopyFromScreen`; no P/Invoke, no window moves/crops. Two more PS 5.1 traps:
piping a script into `Select-Object -First N` can terminate it mid-run (use
`| Out-Null` and read registry state separately), and embedded `"%1"`-style
quotes in `git commit -m` get mangled — use `git commit -F <file>`.

## Settled technical decisions (treat as settled unless benchmarks say otherwise)

- **glow, not wgpu** — warm-start median to first image: glow 1032ms, wgpu 2648ms.
  Custom shaders still work via `egui_glow::CallbackFn`. wgpu kept behind
  `compare-renderers` feature.
- **~900ms is the startup floor** with eframe+glow on this machine. ~910ms of it is GL
  context creation: ~470ms glutin WGL display/pixel-format enumeration, ~265ms winit
  event loop + window, ~220ms make_current + painter init, ~85ms shaders/VAO. The
  Optimus theory was WRONG — it already binds the AMD iGPU. EGL lever untried and
  likely useless (eframe hardcodes FallbackEgl; AMD ships no desktop-Windows EGL).
- **Windows per-app GPU preference does nothing useful** (15 runs each): default 1152ms,
  iGPU 1144ms, dGPU 1444ms. Don't reopen.
- **vsync stays ON** (user decision). Turning it off saves ~180ms once but causes tearing
  every session and uncaps the repaint loop. `IMAGINER_VSYNC` kept only for re-measuring.
- **Startup numbers drift ~150ms+ between sessions. Only same-sitting A/B counts** —
  always A/B against the previous commit, never against a number from an earlier session.
  Distinguish cold vs warm start.
- **No spinner**: decode thread spawns as the first statement of `main()`, before
  `eframe::run_native()`.
- **Color adjustments on GPU shader (preview), CPU reference impl in core (export)**;
  drift test pins the formulas together. Geometric ops (flip/rotate/mirror/crop) preview
  through the CPU path instead — memcpy-class, so preview == export code exactly.
- **Cargo features are compile-time**; "load on first use" was wrong. Dependency count
  affects compile time/binary size, not startup. `imageproc` not needed unless
  convolution filters are wanted.
- **AVIF is expensive** (dav1d FFI, same tier as HEIC), unlike TIFF/ICO.
- **Rotate exists once**, in the edit sidebar (turns the image, savable). View-only
  rotate dropped; `R` maps to it.
- **Convert and scale are export settings** (Save panel), not transform tools.
- **Mirror = canvas-doubling** (output 2x wide/tall), not a flip; belongs to the
  size-changing op class with crop/scale/rotate-90.
- **Icons rasterised at build time** (`build.rs`, resvg) into alpha masks, tinted at draw
  time from `fg_stroke`. No SVG rendering at runtime.
- **Dark title bar via `DWMWA_CAPTION_COLOR`/`DWMWA_TEXT_COLOR`/`DWMWA_BORDER_COLOR`
  (Win11 22H2+), NOT `DWMWA_USE_IMMERSIVE_DARK_MODE`** — the latter makes the canvas
  flicker black on every repaint. Do not "simplify" back. `IMAGINER_DARK_TITLEBAR=0`
  kept for re-testing; any DWM attribute is the first suspect when presentation misbehaves.
- **No UI animations** — rapid presents bring the black flicker back. Chrome appears/
  disappears in one frame; anything animated must be re-tested against the flicker.
  App asks for frames when something changed, never to run an animation.
- **Shortcuts must use `input_mut().consume_key`, never `input.modifiers`** (modifiers
  read end-of-frame state; same-frame press+release vanishes). Consume more specific
  shortcuts first; plain keys need explicit `Modifiers::NONE`.
- **egui-winit eats Ctrl+C/Ctrl+V before key events exist** (and paste events only fire
  when clipboard holds text, so image-paste on Ctrl+V isn't expressible in eframe 0.35).
  Current bindings: copy-image rides `Event::Copy`, paste answers `Event::Paste` and
  plain `V`, copy-path is plain `P`. Re-read egui-winit's interception before changing.
- **eframe `default_fonts` disabled** (bundles Ubuntu+Hack+emoji, ~1-2MB); one font.
- **SendKeys driving is real but flaky**: launch via `cmd /c` with command NOT starting
  with a quote (prefix e.g. `set FOO=1&&`); read stderr redirect only after exit;
  design assertions so a dropped key can't look like a pass. `AppActivate` reports
  success it didn't achieve. Screenshot recipe that works: `scratchpad/shoot.ps1`.

## Conversion requirements (user-stated priorities, 2026-07-29)

- WebP export with quality dial (lossless WebP insufficient; needs libwebp via `webp`
  crate — `image-webp` 0.2 is lossless-only). Quality 100 encodes lossless.
- `.ico` export: **PNG entries below 256 break GDI+ entirely** (`System.Drawing.Icon`
  throws on the whole file). Ladder 16/20/24/32/40/48/64/256 (128 omitted); PNG only at
  256, raw BMP below, BMP mask rows padded to 4 bytes. Size alone isn't the acceptance
  test — check it opens in both GDI+ and WIC. Beats nomacs measured: 51KB/8 entries vs
  nomacs' 135KB single uncompressed full-size entry (=1.03MB for 512x512 logo).
- Explorer batch convert: classic HKCU verbs under `SystemFileAssociations\.<ext>\shell\`
  (lands under "Show more options" on Win11 — accepted staging; packaged
  IExplorerCommand later if wanted), plus single-instance collector: pipe creation via
  `FILE_FLAG_FIRST_PIPE_INSTANCE` is both lock and channel (no separate mutex); late
  instances write path to the named pipe and exit; batch key = format+quality.
- Output rules (user-set): folder dialog once per run, same stem + new extension (built
  by hand, not `Path::with_extension`), overwrite without asking.
- CLI convert mode: `imaginer --convert <format> [--quality N] [--out <dir>]
  [--collect] <files...>`; GUI-subsystem release builds attach to parent console first
  (`console.rs`) and raise dialogs on failure.

## Status / roadmap highlights

DONE: MVP viewer (decode, pan/zoom, EXIF orientation+thumbnail fast paint, prefetch +
byte-budgeted LRU cache in bytes keyed path+len+mtime, eviction stops at 1 entry);
UI rework rounds 1-3 (icons, dark titlebar, toolbar, folder nav, chevrons, slideshow 4s,
fullscreen auto-hide, edit sidebar with flip/rotate/mirror/crop/trim, undo/redo,
export PNG/JPEG/WebP/BMP/ICO with scale); WebP+ICO conversion + Explorer batch + CLI;
copy/paste clipboard; sorting name/date/size (`c3ed159`, size/mtime captured during scan,
ties fall back to name then path, menu stays open, order lives on `App`); adjustment
shader verified (`examples/verify-adjust-shader.rs`, real GPU, worst diff 1 vs CPU);
startup investigation closed (~900ms floor, premise vs nomacs survives); EXIF info
panel (`c490875`); golden-image tests for edit ops (`55e2613`); TIFF/ICO/farbfeld
decode (`5f44fe7`); SVG open via resvg, logo through the same code (`d701827`);
animated GIF/WebP playback + timeline scrub (see animation notes below).

## Animation notes (animated GIF/WebP, landed 2026-08-26)

- **Both codecs composite for you.** `into_frames()` on `image`'s GIF decoder
  applies disposal methods and blends sub-rectangles onto the full canvas;
  image-webp's `read_frame` blends internally. Frames stored in
  `Decoded.animation` are whole pictures — no compositing anywhere else.
- **`Decoded.animation: Option<Arc<Animation>>`, `pixels` is always frame 0** —
  every static path (layout, edits, export, clipboard) keeps working untouched on
  an animation; it acts on the frame showing.
- **Two-phase decode**: `decode_into` sends the still first
  (`decode_full_static`), then the frame set as a second message — a 68-frame
  GIF must not hold first paint hostage. Consequence: **`loading` now means "a
  decode channel is still alive"** and only `poll_decode`'s Disconnected branch
  clears it; `accept` must not touch it.
- **Cache entries know frames**: `get`/`holds` take `want_animation`; a still
  entry is refused (but kept — the animated re-decode replaces it). Prefetch
  warms neighbours with `decode_full_static` only.
- **Playback mirrors `tick_slideshow`**: deadline `Instant` +
  `request_repaint_after`, texture updated via `TextureHandle::set` (no
  re-alloc). Geometry edits pause playback; colour sliders keep working live
  over a playing animation (shader applies at draw time).
- **Space is dual-role**: play/pause when the file is animated, slideshow
  otherwise (guard at the execution site in `handle_shortcuts`).
- **Flicker re-test PASSED**: continuous presents every ~70-100ms during
  playback do NOT bring back the black flicker (30 rapid samples, worst 0.09%
  black fraction, repeated across runs). The no-animations rule stands for UI
  chrome transitions; content animation is safe on this machine.
- **Screen-diff testing gotchas**: pick a shot gap that is not a whole period
  of the test GIF (the 8x100ms demo is exactly 800ms); subtle-motion GIFs
  (blinking cat) need a fine grid + low threshold over the image region only;
  **check for leftover `imaginer` processes** — a zombie window showing the
  same GIF makes the fresh launch diff read 0.00%. Default window (720pt) hangs
  off the bottom of this display (691pt effective), so the timeline bar and
  status bar are off-screen in screenshots.

## Disk thumbnail cache (landed 2026-08-26)

- **Purpose**: files without an EXIF thumbnail (PNG, screenshots, downloads)
  had no fast first paint — every open paid the full decode. Now the first
  decode of any file writes a 256px PNG to `%LOCALAPPDATA%\imaginer\thumbs\`,
  and `decode_thumb` serves later opens of the same version as a Preview-stage
  `Decoded`, exactly like the EXIF lane. `decode_preview` first, then
  `decode_thumb`, then the full decode.
- **Key = hash(path + size + mtime) baked into the file name** (FNV-1a, no
  dependency). A saved-over image lands in a new file and the stale one stops
  being served with no index to update. One file per thumb = no locks between
  instances, which is why the roadmap chose this over a database file.
- **Writes are fire-and-forget** (`thumbs::ensure`, temp name + atomic rename),
  from the decode thread and the prefetcher only — never the UI thread. GC
  (`prune`) deletes oldest past 4096 files down to 3600, after each store.
- **Tests must use the `_in` variants** (`load_from`/`ensure_in` take an
  explicit directory); the public `load`/`ensure` hit the real cache, and the
  first draft of the tests polluted `%LOCALAPPDATA%` before this split.
- `IMAGINER_THUMB_DIR` overrides the location (investigation knob, like
  `IMAGINER_CACHE_MB`).
- Not yet a *consumer-visible* filmstrip — the thumbnails serve the preview
  lane only. A filmstrip UI would be the natural next consumer.

## sRGB texture format — verified identity (closed 2026-08-26)

- `imaginer-ui/examples/verify-srgb.rs` uploads a full-range ramp through the
  app's real texture code (`texture.rs` `#[path]`-included, real
  `TextureOptions`), draws it the way `viewer::show` draws an unadjusted
  image, reads the framebuffer back, and compares byte-for-byte: **worst
  channel difference 1 across 256 texels, exit 0**. The default texture path
  is identity — no gamma conversion anywhere, nothing to fix, item closed.
- Same scaffolding as `verify-adjust-shader.rs` (readback PaintCallback,
  multisampling 0, warmup frames). Run:
  `cargo run --release -p imaginer-ui --example verify-srgb`.

## Settings panel (landed 2026-08-26)

- `S` or the toolbar gear opens it; it shares the right-hand strip with the
  sidebar and info panel under the same one-at-a-time rule, and Esc backs out
  of it before leaving fullscreen.
- Two halves: the shortcut reference (static table in `views/settings.rs`,
  kept honest by review — no test can read a human label) and the preferences
  that persist: slideshow seconds and the default sort order, in
  `%APPDATA%\imaginer\settings.txt` (`core/settings.rs`, hand-parsed
  `key = value`, per-key fallback to defaults on anything corrupt, slideshow
  clamped 1-120).
- Persistence wiring: `Settings::load()` in `App::new` (the saved order is the
  session's starting order); `set_order` writes through — a sort pick anywhere
  (toolbar menu or panel) is now a saved default. The slider updates live and
  writes on release.
- NOT yet human-verified: SendKeys dropped every 's' across five runs
  (`events=[]` — the AppActivate lie again), so the panel opening, the gear
  button and the slider drag still need one pass by hand. The settings file
  round-trip itself is unit-tested.

## Transparency checkerboard (landed 2026-08-26)

- Images with see-through pixels get a Photoshop-style checker grid behind
  them. `Decoded.has_transparency` is scanned on the decode thread (full scan;
  clipboard pixels use a stride-four scan since paste hits the UI thread), and
  `viewer::show` draws one call of a 2x2 checker texture with
  `wrap_mode: Repeat` and UVs past 1.0 — the GPU tiles it, so squares stay
  8pt on screen while the picture pans/zooms (screen-anchored, not baked in).
- Checker colours are dark-theme greys (0x3c/0x2c); the texture is uploaded
  lazily on the first transparent image (`App.checker`).

Open items: scaled decode (deferred by decision — decode is not the
bottleneck). Phase 4 evaluated 2026-08-26, see the PSD/AVIF/RAW/HEIC notes
below. Deferred by decision: multi-window, ICC color management, layered
editing, cloud sync, catalog, plugins.

Open items: scaled decode (deferred by decision — decode is not the
bottleneck). Phase 4 evaluated 2026-08-26, see the PSD/AVIF/RAW/HEIC notes
below. Deferred by decision: multi-window, ICC color management, layered
editing, cloud sync, catalog, plugins.
Deferred by decision: multi-window, ICC color management, layered editing, cloud sync,
catalog, plugins.

## Phase 4 formats — evaluated 2026-08-26

- **PSD — DONE.** `core/psd.rs` reads the flattened composite by hand (header,
  three skip-lengths, planar samples, raw or PackBits; RGB/greyscale/CMYK,
  with or without alpha, 8/16-bit). Hand-rolled because the one existing Rust
  PSD crate (zune-psd 0.5.1) has a bug in its uncompressed path — `while i <
  pixel_count` should be `pixel_count * channels`, so the last channel silently
  decodes as zero. Photoshop itself writes RLE by default, but "rare" is not
  "never" for a wrong colour. Tests synthesise whole PSD files, RLE included.
  Layers/blend modes are a compositor's job and deliberately not attempted.
- **AVIF — rejected, measured.** `avif-native` builds dav1d from C and wants
  pkg-config + a system library (or nasm + meson). The build fails on this
  machine without C toolchain setup no personal viewer should need. Extension
  stays off SUPPORTED_EXTENSIONS — the list is a promise.
- **RAW (CR2/NEF/ARW/DNG) — deferred with a reason.** rawloader/rawler return
  undeveloped CFA data; a viewable image needs a develop pipeline (white
  balance, demosaic, camera matrix, gamma) — a DSP project of its own — and
  there are no sample files on this machine to verify any of it against.
  Revisit only if the user actually shoots RAW.
- **HEIC — deferred with a reason.** No pure-Rust decoder; libheif FFI needs a
  system library, same cost tier as AVIF's dav1d, for a format this machine
  never produces.
## Verification recipes

- Startup: `scripts/bench-startup.ps1` (milestones to stderr); GPU pref:
  `scripts/bench-gpu-preference.ps1`; vs nomacs: `scripts/bench-vs-viewer.ps1`.
- Nav/cache tracing: `IMAGINER_TRACE_NAV=1`; init timing: `IMAGINER_TRACE_INIT`.
- Shader verify: `cargo run --release -p imaginer-ui --example verify-adjust-shader`;
  CPU cost baseline: `cargo test --release -p imaginer-core -- --ignored --nocapture
  adjust_costs` (24MP: 47ms brightness+contrast table, 387ms w/ saturation).
- Clipboard round-trip test: `cargo test -p imaginer-ui -- --ignored`.
- Shell integration: `scripts/install-shell-integration.ps1` (has `-Uninstall`).
