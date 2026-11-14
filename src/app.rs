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
mod pins;
mod pointer;
mod media;
mod commands;

pub struct OnScreenNotesApp {
    config: AppConfig,
    /// Parsed user-editable hotkey bindings. Compiled once at app
    /// start; per-event dispatch is then a flat compare.
    hotkeys_cfg: crate::input::hotkeys_config::ParsedHotkeys,
    canvases: CanvasManager,
    tool: ActiveTool,
    pen: PenInput,
    hotkeys: Option<Hotkeys>,
    overlay_cache: ui::overlay::OverlayCache,

    /// Whether the main overlay is shown. Toggled by the global hotkey
    /// (default Ctrl+Alt+Shift+N).
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
    /// initial-state viewport commands (take input, take focus).
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
    /// The arrange bar's "Move to slide…" list opens next frame: its
    /// key was pressed after the bar was drawn.
    move_picker_pending: bool,
    /// Slide waiting on "Delete this slide?" — the delete key was
    /// pressed on a slide with something on it. Keys go to the dialog
    /// alone while it is up.
    confirm_delete: Option<usize>,
    /// Zoom to fit / to selection in progress: the view glides there
    /// over a few frames instead of jumping.
    view_glide: Option<interaction::zoom_pan::ViewGlide>,
    /// The peek key is held: the view went to the selection (or the
    /// whole slide) and goes back here when the key is let go.
    peek: Option<Peek>,
    /// The selected picture's colours, while the arrange bar shows them.
    picture_colours: Option<PictureColours>,
    /// The palette from before "Use as palette", for "Restore palette".
    /// Kept until the app closes.
    palette_before: Option<Vec<[u8; 4]>>,
    /// The command palette (Ctrl+K). Keys go to it alone while it is up.
    commands: ui::command_palette::Palette,
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
    /// `(canvas index, undo-log length, when)` right after the last
    /// nudge, likewise: a nudge soon after it on the same objects joins
    /// its undo entry, so a held arrow key is one Ctrl+Z.
    nudge_run: Option<(usize, usize, Instant)>,
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
    /// Media types the toolbar's filter highlights.
    media_filter: ui::media_filter::MediaFilter,
    /// The Select tool's hold on pins (objects kept on screen, on their
    /// own slide or on every slide):
    /// which one is selected, and the move / resize / turn in flight.
    pin_tool: crate::canvas::pins::PinTool,
    /// The slide panel (bottom-left): which slide is being named.
    slide_panel: ui::slide_panel::SlidePanel,
    /// The second window showing one slide, while it is open.
    slide_window: Option<ui::slide_window::SlideWindow>,
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
    /// When the last frame started, for `hold_to_refresh_rate`.
    last_frame_at: Option<Instant>,
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

        // Committed ink is drawn from GPU memory when the device allows
        // it; see `canvas::ink_gpu`.
        if let Some(rs) = cc.wgpu_render_state.as_ref() {
            crate::canvas::ink_gpu::init(rs);
        }

        let config = crate::persistence::load_config();
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
            tool,
            pen,
            hotkeys,
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
            move_picker_pending: false,
            confirm_delete: None,
            view_glide: None,
            peek: None,
            picture_colours: None,
            palette_before: None,
            commands: Default::default(),
            pinned_last_rect: None,
            ffmpeg_available: crate::recording::detect_ffmpeg(),
            obj_clipboard: (Vec::new(), Vec::new()),
            paste_count: 0,
            restyle_run: None,
            nudge_run: None,
            paste_chord_prev: false,
            mouse_gesture: false,
            media: crate::media::MediaHub::new(),
            media_ui: ui::media_controls::MediaUi::new(),
            media_filter: ui::media_filter::MediaFilter::default(),
            pin_tool: Default::default(),
            slide_panel: Default::default(),
            slide_window: None,
            media_imports: Vec::new(),
            media_waiting: Vec::new(),
            media_dialog: None,
            pureref_jobs: Vec::new(),
            last_frame_at: None,
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
        // Frame timing for `OSN_PERF_LOG`; does nothing when it is unset.
        let _perf = crate::perf::frame(|| {
            let c = self.canvases.active();
            let (strokes, shapes) = c.layers.iter().fold((0, 0), |(a, b), l| (a + l.strokes.len(), b + l.shapes.len()));
            // Why frames are uncapped, when they are, and pen state.
            let live = [
                (self.pen.is_down() && !self.pen_blocked, "pen-down"),
                (self.pen.is_down() && self.pen_blocked, "pen-down-elsewhere"),
                (self.tool.in_progress_stroke.is_some(), "stroke"),
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

        // ---- Zoom to fit / selection: glide and peek ----------------------
        self.update_peek(ctx);
        self.step_view_glide(ctx);

        // ---- Shape snap: time a held pen, run the morph --------------------
        // A pen held still sends nothing, so the frame it is due is asked for.
        if let Some(wait) = self.tool.tick_snap(Instant::now()) {
            ctx.request_repaint_after(wait);
        }

        // ---- Space-hold pan check ---------------------------------------
        // We treat Space as a *modal* pan key: while it is held, all pen +
        // mouse drags pan the active canvas instead of drawing. This is the
        // same convention used by Photoshop, Figma, Rnote, etc.
        let space_held = ctx.input(|i| i.key_down(egui::Key::Space)) && !self.slide_panel.is_renaming();

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
            // A locked slide, or one still opening, keeps no selection.
            self.tool.select.sync(
                self.tool.kind == crate::tools::ToolKind::Select && self.canvases.active_editable(),
                self.canvases.active_index(),
                self.canvases.active(),
            );
            self.tool.lasso.sync(self.tool.kind, self.canvases.active_index());
            // Pins are picked with the Select tool: leaving it lets go of
            // the pin, and so does unpinning it some other way (an undo).
            if self.tool.kind != crate::tools::ToolKind::Select || !self.canvases.active_editable() {
                self.pin_tool.clear();
            } else if let Some(id) = self.pin_tool.selected {
                if !self.canvases.pinned().iter().any(|p| p.pin.id == id) {
                    self.pin_tool.clear();
                }
            }
            let (prev_color, prev_size) = (self.tool.color, self.tool.size);
            let prev_text_box = self.tool.text_box;
            let text_selected = !self.selected_text().is_empty();
            let mut insert_media = false;
            let mut select_filtered = false;
            let media_counts = ui::media_filter::counts(self.canvases.active());
            let mut pins_hidden = self.canvases.pins_hidden();
            let has_pins = self.canvases.has_pins();
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
                    &mut self.config.shape_snap,
                    (&mut self.config.beside_fit, &mut self.config.beside_side),
                    &mut self.pinned_hwnd,
                    &mut self.pin_picking,
                    &mut insert_media,
                    &mut self.media_filter,
                    media_counts,
                    &mut select_filtered,
                    has_pins.then_some(&mut pins_hidden),
                    text_selected,
                    &self.hotkeys_cfg,
                )
            } else {
                false
            };
            self.set_pins_hidden(pins_hidden);
            self.tool.shape_snap = self.config.shape_snap;
            if self.tool.text_box != prev_text_box {
                self.apply_text_box_to_selection();
            }
            // Recently-used slide history: the active slide counts as
            // visited, and letting go of Ctrl ends a Ctrl+Tab walk.
            if !ctx.input(|i| interaction::slide_walk_held(i.modifiers, &self.hotkeys_cfg)) {
                self.canvases.end_recent_walk();
            }
            self.canvases.note_visit();
            // Slide panel. Going to a slide waits until after this frame's
            // keys, where the arrow keys switch slides too: everything
            // drawn before then still belongs to the current slide.
            let slides = if self.ui_visible {
                // No higher than the REC badge (top-left) while it shows.
                let top = if self.recorder.is_recording() { 44.0 } else { 10.0 };
                ui::slide_panel::show(ctx, &mut self.slide_panel, &mut self.config, top, &self.canvases)
            } else {
                Default::default()
            };
            if select_filtered && self.may_edit() {
                self.select_media_matches();
            }
            // Arrange bar for a selection (only its copy and pin buttons
            // for a single object); hidden mid-gesture so it doesn't
            // flicker over a drag. A selected pin gets its own bar there.
            let selected_count = self.tool.select.sel.strokes.len() + self.tool.select.sel.shapes.len();
            // A picture's colours belong to that picture: another
            // selection puts them away.
            let picture = self.selected_picture();
            if self.picture_colours.as_ref().is_some_and(|p| Some(p.at) != picture) {
                self.picture_colours = None;
            }
            let mut use_colour = None;
            if self.ui_visible
                && self.tool.kind == crate::tools::ToolKind::Select
                && selected_count >= 1
                && self.tool.select.gesture.is_none()
            {
                use ui::arrange_bar::BarAction;
                let pinned = self.selection_pinned();
                let grouped = self.selection_grouped();
                let open_move = std::mem::take(&mut self.move_picker_pending);
                let keys = &self.hotkeys_cfg;
                let row = picture.map(|_| ui::arrange_bar::ColourRow {
                    shown: self.picture_colours.as_ref().map(|p| p.colours.as_slice()),
                    can_restore: self.palette_before.is_some(),
                });
                match ui::arrange_bar::show(
                    ctx,
                    selected_count,
                    pinned,
                    &self.canvases,
                    &mut self.config.arrange_apart,
                    &mut self.config.pack_mode,
                    keys,
                    open_move,
                    row,
                    grouped,
                ) {
                    Some(BarAction::Group(on)) => self.group_selection(on),
                    Some(BarAction::Arrange(op)) => self.arrange_selection(op, ctx),
                    Some(BarAction::Match(m)) => self.match_selection_size(m),
                    Some(BarAction::CopyToNewSlide) => self.copy_selection_to_new_slide(),
                    Some(BarAction::MoveToSlide(to)) => self.move_selection_to(to),
                    Some(BarAction::TogglePin) => self.toggle_pin_selection(),
                    Some(BarAction::Flip(vertical)) => self.flip_selection(vertical),
                    Some(BarAction::ZoomToSelection) => self.zoom_to_selection(ctx),
                    Some(BarAction::PictureColours) => self.toggle_picture_colours(),
                    // Set after the restyle below, so the picture is
                    // not "recoloured" into an empty undo step.
                    Some(BarAction::UseColour(c)) => use_colour = Some(c),
                    Some(BarAction::KeepColours) => self.keep_picture_colours(),
                    Some(BarAction::RestorePalette) => self.restore_palette(),
                    None => {}
                }
            }
            self.show_pin_bar(ctx);
            if insert_media {
                self.open_media_dialog();
            }
            // A colour or size picked in the toolbar while objects are
            // selected restyles those objects.
            self.restyle_selection(prev_color, prev_size);
            if let Some(c) = use_colour {
                self.tool.color = c;
                self.toast = Some((format!("Drawing colour #{:02X}{:02X}{:02X}", c[0], c[1], c[2]), Instant::now()));
            }
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
            let view = self.overlay_cache.canvas_rect(ctx);
            // Pinned videos play wherever their pins are, the slide
            // window included. Its pins count as placed on the canvas.
            let mut pins = self.canvases.pinned();
            if self.slide_window.as_ref().is_some_and(|w| w.shows_pins()) {
                pins.extend(self.canvases.global_pins());
            }
            self.media.sync(ctx, self.canvases.active(), &pins, ctx.pixels_per_point(), view);
            drop(pins);
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
                self.media_filter,
                ui::overlay::PinMarks {
                    selected: self.pin_tool.selected,
                    outlines: self.ui_visible && self.tool.kind == crate::tools::ToolKind::Select,
                },
            );

            // ---- Videos & GIFs ------------------------------------------
            // New files from the dialog / drag-and-drop, finished imports,
            // then the playback bars. Drawn before the pen dispatch below
            // so a tap on a bar counts as UI, never as ink.
            self.poll_media_inputs(ctx, canvas_rect);
            let mut media_objs = ui::media_controls::media_on_screen(self.canvases.active(), canvas_rect);
            let pinned = self.canvases.pinned();
            media_objs.extend(ui::media_controls::pinned_media_on_screen(&pinned, canvas_rect));
            let media_selected = if self.tool.kind == crate::tools::ToolKind::Select {
                ui::media_controls::selected_media(self.canvases.active(), &self.tool.select.sel)
                    .or_else(|| self.pin_tool.selected.and_then(|id| ui::media_controls::pinned_media(&pinned, id)))
            } else {
                None
            };
            drop(pinned);
            let show_media_bars = self.ui_visible
                && show_selection
                && !self.recorder.is_recording()
                && self.tool.in_progress_stroke.is_none()
                && self.tool.select.gesture.is_none()
                && !self.pin_tool.dragging();
            let mut media_out =
                self.media_ui.show(ctx, &self.media, &media_objs, media_selected, canvas_rect, show_media_bars);
            self.media_ui.ffmpeg_window(ctx, self.osn_hwnd, &mut media_out);
            self.apply_media_out(media_out);
            self.show_delete_confirm(ctx);
            if self.commands.is_open() {
                let picked = {
                    let cx = ui::command_palette::Context {
                        keys: &self.hotkeys_cfg,
                        selected: if self.tool.kind == crate::tools::ToolKind::Select {
                            self.tool.select.sel.strokes.len() + self.tool.select.sel.shapes.len()
                        } else {
                            0
                        },
                        picture: self.selected_picture().is_some(),
                        grouped: self.selection_grouped(),
                        pin: self.pin_tool.selected.is_some() && self.tool.kind == crate::tools::ToolKind::Select,
                        palette: &self.config.palette,
                        slides: (0..self.canvases.count()).map(|i| self.canvases.name(i).to_string()).collect(),
                        active_slide: self.canvases.active_index(),
                    };
                    let all = ui::command_palette::entries(&cx);
                    self.commands.show(ctx, &all)
                };
                if let Some(cmd) = picked {
                    self.run_command(cmd, ctx, canvas_rect);
                }
            }

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
            //
            // On a slide that takes no edits (locked, or still opening) a
            // plain drag pans too. The laser and the spotlight still work
            // on a locked slide: they never change it.
            let editable = self.canvases.active_editable();
            let passive_tool = matches!(self.tool.kind, crate::tools::ToolKind::Laser | crate::tools::ToolKind::Spotlight);
            let tool_works_here = editable || (passive_tool && self.canvases.opening().is_none());
            let drag_pans = space_held || !tool_works_here;
            let pen_owns_pointer = !pen_events.is_empty()
                || self.pen.is_down()
                || self.pen.used_within(PEN_OWNS_POINTER);
            self.drag_pan(ctx, &pen_events, client_origin, drag_pans, space_held, editable, pen_owns_pointer, toolbar_clicked, canvas_rect);

            self.dispatch_pen(ctx, &pen_events, client_origin, drag_pans, editable, canvas_rect);

            self.dispatch_mouse(ctx, client_origin, drag_pans, pen_owns_pointer, editable, toolbar_clicked, canvas_rect);

            self.middle_button_pan(ctx, space_held);

            self.run_deferred_tools(ctx, canvas_rect);

            let keys = self.take_keys(ctx, &slides, space_held, canvas_rect);

            self.run_keys(ctx, keys, slides, canvas_rect, &media_objs, media_selected);

            self.show_layer_panel(ctx);
        }

        // ---- Sync last-used tool state into persistent config ------------
        // Treat the most recently selected colour and size as the "default"
        // for the next session, so launching the app does not always reset
        // to red 6 px. The debounced auto-save below writes these to disk.
        self.config.default_pen_color = self.tool.color;
        self.config.default_pen_size  = self.tool.size;
        self.config.default_stroke_style = self.tool.style;

        self.show_rec_and_toast(ctx);

        // ---- Slide loading bar + lock badge ------------------------------
        if self.overlay_visible {
            self.show_slide_status(ctx);
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

        self.show_slide_window(ctx, space_held);

        self.autosave();

        self.request_next_frame(ctx);
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
