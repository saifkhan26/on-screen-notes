//! Screenshot pipeline: capture screen → rasterise annotations → composite → save.
//!
//! Triggered by the global hotkey (default Ctrl+Shift+S). Saved files land
//! in the configured directory or in `<pictures>/on-screen-notes/`.

pub mod compositor;

use crate::canvas::canvas::Canvas;
use crate::canvas::render;
use crate::config::AppConfig;
use crate::error::{Context, Result};
use image::RgbaImage;
use std::path::PathBuf;

/// Outcome of a screenshot. `path` is where the PNG landed on disk;
/// `clipboard` tells the caller whether the image also reached the
/// system clipboard so the toast can say so.
pub struct ShotResult {
    pub path: PathBuf,
    pub clipboard: bool,
}

/// Capture the primary monitor as a raw RGBA image. No file write,
/// no compositing — used by the freeze-frame-to-layer feature to
/// snapshot what is on screen behind the overlay for inline
/// inclusion as a canvas layer.
pub fn capture_primary_image() -> Result<RgbaImage> {
    let monitors = xcap::Monitor::all().context("listing monitors")?;
    let monitor = monitors
        .iter()
        .find(|m| m.is_primary())
        .unwrap_or(&monitors[0]);
    let img = monitor.capture_image().context("capturing screen for freeze")?;
    Ok(img)
}

/// Capture the primary monitor, crop to the OSN window's **client**
/// area, and encode as PNG. The cropped image is positioned at
/// `(canvas_origin, logical_size)` so painting it covers exactly the
/// area OSN sits over at the canvas's *current* pan and zoom —
/// strokes drawn before the freeze stay in place; the captured pixels
/// land precisely under the cursor regardless of how the user has
/// panned or zoomed.
///
/// Why `client_rect` and not `window_rect`: `GetWindowRect` returns
/// the OUTER rect, which on Win10/11 includes a DWM-managed border
/// strip a few pixels wide even when the window is borderless. Using
/// the outer rect made the captured image start a few pixels
/// up-and-left of the visible content — strokes (in client coords)
/// ended up offset bottom-right of the frozen content. `GetClientRect`
/// + `ClientToScreen((0,0))` returns the actual content rectangle
/// the user draws into.
///
/// We also clamp the crop to the captured monitor's bounds. If the
/// OSN window straddles a secondary monitor (which xcap does not
/// capture), the intersection with the primary monitor is what gets
/// frozen — never a wild out-of-bounds slice.
///
/// `pixels_per_point` is the overlay's DPI scale; we divide physical
/// px by it to record the logical-size rect the renderer will draw
/// into. `osn_hwnd` is the platform-native handle of the OSN
/// window; 0 falls back to the full monitor.
pub fn capture_freeze_layer(
    pixels_per_point: f32,
    osn_hwnd: isize,
    canvas_pan: [f32; 2],
    canvas_zoom: f32,
) -> Result<crate::canvas::canvas::LayerImage> {
    let img = capture_primary_image()?;
    let full_w = img.width() as i32;
    let full_h = img.height() as i32;

    // Resolve crop region in monitor-physical pixels. Prefer client
    // rect over outer rect (see doc comment above). Falls back to the
    // whole monitor if we cannot read either.
    let raw_rect = if osn_hwnd != 0 {
        crate::platform::client_rect(osn_hwnd)
            .or_else(|| crate::platform::window_rect(osn_hwnd))
    } else {
        None
    };
    let (cx, cy, cw, ch) = match raw_rect {
        Some((l, t, r, b)) => {
            let l = l.clamp(0, full_w);
            let t = t.clamp(0, full_h);
            let r = r.clamp(0, full_w);
            let b = b.clamp(0, full_h);
            if r > l && b > t {
                (l, t, r - l, b - t)
            } else {
                // Window sits entirely off the primary monitor (or
                // shrank to zero). Fall back to whole monitor so the
                // user still gets *something* recognisable rather
                // than an empty layer.
                (0, 0, full_w, full_h)
            }
        }
        None => (0, 0, full_w, full_h),
    };

    let cropped = image::imageops::crop_imm(&img, cx as u32, cy as u32, cw as u32, ch as u32)
        .to_image();
    let (w, h) = (cropped.width(), cropped.height());
    let mut buf: Vec<u8> = Vec::new();
    {
        let mut cursor = std::io::Cursor::new(&mut buf);
        cropped
            .write_to(&mut cursor, image::ImageFormat::Png)
            .context("encoding freeze PNG")?;
    }
    let ppp = pixels_per_point.max(0.01);
    let zoom = canvas_zoom.max(0.001);
    Ok(crate::canvas::canvas::LayerImage {
        png: buf,
        size: [w, h],
        // Canvas-local extent = client logical px ÷ current zoom.
        // The renderer multiplies this back by zoom on every paint,
        // so the on-screen footprint is exactly the client area.
        logical_size: [w as f32 / ppp / zoom, h as f32 / ppp / zoom],
        // -pan/zoom places the image at canvas-local coords that
        // map to the client top-left under the current view.
        canvas_origin: [-canvas_pan[0] / zoom, -canvas_pan[1] / zoom],
        capture_zoom: zoom,
    })
}

/// Like [`capture_freeze_layer`], but keeps only the pixels *inside*
/// `polygon` (a closed loop in canvas-local coords, as collected by the
/// Lasso-Select tool). Everything outside the polygon is made fully
/// transparent and the result is cropped tight to the selection's
/// bounding box, so the inserted layer is sized to what the user drew.
///
/// Coordinate mapping is the exact inverse of how the renderer places a
/// `LayerImage`: the captured client image covers canvas-local
/// `[canvas_origin, canvas_origin + logical_size]`, so pixel `(px,py)`
/// maps back to that range linearly. Because the renderer paints
/// strokes (and freeze layers) through the same transform, this lands
/// the mask in the same space the polygon was recorded in.
pub fn capture_lasso_layer(
    pixels_per_point: f32,
    osn_hwnd: isize,
    canvas_pan: [f32; 2],
    canvas_zoom: f32,
    polygon: &[[f32; 2]],
) -> Result<crate::canvas::canvas::LayerImage> {
    let img = capture_primary_image()?;
    let full_w = img.width() as i32;
    let full_h = img.height() as i32;

    // Same crop region as the freeze path: prefer client rect, fall
    // back to window rect, then to the whole monitor.
    let raw_rect = if osn_hwnd != 0 {
        crate::platform::client_rect(osn_hwnd).or_else(|| crate::platform::window_rect(osn_hwnd))
    } else {
        None
    };
    let (cx, cy, cw, ch) = match raw_rect {
        Some((l, t, r, b)) => {
            let l = l.clamp(0, full_w);
            let t = t.clamp(0, full_h);
            let r = r.clamp(0, full_w);
            let b = b.clamp(0, full_h);
            if r > l && b > t { (l, t, r - l, b - t) } else { (0, 0, full_w, full_h) }
        }
        None => (0, 0, full_w, full_h),
    };

    let mut cropped =
        image::imageops::crop_imm(&img, cx as u32, cy as u32, cw as u32, ch as u32).to_image();
    let (w, h) = (cropped.width(), cropped.height());
    if w == 0 || h == 0 {
        return Err(crate::error::anyhow!("empty client area for lasso capture"));
    }

    let ppp = pixels_per_point.max(0.01);
    let zoom = canvas_zoom.max(0.001);
    // Canvas-local extent of the full client capture — identical to the
    // freeze-frame math.
    let logical_size = [w as f32 / ppp / zoom, h as f32 / ppp / zoom];
    let canvas_origin = [-canvas_pan[0] / zoom, -canvas_pan[1] / zoom];

    // pixel center -> canvas-local coord.
    let px_to_canvas = |px: u32, py: u32| -> [f32; 2] {
        [
            canvas_origin[0] + ((px as f32 + 0.5) / w as f32) * logical_size[0],
            canvas_origin[1] + ((py as f32 + 0.5) / h as f32) * logical_size[1],
        ]
    };
    // canvas-local coord -> fractional pixel index (inverse of above).
    let cx_to_px = |clx: f32| (clx - canvas_origin[0]) / logical_size[0] * w as f32;
    let cy_to_px = |cly: f32| (cly - canvas_origin[1]) / logical_size[1] * h as f32;

    // Bound the (per-pixel) point-in-polygon test to the polygon's
    // pixel-space bounding box. Pixels outside it can never be inside
    // the polygon, so they are simply excluded from the final crop.
    let (mut pminx, mut pminy, mut pmaxx, mut pmaxy) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for p in polygon {
        pminx = pminx.min(p[0]);
        pminy = pminy.min(p[1]);
        pmaxx = pmaxx.max(p[0]);
        pmaxy = pmaxy.max(p[1]);
    }
    let x_lo = cx_to_px(pminx).floor().clamp(0.0, (w - 1) as f32) as u32;
    let x_hi = cx_to_px(pmaxx).ceil().clamp(0.0, (w - 1) as f32) as u32;
    let y_lo = cy_to_px(pminy).floor().clamp(0.0, (h - 1) as f32) as u32;
    let y_hi = cy_to_px(pmaxy).ceil().clamp(0.0, (h - 1) as f32) as u32;

    // Mask outside-polygon pixels to alpha 0, and track the bounding
    // box of the pixels we keep so we can crop tight afterwards.
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (w, h, 0u32, 0u32);
    let mut any = false;
    for py in y_lo..=y_hi {
        for px in x_lo..=x_hi {
            if crate::canvas::canvas::point_in_polygon(px_to_canvas(px, py), polygon) {
                any = true;
                if px < min_x { min_x = px; }
                if py < min_y { min_y = py; }
                if px > max_x { max_x = px; }
                if py > max_y { max_y = py; }
            } else {
                cropped.get_pixel_mut(px, py).0[3] = 0;
            }
        }
    }
    if !any {
        return Err(crate::error::anyhow!("lasso selection contains no pixels"));
    }

    // Crop tight to the kept bounding box.
    let bw = max_x - min_x + 1;
    let bh = max_y - min_y + 1;
    let masked = image::imageops::crop_imm(&cropped, min_x, min_y, bw, bh).to_image();

    let mut buf: Vec<u8> = Vec::new();
    {
        let mut cursor = std::io::Cursor::new(&mut buf);
        masked
            .write_to(&mut cursor, image::ImageFormat::Png)
            .context("encoding lasso PNG")?;
    }

    // Canvas-local placement of the cropped sub-rect: logical_size is
    // proportional to pixel size, so scale by (bw/w, bh/h) and shift the
    // origin by the bbox offset (also in canvas-local units).
    let sub_logical = [
        bw as f32 / w as f32 * logical_size[0],
        bh as f32 / h as f32 * logical_size[1],
    ];
    let sub_origin = [
        canvas_origin[0] + (min_x as f32 / w as f32) * logical_size[0],
        canvas_origin[1] + (min_y as f32 / h as f32) * logical_size[1],
    ];

    Ok(crate::canvas::canvas::LayerImage {
        png: buf,
        size: [bw, bh],
        logical_size: sub_logical,
        canvas_origin: sub_origin,
        capture_zoom: zoom,
    })
}

/// Capture the primary monitor, composite the active canvas on top,
/// write a PNG, and push the same image to the system clipboard.
///
/// `pixels_per_point` is the DPI scale egui is using for the overlay
/// (1.0 at 100 % display scaling, 1.5 at 150 %, etc.). xcap captures at
/// physical pixels while the canvas coords are in egui logical pixels;
/// passing the ratio here lets the rasteriser line up with what the
/// user sees on screen.
pub fn capture_and_save(canvas: &Canvas, cfg: &AppConfig, pixels_per_point: f32) -> Result<ShotResult> {
    // 1) Pick the primary monitor. xcap returns a Vec; element 0 is usually
    //    the primary, but we also look for the one flagged as primary.
    let monitors = xcap::Monitor::all().context("listing monitors")?;
    // xcap 0.0.14: `is_primary` returns `bool` directly. Pick that monitor
    // if found, otherwise fall back to index 0.
    let monitor = monitors
        .iter()
        .find(|m| m.is_primary())
        .unwrap_or(&monitors[0]);

    // 2) Capture the screen.
    let bg_image = monitor.capture_image().context("capturing screen")?;
    let (w, h) = (bg_image.width(), bg_image.height());

    // 3) Rasterise the canvas to an RGBA buffer the same size as the
    //    screen. tiny-skia owns the pixel buffer; we just borrow a mutable
    //    view of it.
    let mut pixmap = tiny_skia::Pixmap::new(w, h)
        .ok_or_else(|| crate::error::anyhow!("could not allocate pixmap"))?;
    {
        let mut view = pixmap.as_mut();
        // For screenshot we always render at the canvas's current pan/zoom,
        // multiplied by the DPI scale so a 150 % display lays strokes at
        // the physical pixel positions the user sees on the live overlay.
        render::render_canvas(&mut view, canvas, None, None, pixels_per_point);
    }

    // 4) Convert tiny-skia's premultiplied RGBA into the `image` crate's
    //    straight-alpha RgbaImage. We un-premultiply so blend maths stays
    //    correct.
    let raw = pixmap.data().to_vec();
    let mut fg = RgbaImage::from_raw(w, h, raw)
        .ok_or_else(|| crate::error::anyhow!("invalid pixmap data"))?;
    unpremultiply(&mut fg);

    // 5) Convert xcap's image (it returns `image::RgbaImage` already) into
    //    a mutable RgbaImage for blending.
    let mut bg = bg_image; // already RgbaImage
    compositor::alpha_blend(&mut bg, &fg);

    // 6) Compose output filename and save.
    let dir = cfg
        .screenshot_dir
        .clone()
        .unwrap_or(crate::persistence::paths::default_screenshot_dir()?);
    std::fs::create_dir_all(&dir)?;
    let now = chrono_like_timestamp();
    let path = dir.join(format!("on-screen-notes_{now}.png"));
    bg.save(&path).with_context(|| format!("saving {path:?}"))?;

    // Push the composited image to the system clipboard so the user
    // can paste straight into Discord / Slack / Photoshop without
    // opening the saved file. arboard wants straight-alpha RGBA, and
    // `bg` already is — we unpremultiplied the foreground before
    // alpha-blending into it. Failure is non-fatal; we still keep the
    // file on disk.
    let clipboard = match arboard::Clipboard::new() {
        Ok(mut cb) => {
            let (w, h) = (bg.width(), bg.height());
            let bytes = bg.into_raw();
            let img = arboard::ImageData {
                width:  w as usize,
                height: h as usize,
                bytes:  std::borrow::Cow::Owned(bytes),
            };
            match cb.set_image(img) {
                Ok(()) => true,
                Err(e) => { log::warn!("clipboard push failed: {e}"); false }
            }
        }
        Err(e) => { log::warn!("clipboard open failed: {e}"); false }
    };

    Ok(ShotResult { path, clipboard })
}

/// Fully-portable timestamp without pulling chrono in: `YYYYMMDD_HHMMSS`
/// derived from the system clock.
pub fn chrono_like_timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Calculate Y/M/D/H/M/S from epoch seconds. This is a little ugly but
    // avoids an extra crate dependency. Good enough for filenames.
    let s = secs % 60;
    let m = (secs / 60) % 60;
    let h = (secs / 3600) % 24;
    let days = secs / 86400;
    // Approximate date from days since 1970-01-01.
    let (y, mo, d) = days_since_epoch_to_ymd(days);
    format!("{y:04}{mo:02}{d:02}_{h:02}{m:02}{s:02}")
}

fn days_since_epoch_to_ymd(mut days: u64) -> (u64, u64, u64) {
    let mut year: u64 = 1970;
    loop {
        let leap = is_leap(year);
        let dy = if leap { 366 } else { 365 };
        if days < dy { break; }
        days -= dy;
        year += 1;
    }
    let m_lengths = [31, if is_leap(year) { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut month: u64 = 1;
    for &len in &m_lengths {
        if days < len { break; }
        days -= len;
        month += 1;
    }
    (year, month, days + 1)
}

fn is_leap(y: u64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// Convert premultiplied RGBA → straight RGBA, in place. tiny-skia outputs
/// premultiplied pixels (RGB already scaled by alpha); the `image` crate's
/// blend logic assumes straight alpha.
fn unpremultiply(img: &mut RgbaImage) {
    for px in img.pixels_mut() {
        let a = px.0[3];
        if a == 0 || a == 255 { continue; }
        let af = a as f32 / 255.0;
        for c in 0..3 {
            let v = (px.0[c] as f32 / af).min(255.0);
            px.0[c] = v as u8;
        }
    }
}
