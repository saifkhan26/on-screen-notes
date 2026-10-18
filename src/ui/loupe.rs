//! Magnifier loupe — a held-Z modal magnifier showing a live view of
//! the desktop sampled around the cursor.
//!
//! Each frame is the screen under the OSN window's client area, taken
//! from whichever monitors the window is on (see
//! `screenshot::capture_client_area`). Its top-left is the client
//! top-left, so a cursor position in egui points maps into it by
//! `pixels_per_point` alone.
//!
//! A background worker thread continuously calls `xcap` and drops
//! each captured frame into a shared slot. The main UI thread polls
//! the slot every frame and re-uploads the texture when a new frame
//! is ready. xcap costs 30-50 ms per call so doing the capture on
//! the UI thread would tank the overlay's frame rate — the worker
//! keeps the UI buttery while the loupe still tracks reality with
//! ~one capture-period of lag.

use crate::error::Result;
use crate::screenshot::capture_client_area;
use egui::{Color32, ColorImage, Pos2, Rect, TextureHandle, TextureOptions};
use image::RgbaImage;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// One armed loupe. Owns the texture handle plus the capture
/// worker; dropping the option signals the worker to stop and
/// releases the GPU memory.
pub struct LoupeState {
    /// Captured screen, uploaded as an egui texture. Refreshed by
    /// `poll_and_update` whenever the worker drops a new frame.
    pub snapshot: TextureHandle,
    /// Physical pixel size of the most recently uploaded frame.
    /// Needed to convert client-relative cursor coords into
    /// normalised UV space.
    pub size_px: [u32; 2],
    /// Magnification factor. 3.0 = 3× zoom (a 60-px square of source
    /// becomes a 180-px square on screen). Adjusted via the scroll
    /// wheel while the loupe is up.
    pub zoom: f32,
    /// On-screen radius (half-side, logical px) of the painted loupe
    /// square.
    pub radius_px: f32,
    /// Latest captured frame waiting to be uploaded. Worker writes,
    /// UI thread takes.
    latest: Arc<Mutex<Option<RgbaImage>>>,
    /// Set to true to ask the worker to exit.
    stop: Arc<AtomicBool>,
    /// Worker handle; kept so the OS reclaims the thread cleanly
    /// when the loupe drops.
    _worker: Option<JoinHandle<()>>,
    /// HWND of the OSN overlay so we can flip its
    /// `SetWindowDisplayAffinity` back to default on drop.
    osn_hwnd: isize,
}

impl LoupeState {
    /// Start a live loupe: grab one initial frame on the UI thread
    /// so we have something to paint immediately, then spawn the
    /// worker that keeps refreshing the frame in the background.
    ///
    /// Sets `WDA_EXCLUDEFROMCAPTURE` on the OSN HWND BEFORE the
    /// first capture so the worker never sees its own painted
    /// output. Without this the loupe captures the screen with
    /// itself rendered on it, paints that magnified, and the next
    /// capture sees a magnified version — feedback loop that
    /// zooms toward infinity in a few frames.
    pub fn start_live(ctx: &egui::Context, osn_hwnd: isize) -> Result<Self> {
        use crate::error::Context;
        crate::platform::set_window_capture_excluded(osn_hwnd, true);
        let img = capture_client_area(osn_hwnd).context("initial loupe capture")?;
        let (w, h) = (img.width(), img.height());
        let snapshot = ctx.load_texture("osn_loupe", rgba_to_color_image(&img), TextureOptions::LINEAR);

        let latest: Arc<Mutex<Option<RgbaImage>>> = Arc::new(Mutex::new(None));
        let stop: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
        let worker_latest = Arc::clone(&latest);
        let worker_stop = Arc::clone(&stop);
        let worker_ctx = ctx.clone();
        let worker = std::thread::Builder::new()
            .name("osn-loupe".into())
            .spawn(move || worker_loop(worker_latest, worker_stop, worker_ctx, osn_hwnd))
            .ok();

        Ok(Self {
            snapshot,
            size_px: [w, h],
            zoom: 3.0,
            radius_px: 110.0,
            latest,
            stop,
            _worker: worker,
            osn_hwnd,
        })
    }

    /// Drain the latest capture into the texture if the worker has
    /// dropped one since we last polled. Cheap when there is
    /// nothing new — one mutex try-lock.
    pub fn poll_and_update(&mut self) {
        let mut slot = match self.latest.lock() {
            Ok(s) => s,
            Err(_) => return,
        };
        if let Some(img) = slot.take() {
            let (w, h) = (img.width(), img.height());
            self.size_px = [w, h];
            self.snapshot.set(rgba_to_color_image(&img), TextureOptions::LINEAR);
        }
    }
}

impl Drop for LoupeState {
    fn drop(&mut self) {
        // Tell the worker to wind down. We don't join — the worker
        // checks the flag between captures and exits within ~one
        // capture period; the OS reclaims the thread.
        self.stop.store(true, Ordering::Release);
        // Re-allow the OSN window to appear in screenshots /
        // recordings now that the loupe is gone.
        crate::platform::set_window_capture_excluded(self.osn_hwnd, false);
    }
}

/// Worker loop: grab a frame, drop it into `latest`, repeat until
/// `stop` is set. Wakes the UI each successful capture so the
/// magnifier sees the new frame without waiting for an unrelated
/// event. The window rect is read afresh each capture, so moving OSN
/// to another monitor while the loupe is up follows along.
fn worker_loop(
    latest: Arc<Mutex<Option<RgbaImage>>>,
    stop: Arc<AtomicBool>,
    ctx: egui::Context,
    osn_hwnd: isize,
) {
    while !stop.load(Ordering::Acquire) {
        match capture_client_area(osn_hwnd) {
            Ok(img) => {
                if let Ok(mut slot) = latest.lock() {
                    *slot = Some(img);
                }
                ctx.request_repaint();
            }
            Err(e) => {
                log::debug!("loupe capture failed: {e}");
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        // Brief pause to keep CPU sane — capture itself blocks on
        // xcap so this is mostly a yield.
        std::thread::sleep(Duration::from_millis(4));
    }
}

fn rgba_to_color_image(img: &RgbaImage) -> ColorImage {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let pixels: Vec<Color32> = img
        .as_raw()
        .chunks_exact(4)
        .map(|c| Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]))
        .collect();
    ColorImage { size: [w, h], pixels }
}

/// Paint the loupe into the overlay. `cursor_logical` is the egui
/// pointer position in logical pixels relative to the client top-left
/// (same coord space as the canvas rect). `ppp` is
/// `ctx.pixels_per_point()` — converts cursor logical → physical pixels
/// so the uv sub-rect lines up with the captured frame.
pub fn paint(
    painter: &egui::Painter,
    overlay_rect: Rect,
    state: &LoupeState,
    cursor_logical: Pos2,
    ppp: f32,
) {
    let zoom = state.zoom.max(1.0);
    let r = state.radius_px;
    let dest = Rect::from_min_max(
        egui::pos2(cursor_logical.x - r, cursor_logical.y - r),
        egui::pos2(cursor_logical.x + r, cursor_logical.y + r),
    );
    let dest = dest.intersect(overlay_rect);
    if dest.width() <= 0.0 || dest.height() <= 0.0 { return; }

    let uv = source_uv(dest, cursor_logical, zoom, ppp, state.size_px);
    painter.image(state.snapshot.id(), dest, uv, Color32::WHITE);
}

/// The part of the captured frame (`size_px`, covering the client area)
/// to stretch over `dest`. Each point `d` of `dest` shows the screen at
/// `cursor + (d - cursor) / zoom` — the same magnification about the
/// cursor that the ink is re-rendered with — so where `dest` is cut off
/// by the window edge the picture is cut off too, not squeezed.
fn source_uv(dest: Rect, cursor: Pos2, zoom: f32, ppp: f32, size_px: [u32; 2]) -> Rect {
    let (tw, th) = (size_px[0].max(1) as f32, size_px[1].max(1) as f32);
    let uv = |d: Pos2| {
        let src = cursor + (d - cursor) / zoom;
        egui::pos2(src.x * ppp / tw, src.y * ppp / th)
    };
    Rect::from_min_max(uv(dest.min), uv(dest.max))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(c: Pos2, r: f32) -> Rect {
        Rect::from_center_size(c, egui::vec2(2.0 * r, 2.0 * r))
    }

    #[test]
    fn the_cursor_maps_to_its_client_pixel() {
        // A 1000×800 px client area at 150 %: point (100, 200) is pixel
        // (150, 300), wherever the window sits on the desktop.
        let c = egui::pos2(100.0, 200.0);
        let uv = source_uv(square(c, 30.0), c, 3.0, 1.5, [1000, 800]);
        assert!((uv.center().x - 0.15).abs() < 1e-6);
        assert!((uv.center().y - 0.375).abs() < 1e-6);
        // ±30 pt shown 3× = ±10 pt of source = ±15 px.
        assert!((uv.width() * 1000.0 - 30.0).abs() < 1e-3);
        assert!((uv.height() * 800.0 - 30.0).abs() < 1e-3);
    }

    #[test]
    fn a_loupe_cut_off_by_the_window_edge_is_not_squeezed() {
        // Cursor 10 pt from the left edge, radius 40, zoom 2: the left
        // edge of `dest` (x = 0) shows x = 10 - 10/2 = 5 of the source.
        let c = egui::pos2(10.0, 50.0);
        let dest = square(c, 40.0).intersect(Rect::from_min_size(Pos2::ZERO, egui::vec2(200.0, 200.0)));
        let uv = source_uv(dest, c, 2.0, 1.0, [200, 200]);
        assert!((uv.min.x * 200.0 - 5.0).abs() < 1e-4);
        assert!((uv.max.x * 200.0 - 30.0).abs() < 1e-4);
        // Same scale both ways: source spans half of `dest`.
        assert!((uv.width() * 200.0 - dest.width() / 2.0).abs() < 1e-4);
        assert!((uv.height() * 200.0 - dest.height() / 2.0).abs() < 1e-4);
    }
}
