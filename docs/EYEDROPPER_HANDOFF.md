# Handoff: "Shift + tap to sample screen colour" (eyedropper)

This document describes how the **screen colour-pick / eyedropper** feature works in
`on-screen-notes` (an egui/eframe transparent drawing overlay), and gives a precise,
step-by-step plan for porting it into another Rust project.

The behaviour to replicate:

> The user holds **Shift** and taps anywhere on screen with the pen (or mouse). The app
> samples the colour of the single pixel under the pointer — *including pixels belonging
> to other applications behind our window* — and sets the active drawing colour to it.
> There is also a one-shot keyboard-armed variant: press **I**, then the next tap picks.

---

## 1. Mental model

The feature is three independent concerns glued together. Port them in this order:

1. **Sample one pixel of the screen at a physical-pixel coordinate.** This is the only
   genuinely new capability. Everything else is plumbing. (`src/input/picker.rs`)
2. **Convert the pointer position into physical screen pixels.** This is where the bugs
   live: window-client vs. screen space, and logical-points vs. physical-pixels (DPI).
3. **Decide when a tap is a "pick" instead of a "draw", and swallow the rest of the
   gesture** so the same tap does not also leave ink. (`src/app.rs`)

Crucial insight: **the pixel sampler does not read "behind our window" by magic.** It
captures the *whole monitor* (which composites every window including ours) and reads one
pixel out of the resulting bitmap. So whatever your window is showing at that pixel is
what you sample — see the transparency caveat in §7.

---

## 2. The pixel sampler (the core, ~40 lines)

`src/input/picker.rs`, function `pick_screen_color`. It uses `xcap` (a cross-platform
screenshot crate already used by the screenshot pipeline) to capture the monitor that
contains the requested point, then indexes one pixel.

```rust
/// Sample one pixel from whichever monitor contains `(screen_x, screen_y)`.
/// Coordinates are physical (raw) screen pixels — caller is responsible
/// for any DPI scaling. Returns `None` if no monitor contains the point
/// or the underlying capture fails.
pub fn pick_screen_color(screen_x: i32, screen_y: i32) -> Option<[u8; 4]> {
    let monitors = xcap::Monitor::all().ok()?;
    for m in monitors {
        // xcap 0.0.14 returns geometry values directly (not `Result`).
        let mx = m.x();
        let my = m.y();
        let mw = m.width() as i32;
        let mh = m.height() as i32;
        if screen_x < mx || screen_y < my || screen_x >= mx + mw || screen_y >= my + mh {
            continue;
        }
        let img = m.capture_image().ok()?;          // image::RgbaImage
        let lx = (screen_x - mx) as u32;            // monitor-local pixel
        let ly = (screen_y - my) as u32;
        if lx >= img.width() || ly >= img.height() {
            return None;
        }
        let p = img.get_pixel(lx, ly);
        return Some([p.0[0], p.0[1], p.0[2], 255]);  // force opaque alpha
    }
    None
}
```

Things to carry over verbatim:

- **Multi-monitor:** `xcap::Monitor::all()` returns every monitor with its **physical**
  origin in the virtual desktop. Monitors left of / above the primary have **negative**
  `x()`/`y()`. The point-in-rect test plus `screen - monitor_origin` for the local index
  is what makes negative-origin monitors work. Don't assume `(0,0)` is the top-left.
- **Alpha:** force `255`. A captured desktop pixel may carry a meaningless alpha; the
  drawing colour wants opaque.
- **Return `Option`:** capture can fail (locked session, permission, no monitor under
  point). Callers no-op on `None` rather than crashing or clearing the colour.

xcap version pinned in `Cargo.toml`: `xcap = "0.0.14"`. **The geometry API changed across
xcap versions** — in 0.0.14 `m.x()`, `m.width()` return values directly; older/newer
versions return `Result`. If you pull a different version, adjust the `?`/`.ok()?` here.

### Cheaper Windows-only alternative (optional)

A full-monitor capture per pick is heavy (tens of MB copied for a single pixel). On
Windows you can instead use GDI:

```text
HDC dc = GetDC(NULL);            // screen DC
COLORREF c = GetPixel(dc, x, y); // x,y in physical px
ReleaseDC(NULL, dc);
// r = GetRValue(c), g = GetGValue(c), b = GetBValue(c)
```

via the `windows` crate (`Win32::Graphics::Gdi`). on-screen-notes deliberately chose xcap
to stay cross-platform and avoid a second screen-capture path. Pick based on your target:
if you only ship Windows and care about latency, `GetPixel` is much lighter.

---

## 3. Coordinate conversion (the part that breaks)

`pick_screen_color` wants **physical screen pixels**. Your pointer event is in something
else. There are two pointer sources in on-screen-notes and they need *different* maths.

### 3a. The window's client origin

Compute the screen-pixel coordinate of the top-left of your window's client area once per
frame (`src/app.rs`):

```rust
let client_origin = ctx.input(|i| {
    i.viewport()
        .inner_rect
        .map(|r| [r.min.x as i32, r.min.y as i32])
        .unwrap_or([0, 0])
});
```

`inner_rect` is the client area in **physical screen pixels** under eframe 0.29. This is
your bridge between window space and screen space.

### 3b. Pen path — already physical

The Wintab backend opens a **system context** (`CXO::SYSTEM`), so packets arrive in
**physical screen pixels**. The backend subtracts `client_origin` to hand the app
window-client pixels (`PenSample::pos`). To pick, just add it back — no DPI factor:

```rust
let sx = client_origin[0] + s.pos[0] as i32;
let sy = client_origin[1] + s.pos[1] as i32;
pick_screen_color(sx, sy);
```

### 3c. Mouse path — logical points, must scale by DPI

egui's `pointer.hover_pos()` / `interact_pos()` are in **logical points**, not physical
pixels. Multiply by `pixels_per_point()` before adding `client_origin`:

```rust
let ppp = ctx.pixels_per_point();
let sx = client_origin[0] + (p.x * ppp) as i32;
let sy = client_origin[1] + (p.y * ppp) as i32;
```

> **This `* ppp` is the single most common porting bug.** On a 150%-scaled display, a
> mouse pick that forgets it will sample a pixel offset by a third of the distance from
> the window origin — looks "almost right" near the top-left, drifts badly toward the
> bottom-right. If your port only has a mouse (no tablet), this is the line that matters.

---

## 4. Trigger detection & gesture handling

Two ways to enter "pick mode":

1. **Hold Shift** while tapping (modal — read live each frame).
2. **One-shot armed** — press `I`, sets a `eyedropper_pending: bool` flag on the tool; the
   next tap picks and then clears the flag.

```rust
let shift_held = ctx.input(|i| i.modifiers.shift);
let picking = shift_held || self.tool.eyedropper_pending;
```

The hard part is making sure a pick tap does **not** also draw. on-screen-notes does this
with two booleans tracked across the gesture (`src/app.rs` struct fields):

- `pen_picking: bool` — true between the picking Down and its Up.
- `pen_blocked: bool` — separate flag for "this gesture landed on UI / another window".

Flow, on each pen event:

- **Down:** if `shift_held || eyedropper_pending`, set `pen_picking = true`, sample the
  pixel, set `self.tool.color`, clear `eyedropper_pending`, then `continue` (do **not**
  dispatch a stroke-begin).
- **Move:** `if self.pen_blocked || self.pen_picking { continue; }` — swallow.
- **Up:** if `pen_picking`, set `pen_picking = false` and `continue` — swallow, then the
  next gesture is normal.

The mouse fallback needs the analogous guard. It uses `mouse_was_down` as an edge
detector so the still-held button on the next frame neither re-picks nor starts a stroke,
and it `continue`s past the entire mouse-drawing block while `picking` is true:

```rust
if picking && primary_now && !self.mouse_was_down {
    // ...sample, set colour...
    self.tool.eyedropper_pending = false;
    self.mouse_was_down = true;          // consume this press
}
if !primary_now { self.mouse_was_down = false; }
if picking { /* skip the draw path this frame */ }
```

**Precedence:** the pick check is placed *before* the eraser-modifier check, so
Shift-pick wins over any other held modifier. Decide your own precedence but make it
explicit and early in the Down handler.

### UI affordance

While `picking` is true, set the cursor to a crosshair so the user knows the next tap
samples instead of draws:

```rust
ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
```

Note `set_cursor_icon` only changes the glyph; it does **not** suppress drawing on its own
— the `continue`/swallow logic above is what actually prevents ink.

---

## 5. Applying the sampled colour

Trivial — write it onto whatever holds your active drawing colour:

```rust
if let Some(c) = pick_screen_color(sx, sy) {
    self.tool.color = c;   // [u8; 4] RGBA
}
```

In on-screen-notes the tool colour is `[u8; 4]`. If your project stores colour as
`egui::Color32` or `[f32; 4]`, convert at this boundary. No other code needs to know a
pick happened.

---

## 6. Where everything lives (file map)

| File | Role |
|------|------|
| `src/input/picker.rs` | `pick_screen_color` — the pixel sampler. Self-contained. |
| `src/app.rs` (~640–660) | computes `client_origin` from `viewport().inner_rect`. |
| `src/app.rs` (~1005–1115) | pen-path: Shift detection, pick on Down, `pen_picking` swallow of Move/Up. |
| `src/app.rs` (~1118–1150) | mouse fallback: `* ppp` scaling, `mouse_was_down` edge guard. |
| `src/tools/mod.rs` | `eyedropper_pending: bool` field on the tool state. |
| `src/interaction/mod.rs` (~91) | `I` hotkey sets `eyedropper_pending = true`. |
| `src/ui/toolbar.rs` | shows "Eyedropper (1 shot) — I" and "Pick colour — Hold Shift + tap" in help. |

---

## 7. Caveats — read before you ship

These are the things that will waste your afternoon if you don't know them up front.

1. **Overlay transparency / self-tint.** The sampler captures the composited desktop,
   *including your own window*. If your overlay paints a semi-opaque background tint over
   the screen, every picked pixel is contaminated by that tint. on-screen-notes documents
   the rule: pick only when the overlay background is fully transparent (`bg_opacity == 0`),
   or temporarily hide the overlay before capturing if you need the pure source colour.
   If your window is fully opaque, you can **never** sample anything behind it — you'd only
   read your own pixels. The feature only makes sense for a transparent/overlay window.

2. **Latency / cost.** Each pick triggers a full-monitor `capture_image()` (a 4K monitor
   is ~33 MB copied to read 4 bytes). Fine for a deliberate tap; do **not** call it every
   frame to live-preview the hovered colour without throttling. If you want a live loupe,
   use the GDI `GetPixel` path (§2) or cache a capture.

3. **DPI.** Covered in §3c, repeating because it bites: pen samples are physical pixels,
   egui mouse positions are logical points. Only the mouse path multiplies by
   `pixels_per_point()`.

4. **Multi-monitor negative coordinates.** Covered in §2 — don't clamp screen coords to
   `>= 0`; left/upper monitors are legitimately negative.

5. **Always-on-top / focus (Windows).** on-screen-notes is *not* always-on-top and runs
   the Wintab context as a system context, so it receives pen-downs even when the tip is
   over another app. It guards with `top_level_window_at(sx, sy)` to detect taps that land
   on a different top-level window and swallows them (`pen_blocked`) instead of stealing
   focus via `reclaim_foreground`. If your port is a simple always-on-top window you can
   likely drop this guard, but know it exists and why.

6. **Wayland / macOS.** `xcap` supports them, but screen capture may require a permission
   prompt (macOS Screen Recording entitlement; Wayland portal). `pick_screen_color`
   returning `None` on first use can mean "permission not yet granted", not "no monitor".

---

## 8. Porting checklist (generic egui/eframe project)

1. **Add deps** to `Cargo.toml`:
   ```toml
   xcap  = "0.0.14"
   image = { version = "0.25", default-features = false, features = ["png"] }
   ```
   (`image` only needed because xcap returns `image::RgbaImage`; it's transitively present
   anyway.) If you take the GDI route instead, add the `windows` crate with
   `Win32_Graphics_Gdi` and skip xcap.
2. **Drop in `picker.rs`** with `pick_screen_color` exactly as §2. Adjust the geometry
   accessor `?`/`.ok()?` if your xcap version differs.
3. **Confirm your window is transparent** where the user will pick (§7.1). If it isn't,
   this feature cannot read other apps.
4. **Compute `client_origin`** each frame from `ctx.input(|i| i.viewport().inner_rect)`.
5. **Add `eyedropper_pending: bool`** to your tool/app state; set it from your one-shot
   hotkey if you want the `I` variant. Skip if you only want Shift-hold.
6. **In your pointer handling:**
   - read `shift_held`; `picking = shift_held || eyedropper_pending`.
   - while `picking`, set crosshair cursor.
   - on the *press edge* (not every held frame): compute `sx,sy`
     (pen: `client_origin + pos`; mouse: `client_origin + pos * ppp`), call
     `pick_screen_color`, assign the colour, clear `eyedropper_pending`, and
     **swallow the rest of the gesture** so no stroke is drawn.
7. **Test matrix:**
   - [ ] Pick a known colour swatch in another app → drawing colour matches exactly.
   - [ ] On a 150%/200% scaled display, mouse pick lands on the right pixel
         (proves the `* ppp` fix). Test near bottom-right of the window, not just top-left.
   - [ ] Pick on a second monitor positioned to the **left** of primary (negative x).
   - [ ] A Shift-tap leaves **no ink** (gesture fully swallowed: Down, Move, and Up).
   - [ ] `I` then tap picks once; the *following* tap draws normally.
   - [ ] Picking with the overlay tint on shows the contaminated colour (confirms §7.1);
         with tint off shows the true colour.

---

## 9. One-paragraph summary for a reviewer

We capture the monitor under the pointer with `xcap` and read a single pixel
(`pick_screen_color`, returns `[u8;4]`). The pointer position is converted to physical
screen pixels by adding the window's client origin (`viewport().inner_rect.min`); the
mouse path additionally multiplies by `pixels_per_point()` because egui reports logical
points while the pen backend already reports physical pixels. A tap counts as a pick when
Shift is held or a one-shot `eyedropper_pending` flag is set; on the press edge we sample,
assign the colour, and swallow the remainder of the gesture (`pen_picking` / `mouse_was_down`)
so the same tap never draws. The feature only works on a transparent overlay window —
the capture includes our own pixels, so an opaque or tinted window samples itself.
