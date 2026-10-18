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

/// Capture the primary monitor as a raw RGBA image. The fallback for
/// when the OSN window's rect cannot be read.
pub fn capture_primary_image() -> Result<RgbaImage> {
    let monitors = xcap::Monitor::all().context("listing monitors")?;
    let monitor = monitors
        .iter()
        .find(|m| m.is_primary())
        .or(monitors.first())
        .ok_or_else(|| crate::error::anyhow!("no monitors"))?;
    let img = monitor.capture_image().context("capturing screen")?;
    Ok(img)
}

/// The OSN window's client area in screen-physical pixels, as
/// `(left, top, right, bottom)`. Prefers the client rect over the outer
/// rect (see [`capture_freeze_layer`]); `None` when neither can be read
/// or the area is empty.
pub(crate) fn osn_client_rect(osn_hwnd: isize) -> Option<(i32, i32, i32, i32)> {
    if osn_hwnd == 0 {
        return None;
    }
    crate::platform::client_rect(osn_hwnd)
        .or_else(|| crate::platform::window_rect(osn_hwnd))
        .filter(|&(l, t, r, b)| r > l && b > t)
}

/// What is on screen under the OSN window's client area, at physical
/// resolution — taken from whichever monitors the window sits on, not
/// just the primary one. Falls back to the whole primary monitor when
/// the window rect cannot be read.
///
/// Monitors and the client rect share one coordinate space: xcap places
/// each monitor at its position on the virtual desktop in physical
/// pixels, which is what `ClientToScreen` reports for a DPI-aware
/// process. The primary monitor sits at (0, 0) and the others around
/// it, at negative coordinates too.
pub(crate) fn capture_client_area(osn_hwnd: isize) -> Result<RgbaImage> {
    match osn_client_rect(osn_hwnd) {
        Some(rect) => capture_desktop_rect(rect),
        None => capture_primary_image(),
    }
}

/// The screen under `(left, top, right, bottom)` on the virtual desktop,
/// stitched from every monitor it overlaps. Parts on no monitor stay
/// transparent.
pub(crate) fn capture_desktop_rect((l, t, r, b): (i32, i32, i32, i32)) -> Result<RgbaImage> {
    let mut out = RgbaImage::new((r - l) as u32, (b - t) as u32);
    let mut any = false;
    for m in xcap::Monitor::all().context("listing monitors")? {
        let mon = [m.x(), m.y(), m.width() as i32, m.height() as i32];
        if !overlaps([l, t], &out, mon) {
            continue;
        }
        let img = m.capture_image().context("capturing screen")?;
        any |= paste_monitor(&mut out, [l, t], [mon[0], mon[1]], &img);
    }
    if !any {
        return Err(crate::error::anyhow!("the window is not on any monitor"));
    }
    Ok(out)
}

/// Does the monitor `[x, y, w, h]` overlap `out`, whose top-left sits
/// at `origin` on the desktop?
fn overlaps(origin: [i32; 2], out: &RgbaImage, mon: [i32; 4]) -> bool {
    let (r, b) = (origin[0] + out.width() as i32, origin[1] + out.height() as i32);
    mon[0] < r && origin[0] < mon[0] + mon[2] && mon[1] < b && origin[1] < mon[1] + mon[3]
}

/// Copy the part of a monitor's capture `img` (top-left at `mon` on the
/// desktop) that falls inside `out` (top-left at `origin`). Returns
/// whether any of it did.
fn paste_monitor(out: &mut RgbaImage, origin: [i32; 2], mon: [i32; 2], img: &RgbaImage) -> bool {
    let l = origin[0].max(mon[0]);
    let t = origin[1].max(mon[1]);
    let r = (origin[0] + out.width() as i32).min(mon[0] + img.width() as i32);
    let b = (origin[1] + out.height() as i32).min(mon[1] + img.height() as i32);
    if r <= l || b <= t {
        return false;
    }
    // Row by row rather than pixel by pixel: the loupe and the recorder
    // run this on every frame.
    let row = (r - l) as usize * 4;
    let (out_stride, img_stride) = (out.width() as usize * 4, img.width() as usize * 4);
    let (src, dst) = (img.as_raw(), &mut **out);
    for y in t..b {
        let s = (y - mon[1]) as usize * img_stride + (l - mon[0]) as usize * 4;
        let d = (y - origin[1]) as usize * out_stride + (l - origin[0]) as usize * 4;
        dst[d..d + row].copy_from_slice(&src[s..s + row]);
    }
    true
}

/// Capture the screen under the OSN window's **client** area and
/// encode it as PNG. The image is positioned at
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
/// The pixels come from whichever monitors the window is on; a part of
/// the window off every monitor stays transparent.
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
    let img = capture_client_area(osn_hwnd)?;
    let (w, h) = (img.width(), img.height());
    let mut buf: Vec<u8> = Vec::new();
    {
        let mut cursor = std::io::Cursor::new(&mut buf);
        img
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
    // Same client-area capture as the freeze path.
    let mut cropped = capture_client_area(osn_hwnd)?;
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

/// Capture the monitor the OSN window is on, composite the active
/// canvas on top, write a PNG, and push the same image to the system
/// clipboard.
///
/// `pixels_per_point` is the DPI scale egui is using for the overlay
/// (1.0 at 100 % display scaling, 1.5 at 150 %, etc.). xcap captures at
/// physical pixels while the canvas coords are in egui logical pixels;
/// passing the ratio here lets the rasteriser line up with what the
/// user sees on screen.
///
/// `media_frames` holds the frame each video / GIF is showing, so the
/// picture has what is on screen rather than the poster frame.
pub fn capture_and_save(
    canvas: &Canvas,
    cfg: &AppConfig,
    pixels_per_point: f32,
    osn_hwnd: isize,
    media_frames: &render::MediaFrames,
) -> Result<ShotResult> {
    // 1) Pick the monitor under the middle of the OSN window, else the
    //    primary one. The canvas is drawn from the window's client
    //    top-left, so note where that sits on the monitor.
    let client = osn_client_rect(osn_hwnd);
    let under_window = client.and_then(|(l, t, r, b)| xcap::Monitor::from_point((l + r) / 2, (t + b) / 2).ok());
    let monitor = match under_window {
        Some(m) => m,
        None => {
            let monitors = xcap::Monitor::all().context("listing monitors")?;
            let primary = monitors.iter().position(|m| m.is_primary()).unwrap_or(0);
            monitors.into_iter().nth(primary).ok_or_else(|| crate::error::anyhow!("no monitors"))?
        }
    };
    let client_at = match client {
        Some((l, t, _, _)) => [l - monitor.x(), t - monitor.y()],
        None => [0, 0],
    };

    // 2) Capture the screen.
    let bg_image = monitor.capture_image().context("capturing screen")?;
    let (w, h) = (bg_image.width(), bg_image.height());

    // 3) Rasterise the canvas to an RGBA buffer the size of the client
    //    area (the whole monitor without one). tiny-skia owns the pixel
    //    buffer; we just borrow a mutable view of it.
    let (cw, ch) = match client {
        Some((l, t, r, b)) => ((r - l) as u32, (b - t) as u32),
        None => (w, h),
    };
    let mut pixmap = tiny_skia::Pixmap::new(cw, ch)
        .ok_or_else(|| crate::error::anyhow!("could not allocate pixmap"))?;
    {
        let mut view = pixmap.as_mut();
        // For screenshot we always render at the canvas's current pan/zoom,
        // multiplied by the DPI scale so a 150 % display lays strokes at
        // the physical pixel positions the user sees on the live overlay.
        render::render_canvas(&mut view, canvas, None, None, pixels_per_point, Some(media_frames));
    }

    // 4) Convert tiny-skia's premultiplied RGBA into the `image` crate's
    //    straight-alpha RgbaImage. We un-premultiply so blend maths stays
    //    correct.
    let raw = pixmap.data().to_vec();
    let mut ink = RgbaImage::from_raw(cw, ch, raw)
        .ok_or_else(|| crate::error::anyhow!("invalid pixmap data"))?;
    unpremultiply(&mut ink);

    // 5) Lay the ink where the client area sits on the monitor, then
    //    blend it over xcap's image (an `image::RgbaImage` already).
    let mut fg = RgbaImage::new(w, h);
    image::imageops::replace(&mut fg, &ink, client_at[0] as i64, client_at[1] as i64);
    let mut bg = bg_image;
    compositor::alpha_blend(&mut bg, &fg);

    // 6) Save + push to clipboard (shared with the region-capture path).
    save_and_clipboard(bg, cfg)
}

/// Capture the screen under the OSN window, crop to the user-selected
/// rectangle, composite the overlay background tint + the active canvas's
/// ink on top, write a PNG, and push it to the clipboard.
///
/// `rect_points` is the selection rectangle in egui **logical** (point)
/// coordinates relative to the OSN window's client top-left, as
/// `[min_x, min_y, max_x, max_y]`. The capture covers that same client
/// area (same as the freeze path), so the rect maps into it by
/// `pixels_per_point` alone.
///
/// `bg_color` / `bg_opacity` are the overlay window's configured tint. The
/// deferred capture drops the real window opacity to 0 so xcap grabs a
/// clean desktop; we re-apply the tint here in software so the saved image
/// matches what the user sees *through* the semi-transparent overlay.
pub fn capture_region_and_save(
    canvas: &Canvas,
    cfg: &AppConfig,
    pixels_per_point: f32,
    osn_hwnd: isize,
    rect_points: [f32; 4],
    bg_color: [u8; 3],
    bg_opacity: f32,
    media_frames: &render::MediaFrames,
) -> Result<ShotResult> {
    let bg_image = capture_client_area(osn_hwnd)?;
    let (w, h) = (bg_image.width(), bg_image.height());

    let ppp = pixels_per_point.max(0.01);
    // Selection rect → client-physical pixels, clamped to the capture.
    let x0 = (rect_points[0] * ppp).floor().clamp(0.0, w as f32) as u32;
    let y0 = (rect_points[1] * ppp).floor().clamp(0.0, h as f32) as u32;
    let x1 = (rect_points[2] * ppp).ceil().clamp(0.0, w as f32) as u32;
    let y1 = (rect_points[3] * ppp).ceil().clamp(0.0, h as f32) as u32;
    let cw = x1.saturating_sub(x0);
    let ch = y1.saturating_sub(y0);
    if cw == 0 || ch == 0 {
        return Err(crate::error::anyhow!("empty screenshot region"));
    }

    // Rasterise the annotation canvas over the client area at the same
    // scale, then un-premultiply to straight alpha (identical to the
    // full-screen path).
    let mut pixmap = tiny_skia::Pixmap::new(w, h)
        .ok_or_else(|| crate::error::anyhow!("could not allocate pixmap"))?;
    {
        let mut view = pixmap.as_mut();
        render::render_canvas(&mut view, canvas, None, None, ppp, Some(media_frames));
    }
    let raw = pixmap.data().to_vec();
    let mut fg = RgbaImage::from_raw(w, h, raw)
        .ok_or_else(|| crate::error::anyhow!("invalid pixmap data"))?;
    unpremultiply(&mut fg);

    // Crop the desktop and the ink to the selection.
    let mut region = image::imageops::crop_imm(&bg_image, x0, y0, cw, ch).to_image();
    let ink = image::imageops::crop_imm(&fg, x0, y0, cw, ch).to_image();

    // Respect the overlay background: paint the tint (bg_color at
    // bg_opacity) over the desktop crop BEFORE the ink, so the saved region
    // carries the same wash the live overlay shows.
    let a = (bg_opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
    if a > 0 {
        let tint = RgbaImage::from_pixel(cw, ch, image::Rgba([bg_color[0], bg_color[1], bg_color[2], a]));
        compositor::alpha_blend(&mut region, &tint);
    }
    // Ink on top.
    compositor::alpha_blend(&mut region, &ink);

    save_and_clipboard(region, cfg)
}

/// Write `bg` (straight-alpha, opaque RGBA) to the screenshot directory as
/// a timestamped PNG and push it to the system clipboard. Clipboard failure
/// is non-fatal — the file still lands on disk. Shared by the full-screen
/// and region capture paths.
fn save_and_clipboard(bg: RgbaImage, cfg: &AppConfig) -> Result<ShotResult> {
    let dir = cfg
        .screenshot_dir
        .clone()
        .unwrap_or(crate::persistence::paths::default_screenshot_dir()?);
    std::fs::create_dir_all(&dir)?;
    let now = chrono_like_timestamp();
    let path = dir.join(format!("on-screen-notes_{now}.png"));
    bg.save(&path).with_context(|| format!("saving {path:?}"))?;

    // arboard wants straight-alpha RGBA, and `bg` already is.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn filled(w: u32, h: u32, v: u8) -> RgbaImage {
        RgbaImage::from_pixel(w, h, image::Rgba([v, v, v, 255]))
    }

    #[test]
    fn a_window_on_a_second_monitor_gets_that_monitors_pixels() {
        // Primary 100×50 at the origin, a second one to its right.
        let (primary, second) = (filled(100, 50, 1), filled(80, 60, 2));
        let mut out = RgbaImage::new(30, 20);
        let origin = [120, 10];
        assert!(!paste_monitor(&mut out, origin, [0, 0], &primary));
        assert!(paste_monitor(&mut out, origin, [100, 0], &second));
        assert!(out.pixels().all(|p| p.0 == [2, 2, 2, 255]));
    }

    #[test]
    fn a_window_across_two_monitors_is_stitched() {
        // A second monitor to the left of the primary, at negative x.
        let (primary, left) = (filled(100, 50, 1), filled(100, 50, 2));
        let mut out = RgbaImage::new(40, 20);
        let origin = [-10, 40];
        assert!(paste_monitor(&mut out, origin, [-100, 0], &left));
        assert!(paste_monitor(&mut out, origin, [0, 0], &primary));
        assert_eq!(out.get_pixel(9, 0).0, [2, 2, 2, 255]);
        assert_eq!(out.get_pixel(10, 0).0, [1, 1, 1, 255]);
        // Below both monitors (y 50 and on) stays transparent.
        assert_eq!(out.get_pixel(20, 10).0[3], 0);
        assert!(overlaps(origin, &out, [-100, 0, 100, 50]));
        assert!(!overlaps(origin, &out, [200, 0, 100, 50]));
    }

    #[test]
    fn each_pasted_pixel_comes_from_the_same_desktop_spot() {
        // A monitor at (100, 20) whose pixels carry their own position.
        let mon = RgbaImage::from_fn(60, 40, |x, y| image::Rgba([x as u8, y as u8, 7, 255]));
        let mut out = RgbaImage::new(30, 30);
        let origin = [140, 45];
        assert!(paste_monitor(&mut out, origin, [100, 20], &mon));
        // Desktop (150, 50) is monitor pixel (50, 30) and out pixel (10, 5).
        assert_eq!(out.get_pixel(10, 5).0, [50, 30, 7, 255]);
        assert_eq!(out.get_pixel(0, 0).0, [40, 25, 7, 255]);
        assert_eq!(out.get_pixel(19, 14).0, [59, 39, 7, 255]);
        // Right of and below the monitor stays transparent.
        assert_eq!(out.get_pixel(20, 0).0[3], 0);
        assert_eq!(out.get_pixel(0, 15).0[3], 0);
    }
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
