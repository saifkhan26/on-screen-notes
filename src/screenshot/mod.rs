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

/// Capture the primary monitor, crop to the OSN window's screen
/// rect, and encode as PNG. The cropped image lines up with the
/// canvas-local origin so painting it at `[0, 0, logical_size]`
/// covers exactly the area OSN sits over — anything outside the
/// overlay is excluded.
///
/// `pixels_per_point` is the overlay's DPI scale; we divide physical
/// px by it to record the logical-size rect the renderer will draw
/// into. `osn_hwnd` is the platform-native handle of the OSN
/// window; 0 falls back to the full monitor.
pub fn capture_freeze_layer(
    pixels_per_point: f32,
    osn_hwnd: isize,
) -> Result<crate::canvas::canvas::LayerImage> {
    let img = capture_primary_image()?;
    let full_w = img.width() as i32;
    let full_h = img.height() as i32;

    // Resolve crop region in monitor-physical pixels. Falls back
    // to the whole monitor if we cannot read the OSN rect.
    let (cx, cy, cw, ch) = if osn_hwnd != 0 {
        match crate::platform::window_rect(osn_hwnd) {
            Some((l, t, r, b)) => {
                let l = l.max(0);
                let t = t.max(0);
                let r = r.min(full_w);
                let b = b.min(full_h);
                if r > l && b > t {
                    (l, t, r - l, b - t)
                } else {
                    (0, 0, full_w, full_h)
                }
            }
            None => (0, 0, full_w, full_h),
        }
    } else {
        (0, 0, full_w, full_h)
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
    Ok(crate::canvas::canvas::LayerImage {
        png: buf,
        size: [w, h],
        logical_size: [w as f32 / ppp, h as f32 / ppp],
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
