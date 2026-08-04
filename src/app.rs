//! `OnScreenNotesApp` — the top-level type that ties every subsystem together.
//!
//! Responsibilities:
//!   * Holds the `CanvasManager`, `ActiveTool`, `PenInput`, `Hotkeys`,
//!     `OverlayCache`, etc.
//!   * Implements `eframe::App::update`, which is called every frame.
//!   * Delegates work to the right subsystem (UI, interaction, screenshot).
//!
//! Beginner note: in eframe, `update` is the *whole* per-frame entry point.
//! Anything that should happen "live" (input handling, drawing, etc.)
//! happens inside it. eframe calls it as fast as the display can repaint
//! when there is something to redraw.

use std::time::{Duration, Instant};

use crate::canvas::CanvasManager;
use crate::config::AppConfig;
use crate::input::{
    hotkeys::{HotkeyId, Hotkeys},
    modifiers::ModifierTracker,
    pen::{PenInput, RawHandleHolder},
};
use crate::interaction;
use crate::interaction::click_through;
use crate::tools::ActiveTool;
use crate::ui;

pub struct OnScreenNotesApp {
    config: AppConfig,
    canvases: CanvasManager,
    tool: ActiveTool,
    pen: PenInput,
    hotkeys: Option<Hotkeys>,
    modifiers: ModifierTracker,
    overlay_cache: ui::overlay::OverlayCache,

    /// Whether the main overlay is shown. Toggled by the global hotkey
    /// (default Ctrl+Shift+N).
    overlay_visible: bool,
    /// Whether mouse left-button was down on the previous frame; used to
    /// synthesise pen events from mouse for the no-tablet fallback.
    mouse_was_down: bool,

    /// Debounce timer for "save canvases to disk".
    last_save_at: Instant,

    /// Set to false after the very first `update` runs. Used to push
    /// initial-state viewport commands (e.g. clear click-through).
    first_frame: bool,

    /// Anchor for "hold Space + drag to pan". `Some(p)` while the user is
    /// actively dragging — `p` is the previous cursor position in window-
    /// client pixels. We compute the per-frame delta against this anchor
    /// and apply it to `canvas.pan`, then move the anchor forward.
    pan_drag_anchor: Option<egui::Pos2>,

    /// Active **Shift**+Space layer pan: `(layer index, accumulated
    /// canvas-local delta)`. `None` means the current Space drag pans the
    /// whole canvas instead.
    ///
    /// The layer index is latched when the gesture starts, so switching
    /// the active layer — or letting go of Shift — mid-drag cannot
    /// redirect the move to a different layer. The delta accumulates in
    /// canvas-local units (screen delta ÷ zoom) and is baked into the
    /// layer's geometry by `Canvas::move_layer_by` when the drag ends;
    /// until then it only shifts that layer at paint time.
    layer_drag: Option<LayerDrag>,

    /// One Euro Filter state for incoming pen samples. Reset on every
    /// pen-down so a new stroke does not inherit the previous stroke's
    /// filter state. See `input::filter` for the algorithm rationale.
    pen_filter: crate::input::filter::PenFilter,

    /// True while a pen-down landed on a floating UI layer (toolbar,
    /// popup, etc.). Subsequent Move events are dropped until the next
    /// Up so the user can interact with the menu without leaving ink
    /// behind it. Wintab bypasses the egui pointer, so we have to
    /// gate dispatch ourselves.
    pen_blocked: bool,

    /// Eraser tool used while Ctrl is held — temporary override so the
    /// user can erase mid-session without losing their current pen
    /// tool. Stays the same instance across gestures so its in-progress
    /// stroke state is preserved.
    eraser_override: ActiveTool,
    /// True while the current pen gesture (Down→Up) was launched with
    /// Ctrl held. Locks the dispatch target for the rest of the
    /// gesture so releasing Ctrl mid-stroke does not accidentally flip
    /// the destination tool.
    pen_using_eraser: bool,

    /// True while a Shift-pick gesture is in progress: every Move and
    /// Up after the picking Down is silently swallowed so the user
    /// does not leave ink behind while sampling.
    pen_picking: bool,

    /// Master visibility switch for floating UI (toolbar, status badge,
    /// layer panel, etc.). Toggled by the `Tab` shortcut so the user
    /// can hide the chrome and view their drawing un-occluded.
    /// Stroke + shape rendering is unaffected by this flag.
    ui_visible: bool,

    /// Native window handle of the overlay viewport, stored as the
    /// platform-native integer (HWND cast to `isize` on Windows, 0 on
    /// other platforms or when capture fails). Used by
    /// `crate::platform` to set `WS_EX_NOACTIVATE` and to reclaim the
    /// foreground after a click-through. 0 disables both calls.
    osn_hwnd: isize,

    /// Screen-recording state machine. Idle by default; toggled on /
    /// off via `Ctrl+Shift+R`. While capturing we hide the floating UI
    /// so it doesn't end up in the saved GIF.
    recorder: crate::recording::Recorder,

    /// Saved `ui_visible` value at the moment recording started, so we
    /// can restore the user's chrome state after the recording
    /// finishes — they might have had Tab-hidden the UI before, in
    /// which case we shouldn't force it back on.
    ui_visible_before_record: bool,

    /// Toast message + expiry to flash a "saved to <path>" hint after
    /// a recording finishes. `None` = no toast active.
    toast: Option<(String, std::time::Instant)>,

    /// Deferred-screenshot state. Set when the user presses the
    /// screenshot hotkey: we hide the floating UI + drop the
    /// background tint for two frames so the OS compositor has time
    /// to repaint OSN without its chrome, then trigger the actual
    /// xcap capture, then restore the prior UI state.
    pending_screenshot: Option<PendingShot>,

    /// Active magnifier loupe. `Some` while the user holds `Z`; the
    /// snapshot is captured once on the rising edge and dropped on
    /// the falling edge, so re-pressing Z grabs a fresh frame.
    loupe: Option<ui::loupe::LoupeState>,

    /// Deferred freeze-frame-to-layer state. Same chrome-hide +
    /// frame-wait dance as `pending_screenshot`, but the captured
    /// frame is inserted as a new bottom layer on the active canvas
    /// instead of being saved to disk.
    pending_freeze: Option<PendingShot>,

    /// HWND of the window OSN is currently pinned to follow, stored
    /// as the platform-native integer. `None` = no pin. On Windows,
    /// each frame we read `GetWindowRect(hwnd)` and re-issue
    /// `ViewportCommand::OuterPosition` + `InnerSize` only when the
    /// rect changes.
    pinned_hwnd: Option<isize>,
    /// True while the user has armed pin-picking via the toolbar
    /// button; the next external LMB-down captures whichever
    /// top-level window the cursor sits over and stores it in
    /// `pinned_hwnd`.
    pin_picking: bool,
    /// LMB state on the previous frame — used to detect the rising
    /// edge of a click during pin-picking.
    pin_prev_lmb: bool,
    /// Last rect applied via `ViewportCommand` so we can skip the
    /// re-emit when the target window hasn't moved.
    pinned_last_rect: Option<(i32, i32, i32, i32)>,

    /// `true` if `ffmpeg -version` succeeded at app startup. Drives
    /// the MP4-vs-GIF branch in `toggle_record`.
    ffmpeg_available: bool,
}

/// State held while a screenshot is in flight.
struct PendingShot {
    ui_visible_before: bool,
    bg_opacity_before: f32,
    /// Frames remaining before we trigger `capture_and_save`. We
    /// need to wait at least one frame so the OS compositor has
    /// time to repaint after the chrome was hidden, otherwise xcap
    /// still picks up the toolbar.
    frames_to_wait: u32,
    /// DPI scale captured at the moment the hotkey fired so a
    /// later DPI change between request and capture doesn't break
    /// alignment.
    pixels_per_point: f32,
}

/// An in-flight Shift+Space layer pan.
///
/// `canvas` and `layer` are latched when the gesture starts so the move
/// lands where the user aimed it even if they switch sheets or layers
/// before letting go. `delta` is canvas-local, and stays a preview-only
/// offset until the gesture ends — see `Canvas::move_layer_by` for why
/// the translation is baked into the geometry rather than kept as a
/// per-layer transform.
#[derive(Clone, Copy)]
struct LayerDrag {
    canvas: usize,
    layer: usize,
    delta: [f32; 2],
}

impl OnScreenNotesApp {
    /// Constructed by `eframe` once on startup. We capture the OS handles
    /// from `cc` so octotablet can attach to the same window.
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // Apply our project-wide theme once. Every subsequent widget
        // inherits these colours, rounding, and spacing.
        ui::theme::apply(&cc.egui_ctx);

        // Install egui_extras image loaders (SVG, plus the default raster
        // formats). This is what makes `egui::include_image!("...svg")` work
        // in the toolbar. Must be called exactly once, before any `Image`
        // widget renders.
        egui_extras::install_image_loaders(&cc.egui_ctx);

        // Register a "handwritten" font family used by the Text tool's
        // `Shape::Text` renderer. It defaults to the same fallback
        // chain as `Proportional` so text renders even when no
        // dedicated handwriting TTF is bundled. If we later drop a
        // file at `assets/handwritten.ttf`, swap the binding here:
        //
        //   const HANDWRITTEN: &[u8] = include_bytes!("../assets/handwritten.ttf");
        //   fonts.font_data.insert("handwritten".into(), FontData::from_static(HANDWRITTEN));
        //   fonts.families.insert(
        //       FontFamily::Name("handwritten".into()),
        //       vec!["handwritten".into()],
        //   );
        //   cc.egui_ctx.set_fonts(fonts);
        //
        // For now we just alias the family to the default
        // proportional fallbacks so `FontFamily::Name("handwritten")`
        // does not silently drop text.
        {
            use egui::FontFamily;
            let mut fonts = egui::FontDefinitions::default();
            let prop_chain = fonts
                .families
                .get(&FontFamily::Proportional)
                .cloned()
                .unwrap_or_default();
            fonts
                .families
                .insert(FontFamily::Name("handwritten".into()), prop_chain);
            cc.egui_ctx.set_fonts(fonts);
        }

        let config = crate::persistence::load_config();
        let canvases = CanvasManager::load_or_default(&config);
        let tool = ActiveTool::from_config(&config);

        // Capture window/display handles into our holder; pen module needs them.
        // If capture fails (extremely rare), we run without pen pressure.
        // Set OSN_NO_PEN=1 to bypass octotablet entirely — useful for
        // diagnosing message-pump stalls caused by IRealTimeStylus on
        // some Windows tablet drivers.
        let pen = if std::env::var_os("OSN_NO_PEN").is_some() {
            log::info!("OSN_NO_PEN set; running without pen tablet");
            PenInput::disabled()
        } else {
            match RawHandleHolder::capture(cc) {
                Ok(holder) => PenInput::new(holder),
                Err(e) => {
                    log::warn!("could not capture window handles ({e}); pen disabled");
                    PenInput::disabled()
                }
            }
        };

        let hotkeys = match Hotkeys::register(&config) {
            Ok(h) => Some(h),
            Err(e) => {
                log::warn!("could not register global hotkeys: {e}");
                None
            }
        };

        // Capture level before `config` is moved into the struct below.
        let level = config.smoothing_level;
        // Push the persisted pressure-curve value into the global
        // atomic so stroke caches built before the first render pick
        // it up.
        crate::canvas::stroke::set_pressure_curve(config.pressure_curve);
        let mut eraser_override = ActiveTool::from_config(&config);
        eraser_override.kind = crate::tools::ToolKind::Eraser;

        // Grab the overlay's native window handle once — `crate::platform`
        // uses it to set `WS_EX_NOACTIVATE` and to reclaim the foreground
        // after the user's clicks pass through to the app behind. A
        // failure here is non-fatal: the platform helpers silently no-op
        // when the handle is 0.
        let osn_hwnd: isize = {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            match cc.window_handle() {
                Ok(h) => match h.as_raw() {
                    #[cfg(target_os = "windows")]
                    RawWindowHandle::Win32(w) => w.hwnd.get() as isize,
                    _ => 0,
                },
                Err(e) => {
                    log::warn!("could not capture overlay HWND: {e}");
                    0
                }
            }
        };

        Self {
            config,
            canvases,
            tool,
            pen,
            hotkeys,
            modifiers: ModifierTracker::default(),
            overlay_cache: ui::overlay::OverlayCache::new(),
            overlay_visible: true,
            mouse_was_down: false,
            last_save_at: Instant::now(),
            first_frame: true,
            pan_drag_anchor: None,
            layer_drag: None,
            pen_filter: crate::input::filter::PenFilter::for_level(level),
            pen_blocked: false,
            eraser_override,
            pen_using_eraser: false,
            pen_picking: false,
            ui_visible: true,
            osn_hwnd,
            recorder: crate::recording::Recorder::default(),
            ui_visible_before_record: true,
            toast: None,
            pending_screenshot: None,
            loupe: None,
            pending_freeze: None,
            pinned_hwnd: None,
            pin_picking: false,
            pin_prev_lmb: false,
            pinned_last_rect: None,
            ffmpeg_available: crate::recording::detect_ffmpeg(),
        }
    }

    /// Take a screenshot if the user pressed the screenshot hotkey or
    /// menu button. Logs success/failure but never panics.
    ///
    /// `pixels_per_point` is forwarded so the canvas rasteriser scales
    /// stroke coordinates from egui logical pixels into the physical
    /// pixel space of the xcap-captured background.
    fn maybe_screenshot(&mut self, pixels_per_point: f32) {
        match crate::screenshot::capture_and_save(self.canvases.active(), &self.config, pixels_per_point) {
            Ok(r)  => {
                log::info!("screenshot saved to {:?} (clipboard={})", r.path, r.clipboard);
                let msg = if r.clipboard {
                    format!("Screenshot saved: {} · clipboard ✓", r.path.display())
                } else {
                    format!("Screenshot saved: {}", r.path.display())
                };
                self.toast = Some((msg, std::time::Instant::now()));
            }
            Err(e) => log::warn!("screenshot failed: {e}"),
        }
    }

    /// Enqueue a deferred screenshot. Hides the floating UI + drops
    /// the background tint so xcap's monitor capture (which happens
    /// two frames from now) does not pick up the OSN toolbar or its
    /// dimming overlay. The prior state is captured here and
    /// restored after the capture lands.
    fn queue_screenshot(&mut self, pixels_per_point: f32) {
        if self.pending_screenshot.is_some() { return; }
        self.pending_screenshot = Some(PendingShot {
            ui_visible_before: self.ui_visible,
            bg_opacity_before: self.config.bg_opacity,
            // Two frames: one to clear chrome, one for the OS to
            // composite the cleared frame. Empirically one is
            // sometimes enough but two is reliable on Win11.
            frames_to_wait: 2,
            pixels_per_point,
        });
        self.ui_visible = false;
        self.config.bg_opacity = 0.0;
    }

    // -- Space-drag gestures ---------------------------------------------

    /// Begin a Space-drag. `layer_mode` is whether Shift was down at the
    /// instant the pointer went down; latching it here (rather than
    /// re-reading it every frame) means the gesture keeps doing what it
    /// started doing even if the user's fingers slip off Shift halfway
    /// through a drag.
    fn begin_pan_gesture(&mut self, layer_mode: bool) {
        self.layer_drag = layer_mode.then(|| LayerDrag {
            canvas: self.canvases.active_index(),
            layer: self.canvases.active().active_layer,
            delta: [0.0, 0.0],
        });
    }

    /// Apply one frame of drag motion, given in window-client pixels.
    /// Routes to the layer delta or the canvas viewport depending on
    /// which gesture is in flight.
    fn apply_pan_delta(&mut self, dx: f32, dy: f32) {
        let zoom = self.canvases.active().zoom.max(1e-6);
        if let Some(drag) = self.layer_drag.as_mut() {
            // A layer delta ends up added to canvas-local geometry, so
            // the view zoom has to come back out of it — otherwise the
            // layer would race ahead of the cursor at zoom > 1. Canvas
            // `pan` needs no such division because it is applied
            // *after* the zoom scale.
            drag.delta[0] += dx / zoom;
            drag.delta[1] += dy / zoom;
        } else {
            let c = self.canvases.active_mut();
            c.pan[0] += dx;
            c.pan[1] += dy;
        }
    }

    /// End a layer drag, baking the accumulated delta into the layer's
    /// geometry. No-op unless a layer drag is actually in flight, so it
    /// is safe to call from every gesture-end path.
    fn commit_layer_drag(&mut self) {
        let Some(drag) = self.layer_drag.take() else { return };
        // Canvas nav (←/→) is not gated on Space, so the user can walk
        // off the sheet mid-gesture. The preview was showing on a
        // canvas they have since left — drop the move rather than bake
        // it into whichever sheet happens to be active now.
        if drag.canvas != self.canvases.active_index() { return; }
        self.canvases.active_mut().move_layer_by(drag.layer, drag.delta);
    }

    /// The in-flight layer drag in the form the renderer wants, or
    /// `None` when there is no drag or it belongs to another canvas.
    fn layer_drag_for_render(&self) -> Option<(usize, [f32; 2])> {
        self.layer_drag
            .filter(|d| d.canvas == self.canvases.active_index())
            .map(|d| (d.layer, d.delta))
    }

    /// Enqueue a deferred freeze-frame. Same UI-hide dance as a
    /// regular screenshot — we drop OSN's chrome + tint for two
    /// frames so the captured image is the pure desktop, then insert
    /// the bytes as a new bottom layer on the active canvas.
    fn queue_freeze(&mut self, pixels_per_point: f32) {
        if self.pending_freeze.is_some() { return; }
        self.pending_freeze = Some(PendingShot {
            ui_visible_before: self.ui_visible,
            bg_opacity_before: self.config.bg_opacity,
            frames_to_wait: 2,
            pixels_per_point,
        });
        self.ui_visible = false;
        self.config.bg_opacity = 0.0;
    }

    /// Capture the freeze frame and insert it as a new bottom layer.
    /// Toast on success / log on failure — never panics.
    fn do_freeze(&mut self, pixels_per_point: f32) {
        match crate::screenshot::capture_freeze_layer(pixels_per_point, self.osn_hwnd) {
            Ok(image) => {
                let label = format!("Frozen frame {}", crate::screenshot::chrono_like_timestamp());
                self.canvases.active_mut().add_image_layer_bottom(image, label);
                self.toast = Some((
                    "Frozen frame added as bottom layer".to_string(),
                    std::time::Instant::now(),
                ));
            }
            Err(e) => log::warn!("freeze frame failed: {e}"),
        }
    }

    /// Start or stop screen recording. Hides the floating UI on
    /// start so the toolbar and status badge don't end up in the
    /// saved GIF; restores the user's previous UI visibility on
    /// stop. The actual capture + encode lives in `crate::recording`.
    fn toggle_record(&mut self) {
        if self.recorder.is_recording() {
            // Stop: finalise encoder (writes GIF trailer) + clipboard
            // copy, then restore UI.
            self.recorder.stop_and_encode();
            self.ui_visible = self.ui_visible_before_record;
        } else {
            // Start: snapshot UI state, hide chrome, kick off
            // recorder with the destination dir captured up front.
            self.ui_visible_before_record = self.ui_visible;
            self.ui_visible = false;
            let dir = self
                .config
                .screenshot_dir
                .clone()
                .or_else(|| crate::persistence::paths::default_screenshot_dir().ok())
                .unwrap_or_else(|| std::path::PathBuf::from("."));
            let use_mp4 = matches!(
                self.config.recording_format,
                crate::config::RecordingFormat::Mp4IfAvailable
            ) && self.ffmpeg_available;
            self.recorder.start(dir, self.osn_hwnd, use_mp4);
        }
    }
}

impl eframe::App for OnScreenNotesApp {
    /// We want *clear color* to be fully transparent so the window stays
    /// see-through. eframe queries this once per frame.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // Alpha drives the window's see-through level: 0 = fully
        // transparent, 1 = fully opaque. RGB comes from `config.bg_color`
        // so the user can tint the overlay (e.g. paper-cream for a
        // notebook feel, deep navy for low-light work). Pre-multiply RGB
        // by alpha — wgpu's clear path expects pre-multiplied output.
        let a = self.config.bg_opacity.clamp(0.0, 1.0);
        let r = self.config.bg_color[0] as f32 / 255.0 * a;
        let g = self.config.bg_color[1] as f32 / 255.0 * a;
        let b = self.config.bg_color[2] as f32 / 255.0 * a;
        [r, g, b, a]
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // ---- First-frame init -------------------------------------------
        // On startup, explicitly tell the OS: "we want input by default".
        // This synchronises our `modifiers.alt_held = false` model with
        // the actual OS window flag, in case the previous session left the
        // window in passthrough mode.
        if self.first_frame {
            ctx.send_viewport_cmd(egui::ViewportCommand::MousePassthrough(false));
            // OS-level: ensure clicks on the overlay don't activate the
            // window when it's already in passthrough. Combined with
            // the foreground-reclaim on each pen Down (below), this
            // gives the "click-through but OSN stays focused"
            // behaviour. No-op on non-Windows targets.
            crate::platform::set_no_activate(self.osn_hwnd);
            self.first_frame = false;
            log::info!("on-screen-notes started: overlay visible, input enabled");
        }

        // ---- Always-on subsystems ----------------------------------------
        // 1) Click-through edge detection (Alt key).
        let click_through_now = click_through::update(ctx, &mut self.modifiers);

        // 2) Global hotkey events.
        if let Some(h) = self.hotkeys.as_ref() {
            // Capture DPI scale once per frame so multiple screenshots
            // fired in the same frame don't re-read the value.
            let ppp = ctx.pixels_per_point();
            for ev in h.poll() {
                match ev {
                    HotkeyId::ToggleOverlay => self.overlay_visible = !self.overlay_visible,
                    HotkeyId::Screenshot   => self.queue_screenshot(ppp),
                    HotkeyId::ToggleRecord => self.toggle_record(),
                    HotkeyId::TogglePauseRecord => self.recorder.toggle_pause(),
                }
            }
        }

        // Drive the deferred screenshot state machine. Two frames of
        // wait lets the OS compositor repaint OSN without its chrome
        // before xcap snapshots the desktop. When the counter hits
        // zero, we capture, then restore the chrome the next pass.
        let do_capture = if let Some(ps) = self.pending_screenshot.as_mut() {
            if ps.frames_to_wait > 0 {
                ps.frames_to_wait -= 1;
                ctx.request_repaint();
                None
            } else {
                Some((ps.pixels_per_point, ps.ui_visible_before, ps.bg_opacity_before))
            }
        } else {
            None
        };
        if let Some((ppp, ui_prev, bg_prev)) = do_capture {
            self.maybe_screenshot(ppp);
            self.ui_visible = ui_prev;
            self.config.bg_opacity = bg_prev;
            self.pending_screenshot = None;
        }

        // Same state machine for the freeze-frame-to-layer action.
        let do_freeze = if let Some(ps) = self.pending_freeze.as_mut() {
            if ps.frames_to_wait > 0 {
                ps.frames_to_wait -= 1;
                ctx.request_repaint();
                None
            } else {
                Some((ps.pixels_per_point, ps.ui_visible_before, ps.bg_opacity_before))
            }
        } else {
            None
        };
        if let Some((ppp, ui_prev, bg_prev)) = do_freeze {
            self.do_freeze(ppp);
            self.ui_visible = ui_prev;
            self.config.bg_opacity = bg_prev;
            self.pending_freeze = None;
        }

        // Recording capture+encode runs on a dedicated worker thread
        // spawned in `Recorder::start()`. The UI thread does nothing
        // per-frame except poll for the final result below.
        // Surface the last encode result as a toast (one-shot).
        if let Some(res) = self.recorder.take_result() {
            self.toast = Some((
                format!(
                    "Recording saved: {} ({} frame{}{})",
                    res.path.display(),
                    res.frame_count,
                    if res.frame_count == 1 { "" } else { "s" },
                    if res.copied_to_clipboard { ", clipboard ✓" } else { "" }
                ),
                std::time::Instant::now(),
            ));
        }

        // 3) Pen events (no-op when tablet missing).
        //    * `client_origin` = screen-pixel coordinate of our window's
        //      client area's top-left. The Wintab backend reports packets
        //      in screen pixels; subtracting `client_origin` converts to
        //      window-client coordinates.
        //    * After polling, we transform each sample from window-client
        //      to canvas-local (subtract canvas rect, pan, divide zoom),
        //      then dispatch to tools.
        let client_origin = ctx.input(|i| {
            i.viewport()
                .inner_rect
                .map(|r| [r.min.x as i32, r.min.y as i32])
                .unwrap_or([0, 0])
        });
        let raw_pen_events = self.pen.poll(client_origin);

        // ---- Smooth raw pen samples --------------------------------------
        // Wintab packets carry digitizer noise; at low pen speed the noise
        // dominates and the Catmull-Rom subdivision amplifies it into
        // visible jagged wobble. Run each sample through a One Euro filter
        // (speed-adaptive low-pass: tight at rest, near-passthrough at
        // speed) and drop near-duplicate samples.
        //
        //   * `Down` resets the filter and force-accepts the sample so the
        //     stroke starts exactly where the user pressed.
        //   * `Move` is filtered + min-distance gated; samples that don't
        //     move enough are dropped.
        //   * `Up` is force-accepted so pen-up lands exactly.
        use crate::input::pen::PenEvent;
        let mut pen_events: Vec<PenEvent> = Vec::with_capacity(raw_pen_events.len());
        for ev in raw_pen_events {
            match ev {
                PenEvent::Down(s) => {
                    self.pen_filter.reset();
                    if let Some(fs) = self.pen_filter.filter(s, true) {
                        pen_events.push(PenEvent::Down(fs));
                    }
                }
                PenEvent::Move(s) => {
                    if let Some(fs) = self.pen_filter.filter(s, false) {
                        pen_events.push(PenEvent::Move(fs));
                    }
                }
                PenEvent::Up(s) => {
                    if let Some(fs) = self.pen_filter.filter(s, true) {
                        pen_events.push(PenEvent::Up(fs));
                    }
                }
            }
        }

        // ---- Pin-to-window follow ---------------------------------------
        // Two phases:
        //   (a) `pin_picking` — overlay is click-through so the user
        //       can target a window; the next external LMB-down
        //       captures the top-level HWND under the cursor.
        //   (b) `pinned_hwnd` is Some — each frame we read the
        //       target's rect and reposition the overlay to match.
        //       If the rect query fails (window minimised or
        //       destroyed), we drop the pin and toast the user.
        if self.pin_picking {
            // Esc cancels pin-picking without locking us into
            // click-through mode forever.
            if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.pin_picking = false;
                ctx.send_viewport_cmd(egui::ViewportCommand::MousePassthrough(false));
            }
        }
        if self.pin_picking {
            // Force click-through so the visible click lands on the
            // target window, not on OSN.
            ctx.send_viewport_cmd(egui::ViewportCommand::MousePassthrough(true));
            let lmb_now = crate::platform::lmb_down();
            if lmb_now && !self.pin_prev_lmb {
                if let Some((cx, cy)) = crate::platform::cursor_pos() {
                    let hwnd = crate::platform::top_level_window_at(cx, cy);
                    if hwnd != 0 && hwnd != self.osn_hwnd {
                        self.pinned_hwnd = Some(hwnd);
                        self.pinned_last_rect = None;
                        self.toast = Some((
                            "Pinned to window".to_string(),
                            std::time::Instant::now(),
                        ));
                    }
                }
                self.pin_picking = false;
                // Restore normal click handling.
                ctx.send_viewport_cmd(egui::ViewportCommand::MousePassthrough(false));
            }
            self.pin_prev_lmb = lmb_now;
        } else {
            self.pin_prev_lmb = false;
        }

        if let Some(hwnd) = self.pinned_hwnd {
            match crate::platform::window_rect(hwnd) {
                None => {
                    self.pinned_hwnd = None;
                    self.pinned_last_rect = None;
                    self.toast = Some((
                        "Pin lost: target window minimised or closed".to_string(),
                        std::time::Instant::now(),
                    ));
                }
                Some(rect) => {
                    if Some(rect) != self.pinned_last_rect {
                        let ppp = ctx.pixels_per_point().max(0.01);
                        let (l, t, r, b) = rect;
                        let w = (r - l).max(1) as f32 / ppp;
                        let h = (b - t).max(1) as f32 / ppp;
                        let pos = egui::pos2(l as f32 / ppp, t as f32 / ppp);
                        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos));
                        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(w, h)));
                        self.pinned_last_rect = Some(rect);
                    }
                }
            }
        }

        // ---- Magnifier loupe (Z-held modal) -----------------------------
        // Rising edge → capture the primary monitor once and upload as
        // a texture. Falling edge → drop the state so the GPU memory
        // is reclaimed. Captures cost ~30 ms; the one-shot model keeps
        // the loupe at full overlay refresh while held.
        //
        // Gated on !ctrl so Ctrl+Z (undo) and Ctrl+Shift+Z (redo) do
        // not accidentally arm the loupe.
        {
            let z_held = ctx.input(|i| i.key_down(egui::Key::Z));
            let z_ctrl = ctx.input(|i| i.modifiers.ctrl || i.modifiers.command);
            let z_modal = z_held && !z_ctrl;
            if z_modal && self.loupe.is_none() {
                match ui::loupe::LoupeState::start_live(ctx, self.osn_hwnd) {
                    Ok(l) => self.loupe = Some(l),
                    Err(e) => log::warn!("loupe capture failed: {e}"),
                }
            }
            if !z_modal && self.loupe.is_some() {
                self.loupe = None;
            }
            // Drain any new frame the worker has produced and push
            // it into the GPU texture for this frame's paint.
            if let Some(l) = self.loupe.as_mut() {
                l.poll_and_update();
            }
        }

        // ---- Space-hold pan check ---------------------------------------
        // We treat Space as a *modal* pan key: while it is held, all pen +
        // mouse drags pan the active canvas instead of drawing. This is the
        // same convention used by Photoshop, Figma, Rnote, etc.
        let space_held = ctx.input(|i| i.key_down(egui::Key::Space));

        // ---- Main overlay ------------------------------------------------
        if self.overlay_visible {
            // Toolbar first so it's drawn under the central canvas's hover
            // rect (egui draws Windows on top of CentralPanel anyway).
            let prev_level = self.config.smoothing_level;
            let prev_pressure_curve = self.config.pressure_curve;
            let toolbar_clicked = if self.ui_visible {
                ui::toolbar::show(
                    ctx,
                    &mut self.tool,
                    &mut self.config.bg_opacity,
                    &mut self.config.bg_color,
                    &mut self.config.palette,
                    &mut self.config.smoothing_level,
                    &mut self.config.pressure_curve,
                    &mut self.pinned_hwnd,
                    &mut self.pin_picking,
                )
            } else {
                false
            };
            // If the user switched smoothing levels via the toolbar, push
            // the new value through the input filter, the tool's pass
            // budget, and rebuild every stroke cache so existing strokes
            // visually match the new level immediately.
            if self.config.smoothing_level != prev_level {
                self.pen_filter.set_level(self.config.smoothing_level);
                let passes = self.config.smoothing_level.position_passes();
                self.tool.position_passes = passes;
                self.canvases.rebuild_all_caches(passes);
            }
            // Pressure-curve change: push to the global atomic that
            // every cache builder reads, then rebuild caches so already-
            // drawn strokes pick up the new curve.
            if (self.config.pressure_curve - prev_pressure_curve).abs() > 1e-4 {
                crate::canvas::stroke::set_pressure_curve(self.config.pressure_curve);
                let passes = self.config.smoothing_level.position_passes();
                self.canvases.rebuild_all_caches(passes);
            }

            // Render canvas + texture upload. The layer-drag preview is
            // resolved up front because `show` takes `&mut
            // self.overlay_cache`, which rules out reading `self` again
            // inside the argument list.
            //
            // Note this paint runs *before* the drag block below updates
            // the delta, so the previewed layer trails the cursor by one
            // frame — the same one-frame lag the canvas pan has always
            // had.
            let layer_drag = self.layer_drag_for_render();
            let canvas_rect = ui::overlay::show(
                ctx,
                &mut self.overlay_cache,
                &self.canvases,
                &self.tool,
                self.config.smoothing_level.position_passes(),
                self.ui_visible,
                self.loupe.as_ref(),
                layer_drag,
            );

            // ---- Space-hold pan: consume drags before drawing -----------
            // While Space is held, the cursor turns into a grab hand and any
            // primary-button drag (pen OR mouse) pans the canvas. Drawing
            // dispatch is skipped this frame so pen samples don't accidentally
            // commit a stroke during a pan gesture.
            //
            // Two sources of motion:
            //   1. Pen events from Wintab (in window-client pixels). These
            //      DO NOT go through the egui pointer on Windows when a
            //      tablet driver is intercepting input — they only reach us
            //      via the polled Wintab queue. So we route them explicitly.
            //   2. Mouse / trackpad via egui's pointer state. Used when
            //      there is no tablet (mouse fallback path).
            if space_held {
                // Holding Shift as well retargets the drag from the
                // viewport to the *active layer*: its ink slides while
                // every other layer stays nailed down. The mode is
                // latched at pointer-down by `begin_pan_gesture`, so
                // this per-frame read only drives the cursor hint.
                let layer_mode = ctx.input(|i| i.modifiers.shift);
                ctx.set_cursor_icon(if layer_mode || self.layer_drag.is_some() {
                    egui::CursorIcon::Move
                } else {
                    egui::CursorIcon::Grabbing
                });

                use crate::input::pen::PenEvent;
                let mut had_pen = false;
                for ev in &pen_events {
                    had_pen = true;
                    let s = match ev {
                        PenEvent::Down(s) | PenEvent::Move(s) | PenEvent::Up(s) => s,
                    };
                    // Pen samples are already in window-client logical
                    // pixels, same space as `canvas.pan`. No offset needed.
                    let here = egui::pos2(s.pos[0], s.pos[1]);
                    match ev {
                        PenEvent::Down(_) => {
                            self.begin_pan_gesture(layer_mode);
                            self.pan_drag_anchor = Some(here);
                        }
                        PenEvent::Move(_) => {
                            if let Some(anchor) = self.pan_drag_anchor {
                                self.apply_pan_delta(here.x - anchor.x, here.y - anchor.y);
                            }
                            self.pan_drag_anchor = Some(here);
                        }
                        PenEvent::Up(_) => {
                            self.pan_drag_anchor = None;
                            self.commit_layer_drag();
                        }
                    }
                }

                // Mouse path — only run when no pen events fired this frame
                // so the two sources don't double-add motion. The pointer
                // state is read out of `ctx.input` first and acted on
                // afterwards, so no canvas mutation happens while egui's
                // input lock is held.
                if !had_pen {
                    let (primary, cursor) = ctx.input(|i| {
                        (
                            i.pointer.primary_down(),
                            i.pointer
                                .hover_pos()
                                .or(i.pointer.interact_pos())
                                .or(i.pointer.latest_pos()),
                        )
                    });
                    if primary {
                        if let Some(p) = cursor {
                            match self.pan_drag_anchor {
                                Some(anchor) => self.apply_pan_delta(p.x - anchor.x, p.y - anchor.y),
                                // No anchor yet: the button just went
                                // down, so this is the gesture start.
                                None => self.begin_pan_gesture(layer_mode),
                            }
                            self.pan_drag_anchor = Some(p);
                        }
                    } else {
                        self.pan_drag_anchor = None;
                        self.commit_layer_drag();
                    }
                }
            } else {
                // Pan released — drop anchor, restore default cursor.
                self.pan_drag_anchor = None;
                // Letting go of Space mid-drag still ends the gesture, so
                // bake any pending layer move rather than losing it.
                self.commit_layer_drag();

                // Hide the OS cursor while the Pen / FreehandArrow tools
                // are active so it doesn't sit on top of the pen tip and
                // distract from the drawn line. Skip the hide when the
                // pointer is over a floating UI layer (toolbar, popup)
                // so the user can still see what they're about to click.
                let tool_hides_cursor = matches!(
                    self.tool.kind,
                    crate::tools::ToolKind::Pen
                        | crate::tools::ToolKind::FreehandArrow
                        | crate::tools::ToolKind::Laser,
                );
                // The Fill tool needs a visible pointer because the user
                // has to land the stroke's starting point precisely on
                // the edge of the area they want to enclose. Showing the
                // crosshair instead of the regular arrow matches what
                // the user gets while picking a colour with Shift — a
                // "precision aim" cue rather than a generic arrow.
                let tool_wants_crosshair = matches!(
                    self.tool.kind,
                    crate::tools::ToolKind::Fill,
                );
                let hover = ctx.input(|i| i.pointer.hover_pos());
                let on_ui = hover
                    .and_then(|p| ctx.layer_id_at(p))
                    .map(|l| l.order > egui::Order::Background)
                    .unwrap_or(false);
                if tool_hides_cursor && !on_ui {
                    ctx.set_cursor_icon(egui::CursorIcon::None);
                } else if tool_wants_crosshair && !on_ui {
                    ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
                }
            }

            // ---- Dispatch pen events (window-client → canvas-local) -----
            // Apply the canvas's pan + zoom + the canvas-rect offset to each
            // sample, then dispatch into the active tool. Suppressed when
            // Space is held so a pan gesture doesn't leave ink behind.
            //
            // Also block dispatch when the pen is touching a floating UI
            // layer (toolbar, settings popup, color picker). Wintab does
            // not flow through the egui pointer, so the usual
            // "click-through ignored on widgets" behaviour does not kick
            // in automatically — we have to compare the pen position
            // against `ctx.layer_id_at` ourselves. Block on a UI Down
            // and stay blocked until the next Up so the gesture cannot
            // accidentally start a stroke half-way through tapping a
            // menu item.
            if !space_held && self.loupe.is_none() {
                // Mirror the current tool's size onto the eraser
                // override so the temporary erase uses the same brush
                // size as the active pen — feels natural while letting
                // the user keep their pen tool selected.
                self.eraser_override.size  = self.tool.size;
                self.eraser_override.color = self.tool.color;

                let ctrl_held  = ctx.input(|i| i.modifiers.ctrl);
                let shift_held = ctx.input(|i| i.modifiers.shift);

                // Shift-pick: while Shift is held over the canvas, give the
                // user a crosshair so they know the next tap will sample a
                // pixel instead of drawing ink. Toolbar / popups stay
                // visible — `set_cursor_icon` does not affect drawing
                // dispatch on its own, only the OS cursor glyph.
                if shift_held && !self.pen_blocked {
                    ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
                }

                for ev in &pen_events {
                    let s = match ev {
                        PenEvent::Down(s) | PenEvent::Move(s) | PenEvent::Up(s) => *s,
                    };
                    let pen_pos = egui::pos2(s.pos[0], s.pos[1]);
                    let on_ui = ctx.layer_id_at(pen_pos)
                        .map(|l| l.order > egui::Order::Background)
                        .unwrap_or(false);
                    match ev {
                        PenEvent::Down(_) => {
                            if on_ui {
                                self.pen_blocked = true;
                                continue;
                            }
                            self.pen_blocked = false;
                            // Pen landed on the canvas — the user wants
                            // to draw in OSN. If a prior click-through
                            // had handed the foreground to another app,
                            // reclaim it so keyboard shortcuts (P, B,
                            // T…) keep flowing here. No-op on non-Win
                            // targets and when OSN is already foreground.
                            crate::platform::reclaim_foreground(self.osn_hwnd);
                            // Shift-pick wins over the eraser modifier:
                            // sample the pixel at the pen tip on Down,
                            // set the pen colour, and lock the rest of
                            // the gesture out of dispatch.
                            if shift_held || self.tool.eyedropper_pending {
                                self.pen_picking = true;
                                let sx = client_origin[0] + s.pos[0] as i32;
                                let sy = client_origin[1] + s.pos[1] as i32;
                                if let Some(c) = crate::input::picker::pick_screen_color(sx, sy) {
                                    self.tool.color = c;
                                }
                                // One-shot eyedropper: clear the pending
                                // flag so the next click draws normally.
                                self.tool.eyedropper_pending = false;
                                continue;
                            }
                            self.pen_picking = false;
                            // Lock the dispatch target for the rest of
                            // this gesture: if Ctrl was held at Down,
                            // route every Move + Up to the eraser
                            // override regardless of Ctrl state later.
                            self.pen_using_eraser = ctrl_held;
                        }
                        PenEvent::Move(_) => {
                            if self.pen_blocked || self.pen_picking { continue; }
                        }
                        PenEvent::Up(_) => {
                            if self.pen_blocked {
                                self.pen_blocked = false;
                                continue;
                            }
                            if self.pen_picking {
                                self.pen_picking = false;
                                continue;
                            }
                        }
                    }
                    let dispatched = transform_pen_event(*ev, canvas_rect, self.canvases.active());
                    let target_tool: &mut ActiveTool = if self.pen_using_eraser {
                        &mut self.eraser_override
                    } else {
                        &mut self.tool
                    };
                    crate::input::pen::dispatch(
                        dispatched,
                        target_tool,
                        self.canvases.active_mut(),
                    );
                    if matches!(ev, PenEvent::Up(_)) {
                        // Gesture done — release the lock so the next
                        // Down can re-evaluate Ctrl state.
                        self.pen_using_eraser = false;
                    }
                }
            }

            // ---- Mouse fallback (only when tablet absent) ----------------
            if !space_held && !self.pen.is_active() && !click_through_now && !toolbar_clicked {
                // Shift + click = colour pick. Resolve here so the
                // sampling click does not also start a fallback stroke.
                let (shift_held, primary_now) = ctx.input(|i| {
                    (i.modifiers.shift, i.pointer.primary_down())
                });
                let picking = shift_held || self.tool.eyedropper_pending;
                if picking {
                    ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
                }
                let cursor_for_pick = ctx.input(|i| i.pointer.hover_pos().or(i.pointer.interact_pos()));
                if picking && primary_now && !self.mouse_was_down {
                    if let Some(p) = cursor_for_pick {
                        let ppp = ctx.pixels_per_point();
                        let sx = client_origin[0] + (p.x * ppp) as i32;
                        let sy = client_origin[1] + (p.y * ppp) as i32;
                        if let Some(c) = crate::input::picker::pick_screen_color(sx, sy) {
                            self.tool.color = c;
                        }
                    }
                    // One-shot eyedropper: clear after a successful pick.
                    self.tool.eyedropper_pending = false;
                    // Mark down so the next frame's still-held click
                    // does not trigger a stroke or a second pick.
                    self.mouse_was_down = true;
                }
                if !primary_now {
                    self.mouse_was_down = false;
                }
                if picking {
                    // Skip the rest of the mouse-drawing path entirely
                    // while picking; otherwise a release would synthesise
                    // a pen-up event over the freshly picked colour.
                } else {
                ctx.input(|i| {
                    let pressed = i.pointer.primary_down();
                    let cursor = i.pointer.hover_pos().or(i.pointer.interact_pos());
                    if let Some(p) = cursor {
                        let cursor_screen = [
                            (p.x - canvas_rect.min.x).max(0.0),
                            (p.y - canvas_rect.min.y).max(0.0),
                        ];
                        if let Some(ev) = interaction::mouse_to_pen_event(
                            pressed,
                            &mut self.mouse_was_down,
                            cursor_screen,
                            self.canvases.active(),
                        ) {
                            crate::input::pen::dispatch(
                                ev, &mut self.tool, self.canvases.active_mut(),
                            );
                        }
                    }
                });
                }
            }

            // ---- Deferred flood-fill --------------------------------------
            // The Fill tool's pen-down arms `tool.pending_fill_at`; we
            // run the actual flood here so it has access to the
            // viewport size + DPI.
            if let Some(p) = self.tool.pending_fill_at.take() {
                let viewport = [canvas_rect.width(), canvas_rect.height()];
                let ppp = ctx.pixels_per_point();
                let result = crate::flood_fill::flood_fill_active(
                    self.canvases.active(),
                    p,
                    self.tool.color,
                    viewport,
                    ppp,
                );
                match result {
                    crate::flood_fill::FillResult::Filled(shape) => {
                        self.canvases.active_mut().add_shape(shape);
                    }
                    crate::flood_fill::FillResult::OpenRegion => {
                        self.toast = Some((
                            "Fill cancelled: no closed region under cursor".to_string(),
                            std::time::Instant::now(),
                        ));
                    }
                    crate::flood_fill::FillResult::Empty => {}
                }
            }

            // ---- Keyboard shortcuts --------------------------------------
            // Ctrl+0: reset canvas view. Handled via `consume_shortcut` so
            // egui atomically matches modifier + key in one call, dodging
            // the "events loop sees Num0 but Ctrl was already filtered out"
            // failure mode we hit before with the per-event match below.
            //
            // Ctrl+Shift+0 (reset the active layer's position) is tested
            // *first* and deliberately so: `consume_shortcut` matches
            // modifiers logically, i.e. a held Shift does not disqualify
            // a Ctrl-only pattern. Checking Ctrl+0 first would let it
            // swallow Ctrl+Shift+0 and reset the view instead.
            //
            // Layer pans are baked into the geometry, so "reset" is just
            // a move by the negated running total — which also means it
            // lands in the undo history like any other move.
            let reset_layer = egui::KeyboardShortcut::new(
                egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
                egui::Key::Num0,
            );
            if ctx.input_mut(|i| i.consume_shortcut(&reset_layer)) {
                let c = self.canvases.active_mut();
                let li = c.active_layer;
                if let Some(m) = c.layers.get(li).map(|l| l.moved_by) {
                    c.move_layer_by(li, [-m[0], -m[1]]);
                }
            }
            let reset_view = egui::KeyboardShortcut::new(
                egui::Modifiers::CTRL,
                egui::Key::Num0,
            );
            if ctx.input_mut(|i| i.consume_shortcut(&reset_view)) {
                let c = self.canvases.active_mut();
                c.zoom = 1.0;
                c.pan  = [0.0, 0.0];
            }
            // Ctrl+Shift+F = freeze the screen behind OSN as a new
            // bottom layer on the active canvas. Captured via the
            // deferred-shot state machine so OSN's chrome does not
            // appear in the frame.
            let freeze_sc = egui::KeyboardShortcut::new(
                egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
                egui::Key::F,
            );
            if ctx.input_mut(|i| i.consume_shortcut(&freeze_sc)) {
                self.queue_freeze(ctx.pixels_per_point());
            }

            // Text tool steals keyboard input while a caret is open.
            // Returns true when the events were consumed — in that
            // case we skip the tool-shortcut / undo / canvas-nav
            // dispatcher so typing "t" / "Ctrl+Z" lands in the label,
            // not as a tool change or an undo.
            let text_consumed = interaction::route_text_input(ctx, &mut self.tool, &mut self.canvases);

            ctx.input(|i| {
                let shift = i.modifiers.shift;
                let ctrl  = i.modifiers.ctrl || i.modifiers.command;
                if text_consumed {
                    // Still allow scroll-wheel events (handled below
                    // outside this for-loop) but drop key events.
                    // Falling through to the wheel block below means
                    // we skip the per-event match by emptying the
                    // iterator.
                    return;
                }
                for ev in &i.events {
                    if let egui::Event::Key { key, pressed: true, .. } = ev {
                        // Tool switch shortcuts.
                        interaction::react_tool_shortcut(*key, shift, &mut self.tool);
                        // Canvas navigation (left / right).
                        interaction::react_canvas_nav(*key, &mut self.canvases);
                        // Layer navigation (up / down) on the active canvas.
                        interaction::react_layer_nav(*key, &mut self.canvases);
                        // Tab toggles every floating UI element off so
                        // the user can view their drawing without
                        // chrome. Strokes / shapes / cursor still
                        // render normally.
                        if *key == egui::Key::Tab && !ctrl && !shift {
                            self.ui_visible = !self.ui_visible;
                        }
                        // Undo / redo / clear.
                        if ctrl && *key == egui::Key::Z {
                            if shift {
                                self.canvases.active_mut().redo();
                            } else {
                                self.canvases.active_mut().undo();
                            }
                        }
                        if ctrl && *key == egui::Key::Delete {
                            self.canvases.active_mut().clear();
                        }
                        // Ctrl+Shift+Backspace: delete the active canvas
                        // (or clear it, if it's the only one).
                        if ctrl && shift && *key == egui::Key::Backspace {
                            self.canvases.delete_active();
                        }
                    }
                }
                // Scroll wheel — count discrete notches per frame.
                //
                // We must NOT use `smooth_scroll_delta` or `zoom_delta`
                // here: both accumulate across frames during egui's
                // smooth-scroll animation, so a single wheel click
                // ends up firing the handler 10-30 times → the user
                // sees one click jump bg_opacity from 0 to 1.
                //
                // `Event::MouseWheel` is fired exactly once per real
                // wheel event by the windowing backend, with the
                // user's actual modifier state attached. Summing the
                // *signs* gives an integer notch count immune to
                // platform-specific unit/magnitude differences (Line
                // vs Point, ±1 vs ±120).
                let mut notches: i32 = 0;
                let mut raw_delta_y: f32 = 0.0;
                for ev in &i.events {
                    if let egui::Event::MouseWheel { delta, modifiers, .. } = ev {
                        raw_delta_y += delta.y;
                        if delta.y > 0.0 { notches += 1; }
                        else if delta.y < 0.0 { notches -= 1; }
                        let _ = modifiers; // documented use
                    }
                }
                // Also pull egui's accumulated zoom-delta. Trackpad
                // pinches and `Ctrl+wheel` smooth-scroll feed this
                // each frame even when no discrete MouseWheel
                // event fires — including it keeps zoom truly
                // realtime on hi-res input devices.
                let zoom_delta_extra = i.zoom_delta();
                if (zoom_delta_extra - 1.0).abs() > 1e-4 {
                    // Convert multiplicative zoom factor into a
                    // wheel-delta equivalent matching the 1.10× /
                    // unit mapping used inside `handle_scroll`.
                    raw_delta_y += zoom_delta_extra.ln() / 1.10_f32.ln();
                }
                if notches != 0 || raw_delta_y.abs() > 1e-4 {
                    if let Some(l) = self.loupe.as_mut() {
                        if notches != 0 {
                            l.zoom = (l.zoom + notches as f32 * 0.25).clamp(1.5, 12.0);
                        }
                    } else {
                        let cursor = i.pointer.hover_pos().unwrap_or_default();
                        let cursor_screen = [
                            (cursor.x - canvas_rect.min.x).max(0.0),
                            (cursor.y - canvas_rect.min.y).max(0.0),
                        ];
                        interaction::zoom_pan::handle_scroll(
                            notches,
                            raw_delta_y,
                            cursor_screen,
                            ctrl,
                            shift,
                            &mut self.tool,
                            self.canvases.active_mut(),
                            &mut self.config.bg_opacity,
                        );
                    }
                }
            });

            // ---- Status badge + layer panel ------------------------------
            // Both hide when the master UI toggle is off.
            if self.ui_visible {
                ui::status::show(
                    ctx,
                    self.canvases.active_index(),
                    self.canvases.count(),
                    click_through_now,
                    self.pen.is_active(),
                );
                ui::layer_panel::show(ctx, &mut self.canvases);
            }
        }

        // ---- Sync last-used tool state into persistent config ------------
        // Treat the most recently selected colour and size as the "default"
        // for the next session, so launching the app does not always reset
        // to red 6 px. The debounced auto-save below writes these to disk.
        self.config.default_pen_color = self.tool.color;
        self.config.default_pen_size  = self.tool.size;
        self.config.default_stroke_style = self.tool.style;

        // ---- Recording indicator + toast ---------------------------------
        // REC dot painted while capturing. We use a tiny floating
        // Area in the top-left so the user always knows the recorder
        // is on, even with `ui_visible = false`. Pen events pass
        // through it (the area is render-only).
        if self.recorder.is_recording() {
            let elapsed = self.recorder.elapsed().as_secs();
            egui::Area::new(egui::Id::new("osn_rec_indicator"))
                .anchor(egui::Align2::LEFT_TOP, egui::vec2(12.0, 12.0))
                .interactable(false)
                .show(ctx, |ui| {
                    egui::Frame::none()
                        .fill(egui::Color32::from_rgba_unmultiplied(0, 0, 0, 140))
                        .rounding(6.0)
                        .inner_margin(egui::Margin::symmetric(8.0, 4.0))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                // Pulsing red dot — alpha modulated by
                                // the system clock so it reads as "live".
                                let pulse = {
                                    let ms = ctx.input(|i| i.time);
                                    (((ms * 2.0).sin() * 0.5 + 0.5) * 255.0) as u8
                                };
                                let (rect, _) = ui.allocate_exact_size(
                                    egui::vec2(10.0, 10.0),
                                    egui::Sense::hover(),
                                );
                                ui.painter().circle_filled(
                                    rect.center(),
                                    4.5,
                                    egui::Color32::from_rgba_unmultiplied(232, 71, 71, pulse),
                                );
                                ui.label(format!("REC  {elapsed:02}s  · Ctrl+Shift+R stop"));
                            });
                        });
                });
        }
        // Toast — clears after 5 s.
        if let Some((msg, t)) = self.toast.clone() {
            if t.elapsed() > Duration::from_secs(5) {
                self.toast = None;
            } else {
                egui::Area::new(egui::Id::new("osn_toast"))
                    .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -16.0))
                    .interactable(false)
                    .show(ctx, |ui| {
                        egui::Frame::none()
                            .fill(egui::Color32::from_rgba_unmultiplied(15, 18, 24, 220))
                            .rounding(8.0)
                            .inner_margin(egui::Margin::symmetric(12.0, 8.0))
                            .show(ui, |ui| {
                                ui.label(msg);
                            });
                    });
            }
        }

        // ---- Laser-stroke pruning ----------------------------------------
        // Drop laser strokes whose fade has run out. The render path
        // skips strokes with alpha <= 0 already, but holding onto them
        // would grow the Vec forever during long sessions.
        if !self.tool.laser_strokes.is_empty() {
            let fade = crate::canvas::render::LASER_FADE_SECS;
            self.tool
                .laser_strokes
                .retain(|(_, up_at)| up_at.elapsed().as_secs_f32() < fade);
        }

        // ---- Auto-save (debounced) ---------------------------------------
        if self.last_save_at.elapsed() > Duration::from_millis(1500) {
            self.canvases.save_all();
            if let Err(e) = crate::persistence::save_config(&self.config) {
                log::warn!("config save failed: {e}");
            }
            self.last_save_at = Instant::now();
        }

        // Always request the next frame. Reasoning:
        //   * Pen events come from a polled Wintab queue; we must call
        //     `pen.poll()` every frame or packets pile up and feel laggy.
        //   * Global hotkey events are also polled each frame.
        //   * `request_repaint()` (no delay) is what minimises pen-to-pixel
        //     latency — eframe will paint as fast as the display allows.
        // Cost: continuous repaint when idle. Acceptable: this is a
        // foreground drawing tool, not a background daemon.
        ctx.request_repaint();
    }

    /// Save state on graceful exit so nothing is lost.
    /// Note: signature is `(&mut self)` only when the `glow` feature is off.
    /// With glow it would be `(&mut self, Option<&glow::Context>)`. Since we
    /// build with `wgpu` only, the no-arg form is correct.
    fn on_exit(&mut self) {
        self.canvases.save_all();
        if let Err(e) = crate::persistence::save_config(&self.config) {
            log::warn!("config save on exit failed: {e}");
        }
    }
}

/// Transform a `PenEvent` whose sample is in *window-client* pixels into one
/// whose sample is in *canvas-local* coordinates by:
///   1. Subtracting the canvas rect's top-left to get canvas-rect-relative.
///   2. Subtracting the canvas's `pan` and dividing by `zoom`.
///
/// Done outside the pen module so the pen module stays platform-focused.
fn transform_pen_event(
    ev: crate::input::pen::PenEvent,
    canvas_rect: egui::Rect,
    canvas: &crate::canvas::canvas::Canvas,
) -> crate::input::pen::PenEvent {
    use crate::input::pen::PenEvent;
    let convert = |sample: crate::canvas::stroke::PenSample| {
        let win = sample.pos;
        let rel = [win[0] - canvas_rect.min.x, win[1] - canvas_rect.min.y];
        let local = [
            (rel[0] - canvas.pan[0]) / canvas.zoom,
            (rel[1] - canvas.pan[1]) / canvas.zoom,
        ];
        crate::canvas::stroke::PenSample {
            pos: local,
            pressure: sample.pressure,
            tilt: sample.tilt,
        }
    };
    match ev {
        PenEvent::Down(s) => PenEvent::Down(convert(s)),
        PenEvent::Move(s) => PenEvent::Move(convert(s)),
        PenEvent::Up(s)   => PenEvent::Up(convert(s)),
    }
}

