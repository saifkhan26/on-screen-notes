# On-Screen Notes

A cross-platform pressure-sensitive on-screen pen-annotation app written in Rust.

Draw and write directly over anything on your screen — browser, IDE, video,
slides — with a pressure-sensitive pen tablet. The window is transparent and
borderless, holds multiple canvases (←/→ to flip), supports per-canvas layers,
and can be made click-through with **Alt** so the app behind still receives
input.

---

## Features

- Transparent, borderless, resizable overlay window
- Tiny floating circular activator button (always-on-top)
- Pressure-sensitive ink (Wacom / Huion / XP-Pen / native pen via Wintab on
  Windows, XInput2 on X11, tablet-v2 on Wayland)
- **Krita-parity stroke smoothing** — five algorithms, per-mode knobs (see
  [Stroke smoothing](#stroke-smoothing))
- **Brush styles**: Pen, Pencil (translucent graphite stamp), Marker
  (highlighter), Airbrush (soft-cloud spray)
- **Shapes**: rectangle, ellipse, straight line, straight arrow, freehand
  arrow, with optional animated-dashed border
- **Fill tool** — flood fill via lyon non-zero winding (handles
  self-intersecting outlines)
- **Laser pointer** — strokes fade ~1 s after pen-up
- **Spotlight** — dims everything except a square around the cursor
- **Lasso erase** — draw a closed loop, ink inside gets removed (with undo)
- **Text labels** — click to place, type, Enter to commit
- **Multiple canvases**, persistent on disk via RON
- **Multiple layers per canvas** (independent visibility + opacity, and
  each layer can be panned / zoomed on its own with `Shift+Space`)
- **Click-through** while holding `Alt`
- **Global hotkey screenshot** — saved PNG contains background + ink
- **Screen recording** — GIF (built-in) or MP4 (if ffmpeg on PATH)
- **Pin to window** — overlay follows another window's position
- **Freeze frame** — bake the current screen behind into a new layer

---

## Build

```sh
# Release build (smallest, fastest)
cargo build --release

# Or run directly with logging
RUST_LOG=on_screen_notes=debug cargo run --release
```

Release binary lands in `target/release/on-screen-notes`
(`.exe` on Windows). Expect 6–10 MB after `strip` + `lto`.

### Linux build deps

eframe (wgpu / winit / xkbcommon) and xcap (screenshot) link
against system libs:

```sh
sudo apt-get install -y \
  libxkbcommon-dev libxkbcommon-x11-dev \
  libwayland-dev wayland-protocols libxdo-dev \
  libgtk-3-dev libxcb1-dev libxcb-render0-dev libxcb-shape0-dev \
  libxcb-xfixes0-dev libxcb-randr0-dev libxcb-xinerama0-dev \
  libxcb-cursor-dev libssl-dev pkg-config
```

### CI builds

Every push (any branch) and every PR to `main` triggers
`.github/workflows/build.yml`, which builds the release binary on
Windows, Ubuntu, and macOS hosted runners. Artifacts are uploaded as
**workflow artifacts** under the Actions tab and kept for 90 days.

To grab the latest CI build: **GitHub repo → Actions → pick a green
run → scroll to Artifacts → download the zip for your OS.**

### Cutting a release

Push a `v*` tag and `.github/workflows/release.yml` builds all three
platforms, zips each binary with `README.md` + sample `hotkeys.toml`,
and publishes a public GitHub Release with the zips attached:

```sh
git tag v0.2.0
git push --tags
```

Releases are **portable** — no installer, no admin rights. Extract
the zip anywhere, run the binary. `hotkeys.toml` auto-writes next to
the binary on first launch.

> First Windows launch shows a SmartScreen warning because the
> binary is unsigned. Click *More info → Run anyway*. A
> code-signing cert would suppress this — not required for personal
> use.

---

## Tools

| Tool | Key | Notes |
|---|---|---|
| Pen | `P` | Smooth ribbon ink |
| Pencil | `B` | Translucent graphite, edge-jittered stamps |
| Marker | `M` | ~45 % alpha highlighter; overlaps darken |
| Airbrush | `G` | Soft disc spray; hold still to build density |
| Fill | `F` | Click → flood-fills enclosed region |
| Laser pointer | `L` | Strokes fade after pen-up |
| Text | `T` | Click anywhere, type, Enter commits |
| Spotlight | `S` | Press again to exit |
| Lasso erase | `X` | Draw closed loop |
| Eyedropper | `I` | One-shot — next click picks a pixel |
| Eraser | `Ctrl` (hold) | Modifier — pen erases while held |
| Rectangle | `R` | |
| Ellipse | `O` | |
| Straight arrow | `A` | |
| Freehand arrow | `Shift+A` | |
| Straight line | toolbar only | |

### Dashed border

Rect / Ellipse / Line / Arrow tools expose a contextual **dashed-border
toggle** in the toolbar. When active, the shape commits with an
animated marching-ants outline. Toggle is per-tool, not global.

---

## Stroke smoothing

Settings popup (toolbar gear icon) exposes two selectors:

### Stroke smoothing — Low / Medium / High

Drives the legacy `Adaptive` pipeline (One Euro filter + wobble killer +
causal EMA on cache build). Picks a position-pass budget and an internal
cutoff/beta tuning.

### Smoothing mode — Krita-parity

| Mode | Algorithm | Feel |
|---|---|---|
| **Adaptive** | OEF + wobble + EMA | Default. Speed-adaptive, no perceptible lag |
| **None** | Raw pass-through | Every digitiser tick lands unchanged |
| **Simple** | 2-tap moving average | Krita "Basic" — old-tablet jitter only |
| **Weighted** | Gaussian history with tail-aggressiveness | Krita "Weighted" — pressure-aware lift-off taper |
| **Stabilizer** | Sample queue + delay-distance gate | Krita "Stabilizer" — rope-pull, cursor leads, ink trails |

#### Weighted knobs

- **Distance px** — Gaussian σ · 3. Bigger = smoother, laggier.
- **Tail aggressiveness** — boosts the penalty on rising-pressure
  samples so lift-off taper tightens.
- **Smooth pressure** — also blends pressure through the history.

#### Stabilizer knobs

- **Sample size** — queue depth. Bigger = more rope-pull lag.
- **Use delay distance** — gate ink behind a radius; cursor must move
  further than the radius before a sample commits.
- **Delay px** — that radius.
- **Finish stabilized curve** — drain queue on pen-up so the trail
  catches up to the final cursor position.
- **Stabilize sensors** — mix pressure / tilt through the running-mean
  blender, not only position.

#### Render-time

- **Fan corners** — stamps a disc at every interior polyline vertex
  whose tangent rotates by more than the step. Cheap version of
  Krita's `paintFan` — fills the wedge a miter joint would leave
  empty.

Each stroke snapshots the active options at pen-down so a mid-stroke UI
change does not retro-apply.

---

## Global hotkeys

Work even when overlay is unfocused.

| Action | Default |
|---|---|
| Toggle overlay visibility | `Ctrl+Shift+N` |
| Screenshot (bg + ink) | `Ctrl+Shift+S` |
| Start / stop recording | `Ctrl+Shift+R` |
| Pause / resume recording | `Ctrl+Shift+P` |

All hotkeys (globals + in-overlay shortcuts) are configurable via
`hotkeys.toml`. See [Customising hotkeys](#customising-hotkeys).

---

## In-overlay shortcuts

| Action | Binding |
|---|---|
| Tool switch | letter keys above |
| Eraser (modifier) | hold `Ctrl` |
| Undo / redo | `Ctrl+Z` / `Ctrl+Shift+Z` |
| Clear active layer | `Ctrl+Delete` |
| Delete active canvas | `Ctrl+Shift+Backspace` |
| Previous / next canvas | `←` / `→` |
| Previous / next layer | `↓` / `↑` |
| Reset pan + zoom | `Ctrl+0` |
| Reset active layer position | `Ctrl+Shift+0` |
| Reset active layer zoom | `Ctrl+Shift+9` |
| Freeze frame → layer | `Ctrl+Shift+F` |
| Click-through (passthrough) | hold `Alt` |
| Pan canvas | hold `Space` + drag |
| Pan active layer | hold `Shift+Space` + drag |
| Zoom active layer | hold `Shift+Space` + scroll |
| Active layer opacity — absolute | `1`=10 % … `9`=90 %, `0`=0 % |
| Active layer opacity — step | `-` −10 %, `=` +10 % |
| Layer preview popup (active layer) | `Q` (toggle) |
| Toggle floating UI | `Tab` |
| Cancel pending (eyedropper / lasso / text) | `Esc` |
| Text mode: newline | `Shift+Enter` |
| Text mode: commit | `Enter` |

---

## Customising hotkeys

A `hotkeys.toml` file is auto-generated next to the executable on
first launch. Edit it in any text editor and restart the app — the
changes are picked up at startup.

Resolution order (first hit wins):

1. `<binary-dir>/hotkeys.toml` — portable mode, preferred.
2. `<user-config-dir>/hotkeys.toml` — installed mode fallback
   (`%APPDATA%\on-screen-notes\` on Windows etc.).
3. Built-in defaults if neither file exists.

### Format

Strings are case-insensitive. Modifiers: `ctrl` (also `cmd` /
`command` on macOS), `shift`, `alt` (also `option`). Special keys:
`arrowleft|right|up|down`, `backspace`, `delete`, `enter`, `escape`,
`tab`, `space`, `f1`..`f12`. Letters and digits as-is (`a`, `0`).

A bad binding (typo, unknown token) is silently disabled — only that
one shortcut goes dead, the rest still work. Look at `RUST_LOG=warn`
output for the offending line.

```toml
[global]
toggle_overlay = "ctrl+shift+n"
screenshot     = "ctrl+shift+s"
start_record   = "ctrl+shift+r"
pause_record   = "ctrl+shift+p"

[tool]
pen        = "p"
pencil     = "b"
marker     = "m"
airbrush   = "g"
fill       = "f"
laser      = "l"
text       = "t"
spotlight  = "s"
lasso      = "x"
eyedropper = "i"
rect       = "r"
ellipse    = "o"
arrow      = "a"
freehand_arrow = "shift+a"

[canvas]
undo          = "ctrl+z"
redo          = "ctrl+shift+z"
clear_layer   = "ctrl+delete"
delete_canvas = "ctrl+shift+backspace"
reset_view    = "ctrl+0"
reset_layer_pos = "ctrl+shift+0"
reset_layer_zoom = "ctrl+shift+9"
freeze_frame  = "ctrl+shift+f"
prev_canvas   = "arrowleft"
next_canvas   = "arrowright"
prev_layer    = "arrowdown"
next_layer    = "arrowup"
toggle_ui     = "tab"

# Layer opacity (active layer). Absolute values + step keys.
opacity_0 = "0"
opacity_1 = "1"
opacity_2 = "2"
opacity_3 = "3"
opacity_4 = "4"
opacity_5 = "5"
opacity_6 = "6"
opacity_7 = "7"
opacity_8 = "8"
opacity_9 = "9"
opacity_dec = "-"
opacity_inc = "="
preview_layer = "q"
```

Modifier-key gestures stay hard-coded (they are not shortcuts):
`Ctrl` (eraser), `Alt` (passthrough), `Space` (pan), `Shift+Space`
(pan / zoom the active layer instead of the view).

---

## Mouse & wheel

| Context | Wheel | Result |
|---|---|---|
| Pen / Pencil / Marker / Airbrush / Eraser | scroll | Adjust brush size |
| Spotlight active | scroll | Resize spotlight square |
| Any other tool | scroll | Canvas zoom |
| Any tool | `Shift` + scroll | Active layer opacity |
| Any tool | `Shift+Space` + scroll | Active layer zoom (about the cursor) |

Right-click a palette swatch to recolour it. Mouse fallback works for
every tool when no tablet driver is present.

---

## Canvases & layers

- Canvases are independent sheets. `→` opens a new blank canvas past the
  last one (infinite on demand). `←` walks backward; wraps from the
  first.
- Each canvas holds a stack of **layers** (strokes + shapes + optional
  raster background). `↑` / `↓` walk the active layer. Per-layer
  visibility + opacity edit lives in the layer panel.
- Both canvases and layers persist as RON files under the platform's
  config dir (`%APPDATA%\on-screen-notes\` on Windows,
  `~/.config/on-screen-notes/` on Linux,
  `~/Library/Application Support/on-screen-notes/` on macOS).

---

## Recording

`Ctrl+Shift+R` starts a screen capture of the area under the overlay;
press again to stop. `Ctrl+Shift+P` pauses / resumes mid-record.

Output format toggle in settings:

- **GIF** — pure-Rust encoder, ~80 KB dependency, always available.
- **MP4 if available** — uses `ffmpeg` from PATH; falls back to GIF
  when ffmpeg is missing. Smaller files, better quality.

Recorded GIF bytes are pushed to the clipboard too — paste straight
into Slack / Discord.

---

## Platform notes

- **Windows** — pen pressure via Wintab (Wacom, Huion, XP-Pen, Gaomon).
  We pick Wintab over RTS / WM_POINTER because it polls on the main
  thread and never starves the message pump.
- **macOS** — Screen Recording permission required for screenshots
  and recording. Pen-pressure via octotablet is partial; mouse fallback
  always works.
- **Linux X11** — pen pressure via XInput2.
- **Linux Wayland** — pen pressure via tablet-v2. Click-through depends
  on compositor support.

---

## Repository layout

```
src/
├── app.rs            ─ top-level eframe app, event loop
├── canvas/
│   ├── canvas.rs     ─ one drawable sheet + layers + undo
│   ├── render.rs     ─ egui Painter + tiny-skia paths
│   ├── shape.rs      ─ Rect / Ellipse / Line / Arrow / Text / Raster
│   ├── smoothing.rs  ─ Krita-parity smoothing modes + per-mode state
│   └── stroke.rs     ─ PenSample, Stroke, OneEuroFilter, cache build
├── config.rs         ─ AppConfig (RON-serialised preferences)
├── flood_fill.rs     ─ scan-line flood, lyon outline
├── input/
│   ├── filter.rs     ─ wobble + One Euro pre-conditioner
│   ├── hotkeys.rs    ─ global hotkey bindings
│   ├── modifiers.rs  ─ Ctrl/Alt/Shift tracker
│   ├── pen.rs        ─ Wintab queue → PenEvent
│   └── picker.rs     ─ window-pick helpers
├── interaction/      ─ keyboard / mouse dispatch
├── persistence/      ─ canvas + config load/save
├── platform/         ─ OS-specific window tweaks
├── recording.rs      ─ frame capture, GIF / MP4 encode
├── screenshot/       ─ xcap composite + PNG write
├── tools/            ─ ActiveTool dispatch, per-tool helpers
└── ui/               ─ toolbar, overlay, loupe, status, layer panel
```

Every module starts with a `//!` doc comment explaining its role —
`cargo doc --open` for a navigable reference.
