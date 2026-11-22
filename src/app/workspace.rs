//! One window's own state: what the main window's canvas has, and the
//! slide window's has apart from it — the tool and its gestures in
//! flight, the selection, the panels, find, the palette, crop and the
//! drawing pad, the view's glide. Slides, the config, the pen, media and
//! caches stay with the app and are shared.

use super::*;

pub(super) struct Workspace {
    /// The window's native handle: a press over another window is not
    /// ours, and a press here takes the foreground back to it.
    pub(super) hwnd: isize,
    /// A colour clicked on the slide window's bar, which has no toolbar:
    /// taken at the toolbar's turn in the next frame, so it restyles a
    /// selection as a toolbar click would.
    pub(super) picked_colour: Option<[u8; 4]>,
    pub(super) tool: ActiveTool,
    /// Eraser tool used while Ctrl is held — temporary override so the
    /// user can erase mid-session without losing their current pen
    /// tool. Stays the same instance across gestures so its in-progress
    /// stroke state is preserved.
    pub(super) eraser_override: ActiveTool,
    /// Whether mouse left-button was down on the previous frame; used to
    /// synthesise pen events from mouse for the no-tablet fallback.
    pub(super) mouse_was_down: bool,
    /// Anchor for "hold Space + drag to pan". `Some(p)` while the user is
    /// actively dragging — `p` is the previous cursor position in window-
    /// client pixels. We compute the per-frame delta against this anchor
    /// and apply it to `canvas.pan`, then move the anchor forward.
    pub(super) pan_drag_anchor: Option<egui::Pos2>,
    /// Anchor for middle-button drag pan, same meaning as
    /// `pan_drag_anchor`. Kept separate because the Space path clears
    /// that one every frame Space is up.
    pub(super) middle_pan_anchor: Option<egui::Pos2>,
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
    pub(super) layer_xform: Option<LayerXform>,
    /// True while a pen-down landed on a floating UI layer (toolbar,
    /// popup, etc.). Subsequent Move events are dropped until the next
    /// Up so the user can interact with the menu without leaving ink
    /// behind it. Wintab bypasses the egui pointer, so we have to
    /// gate dispatch ourselves.
    pub(super) pen_blocked: bool,
    /// True while the current pen gesture (Down→Up) was launched with
    /// Ctrl held. Locks the dispatch target for the rest of the
    /// gesture so releasing Ctrl mid-stroke does not accidentally flip
    /// the destination tool.
    pub(super) pen_using_eraser: bool,
    /// True while a Shift-pick gesture is in progress: every Move and
    /// Up after the picking Down is silently swallowed so the user
    /// does not leave ink behind while sampling.
    pub(super) pen_picking: bool,
    /// True while the layer-preview popup is open. Toggled by the
    /// `canvas.preview_layer` hotkey; Esc closes. Renders a centred
    /// large thumbnail of the active layer so the user can confirm
    /// "what's on this layer" before drawing into it.
    pub(super) show_layer_preview: bool,
    /// Master visibility switch for floating UI (toolbar, status badge,
    /// layer panel, etc.). Toggled by the `Tab` shortcut so the user
    /// can hide the chrome and view their drawing un-occluded.
    /// Stroke + shape rendering is unaffected by this flag.
    pub(super) ui_visible: bool,
    /// The arrange bar's "Move to slide…" list opens next frame: its
    /// key was pressed after the bar was drawn.
    pub(super) move_picker_pending: bool,
    /// Slide waiting on "Delete this slide?" — the delete key was
    /// pressed on a slide with something on it. Keys go to the dialog
    /// alone while it is up.
    pub(super) confirm_delete: Option<usize>,
    /// Zoom to fit / to selection in progress: the view glides there
    /// over a few frames instead of jumping.
    pub(super) view_glide: Option<interaction::zoom_pan::ViewGlide>,
    /// The peek key is held: the view went to the selection (or the
    /// whole slide) and goes back here when the key is let go.
    pub(super) peek: Option<Peek>,
    /// The selected picture's colours, while the arrange bar shows them.
    pub(super) picture_colours: Option<PictureColours>,
    /// The palette from before "Use as palette", for "Restore palette".
    /// Kept until the app closes.
    pub(super) palette_before: Option<Vec<[u8; 4]>>,
    /// The command palette (Ctrl+K). Keys go to it alone while it is up.
    pub(super) commands: ui::command_palette::Palette,
    /// Find (Ctrl+F): text labels on every slide. Keys go to it alone
    /// while it is up.
    pub(super) find: ui::find_bar::FindBar,
    /// The label find just went to, flashing.
    pub(super) find_flash: Option<find::FindFlash>,
    /// Where find started, while it is open: Esc goes back there.
    pub(super) find_start: Option<find::FindStart>,
    /// Ctrl+V was held last frame, for find's own paste.
    pub(super) find_paste_prev: bool,
    /// A file dropped on find, being read.
    pub(super) find_reading: Option<similar::FindReading>,
    /// The pictures that show the words typed in find, being found.
    pub(super) word_search: Option<similar::WordSearch>,
    /// The slides' pictures being gathered into sets of copies for find.
    pub(super) copies_job: Option<similar::CopiesJob>,
    /// The picture (and the vectors' and tags' bars' generations) the tag
    /// row's likely tags are for.
    pub(super) likely_for: Option<((usize, usize, usize), u64, u64)>,
    /// The selection's tags being typed on the arrange bar. Keys go to
    /// it alone while it is open.
    pub(super) tag_edit: Option<ui::arrange_bar::TagEdit>,
    /// `(canvas index, undo-log length)` right after the last size
    /// restyle of a selection. While both still match, the next size
    /// tick is folded into that same undo entry — a slider drag is one
    /// Ctrl+Z, not hundreds.
    pub(super) restyle_run: Option<(usize, usize)>,
    /// `(canvas index, undo-log length, when)` right after the last
    /// nudge, likewise: a nudge soon after it on the same objects joins
    /// its undo entry, so a held arrow key is one Ctrl+Z.
    pub(super) nudge_run: Option<(usize, usize, Instant)>,
    /// Ctrl+V state on the previous frame, for edge detection — see
    /// `platform::paste_chord_down`.
    pub(super) paste_chord_prev: bool,
    /// True while a mouse drag that began on the canvas is being fed to
    /// a tool. False for a press that landed on a widget, and after the
    /// pen has taken a gesture over.
    pub(super) mouse_gesture: bool,
    /// Their playback bars, and the FFmpeg setup window.
    pub(super) media_ui: ui::media_controls::MediaUi,
    /// The Select tool's hold on pins (objects kept on screen, on their
    /// own slide or on every slide):
    /// which one is selected, and the move / resize / turn in flight.
    pub(super) pin_tool: crate::canvas::pins::PinTool,
    /// The slide panel (bottom-left): which slide is being named.
    pub(super) slide_panel: ui::slide_panel::SlidePanel,
    /// The picture being drawn on, if any (`app::pad`).
    pub(super) pad: pad::PadState,
    /// The picture being cropped, if any (`app::crop`).
    pub(super) crop: crop::CropState,
}

impl Workspace {
    /// A window's state at the start: `tool` as the config has it, and
    /// the eraser Ctrl holds.
    pub(super) fn new(tool: ActiveTool, eraser_override: ActiveTool) -> Self {
        Self {
            hwnd: 0,
            picked_colour: None,
            tool,
            eraser_override,
            mouse_was_down: false,
            pan_drag_anchor: None,
            middle_pan_anchor: None,
            layer_xform: None,
            pen_blocked: false,
            pen_using_eraser: false,
            pen_picking: false,
            show_layer_preview: false,
            ui_visible: true,
            move_picker_pending: false,
            confirm_delete: None,
            view_glide: None,
            peek: None,
            picture_colours: None,
            palette_before: None,
            commands: Default::default(),
            find: Default::default(),
            find_flash: None,
            find_start: None,
            find_paste_prev: false,
            find_reading: None,
            word_search: None,
            copies_job: None,
            likely_for: None,
            tag_edit: None,
            restyle_run: None,
            nudge_run: None,
            paste_chord_prev: false,
            mouse_gesture: false,
            media_ui: ui::media_controls::MediaUi::new(),
            pin_tool: Default::default(),
            slide_panel: Default::default(),
            pad: Default::default(),
            crop: Default::default(),
        }
    }
}
