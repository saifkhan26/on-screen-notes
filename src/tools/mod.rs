//! Drawing tools: pen, eraser, rectangle, ellipse, arrow.
//!
//! Each tool is a small struct that maintains its own per-drag state (e.g.
//! "in-progress stroke"). The interaction layer feeds tools high-level events
//! (`begin`, `update`, `end`); tools mutate the active canvas directly.
//!
//! Why not a `dyn Tool` trait object? Each tool's state shape is different,
//! and we only ever have *one* active tool at a time. A plain enum dispatch
//! with explicit variants is cleaner, faster, and easier to reason about
//! for a Rust beginner than dyn dispatch.

pub mod arrange;
pub mod arrow_tool;
pub mod eraser_tool;
pub mod lasso_tool;
pub mod pen_tool;
pub mod picture_colours;
pub mod select_tool;
pub mod shape_tool;

use crate::canvas::canvas::Canvas;
use crate::canvas::shape::{BorderStyle, Shape};
use crate::canvas::smoothing::SmoothingOptions;
use crate::canvas::stroke::{PenSample, Stroke, StrokeStyle};
use crate::config::AppConfig;
use serde::{Deserialize, Serialize};

/// Strip pressure variation from a sample (force to 1.0). Used by the
/// Fill tool — the outline of a fill should be uniform width so the
/// closed shape reads as one solid silhouette, not a tapered ribbon.
fn flatten_pressure(mut s: PenSample) -> PenSample {
    s.pressure = 1.0;
    s
}

/// User-selectable tools. The enum variant doubles as the *kind*; per-drag
/// state lives in `ActiveTool` below.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolKind {
    Pen,
    /// Eraser tool. Still in the enum so the Ctrl-held override
    /// (`app::eraser_override`) keeps working, but is no longer
    /// surfaced as a toolbar button — the user reaches it via Ctrl
    /// instead. Keeping the variant also lets older config files load
    /// without a migration if the user had Eraser selected.
    Eraser,
    Rect,
    Ellipse,
    /// Straight line shape: two-corner drag commits a `Shape::Line`.
    /// Distinct from the auto-ruler "straight-line snap" (which still
    /// emits a `Stroke`); the dedicated Line tool exists so the user
    /// can apply the animated-dashed border without hold-still timing.
    Line,
    Arrow,
    FreehandArrow,
    /// Closed-polygon fill. Behaves like Pen for input collection, but
    /// the renderer auto-joins the stroke's start ↔ end and fills the
    /// enclosed area. Pressure is ignored — the outline is uniform width.
    Fill,
    /// Laser pointer. Behaves like Pen during the stroke (live preview
    /// uses the same path), but the committed stroke is appended to
    /// `ActiveTool::laser_strokes` and fades over ~1 s after pen-up.
    /// Nothing is persisted to disk — laser ink is transient by design.
    Laser,
    /// Text label. Click anywhere on the canvas to drop a caret;
    /// printable keys append to a per-caret buffer, `Shift+Enter`
    /// inserts a newline, `Enter` commits the buffer as a
    /// `Shape::Text` on the active canvas. `Escape` cancels.
    Text,
    /// Spotlight tool: dims the whole overlay except for a square
    /// region tracking the cursor. Used for demos / teaching. The
    /// size of the bright region is `ActiveTool::spotlight_size`,
    /// adjustable via the scroll wheel.
    Spotlight,
    /// Lasso erase. The user drags a closed loop, or clicks a polygon
    /// corner by corner (see `lasso_tool`); once it closes, every
    /// stroke / shape on the active layer whose centroid lies inside
    /// is removed (with undo). More surgical than the point-radius
    /// eraser when cleaning up around dense ink.
    LassoErase,
    /// Lasso select. Same loop / polygon collection as `LassoErase`.
    /// Once the shape closes, the on-screen pixels
    /// *inside* it are captured and dropped onto the active layer as an
    /// image object, selected and ready to move. The capture itself runs
    /// in the app loop — `end` only stashes the polygon in
    /// `pending_lasso_capture`.
    LassoSelect,
    /// Object select. Click an object on the active layer to select it
    /// (Shift+click adds / removes, dragging empty space rubber-bands),
    /// then move, resize or rotate the selection through its handles.
    /// All state lives in `ActiveTool::select`.
    Select,
}

impl ToolKind {
    /// Single-letter shortcut keys, matching the table in the README.
    #[allow(dead_code)]
    pub fn shortcut(&self) -> &'static str {
        match self {
            ToolKind::Pen           => "P",
            ToolKind::Eraser        => "Ctrl (hold)",
            ToolKind::Rect          => "R",
            ToolKind::Ellipse       => "O",
            ToolKind::Line          => "—",
            ToolKind::Arrow         => "A",
            ToolKind::FreehandArrow => "Shift+A",
            ToolKind::Fill          => "F",
            ToolKind::Laser         => "L",
            ToolKind::Text          => "T",
            ToolKind::Spotlight     => "S",
            ToolKind::LassoErase    => "X",
            ToolKind::LassoSelect   => "Shift+X",
            ToolKind::Select        => "V",
        }
    }
}

/// One in-progress text-tool buffer. Lives on `ActiveTool` while the
/// user is typing and is taken on `Enter` to produce a committed
/// `Shape::Text`. `caret` is a byte offset into `buffer`; we keep it
/// even though the simple input dispatcher only ever appends and
/// backspaces, because the field leaves room for arrow-key cursor
/// movement without another struct change.
#[derive(Debug, Clone)]
pub struct TextEdit {
    /// Canvas-local position of the top-left of the first line.
    pub pos: [f32; 2],
    /// Text typed so far, including embedded `\n` for newlines.
    pub buffer: String,
    /// Byte offset of the caret within `buffer`. Equal to
    /// `buffer.len()` while the user is appending.
    pub caret: usize,
    /// Font size in canvas-local logical pixels. Captured from
    /// `ActiveTool::size` at begin so changing the slider mid-typing
    /// doesn't resize the live caret.
    pub font_size: f32,
    /// Colour captured at begin for the same reason.
    pub color: [u8; 4],
}

/// Holds the *current* tool plus any in-progress stroke/shape.
pub struct ActiveTool {
    pub kind: ToolKind,
    pub color: [u8; 4],
    pub size: f32,
    /// Active stroke style — currently only the Pen / FreehandArrow tools
    /// honour this. Eraser and shape tools ignore it.
    pub style: StrokeStyle,
    /// In-progress freehand stroke (Pen / Eraser / FreehandArrow / Laser).
    pub in_progress_stroke: Option<Stroke>,
    /// In-progress two-corner shape (Rect / Ellipse / Arrow). Stored as
    /// `(start_position, current_position)` so the renderer can preview.
    pub in_progress_shape: Option<([f32; 2], [f32; 2])>,
    /// Number of position-smoothing passes baked into the cache when a
    /// stroke commits — driven by the user-selected smoothing level
    /// (`config.smoothing_level.position_passes()`). The app keeps this
    /// in sync whenever the level changes.
    pub position_passes: usize,
    /// Committed laser-pointer strokes — one entry per pen-up gesture.
    /// Each holds the cached stroke plus the moment the pen lifted.
    /// The renderer paints them with a time-decaying alpha; the app
    /// loop prunes entries once their alpha hits zero. Not persisted.
    pub laser_strokes: Vec<(Stroke, std::time::Instant)>,
    /// Outline style applied to the *next* Rect / Ellipse the user
    /// commits. Toggled by the toolbar's contextual dashed-border
    /// button when one of those shape tools is active. Ignored for
    /// every other tool. Not persisted across runs — sensible default
    /// is `Solid`, and any previously-committed dashed shape carries
    /// its own `border` field independently.
    pub border_style: BorderStyle,
    /// Text labels go in a box (`Shape::Text::background`), in a colour
    /// that keeps them readable. The toolbar's toggle beside the Text
    /// tool; for the labels placed from now on.
    pub text_box: bool,
    /// Active text-tool caret buffer, if any. Created when the user
    /// activates the Text tool (and either clicks or presses `T`),
    /// mutated by keyboard input in `interaction::route_text_input`,
    /// consumed on `Enter` to produce a `Shape::Text`.
    pub in_progress_text: Option<TextEdit>,
    /// `true` when the user pressed the `I` eyedropper hotkey and the
    /// next click should sample a screen pixel into `color` instead
    /// of starting a stroke. One-shot; cleared on the next click,
    /// `Esc`, or tool switch.
    pub eyedropper_pending: bool,
    /// Pending flood-fill click position in canvas-local coords.
    /// Set by the Fill tool on pen-down; the app loop picks it up
    /// next frame, runs the flood with full DPI / viewport context,
    /// and commits the resulting raster sticker onto the active
    /// layer. Cleared once consumed.
    pub pending_fill_at: Option<[f32; 2]>,
    /// Half-side length of the spotlight square in screen-logical
    /// pixels. Only meaningful while `ToolKind::Spotlight` is active.
    /// Adjusted by the scroll wheel; reset to a sensible default when
    /// the user re-enters the tool.
    pub spotlight_size: f32,
    /// In-progress lasso path of the two lasso tools: a freehand loop
    /// while the pen is down, or Lasso capture's polygon, which stays
    /// open between presses until it is closed.
    pub lasso: lasso_tool::LassoPath,
    /// Pending lasso-select polygon (canvas-local coords). Set by the
    /// `LassoSelect` tool once the shape closes; the app loop picks it up next
    /// frame and runs the deferred screen capture (it needs the HWND,
    /// DPI, and the 2-frame chrome-hide that only the app has).
    /// Cleared once consumed.
    pub pending_lasso_capture: Option<Vec<[f32; 2]>>,
    /// Select-tool state: which objects are selected and the move /
    /// resize / rotate gesture in flight, if any. Kept even while
    /// another tool is active only so the struct has one shape — the
    /// app clears it every frame the Select tool is not the active one.
    pub select: select_tool::SelectState,

    /// Snapshot of the user's current smoothing options. Copied onto
    /// each new stroke at pen-down so a mid-stroke UI change does not
    /// retro-apply to ink already laid down. The app loop keeps this
    /// in sync with `config.smoothing`.
    pub smoothing: SmoothingOptions,
}

impl ActiveTool {
    pub fn from_config(cfg: &AppConfig) -> Self {
        Self {
            kind: ToolKind::Pen,
            color: cfg.default_pen_color,
            // Clamp to the same range the slider + scroll handler use so a
            // hand-edited config can't seed an unreasonable starting size.
            size: cfg.default_pen_size.clamp(1.0, 80.0),
            style: cfg.default_stroke_style,
            in_progress_stroke: None,
            in_progress_shape:  None,
            position_passes: cfg.smoothing_level.position_passes(),
            laser_strokes:   Vec::new(),
            border_style:    BorderStyle::Solid,
            text_box:        false,
            in_progress_text: None,
            eyedropper_pending: false,
            pending_fill_at: None,
            spotlight_size: 120.0,
            lasso: lasso_tool::LassoPath::default(),
            pending_lasso_capture: None,
            select: select_tool::SelectState::default(),
            smoothing: cfg.smoothing,
        }
    }

    /// Canvas units per screen pixel for what this tool draws or rubs
    /// out. The pen and the eraser work at their size as it looks on
    /// screen, whatever the zoom (`Stroke::px`); the freehand arrow and
    /// the laser keep their size in canvas units.
    fn px(&self, canvas: &Canvas) -> f32 {
        match self.kind {
            ToolKind::Pen | ToolKind::Eraser => 1.0 / canvas.zoom.max(1e-3),
            _ => 1.0,
        }
    }

    /// How far from its centre the eraser rubs out ink at `zoom`, in
    /// canvas units: `size` screen pixels.
    pub fn eraser_radius(&self, zoom: f32) -> f32 {
        self.size / zoom.max(1e-3)
    }

    /// Radius, in screen points, of the ring that previews the tool at
    /// the pointer: the widest line a pen-down would draw, or how far
    /// the eraser reaches.
    pub fn cursor_radius(&self, zoom: f32) -> f32 {
        match self.kind {
            ToolKind::Pen => (self.size * 0.5).max(2.0),
            ToolKind::Eraser => self.size.max(2.0),
            _ => (self.size * 0.5).max(2.0) * zoom,
        }
    }

    /// Pen-down at `sample`. Begins a stroke or shape depending on tool.
    /// The Select tool uses `canvas` to find out what the pen landed on
    /// (and to switch the active layer when that is on another one); the
    /// pen and eraser read its zoom.
    pub fn begin(&mut self, sample: PenSample, canvas: &mut Canvas) {
        match self.kind {
            ToolKind::Select => self.select.begin(sample.pos, canvas),
            ToolKind::Pen | ToolKind::FreehandArrow => {
                // Pen + freehand arrow honour the current stroke style
                // (Default / Pencil / Marker / Airbrush) AND the
                // current smoothing options so the user's choice locks
                // in at pen-down.
                // Every pen style draws at the size as it looks on
                // screen (`Stroke::px`), so zooming in to add detail
                // gives finer lines and brush strokes, not bigger ones.
                // Once drawn the width is fixed, so the line scales with
                // the view like everything else.
                let px = self.px(canvas);
                let mut s = Stroke::with_style_and_smoothing(
                    self.color, self.size * px, self.style, self.smoothing,
                );
                s.px = px;
                s.push(sample);
                self.in_progress_stroke = Some(s);
            }
            ToolKind::Fill => {
                // Flood-fill: arm a deferred flood action at the
                // click point. The actual rasterisation needs the
                // viewport size and DPI, which only the app loop
                // has — we just record the canvas-local position
                // here and let the app pick it up next frame.
                self.pending_fill_at = Some(sample.pos);
            }
            ToolKind::Eraser => {
                // Eraser keeps the plain ribbon look for its visible
                // trail — pencil styling would make the trail confusingly
                // faint. The trail is as wide as what the eraser reaches
                // (`eraser_radius` either side), so it shows what goes.
                let px = self.px(canvas);
                let mut s = Stroke::with_style_and_smoothing(
                    self.color, 2.0 * self.size * px, StrokeStyle::Default, self.smoothing,
                );
                s.px = px;
                s.push(sample);
                self.in_progress_stroke = Some(s);
            }
            ToolKind::Laser => {
                // Laser pointer: same collection model as Pen but the
                // outline always uses the smooth ribbon style — a
                // pencil-graphite look would be confusing for a
                // disappearing tool. Pressure is forced to 1.0 so the
                // line has uniform width (a real laser dot does not
                // taper).
                let mut s = Stroke::with_style_and_smoothing(
                    self.color, self.size, StrokeStyle::Default, self.smoothing,
                );
                s.push(flatten_pressure(sample));
                self.in_progress_stroke = Some(s);
            }
            ToolKind::Text => {
                // Caret placement happens on pen-up (see `end` below) so
                // we have canvas access for committing any prior buffer.
                // `begin` is a no-op for the Text tool.
            }
            ToolKind::Spotlight => {
                // Tap on canvas deactivates the spotlight: switch
                // back to the plain Pen tool without committing
                // any stroke. Lets the user "tap to dismiss" the
                // dim overlay.
                self.kind = ToolKind::Pen;
            }
            ToolKind::LassoErase | ToolKind::LassoSelect => {
                // Start a fresh lasso path, or the next corner of an open
                // polygon. The render layer paints the in-progress path
                // as a dashed grey loop. Erase and Select share the
                // collection + preview; they only differ in what `end`
                // does with the polygon.
                self.lasso.begin(sample.pos);
            }
            ToolKind::Rect | ToolKind::Ellipse | ToolKind::Line | ToolKind::Arrow => {
                self.in_progress_shape = Some((sample.pos, sample.pos));
            }
        }
    }

    /// Pen-move while down. Updates the in-progress stroke or shape.
    /// For the eraser, deletes any stroke/shape we touch *immediately*.
    pub fn update(&mut self, sample: PenSample, canvas: &mut Canvas) {
        match self.kind {
            ToolKind::Select => self.select.drag(sample.pos),
            ToolKind::Pen | ToolKind::FreehandArrow => {
                if let Some(s) = self.in_progress_stroke.as_mut() {
                    s.push(sample);
                }
            }
            ToolKind::Fill => {
                // Flood-fill is a single-click action — drag has
                // no meaning.
            }
            ToolKind::Laser => {
                if let Some(s) = self.in_progress_stroke.as_mut() {
                    s.push(flatten_pressure(sample));
                }
            }
            ToolKind::Text => {
                // Text drag-while-down has no meaning. The caret moves
                // only on a fresh click (handled in `end`).
            }
            ToolKind::Spotlight => {
                // Spotlight follows the cursor passively. Nothing to
                // do on pen-move.
            }
            ToolKind::LassoErase | ToolKind::LassoSelect => self.lasso.update(sample.pos),
            ToolKind::Eraser => {
                if let Some(s) = self.in_progress_stroke.as_mut() {
                    s.push(sample);
                }
                // Eraser radius scales with `size`. Real-time delete keeps
                // the UX responsive — user sees ink disappear under the tip.
                let r = self.eraser_radius(canvas.zoom);
                canvas.erase_at(sample.pos, r);
            }
            ToolKind::Rect | ToolKind::Ellipse | ToolKind::Line | ToolKind::Arrow => {
                if let Some((_, end)) = self.in_progress_shape.as_mut() {
                    *end = sample.pos;
                }
            }
        }
    }

    /// Pen-up. Commits the stroke or shape into `canvas`.
    pub fn end(&mut self, sample: PenSample, canvas: &mut Canvas) {
        match self.kind {
            ToolKind::Select => self.select.end(sample.pos, canvas),
            ToolKind::Pen => {
                if let Some(mut s) = self.in_progress_stroke.take() {
                    s.push(sample);
                    // Stabilizer mode keeps a rope-pull queue between
                    // cursor and committed ink; on pen-up we drain the
                    // queue so the trailing samples catch up to the
                    // cursor. No-op for every other mode.
                    s.finish();
                    // Pre-build the cache with the user's current
                    // smoothing budget; `Canvas::add_stroke` will
                    // happily skip its own default build because the
                    // cache is already populated.
                    s.build_cache_with(self.position_passes);
                    canvas.add_stroke(s);
                }
            }
            ToolKind::Fill => {
                // Flood-fill commit happens in the app loop once
                // the deferred `pending_fill_at` is consumed; no
                // stroke to flush here. Sample param unused.
                let _ = sample;
                let _ = canvas;
            }
            ToolKind::FreehandArrow => {
                if let Some(mut s) = self.in_progress_stroke.take() {
                    s.push(sample);
                    s.finish();
                    s.build_cache_with(self.position_passes);
                    canvas.add_stroke(s);
                    // Arrowhead width must match the rendered stroke
                    // width at the very tip — i.e. `base_width × tip
                    // pressure` — otherwise a light-pressure stroke
                    // gets a chunky out-of-proportion head and a heavy
                    // stroke gets a wimpy one. Floor at 30 % of the
                    // base width so a near-zero-pressure tip still
                    // gets a visible arrowhead.
                    let tip_pressure = canvas
                        .layers
                        .get(canvas.active_layer)
                        .and_then(|l| l.strokes.last())
                        .and_then(|s| s.samples.last())
                        .map(|s| s.pressure)
                        .unwrap_or(1.0)
                        .clamp(0.3, 1.0);
                    let head_width = self.size * tip_pressure;
                    if let Some(arrow) = arrow_tool::arrowhead_for_freehand_end(canvas, self.color, head_width) {
                        canvas.add_shape(arrow);
                    }
                }
            }
            ToolKind::Eraser => {
                self.in_progress_stroke = None;
            }
            ToolKind::Laser => {
                if let Some(mut s) = self.in_progress_stroke.take() {
                    s.push(flatten_pressure(sample));
                    s.finish();
                    // Build the cache so the render path can reuse the
                    // same fast cached-polyline route Pen strokes use.
                    s.build_cache_with(self.position_passes);
                    self.laser_strokes.push((s, std::time::Instant::now()));
                }
            }
            ToolKind::Text => {
                // Pen-up = the click is complete. Commit any prior
                // buffered text into the canvas (so the user does not
                // lose what they typed before clicking elsewhere) and
                // open a fresh caret at the click point.
                if let Some(prev) = self.in_progress_text.take() {
                    if !prev.buffer.is_empty() {
                        canvas.add_shape(Shape::Text {
                            pos: prev.pos,
                            content: prev.buffer,
                            font_size: prev.font_size,
                            color: prev.color,
                            rotation: 0.0,
                            pin: 0,
                            background: self.text_box.then(|| crate::canvas::shape::auto_text_box(prev.color)),
                        });
                    }
                }
                // Font size scales with the toolbar's "pen size" slider
                // (× 3 so a 6-px pen ≈ 18-px text). Captured at click
                // time so the user can keep editing while the slider
                // moves without resizing what they already typed.
                let font_size = (self.size.max(6.0) * 3.0).clamp(12.0, 240.0);
                self.in_progress_text = Some(TextEdit {
                    pos: sample.pos,
                    buffer: String::new(),
                    caret: 0,
                    font_size,
                    color: self.color,
                });
            }
            ToolKind::Spotlight => {
                // Pen events do nothing for the spotlight — it is a
                // passive display tool. The render layer follows the
                // pointer each frame.
            }
            ToolKind::LassoErase | ToolKind::LassoSelect => {
                // A click only lays a corner, so the shape may not be
                // closed yet.
                if let Some(poly) = self.lasso.end(sample.pos, canvas.zoom) {
                    self.use_lasso(poly, canvas);
                }
            }
            ToolKind::Rect => {
                if let Some((a, _)) = self.in_progress_shape.take() {
                    canvas.add_shape(Shape::Rect {
                        a,
                        b: sample.pos,
                        color: self.color,
                        stroke_width: self.size,
                        border: self.border_style,
                        rotation: 0.0,
                        pin: 0,
                    });
                }
            }
            ToolKind::Ellipse => {
                if let Some((a, _)) = self.in_progress_shape.take() {
                    canvas.add_shape(Shape::Ellipse {
                        a,
                        b: sample.pos,
                        color: self.color,
                        stroke_width: self.size,
                        border: self.border_style,
                        rotation: 0.0,
                        pin: 0,
                    });
                }
            }
            ToolKind::Line => {
                if let Some((a, _)) = self.in_progress_shape.take() {
                    canvas.add_shape(Shape::Line {
                        a,
                        b: sample.pos,
                        color: self.color,
                        stroke_width: self.size,
                        border: self.border_style,
                        pin: 0,
                    });
                }
            }
            ToolKind::Arrow => {
                if let Some((a, _)) = self.in_progress_shape.take() {
                    canvas.add_shape(Shape::Arrow {
                        a,
                        b: sample.pos,
                        color: self.color,
                        stroke_width: self.size,
                        border: self.border_style,
                        pin: 0,
                    });
                }
            }
        }
    }

    /// Enter: close an open lasso polygon and use it, as pen-up does
    /// with a shape that closes.
    pub fn finish_lasso(&mut self, canvas: &mut Canvas) {
        if let Some(poly) = self.lasso.finish() {
            self.use_lasso(poly, canvas);
        }
    }

    /// Act on a closed lasso shape. Erase cuts the ink inside it; capture
    /// hands it to the app loop instead, which grabs the screen pixels
    /// inside and adds them to the active layer as an image — the canvas
    /// is untouched here.
    fn use_lasso(&mut self, poly: Vec<[f32; 2]>, canvas: &mut Canvas) {
        match self.kind {
            ToolKind::LassoErase => {
                canvas.erase_in_polygon(&poly);
            }
            ToolKind::LassoSelect => self.pending_lasso_capture = Some(poly),
            _ => {}
        }
    }

    /// Cancel an in-progress drag (e.g. user pressed Escape). A lasso
    /// polygon only loses the press in flight, not its earlier corners.
    #[allow(dead_code)]
    pub fn cancel(&mut self) {
        self.in_progress_stroke = None;
        self.in_progress_shape  = None;
        self.lasso.cancel_press();
        self.select.gesture     = None;
    }

    /// Build a synthetic preview `Shape` for the in-progress two-corner drag,
    /// so the renderer can show what the user is about to commit.
    pub fn preview_shape(&self) -> Option<Shape> {
        let (a, b) = self.in_progress_shape?;
        Some(match self.kind {
            ToolKind::Rect    => Shape::Rect    { a, b, color: self.color, stroke_width: self.size, border: self.border_style, rotation: 0.0, pin: 0 },
            ToolKind::Ellipse => Shape::Ellipse { a, b, color: self.color, stroke_width: self.size, border: self.border_style, rotation: 0.0, pin: 0 },
            ToolKind::Line    => Shape::Line    { a, b, color: self.color, stroke_width: self.size, border: self.border_style, pin: 0 },
            ToolKind::Arrow   => Shape::Arrow   { a, b, color: self.color, stroke_width: self.size, border: self.border_style, pin: 0 },
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn width_drawn(kind: ToolKind, style: StrokeStyle, zoom: f32) -> f32 {
        let mut tool = ActiveTool::from_config(&crate::config::AppConfig::default());
        tool.kind = kind;
        tool.style = style;
        tool.size = 8.0;
        let mut canvas = Canvas { zoom, ..Canvas::default() };
        tool.begin(PenSample::from_pos([10.0, 10.0]), &mut canvas);
        tool.in_progress_stroke.expect("a stroke started").base_width
    }

    #[test]
    fn every_pen_style_draws_the_size_seen_on_screen() {
        assert_eq!(width_drawn(ToolKind::Pen, StrokeStyle::Default, 1.0), 8.0);
        assert_eq!(width_drawn(ToolKind::Pen, StrokeStyle::Default, 4.0), 2.0, "zoomed in, finer on the canvas");
        assert_eq!(width_drawn(ToolKind::Pen, StrokeStyle::Pencil, 0.5), 16.0, "zoomed out, wider on the canvas");
        assert_eq!(width_drawn(ToolKind::Pen, StrokeStyle::Marker, 4.0), 2.0, "the marker too");
        assert_eq!(width_drawn(ToolKind::Pen, StrokeStyle::Airbrush, 0.25), 32.0, "and the airbrush");
        assert_eq!(width_drawn(ToolKind::FreehandArrow, StrokeStyle::Default, 4.0), 8.0);
    }

    /// Zoomed in 4×, an 8 px eraser reaches 2 canvas units: what it
    /// covers on screen. The ring at the pointer is that size too, and
    /// the pen's ring is the line it would draw, at any zoom.
    #[test]
    fn the_eraser_and_the_rings_keep_their_size_on_screen() {
        let mut tool = ActiveTool::from_config(&crate::config::AppConfig::default());
        tool.size = 2.0;
        let mut canvas = Canvas { zoom: 4.0, ..Canvas::default() };
        for y in [0.0, 20.0] {
            tool.begin(PenSample::from_pos([0.0, y]), &mut canvas);
            for i in 1..10 {
                tool.update(PenSample::from_pos([i as f32 * 10.0, y]), &mut canvas);
            }
            tool.end(PenSample::from_pos([100.0, y]), &mut canvas);
        }
        let strokes = |c: &Canvas| c.layers[c.active_layer].strokes.len();
        assert_eq!(strokes(&canvas), 2);

        tool.kind = ToolKind::Eraser;
        tool.size = 8.0;
        assert_eq!(tool.eraser_radius(canvas.zoom), 2.0);
        tool.begin(PenSample::from_pos([50.0, 1.5]), &mut canvas);
        tool.update(PenSample::from_pos([50.0, 1.5]), &mut canvas);
        assert_eq!(strokes(&canvas), 1, "6 px from the first line: within reach");
        tool.update(PenSample::from_pos([50.0, 17.0]), &mut canvas);
        tool.end(PenSample::from_pos([50.0, 17.0]), &mut canvas);
        assert_eq!(strokes(&canvas), 1, "12 px from the second line: out of reach");

        assert_eq!(tool.cursor_radius(4.0), 8.0, "the eraser ring shows its reach");
        tool.kind = ToolKind::Pen;
        assert_eq!(tool.cursor_radius(4.0), 4.0, "an 8 px line, whatever the zoom");
        assert_eq!(tool.cursor_radius(0.5), 4.0);
        tool.kind = ToolKind::FreehandArrow;
        assert_eq!(tool.cursor_radius(4.0), 16.0, "the arrow keeps canvas units");
    }

    /// A brush stroke drawn zoomed in remembers the pixel size, so its
    /// thinnest mark and stamp spacing are screen pixels too.
    #[test]
    fn a_stroke_remembers_the_pixel_size_it_was_drawn_at() {
        let mut tool = ActiveTool::from_config(&crate::config::AppConfig::default());
        tool.style = StrokeStyle::Airbrush;
        let mut canvas = Canvas { zoom: 4.0, ..Canvas::default() };
        tool.begin(PenSample::from_pos([10.0, 10.0]), &mut canvas);
        assert_eq!(tool.in_progress_stroke.as_ref().unwrap().px, 0.25);
        tool.kind = ToolKind::FreehandArrow;
        tool.begin(PenSample::from_pos([10.0, 10.0]), &mut canvas);
        assert_eq!(tool.in_progress_stroke.as_ref().unwrap().px, 1.0, "the arrow keeps canvas units");
    }

    #[test]
    fn lasso_capture_clicks_a_polygon_and_enter_closes_it() {
        let mut tool = ActiveTool::from_config(&crate::config::AppConfig::default());
        tool.kind = ToolKind::LassoSelect;
        let mut canvas = Canvas { zoom: 2.0, ..Canvas::default() };
        let corners = [[10.0, 10.0], [60.0, 10.0], [60.0, 40.0], [10.0, 40.0]];
        for p in corners {
            tool.begin(PenSample::from_pos(p), &mut canvas);
            tool.end(PenSample::from_pos([p[0] + 0.5, p[1]]), &mut canvas);
            assert!(tool.pending_lasso_capture.is_none(), "a click only lays a corner");
        }
        crate::interaction::react_lasso_keys(egui::Key::Backspace, egui::Modifiers::NONE, &mut tool, &mut canvas);
        crate::interaction::react_lasso_keys(egui::Key::Enter, egui::Modifiers::NONE, &mut tool, &mut canvas);
        assert_eq!(tool.pending_lasso_capture.as_deref(), Some(&corners[..3]));
        assert!(tool.lasso.points().is_empty());
    }

    #[test]
    fn lasso_erase_clicks_a_polygon_too() {
        let mut tool = ActiveTool::from_config(&crate::config::AppConfig::default());
        tool.kind = ToolKind::LassoErase;
        let mut canvas = Canvas::default();
        let dot = |x: f32, y: f32| {
            let mut s = Stroke::with_style_and_smoothing([0, 0, 0, 255], 4.0, StrokeStyle::Default, SmoothingOptions::default());
            s.push(PenSample::from_pos([x, y]));
            s.push(PenSample::from_pos([x + 2.0, y]));
            s
        };
        canvas.add_stroke(dot(30.0, 30.0));
        canvas.add_stroke(dot(200.0, 200.0));
        let strokes = |c: &Canvas| c.layers[c.active_layer].strokes.len();
        for p in [[0.0, 0.0], [100.0, 0.0], [100.0, 100.0], [0.0, 100.0], [1.0, 1.0]] {
            tool.begin(PenSample::from_pos(p), &mut canvas);
            assert_eq!(strokes(&canvas), 2, "nothing goes until the shape closes");
            tool.end(PenSample::from_pos(p), &mut canvas);
        }
        assert_eq!(strokes(&canvas), 1, "the click on the first corner erased the ink inside");
        assert!(tool.lasso.points().is_empty());
    }
}
