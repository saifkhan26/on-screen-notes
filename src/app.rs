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

use crate::canvas::shape::Shape;
use crate::canvas::CanvasManager;
use crate::config::AppConfig;
use crate::input::{
    hotkeys::{HotkeyId, Hotkeys},
    pen::{PenInput, RawHandleHolder},
};
use crate::interaction;
use crate::tools::ActiveTool;
use crate::ui;

mod capture;
mod frame;
mod frames;
mod keys;
mod layer_gesture;
mod selection;
mod sel_actions;
mod view;
mod slides;
mod confirm;
mod dup_check;
#[cfg(test)]
pub(crate) use dup_check::snapshot_bar as snapshot_dup_bar;
mod pins;
mod pointer;
mod media;
mod commands;
mod find;
mod similar;
mod pad;
mod crop;
mod workspace;
mod view_pass;
mod window_region;
mod press_log;

pub struct OnScreenNotesApp {
    config: AppConfig,
    /// Parsed user-editable hotkey bindings. Compiled once at app
    /// start; per-event dispatch is then a flat compare.
    hotkeys_cfg: crate::input::hotkeys_config::ParsedHotkeys,
    canvases: CanvasManager,
    /// What one window's canvas has of its own: the tool and its
    /// gestures, the selection, panels, find, crop, the pad. The slide
    /// window keeps another (`app::workspace`).
    ws: workspace::Workspace,
    pen: PenInput,
    hotkeys: Option<Hotkeys>,
    overlay_cache: ui::overlay::OverlayCache,

    /// Whether the main overlay is shown. Toggled by the global hotkey
    /// (default Ctrl+Alt+Shift+N).
    overlay_visible: bool,

    /// Debounce timer for "save canvases to disk".
    last_save_at: Instant,
    /// Gap between frames while nothing is happening: one refresh of
    /// the monitor the overlay is on. See the end of `update`.
    idle_frame: Duration,

    /// Set to false after the very first `update` runs. Used to push
    /// initial-state viewport commands (take input, take focus).
    first_frame: bool,




    /// One Euro Filter state for incoming pen samples. Reset on every
    /// pen-down so a new stroke does not inherit the previous stroke's
    /// filter state. See `input::filter` for the algorithm rationale.
    pen_filter: crate::input::filter::PenFilter,






    /// Native window handle of the overlay viewport, stored as the
    /// platform-native integer (HWND cast to `isize` on Windows, 0 on
    /// other platforms or when capture fails). Used by
    /// `crate::platform` to reclaim the foreground after the user worked
    /// in another app, among other things. 0 disables those calls.
    osn_hwnd: isize,

    /// Screen-recording state machine. Idle by default; toggled on /
    /// off via the global hotkey (default `Ctrl+Alt+Shift+R`). While
    /// capturing we hide the floating UI so it doesn't end up in the
    /// saved GIF.
    recorder: crate::recording::Recorder,
    /// That hotkey as the REC badge shows it, e.g. "Ctrl+Shift+Alt+R";
    /// `None` when the binding does not parse.
    record_hotkey: Option<String>,

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
    /// The slides' pictures read for finding ones like a picture; read
    /// only once find is given one.
    picture_index: crate::canvas::picture_index::PictureIndex,
    /// What each picture shows, for finding pictures of the same subject;
    /// read in the background while picture search is on.
    picture_vectors: crate::canvas::picture_vectors::PictureVectors,
    /// Pictures turned down for a tag; read once picture search needs them.
    tag_rejections: Option<crate::persistence::tag_rejections::Rejections>,
    /// What was ignored in the duplicates list; read once it is listed.
    copy_ignores: Option<crate::persistence::copy_ignores::CopyIgnores>,
    /// The pictures pasted or dropped last, checked for copies already on
    /// the slides (`app::dup_check`).
    dup_check: Option<dup_check::DupCheck>,
    /// Each tag's bars for the tag row's likely tags, kept from one
    /// picture to the next.
    tag_bars: similar::TagBars,
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

    /// Players for the videos and GIFs on the active slide.
    media: crate::media::MediaHub,
    /// Media types the toolbar's filter highlights.
    media_filter: ui::media_filter::MediaFilter,
    /// The second window showing one slide, while it is open.
    slide_window: Option<ui::slide_window::SlideWindow>,
    /// The slide window's own state (`app::workspace`), while it is open.
    window_ws: Option<workspace::Workspace>,
    /// Close the slide window once its frame is over (asked for in it).
    window_close: bool,
    /// Where the main window's canvas is, for the slide window's frame:
    /// a selection dragged out of the window lands there.
    main_canvas: Option<MainCanvas>,
    /// The pins window: the one place pins show (the canvas and the slide
    /// window show none), on the pins board, with its own state.
    pins_window: Option<ui::slide_window::SlideWindow>,
    pins_ws: Option<workspace::Workspace>,
    /// The window whose frame is running now, if not the overlay's.
    running: Option<WindowKind>,
    /// A pin double-clicked (or "Go to its slide") on the pins board: the
    /// main window goes to its slide once the slide window's frame is over.
    pin_to_open: Option<u64>,
    /// Objects dropped on the main window from the slide window's frame,
    /// to select there once that frame is over: their slide, and them.
    main_select: Option<(usize, crate::canvas::hit::Selection)>,
    /// Files being copied in and read as videos / GIFs.
    media_imports: Vec<crate::media::import::ImportJob>,
    /// Videos picked or dropped before FFmpeg was set up, with where
    /// they were dropped. Imported once it is; dropped if the setup
    /// window is closed without it.
    media_waiting: Vec<(std::path::PathBuf, Option<egui::Pos2>)>,
    /// Open dialog started by the toolbar's "Insert video / GIF".
    media_dialog: Option<std::sync::mpsc::Receiver<Vec<std::path::PathBuf>>>,
    /// The dialog asking which PureRef board to resync a slide from.
    resync_dialog: Option<(usize, std::sync::mpsc::Receiver<Vec<std::path::PathBuf>>)>,
    /// PureRef boards being read, each becoming a new slide.
    pureref_jobs: Vec<crate::media::pureref::OpenJob>,
    /// When the last frame started, for `hold_to_refresh_rate`.
    last_frame_at: Option<Instant>,
    /// Pen events `hold_to_refresh_rate` read while it waited, handed on
    /// with the frame's own (`smoothed_pen_events`).
    early_pen: Vec<crate::input::pen::PenEvent>,
    /// The picture the last in-app copy put on the OS clipboard, so a
    /// paste that finds it there pastes the objects instead. Set by the
    /// thread that builds it.
    clip_image: std::sync::Arc<std::sync::Mutex<Option<crate::clipboard::Fingerprint>>>,

    /// Since when the slide-loading bar has had something to show, so a
    /// quick load does not flash it.
    progress_since: Option<Instant>,

}

/// How long after the pen was last used the pointer still counts as
/// the pen's. Covers the gap between the pen lifting and the driver
/// releasing the mouse button it was holding down for it.
const PEN_OWNS_POINTER: Duration = Duration::from_millis(250);

/// How long after its last Wintab packet the pen still counts as in
/// range, keeping frames coming at full speed. A hovering pen sends
/// packets as it moves; this bridges the pauses of a hand held still.
const PEN_NEARBY: Duration = Duration::from_secs(1);

/// How often a frame held to the refresh rate looks for the pen touching
/// down while it waits (`hold_to_refresh_rate`).
const PEN_WATCH_STEP: Duration = Duration::from_millis(1);

/// Frame interval when nothing on screen moves by itself. Frames keep
/// coming this often because Wintab packets and global hotkeys are
/// polled per frame; anything the user does with the mouse, keyboard or
/// pen (whose driver moves the cursor as the pen comes into range) wakes
/// egui at once anyway. Every frame redraws the whole window, so at the
/// display's refresh rate an idle overlay kept the GPU a quarter busy.
const IDLE_TICK: Duration = Duration::from_millis(100);

/// The monitor's refresh period, or 60 Hz when Windows does not say.
fn refresh_period(hwnd: isize) -> Duration {
    let hz = crate::platform::refresh_rate_hz(hwnd).unwrap_or(60).clamp(30, 500);
    Duration::from_secs_f64(1.0 / f64::from(hz))
}

/// Text put on the OS clipboard by an in-app copy. Seeing it come back
/// in a paste means the newest thing the user copied is `obj_clipboard`.
const CLIP_MARKER: &str = "on-screen-notes:selection";

/// The view the peek key goes back to (`OnScreenNotesApp::update_peek`).
struct Peek {
    slide: usize,
    back: ([f32; 2], f32),
    since: Instant,
}

/// Colours found in a picture, for the arrange bar.
struct PictureColours {
    /// `(slide, layer, shape)` of the picture.
    at: (usize, usize, usize),
    colours: Vec<[u8; 4]>,
}

/// The box `[x0, y0, x1, y1]` around two others.
fn union_box(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]
}

/// A Select-tool command picked out of this frame's key events, run
/// after egui's input lock is released.
#[derive(Clone, Copy)]
enum SelAction {
    Delete,
    Duplicate,
    CopyToNewSlide,
    TogglePin,
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
    /// What the box is for.
    purpose: BoxFor,
}

/// The app's second windows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum WindowKind {
    /// The slide window: any slide, or part of one.
    Slide,
    /// The pins window: the pins and the pins board.
    Pins,
}

/// The main window's canvas, as the slide window's frame sees it.
#[derive(Clone, Copy)]
struct MainCanvas {
    /// The canvas area, in the main window's points.
    rect: egui::Rect,
    /// The main window's client area's top-left on screen, pixels.
    origin_px: [f32; 2],
    /// The main window's pixels per point.
    ppp: f32,
}

/// What a box dragged over the screen is for (`RegionSelect`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum BoxFor {
    /// A screenshot of what is inside it.
    Screenshot,
    /// The slide window shows that part of the slide.
    SlideWindow,
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
        // inherits these colours, rounding, spacing and fonts — the UI's
        // and the "handwritten" family the Text tool's labels are drawn
        // in (see `theme::install_fonts`).
        ui::theme::apply(&cc.egui_ctx);

        // Install egui_extras image loaders (SVG, plus the default raster
        // formats). This is what makes `egui::include_image!("...svg")` work
        // in the toolbar. Must be called exactly once, before any `Image`
        // widget renders.
        egui_extras::install_image_loaders(&cc.egui_ctx);


        // Committed ink is drawn from GPU memory when the device allows
        // it; see `canvas::ink_gpu`.
        if let Some(rs) = cc.wgpu_render_state.as_ref() {
            crate::canvas::ink_gpu::init(rs);
            // Drawing pads paint on the same device.
            crate::canvas::pad_gpu::init(rs);
        }

        let mut config = crate::persistence::load_config();
        // The UI at the size it was left at. The slides' views were kept
        // at this size too, so nothing is rescaled here (see
        // `apply_ui_scale`). egui's own Ctrl+= / Ctrl+- / Ctrl+0 zoom is
        // off (`theme::apply`): those keys are the user's to bind.
        config.ui_scale = crate::config::clamp_ui_scale(config.ui_scale);
        cc.egui_ctx.set_zoom_factor(config.ui_scale);
        // Before any slide is read: stroke caches built while loading
        // read the curve from this global.
        crate::canvas::stroke::set_pressure_curve(config.pressure_curve);
        // Reads the first slide now; the rest follow on background
        // threads so the window does not wait for them.
        let load_started = Instant::now();
        let canvases = CanvasManager::start_loading(&config);
        crate::perf::line(format!(
            "startup load: first of {} slides in {:.1} ms, the rest in the background",
            canvases.count(),
            crate::perf::ms(load_started.elapsed()),
        ));
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
        let record_hotkey = crate::input::hotkeys_config::parse(&hotkeys_file.global.start_record)
            .map(|b| b.label());

        let hotkeys = match Hotkeys::register(&hotkeys_file.global) {
            Ok(h) => Some(h),
            Err(e) => {
                log::warn!("could not register global hotkeys: {e}");
                None
            }
        };

        // Capture level before `config` is moved into the struct below.
        let level = config.smoothing_level;
        let mut eraser_override = ActiveTool::from_config(&config);
        eraser_override.kind = crate::tools::ToolKind::Eraser;

        // Grab the overlay's native window handle once — `crate::platform`
        // uses it to reclaim the foreground after the user's clicks pass
        // through to the app behind, among other things. A
        // failure here is non-fatal: the platform helpers silently no-op
        // when the handle is 0.
        let osn_hwnd: isize = {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            match cc.window_handle() {
                Ok(h) => match h.as_raw() {
                    #[cfg(target_os = "windows")]
                    RawWindowHandle::Win32(w) => w.hwnd.get(),
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
            ws: workspace::Workspace::new(tool, eraser_override),
            pen,
            hotkeys,
            overlay_cache: ui::overlay::OverlayCache::new(),
            overlay_visible: true,
            last_save_at: Instant::now(),
            idle_frame: Duration::from_secs_f64(1.0 / 60.0),
            first_frame: true,
            pen_filter: crate::input::filter::PenFilter::for_level(level),
            osn_hwnd,
            recorder: crate::recording::Recorder::default(),
            record_hotkey,
            ui_visible_before_record: true,
            toast: None,
            pending_screenshot: None,
            pending_lasso: None,
            region_select: None,
            loupe: None,
            pending_freeze: None,
            pinned_hwnd: None,
            pin_picking: false,
            pin_prev_lmb: false,
            picture_index: Default::default(),
            picture_vectors: Default::default(),
            tag_rejections: None,
            copy_ignores: None,
            dup_check: None,
            tag_bars: Default::default(),
            pinned_last_rect: None,
            ffmpeg_available: crate::recording::detect_ffmpeg(),
            obj_clipboard: (Vec::new(), Vec::new()),
            paste_count: 0,
            media: crate::media::MediaHub::new(),
            media_filter: ui::media_filter::MediaFilter::default(),
            slide_window: None,
            window_ws: None,
            window_close: false,
            main_canvas: None,
            main_select: None,
            pin_to_open: None,
            pins_window: None,
            pins_ws: None,
            running: None,
            media_imports: Vec::new(),
            media_waiting: Vec::new(),
            media_dialog: None,
            resync_dialog: None,
            pureref_jobs: Vec::new(),
            last_frame_at: None,
            early_pen: Vec::new(),
            clip_image: Default::default(),
            progress_since: None,
        }
    }


    // -- Space-drag gestures ---------------------------------------------

}

impl eframe::App for OnScreenNotesApp {
    /// We want *clear color* to be fully transparent so the window stays
    /// see-through. eframe queries this once per frame.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // Fully clear: the overlay's tint (`config.bg_color` at
        // `bg_opacity`) is painted in `update` as a rectangle with
        // Windows 11's rounded corners, which a clear colour cannot have
        // (`theme::paint_window`). Corners outside it stay see-through.
        [0.0; 4]
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.hold_to_refresh_rate(ctx);
        self.apply_ui_scale(ctx);
        ui::theme::set_accent(ctx, self.config.accent);
        // Frame timing for `OSN_PERF_LOG`; does nothing when it is unset.
        let _perf = crate::perf::frame(|| {
            let c = self.canvases.active();
            let (strokes, shapes) = c.layers.iter().fold((0, 0), |(a, b), l| (a + l.strokes.len(), b + l.shapes.len()));
            // Why frames are uncapped, when they are, and pen state.
            let live = [
                (self.pen.is_down() && !self.ws.pen_blocked, "pen-down"),
                (self.pen.is_down() && self.ws.pen_blocked, "pen-down-elsewhere"),
                (self.ws.tool.in_progress_stroke.is_some(), "stroke"),
                (self.pending_screenshot.is_some() || self.pending_freeze.is_some() || self.pending_lasso.is_some(), "capture"),
                (self.recorder.is_recording(), "recording"),
            ]
            .iter()
            .filter(|(on, _)| *on)
            .map(|(_, name)| *name)
            .collect::<Vec<_>>()
            .join(",");
            let (atlas, fill) = ctx.fonts(|f| (f.font_image_size(), f.font_atlas_fill_ratio()));
            format!(
                "slide {}/{} strokes {strokes} shapes {shapes} zoom {:.3}{} | {} | font atlas {}x{} {:.0}%",
                self.canvases.active_index() + 1,
                self.canvases.count(),
                c.zoom,
                if live.is_empty() { String::new() } else { format!(" flags {live}") },
                self.overlay_cache.stats(),
                atlas[0],
                atlas[1],
                fill * 100.0,
            )
        });
        // ---- Window background ------------------------------------------
        // The tint lets the desktop show through: 0 opacity is clear, 1
        // opaque; its colour can be paper-cream, deep navy… Painted with
        // Windows 11's rounded corners (square while maximized), under
        // everything else drawn this frame.
        {
            let square = ctx.input(|i| i.viewport().maximized == Some(true) || i.viewport().fullscreen == Some(true));
            let tint = ui::slide_window::Look::background(self.config.bg_color, self.config.bg_opacity);
            ui::theme::paint_window(&ctx.layer_painter(egui::LayerId::background()), ctx.screen_rect(), tint, square);
        }

        // ---- First-frame init -------------------------------------------
        // On startup, explicitly tell the OS: "we want input by default".
        // Only pin-picking makes the window pass clicks through, and only
        // while it runs.
        if self.first_frame {
            ctx.send_viewport_cmd(egui::ViewportCommand::MousePassthrough(false));
            // Open focused, like any app. (The window used to be made
            // `WS_EX_NOACTIVATE` here, so it opened unfocused and clicks
            // on the toolbar or a popup never focused it; a press on the
            // canvas still reclaims the foreground from another app.)
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            // Rounded corners, like any Windows 11 window (DWM squares
            // a window without a title bar unless asked).
            crate::platform::round_corners(self.osn_hwnd);
            self.idle_frame = refresh_period(self.osn_hwnd);
            self.first_frame = false;
            log::info!("on-screen-notes started: overlay visible, input enabled");
        }

        self.take_in_slides(ctx);

        self.advance_captures(ctx);

        let (pen_events, client_origin) = self.smoothed_pen_events(ctx);

        self.follow_pinned_window(ctx);

        self.update_loupe(ctx);

        // ---- This window's canvas, its tool and its UI -------------------
        self.ws.hwnd = self.osn_hwnd;
        self.view_pass(ctx, &pen_events, client_origin, self.overlay_visible);

        // ---- Sync last-used tool state into persistent config ------------
        // Treat the most recently selected colour and size as the "default"
        // for the next session, so launching the app does not always reset
        // to red 6 px. The debounced auto-save below writes these to disk.
        self.config.default_pen_color = self.ws.tool.color;
        self.config.default_pen_size  = self.ws.tool.size;
        self.config.default_stroke_style = self.ws.tool.style;

        self.show_rec_and_toast(ctx);

        // ---- Slide loading bar + lock badge ------------------------------
        if self.overlay_visible {
            self.show_slide_status(ctx);
        }

        self.show_windows(ctx);

        self.autosave();

        self.request_next_frame(ctx);
    }

    /// Save state on graceful exit so nothing is lost.
    /// Note: signature is `(&mut self)` only when the `glow` feature is off.
    /// With glow it would be `(&mut self, Option<&glow::Context>)`. Since we
    /// build with `wgpu` only, the no-arg form is correct.
    fn on_exit(&mut self) {
        // Picture search's threads stop before ONNX Runtime is unloaded.
        self.picture_vectors.stop();
        // A pad still open goes into its slide first.
        self.close_pad(false);
        // Shutdown path: block until every dirty canvas is on disk.
        // The background variant would race with process exit.
        self.canvases.save_all_blocking();
        if let Err(e) = crate::persistence::save_config(&self.config) {
            log::warn!("config save on exit failed: {e}");
        }
    }
}

/// A file size for the user: "1.3 GB", "540 MB".
fn size_label(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 30 => format!("{:.1} GB", b as f64 / (1u64 << 30) as f64),
        b if b >= 10 << 20 => format!("{} MB", b >> 20),
        b => format!("{:.1} MB", b as f64 / (1u64 << 20) as f64),
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


/// Nearest of the four resize cursors to the on-screen direction
/// (radians, y down) a handle pulls in, which follows a turned frame.
fn resize_cursor(direction: f32) -> egui::CursorIcon {
    use egui::CursorIcon as C;
    let deg = direction.to_degrees().rem_euclid(180.0);
    if !(22.5..157.5).contains(&deg) {
        C::ResizeHorizontal
    } else if deg < 67.5 {
        C::ResizeNwSe
    } else if deg < 112.5 {
        C::ResizeVertical
    } else {
        C::ResizeNeSw
    }
}

/// Cursor for the Select tool over pins: what a press at `q` (points
/// from the canvas area's top-left) would grab, or what the pin gesture
/// in flight is doing. `None` when the pointer is over no pin.
pub(crate) fn pin_cursor(
    tool: &crate::canvas::pins::PinTool,
    q: [f32; 2],
    pins: &[crate::canvas::pins::Pinned],
) -> Option<egui::CursorIcon> {
    use crate::canvas::pins::PinHandle;
    use egui::CursorIcon as C;
    let corner = |x: i8, y: i8| {
        let frame = tool.selected.and_then(|id| pins.iter().find(|p| p.pin.id == id)).map(|p| p.frame());
        resize_cursor(frame.map_or(0.0, |f| (y as f32 * f.half[1]).atan2(x as f32 * f.half[0]) + f.angle))
    };
    if tool.busy() {
        return Some(match tool.active_handle() {
            Some(PinHandle::Corner { x, y }) => corner(x, y),
            _ => C::Grabbing,
        });
    }
    match tool.hover(q, pins)? {
        Some(PinHandle::Corner { x, y }) => Some(corner(x, y)),
        Some(PinHandle::Rotate) => Some(C::Grab),
        None => Some(C::Move),
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
    let resize = |x: i8, y: i8| resize_cursor(state.handle_direction(x, y, canvas));
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
