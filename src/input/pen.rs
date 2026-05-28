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
    ) -> crate::error::Result<std::sync::Arc<Self>> {
        let raw_window = src.window_handle().ok().map(|w| w.as_raw());
        Ok(std::sync::Arc::new(Self { raw_window }))
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
    pub fn new(_holder: std::sync::Arc<RawHandleHolder>) -> Self {
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

    /// Whether a real pen backend is initialised. UI status badge uses this.
    pub fn is_active(&self) -> bool {
        !matches!(self.backend, Backend::Disabled)
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
        PenEvent::Down(s) => tool.begin(s),
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

        /// True between Down and Up — used to emit synthetic `Move` only
        /// while the pen is in contact.
        is_down: bool,
        /// Most recent sample (used so `Up` always carries a position).
        last_sample: Option<PenSample>,
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

            log::info!(
                "Wintab opened: pressure_max = {pressure_max}, ctx = {:?}",
                ctx_handle
            );

            Ok(Self {
                wt_close,
                wt_queue,
                wt_data_get,
                ctx_handle,
                pressure_max,
                is_down: false,
                last_sample: None,
            })
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

            for p in &packets[..removed] {
                let pos = [
                    (p.pkXYZ.x - client_origin[0]) as f32,
                    (p.pkXYZ.y - client_origin[1]) as f32,
                ];
                let pressure = (p.pkNormalPressure as f32 / self.pressure_max).clamp(0.0, 1.0);
                let sample = PenSample {
                    pos,
                    pressure,
                    tilt: [0.0, 0.0],
                };
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
}
