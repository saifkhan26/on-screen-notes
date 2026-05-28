# On-Screen Notes

A cross-platform pressure-sensitive on-screen pen-annotation app written in Rust.

Draw and write directly over anything on your screen — browser, IDE, video, anything — with a pressure-sensitive pen tablet. The window is transparent and borderless, holds multiple canvases (left/right arrow to flip), and can be made click-through with the **Alt** key so the app behind still receives input.

## Features

- Transparent, borderless, resizable overlay window
- Tiny floating circular activator button (always-on-top)
- Pressure-sensitive ink (Wacom / Huion / XP-Pen / native pen)
- Shapes: rectangle, ellipse, straight arrow, freehand arrow
- Multiple canvases, persistent on disk, switch with `←` / `→`
- Click-through while holding `Alt`
- Scroll wheel = pen size (when pen is selected) or canvas zoom (otherwise)
- Global hotkey screenshot — saved file contains your annotations *and* the background

## Build

```sh
# Release build (smallest, fastest)
cargo build --release

# Or run directly with logging
RUST_LOG=on_screen_notes=debug cargo run --release
```

The release binary lands in `target/release/on-screen-notes` (`.exe` on Windows). Expect 6–10 MB after `strip` + `lto`.

## Default keybindings

| Action | Binding |
|---|---|
| Toggle overlay visibility | floating-button click, or `Ctrl+Shift+N` (global) |
| Screenshot (bg + ink) | `Ctrl+Shift+S` (global) |
| Click-through | hold `Alt` |
| Previous / next canvas | `←` / `→` |
| Pen | `P` |
| Eraser | `E` |
| Rectangle | `R` |
| Ellipse | `O` |
| Straight arrow | `A` |
| Freehand arrow | `Shift+A` |
| Undo / redo | `Ctrl+Z` / `Ctrl+Shift+Z` |
| Clear canvas | `Ctrl+Delete` |

## Platform notes

- **Windows** — pen pressure works natively via Windows Ink. No extra setup.
- **macOS** — requires Screen Recording permission for screenshots. Pen-pressure support via octotablet is partial; mouse fallback always works.
- **Linux X11** — pen pressure via XInput2.
- **Linux Wayland** — pen pressure via tablet-v2. Click-through depends on compositor support.

## Repository layout

See `src/` — every module starts with a `//!` doc comment explaining its role.
# on-screen-notes
