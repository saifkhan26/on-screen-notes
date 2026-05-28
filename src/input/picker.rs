//! Screen colour picker.
//!
//! Samples the colour of a single pixel on the user's monitors, given a
//! physical-pixel screen coordinate. Used by the "Shift + click" eyedropper
//! flow: the user holds Shift, taps a spot anywhere on screen, and the
//! pen tool's colour is set to whatever pixel was under the pointer.
//!
//! Implementation: `xcap` (already a dependency for the screenshot
//! pipeline) captures the monitor that contains the requested point, then
//! we read one pixel out of the returned `image::RgbaImage`.
//!
//! Caveats:
//!   * Each pick triggers a full-monitor capture. That is heavier than a
//!     GDI `GetPixel` would be, but avoids adding a Windows-only
//!     dependency and stays cross-platform.
//!   * If our overlay window itself has non-zero `bg_opacity`, the
//!     captured pixel will include our own tint. Callers should pick
//!     with the overlay either transparent (bg_opacity = 0) or
//!     temporarily hidden if pure source colour is required.

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
        let img = m.capture_image().ok()?;
        let lx = (screen_x - mx) as u32;
        let ly = (screen_y - my) as u32;
        if lx >= img.width() || ly >= img.height() {
            return None;
        }
        let p = img.get_pixel(lx, ly);
        return Some([p.0[0], p.0[1], p.0[2], 255]);
    }
    None
}
