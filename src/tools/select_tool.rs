//! Select tool: pick objects on the active layer, then move, resize or
//! rotate them through a frame with handles.
//!
//! Nothing here writes to the canvas while a gesture is in flight. The
//! gesture only produces an [`Affine`]; the renderer previews the
//! selected objects through it, and `end` bakes it in with one
//! `Canvas::transform_objects` call (one undo step). That is the same
//! preview-then-commit split the Shift+Space layer gesture uses.
//!
//! Coordinates are canvas-local throughout. Sizes that should look the
//! same at any zoom (handle size, click slop) are given in screen pixels
//! and divided by the canvas zoom where they are used.

use crate::canvas::affine::Affine;
use crate::canvas::canvas::{shape_bounds, Canvas, Layer};
use crate::canvas::hit::{self, CanvasHit, ObjRef, Selection};
use crate::canvas::shape::{Obb, Shape};

/// How close (screen px) a click must land to an object to pick it.
const CLICK_SLOP_PX: f32 = 6.0;
/// Pointer travel (screen px) before a press on an object turns into a
/// move. Keeps a tap with a slightly wobbly pen from nudging things.
const DRAG_SLOP_PX: f32 = 4.0;
/// Grab radius (screen px) around a handle.
const HANDLE_GRAB_PX: f32 = 11.0;
/// Smallest width / height (screen px) of the selection frame. Twice
/// the grab radius on each side of the centre, so an object's middle is
/// always clear of every handle.
const MIN_FRAME_PX: f32 = 44.0;
/// Distance (screen px) of the rotate knob above the frame's top edge.
pub const ROTATE_STEM_PX: f32 = 28.0;
/// Smallest factor a resize may shrink to. Stops a drag through the
/// anchor from collapsing or mirroring the selection.
const MIN_SCALE: f32 = 0.02;
/// Rotation snap while Shift is held.
const ROTATE_SNAP: f32 = std::f32::consts::PI / 12.0;

/// One of the grab points on the selection frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handle {
    /// Resize handle at `(x, y)` on the frame, each of −1, 0 or 1 in the
    /// frame's own axes. Both non-zero is a corner (proportional
    /// resize); one zero is a side (stretch along one axis).
    Scale { x: i8, y: i8 },
    Rotate,
}

/// What the pointer is doing between pen-down and pen-up.
#[derive(Debug, Clone, Copy)]
pub enum Gesture {
    /// Rubber-band box. `click_target` is an object the press landed on
    /// that only gets selected if the pointer never travels — see
    /// `SelectState::begin` for which objects are treated that way.
    Marquee { start: [f32; 2], cur: [f32; 2], click_target: Option<CanvasHit> },
    Move { start: [f32; 2], cur: [f32; 2] },
    /// `frame` is the selection frame as it stood at pen-down; the
    /// resize is measured against it for the whole gesture.
    Resize { x: i8, y: i8, frame: Obb, cur: [f32; 2] },
    Rotate { frame: Obb, start: [f32; 2], cur: [f32; 2] },
}

/// What is under the pointer, for the cursor hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hover {
    Nothing,
    Handle(Handle),
    /// Inside the selection frame — a drag here moves the selection.
    Selection,
    /// Over an unselected object.
    Object,
}

/// Everything the overlay needs to draw the selection chrome, in
/// canvas-local coordinates.
pub struct Chrome {
    /// Frame corners: top-left, top-right, bottom-right, bottom-left.
    pub frame: Option<[[f32; 2]; 4]>,
    pub handles: Vec<([f32; 2], Handle)>,
    /// `(top-edge midpoint, knob centre)` of the rotate handle.
    pub rotate_stem: Option<([f32; 2], [f32; 2])>,
    /// Rubber-band box `[x0, y0, x1, y1]`.
    pub marquee: Option<[f32; 4]>,
}

#[derive(Default)]
pub struct SelectState {
    pub sel: Selection,
    pub gesture: Option<Gesture>,
    /// Shift state for the current frame, pushed in by the app: adds to
    /// / removes from the selection on click, snaps rotation.
    pub shift: bool,
    /// Which canvas / layer / object counts `sel` was made against. Its
    /// indices mean nothing once any of these change.
    bound: (usize, usize, usize, usize),
}

impl SelectState {
    /// Called once per frame before input is dispatched. Drops the
    /// selection when the Select tool is not active, or when the canvas,
    /// the active layer or the number of objects on it changed since the
    /// selection was made (another tool drew, an undo removed something,
    /// the user switched slide…) — its indices would point at the wrong
    /// objects.
    pub fn sync(&mut self, tool_active: bool, canvas_idx: usize, canvas: &Canvas) {
        let now = Self::binding(canvas_idx, canvas);
        if !tool_active || now != self.bound {
            self.sel.clear();
            self.gesture = None;
        }
        self.bound = now;
    }

    /// Accept the canvas as it is now as the state `sel` refers to. Call
    /// after an edit that changed the object counts *and* fixed up `sel`
    /// to match (delete, duplicate, paste, capture).
    pub fn rebind(&mut self, canvas_idx: usize, canvas: &Canvas) {
        self.bound = Self::binding(canvas_idx, canvas);
    }

    /// `rebind` for changes made from inside this tool, which is only
    /// ever handed the canvas the last `sync` was for.
    fn rebind_here(&mut self, canvas: &Canvas) {
        self.bound = Self::binding(self.bound.0, canvas);
    }

    fn binding(canvas_idx: usize, canvas: &Canvas) -> (usize, usize, usize, usize) {
        let li = canvas.active_layer;
        let (n_strokes, n_shapes) = canvas
            .layers
            .get(li)
            .map_or((0, 0), |l| (l.strokes.len(), l.shapes.len()));
        (canvas_idx, li, n_strokes, n_shapes)
    }

    /// Cancel an in-flight gesture, or — if there is none — drop the
    /// selection. One `Esc` backs out one level.
    pub fn cancel(&mut self) {
        if self.gesture.take().is_none() {
            self.sel.clear();
        }
    }

    // ---- frame -----------------------------------------------------------

    /// The box the handles sit on. A lone rectangle, ellipse, image or
    /// text label keeps its own orientation, so its frame turns with
    /// it. Anything else (strokes, lines, several objects) gets the
    /// upright box around everything selected.
    pub fn frame(&self, layer: &Layer, zoom: f32) -> Option<Obb> {
        if self.sel.is_empty() { return None; }
        let mut frame = match (self.sel.strokes.as_slice(), self.sel.shapes.as_slice()) {
            ([], [i]) => match layer.shapes.get(*i)? {
                s @ (Shape::Rect { stroke_width, .. } | Shape::Ellipse { stroke_width, .. }) => {
                    let mut o = s.obb();
                    o.half[0] += stroke_width * 0.5;
                    o.half[1] += stroke_width * 0.5;
                    o
                }
                s @ (Shape::Raster { .. } | Shape::Text { .. }) => s.obb(),
                s => aabb_frame(shape_bounds(s)),
            },
            _ => {
                let mut bb: Option<[f32; 4]> = None;
                let mut grow = |b: [f32; 4]| {
                    bb = Some(match bb {
                        None => b,
                        Some(a) => [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])],
                    });
                };
                for &i in &self.sel.strokes {
                    if let Some(b) = layer.strokes.get(i).and_then(|s| s.bounds()) { grow(b); }
                }
                for &i in &self.sel.shapes {
                    if let Some(s) = layer.shapes.get(i) { grow(shape_bounds(s)); }
                }
                aabb_frame(bb?)
            }
        };
        // Never let the frame get smaller than `MIN_FRAME_PX` on screen.
        // Around a thin stroke or a dot the handles would otherwise sit
        // right on the ink, and pressing the object to drag it would
        // grab a resize handle instead. (It also keeps the extent away
        // from zero, which resizing divides by.)
        let min_half = MIN_FRAME_PX * 0.5 / zoom.max(1e-3);
        frame.half[0] = frame.half[0].max(min_half);
        frame.half[1] = frame.half[1].max(min_half);
        Some(frame)
    }

    /// Whether the side handles (one-axis stretch) are offered. They are
    /// only shown where the stretch can be stored exactly: a single
    /// shape stretches along its own sides, and strokes / lines / arrows
    /// / unrotated boxes stretch along the canvas axes. Text has one
    /// font size for both axes, and a box turned relative to the frame
    /// would have to shear, so those selections resize from the corners
    /// only.
    fn side_handles(&self, layer: &Layer) -> bool {
        let shape = |i: &usize| layer.shapes.get(*i);
        if self.sel.strokes.is_empty() && self.sel.shapes.len() == 1 {
            return !matches!(shape(&self.sel.shapes[0]), Some(Shape::Text { .. }) | None);
        }
        self.sel.shapes.iter().all(|i| match shape(i) {
            Some(Shape::Rect { rotation, .. })
            | Some(Shape::Ellipse { rotation, .. })
            | Some(Shape::Raster { rotation, .. }) => *rotation == 0.0,
            Some(Shape::Line { .. }) | Some(Shape::Arrow { .. }) => true,
            Some(Shape::Text { .. }) | None => false,
        })
    }

    // ---- gesture → transform ----------------------------------------------

    /// The transform the in-flight gesture would apply, or `None` when
    /// there is no gesture, it is a marquee, or it has not moved far
    /// enough to count.
    pub fn preview_affine(&self, zoom: f32) -> Option<Affine> {
        let zoom = zoom.max(1e-3);
        let m = match self.gesture? {
            Gesture::Marquee { .. } => return None,
            Gesture::Move { start, cur } => {
                let d = [cur[0] - start[0], cur[1] - start[1]];
                if (d[0] * d[0] + d[1] * d[1]).sqrt() < DRAG_SLOP_PX / zoom { return None; }
                Affine::translate(d)
            }
            Gesture::Rotate { frame, start, cur } => {
                let c = frame.center;
                let angle_to = |p: [f32; 2]| (p[1] - c[1]).atan2(p[0] - c[0]);
                let mut angle = angle_to(cur) - angle_to(start);
                if self.shift {
                    // Snap the *resulting* orientation, so a frame that
                    // is already turned lands on 0°, 15°, 30°… rather
                    // than on its old angle plus a multiple of 15°.
                    let target = ((frame.angle + angle) / ROTATE_SNAP).round() * ROTATE_SNAP;
                    angle = target - frame.angle;
                }
                Affine::rotate_about(c, angle)
            }
            Gesture::Resize { x, y, frame, cur } => {
                let [hw, hh] = frame.half;
                let q = frame.to_local(cur);
                // The handle opposite the one being dragged stays put.
                let anchor = [-(x as f32) * hw, -(y as f32) * hh];
                let (sx, sy) = if x != 0 && y != 0 {
                    // Corner: project the pointer onto the frame's
                    // diagonal so the resize stays proportional however
                    // the pointer wanders off it.
                    let diag = [2.0 * x as f32 * hw, 2.0 * y as f32 * hh];
                    let s = ((q[0] - anchor[0]) * diag[0] + (q[1] - anchor[1]) * diag[1])
                        / (diag[0] * diag[0] + diag[1] * diag[1]);
                    let s = s.max(MIN_SCALE);
                    (s, s)
                } else if x != 0 {
                    (((q[0] - anchor[0]) / (2.0 * x as f32 * hw)).max(MIN_SCALE), 1.0)
                } else {
                    (1.0, ((q[1] - anchor[1]) / (2.0 * y as f32 * hh)).max(MIN_SCALE))
                };
                // Into the frame's own axes, scale about the anchor, and
                // back out.
                Affine::translate([-frame.center[0], -frame.center[1]])
                    .then(Affine::rotate_about([0.0, 0.0], -frame.angle))
                    .then(Affine::scale_about(anchor, sx, sy))
                    .then(Affine::rotate_about([0.0, 0.0], frame.angle))
                    .then(Affine::translate(frame.center))
            }
        };
        (!m.is_identity()).then_some(m)
    }

    // ---- chrome + hit-testing of the chrome --------------------------------

    /// Frame, handles and marquee as they should be drawn right now —
    /// i.e. with the in-flight gesture applied.
    pub fn chrome(&self, canvas: &Canvas) -> Chrome {
        let mut out = Chrome { frame: None, handles: Vec::new(), rotate_stem: None, marquee: None };
        if let Some(Gesture::Marquee { start, cur, .. }) = self.gesture {
            out.marquee = Some([
                start[0].min(cur[0]), start[1].min(cur[1]),
                start[0].max(cur[0]), start[1].max(cur[1]),
            ]);
        }
        let Some(layer) = canvas.layers.get(canvas.active_layer) else { return out };
        let zoom = canvas.zoom.max(1e-3);
        // A resize or rotate is measured against the frame it started
        // from; a move just carries the current frame along.
        let base = match self.gesture {
            Some(Gesture::Resize { frame, .. }) | Some(Gesture::Rotate { frame, .. }) => Some(frame),
            _ => self.frame(layer, zoom),
        };
        let Some(base) = base else { return out };
        let m = self.preview_affine(zoom).unwrap_or(Affine::IDENTITY);
        let c = base.corners().map(|p| m.apply(p));
        out.frame = Some(c);

        let mid = |a: [f32; 2], b: [f32; 2]| [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5];
        let corners = [(-1, -1), (1, -1), (1, 1), (-1, 1)];
        for (p, (x, y)) in c.iter().zip(corners) {
            out.handles.push((*p, Handle::Scale { x, y }));
        }
        if self.side_handles(layer) {
            let sides = [(0, -1), (1, 0), (0, 1), (-1, 0)];
            for (k, (x, y)) in sides.into_iter().enumerate() {
                out.handles.push((mid(c[k], c[(k + 1) % 4]), Handle::Scale { x, y }));
            }
        }
        // Rotate knob: straight out from the middle of the top edge.
        let top = mid(c[0], c[1]);
        let center = mid(c[0], c[2]);
        let out_dir = [top[0] - center[0], top[1] - center[1]];
        let len = (out_dir[0] * out_dir[0] + out_dir[1] * out_dir[1]).sqrt().max(1e-6);
        let stem = ROTATE_STEM_PX / zoom;
        let knob = [top[0] + out_dir[0] / len * stem, top[1] + out_dir[1] / len * stem];
        out.rotate_stem = Some((top, knob));
        out.handles.push((knob, Handle::Rotate));
        out
    }

    /// Handle under `p`, if any. The rotate knob is checked first, then
    /// corners, then sides, so the more specific grab wins where they
    /// crowd together on a small selection.
    fn handle_at(&self, p: [f32; 2], canvas: &Canvas) -> Option<Handle> {
        let reach = HANDLE_GRAB_PX / canvas.zoom.max(1e-3);
        let rank = |h: &Handle| match h {
            Handle::Rotate => 0,
            Handle::Scale { x, y } if *x != 0 && *y != 0 => 1,
            Handle::Scale { .. } => 2,
        };
        let mut handles = self.chrome(canvas).handles;
        handles.sort_by_key(|(_, h)| rank(h));
        handles
            .into_iter()
            .find(|(pos, _)| {
                let d = [p[0] - pos[0], p[1] - pos[1]];
                d[0] * d[0] + d[1] * d[1] <= reach * reach
            })
            .map(|(_, h)| h)
    }

    fn inside_frame(&self, p: [f32; 2], layer: &Layer, zoom: f32) -> bool {
        self.frame(layer, zoom).is_some_and(|f| {
            let q = f.to_local(p);
            q[0].abs() <= f.half[0] && q[1].abs() <= f.half[1]
        })
    }

    /// On-screen direction (radians, y down) along which resize handle
    /// `(x, y)` pulls, accounting for a turned frame. Lets the app pick
    /// a resize cursor that points the way the handle actually moves.
    pub fn handle_direction(&self, x: i8, y: i8, canvas: &Canvas) -> f32 {
        let frame = match self.gesture {
            Some(Gesture::Resize { frame, .. }) => Some(frame),
            _ => canvas.layers.get(canvas.active_layer).and_then(|l| self.frame(l, canvas.zoom)),
        };
        let Some(f) = frame else { return 0.0 };
        (y as f32 * f.half[1]).atan2(x as f32 * f.half[0]) + f.angle
    }

    /// What a press at `p` would grab — for the cursor hint. Mirrors the
    /// order `begin` decides in. Runs every frame, so it never decodes
    /// an image to check transparency; until an image has been clicked
    /// once its whole box counts.
    pub fn hover(&self, p: [f32; 2], canvas: &Canvas) -> Hover {
        let active = canvas.active_layer;
        let Some(layer) = canvas.layers.get(active) else { return Hover::Nothing };
        if let Some(h) = self.handle_at(p, canvas) {
            return Hover::Handle(h);
        }
        let tol = CLICK_SLOP_PX / canvas.zoom.max(1e-3);
        let picked = hit::pick(canvas, p, tol, false);
        if let Some(CanvasHit::Object(li, obj)) = picked {
            if li == active && self.sel.contains(obj) { return Hover::Selection; }
            if li == active && !is_raster(canvas, li, obj) { return Hover::Object; }
        }
        if !self.shift && self.inside_frame(p, layer, canvas.zoom) { return Hover::Selection; }
        if picked.is_some() { Hover::Object } else { Hover::Nothing }
    }

    // ---- pen events --------------------------------------------------------

    /// Pen-down. Picks through every visible layer, not just the active
    /// one: whatever is topmost under the pointer wins, and selecting
    /// something on another layer makes that layer the active one (a
    /// selection lives on a single layer). The canvas is only written
    /// to here to make that switch; no object is touched until `end`.
    pub fn begin(&mut self, p: [f32; 2], canvas: &mut Canvas) {
        self.gesture = None;
        let active = canvas.active_layer;
        let Some(layer) = canvas.layers.get(active) else { return };

        // 1) A handle of the current selection.
        if let (Some(h), Some(frame)) = (self.handle_at(p, canvas), self.frame(layer, canvas.zoom)) {
            self.gesture = Some(match h {
                Handle::Rotate => Gesture::Rotate { frame, start: p, cur: p },
                Handle::Scale { x, y } => Gesture::Resize { x, y, frame, cur: p },
            });
            return;
        }

        let tol = CLICK_SLOP_PX / canvas.zoom.max(1e-3);
        let picked = hit::pick(canvas, p, tol, true);

        // 2) Ink on the active layer, or anything already selected.
        if let Some(CanvasHit::Object(li, obj)) = picked {
            if li == active && (self.sel.contains(obj) || !is_raster(canvas, li, obj)) {
                if self.shift {
                    // Shift+click only edits membership; a drag that
                    // follows moves whatever is selected afterwards.
                    self.sel.toggle(obj);
                } else if !self.sel.contains(obj) {
                    self.sel.set_only(obj);
                }
                if self.sel.contains(obj) {
                    self.gesture = Some(Gesture::Move { start: p, cur: p });
                }
                return;
            }
        }

        // 3) Inside the current frame: move the selection. This comes
        //    before anything on other layers and before any picture,
        //    because a slide with a screenshot behind it has "something"
        //    under nearly every point — without this a selection could
        //    never be dragged by the empty space inside its frame.
        if !self.shift && self.inside_frame(p, &canvas.layers[active], canvas.zoom) {
            self.gesture = Some(Gesture::Move { start: p, cur: p });
            return;
        }

        match picked {
            // 4) A picture on the active layer (image, video, GIF, fill):
            //    take it and move it in one go, like ink. Only after the
            //    frame, so a selection over a picture can still be
            //    dragged by its empty middle.
            Some(CanvasHit::Object(li, obj)) if li == active => {
                if self.shift { self.sel.toggle(obj); } else { self.sel.set_only(obj); }
                if self.sel.contains(obj) {
                    self.gesture = Some(Gesture::Move { start: p, cur: p });
                }
            }
            // 5) Ink on another layer: go to that layer and take it.
            Some(CanvasHit::Object(li, obj)) if !is_raster(canvas, li, obj) => {
                canvas.active_layer = li;
                self.sel.set_only(obj);
                self.rebind_here(canvas);
                self.gesture = Some(Gesture::Move { start: p, cur: p });
            }
            // 6) A picture on another layer, or a layer's background
            //    plate. A screen capture can sit under most of the
            //    slide, below the layer being drawn on; if pressing on
            //    one grabbed it straight away, every attempt to
            //    rubber-band the ink drawn over it would drag the
            //    picture instead. So such a picture is only picked by a
            //    click that does not travel (resolved in `end`); a drag
            //    from it draws the marquee.
            Some(target) => {
                if !self.shift { self.sel.clear(); }
                self.gesture = Some(Gesture::Marquee { start: p, cur: p, click_target: Some(target) });
            }
            // 7) Nothing at all.
            None => {
                if !self.shift { self.sel.clear(); }
                self.gesture = Some(Gesture::Marquee { start: p, cur: p, click_target: None });
            }
        }
    }

    /// A click (press and release without travel) landed on `target`.
    fn click_select(&mut self, target: CanvasHit, canvas: &mut Canvas) {
        let (li, obj) = match target {
            CanvasHit::Object(li, obj) => (li, obj),
            CanvasHit::Plate(li) => {
                // An old capture / frozen frame stored as the layer's
                // background: make it an object so it can be selected
                // like any other image.
                let Some(i) = canvas.promote_plate(li) else { return };
                if li == canvas.active_layer {
                    // It went in at the front of the shape list, so the
                    // shapes already selected there each moved up one.
                    for s in &mut self.sel.shapes { *s += 1; }
                }
                (li, ObjRef::Shape(i))
            }
        };
        if li != canvas.active_layer {
            canvas.active_layer = li;
            self.sel.clear();
        }
        if self.shift { self.sel.toggle(obj); } else { self.sel.set_only(obj); }
        self.rebind_here(canvas);
    }

    pub fn drag(&mut self, p: [f32; 2]) {
        match self.gesture.as_mut() {
            Some(Gesture::Marquee { cur, .. })
            | Some(Gesture::Move { cur, .. })
            | Some(Gesture::Resize { cur, .. })
            | Some(Gesture::Rotate { cur, .. }) => *cur = p,
            None => {}
        }
    }

    pub fn end(&mut self, p: [f32; 2], canvas: &mut Canvas) {
        self.drag(p);
        let li = canvas.active_layer;
        let zoom = canvas.zoom.max(1e-3);
        // Taken, not just read: the marquee arm below needs `&mut self`.
        match self.gesture.take() {
            Some(Gesture::Marquee { start, cur, click_target }) => {
                let travelled = ((cur[0] - start[0]).powi(2) + (cur[1] - start[1]).powi(2)).sqrt();
                if travelled < DRAG_SLOP_PX / zoom {
                    // A click, not a drag.
                    if let Some(target) = click_target { self.click_select(target, canvas); }
                } else if canvas.layers.get(li).is_some() {
                    let r = [
                        start[0].min(cur[0]), start[1].min(cur[1]),
                        start[0].max(cur[0]), start[1].max(cur[1]),
                    ];
                    // Fully enclosed only — a big background image is
                    // not swept up by a box drawn across part of it.
                    let within = |b: [f32; 4]| b[0] >= r[0] && b[1] >= r[1] && b[2] <= r[2] && b[3] <= r[3];
                    let enclosed = |layer: &Layer| {
                        let mut found = Selection::default();
                        for (i, s) in layer.strokes.iter().enumerate() {
                            if s.bounds().is_some_and(within) { found.strokes.push(i); }
                        }
                        for (i, s) in layer.shapes.iter().enumerate() {
                            if within(shape_bounds(s)) { found.shapes.push(i); }
                        }
                        found
                    };
                    let mut found = enclosed(&canvas.layers[li]);
                    if found.is_empty() && !self.shift {
                        // Nothing in the box on the active layer. Rather
                        // than select nothing, take the topmost other
                        // visible layer that does have something in it.
                        let other = canvas
                            .layers
                            .iter()
                            .enumerate()
                            .rev()
                            .filter(|(i, l)| *i != li && l.visible && l.opacity > 0.0)
                            .map(|(i, l)| (i, enclosed(l)))
                            .find(|(_, sel)| !sel.is_empty());
                        if let Some((other_li, sel)) = other {
                            canvas.active_layer = other_li;
                            self.sel.clear();
                            found = sel;
                        }
                    }
                    for i in found.strokes { self.sel.insert(ObjRef::Stroke(i)); }
                    for i in found.shapes { self.sel.insert(ObjRef::Shape(i)); }
                    self.rebind_here(canvas);
                }
            }
            Some(gesture) => {
                // `preview_affine` reads the gesture from `self`.
                self.gesture = Some(gesture);
                if let Some(m) = self.preview_affine(zoom) {
                    canvas.transform_objects(li, &self.sel.strokes, &self.sel.shapes, &m);
                }
            }
            None => {}
        }
        self.gesture = None;
    }
}

fn is_raster(canvas: &Canvas, li: usize, obj: ObjRef) -> bool {
    matches!(obj, ObjRef::Shape(i)
        if matches!(canvas.layers.get(li).and_then(|l| l.shapes.get(i)), Some(Shape::Raster { .. })))
}

fn aabb_frame(b: [f32; 4]) -> Obb {
    Obb {
        center: [(b[0] + b[2]) * 0.5, (b[1] + b[3]) * 0.5],
        half: [(b[2] - b[0]) * 0.5, (b[3] - b[1]) * 0.5],
        angle: 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::shape::{BorderStyle, RasterKind};
    use crate::canvas::stroke::{PenSample, Stroke, StrokeStyle};
    use std::sync::Arc;

    fn stroke(from: [f32; 2], to: [f32; 2]) -> Stroke {
        let mut s = Stroke::with_style([0, 0, 0, 255], 4.0, StrokeStyle::Default);
        for i in 0..=10 {
            let t = i as f32 / 10.0;
            s.samples.push(PenSample {
                pos: [from[0] + (to[0] - from[0]) * t, from[1] + (to[1] - from[1]) * t],
                pressure: 1.0,
                tilt: [0.0, 0.0],
            });
        }
        s.build_cache();
        s
    }

    fn rect(a: [f32; 2], b: [f32; 2]) -> Shape {
        Shape::Rect { a, b, color: [0; 4], stroke_width: 2.0, border: BorderStyle::Solid, rotation: 0.0 }
    }

    fn image(pos: [f32; 2], size: [f32; 2]) -> Shape {
        Shape::Raster {
            pos, size, png: Arc::new(Vec::new()), px_size: [1, 1], color: [0; 4],
            rotation: 0.0, kind: RasterKind::Image, media: None,
            picture: None,
        }
    }

    /// Two strokes far apart plus one rectangle.
    fn canvas() -> Canvas {
        let mut c = Canvas::default();
        c.layers[0].strokes.push(stroke([0.0, 0.0], [100.0, 0.0]));
        c.layers[0].strokes.push(stroke([0.0, 200.0], [100.0, 200.0]));
        c.layers[0].shapes.push(rect([300.0, 300.0], [400.0, 360.0]));
        c
    }

    fn tool(c: &Canvas) -> SelectState {
        let mut t = SelectState::default();
        t.sync(true, 0, c);
        t
    }

    fn click(t: &mut SelectState, c: &mut Canvas, p: [f32; 2]) {
        t.begin(p, c);
        t.end(p, c);
    }

    #[test]
    fn click_selects_and_shift_click_adds_and_removes() {
        let mut c = canvas();
        let mut t = tool(&c);
        click(&mut t, &mut c, [50.0, 0.0]);
        assert_eq!(t.sel, Selection { strokes: vec![0], shapes: vec![] });

        t.shift = true;
        click(&mut t, &mut c, [50.0, 200.0]);
        click(&mut t, &mut c, [350.0, 300.0]);
        assert_eq!(t.sel, Selection { strokes: vec![0, 1], shapes: vec![0] });

        // Shift+click on a member drops it again.
        click(&mut t, &mut c, [50.0, 0.0]);
        assert_eq!(t.sel, Selection { strokes: vec![1], shapes: vec![0] });

        // A plain click on empty space clears everything.
        t.shift = false;
        click(&mut t, &mut c, [800.0, 800.0]);
        assert!(t.sel.is_empty());
        assert!(c.undo_log.is_empty(), "selecting must not touch the canvas");
    }

    #[test]
    fn dragging_a_stroke_moves_it_in_one_undo_step() {
        let mut c = canvas();
        let mut t = tool(&c);
        t.begin([50.0, 0.0], &mut c);
        t.drag([70.0, 30.0]);
        // Still a preview: nothing written yet.
        assert_eq!(c.layers[0].strokes[0].samples[0].pos, [0.0, 0.0]);
        t.end([70.0, 30.0], &mut c);
        assert_eq!(c.layers[0].strokes[0].samples[0].pos, [20.0, 30.0]);
        assert_eq!(c.layers[0].strokes[1].samples[0].pos, [0.0, 200.0], "unselected stroke stays");
        assert_eq!(c.undo_log.len(), 1);
        c.undo();
        assert_eq!(c.layers[0].strokes[0].samples[0].pos, [0.0, 0.0]);
        c.redo();
        assert_eq!(c.layers[0].strokes[0].samples[0].pos, [20.0, 30.0]);
    }

    #[test]
    fn a_tap_that_barely_moves_changes_nothing() {
        let mut c = canvas();
        let mut t = tool(&c);
        t.begin([50.0, 0.0], &mut c);
        t.end([51.0, 1.0], &mut c);
        assert!(c.undo_log.is_empty());
    }

    #[test]
    fn marquee_takes_only_fully_enclosed_objects() {
        let mut c = canvas();
        let mut t = tool(&c);
        t.begin([-20.0, -20.0], &mut c);
        // Encloses stroke 0, cuts through nothing else.
        t.end([120.0, 20.0], &mut c);
        assert_eq!(t.sel, Selection { strokes: vec![0], shapes: vec![] });

        // A box across only half of the second stroke takes nothing.
        t.begin([-20.0, 180.0], &mut c);
        t.end([50.0, 220.0], &mut c);
        assert!(t.sel.is_empty());
    }

    #[test]
    fn corner_resize_is_proportional_about_the_opposite_corner() {
        let mut c = Canvas::default();
        c.layers[0].shapes.push(image([0.0, 0.0], [100.0, 50.0]));
        let mut t = tool(&c);
        click(&mut t, &mut c, [50.0, 25.0]);
        assert_eq!(t.sel.shapes, vec![0]);

        // Drag the bottom-right corner out to twice the size.
        t.begin([100.0, 50.0], &mut c);
        assert!(matches!(t.gesture, Some(Gesture::Resize { x: 1, y: 1, .. })));
        t.end([200.0, 100.0], &mut c);
        match &c.layers[0].shapes[0] {
            Shape::Raster { pos, size, .. } => {
                assert!(pos[0].abs() < 1e-2 && pos[1].abs() < 1e-2, "top-left stays: {pos:?}");
                assert!((size[0] - 200.0).abs() < 1e-2 && (size[1] - 100.0).abs() < 1e-2, "{size:?}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn side_handle_stretches_one_axis() {
        let mut c = Canvas::default();
        c.layers[0].shapes.push(image([0.0, 0.0], [100.0, 50.0]));
        let mut t = tool(&c);
        click(&mut t, &mut c, [50.0, 25.0]);
        // Right-hand side handle, pulled 50 px further right.
        t.begin([100.0, 25.0], &mut c);
        assert!(matches!(t.gesture, Some(Gesture::Resize { x: 1, y: 0, .. })));
        t.end([150.0, 25.0], &mut c);
        assert!(matches!(&c.layers[0].shapes[0], Shape::Raster { size, .. }
            if (size[0] - 150.0).abs() < 1e-2 && (size[1] - 50.0).abs() < 1e-2));
    }

    #[test]
    fn rotate_knob_turns_the_shape_and_its_frame() {
        let mut c = Canvas::default();
        c.layers[0].shapes.push(image([0.0, 0.0], [100.0, 50.0]));
        let mut t = tool(&c);
        click(&mut t, &mut c, [50.0, 25.0]);
        // Knob sits ROTATE_STEM_PX above the top edge's midpoint.
        let knob = [50.0, -ROTATE_STEM_PX];
        t.begin(knob, &mut c);
        assert!(matches!(t.gesture, Some(Gesture::Rotate { .. })));
        // Swing it from straight up to straight right: a quarter turn.
        t.end([50.0 + 60.0, 25.0], &mut c);
        let angle = match &c.layers[0].shapes[0] {
            Shape::Raster { rotation, .. } => *rotation,
            other => panic!("{other:?}"),
        };
        assert!((angle - std::f32::consts::FRAC_PI_2).abs() < 1e-3, "{angle}");
        // The frame follows the shape's own orientation.
        let frame = t.frame(&c.layers[0], c.zoom).unwrap();
        assert!((frame.angle - angle).abs() < 1e-4);
        assert!((frame.half[0] - 50.0).abs() < 1e-2 && (frame.half[1] - 25.0).abs() < 1e-2);
    }

    #[test]
    fn shift_snaps_rotation_to_fifteen_degrees() {
        let mut c = Canvas::default();
        c.layers[0].shapes.push(image([0.0, 0.0], [100.0, 50.0]));
        let mut t = tool(&c);
        click(&mut t, &mut c, [50.0, 25.0]);
        t.begin([50.0, -ROTATE_STEM_PX], &mut c);
        t.shift = true;
        // About 40° clockwise from straight up → snaps to 45°.
        let a = (-90.0_f32 + 40.0).to_radians();
        t.end([50.0 + 80.0 * a.cos(), 25.0 + 80.0 * a.sin()], &mut c);
        assert!(matches!(&c.layers[0].shapes[0], Shape::Raster { rotation, .. }
            if (rotation - 45.0_f32.to_radians()).abs() < 1e-3));
    }

    #[test]
    fn dragging_an_unselected_image_on_the_active_layer_moves_it_in_one_go() {
        let mut c = Canvas::default();
        c.layers[0].shapes.push(image([0.0, 0.0], [500.0, 500.0]));
        c.layers[0].strokes.push(stroke([100.0, 100.0], [200.0, 100.0]));
        let mut t = tool(&c);
        t.begin([400.0, 400.0], &mut c);
        assert!(matches!(t.gesture, Some(Gesture::Move { .. })));
        t.end([430.0, 420.0], &mut c);
        assert_eq!(t.sel, Selection { strokes: vec![], shapes: vec![0] });
        assert!(matches!(&c.layers[0].shapes[0], Shape::Raster { pos, .. } if *pos == [30.0, 20.0]));
        assert_eq!(c.layers[0].strokes[0].samples[0].pos, [100.0, 100.0], "the ink stays put");
    }

    #[test]
    fn dragging_from_an_image_on_another_layer_draws_a_marquee_instead_of_moving_it() {
        let mut c = Canvas::default();
        c.layers[0].shapes.push(image([0.0, 0.0], [500.0, 500.0]));
        c.layers.push(Layer::default());
        c.layers[1].strokes.push(stroke([100.0, 100.0], [200.0, 100.0]));
        c.active_layer = 1;
        let mut t = tool(&c);
        // Press on bare image, drag a box around the stroke.
        t.begin([80.0, 80.0], &mut c);
        t.end([220.0, 120.0], &mut c);
        assert_eq!(t.sel, Selection { strokes: vec![0], shapes: vec![] });
        assert_eq!(c.active_layer, 1);
        assert!(c.undo_log.is_empty(), "the image must not have moved");

        // A click on the image that does not travel selects it.
        click(&mut t, &mut c, [400.0, 400.0]);
        assert_eq!(c.active_layer, 0);
        assert_eq!(t.sel, Selection { strokes: vec![], shapes: vec![0] });
    }

    #[test]
    fn selection_is_dropped_when_the_layer_changes_under_it() {
        let mut c = canvas();
        let mut t = tool(&c);
        click(&mut t, &mut c, [50.0, 0.0]);
        t.sync(true, 0, &c);
        assert!(!t.sel.is_empty(), "nothing changed, selection stays");
        // Another tool removes an object: indices are stale now.
        c.layers[0].strokes.remove(1);
        t.sync(true, 0, &c);
        assert!(t.sel.is_empty());
    }

    #[test]
    fn mixed_selection_with_text_gets_corner_handles_only() {
        let mut c = Canvas::default();
        c.layers[0].strokes.push(stroke([0.0, 0.0], [100.0, 0.0]));
        c.layers[0].shapes.push(Shape::Text {
            pos: [0.0, 50.0], content: "hi".into(), font_size: 20.0, color: [0; 4], rotation: 0.0,
        });
        let mut t = tool(&c);
        t.sel = Selection { strokes: vec![0], shapes: vec![0] };
        let scale_handles = t
            .chrome(&c)
            .handles
            .iter()
            .filter(|(_, h)| matches!(h, Handle::Scale { .. }))
            .count();
        assert_eq!(scale_handles, 4);
        // Strokes alone can stretch, so all eight are offered.
        t.sel = Selection { strokes: vec![0], shapes: vec![] };
        assert_eq!(t.chrome(&c).handles.len(), 9);
    }

    #[test]
    fn pressing_a_thin_stroke_moves_it_rather_than_grabbing_a_handle() {
        // A horizontal stroke is only a few pixels tall. Its frame is
        // padded so the side handles do not sit on the ink.
        let mut c = canvas();
        let mut t = tool(&c);
        click(&mut t, &mut c, [50.0, 0.0]);
        let frame = t.frame(&c.layers[0], c.zoom).unwrap();
        assert!(frame.half[1] >= 22.0, "{frame:?}");
        // Dead centre of the stroke: exactly between the top and bottom
        // side handles.
        t.begin([50.0, 0.0], &mut c);
        assert!(matches!(t.gesture, Some(Gesture::Move { .. })), "{:?}", t.gesture);
        t.end([50.0, 30.0], &mut c);
        assert_eq!(c.layers[0].strokes[0].samples[0].pos, [0.0, 30.0]);
    }

    // ---- other layers and background plates ------------------------------

    use crate::canvas::canvas::LayerImage;

    /// A 500×500 background plate at the origin — what a lasso capture
    /// or frozen frame made before captures were objects looks like.
    fn plate() -> LayerImage {
        LayerImage {
            png: Vec::new(),
            size: [500, 500],
            logical_size: [500.0, 500.0],
            canvas_origin: [0.0, 0.0],
            capture_zoom: 1.5,
        }
    }

    /// The shape most real slides have: a capture on layer 0, ink on
    /// layer 1, layer 1 active.
    fn capture_under_ink() -> Canvas {
        let mut c = Canvas::default();
        c.layers[0].image = Some(plate());
        c.layers.push(Layer::default());
        c.layers[1].strokes.push(stroke([100.0, 100.0], [200.0, 100.0]));
        c.active_layer = 1;
        c
    }

    #[test]
    fn clicking_ink_on_another_layer_selects_it_and_makes_that_layer_active() {
        let mut c = canvas();
        c.layers.push(Layer::default());
        c.active_layer = 1;
        let mut t = tool(&c);
        click(&mut t, &mut c, [50.0, 0.0]);
        assert_eq!(c.active_layer, 0);
        assert_eq!(t.sel, Selection { strokes: vec![0], shapes: vec![] });
        // The switch must not look like "the layer changed under the
        // selection" on the next frame.
        t.sync(true, 0, &c);
        assert!(!t.sel.is_empty());
        assert!(c.undo_log.is_empty(), "picking is not an edit");
    }

    #[test]
    fn dragging_ink_on_another_layer_moves_it_in_one_go() {
        let mut c = canvas();
        c.layers.push(Layer::default());
        c.active_layer = 1;
        let mut t = tool(&c);
        t.begin([50.0, 0.0], &mut c);
        t.end([60.0, 40.0], &mut c);
        assert_eq!(c.active_layer, 0);
        assert_eq!(c.layers[0].strokes[0].samples[0].pos, [10.0, 40.0]);
    }

    #[test]
    fn clicking_an_old_plate_turns_it_into_a_selected_image_in_place() {
        let mut c = capture_under_ink();
        let mut t = tool(&c);
        click(&mut t, &mut c, [400.0, 400.0]);
        assert_eq!(c.active_layer, 0);
        assert!(c.layers[0].image.is_none());
        assert!(matches!(&c.layers[0].shapes[0], Shape::Raster { pos, size, .. }
            if *pos == [0.0, 0.0] && *size == [500.0, 500.0]));
        assert_eq!(t.sel, Selection { strokes: vec![], shapes: vec![0] });
        t.sync(true, 0, &c);
        assert!(!t.sel.is_empty());

        // Now it moves like any image: drag from inside its frame.
        t.begin([400.0, 400.0], &mut c);
        t.end([430.0, 420.0], &mut c);
        assert!(matches!(&c.layers[0].shapes[0], Shape::Raster { pos, .. } if *pos == [30.0, 20.0]));

        // One Ctrl+Z takes the move back …
        c.undo();
        assert!(matches!(&c.layers[0].shapes[0], Shape::Raster { pos, .. } if *pos == [0.0, 0.0]));
        // … and the next restores the plate exactly as it was.
        c.undo();
        assert!(c.layers[0].shapes.is_empty());
        let img = c.layers[0].image.as_ref().expect("plate is back");
        assert_eq!((img.size, img.logical_size, img.canvas_origin, img.capture_zoom),
            ([500, 500], [500.0, 500.0], [0.0, 0.0], 1.5));
        // Redo walks forward through both again.
        c.redo();
        assert!(matches!(&c.layers[0].shapes[0], Shape::Raster { pos, .. } if *pos == [30.0, 20.0]));
    }

    #[test]
    fn dragging_from_a_plate_box_selects_and_leaves_the_plate_alone() {
        let mut c = capture_under_ink();
        let mut t = tool(&c);
        t.begin([80.0, 80.0], &mut c);
        t.end([220.0, 120.0], &mut c);
        assert_eq!(t.sel, Selection { strokes: vec![0], shapes: vec![] });
        assert_eq!(c.active_layer, 1);
        assert!(c.layers[0].image.is_some(), "a drag must not convert the plate");
        assert!(c.undo_log.is_empty());
    }

    #[test]
    fn the_selection_frame_wins_over_the_picture_underneath() {
        let mut c = capture_under_ink();
        // A diagonal stroke, so its frame has room inside that is
        // neither ink nor within reach of a handle.
        c.layers[1].strokes[0] = stroke([100.0, 100.0], [200.0, 200.0]);
        let mut t = tool(&c);
        click(&mut t, &mut c, [150.0, 150.0]);
        assert_eq!(t.sel.strokes, vec![0]);
        // Press inside the frame but well off the ink — the plate is
        // right there underneath — and drag.
        t.begin([180.0, 120.0], &mut c);
        assert!(matches!(t.gesture, Some(Gesture::Move { .. })));
        t.end([200.0, 150.0], &mut c);
        assert_eq!(c.layers[1].strokes[0].samples[0].pos, [120.0, 130.0]);
        assert!(c.layers[0].image.is_some());
    }

    #[test]
    fn box_select_falls_through_to_a_layer_that_has_something_in_the_box() {
        let mut c = canvas();
        c.layers.push(Layer::default());
        c.active_layer = 1;
        let mut t = tool(&c);
        t.begin([-20.0, -20.0], &mut c);
        t.end([120.0, 20.0], &mut c);
        assert_eq!(c.active_layer, 0);
        assert_eq!(t.sel, Selection { strokes: vec![0], shapes: vec![] });
    }

    #[test]
    fn selecting_a_plate_keeps_shapes_already_selected_on_that_layer() {
        // Shift+click a plate on the active layer while a rectangle
        // there is selected: the plate goes in at shape index 0, so the
        // rectangle's index has to follow it from 0 to 1.
        let mut c = Canvas::default();
        c.layers[0].image = Some(plate());
        c.layers[0].shapes.push(rect([600.0, 600.0], [700.0, 660.0]));
        let mut t = tool(&c);
        click(&mut t, &mut c, [650.0, 600.0]);
        assert_eq!(t.sel.shapes, vec![0]);
        t.shift = true;
        click(&mut t, &mut c, [250.0, 250.0]);
        assert_eq!(t.sel.shapes, vec![0, 1]);
        assert!(matches!(&c.layers[0].shapes[0], Shape::Raster { .. }));
        assert!(matches!(&c.layers[0].shapes[1], Shape::Rect { .. }));
    }
}
