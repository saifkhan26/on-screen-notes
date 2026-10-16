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
- **Lasso capture** — draw a closed loop, the screen pixels inside become
  an image on the active layer
- **Select** — pick objects, then move / resize / rotate them with
  handles; multi-select, duplicate, copy / paste, recolour, re-order
- **Text labels** — click to place, type, Enter to commit
- **Videos, GIFs & pictures** — drop a video, animated GIF or picture
  onto the canvas (or use the film button); videos and GIFs play /
  pause, scrub, and step frame by frame forward and back. See
  [Videos & GIFs](#videos--gifs)
- **PureRef boards** — drop a `.pur` file (PureRef 1.x or 2.x) to open it
  as a new slide. See [PureRef boards](#pureref-boards)
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
| Lasso capture | `Shift+X` | Draw closed loop → image object on the active layer |
| Select | `V` | See [Selecting and transforming](#selecting-and-transforming) |
| Eyedropper | `I` | One-shot — next click picks a pixel |
| Eraser | `Ctrl` (hold) | Modifier — pen erases the ink it touches while held |
| Rectangle | `R` | |
| Ellipse | `O` | |
| Straight arrow | `A` | |
| Freehand arrow | `Shift+A` | |
| Straight line | toolbar only | |
| Insert picture / video / GIF | toolbar film button, or drag & drop | See [Videos & GIFs](#videos--gifs) |
| Open a PureRef board | toolbar film button, or drag & drop a `.pur` | Opens as a new slide; see [PureRef boards](#pureref-boards) |

The pen and pencil draw at the chosen size as it looks on screen,
whatever the zoom: zoomed in 4×, a new line is a quarter as wide on the
canvas, so detail work gets fine lines. A line keeps its width once
drawn and scales with the view like everything else; the size slider
re-widths selected pen and pencil lines the same way. Marker, airbrush,
eraser and shapes keep their size in canvas units.

### Dashed border

Rect / Ellipse / Line / Arrow tools expose a contextual **dashed-border
toggle** in the toolbar. When active, the shape commits with an
animated marching-ants outline. Toggle is per-tool, not global.

### Selecting and transforming

The Select tool (`V`) picks through **every visible layer**: strokes,
shapes, text, fills and images. Whatever is topmost under the pointer
wins. A selection lives on one layer, so selecting something on another
layer makes that layer the active one.

| Do this | To |
|---|---|
| Click an object | Select it |
| `Shift` + click | Add it to / remove it from the selection |
| Drag from empty space | Box-select everything fully inside the box (on the active layer; if nothing there is inside, on the topmost layer that has something) |
| Drag inside the frame | Move the selection |
| Drag a corner handle | Resize, keeping proportions |
| Drag a side handle | Stretch along one axis |
| Drag the knob above the frame | Rotate (`Shift` snaps to 15°) |
| `Delete` / `Backspace` | Delete the selection |
| `Ctrl+D` | Duplicate |
| `Ctrl+C` / `Ctrl+X` / `Ctrl+V` | Copy / cut / paste (works across slides and layers; on Windows the copy also pastes as a picture in other apps) |
| `Ctrl+V` with an image on the clipboard | Paste it as an image object |
| `Ctrl+↑` / `Ctrl+↓` | Bring to front / send to back |
| Pick a palette colour, move the size slider | Recolour / change line width of the selection |
| `Esc` | Cancel the drag in progress, then deselect |

Every action is one undo step. Things worth knowing:

- An image (lasso capture, pasted picture) is selected by a **click**.
  Dragging from an unselected image draws the selection box instead, so
  you can box-select ink drawn over a screenshot without dragging the
  screenshot away. Clicks pass through the transparent parts of an
  image to whatever is visible underneath. Neither eraser removes
  images — delete them here.
- Captures made by older versions, and frozen frames, are stored as a
  layer's background rather than as objects. Clicking one turns it into
  an ordinary image object in place — nothing moves — and from then on
  it selects, moves, resizes and rotates like any other.
- Side handles appear only where a one-axis stretch can be stored
  exactly: a single shape, or a selection of strokes, lines, arrows and
  unrotated boxes. Text, and groups containing rotated shapes, resize
  from the corners.
- Within a layer, ink always paints above shapes and images. Bring to
  front / send to back re-orders strokes among strokes and shapes among
  shapes; to put an image over ink, use a layer above.
- While the Select tool is active, `Shift` + click selects rather than
  picking a colour, and `Ctrl` does not erase. `I` still picks a colour.
- Objects on different layers cannot be in one selection.
- Pasting an image-only clipboard works on Windows; elsewhere the
  clipboard must also carry text for the paste to be noticed.
- **Copying into other apps (Windows).** `Ctrl+C` / `Ctrl+X` also put
  the selection on the clipboard as a picture, so it pastes into Krita,
  Photoshop, a browser or a chat. A single picture goes over at its full
  size, as its original pixels (a PureRef picture with its crop and
  mirror, a video or GIF as the frame it shows). Anything else goes
  over as drawn, at its on-screen sharpness, cropped to the selection,
  on a transparent background; text labels are left out of it for now.
  Pasting back into this app still pastes the objects themselves, not
  a flattened picture of them. A big picture takes a moment to reach
  the clipboard.

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
| Cancel pending (eyedropper / lasso / text / selection) | `Esc` |
| Selection: delete / duplicate | `Delete` / `Ctrl+D` |
| Selection: copy / cut / paste | `Ctrl+C` / `Ctrl+X` / `Ctrl+V` |
| Selection: bring to front / send to back | `Ctrl+↑` / `Ctrl+↓` |
| Text mode: newline | `Shift+Enter` |
| Text mode: commit | `Enter` |
| Video / GIF: play / pause | `K` |
| Video / GIF: previous / next frame | `,` / `.` (hold to repeat) |

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
`tab`, `space`, `comma`, `period`, `f1`..`f12`. Letters and digits as-is
(`a`, `0`).

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
lasso_select = "shift+x"
select     = "v"
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

# Select-tool actions (only act while objects are selected).
delete_selection = "delete"
duplicate   = "ctrl+d"
bring_front = "ctrl+arrowup"
send_back   = "ctrl+arrowdown"

# Video / GIF playback: the selected one, else the one under the
# pointer, else the only one on the slide.
media_play_pause = "k"
media_prev_frame = "comma"
media_next_frame = "period"
```

Copy / cut / paste are fixed to `Ctrl+C` / `Ctrl+X` / `Ctrl+V`.

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

Right-click a palette swatch to recolour it.

### Drawing with the mouse

Every tool works with the mouse, with or without a tablet connected:
left-drag draws, `Ctrl` + drag erases, `Shift` + click picks a colour,
`Space` + drag pans. Mouse strokes have no pressure, so they are drawn
at full width.

With a tablet connected the pen and the mouse share the pointer. The
mouse takes over about a quarter of a second after the pen stops
pressing or moving — lift the pen out of range, or leave it lying
still, and draw with the mouse.

---

## Canvases & layers

- Canvases are independent sheets. `→` opens a new blank canvas past the
  last one (infinite on demand). `←` walks backward; wraps from the
  first.
- Each canvas holds a stack of **layers** (strokes + shapes + optional
  raster background). `↑` / `↓` walk the active layer. Per-layer
  visibility + opacity edit lives in the layer panel.
- Both canvases and layers persist as RON files (`canvases\canvas_N.ron`,
  one per slide, plus `config.ron` and the `media` folder) under the
  platform's data dir: `%APPDATA%\OnScreenNotes\on-screen-notes\data\` on
  Windows, `~/.local/share/on-screen-notes/` on Linux,
  `~/Library/Application Support/dev.OnScreenNotes.on-screen-notes/` on
  macOS. Setting the `OSN_DATA_DIR` environment variable points the app
  at another folder instead.

---

## Videos & GIFs

Drop a video or animated GIF from Explorer onto the canvas, or click the
**film** button in the toolbar and pick one or more files. The object
lands where it was dropped (or mid-view), sized to its natural size or
shrunk to fit, and selected. From there it is an ordinary picture:
move / resize / rotate it with the Select tool, draw over it, undo,
duplicate (each copy plays on its own), put it on any layer.

**Pictures** (PNG, JPEG, WebP, BMP) drop in the same way and become a
still image, like a pasted one: turned upright if a phone stored it
sideways, scaled down to 3840 px on the long side, and saved inside the
slide (not in the `media` folder). A single-frame GIF does the same.

- **Formats** — GIF always; MP4, M4V, MOV, MKV, WebM, AVI, WMV through
  FFmpeg. Video plays **muted**.
- **On a slide** — nothing plays by itself. GIFs and videos wait on
  their first frame until you press play (the bar's play button, or
  `K`), every time the app starts. A GIF then loops; a video stops on
  its last frame (turn on loop in the bar to repeat). One you started
  keeps playing, from where it was, when you flip away from its slide
  and back.
- **Playback bar** — appears just below the object while the pointer is
  over it (with any tool) and while it is selected:
  previous frame · play / pause · next frame · seek bar · time and
  frame number · loop. Tapping the bar never draws ink. The bar is
  never in screenshots or recordings.
- **Keys** — `K` play / pause, `,` / `.` previous / next frame (see
  [In-overlay shortcuts](#in-overlay-shortcuts)).
- **Screenshots** include the frame each video / GIF is showing.

### Where the files go

Inserted files are **copied** into a `media` folder next to the
slides (`%APPDATA%\OnScreenNotes\on-screen-notes\data\media\` on
Windows), named after a hash of their content, so moving or deleting
the original does not break the slide and inserting the same file twice
stores it once. Slides keep only the file name plus a small poster
frame.

Deleting a slide (`Ctrl+Shift+Backspace`) also deletes the copies of its
videos and GIFs — once the slides have been saved without it, and only
copies nothing else still uses: no other slide, no undo history on
another slide, nothing in the in-app clipboard (those wait until the
app closes). Deleting a single object leaves its file in place, since
Ctrl+Z can bring it back. The file you originally dropped or picked is
never touched.

### PureRef boards

Drop a PureRef `.pur` file onto the canvas, pick one with the **film**
button, or open it with the app from Explorer ("Open with", or
`on-screen-notes.exe board.pur`), and it opens as a **new slide** at the
end, which the app switches to. Boards saved by PureRef 1.10 / 1.11 and
by PureRef 2 (including 2's "save as legacy") both work.

- **Progress:** while a board opens, a card at the bottom shows what it
  is doing (reading the board, then picture N of M) with a progress bar
  and a **Cancel** button. Big boards take a while: a 1.8 GB board with
  1,500 pictures takes about half a minute. Several boards can open at
  once.
- **Pictures** keep their place, size, rotation, crop and mirroring. The
  whole board is shown at its natural size and shrunk to fit the view if
  it is bigger. Each picture becomes an ordinary picture object on the
  slide.
- **Animated GIFs** come in as GIFs you can play, like a dropped one
  (paused until you start them), unless the board crops or mirrors
  them: those come in as a still of the first frame.
- **Notes** become text labels with their colour and rotation, wrapped
  to the note's width when it has a fixed one. Their background box and
  rich-text styling are dropped.
- **Not brought over:** pictures that are only linked from disk rather
  than stored in the board, formats the app cannot decode (such as PSD,
  TIFF or EXR in PureRef 2 boards), and drawings. The toast says how
  many items were skipped.

**How it is stored.** A board's pictures are copied into the app's
`media` folder (next to `canvases`), mostly as the original files,
named after a hash of their content so the same picture is stored once.
The slide itself keeps only their names, where they go, and a small
preview of each, so even a board with hundreds of pictures makes a slide
file of a few MB, and startup and autosave stay fast. On screen each
picture is loaded in the background at about the size it is shown, so a
big board opens quickly and zooming in sharpens what you look at.
Screenshots use the full pictures.

Opening is one-way: the board file is only read, never changed, and
the slide keeps no link to it or to the pictures' original files.
Deleting the slide never touches the `.pur` file or any picture on disk;
the app's own copies in `media` are deleted once the slides are saved
without it and nothing else uses them (another slide, undo history, the
in-app clipboard), as with videos and GIFs. A huge PureRef 2 board is
read through a temporary copy of its database in the system temp folder,
deleted when the board is open.

### FFmpeg

Videos are decoded by an `ffmpeg` child process (nothing is linked, so
the app builds and runs without it — GIFs need no FFmpeg at all). The
app looks for `ffmpeg` + `ffprobe`, in order:

1. `ffmpeg_path` in `config.ron` (the exe or its folder — set by
   **Locate ffmpeg…**),
2. next to `on-screen-notes.exe` (portable setup),
3. WinGet's install folders (so a fresh install is found without a
   restart),
4. `PATH`.

If it is missing, inserting a video opens a **Video needs FFmpeg**
window: **Install FFmpeg** runs
`winget install Gyan.FFmpeg.Essentials` in a console window (answer its
prompts there) and picks FFmpeg up when it finishes; **Locate ffmpeg…**
points the app at an existing copy. On macOS / Linux install it with
`brew install ffmpeg` / your package manager.

Decoding is capped at 1920 px on the long side and at about the size the
video is shown at. Decoders stop a few seconds after a video is paused
and always exit with the app.

---

## Recording

`Ctrl+Shift+R` starts a screen capture of the area under the overlay;
press again to stop. `Ctrl+Shift+P` pauses / resumes mid-record.

Output format toggle in settings:

- **GIF** — pure-Rust encoder, ~80 KB dependency, always available.
- **MP4 if available** — uses `ffmpeg` (found the same way as for
  [videos](#ffmpeg)); falls back to GIF when ffmpeg is missing. Smaller
  files, better quality.

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
├── media/            ─ video / GIF playback, import, ffmpeg discovery
├── persistence/      ─ canvas + config load/save (`OSN_DATA_DIR` redirects it)
├── platform/         ─ OS-specific window tweaks
├── recording.rs      ─ frame capture, GIF / MP4 encode
├── screenshot/       ─ xcap composite + PNG write
├── tools/            ─ ActiveTool dispatch, per-tool helpers
└── ui/               ─ toolbar, overlay, loupe, status, layer panel,
                        media playback bar
```

Every module starts with a `//!` doc comment explaining its role —
`cargo doc --open` for a navigable reference.
