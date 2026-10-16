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
    /// Parsed user-editable hotkey bindings. Compiled once at app
    /// start; per-event dispatch is then a flat compare.
    hotkeys_cfg: crate::input::hotkeys_config::ParsedHotkeys,
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
    /// Gap between frames while nothing is happening: one refresh of
    /// the monitor the overlay is on. See the end of `update`.
    idle_frame: Duration,

    /// Set to false after the very first `update` runs. Used to push
    /// initial-state viewport commands (e.g. clear click-through).
    first_frame: bool,

    /// Anchor for "hold Space + drag to pan". `Some(p)` while the user is
    /// actively dragging — `p` is the previous cursor position in window-
    /// client pixels. We compute the per-frame delta against this anchor
    /// and apply it to `canvas.pan`, then move the anchor forward.
    pan_drag_anchor: Option<egui::Pos2>,

    /// Anchor for middle-button drag pan, same meaning as
    /// `pan_drag_anchor`. Kept separate because the Space path clears
    /// that one every frame Space is up.
    middle_pan_anchor: Option<egui::Pos2>,

    /// Active **Shift**+Space layer gesture — a drag pans the layer, the
    /// wheel zooms it, and both accumulate into one similarity
    /// transform. `None` means the current Space drag pans the whole
    /// canvas and the wheel zooms the whole canvas instead.
    ///
    /// The layer index is latched when the gesture starts, so switching
    /// the active layer — or letting go of Shift — mid-gesture cannot
    /// redirect it to a different layer. The transform accumulates in
    /// canvas-local units and is baked into the layer's geometry by
    /// `Canvas::transform_layer` when the gesture ends; until then it
    /// only moves / scales that layer at paint time.
    layer_xform: Option<LayerXform>,

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

    /// True while the layer-preview popup is open. Toggled by the
    /// `canvas.preview_layer` hotkey; Esc closes. Renders a centred
    /// large thumbnail of the active layer so the user can confirm
    /// "what's on this layer" before drawing into it.
    show_layer_preview: bool,

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

    /// Deferred lasso-select capture state. Same chrome-hide + frame-
    /// wait dance as `pending_freeze`, but only the pixels inside the
    /// recorded polygon are kept (cropped to the selection bbox).
    pending_lasso: Option<PendingLasso>,

    /// Active screenshot region selection. `Some` while the user drags out
    /// the capture rectangle after pressing the screenshot hotkey; on
    /// release it becomes a region `pending_screenshot`. Suppresses drawing
    /// dispatch while set.
    region_select: Option<RegionSelect>,

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

    /// Objects copied with Ctrl+C / Ctrl+X in the Select tool. The OS
    /// clipboard only carries `CLIP_MARKER` to say "the newest copy is
    /// this one"; the strokes and shapes themselves stay here.
    obj_clipboard: (Vec<crate::canvas::stroke::Stroke>, Vec<crate::canvas::shape::Shape>),
    /// How many times the current `obj_clipboard` has been pasted, so
    /// each paste lands a step further from the original instead of
    /// exactly on top of the last one.
    paste_count: u32,
    /// `(canvas index, undo-log length)` right after the last size
    /// restyle of a selection. While both still match, the next size
    /// tick is folded into that same undo entry — a slider drag is one
    /// Ctrl+Z, not hundreds.
    restyle_run: Option<(usize, usize)>,
    /// Ctrl+V state on the previous frame, for edge detection — see
    /// `platform::paste_chord_down`.
    paste_chord_prev: bool,
    /// True while a mouse drag that began on the canvas is being fed to
    /// a tool. False for a press that landed on a widget, and after the
    /// pen has taken a gesture over.
    mouse_gesture: bool,

    /// Players for the videos and GIFs on the active slide.
    media: crate::media::MediaHub,
    /// Their playback bars, and the FFmpeg setup window.
    media_ui: ui::media_controls::MediaUi,
    /// Files being copied in and read as videos / GIFs.
    media_imports: Vec<crate::media::import::ImportJob>,
    /// Videos picked or dropped before FFmpeg was set up, with where
    /// they were dropped. Imported once it is; dropped if the setup
    /// window is closed without it.
    media_waiting: Vec<(std::path::PathBuf, Option<egui::Pos2>)>,
    /// Open dialog started by the toolbar's "Insert video / GIF".
    media_dialog: Option<std::sync::mpsc::Receiver<Vec<std::path::PathBuf>>>,
    /// PureRef boards being read, each becoming a new slide.
    pureref_jobs: Vec<crate::media::pureref::OpenJob>,
    /// The picture the last in-app copy put on the OS clipboard, so a
    /// paste that finds it there pastes the objects instead. Set by the
    /// thread that builds it.
    clip_image: std::sync::Arc<std::sync::Mutex<Option<crate::clipboard::Fingerprint>>>,
}

/// How long after the pen was last used the pointer still counts as
/// the pen's. Covers the gap between the pen lifting and the driver
/// releasing the mouse button it was holding down for it.
const PEN_OWNS_POINTER: Duration = Duration::from_millis(250);

/// How long after its last Wintab packet the pen still counts as in
/// range, keeping frames coming at full speed. A hovering pen sends
/// packets as it moves; this bridges the pauses of a hand held still.
const PEN_NEARBY: Duration = Duration::from_secs(1);

/// The monitor's refresh period, or 60 Hz when Windows does not say.
fn refresh_period(hwnd: isize) -> Duration {
    let hz = crate::platform::refresh_rate_hz(hwnd).unwrap_or(60).clamp(30, 500);
    Duration::from_secs_f64(1.0 / f64::from(hz))
}

/// Text put on the OS clipboard by an in-app copy. Seeing it come back
/// in a paste means the newest thing the user copied is `obj_clipboard`.
const CLIP_MARKER: &str = "on-screen-notes:selection";

/// A Select-tool command picked out of this frame's key events, run
/// after egui's input lock is released.
#[derive(Clone, Copy)]
enum SelAction {
    Delete,
    Duplicate,
    ToFront,
    ToBack,
    Copy,
    Cut,
    PasteObjects,
    PasteImage,
}

/// State held while a screenshot is in flight.
struct PendingShot {
    ui_visible_before: bool,
    bg_opacity_before: f32,
    /// Frames remaining before we trigger the capture. We need to wait at
    /// least one frame so the OS compositor has time to repaint after the
    /// chrome was hidden, otherwise xcap still picks up the toolbar.
    frames_to_wait: u32,
    /// DPI scale captured at the moment the hotkey fired so a
    /// later DPI change between request and capture doesn't break
    /// alignment.
    pixels_per_point: f32,
    /// Overlay tint at request time. For a region shot we re-composite this
    /// (`bg_color` at `bg_opacity_before`) in software, since the capture
    /// dropped the real window opacity to 0 for a clean grab.
    bg_color: [u8; 3],
    /// `Some([min_x, min_y, max_x, max_y])` in egui points → capture only
    /// that region (respecting the tint above). `None` → full-screen shot.
    region_rect: Option<[f32; 4]>,
}

/// In-flight screenshot region selection: the user is dragging out the
/// capture rectangle (in egui screen points) after pressing the screenshot
/// hotkey. Drawing dispatch is suppressed while this is `Some`.
#[derive(Clone, Copy)]
struct RegionSelect {
    /// Drag anchor; `None` until the primary button first goes down.
    start: Option<egui::Pos2>,
    /// Latest pointer position (the moving corner while dragging).
    current: egui::Pos2,
    /// `ui_visible` at selection start, restored if the user cancels.
    ui_visible_before: bool,
}

/// State held while a lasso-select capture is in flight. Carries the
/// recorded polygon plus the pan/zoom snapshot taken at queue time so
/// the masking math matches the view the lasso was drawn in even if
/// the canvas is nudged during the 2-frame chrome-hide.
struct PendingLasso {
    ui_visible_before: bool,
    bg_opacity_before: f32,
    frames_to_wait: u32,
    pixels_per_point: f32,
    pan: [f32; 2],
    zoom: f32,
    /// Closed polygon in canvas-local coords.
    polygon: Vec<[f32; 2]>,
}

/// An in-flight Shift+Space layer pan / zoom.
///
/// `canvas` and `layer` are latched when the gesture starts so the
/// transform lands where the user aimed it even if they switch sheets
/// or layers before letting go. `scale` and `offset` are the `a` and
/// `b` of the canvas-local similarity `p → a · p + b`, and stay
/// preview-only until the gesture ends — see `Canvas::transform_layer`
/// for why the result is baked into the geometry rather than kept as a
/// per-layer transform.
///
/// Keeping one similarity (rather than a delta plus a separate scale +
/// pivot) is what lets a single gesture mix dragging and wheeling: each
/// new step composes onto the pair, and the pivot of every wheel notch
/// is remembered implicitly inside `offset`.
#[derive(Clone, Copy)]
struct LayerXform {
    canvas: usize,
    layer: usize,
    scale: f32,
    offset: [f32; 2],
}

impl LayerXform {
    fn new(canvas: usize, layer: usize) -> Self {
        Self { canvas, layer, scale: 1.0, offset: [0.0, 0.0] }
    }

    /// Slide the layer by `d` canvas-local pixels. Translation applies
    /// *after* the accumulated scale, so it lands on `offset` alone and
    /// the layer tracks the cursor 1:1 however far it has been zoomed.
    fn pan(&mut self, d: [f32; 2]) {
        self.offset[0] += d[0];
        self.offset[1] += d[1];
    }

    /// Zoom the layer by `s` about the canvas-local point `pivot`,
    /// composing `p → s · (p − pivot) + pivot` onto what the gesture has
    /// accumulated so far.
    fn zoom(&mut self, s: f32, pivot: [f32; 2]) {
        self.scale *= s;
        self.offset[0] = s * self.offset[0] + (1.0 - s) * pivot[0];
        self.offset[1] = s * self.offset[1] + (1.0 - s) * pivot[1];
    }
}

/// Zoom bounds for a single layer, as a multiple of the layer's
/// original size. Wider than the canvas view's 0.1–16 range is pointless
/// — beyond this the ink is either a dot or unusably huge — and the
/// clamp also keeps `scaled_by` away from values whose reciprocal
/// rounds badly when the user resets.
const LAYER_ZOOM_MIN: f32 = 0.05;
const LAYER_ZOOM_MAX: f32 = 20.0;

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
        let mut canvases = CanvasManager::load_or_default(&config);
        // A slide that failed to load is the user's work in a state we
        // could not read — say so on screen rather than only in the log.
        let load_toast = {
            let errs = canvases.take_load_errors();
            match errs.len() {
                0 => None,
                1 => Some(errs[0].clone()),
                n => Some(format!("{n} slides could not be read — originals kept beside them (*.unreadable-…)")),
            }
        };
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

        // Load hand-edited hotkeys.toml (next-to-binary or fallback).
        // Globals are registered immediately from its `global` section;
        // tool / canvas bindings get parsed into compact `Binding`s and
        // cached on the App for the in-overlay dispatcher.
        let hotkeys_file = crate::input::hotkeys_config::HotkeysConfig::load_or_default();
        let hotkeys_cfg = hotkeys_file.parsed();

        let hotkeys = match Hotkeys::register(&hotkeys_file.global) {
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

        crate::media::ffmpeg::set_configured(config.ffmpeg_path.clone());

        Self {
            config,
            hotkeys_cfg,
            canvases,
            tool,
            pen,
            hotkeys,
            modifiers: ModifierTracker::default(),
            overlay_cache: ui::overlay::OverlayCache::new(),
            overlay_visible: true,
            mouse_was_down: false,
            last_save_at: Instant::now(),
            idle_frame: Duration::from_secs_f64(1.0 / 60.0),
            first_frame: true,
            pan_drag_anchor: None,
            middle_pan_anchor: None,
            layer_xform: None,
            pen_filter: crate::input::filter::PenFilter::for_level(level),
            pen_blocked: false,
            eraser_override,
            pen_using_eraser: false,
            pen_picking: false,
            show_layer_preview: false,
            ui_visible: true,
            osn_hwnd,
            recorder: crate::recording::Recorder::default(),
            ui_visible_before_record: true,
            toast: load_toast.map(|m| (m, std::time::Instant::now())),
            pending_screenshot: None,
            pending_lasso: None,
            region_select: None,
            loupe: None,
            pending_freeze: None,
            pinned_hwnd: None,
            pin_picking: false,
            pin_prev_lmb: false,
            pinned_last_rect: None,
            ffmpeg_available: crate::recording::detect_ffmpeg(),
            obj_clipboard: (Vec::new(), Vec::new()),
            paste_count: 0,
            restyle_run: None,
            paste_chord_prev: false,
            mouse_gesture: false,
            media: crate::media::MediaHub::new(),
            media_ui: ui::media_controls::MediaUi::new(),
            media_imports: Vec::new(),
            media_waiting: Vec::new(),
            media_dialog: None,
            pureref_jobs: Vec::new(),
            clip_image: Default::default(),
        }
    }

    /// Run the deferred screenshot once its frame countdown elapses. A
    /// `region_rect` captures only the user-selected rectangle (compositing
    /// the overlay tint back in); otherwise the whole primary monitor is
    /// grabbed. Logs success/failure but never panics.
    fn do_screenshot(&mut self, ps: &PendingShot) {
        let res = if let Some(rect) = ps.region_rect {
            crate::screenshot::capture_region_and_save(
                self.canvases.active(),
                &self.config,
                ps.pixels_per_point,
                self.osn_hwnd,
                rect,
                ps.bg_color,
                ps.bg_opacity_before,
                &self.media.current_frames(),
            )
        } else {
            crate::screenshot::capture_and_save(
                self.canvases.active(),
                &self.config,
                ps.pixels_per_point,
                &self.media.current_frames(),
            )
        };
        match res {
            Ok(r) => {
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

    /// Begin an interactive screenshot region selection. Hides the floating
    /// UI so it stays out of the way (and out of the capture); the actual
    /// grab is deferred until the user drags out a rectangle, handled in
    /// `update_region_select`. The background tint is left alone so the user
    /// aims over a still-visible desktop — it is dropped only at the moment
    /// the region is confirmed, then re-composited into the saved image.
    fn begin_region_select(&mut self) {
        if self.region_select.is_some() || self.pending_screenshot.is_some() {
            return;
        }
        self.region_select = Some(RegionSelect {
            start: None,
            current: egui::Pos2::ZERO,
            ui_visible_before: self.ui_visible,
        });
        self.ui_visible = false;
    }

    /// Handle the interactive region-selection overlay for one frame: read
    /// the drag, draw the dimmed rubber-band, and on release turn the
    /// selection into a region `pending_screenshot` (or cancel). No-op when
    /// no selection is active.
    fn update_region_select(&mut self, ctx: &egui::Context) {
        let Some(mut rs) = self.region_select else { return };

        let (pressed, released, esc, pos) = ctx.input(|i| {
            (
                i.pointer.primary_pressed(),
                i.pointer.primary_released(),
                i.key_pressed(egui::Key::Escape),
                i.pointer.latest_pos().or(i.pointer.interact_pos()),
            )
        });

        // Esc cancels: restore the chrome, leave the tint untouched.
        if esc {
            self.ui_visible = rs.ui_visible_before;
            self.region_select = None;
            return;
        }

        if let Some(p) = pos {
            rs.current = p;
            if pressed {
                rs.start = Some(p);
            }
        }

        if released {
            if let Some(start) = rs.start {
                let sel = egui::Rect::from_two_pos(start, rs.current);
                let ppp = ctx.pixels_per_point().max(0.01);
                // Reject a too-small drag (a stray click) — treat as cancel.
                if sel.width() * ppp >= 4.0 && sel.height() * ppp >= 4.0 {
                    self.pending_screenshot = Some(PendingShot {
                        ui_visible_before: rs.ui_visible_before,
                        bg_opacity_before: self.config.bg_opacity,
                        frames_to_wait: 2,
                        pixels_per_point: ppp,
                        bg_color: self.config.bg_color,
                        region_rect: Some([sel.min.x, sel.min.y, sel.max.x, sel.max.y]),
                    });
                    // Drop the live tint so xcap grabs a clean desktop; the
                    // capture re-applies `bg_color` at the stored opacity.
                    self.config.bg_opacity = 0.0;
                } else {
                    self.ui_visible = rs.ui_visible_before;
                }
                self.region_select = None;
                return;
            }
        }

        // Still selecting — persist state and draw the overlay on top.
        self.region_select = Some(rs);
        ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("osn_region_select"),
        ));
        let screen = ctx.screen_rect();
        let dim = egui::Color32::from_black_alpha(110);
        if let Some(start) = rs.start {
            let sel = egui::Rect::from_two_pos(start, rs.current);
            // Four dim bands around the selection (the selection stays clear).
            painter.rect_filled(
                egui::Rect::from_min_max(screen.left_top(), egui::pos2(screen.right(), sel.top())),
                0.0,
                dim,
            );
            painter.rect_filled(
                egui::Rect::from_min_max(egui::pos2(screen.left(), sel.bottom()), screen.right_bottom()),
                0.0,
                dim,
            );
            painter.rect_filled(
                egui::Rect::from_min_max(egui::pos2(screen.left(), sel.top()), egui::pos2(sel.left(), sel.bottom())),
                0.0,
                dim,
            );
            painter.rect_filled(
                egui::Rect::from_min_max(egui::pos2(sel.right(), sel.top()), egui::pos2(screen.right(), sel.bottom())),
                0.0,
                dim,
            );
            // Animated dashed border (reuses the marching-ants walker).
            let stroke = egui::Stroke::new(1.5, egui::Color32::WHITE);
            let pts = [sel.left_top(), sel.right_top(), sel.right_bottom(), sel.left_bottom(), sel.left_top()];
            crate::canvas::render::paint_dashed_polyline(&painter, &pts, stroke, 1.0);
            // Dimension label in physical pixels (what the PNG will be).
            let ppp = ctx.pixels_per_point().max(0.01);
            let wpx = (sel.width() * ppp).round() as i32;
            let hpx = (sel.height() * ppp).round() as i32;
            let label_pos = egui::pos2(sel.left(), (sel.top() - 6.0).max(screen.top() + 2.0));
            painter.text(
                label_pos,
                egui::Align2::LEFT_BOTTOM,
                format!("{wpx} × {hpx}"),
                egui::FontId::proportional(14.0),
                egui::Color32::WHITE,
            );
        } else {
            // Before the first drag: dim everything + a hint.
            painter.rect_filled(screen, 0.0, egui::Color32::from_black_alpha(70));
            painter.text(
                screen.center(),
                egui::Align2::CENTER_CENTER,
                "Drag to select a screenshot region  ·  Esc to cancel",
                egui::FontId::proportional(18.0),
                egui::Color32::WHITE,
            );
        }
        // Keep the frame live for the dashed animation + drag tracking.
        ctx.request_repaint();
    }

    /// Whether the next frame should come at once rather than at the
    /// idle pace: the pen is in range or drawing, a button is held, a
    /// gesture, capture or recording is under way, or something on
    /// screen moves by itself.
    fn needs_live_frames(&self, ctx: &egui::Context) -> bool {
        self.pen.is_down()
            || self.pen.seen_within(PEN_NEARBY)
            || self.mouse_gesture
            || ctx.input(|i| i.pointer.any_down())
            || self.tool.in_progress_stroke.is_some()
            || self.tool.select.gesture.is_some()
            || self.layer_xform.is_some()
            || self.pan_drag_anchor.is_some()
            || self.middle_pan_anchor.is_some()
            || self.region_select.is_some()
            || self.pending_screenshot.is_some()
            || self.pending_freeze.is_some()
            || self.pending_lasso.is_some()
            || self.recorder.is_recording()
            || self.loupe.is_some()
            || !self.tool.laser_strokes.is_empty()
            || self.media.is_animating()
    }

    // -- Space-drag gestures ---------------------------------------------

    /// Begin a Space-drag. `layer_mode` is whether Shift was down at the
    /// instant the pointer went down; latching it here (rather than
    /// re-reading it every frame) means the gesture keeps doing what it
    /// started doing even if the user's fingers slip off Shift halfway
    /// through a drag.
    fn begin_pan_gesture(&mut self, layer_mode: bool) {
        if !layer_mode {
            // A plain Space drag pans the viewport. Anything the user
            // already aimed at the layer (a wheel zoom before touching
            // the pen) is a finished gesture — bake it rather than
            // dropping it on the floor.
            self.commit_layer_xform();
            return;
        }
        let canvas = self.canvases.active_index();
        let layer = self.canvases.active().active_layer;
        match self.layer_xform {
            // Wheel-zoomed first, now dragging: same target, so keep
            // accumulating into the gesture already in flight instead
            // of throwing its scale away.
            Some(x) if x.canvas == canvas && x.layer == layer => {}
            Some(_) => {
                self.commit_layer_xform();
                self.layer_xform = Some(LayerXform::new(canvas, layer));
            }
            None => self.layer_xform = Some(LayerXform::new(canvas, layer)),
        }
    }

    /// Apply one frame of drag motion, given in window-client pixels.
    /// Routes to the layer transform or the canvas viewport depending on
    /// which gesture is in flight.
    fn apply_pan_delta(&mut self, dx: f32, dy: f32) {
        let zoom = self.canvases.active().zoom.max(1e-6);
        if let Some(x) = self.layer_xform.as_mut() {
            // A layer delta ends up added to canvas-local geometry, so
            // the view zoom has to come back out of it — otherwise the
            // layer would race ahead of the cursor at zoom > 1. Canvas
            // `pan` needs no such division because it is applied
            // *after* the zoom scale.
            x.pan([dx / zoom, dy / zoom]);
        } else {
            let (pan, _) = self.canvases.active_view_mut();
            pan[0] += dx;
            pan[1] += dy;
        }
    }

    /// Zoom the gesture's layer by `s` about `pivot` (canvas-local),
    /// starting a gesture on the active layer if the wheel arrived
    /// before any drag. The accumulated scale is clamped so the preview
    /// can't run away past what `transform_layer` will accept.
    fn apply_layer_zoom(&mut self, s: f32, pivot: [f32; 2]) {
        if self.layer_xform.is_none() {
            let canvas = self.canvases.active_index();
            let layer = self.canvases.active().active_layer;
            self.layer_xform = Some(LayerXform::new(canvas, layer));
        }
        let Some(x) = self.layer_xform.as_mut() else { return };
        // Clamp against the layer's *total* scale, not just this
        // gesture's, so ten gestures of 2× stop at the same ceiling one
        // long gesture would.
        let already = self
            .canvases
            .active()
            .layers
            .get(x.layer)
            .map(|l| l.scaled_by)
            .unwrap_or(1.0)
            .max(1e-6);
        let want = (already * x.scale * s).clamp(LAYER_ZOOM_MIN, LAYER_ZOOM_MAX);
        let s = want / (already * x.scale).max(1e-6);
        if (s - 1.0).abs() < 1e-6 { return; }
        x.zoom(s, pivot);
    }

    /// End a layer gesture, baking the accumulated transform into the
    /// layer's geometry. No-op unless a gesture is actually in flight,
    /// so it is safe to call from every gesture-end path.
    fn commit_layer_xform(&mut self) {
        let Some(x) = self.layer_xform.take() else { return };
        // Canvas nav (←/→) is not gated on Space, so the user can walk
        // off the sheet mid-gesture. The preview was showing on a
        // canvas they have since left — drop the transform rather than
        // bake it into whichever sheet happens to be active now.
        if x.canvas != self.canvases.active_index() { return; }
        if (x.scale - 1.0).abs() < 1e-6 {
            // Pure pan: keep it in the history as a `LayerMoved` so
            // "reset layer position" and the existing undo path see
            // exactly what they saw before layer zoom existed.
            self.canvases.active_mut().move_layer_by(x.layer, x.offset);
        } else {
            self.canvases.active_mut().transform_layer(x.layer, x.scale, x.offset);
        }
    }

    /// The in-flight layer gesture in the form the renderer wants, or
    /// `None` when there is no gesture or it belongs to another canvas.
    fn layer_xform_for_render(&self) -> Option<(usize, f32, [f32; 2])> {
        self.layer_xform
            .filter(|x| x.canvas == self.canvases.active_index())
            .map(|x| (x.layer, x.scale, x.offset))
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
            bg_color: self.config.bg_color,
            region_rect: None,
        });
        self.ui_visible = false;
        self.config.bg_opacity = 0.0;
    }

    /// Capture the freeze frame and insert it as a new bottom layer.
    /// Toast on success / log on failure — never panics.
    fn do_freeze(&mut self, pixels_per_point: f32) {
        // Snapshot the active canvas's pan/zoom so the captured
        // pixels can be positioned in canvas-local coords that land
        // under the user's current view — without rewriting their
        // pan/zoom state behind their back.
        let canvas = self.canvases.active();
        let pan  = canvas.pan;
        let zoom = canvas.zoom;
        match crate::screenshot::capture_freeze_layer(pixels_per_point, self.osn_hwnd, pan, zoom) {
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

    /// Enqueue a deferred lasso-select capture. Snapshots the active
    /// canvas's pan/zoom alongside the polygon, then runs the same
    /// 2-frame chrome-hide as freeze so the captured pixels are the
    /// pure desktop behind the overlay.
    fn queue_lasso_capture(&mut self, polygon: Vec<[f32; 2]>, pixels_per_point: f32) {
        if self.pending_lasso.is_some() { return; }
        let canvas = self.canvases.active();
        self.pending_lasso = Some(PendingLasso {
            ui_visible_before: self.ui_visible,
            bg_opacity_before: self.config.bg_opacity,
            frames_to_wait: 2,
            pixels_per_point,
            pan: canvas.pan,
            zoom: canvas.zoom,
            polygon,
        });
        self.ui_visible = false;
        self.config.bg_opacity = 0.0;
    }

    /// Capture the lasso region and drop it onto the **active layer** as
    /// an image object, selected and ready to move / resize / rotate.
    /// It paints under that layer's ink (a layer draws its shapes before
    /// its strokes) and is an ordinary undo step. Toast on success / log
    /// on failure — never panics.
    fn do_lasso_capture(
        &mut self,
        pixels_per_point: f32,
        pan: [f32; 2],
        zoom: f32,
        polygon: &[[f32; 2]],
    ) {
        match crate::screenshot::capture_lasso_layer(
            pixels_per_point,
            self.osn_hwnd,
            pan,
            zoom,
            polygon,
        ) {
            Ok(image) => {
                let shape = crate::canvas::shape::Shape::Raster {
                    pos: image.canvas_origin,
                    size: image.logical_size,
                    png: std::sync::Arc::new(image.png),
                    px_size: image.size,
                    color: [0, 0, 0, 0],
                    rotation: 0.0,
                    kind: crate::canvas::shape::RasterKind::Image,
                    media: None,
                    picture: None,
                };
                let c = self.canvases.active_mut();
                c.add_shape(shape);
                let placed = c.layers[c.active_layer].shapes.len() - 1;
                self.select_only_shape(placed);
                self.toast = Some((
                    "Captured onto the active layer — drag to move, handles to resize / rotate".to_string(),
                    std::time::Instant::now(),
                ));
            }
            Err(e) => log::warn!("lasso capture failed: {e}"),
        }
    }

    /// Switch to the Select tool with exactly the shape at `index` on
    /// the active layer selected.
    fn select_only_shape(&mut self, index: usize) {
        use crate::canvas::hit::ObjRef;
        self.tool.kind = crate::tools::ToolKind::Select;
        self.tool.select.gesture = None;
        self.tool.select.sel.set_only(ObjRef::Shape(index));
        self.tool.select.rebind(self.canvases.active_index(), self.canvases.active());
    }

    /// Clones of the selected objects on the active layer.
    fn selected_objects(&self) -> (Vec<crate::canvas::stroke::Stroke>, Vec<crate::canvas::shape::Shape>) {
        let c = self.canvases.active();
        let Some(layer) = c.layers.get(c.active_layer) else { return (Vec::new(), Vec::new()) };
        let sel = &self.tool.select.sel;
        (
            sel.strokes.iter().filter_map(|&i| layer.strokes.get(i).cloned()).collect(),
            sel.shapes.iter().filter_map(|&i| layer.shapes.get(i).cloned()).collect(),
        )
    }

    /// Add `strokes` / `shapes` to the active layer, shifted by `offset`
    /// canvas pixels, as one undo step, and select them.
    fn insert_and_select(
        &mut self,
        mut strokes: Vec<crate::canvas::stroke::Stroke>,
        mut shapes: Vec<crate::canvas::shape::Shape>,
        offset: [f32; 2],
    ) {
        if strokes.is_empty() && shapes.is_empty() { return; }
        for s in &mut strokes { s.translate(offset); }
        for s in &mut shapes {
            s.translate(offset);
            // A copy of a video or GIF plays on its own.
            if let crate::canvas::shape::Shape::Raster { media: Some(m), .. } = s {
                m.id = crate::canvas::shape::fresh_media_id();
            }
        }
        let c = self.canvases.active_mut();
        let li = c.active_layer;
        let placed = c.insert_objects(li, strokes, shapes);
        self.tool.kind = crate::tools::ToolKind::Select;
        self.tool.select.gesture = None;
        self.tool.select.sel = placed;
        self.tool.select.rebind(self.canvases.active_index(), self.canvases.active());
    }

    /// Run one Select-tool command against the current selection.
    fn apply_sel_action(&mut self, action: SelAction, canvas_rect: egui::Rect, pixels_per_point: f32) {
        // One nudge so a copy does not sit exactly on its original. Kept
        // constant on screen, so it is divided by the zoom.
        let step = 16.0 / self.canvases.active().zoom.max(1e-3);
        let sel = self.tool.select.sel.clone();
        let has_selection = self.tool.kind == crate::tools::ToolKind::Select && !sel.is_empty();
        match action {
            SelAction::Delete if has_selection => {
                let c = self.canvases.active_mut();
                let li = c.active_layer;
                c.delete_objects(li, &sel.strokes, &sel.shapes);
                self.tool.select.sel.clear();
            }
            SelAction::Duplicate if has_selection => {
                let (strokes, shapes) = self.selected_objects();
                self.insert_and_select(strokes, shapes, [step, step]);
            }
            SelAction::ToFront | SelAction::ToBack if has_selection => {
                let to_front = matches!(action, SelAction::ToFront);
                let c = self.canvases.active_mut();
                let li = c.active_layer;
                self.tool.select.sel = c.reorder_objects(li, &sel.strokes, &sel.shapes, to_front);
            }
            SelAction::Copy | SelAction::Cut if has_selection => {
                self.obj_clipboard = self.selected_objects();
                self.paste_count = 0;
                // Mark the OS clipboard so a later paste knows this copy
                // is newer than whatever another app put there before.
                match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(CLIP_MARKER)) {
                    Ok(()) => {}
                    Err(e) => log::warn!("could not mark the clipboard: {e}"),
                }
                #[cfg(target_os = "windows")]
                self.copy_image_to_clipboard(pixels_per_point);
                if matches!(action, SelAction::Cut) {
                    let c = self.canvases.active_mut();
                    let li = c.active_layer;
                    c.delete_objects(li, &sel.strokes, &sel.shapes);
                    self.tool.select.sel.clear();
                }
            }
            SelAction::PasteObjects => self.paste_objects(step),
            SelAction::PasteImage => self.paste_clipboard_image(canvas_rect, pixels_per_point, step),
            // A selection command with nothing selected.
            _ => {}
        }
        self.tool.select.rebind(self.canvases.active_index(), self.canvases.active());
    }

    /// Paste the in-app clipboard's objects, each paste a step further
    /// along so repeated pastes fan out.
    fn paste_objects(&mut self, step: f32) {
        self.paste_count += 1;
        let d = step * self.paste_count as f32;
        let (strokes, shapes) = self.obj_clipboard.clone();
        self.insert_and_select(strokes, shapes, [d, d]);
    }

    /// Put the objects just copied on the OS clipboard as a picture too,
    /// for pasting into other apps (see `crate::clipboard`). Built on a
    /// thread: decoding and encoding a big picture takes a moment, and
    /// until it lands the clipboard holds the text marker. Windows only:
    /// elsewhere pasting an image raises no event the app can catch
    /// (`platform::paste_chord_down`), and in-app paste needs the marker.
    #[cfg(target_os = "windows")]
    fn copy_image_to_clipboard(&self, pixels_per_point: f32) {
        let (strokes, shapes) = self.obj_clipboard.clone();
        let zoom = self.canvases.active().zoom;
        let frames = self.media.current_frames();
        let media_dir = crate::persistence::paths::media_dir().ok();
        let mark = std::sync::Arc::clone(&self.clip_image);
        *mark.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let spawned = std::thread::Builder::new().name("osn-copy-image".into()).spawn(move || {
            let Some(img) = crate::clipboard::image_of(strokes, shapes, zoom, pixels_per_point, &frames, media_dir.as_deref()) else {
                return;
            };
            let (w, h) = (img.width() as usize, img.height() as usize);
            let bytes = img.into_raw();
            let Ok(mut cb) = arboard::Clipboard::new() else { return };
            // Whatever the user copied elsewhere in the meantime wins.
            if cb.get_text().ok().as_deref() != Some(CLIP_MARKER) {
                return;
            }
            // Marked first, so a paste right after the image lands
            // already knows it.
            let fp = crate::clipboard::fingerprint(w, h, &bytes);
            *mark.lock().unwrap_or_else(|e| e.into_inner()) = Some(fp);
            let image = arboard::ImageData { width: w, height: h, bytes: std::borrow::Cow::Owned(bytes) };
            if let Err(e) = cb.set_image(image) {
                log::warn!("could not put the copy on the clipboard as a picture: {e}");
                *mark.lock().unwrap_or_else(|e| e.into_inner()) = None;
            }
        });
        if let Err(e) = spawned {
            log::warn!("could not start copying a picture: {e}");
        }
    }

    /// Paste an image from the OS clipboard onto the active layer as an
    /// image object, centred in the view at its natural on-screen size
    /// (shrunk to fit if it is larger than the view). Silently does
    /// nothing when the clipboard holds no image. The picture of an
    /// in-app copy pastes as the objects it was made from instead.
    fn paste_clipboard_image(&mut self, canvas_rect: egui::Rect, pixels_per_point: f32, step: f32) {
        let img = match arboard::Clipboard::new().and_then(|mut cb| cb.get_image()) {
            Ok(img) => img,
            Err(_) => return,
        };
        let own = *self.clip_image.lock().unwrap_or_else(|e| e.into_inner());
        if own.is_some() && own == Some(crate::clipboard::fingerprint(img.width, img.height, &img.bytes)) {
            self.paste_objects(step);
            return;
        }
        let (w, h) = (img.width as u32, img.height as u32);
        let Some(rgba) = image::RgbaImage::from_raw(w, h, img.bytes.into_owned()) else { return };
        if w == 0 || h == 0 { return; }
        let mut png: Vec<u8> = Vec::new();
        if let Err(e) = rgba.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png) {
            log::warn!("could not encode pasted image: {e}");
            return;
        }
        let (pan, zoom) = {
            let c = self.canvases.active();
            (c.pan, c.zoom.max(1e-3))
        };
        // Natural size: one image pixel per physical screen pixel.
        let ppp = pixels_per_point.max(0.01);
        let mut size = [w as f32 / ppp / zoom, h as f32 / ppp / zoom];
        let view = [canvas_rect.width() / zoom, canvas_rect.height() / zoom];
        let fit = (view[0] * 0.9 / size[0]).min(view[1] * 0.9 / size[1]).min(1.0);
        if fit > 0.0 {
            size = [size[0] * fit, size[1] * fit];
        }
        let center = [
            (canvas_rect.width() * 0.5 - pan[0]) / zoom,
            (canvas_rect.height() * 0.5 - pan[1]) / zoom,
        ];
        let shape = crate::canvas::shape::Shape::Raster {
            pos: [center[0] - size[0] * 0.5, center[1] - size[1] * 0.5],
            size,
            png: std::sync::Arc::new(png),
            px_size: [w, h],
            color: [0, 0, 0, 0],
            rotation: 0.0,
            kind: crate::canvas::shape::RasterKind::Image,
            media: None,
            picture: None,
        };
        self.insert_and_select(Vec::new(), vec![shape], [0.0, 0.0]);
        self.toast = Some(("Image pasted onto the active layer".to_string(), std::time::Instant::now()));
    }

    /// Start bringing in a picture, video or GIF. A video waits for
    /// FFmpeg to be set up first.
    fn start_media_import(&mut self, path: std::path::PathBuf, at: Option<egui::Pos2>) {
        use crate::canvas::shape::MediaKind;
        if crate::media::import::media_kind(&path) == Some(MediaKind::Video) && crate::media::ffmpeg::tools().is_none() {
            self.media_waiting.push((path, at));
            self.media_ui.ffmpeg_window = true;
            return;
        }
        let job = crate::media::import::ImportJob::start(path, at);
        self.toast = Some((format!("Inserting {}…", job.file_name()), Instant::now()));
        self.media_imports.push(job);
    }

    /// Where a file was dropped, in screen points. egui's pointer is not
    /// updated during an OS drag, so ask the OS where the cursor is.
    fn drop_point(&self, ctx: &egui::Context) -> Option<egui::Pos2> {
        let ppp = ctx.pixels_per_point().max(0.01);
        if let (Some((cx, cy)), Some((left, top, _, _))) =
            (crate::platform::cursor_pos(), crate::platform::client_rect(self.osn_hwnd))
        {
            return Some(egui::pos2((cx - left) as f32 / ppp, (cy - top) as f32 / ppp));
        }
        ctx.input(|i| i.pointer.hover_pos())
    }

    /// Collect files from the Open dialog and from drag-and-drop, and
    /// turn finished imports into objects on the active layer.
    fn poll_media_inputs(&mut self, ctx: &egui::Context, canvas_rect: egui::Rect) {
        use std::sync::mpsc::TryRecvError;
        if let Some(rx) = &self.media_dialog {
            match rx.try_recv() {
                Ok(paths) => {
                    self.media_dialog = None;
                    for p in paths {
                        if crate::media::pureref::is_pureref(&p) {
                            self.start_pureref_open(p);
                        } else {
                            self.start_media_import(p, None);
                        }
                    }
                }
                Err(TryRecvError::Disconnected) => self.media_dialog = None,
                Err(TryRecvError::Empty) => {}
            }
        }

        let (dropped, hovering) = ctx.input(|i| (i.raw.dropped_files.clone(), !i.raw.hovered_files.is_empty()));
        if !dropped.is_empty() {
            let at = self.drop_point(ctx);
            for (n, file) in dropped.into_iter().enumerate() {
                let Some(path) = file.path else { continue };
                if crate::media::pureref::is_pureref(&path) {
                    self.start_pureref_open(path);
                    continue;
                }
                if !crate::media::import::is_supported(&path) {
                    self.toast = Some(("Only pictures, videos, GIFs and PureRef boards can be dropped here".into(), Instant::now()));
                    continue;
                }
                // Several files at once fan out instead of stacking.
                let at = at.map(|p| p + egui::vec2(24.0, 24.0) * n as f32);
                self.start_media_import(path, at);
            }
        }
        if hovering {
            egui::Area::new(egui::Id::new("osn_drop_hint"))
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .interactable(false)
                .show(ctx, |ui| {
                    ui::theme::card_frame().inner_margin(egui::Margin::symmetric(16.0, 10.0)).show(ui, |ui| {
                        ui.label("Drop to insert picture / video / GIF, or open a PureRef board");
                    });
                });
        }

        let mut finished = Vec::new();
        self.media_imports.retain(|job| match job.take() {
            Some(r) => {
                finished.push((job.path.clone(), job.at, job.file_name(), r));
                false
            }
            None => true,
        });
        for (path, at, name, result) in finished {
            use crate::media::import::ImportError;
            match result {
                Ok(imported) => {
                    self.insert_media(imported, at, canvas_rect, ctx.pixels_per_point());
                    self.toast = Some((format!("Inserted {name}"), Instant::now()));
                }
                Err(ImportError::NeedsFfmpeg) => {
                    self.media_waiting.push((path, at));
                    self.media_ui.ffmpeg_window = true;
                }
                Err(ImportError::Failed(e)) => {
                    log::warn!("import of {name} failed: {e}");
                    self.toast = Some((format!("Couldn't insert {name}: {e}"), Instant::now()));
                }
            }
        }

        self.show_pureref_progress(ctx);
        let mut boards = Vec::new();
        self.pureref_jobs.retain(|job| match job.take() {
            Some(r) => {
                boards.push((job.path.clone(), job.file_name(), r));
                false
            }
            None => true,
        });
        for (path, name, result) in boards {
            match result {
                Ok(board) => {
                    let (pictures, notes) = board.counts();
                    let skipped = board.skipped;
                    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                    let canvas = crate::media::pureref::into_canvas(
                        board,
                        [canvas_rect.width(), canvas_rect.height()],
                        ctx.pixels_per_point(),
                        &stem,
                    );
                    let idx = self.canvases.push_canvas(canvas);
                    let count = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
                    let mut msg = format!(
                        "Opened {name} as slide {} — {}, {}",
                        idx + 1,
                        count(pictures, "picture", "pictures"),
                        count(notes, "note", "notes"),
                    );
                    if skipped > 0 {
                        msg.push_str(&format!(", {skipped} skipped"));
                    }
                    self.toast = Some((msg, Instant::now()));
                }
                Err(e) if e == "cancelled" => {
                    self.toast = Some((format!("Stopped opening {name}"), Instant::now()));
                }
                Err(e) => {
                    log::warn!("opening {name} failed: {e}");
                    self.toast = Some((format!("Couldn't open {name}: {e}"), Instant::now()));
                }
            }
        }
    }

    /// Start reading a PureRef board; it becomes a new slide once read.
    /// The board file is only read, never written.
    pub(crate) fn start_pureref_open(&mut self, path: std::path::PathBuf) {
        self.pureref_jobs.push(crate::media::pureref::OpenJob::start(path));
    }

    /// A card at the bottom of the screen for each board being opened:
    /// what it is doing, a progress bar, and a button to stop it.
    fn show_pureref_progress(&mut self, ctx: &egui::Context) {
        use crate::media::pureref::Stage;
        if self.pureref_jobs.is_empty() {
            return;
        }
        let mut cancel = Vec::new();
        egui::Area::new(egui::Id::new("osn_pureref_progress"))
            .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -72.0))
            .show(ctx, |ui| {
                ui::theme::card_frame().inner_margin(egui::Margin::symmetric(14.0, 10.0)).show(ui, |ui| {
                    for (i, job) in self.pureref_jobs.iter().enumerate() {
                        let (stage, done, total) = job.progress();
                        let fraction = if total > 0 { (done as f32 / total as f32).clamp(0.0, 1.0) } else { 0.0 };
                        let name = job.file_name();
                        let text = match stage {
                            Stage::Reading if total > 0 => {
                                format!("Reading {name} — {} of {} MB", done >> 20, total >> 20)
                            }
                            Stage::Reading => format!("Reading {name}…"),
                            Stage::Pictures => format!("Opening {name} — picture {} of {total}", done.min(total)),
                        };
                        if i > 0 {
                            ui.add_space(6.0);
                        }
                        ui.horizontal(|ui| {
                            ui.label(text);
                            if job.cancelling() {
                                ui.label("Stopping…");
                            } else if ui.button("Cancel").clicked() {
                                cancel.push(i);
                            }
                        });
                        ui.add(egui::ProgressBar::new(fraction).desired_width(360.0).show_percentage());
                    }
                });
            });
        for i in cancel {
            self.pureref_jobs[i].cancel();
            crate::platform::reclaim_foreground(self.osn_hwnd);
        }
    }

    /// Place an imported picture / video / GIF on the active layer at its
    /// natural on-screen size (shrunk to fit the view), centred on `at` or
    /// on the view, and select it (a video's playback bar then shows).
    fn insert_media(
        &mut self,
        imported: crate::media::import::Imported,
        at: Option<egui::Pos2>,
        canvas_rect: egui::Rect,
        pixels_per_point: f32,
    ) {
        let (pan, zoom) = {
            let c = self.canvases.active();
            (c.pan, c.zoom.max(1e-3))
        };
        let ppp = pixels_per_point.max(0.01);
        let [w, h] = imported.size;
        let mut size = [w.max(1) as f32 / ppp / zoom, h.max(1) as f32 / ppp / zoom];
        let view = [canvas_rect.width() / zoom, canvas_rect.height() / zoom];
        let fit = (view[0] * 0.9 / size[0]).min(view[1] * 0.9 / size[1]).min(1.0);
        if fit > 0.0 {
            size = [size[0] * fit, size[1] * fit];
        }
        let screen = at.unwrap_or(canvas_rect.center()) - canvas_rect.min;
        let center = [(screen.x - pan[0]) / zoom, (screen.y - pan[1]) / zoom];
        let shape = crate::canvas::shape::Shape::Raster {
            pos: [center[0] - size[0] * 0.5, center[1] - size[1] * 0.5],
            size,
            png: std::sync::Arc::new(imported.png),
            px_size: imported.png_size,
            color: [0, 0, 0, 0],
            rotation: 0.0,
            kind: crate::canvas::shape::RasterKind::Image,
            media: imported.media.map(Box::new),
            picture: None,
        };
        self.insert_and_select(Vec::new(), vec![shape], [0.0, 0.0]);
    }

    /// Carry out what the playback bars and the FFmpeg window asked for.
    fn apply_media_out(&mut self, out: ui::media_controls::MediaUiOut) {
        for (id, cmd) in out.commands {
            self.media.command(id, cmd);
        }
        if out.clicked {
            crate::platform::reclaim_foreground(self.osn_hwnd);
        }
        if let Some(p) = out.located {
            self.config.ffmpeg_path = Some(p);
        }
        if out.ffmpeg_ready {
            self.ffmpeg_available = true;
            self.media.ffmpeg_changed();
            for (path, at) in std::mem::take(&mut self.media_waiting) {
                self.start_media_import(path, at);
            }
        } else if !self.media_ui.ffmpeg_window {
            // Set-up window closed without FFmpeg: give up on those.
            self.media_waiting.clear();
        }
        if let Some(t) = out.toast {
            self.toast = Some((t, Instant::now()));
        }
    }

    /// Apply a toolbar colour / size change to the selected objects.
    /// `prev_*` are the tool's values before the toolbar ran this frame.
    fn restyle_selection(&mut self, prev_color: [u8; 4], prev_size: f32) {
        if self.tool.kind != crate::tools::ToolKind::Select || self.tool.select.sel.is_empty() {
            return;
        }
        let color = (self.tool.color != prev_color).then_some(self.tool.color);
        let width = ((self.tool.size - prev_size).abs() > 1e-4).then_some(self.tool.size);
        if color.is_none() && width.is_none() { return; }
        let idx = self.canvases.active_index();
        let sel = self.tool.select.sel.clone();
        let c = self.canvases.active_mut();
        let li = c.active_layer;
        // Fold a run of size ticks into the entry the first one made.
        let merge = color.is_none() && self.restyle_run == Some((idx, c.undo_log.len()));
        c.restyle_objects(li, &sel.strokes, &sel.shapes, color, width, merge);
        self.restyle_run = (width.is_some() && color.is_none()).then_some((idx, c.undo_log.len()));
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
            self.idle_frame = refresh_period(self.osn_hwnd);
            self.first_frame = false;
            log::info!("on-screen-notes started: overlay visible, input enabled");
        }

        // ---- Always-on subsystems ----------------------------------------
        // 1) Click-through edge detection (Alt key).
        let click_through_now = click_through::update(ctx, &mut self.modifiers);

        // 2) Global hotkey events.
        if let Some(h) = self.hotkeys.as_ref() {
            for ev in h.poll() {
                match ev {
                    HotkeyId::ToggleOverlay => self.overlay_visible = !self.overlay_visible,
                    // Screenshot: enter interactive region-select; the grab
                    // is deferred until the user drags out a rectangle.
                    HotkeyId::Screenshot   => self.begin_region_select(),
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
                false
            } else {
                true
            }
        } else {
            false
        };
        if do_capture {
            if let Some(ps) = self.pending_screenshot.take() {
                self.do_screenshot(&ps);
                self.ui_visible = ps.ui_visible_before;
                self.config.bg_opacity = ps.bg_opacity_before;
            }
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

        // Same state machine for the lasso-select capture. The polygon
        // is owned (not Copy), so we `take()` the whole pending struct
        // once the countdown elapses rather than copying fields out.
        let do_lasso = self
            .pending_lasso
            .as_mut()
            .map(|ps| {
                if ps.frames_to_wait > 0 {
                    ps.frames_to_wait -= 1;
                    false
                } else {
                    true
                }
            })
            .unwrap_or(false);
        if self.pending_lasso.is_some() && !do_lasso {
            ctx.request_repaint();
        }
        if do_lasso {
            if let Some(ps) = self.pending_lasso.take() {
                self.do_lasso_capture(ps.pixels_per_point, ps.pan, ps.zoom, &ps.polygon);
                self.ui_visible = ps.ui_visible_before;
                self.config.bg_opacity = ps.bg_opacity_before;
            }
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
        // Sync the active tool's smoothing snapshot from the live
        // config. A new stroke begun this frame will lock-in the
        // current options at pen-down; an in-progress stroke keeps the
        // snapshot it was started with.
        self.tool.smoothing = self.config.smoothing;
        // Push the render-side fan-corner toggle so the ribbon mesh
        // builder can read it lock-free on the hot path.
        crate::canvas::render::set_fan_corners(
            self.config.smoothing.fan_corners,
            self.config.smoothing.fan_corners_step,
        );

        use crate::input::pen::PenEvent;
        let mut pen_events: Vec<PenEvent> = Vec::with_capacity(raw_pen_events.len());
        // Krita modes (Stabilizer / Weighted / Simple / None) run their
        // own algorithm on raw input — the upstream PenFilter would be
        // a second-stage smoother the user did not ask for. Only
        // Adaptive mode keeps it.
        let use_pen_filter = self.config.smoothing.use_upstream_pen_filter();
        for ev in raw_pen_events {
            match ev {
                PenEvent::Down(s) => {
                    self.pen_filter.reset();
                    if use_pen_filter {
                        if let Some(fs) = self.pen_filter.filter(s, true) {
                            pen_events.push(PenEvent::Down(fs));
                        }
                    } else {
                        pen_events.push(PenEvent::Down(s));
                    }
                }
                PenEvent::Move(s) => {
                    if use_pen_filter {
                        if let Some(fs) = self.pen_filter.filter(s, false) {
                            pen_events.push(PenEvent::Move(fs));
                        }
                    } else {
                        pen_events.push(PenEvent::Move(s));
                    }
                }
                PenEvent::Up(s) => {
                    if use_pen_filter {
                        if let Some(fs) = self.pen_filter.filter(s, true) {
                            pen_events.push(PenEvent::Up(fs));
                        }
                    } else {
                        pen_events.push(PenEvent::Up(s));
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
        // Esc also closes the layer-preview popup.
        if self.show_layer_preview && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.show_layer_preview = false;
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
            // Keep the Select tool's selection honest before anything
            // this frame reads it: it is dropped the moment the tool,
            // the slide, the layer or the layer's object count changes
            // underneath it.
            self.tool.select.sync(
                self.tool.kind == crate::tools::ToolKind::Select,
                self.canvases.active_index(),
                self.canvases.active(),
            );
            let (prev_color, prev_size) = (self.tool.color, self.tool.size);
            let mut insert_media = false;
            let toolbar_clicked = if self.ui_visible {
                ui::toolbar::show(
                    ctx,
                    &mut self.tool,
                    &mut self.config.bg_opacity,
                    &mut self.config.bg_color,
                    &mut self.config.palette,
                    &mut self.config.smoothing_level,
                    &mut self.config.smoothing,
                    &mut self.config.pressure_curve,
                    &mut self.pinned_hwnd,
                    &mut self.pin_picking,
                    &mut insert_media,
                )
            } else {
                false
            };
            if insert_media && self.media_dialog.is_none() {
                let mut exts = vec!["gif"];
                exts.extend_from_slice(crate::media::import::VIDEO_EXTS);
                exts.extend_from_slice(crate::media::import::IMAGE_EXTS);
                exts.push("pur");
                self.media_dialog = Some(ui::media_controls::pick_files(
                    self.osn_hwnd,
                    "Insert picture, video, GIF or PureRef board",
                    &[("Pictures, videos, GIFs and PureRef boards", &exts), ("PureRef boards", &["pur"])],
                    true,
                ));
            }
            // A colour or size picked in the toolbar while objects are
            // selected restyles those objects.
            self.restyle_selection(prev_color, prev_size);
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
            let layer_xform = self.layer_xform_for_render();
            // Advance video / GIF playback and upload new frames before
            // the canvas paints them.
            self.media.sync(ctx, self.canvases.active(), ctx.pixels_per_point());
            // Selection handles must stay out of anything that grabs the
            // screen.
            let show_selection = self.pending_screenshot.is_none()
                && self.pending_freeze.is_none()
                && self.pending_lasso.is_none()
                && self.region_select.is_none();
            let canvas_rect = ui::overlay::show(
                ctx,
                &mut self.overlay_cache,
                &self.canvases,
                &self.tool,
                self.config.smoothing_level.position_passes(),
                self.ui_visible,
                self.loupe.as_ref(),
                layer_xform,
                show_selection,
                &self.media,
            );

            // ---- Videos & GIFs ------------------------------------------
            // New files from the dialog / drag-and-drop, finished imports,
            // then the playback bars. Drawn before the pen dispatch below
            // so a tap on a bar counts as UI, never as ink.
            self.poll_media_inputs(ctx, canvas_rect);
            let media_objs = ui::media_controls::media_on_screen(self.canvases.active(), canvas_rect);
            let media_selected = if self.tool.kind == crate::tools::ToolKind::Select {
                ui::media_controls::selected_media(self.canvases.active(), &self.tool.select.sel)
            } else {
                None
            };
            let show_media_bars = self.ui_visible
                && show_selection
                && !click_through_now
                && !self.recorder.is_recording()
                && self.tool.in_progress_stroke.is_none()
                && self.tool.select.gesture.is_none();
            let mut media_out =
                self.media_ui.show(ctx, &self.media, &media_objs, media_selected, canvas_rect, show_media_bars);
            self.media_ui.ffmpeg_window(ctx, self.osn_hwnd, &mut media_out);
            self.apply_media_out(media_out);

            // ---- Screenshot region selection --------------------------------
            // Draws the dimmed rubber-band over the canvas and, on release,
            // turns the drag into a deferred region capture. While active it
            // suppresses the pen/mouse drawing dispatch below (guarded on
            // `region_select.is_none()`).
            self.update_region_select(ctx);

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
                ctx.set_cursor_icon(if layer_mode || self.layer_xform.is_some() {
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
                            // Only a real drag ends here. A wheel-only
                            // layer zoom stays open until Space is
                            // released, so a stray pen tap mid-zoom
                            // doesn't chop the gesture into two undo
                            // steps.
                            if self.pan_drag_anchor.take().is_some() {
                                self.commit_layer_xform();
                            }
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
                    } else if self.pan_drag_anchor.take().is_some() {
                        // Button-up ends a drag; same reasoning as the
                        // pen path — a wheel-only zoom keeps going
                        // until Space is released.
                        self.commit_layer_xform();
                    }
                }
            } else {
                // Pan released — drop anchor, restore default cursor.
                self.pan_drag_anchor = None;
                // Letting go of Space mid-gesture still ends it, so bake
                // any pending layer move / zoom rather than losing it.
                // This is also the only end-point a wheel-only layer
                // zoom has.
                self.commit_layer_xform();

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
                    crate::tools::ToolKind::Fill | crate::tools::ToolKind::LassoSelect,
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
                } else if self.tool.kind == crate::tools::ToolKind::Select && !on_ui {
                    // Say what a press here would do: grab a handle,
                    // move the selection, or pick the object underneath.
                    if let Some(p) = hover {
                        let c = self.canvases.active();
                        let local = [
                            ((p.x - canvas_rect.min.x) - c.pan[0]) / c.zoom,
                            ((p.y - canvas_rect.min.y) - c.pan[1]) / c.zoom,
                        ];
                        if let Some(icon) = select_cursor(&self.tool.select, local, c) {
                            ctx.set_cursor_icon(icon);
                        }
                    }
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
            // The tablet driver's synthetic mouse press can reach us a frame
            // before the pen's own packet does. If the mouse path began a
            // gesture for it, drop that now — the pen events below are the
            // real thing, and the stroke must not be drawn twice.
            if !pen_events.is_empty() && self.mouse_gesture {
                self.tool.cancel();
                self.eraser_override.cancel();
                self.mouse_gesture = false;
                self.pen_using_eraser = false;
            }
            if !space_held && self.loupe.is_none() && self.region_select.is_none() {
                // Mirror the current tool's size onto the eraser
                // override so the temporary erase uses the same brush
                // size as the active pen — feels natural while letting
                // the user keep their pen tool selected.
                self.eraser_override.size  = self.tool.size;
                self.eraser_override.color = self.tool.color;

                let ctrl_held  = ctx.input(|i| i.modifiers.ctrl);
                let shift_held = ctx.input(|i| i.modifiers.shift);
                // In the Select tool Shift means "add to / remove from
                // the selection" (and "snap rotation"), and Ctrl is left
                // for the copy / duplicate chords — so neither the
                // Shift colour-pick nor the Ctrl eraser applies there.
                // The `I` eyedropper still works.
                let select_active = self.tool.kind == crate::tools::ToolKind::Select;
                self.tool.select.shift = shift_held;

                // Shift-pick: while Shift is held over the canvas, give the
                // user a crosshair so they know the next tap will sample a
                // pixel instead of drawing ink. Toolbar / popups stay
                // visible — `set_cursor_icon` does not affect drawing
                // dispatch on its own, only the OS cursor glyph.
                if shift_held && !select_active && !self.pen_blocked {
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
                            // Wintab runs a *system* context, so it hands
                            // us pen-down packets even when the tip is over
                            // another app's window — OSN is not
                            // always-on-top, so another window may sit
                            // above it (or we may be in Alt-passthrough).
                            // Without this guard every such tap falls
                            // through to `reclaim_foreground` below and
                            // yanks focus back to OSN: the user clicks a
                            // different app with the pen and OSN keeps
                            // stealing focus. Compare the top-level window
                            // under the pen tip against our own HWND; if
                            // it's a real, different window the user is
                            // drawing/clicking *there*, so swallow the
                            // whole gesture (Move + Up too, via
                            // `pen_blocked`) and don't reclaim.
                            let sx = client_origin[0] + s.pos[0] as i32;
                            let sy = client_origin[1] + s.pos[1] as i32;
                            let win_under = crate::platform::top_level_window_at(sx, sy);
                            if win_under != 0 && win_under != self.osn_hwnd {
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
                            if (shift_held && !select_active) || self.tool.eyedropper_pending {
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
                            self.pen_using_eraser = ctrl_held && !select_active;
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

            // ---- Mouse ----------------------------------------------------
            // The mouse draws whenever the pen is not the one driving the
            // pointer. A tablet driver moves the system cursor and presses
            // the left button on the pen's behalf, so while the pen is
            // down, or was used a moment ago, those synthetic mouse
            // events must be ignored or every pen stroke would be drawn
            // twice. With the pen out of range — or no tablet at all —
            // the mouse has the canvas to itself.
            let pen_owns_pointer = !pen_events.is_empty()
                || self.pen.is_down()
                || self.pen.used_within(PEN_OWNS_POINTER);
            if !space_held && !pen_owns_pointer && !click_through_now
                && self.region_select.is_none() && self.loupe.is_none()
            {
                let (shift_held, ctrl_held, primary_now, cursor) = ctx.input(|i| {
                    (
                        i.modifiers.shift,
                        i.modifiers.ctrl || i.modifiers.command,
                        i.pointer.primary_down(),
                        i.pointer.hover_pos().or(i.pointer.interact_pos()),
                    )
                });
                // Same rule as the pen path: Shift belongs to the Select
                // tool while it is active.
                let select_active = self.tool.kind == crate::tools::ToolKind::Select;
                self.tool.select.shift = shift_held;
                // Shift + click = colour pick. Resolve here so the
                // sampling click does not also start a stroke.
                let picking = (shift_held && !select_active) || self.tool.eyedropper_pending;
                if picking {
                    ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
                }
                if picking && primary_now && !self.mouse_was_down {
                    if let Some(p) = cursor {
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
                if !primary_now && !self.mouse_gesture {
                    self.mouse_was_down = false;
                }
                if picking {
                    // Skip the rest of the mouse-drawing path entirely
                    // while picking; otherwise a release would synthesise
                    // a pen-up event over the freshly picked colour.
                } else if let Some(p) = cursor {
                    let cursor_screen = [
                        (p.x - canvas_rect.min.x).max(0.0),
                        (p.y - canvas_rect.min.y).max(0.0),
                    ];
                    if let Some(ev) = interaction::mouse_to_pen_event(
                        primary_now,
                        &mut self.mouse_was_down,
                        cursor_screen,
                        self.canvases.active(),
                    ) {
                        if matches!(ev, PenEvent::Down(_)) {
                            // A press on the toolbar, the layer panel or a
                            // popup is a click on that widget, not ink on
                            // the canvas underneath it. Decided once at
                            // press; the rest of the drag follows it.
                            let on_ui = toolbar_clicked
                                || ctx
                                    .layer_id_at(p)
                                    .map(|l| l.order > egui::Order::Background)
                                    .unwrap_or(false);
                            self.mouse_gesture = !on_ui;
                            if self.mouse_gesture {
                                // Same reasons as the pen path: keep
                                // keyboard shortcuts flowing here, and
                                // let Ctrl turn this drag into an erase.
                                crate::platform::reclaim_foreground(self.osn_hwnd);
                                self.pen_using_eraser = ctrl_held && !select_active;
                            }
                        }
                        if self.mouse_gesture {
                            let target_tool: &mut ActiveTool = if self.pen_using_eraser {
                                &mut self.eraser_override
                            } else {
                                &mut self.tool
                            };
                            crate::input::pen::dispatch(ev, target_tool, self.canvases.active_mut());
                            if matches!(ev, PenEvent::Up(_)) {
                                self.mouse_gesture = false;
                                self.pen_using_eraser = false;
                            }
                        }
                    }
                }
            } else if !self.mouse_gesture {
                // Someone else has the pointer. Follow the button so that
                // when the mouse gets it back, drawing needs a fresh
                // press rather than picking up one that was never its
                // own (the pen's synthetic press, a pan drag…). A mouse
                // drag that is merely paused — Space held mid-stroke —
                // keeps its state so its release still ends the stroke.
                self.mouse_was_down = ctx.input(|i| i.pointer.primary_down());
            }

            // ---- Middle-button pan ----------------------------------------
            // Middle drag pans the viewport without touching the keyboard.
            // Drawing only reads the primary button, so the two never
            // compete. Runs after the tool cursors above so the grab hand
            // wins. Space owns the pan while held; a press on the toolbar
            // or a popup is not a pan.
            let (middle_down, middle_pressed, cursor) = ctx.input(|i| {
                (
                    i.pointer.middle_down(),
                    i.pointer.button_pressed(egui::PointerButton::Middle),
                    i.pointer
                        .hover_pos()
                        .or(i.pointer.interact_pos())
                        .or(i.pointer.latest_pos()),
                )
            });
            if !middle_down || space_held || click_through_now
                || self.loupe.is_some() || self.region_select.is_some()
            {
                self.middle_pan_anchor = None;
            } else if let Some(p) = cursor {
                match self.middle_pan_anchor {
                    Some(anchor) => self.apply_pan_delta(p.x - anchor.x, p.y - anchor.y),
                    None if middle_pressed => {
                        let on_ui = ctx
                            .layer_id_at(p)
                            .map(|l| l.order > egui::Order::Background)
                            .unwrap_or(false);
                        if !on_ui {
                            self.begin_pan_gesture(false);
                            self.middle_pan_anchor = Some(p);
                        }
                    }
                    None => {}
                }
                if self.middle_pan_anchor.is_some() {
                    self.middle_pan_anchor = Some(p);
                    ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
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

            // ---- Deferred lasso-select capture ----------------------------
            // The Lasso-Select tool stashes the closed polygon on pen-up;
            // run the (chrome-hidden) screen capture here where the HWND +
            // DPI are available, then insert the masked region as a bottom
            // image layer.
            if let Some(poly) = self.tool.pending_lasso_capture.take() {
                self.queue_lasso_capture(poly, ctx.pixels_per_point());
            }

            // ---- Keyboard shortcuts --------------------------------------
            // Both Ctrl+0 (reset view) and Ctrl+Shift+F (freeze frame)
            // used to be matched via `ctx.input_mut(consume_shortcut)`
            // with hardcoded `KeyboardShortcut::new` values. They now
            // come from `HotkeysConfig`, so we build the shortcut from
            // the parsed binding and feed it to the same consumer —
            // egui still does the atomic modifier+key match for us.
            //
            // `reset_layer_pos` (Ctrl+Shift+0) is tested *before*
            // `reset_view` (Ctrl+0) and deliberately so: `consume_shortcut`
            // matches modifiers logically, i.e. a held Shift does not
            // disqualify a Ctrl-only pattern. Checking Ctrl+0 first would
            // let it swallow Ctrl+Shift+0 and reset the view instead.
            //
            // Layer pans are baked into the geometry, so "reset" is just
            // a move by the negated running total — which also means it
            // lands in the undo history like any other move.
            if let Some(b) = self.hotkeys_cfg.canvas.reset_layer_pos {
                let sc = egui::KeyboardShortcut::new(b.to_modifiers(), b.key);
                if ctx.input_mut(|i| i.consume_shortcut(&sc)) {
                    let c = self.canvases.active_mut();
                    let li = c.active_layer;
                    if let Some(m) = c.layers.get(li).map(|l| l.moved_by) {
                        c.move_layer_by(li, [-m[0], -m[1]]);
                    }
                }
            }
            // Layer zooms bake into the geometry the same way pans do,
            // so "reset" divides the running total back out. The pivot
            // is the layer's own bounding-box centre, which keeps the
            // layer where the user can see it — scaling about the
            // canvas origin instead would fling a far-from-origin layer
            // off-screen. `moved_by` is rewritten by the same maths, so
            // a later `reset_layer_pos` still lands on the layer's
            // original coordinates exactly.
            if let Some(b) = self.hotkeys_cfg.canvas.reset_layer_zoom {
                let sc = egui::KeyboardShortcut::new(b.to_modifiers(), b.key);
                if ctx.input_mut(|i| i.consume_shortcut(&sc)) {
                    let c = self.canvases.active_mut();
                    let li = c.active_layer;
                    if let Some(layer) = c.layers.get(li) {
                        let a = layer.scaled_by;
                        if a > 1e-6 && (a - 1.0).abs() > 1e-6 {
                            let pivot = layer
                                .content_bounds()
                                .map(|bb| [(bb[0] + bb[2]) * 0.5, (bb[1] + bb[3]) * 0.5])
                                .unwrap_or([0.0, 0.0]);
                            c.scale_layer_about(li, 1.0 / a, pivot);
                        }
                    }
                }
            }
            if let Some(b) = self.hotkeys_cfg.canvas.reset_view {
                let sc = egui::KeyboardShortcut::new(b.to_modifiers(), b.key);
                if ctx.input_mut(|i| i.consume_shortcut(&sc)) {
                    let (pan, zoom) = self.canvases.active_view_mut();
                    *zoom = 1.0;
                    *pan  = [0.0, 0.0];
                }
            }
            if let Some(b) = self.hotkeys_cfg.canvas.freeze_frame {
                let sc = egui::KeyboardShortcut::new(b.to_modifiers(), b.key);
                if ctx.input_mut(|i| i.consume_shortcut(&sc)) {
                    self.queue_freeze(ctx.pixels_per_point());
                }
            }

            // Text tool steals keyboard input while a caret is open.
            // Returns true when the events were consumed — in that
            // case we skip the tool-shortcut / undo / canvas-nav
            // dispatcher so typing "t" / "Ctrl+Z" lands in the label,
            // not as a tool change or an undo.
            let text_consumed = interaction::route_text_input(ctx, &mut self.tool, &mut self.canvases);

            // Select-tool commands found in this frame's events. They
            // are only collected inside the input closure and run after
            // it, because several of them need `&mut self` as a whole.
            let mut sel_actions: Vec<SelAction> = Vec::new();
            // Play / step keys, applied after the input closure. Not while
            // a text field has the keyboard.
            let mut media_keys: Vec<crate::media::Cmd> = Vec::new();
            let media_keys_live = !ctx.wants_keyboard_input();

            ctx.input(|i| {
                let mods = i.modifiers;
                // Locals retained for the scroll handler below, which
                // takes raw bools (not Modifiers).
                let ctrl  = mods.ctrl || mods.command;
                let shift = mods.shift;
                if text_consumed {
                    // Still allow scroll-wheel events (handled below
                    // outside this for-loop) but drop key events.
                    // Falling through to the wheel block below means
                    // we skip the per-event match by emptying the
                    // iterator.
                    return;
                }
                let keys = &self.hotkeys_cfg;
                for ev in &i.events {
                    match ev {
                        // egui-winit swallows the Ctrl+C / X / V key
                        // presses and hands us these instead.
                        egui::Event::Copy => sel_actions.push(SelAction::Copy),
                        egui::Event::Cut => sel_actions.push(SelAction::Cut),
                        egui::Event::Paste(text) if text == CLIP_MARKER => {
                            sel_actions.push(SelAction::PasteObjects);
                        }
                        // Elsewhere there is no key-state fallback (see
                        // `platform::paste_chord_down`), so a paste of
                        // anything that is not our own marker is the
                        // only cue to look for an image.
                        #[cfg(not(target_os = "windows"))]
                        egui::Event::Paste(_) => sel_actions.push(SelAction::PasteImage),
                        egui::Event::Key { key, pressed: true, .. } => {
                            let c = &keys.canvas;
                            let hit = |b: Option<crate::input::hotkeys_config::Binding>| {
                                b.is_some_and(|b| b.matches(*key, mods))
                            };
                            // Plain Backspace doubles as Delete: on a
                            // tablet-side keyboard it is the nearer key.
                            let bare_backspace = *key == egui::Key::Backspace
                                && !(mods.ctrl || mods.command || mods.shift || mods.alt);
                            if hit(c.delete_selection) || bare_backspace {
                                sel_actions.push(SelAction::Delete);
                            } else if hit(c.duplicate) {
                                sel_actions.push(SelAction::Duplicate);
                            } else if hit(c.bring_front) {
                                sel_actions.push(SelAction::ToFront);
                            } else if hit(c.send_back) {
                                sel_actions.push(SelAction::ToBack);
                            }
                            if media_keys_live {
                                if hit(c.media_play_pause) {
                                    media_keys.push(crate::media::Cmd::TogglePlay);
                                } else if hit(c.media_prev_frame) {
                                    media_keys.push(crate::media::Cmd::Step(-1));
                                } else if hit(c.media_next_frame) {
                                    media_keys.push(crate::media::Cmd::Step(1));
                                }
                            }
                        }
                        _ => {}
                    }
                    if let egui::Event::Key { key, pressed: true, .. } = ev {
                        // Tool switch shortcuts.
                        interaction::react_tool_shortcut(*key, mods, &mut self.tool, keys);
                        // Canvas + layer navigation.
                        interaction::react_canvas_nav(*key, mods, &mut self.canvases, keys);
                        interaction::react_layer_nav (*key, mods, &mut self.canvases, keys);
                        // Toggle floating UI chrome.
                        if let Some(b) = keys.canvas.toggle_ui {
                            if b.matches(*key, mods) {
                                self.ui_visible = !self.ui_visible;
                            }
                        }
                        // Undo / redo / clear / delete-canvas.
                        if let Some(b) = keys.canvas.undo {
                            if b.matches(*key, mods) {
                                self.canvases.active_mut().undo();
                                // The entry a size-slider run was being
                                // folded into may be gone now.
                                self.restyle_run = None;
                            }
                        }
                        if let Some(b) = keys.canvas.redo {
                            if b.matches(*key, mods) {
                                self.canvases.active_mut().redo();
                                self.restyle_run = None;
                            }
                        }
                        if let Some(b) = keys.canvas.clear_layer {
                            if b.matches(*key, mods) { self.canvases.active_mut().clear(); }
                        }
                        if let Some(b) = keys.canvas.delete_canvas {
                            if b.matches(*key, mods) { self.canvases.delete_active(); }
                        }
                        // Layer opacity: 1..9 set absolute N·10 %,
                        // 0 sets 0 %. `-` / `=` step ±10 %. All
                        // clamped to [0, 1]. Default bindings live in
                        // `HotkeysConfig::CanvasSection::opacity_*`.
                        for (n, b) in keys.canvas.opacity.iter().enumerate() {
                            if let Some(b) = b {
                                if b.matches(*key, mods) {
                                    let v = (n as f32) * 0.10;
                                    let c = self.canvases.active_mut();
                                    c.active_layer_mut().opacity = v;
                                    c.mark_dirty();
                                }
                            }
                        }
                        if let Some(b) = keys.canvas.opacity_dec {
                            if b.matches(*key, mods) {
                                let c = self.canvases.active_mut();
                                let l = c.active_layer_mut();
                                l.opacity = (l.opacity - 0.10).clamp(0.0, 1.0);
                                c.mark_dirty();
                            }
                        }
                        if let Some(b) = keys.canvas.opacity_inc {
                            if b.matches(*key, mods) {
                                let c = self.canvases.active_mut();
                                let l = c.active_layer_mut();
                                l.opacity = (l.opacity + 0.10).clamp(0.0, 1.0);
                                c.mark_dirty();
                            }
                        }
                        if let Some(b) = keys.canvas.preview_layer {
                            if b.matches(*key, mods) {
                                self.show_layer_preview = !self.show_layer_preview;
                            }
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
                    } else if space_held && shift {
                        // Shift+Space + wheel zooms the *active layer*
                        // about the cursor — the wheel counterpart of
                        // the Shift+Space drag that pans it. Same
                        // 1.10×-per-raw-unit feel as the canvas zoom so
                        // one notch is ≈ ±10 % either way.
                        if raw_delta_y.abs() >= 0.001 {
                            let c = self.canvases.active();
                            let cursor = i.pointer.hover_pos().unwrap_or_default();
                            let pivot = [
                                ((cursor.x - canvas_rect.min.x) - c.pan[0]) / c.zoom,
                                ((cursor.y - canvas_rect.min.y) - c.pan[1]) / c.zoom,
                            ];
                            self.apply_layer_zoom(1.10_f32.powf(raw_delta_y), pivot);
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
                            &mut self.canvases,
                            &mut self.config.bg_opacity,
                        );
                    }
                }
            });

            // ---- Select-tool commands -----------------------------------
            // Ctrl+V with only an image on the clipboard produces no egui
            // event at all, so watch the physical chord for that case.
            // When the clipboard holds our own marker text this fires
            // alongside `PasteObjects` and finds no image — harmless.
            let chord = crate::platform::paste_chord_down() && ctx.input(|i| i.focused);
            if chord && !self.paste_chord_prev && !text_consumed {
                sel_actions.push(SelAction::PasteImage);
            }
            self.paste_chord_prev = chord;
            if !sel_actions.is_empty() {
                let ppp = ctx.pixels_per_point();
                for action in sel_actions {
                    self.apply_sel_action(action, canvas_rect, ppp);
                }
            }
            if !media_keys.is_empty() {
                let pointer = ctx.input(|i| i.pointer.hover_pos());
                if let Some(id) = ui::media_controls::hotkey_target(&media_objs, media_selected, pointer) {
                    for cmd in media_keys {
                        self.media.command(id, cmd);
                    }
                }
            }

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
                let canvas_idx = self.canvases.active_index();
                // Pass the manager, not `active_mut()`: taking the mutable
                // canvas borrow every frame just to paint would flag it
                // dirty ~60×/s and put the autosave back into a permanent
                // rewrite loop. The panel takes it only on a real click.
                ui::layer_panel::show(
                    ctx,
                    &mut self.overlay_cache,
                    canvas_idx,
                    &mut self.canvases,
                );
            }
            // Layer preview popup — large centred thumbnail of the
            // active layer. Toggled by the `canvas.preview_layer`
            // hotkey (`q` by default). Esc closes (handled earlier in
            // this frame).
            if self.show_layer_preview {
                let canvas_idx = self.canvases.active_index();
                let active_li = self.canvases.active().active_layer;
                let layer_clone = self.canvases.active().layers[active_li].clone();
                let tex = self.overlay_cache.ensure_layer_thumb(
                    ctx, canvas_idx, active_li, &layer_clone,
                );
                let screen = ctx.screen_rect();
                egui::Area::new(egui::Id::new("osn_layer_preview_popup"))
                    .fixed_pos(egui::pos2(
                        screen.center().x - 260.0,
                        screen.center().y - 220.0,
                    ))
                    .show(ctx, |ui| {
                        egui::Frame {
                            inner_margin: egui::Margin::same(12.0),
                            outer_margin: egui::Margin::ZERO,
                            rounding: egui::Rounding::same(12.0),
                            shadow: ui::theme::card_shadow(),
                            fill: ui::theme::GLASS_FILL,
                            stroke: egui::Stroke::NONE,
                        }
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new(format!(
                                    "Layer {} — {} strokes · {} shapes",
                                    active_li + 1,
                                    layer_clone.strokes.len(),
                                    layer_clone.shapes.len(),
                                ))
                                .color(ui::theme::TEXT_PRIMARY),
                            );
                            ui.add_space(6.0);
                            match tex {
                                Some(tex_id) => {
                                    let size = egui::Vec2::new(480.0, 360.0);
                                    ui.add(
                                        egui::Image::new((tex_id, size))
                                            .fit_to_exact_size(size),
                                    );
                                }
                                None => {
                                    ui.label(
                                        egui::RichText::new("(empty layer)")
                                            .color(ui::theme::TEXT_MUTED),
                                    );
                                }
                            }
                            ui.add_space(4.0);
                            ui.label(
                                egui::RichText::new("Esc or hotkey to close")
                                    .small()
                                    .color(ui::theme::TEXT_MUTED),
                            );
                        });
                    });
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
        // Bumped from 1500 ms to 5000 ms because the save itself is
        // now (a) background-threaded, (b) dirty-gated so most ticks
        // are no-ops, and (c) using compact RON. Worst-case data loss
        // on a crash is now 5 s instead of 1.5 s — acceptable for an
        // annotation tool, and the UI stays smooth even with 9+
        // PNG-bearing canvases.
        if self.last_save_at.elapsed() > Duration::from_millis(5000) {
            // Media the in-app clipboard holds must survive a slide
            // delete; while an import or a board is being opened (its
            // pictures may be files a deleted slide also used), media
            // cleanup waits.
            let clipboard_media: std::collections::HashSet<String> = self
                .obj_clipboard
                .1
                .iter()
                .filter_map(|s| s.media_file().map(str::to_string))
                .collect();
            let media_in_use = (self.media_imports.is_empty() && self.pureref_jobs.is_empty()).then_some(&clipboard_media);
            self.canvases.save_all(media_in_use);
            if let Err(e) = crate::persistence::save_config(&self.config) {
                log::warn!("config save failed: {e}");
            }
            // The overlay may have moved to another monitor.
            self.idle_frame = refresh_period(self.osn_hwnd);
            self.last_save_at = Instant::now();
        }

        // Always request the next frame: pen events come from a polled
        // Wintab queue and global hotkeys are polled too, so frames have
        // to keep coming even when nothing on screen changes.
        //   * While the pen is in range or anything is in motion, at
        //     once. `request_repaint()` with no delay is what minimises
        //     pen-to-pixel latency — with Mailbox present mode each
        //     refresh shows the newest frame.
        //   * Otherwise one frame per display refresh. With no frame cap
        //     an idle overlay drew 1000+ fps and kept a core busy. The
        //     slower pace still drains Wintab well before a pen coming
        //     into range touches down (its driver moves the mouse too,
        //     which wakes egui at once), runs toasts and other timers,
        //     and input events still repaint immediately.
        if self.needs_live_frames(ctx) {
            ctx.request_repaint();
        } else {
            // egui takes `predicted_dt` (1/60 s unless set) off every
            // delay to aim for the frame *before* the deadline, which
            // turns a one-refresh delay into no delay at all. Add it back.
            let predicted = Duration::try_from_secs_f32(ctx.input(|i| i.predicted_dt)).unwrap_or_default();
            ctx.request_repaint_after(self.idle_frame + predicted);
        }
    }

    /// Save state on graceful exit so nothing is lost.
    /// Note: signature is `(&mut self)` only when the `glow` feature is off.
    /// With glow it would be `(&mut self, Option<&glow::Context>)`. Since we
    /// build with `wgpu` only, the no-arg form is correct.
    fn on_exit(&mut self) {
        // Shutdown path: block until every dirty canvas is on disk.
        // The background variant would race with process exit.
        self.canvases.save_all_blocking();
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


/// Cursor for the Select tool with the pointer at canvas point `p`: it
/// previews what a press there would do (or shows what the gesture in
/// flight is doing). `None` leaves the default arrow.
fn select_cursor(
    state: &crate::tools::select_tool::SelectState,
    p: [f32; 2],
    canvas: &crate::canvas::canvas::Canvas,
) -> Option<egui::CursorIcon> {
    use crate::tools::select_tool::{Gesture, Handle, Hover};
    use egui::CursorIcon as C;
    // Nearest of the four resize cursors to the direction the handle
    // actually pulls in, which follows the frame when it is turned.
    let resize = |x: i8, y: i8| {
        let deg = state.handle_direction(x, y, canvas).to_degrees().rem_euclid(180.0);
        if !(22.5..157.5).contains(&deg) {
            C::ResizeHorizontal
        } else if deg < 67.5 {
            C::ResizeNwSe
        } else if deg < 112.5 {
            C::ResizeVertical
        } else {
            C::ResizeNeSw
        }
    };
    match state.gesture {
        Some(Gesture::Move { .. }) | Some(Gesture::Rotate { .. }) => Some(C::Grabbing),
        Some(Gesture::Resize { x, y, .. }) => Some(resize(x, y)),
        Some(Gesture::Marquee { .. }) => Some(C::Crosshair),
        None => match state.hover(p, canvas) {
            Hover::Handle(Handle::Rotate) => Some(C::Grab),
            Hover::Handle(Handle::Scale { x, y }) => Some(resize(x, y)),
            Hover::Selection => Some(C::Move),
            Hover::Object => Some(C::PointingHand),
            Hover::Nothing => None,
        },
    }
}
