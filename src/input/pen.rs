//! Pen / stylus input bridge.
//!
//! ## Cross-platform plan
//!
//! | Platform | Backend |
//! |----------|--------|
//! | Windows  | Wintab via `wintab_lite` (poll once per frame, main thread). |
//! | Linux    | Mouse fallback for now. *Future:* `octotablet` (Wayland tablet-v2 / X11 XInput2). |
//! | macOS    | Mouse fallback for now. *Future:* NSEvent pressure. |
//!
//! ## Why Wintab and not IRealTimeStylus / WM_POINTER on Windows
//!
//! IRealTimeStylus (used by `octotablet 0.1`) installs a COM event sink on
//! our window that, in combination with NVIDIA's Vulkan present-mode
//! advertising and OBS's Vulkan capture layer, starves the message pump and
//! makes Windows mark the window "(Not Responding)". Wintab is a *polling*
//! API: we call `WTQueuePacketsEx` once per frame, drain new packets, and
//! return. No hooks, no message-pump interference.
//!
//! Almost every tablet driver still ships a Wintab DLL for Photoshop / Krita
//! compatibility (Wacom, Huion, XP-Pen, Gaomon, …) so this picks up
//! pressure across the entire mainstream tablet ecosystem.
//!
//! ## Coordinate space
//!
//! `PenSample::pos` is in *window-client* pixels (top-left of the window's
//! client area is `(0, 0)`). The caller (`app.rs`) further subtracts the
//! canvas rect's top-left and applies the canvas pan/zoom to land in
//! canvas-local coordinates before feeding tools.

use crate::canvas::stroke::PenSample;
use crate::tools::ActiveTool;

/// One application-level pen event.
#[derive(Debug, Clone, Copy)]
pub enum PenEvent {
    Down(PenSample),
    Move(PenSample),
    Up(PenSample),
}

/// Window-handle holder. Different backends need different handle types,
/// so we store the raw handles and pull out what each backend needs.
pub struct RawHandleHolder {
    /// Captured raw window handle (HWND on Windows). `None` if capture
    /// failed (extremely rare).
    #[allow(dead_code)]
    raw_window: Option<raw_window_handle::RawWindowHandle>,
}

impl RawHandleHolder {
    /// Capture handles from a `HasWindowHandle + HasDisplayHandle` source
    /// (in practice `&eframe::CreationContext`).
    pub fn capture(
        src: &(impl raw_window_handle::HasWindowHandle + raw_window_handle::HasDisplayHandle),
    ) -> crate::error::Result<Self> {
        let raw_window = src.window_handle().ok().map(|w| w.as_raw());
        Ok(Self { raw_window })
    }
}

// =============================================================================
// Backend dispatcher: PenInput hides the platform-specific impl.
// =============================================================================

/// Public pen-input handle. The actual backend lives in `backend` and is
/// chosen by `cfg`.
pub struct PenInput {
    backend: Backend,
}

enum Backend {
    /// No active backend (other OSes, or Wintab DLL missing).
    Disabled,
    #[cfg(target_os = "windows")]
    Wintab(windows_backend::Wintab),
}

impl PenInput {
    /// Try to bring up the platform pen backend. Failures degrade to
    /// `Disabled` (mouse fallback still works).
    pub fn new(_holder: RawHandleHolder) -> Self {
        #[cfg(target_os = "windows")]
        {
            match windows_backend::Wintab::try_init(&_holder) {
                Ok(w) => {
                    log::info!("Wintab pen backend ready");
                    return Self { backend: Backend::Wintab(w) };
                }
                Err(e) => {
                    log::warn!(
                        "Wintab init failed ({e}); pen pressure disabled. \
                         Mouse fallback still works."
                    );
                }
            }
        }
        Self { backend: Backend::Disabled }
    }

    /// Disabled instance: used when the user sets `OSN_NO_PEN`, or window
    /// handle capture failed.
    pub fn disabled() -> Self { Self { backend: Backend::Disabled } }

    /// Drain new pen events. Called once per frame from `app::update`.
    /// `client_origin` is the screen-pixel position of the window's top-left
    /// (client area). Backends subtract it so emitted samples are in
    /// window-client space.
    pub fn poll(&mut self, _client_origin: [i32; 2]) -> Vec<PenEvent> {
        match &mut self.backend {
            Backend::Disabled => Vec::new(),
            #[cfg(target_os = "windows")]
            Backend::Wintab(w) => w.poll(_client_origin),
        }
    }

    /// True while the pen tip is touching the tablet.
    pub fn is_down(&self) -> bool {
        match &self.backend {
            Backend::Disabled => false,
            #[cfg(target_os = "windows")]
            Backend::Wintab(w) => w.is_down(),
        }
    }

    /// True if any packet arrived within the last `window` — the pen is
    /// in range, even if it is holding still. Wider than `used_within`.
    pub fn seen_within(&self, window: std::time::Duration) -> bool {
        match &self.backend {
            Backend::Disabled => false,
            #[cfg(target_os = "windows")]
            Backend::Wintab(w) => w.seen_within(window),
        }
    }

    /// True if the pen pressed or visibly moved within the last
    /// `window`. A tablet driver also moves the system cursor and
    /// presses the left mouse button on the pen's behalf, so the mouse
    /// path has to stand down while this is true or every pen stroke
    /// would be drawn twice. With the pen out of range (or resting
    /// still) this goes false and the mouse draws.
    pub fn used_within(&self, window: std::time::Duration) -> bool {
        match &self.backend {
            Backend::Disabled => false,
            #[cfg(target_os = "windows")]
            Backend::Wintab(w) => w.used_within(window),
        }
    }
}

/// Translate a `PenEvent` into tool actions. Lives next to the event type
/// so callers don't need to import variants.
pub fn dispatch(
    event: PenEvent,
    tool: &mut ActiveTool,
    canvas: &mut crate::canvas::canvas::Canvas,
) {
    match event {
        PenEvent::Down(s) => tool.begin(s, canvas),
        PenEvent::Move(s) => tool.update(s, canvas),
        PenEvent::Up(s)   => tool.end(s, canvas),
    }
}

// =============================================================================
// Windows Wintab backend.
// =============================================================================
#[cfg(target_os = "windows")]
mod windows_backend {
    //! Loads `Wintab32.dll` at runtime, opens a "system context" tied to our
    //! HWND, and on each `poll()` drains any new packets. Each packet
    //! becomes a `PenSample` in window-client coordinates with normalised
    //! pressure 0.0..=1.0.

    use super::{PenEvent, RawHandleHolder};
    use crate::canvas::stroke::PenSample;
    use crate::error::{anyhow, Context, Result};
    use libloading::Library;
    use windows::Win32::Foundation::HWND;
    use wintab_lite::{
        cast_void, Packet, WTClose, WTDataGet, WTInfo, WTOpen, WTQueuePacketsEx, AXIS, CXO, DVC,
        LOGCONTEXT, WTI, WTPKT,
    };

    /// `WTGetA`: reads an open context's settings back. wintab_lite does
    /// not wrap it.
    type WTGet = unsafe extern "system" fn(*mut wintab_lite::HCTX, *mut LOGCONTEXT) -> i32;

    /// Linear map from a context's output space to screen pixels: the
    /// output area of the context as opened onto the one the default
    /// context reports, which is the screen. Extents are kept positive;
    /// the sign only says which way the axis runs, and that is the same
    /// in both.
    #[derive(Debug, Clone, Copy, PartialEq)]
    pub(super) struct CoordMap {
        pub(super) out_org: [f64; 2],
        pub(super) out_ext: [f64; 2],
        pub(super) screen_org: [f64; 2],
        pub(super) screen_ext: [f64; 2],
    }

    impl CoordMap {
        /// The map that leaves `ctx`'s output coordinates unchanged.
        pub(super) fn from_context(ctx: &LOGCONTEXT) -> Self {
            let org = [ctx.lcOutOrgXYZ.x as f64, ctx.lcOutOrgXYZ.y as f64];
            let ext = [
                (ctx.lcOutExtXYZ.x as f64).abs().max(1.0),
                (ctx.lcOutExtXYZ.y as f64).abs().max(1.0),
            ];
            Self { out_org: org, out_ext: ext, screen_org: org, screen_ext: ext }
        }

        pub(super) fn to_screen(self, x: i32, y: i32) -> [f64; 2] {
            let f = |i: usize, v: i32| {
                self.screen_org[i] + (v as f64 - self.out_org[i]) * self.screen_ext[i] / self.out_ext[i]
            };
            [f(0, x), f(1, y)]
        }
    }

    /// One open Wintab context.
    ///
    /// `wintab_lite` defines its function-pointer types (e.g. `WTOpen`) as
    /// `libloading::Symbol<'a, fn(...)>`. To dodge the resulting
    /// self-referential lifetime, we leak the `Library` so the symbols are
    /// `'static`. The DLL stays loaded for the rest of the process — fine,
    /// since nothing else needs to unload it.
    pub struct Wintab {
        wt_close:    WTClose<'static>,
        wt_queue:    WTQueuePacketsEx<'static>,
        wt_data_get: WTDataGet<'static>,
        /// Pointer to a Wintab `HCTX`. We use it as opaque; only WTClose
        /// dereferences it, and only when our struct drops.
        ctx_handle: *mut wintab_lite::HCTX,

        /// Tablet pressure max value reported by the driver. We divide
        /// `pkNormalPressure` by this to get a 0..1 float.
        pressure_max: f32,

        /// Packet coordinates → screen pixels.
        map: CoordMap,

        /// True between Down and Up — used to emit synthetic `Move` only
        /// while the pen is in contact.
        is_down: bool,
        /// Most recent sample (used so `Up` always carries a position).
        last_sample: Option<PenSample>,
        /// When the pen last pressed, or moved by more than sensor
        /// jitter. A pen lying still on the tablet stays "in range" and
        /// can keep emitting packets; those must not count as use, or
        /// the mouse would be locked out for as long as it lies there.
        last_used: Option<std::time::Instant>,
        /// Where the pen was when it last counted as having moved.
        rest_anchor: Option<[f32; 2]>,
        /// When the last packet of any kind arrived.
        last_packet: Option<std::time::Instant>,
    }

    // Wintab handles are not Send / Sync; we never move Wintab off the main
    // thread, so this is fine.

    impl Wintab {
        /// Try to load Wintab and open a system context for our window.
        /// Returns `Err` if the DLL is missing (no tablet driver), the HWND
        /// is unavailable, or `WTOpen` fails.
        pub fn try_init(holder: &RawHandleHolder) -> Result<Self> {
            // 1) Extract HWND from the captured raw handle.
            let hwnd = match holder.raw_window {
                Some(raw_window_handle::RawWindowHandle::Win32(h)) => HWND(h.hwnd.get() as _),
                Some(_) => return Err(anyhow!("raw window handle is not Win32")),
                None    => return Err(anyhow!("no raw window handle captured")),
            };

            // 2) Load Wintab32.dll. Will fail if no tablet driver is installed.
            //    We Box::leak the Library so the resolved symbols can be
            //    held as `'static` (avoids a self-referential struct).
            let lib: &'static Library = Box::leak(Box::new(
                unsafe { Library::new("Wintab32.dll") }
                    .context("Wintab32.dll not found (no tablet driver?)")?,
            ));

            // 3) Resolve the function pointers we need. `wintab_lite`
            //    provides the *types* (function-pointer signatures wrapped
            //    in `libloading::Symbol`); we look them up by name.
            let wt_open: WTOpen<'static> =
                unsafe { lib.get(c"WTOpenA".to_bytes()).context("WTOpenA")? };
            let wt_info: WTInfo<'static> =
                unsafe { lib.get(c"WTInfoA".to_bytes()).context("WTInfoA")? };
            let wt_close: WTClose<'static> =
                unsafe { lib.get(c"WTClose".to_bytes()).context("WTClose")? };
            let wt_queue: WTQueuePacketsEx<'static> = unsafe {
                lib.get(c"WTQueuePacketsEx".to_bytes())
                    .context("WTQueuePacketsEx")?
            };
            let wt_data_get: WTDataGet<'static> = unsafe {
                lib.get(c"WTDataGet".to_bytes()).context("WTDataGet")?
            };

            // 4) Get the default system context as a starting template.
            let mut log_context = LOGCONTEXT::default();
            let r = unsafe { wt_info(WTI::DEFSYSCTX, 0, cast_void!(log_context)) };
            if r == 0 {
                return Err(anyhow!("WTInfo(DEFSYSCTX) failed"));
            }

            // 5) Configure the context.
            //    * Name: any string; helps debugging.
            //    * CXO::SYSTEM: tablet output coords are in *screen pixels*.
            //    * lcPktData = ALL: receive every available field per packet.
            //    * lcPktMode = empty: all fields are absolute (not relative).
            //    * lcMoveMask = X|Y|NORMAL_PRESSURE: only emit packet on
            //      change to one of these (less queue spam).
            log_context.lcName.write_str("on-screen-notes");
            log_context.lcOptions |= CXO::SYSTEM;
            log_context.lcPktData = WTPKT::all();
            log_context.lcPktMode = WTPKT::empty();
            log_context.lcMoveMask = WTPKT::X | WTPKT::Y | WTPKT::NORMAL_PRESSURE;

            // 6) Wintab's default Y axis is flipped vs Windows screen
            //    coordinates. Negate the output extent so packet.y matches
            //    GDI's "Y grows down".
            let default_y_extent = log_context.lcOutExtXYZ.y;
            log_context.lcOutExtXYZ.y = -default_y_extent;

            // 6b) Sub-pixel positions. The default output space is whole
            //     screen pixels, so every packet is rounded to a pixel: a
            //     slow or shallow line becomes a staircase, and a stroke
            //     drawn zoomed out shows its steps blown up when zoomed in.
            //     Asking for an output space `k` times finer over the same
            //     screen area keeps the mapping and adds the tablet's own
            //     precision. `k` stays within the tablet's resolution.
            let screen = CoordMap::from_context(&log_context);
            let wt_get: Option<WTGet> = unsafe { lib.get(c"WTGetA".to_bytes()) }.ok().map(|s| *s);
            let k = if wt_get.is_some() {
                let per_px = log_context.lcInExtXYZ.x / log_context.lcOutExtXYZ.x.abs().max(1);
                per_px.clamp(1, 16)
            } else {
                1
            };
            log_context.lcOutOrgXYZ.x *= k;
            log_context.lcOutOrgXYZ.y *= k;
            log_context.lcOutExtXYZ.x *= k;
            log_context.lcOutExtXYZ.y *= k;

            // 7) Look up tablet pressure axis to know its max value (used
            //    later to normalise to 0..=1). Drivers commonly report
            //    1023 or 8191; we read whatever the device says.
            let mut pressure_axis = AXIS::default();
            let pr =
                unsafe { wt_info(WTI::DEVICES, DVC::NPRESSURE as u32, cast_void!(pressure_axis)) };
            let pressure_max = if pr as usize == std::mem::size_of::<AXIS>() {
                pressure_axis.axMax as f32
            } else {
                // Some drivers don't expose NPRESSURE; assume a sane default
                // so we still get useful values.
                1023.0
            };
            if pressure_max < 1.0 {
                return Err(anyhow!("invalid pressure_max from driver: {pressure_max}"));
            }

            // 8) Open the context. Third arg `1` = enabled immediately.
            let ctx_handle = unsafe { wt_open(hwnd, &mut log_context, 1) };
            if ctx_handle.is_null() {
                return Err(anyhow!("WTOpen returned null (driver refused)"));
            }

            // Read back the output space the driver settled on, so a
            // driver that ignored the finer one still maps 1:1.
            let mut opened = log_context;
            let out = match wt_get {
                Some(get) if unsafe { get(ctx_handle, &mut opened) } != 0 => CoordMap::from_context(&opened),
                _ => CoordMap::from_context(&log_context),
            };
            let map = CoordMap { screen_org: screen.out_org, screen_ext: screen.out_ext, ..out };

            log::info!(
                "Wintab opened: pressure_max = {pressure_max}, ctx = {:?}, {map:?}",
                ctx_handle
            );

            Ok(Self {
                wt_close,
                wt_queue,
                wt_data_get,
                ctx_handle,
                pressure_max,
                map,
                is_down: false,
                last_sample: None,
                last_used: None,
                rest_anchor: None,
                last_packet: None,
            })
        }

        pub fn is_down(&self) -> bool { self.is_down }

        pub fn used_within(&self, window: std::time::Duration) -> bool {
            self.last_used.is_some_and(|t| t.elapsed() <= window)
        }

        pub fn seen_within(&self, window: std::time::Duration) -> bool {
            self.last_packet.is_some_and(|t| t.elapsed() <= window)
        }

        /// Drain any new packets and translate into `PenEvent`s.
        ///
        /// `client_origin` is the screen-pixel coordinate of the window's
        /// client-area top-left. Subtracting it converts each packet's
        /// screen-pixel position into window-client space.
        pub fn poll(&mut self, client_origin: [i32; 2]) -> Vec<PenEvent> {
            let mut out = Vec::new();
            let mut from = 0u32;
            let mut to = 0u32;
            // Quick "any packets queued?" check.
            let any = unsafe { (self.wt_queue)(self.ctx_handle, &mut from, &mut to) };
            if any == 0 {
                return out;
            }

            // Drain up to 64 packets in one go. 64 keeps us well under the
            // typical 128-deep driver queue while bounding the worst-case
            // copy cost.
            const MAX: usize = 64;
            let mut packets: [Packet; MAX] = core::array::from_fn(|_| Packet::default());
            // WTDataGet writes the count of removed packets here. It expects
            // an `i32 *` (LPINT) per the Wintab API spec.
            let mut removed: i32 = 0;
            let _ = unsafe {
                (self.wt_data_get)(
                    self.ctx_handle,
                    from,
                    to,
                    MAX as i32,
                    cast_void!(packets),
                    &mut removed,
                )
            };
            let removed = removed as usize;
            if removed > 0 {
                self.last_packet = Some(std::time::Instant::now());
            }

            for p in &packets[..removed] {
                let s = self.map.to_screen(p.pkXYZ.x, p.pkXYZ.y);
                let pos = [
                    (s[0] - client_origin[0] as f64) as f32,
                    (s[1] - client_origin[1] as f64) as f32,
                ];
                let pressure = (p.pkNormalPressure as f32 / self.pressure_max).clamp(0.0, 1.0);
                let sample = PenSample {
                    pos,
                    pressure,
                    tilt: [0.0, 0.0],
                };
                // Two pixels clears the jitter of a pen at rest. Measured
                // from where the pen last counted as moved rather than
                // from the previous packet, so a slow glide still adds up.
                let moved = self.rest_anchor.map_or(true, |a| {
                    (a[0] - pos[0]).abs() >= 2.0 || (a[1] - pos[1]).abs() >= 2.0
                });
                if moved {
                    self.rest_anchor = Some(pos);
                }
                if pressure > 0.0 || moved {
                    self.last_used = Some(std::time::Instant::now());
                }
                self.last_sample = Some(sample);

                // Edge detection: we treat "pressure > 0" as the pen being
                // down. Some drivers still emit pressure 0 packets on hover;
                // those become Move events but only after the pen first
                // became "down" once.
                let pen_down_now = pressure > 0.0;
                match (pen_down_now, self.is_down) {
                    (true, false) => {
                        self.is_down = true;
                        out.push(PenEvent::Down(sample));
                    }
                    (true, true) => {
                        out.push(PenEvent::Move(sample));
                    }
                    (false, true) => {
                        self.is_down = false;
                        out.push(PenEvent::Up(sample));
                    }
                    (false, false) => {}
                }
            }
            out
        }
    }

    impl Drop for Wintab {
        fn drop(&mut self) {
            // Close the Wintab context before letting the DLL unload.
            let _ = unsafe { (self.wt_close)(self.ctx_handle) };
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// A default context for a desktop whose left screen starts at x = -1920.
        fn desktop() -> LOGCONTEXT {
            let mut c = LOGCONTEXT::default();
            c.lcOutOrgXYZ.x = -1920;
            c.lcOutOrgXYZ.y = 0;
            c.lcOutExtXYZ.x = 4480;
            c.lcOutExtXYZ.y = -1440;
            c
        }

        #[test]
        fn an_unchanged_context_maps_one_to_one() {
            let c = desktop();
            let m = CoordMap::from_context(&c);
            assert_eq!(m.to_screen(-1920, 0), [-1920.0, 0.0]);
            assert_eq!(m.to_screen(1234, 567), [1234.0, 567.0]);
        }

        #[test]
        fn a_finer_output_space_gives_fractions_of_a_pixel() {
            let c = desktop();
            let mut fine = c;
            for v in [&mut fine.lcOutOrgXYZ.x, &mut fine.lcOutOrgXYZ.y, &mut fine.lcOutExtXYZ.x, &mut fine.lcOutExtXYZ.y] {
                *v *= 8;
            }
            let screen = CoordMap::from_context(&c);
            let m = CoordMap { screen_org: screen.out_org, screen_ext: screen.out_ext, ..CoordMap::from_context(&fine) };
            assert_eq!(m.to_screen(-1920 * 8, 0), [-1920.0, 0.0]);
            assert_eq!(m.to_screen(1234 * 8 + 3, 567 * 8 + 4), [1234.375, 567.5]);
        }
    }
}
