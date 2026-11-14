//! Pins: objects from one slide kept on screen, on that slide only or on
//! every slide.
//!
//! Pinning tags the chosen objects with a pin id (`Stroke::pin`,
//! `Shape::pin`) on the slide they live on, and gives the pin a place on
//! screen (`Pin`). The objects stay on their slide, editable there as
//! before. The pin draws them again, at its own position, size and turn,
//! on top of the slide — a local pin (`Pin::local`) only on its own
//! slide, a global one on whichever slide is showing, their own
//! included — and stays put as the slide is panned and zoomed. It
//! always shows the objects as they are now: recolour one, edit its
//! text or play its video and the pin shows that too. Moving, resizing
//! or turning a pin never touches the objects.
//!
//! The tags are part of the slide, so pinning is an undo step there. A
//! pin's place is not: it is kept in `canvases/pins.ron`
//! (`persistence::save_pins`), the way slide views are kept in
//! `views.ron`, because moving a pin is not an edit of any slide and must
//! not make the autosave rewrite one.
//!
//! Screen positions here are in points from the canvas area's top-left.

use crate::canvas::affine::Affine;
use crate::canvas::canvas::{shape_bounds, Canvas};
use crate::canvas::hit::{self, ObjRef};
use crate::canvas::shape::Obb;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Where a pin shows its objects. A pinned object's point `p` (canvas
/// units of its slide) lands on screen at
/// `center + R(angle) · scale · (p − anchor)`, where `anchor` is the
/// centre of the box around all the pin's objects as they are now. So
/// moving an object on its slide leaves the pin where it is, while the
/// pin still shows the object's new look.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Pin {
    pub id: u64,
    /// Screen position of the centre of the objects' box.
    pub center: [f32; 2],
    /// Screen points per canvas unit of the objects' slide.
    pub scale: f32,
    /// Turn, radians, clockwise on screen.
    #[serde(default)]
    pub angle: f32,
    /// Shows only on the slide its objects are on, not on every slide.
    /// Missing from a `pins.ron` written before there were local pins,
    /// whose pins all showed everywhere.
    #[serde(default)]
    pub local: bool,
}

/// Smallest and largest a pin may be scaled to, in screen points per
/// canvas unit. Keeps a pin from vanishing or swallowing the screen.
const MIN_SCALE: f32 = 1e-4;
const MAX_SCALE: f32 = 1e4;
/// Smallest on-screen width / height of a pin's frame, like the Select
/// tool's: around a dot or a thin line the handles would otherwise sit
/// on top of each other.
const MIN_FRAME_PX: f32 = 44.0;
/// How close (screen points) a press must land to a pinned object.
const CLICK_SLOP_PX: f32 = 6.0;
/// Grab radius (screen points) around a handle.
const HANDLE_GRAB_PX: f32 = 11.0;
/// Distance of the rotate knob above the frame's top edge.
pub const ROTATE_STEM_PX: f32 = 28.0;
/// Rotation snap while Shift is held.
const ROTATE_SNAP: f32 = std::f32::consts::PI / 12.0;
/// A second press on the same pin within this long is a double-click.
const DOUBLE_CLICK: Duration = Duration::from_millis(450);
/// Pointer travel (screen points) before a press on a pin moves it.
const DRAG_SLOP_PX: f32 = 4.0;

impl Pin {
    /// Canvas units of the objects' slide → canvas units again, turned
    /// and moved so that scaling the result by `scale` about the canvas
    /// area's top-left gives the screen. Kept rigid (no scale) so stroke
    /// widths and font sizes are scaled once, by the renderer's zoom.
    pub fn rigid(&self, anchor: [f32; 2]) -> Affine {
        let s = self.scale.max(MIN_SCALE);
        Affine::translate([-anchor[0], -anchor[1]])
            .then(Affine::rotate_about([0.0, 0.0], self.angle))
            .then(Affine::translate([self.center[0] / s, self.center[1] / s]))
    }

    /// The canvas point (of the objects' slide) shown at screen point `q`.
    pub fn canvas_point(&self, anchor: [f32; 2], q: [f32; 2]) -> [f32; 2] {
        let s = self.scale.max(MIN_SCALE);
        let d = [(q[0] - self.center[0]) / s, (q[1] - self.center[1]) / s];
        let (sin, cos) = self.angle.sin_cos();
        [anchor[0] + cos * d[0] + sin * d[1], anchor[1] - sin * d[0] + cos * d[1]]
    }

    /// The pin's frame on screen around objects whose box is `bounds`,
    /// at least `MIN_FRAME_PX` across each way.
    pub fn frame(&self, bounds: [f32; 4]) -> Obb {
        let half = |a: f32, b: f32| ((b - a) * 0.5 * self.scale).max(MIN_FRAME_PX * 0.5);
        Obb { center: self.center, half: [half(bounds[0], bounds[2]), half(bounds[1], bounds[3])], angle: self.angle }
    }

    /// This pin as seen through a view that shows its screen point `q` at
    /// `origin + q · zoom` (the slide window's): its handles, grab areas
    /// and smallest frame then keep their size on screen there, as on the
    /// canvas. `unviewed` turns a place found there back into the pin's.
    pub fn viewed(&self, origin: [f32; 2], zoom: f32) -> Pin {
        Pin {
            center: [origin[0] + self.center[0] * zoom, origin[1] + self.center[1] * zoom],
            scale: self.scale * zoom,
            ..*self
        }
    }

    /// The inverse of `viewed`.
    pub fn unviewed(&self, origin: [f32; 2], zoom: f32) -> Pin {
        let zoom = zoom.max(1e-6);
        Pin {
            center: [(self.center[0] - origin[0]) / zoom, (self.center[1] - origin[1]) / zoom],
            scale: (self.scale / zoom).clamp(MIN_SCALE, MAX_SCALE),
            ..*self
        }
    }

    /// A pin over objects of `canvas` whose box is `bounds`, placed
    /// exactly where they show on that slide now. Global, as every pin
    /// whose place could be lost was before local pins.
    fn over(id: u64, canvas: &Canvas, bounds: [f32; 4]) -> Pin {
        let a = center_of(bounds);
        Pin {
            id,
            center: [canvas.pan[0] + a[0] * canvas.zoom, canvas.pan[1] + a[1] * canvas.zoom],
            scale: canvas.zoom.clamp(MIN_SCALE, MAX_SCALE),
            angle: 0.0,
            local: false,
        }
    }
}

pub fn center_of(b: [f32; 4]) -> [f32; 2] {
    [(b[0] + b[2]) * 0.5, (b[1] + b[3]) * 0.5]
}

/// One pinned object: its layer and its place in that layer's lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinMember {
    pub layer: usize,
    pub obj: ObjRef,
}

/// The objects of one pin on a slide, in paint order: layer by layer,
/// each layer's shapes and then its strokes, as the slide paints them.
#[derive(Debug, Clone)]
struct Found {
    id: u64,
    members: Vec<PinMember>,
    /// `None` only if no object has any extent (an empty stroke).
    bounds: Option<[f32; 4]>,
}

/// What a slide held when it was last looked through.
struct SlideScan {
    stamp: u64,
    found: Vec<Found>,
}

/// Cheap fingerprint of a slide that changes whenever an object is
/// added, removed or edited in place (see `Canvas::edit_stamp`) — and
/// so whenever a pin tag could have.
fn stamp(c: &Canvas) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    c.edit_stamp().hash(&mut h);
    h.finish()
}

fn grow(bb: &mut Option<[f32; 4]>, b: [f32; 4]) {
    *bb = Some(match *bb {
        None => b,
        Some(a) => [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])],
    });
}

/// Every pin with objects on `c`, in the order their first objects paint.
fn scan(c: &Canvas) -> Vec<Found> {
    let mut found: Vec<Found> = Vec::new();
    let mut at: HashMap<u64, usize> = HashMap::new();
    let mut add = |id: u64, member: PinMember, b: Option<[f32; 4]>| {
        let k = *at.entry(id).or_insert_with(|| {
            found.push(Found { id, members: Vec::new(), bounds: None });
            found.len() - 1
        });
        let f = &mut found[k];
        if let Some(b) = b {
            grow(&mut f.bounds, b);
        }
        f.members.push(member);
    };
    for (li, layer) in c.layers.iter().enumerate() {
        for (i, s) in layer.shapes.iter().enumerate() {
            if s.pin() != 0 {
                add(s.pin(), PinMember { layer: li, obj: ObjRef::Shape(i) }, Some(shape_bounds(s)));
            }
        }
        for (i, s) in layer.strokes.iter().enumerate() {
            if s.pin != 0 {
                add(s.pin, PinMember { layer: li, obj: ObjRef::Stroke(i) }, s.bounds());
            }
        }
    }
    found
}

/// Box around the chosen objects of `layer`, canvas units.
pub fn objects_bounds(layer: &crate::canvas::canvas::Layer, strokes: &[usize], shapes: &[usize]) -> Option<[f32; 4]> {
    let mut bb = None;
    for s in strokes.iter().filter_map(|&i| layer.strokes.get(i)) {
        if let Some(b) = s.bounds() { grow(&mut bb, b); }
    }
    for s in shapes.iter().filter_map(|&i| layer.shapes.get(i)) {
        grow(&mut bb, shape_bounds(s));
    }
    bb
}

/// The pin tag of object `m` on `c`, or `None` if there is no such object.
fn tag_of(c: &Canvas, m: PinMember) -> Option<u64> {
    let layer = c.layers.get(m.layer)?;
    match m.obj {
        ObjRef::Stroke(i) => layer.strokes.get(i).map(|s| s.pin),
        ObjRef::Shape(i) => layer.shapes.get(i).map(|s| s.pin()),
    }
}

/// A pin with objects to show, for drawing and hit-testing. `pin` is its
/// place as shown: the stored one, nudged to keep the pin on the canvas
/// area (see `PinBoard::set_area`).
#[derive(Clone, Copy)]
pub struct Pinned<'a> {
    pub pin: Pin,
    /// Index of the slide its objects live on.
    pub slide: usize,
    pub canvas: &'a Canvas,
    pub members: &'a [PinMember],
    /// Box around the objects, canvas units of their slide.
    pub bounds: [f32; 4],
}

impl Pinned<'_> {
    pub fn anchor(&self) -> [f32; 2] {
        center_of(self.bounds)
    }

    pub fn frame(&self) -> Obb {
        self.pin.frame(self.bounds)
    }

    /// The on-screen box of `shape` (one of this pin's).
    pub fn shape_obb(&self, shape: &crate::canvas::shape::Shape) -> Obb {
        let o = shape.obb();
        let s = self.pin.scale;
        let p = self.pin.rigid(self.anchor()).apply(o.center);
        Obb { center: [p[0] * s, p[1] * s], half: [o.half[0] * s, o.half[1] * s], angle: o.angle + self.pin.angle }
    }

    /// Whether screen point `q` lands on one of the pin's objects. They
    /// all show, whatever their layer (see `render::paint_pinned`).
    fn touches(&self, q: [f32; 2]) -> bool {
        let p = self.pin.canvas_point(self.anchor(), q);
        let tol = CLICK_SLOP_PX / self.pin.scale.max(MIN_SCALE);
        self.members.iter().any(|m| {
            let Some(layer) = self.canvas.layers.get(m.layer) else { return false };
            match m.obj {
                ObjRef::Stroke(i) => layer.strokes.get(i).is_some_and(|s| hit::stroke_hit(s, p, tol)),
                ObjRef::Shape(i) => layer.shapes.get(i).is_some_and(|s| hit::shape_hit(s, p, tol)),
            }
        })
    }
}

/// All pins: their places, and which slide holds each one's objects.
#[derive(Default)]
pub struct PinBoard {
    /// Every place known this run, bottom to top. A place outlives its
    /// objects for the rest of the run, so an undo that brings them back
    /// brings the pin back where it was.
    pins: Vec<Pin>,
    /// What `pins.ron` last received; `None` before the first write.
    saved: Option<Vec<Pin>>,
    /// Per slide, what it held when last looked through; `None` for a
    /// slide not read yet or not looked through since it changed.
    scans: Vec<Option<SlideScan>>,
    /// Pin id → (slide, index into that slide's `found`).
    located: HashMap<u64, (usize, usize)>,
    /// Every pin is hidden for now (the toolbar toggle). Not saved: each
    /// run starts with the pins showing.
    hidden: bool,
    /// The global pins show in the slide window instead of on the
    /// canvas (its "Pins" toggle). Not saved: the slide window is not.
    globals_away: bool,
    /// Size of the canvas area, points: pins are shown inside it.
    area: Option<[f32; 2]>,
}

/// `pin` moved, if need be, so that its frame around objects whose box is
/// `bounds` lies inside `area` (from the canvas area's top-left). One too
/// big to fit shows from its top-left corner.
fn kept_in(pin: Pin, bounds: [f32; 4], area: [f32; 2]) -> Pin {
    let [x0, y0, x1, y1] = pin.frame(bounds).aabb();
    let shift = |lo: f32, hi: f32, room: f32| {
        if lo < 0.0 || hi - lo > room {
            -lo
        } else if hi > room {
            room - hi
        } else {
            0.0
        }
    };
    Pin { center: [pin.center[0] + shift(x0, x1, area[0]), pin.center[1] + shift(y0, y1, area[1])], ..pin }
}

impl PinBoard {
    pub fn new(pins: Vec<Pin>) -> Self {
        let mut seen = std::collections::HashSet::new();
        let pins: Vec<Pin> = pins
            .into_iter()
            .filter(|p| p.id != 0 && seen.insert(p.id) && usable(p))
            .collect();
        Self { saved: Some(pins.clone()), pins, ..Self::default() }
    }

    /// A board holding just these places, unchecked, for `PinTool` to move
    /// pins on in another space (`Pin::viewed`); read the result back with
    /// `get_mut`.
    pub fn scratch(pins: Vec<Pin>) -> Self {
        Self { pins, ..Self::default() }
    }

    /// Look through the slides that changed since the last call (or
    /// whose pinned objects moved without changing the slide's
    /// fingerprint). A tag with no place — `pins.ron` lost, or the pin
    /// was made by a run that never saved it — gets one: where its
    /// objects show on their slide. Cheap when nothing changed; call
    /// once a frame and after anything that tags objects.
    pub fn refresh(&mut self, canvases: &[Option<Canvas>]) {
        self.scans.resize_with(canvases.len(), || None);
        self.scans.truncate(canvases.len());
        let mut changed = false;
        for (i, slot) in canvases.iter().enumerate() {
            let Some(c) = slot else {
                changed |= self.scans[i].take().is_some();
                continue;
            };
            let st = stamp(c);
            let fresh = self.scans[i].as_ref().is_some_and(|s| {
                s.stamp == st && s.found.iter().all(|f| f.members.iter().all(|m| tag_of(c, *m) == Some(f.id)))
            });
            if fresh { continue; }
            let found = scan(c);
            for f in &found {
                if let Some(b) = f.bounds.filter(|_| !self.pins.iter().any(|p| p.id == f.id)) {
                    self.pins.push(Pin::over(f.id, c, b));
                }
            }
            self.scans[i] = Some(SlideScan { stamp: st, found });
            changed = true;
        }
        if changed {
            self.located.clear();
            for (si, scan) in self.scans.iter().enumerate() {
                for (k, f) in scan.iter().flat_map(|s| s.found.iter()).enumerate() {
                    // The first slide found wins should a tag ever turn up
                    // on two.
                    self.located.entry(f.id).or_insert((si, k));
                }
            }
        }
    }

    /// Forget every scan: slides were renumbered (one was deleted).
    pub fn forget_scans(&mut self) {
        self.scans.clear();
        self.located.clear();
    }

    /// The pins that have objects to show on slide `active`, bottom to
    /// top: the global ones and its own local ones — only its local ones
    /// while the global pins are away in the slide window. None while
    /// the pins are hidden, so nothing draws, plays or grabs them then.
    pub fn pinned<'a>(&'a self, canvases: &'a [Option<Canvas>], active: usize) -> Vec<Pinned<'a>> {
        self.shown(canvases, |pin, slide| if pin.local { slide == active } else { !self.globals_away })
    }

    /// The global pins, bottom to top, wherever they show; none while the
    /// pins are hidden.
    pub fn globals<'a>(&'a self, canvases: &'a [Option<Canvas>]) -> Vec<Pinned<'a>> {
        self.shown(canvases, |pin, _| !pin.local)
    }

    /// The pins `keep` takes (given the pin and its objects' slide) that
    /// have objects to show.
    fn shown<'a>(&'a self, canvases: &'a [Option<Canvas>], keep: impl Fn(&Pin, usize) -> bool) -> Vec<Pinned<'a>> {
        if self.hidden {
            return Vec::new();
        }
        self.pins
            .iter()
            .filter_map(|pin| {
                let &(slide, k) = self.located.get(&pin.id)?;
                if !keep(pin, slide) { return None; }
                let canvas = canvases.get(slide)?.as_ref()?;
                let f = self.scans.get(slide)?.as_ref()?.found.get(k)?;
                let bounds = f.bounds?;
                let pin = match self.area {
                    Some(area) => kept_in(*pin, bounds, area),
                    None => *pin,
                };
                Some(Pinned { pin, slide, canvas, members: &f.members, bounds })
            })
            .collect()
    }

    /// Show the global pins in the slide window instead of on the canvas,
    /// or back on the canvas.
    pub fn set_globals_away(&mut self, away: bool) {
        self.globals_away = away;
    }

    /// The canvas area is `area` points across: every pin shows wholly
    /// inside it, moved in from an edge if its place would put part of it
    /// outside (a smaller window, a pin grown or turned past the edge).
    /// Only where it shows: the place kept for it stays, so a window that
    /// is small for a moment does not push the pins about for good.
    pub fn set_area(&mut self, area: [f32; 2]) {
        self.area = (area[0] > 1.0 && area[1] > 1.0).then_some(area);
    }

    /// Some pin has objects to show on slide `active` (hidden or not).
    pub fn any(&self, active: usize) -> bool {
        self.located.iter().any(|(id, &(slide, _))| {
            slide == active || self.pins.iter().any(|p| p.id == *id && !p.local)
        })
    }

    pub fn hidden(&self) -> bool {
        self.hidden
    }

    /// Hide every pin, or show them again. Only the pins: their objects
    /// stay on their own slides as they are.
    pub fn set_hidden(&mut self, hidden: bool) {
        self.hidden = hidden;
    }

    /// The slide holding pin `id`'s objects, and those objects.
    pub fn members(&self, id: u64) -> Option<(usize, &[PinMember])> {
        let &(slide, k) = self.located.get(&id)?;
        let f = self.scans.get(slide)?.as_ref()?.found.get(k)?;
        Some((slide, &f.members))
    }

    pub fn get_mut(&mut self, id: u64) -> Option<&mut Pin> {
        self.pins.iter_mut().find(|p| p.id == id)
    }

    /// Put pin `id` on top of the others.
    pub fn raise(&mut self, id: u64) {
        if let Some(i) = self.pins.iter().position(|p| p.id == id) {
            let p = self.pins.remove(i);
            self.pins.push(p);
        }
    }

    /// Add a place for a new pin.
    pub fn add(&mut self, pin: Pin) {
        self.pins.retain(|p| p.id != pin.id);
        self.pins.push(pin);
    }

    /// An id no pin has used this run (nor, practically, ever: it starts
    /// from the clock, and tags outlive runs).
    pub fn fresh_id(&self) -> u64 {
        let base = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |d| d.as_nanos() as u64);
        (0..)
            .map(|n: u64| base.wrapping_add(n))
            .find(|&id| id != 0 && !self.pins.iter().any(|p| p.id == id))
            .unwrap_or(1)
    }

    /// The places to write to `pins.ron`, if they changed since the last
    /// write: those of pins that still have objects. `None` while some
    /// slide is still being read — its pins are not known yet, and
    /// leaving them out would lose their places.
    pub fn take_changed(&mut self, all_read: bool) -> Option<Vec<Pin>> {
        if !all_read { return None; }
        let live: Vec<Pin> = self.pins.iter().filter(|p| self.located.contains_key(&p.id)).copied().collect();
        if self.saved.as_ref() == Some(&live) { return None; }
        self.saved = Some(live.clone());
        Some(live)
    }
}

/// A place read from a damaged or hand-edited `pins.ron` is dropped (its
/// pin then gets a fresh place on its slide).
fn usable(p: &Pin) -> bool {
    p.center.iter().all(|v| v.is_finite()) && p.angle.is_finite() && (MIN_SCALE..=MAX_SCALE).contains(&p.scale)
}

// ---- pointer --------------------------------------------------------------

/// A grab point on a selected pin's frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinHandle {
    /// Corner `(x, y)`, each −1 or 1 in the frame's own axes. Resizing a
    /// pin keeps its proportions, so there are no side handles.
    Corner { x: i8, y: i8 },
    Rotate,
}

/// The selected pin's handles on screen, rotate knob last.
pub fn handles(frame: &Obb) -> Vec<([f32; 2], PinHandle)> {
    let c = frame.corners();
    let mut out: Vec<([f32; 2], PinHandle)> = c
        .iter()
        .zip([(-1, -1), (1, -1), (1, 1), (-1, 1)])
        .map(|(p, (x, y))| (*p, PinHandle::Corner { x, y }))
        .collect();
    out.push((rotate_knob(frame).1, PinHandle::Rotate));
    out
}

/// `(top-edge midpoint, knob centre)` of the rotate handle.
pub fn rotate_knob(frame: &Obb) -> ([f32; 2], [f32; 2]) {
    let top = frame.to_canvas([0.0, -frame.half[1]]);
    let knob = frame.to_canvas([0.0, -frame.half[1] - ROTATE_STEM_PX]);
    (top, knob)
}

#[derive(Debug, Clone, Copy)]
enum Gesture {
    /// `moved` once the pointer has travelled past the drag slop.
    Move { id: u64, start: [f32; 2], center: [f32; 2], moved: bool },
    Resize { id: u64, x: i8, y: i8, frame: Obb, scale: f32 },
    Rotate { id: u64, start: [f32; 2], angle: f32 },
}

/// What a press did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Press {
    /// Missed every pin; the press is the Select tool's.
    Missed,
    /// Took a pin (selected it, and maybe started a drag).
    Took,
    /// Second press on the same pin: go to the slide its objects are on.
    Open(u64),
}

/// What the pointer under the Select tool is doing to pins.
#[derive(Default)]
pub struct PinTool {
    /// The pin whose frame and handles show.
    pub selected: Option<u64>,
    gesture: Option<Gesture>,
    last_press: Option<(u64, Instant)>,
}

impl PinTool {
    /// A press is being carried out on a pin.
    pub fn busy(&self) -> bool {
        self.gesture.is_some()
    }

    /// The press started on a pin and moved it, resized it or turned it.
    pub fn dragging(&self) -> bool {
        match self.gesture {
            Some(Gesture::Move { moved, .. }) => moved,
            Some(_) => true,
            None => false,
        }
    }

    /// Drop the selection and any gesture.
    pub fn clear(&mut self) {
        self.selected = None;
        self.gesture = None;
    }

    /// Forget the press in flight, which another input reports again: the
    /// tablet driver's mouse press can arrive a frame before the pen's
    /// own. Counted twice, one tap would be a double-click.
    pub fn forget_press(&mut self) {
        self.gesture = None;
        self.last_press = None;
    }

    /// The selected pin's handle under `q`, if any.
    fn handle_at(&self, q: [f32; 2], pins: &[Pinned]) -> Option<(u64, PinHandle, Obb)> {
        let id = self.selected?;
        let p = pins.iter().find(|p| p.pin.id == id)?;
        let frame = p.frame();
        handles(&frame)
            .into_iter()
            .rev() // the knob first
            .find(|(h, _)| (h[0] - q[0]).powi(2) + (h[1] - q[1]).powi(2) <= HANDLE_GRAB_PX * HANDLE_GRAB_PX)
            .map(|(_, h)| (id, h, frame))
    }

    /// The topmost pin a press at `q` lands on: the selected pin anywhere
    /// in its frame, any other where one of its objects is drawn.
    fn pin_at(&self, q: [f32; 2], pins: &[Pinned]) -> Option<u64> {
        pins.iter()
            .rev()
            .find(|p| {
                let inside = || {
                    let f = p.frame();
                    let l = f.to_local(q);
                    l[0].abs() <= f.half[0] && l[1].abs() <= f.half[1]
                };
                (self.selected == Some(p.pin.id) && inside()) || p.touches(q)
            })
            .map(|p| p.pin.id)
    }

    /// What is under `q`, for the cursor: a handle, a pin, or nothing.
    pub fn hover(&self, q: [f32; 2], pins: &[Pinned]) -> Option<Option<PinHandle>> {
        if let Some((_, h, _)) = self.handle_at(q, pins) {
            return Some(Some(h));
        }
        self.pin_at(q, pins).map(|_| None)
    }

    /// The handle being dragged, for the cursor.
    pub fn active_handle(&self) -> Option<PinHandle> {
        match self.gesture? {
            Gesture::Resize { x, y, .. } => Some(PinHandle::Corner { x, y }),
            Gesture::Rotate { .. } => Some(PinHandle::Rotate),
            Gesture::Move { .. } => None,
        }
    }

    pub fn press(&mut self, q: [f32; 2], pins: &[Pinned]) -> Press {
        self.gesture = None;
        if let Some((id, h, frame)) = self.handle_at(q, pins) {
            let pin = pins.iter().find(|p| p.pin.id == id).map(|p| p.pin);
            let Some(pin) = pin else { return Press::Missed };
            self.gesture = Some(match h {
                PinHandle::Corner { x, y } => Gesture::Resize { id, x, y, frame, scale: pin.scale },
                PinHandle::Rotate => Gesture::Rotate { id, start: q, angle: pin.angle },
            });
            self.last_press = None;
            return Press::Took;
        }
        let Some(id) = self.pin_at(q, pins) else {
            self.selected = None;
            self.last_press = None;
            return Press::Missed;
        };
        let now = Instant::now();
        if self.last_press.is_some_and(|(last, at)| last == id && now.duration_since(at) <= DOUBLE_CLICK) {
            self.last_press = None;
            return Press::Open(id);
        }
        self.last_press = Some((id, now));
        self.selected = Some(id);
        let center = pins.iter().find(|p| p.pin.id == id).map_or(q, |p| p.pin.center);
        self.gesture = Some(Gesture::Move { id, start: q, center, moved: false });
        Press::Took
    }

    /// Carry the gesture to `q`. `snap` (Shift) turns in 15° steps.
    pub fn drag(&mut self, q: [f32; 2], snap: bool, board: &mut PinBoard) {
        let Some(g) = self.gesture.as_mut() else { return };
        match g {
            Gesture::Move { id, start, center, moved } => {
                let d = [q[0] - start[0], q[1] - start[1]];
                if !*moved && (d[0] * d[0] + d[1] * d[1]).sqrt() < DRAG_SLOP_PX { return; }
                *moved = true;
                // Not a click any more, so not half of a double-click.
                self.last_press = None;
                if let Some(pin) = board.get_mut(*id) {
                    pin.center = [center[0] + d[0], center[1] + d[1]];
                }
            }
            Gesture::Resize { id, x, y, frame, scale } => {
                let [hw, hh] = frame.half;
                let l = frame.to_local(q);
                // The opposite corner stays put; the pointer is projected
                // onto the diagonal so the pin keeps its proportions.
                let anchor = [-(*x as f32) * hw, -(*y as f32) * hh];
                let diag = [2.0 * *x as f32 * hw, 2.0 * *y as f32 * hh];
                let s = ((l[0] - anchor[0]) * diag[0] + (l[1] - anchor[1]) * diag[1]) / (diag[0] * diag[0] + diag[1] * diag[1]);
                // Never smaller than a few points across.
                let s = s.max(8.0 / (2.0 * hw.min(hh)).max(1e-3)).max(0.02);
                if let Some(pin) = board.get_mut(*id) {
                    pin.scale = (*scale * s).clamp(MIN_SCALE, MAX_SCALE);
                    pin.center = frame.to_canvas([anchor[0] * (1.0 - s), anchor[1] * (1.0 - s)]);
                }
            }
            Gesture::Rotate { id, start, angle } => {
                let Some(pin) = board.get_mut(*id) else { return };
                let c = pin.center;
                let turn = |p: [f32; 2]| (p[1] - c[1]).atan2(p[0] - c[0]);
                let mut a = *angle + turn(q) - turn(*start);
                if snap {
                    a = (a / ROTATE_SNAP).round() * ROTATE_SNAP;
                }
                pin.angle = a;
            }
        }
    }

    /// End the gesture at `q`.
    pub fn release(&mut self, q: [f32; 2], snap: bool, board: &mut PinBoard) {
        self.drag(q, snap, board);
        self.gesture = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::shape::{BorderStyle, Shape};

    fn rect(a: [f32; 2], b: [f32; 2], pin: u64) -> Shape {
        Shape::Rect { a, b, color: [0; 4], stroke_width: 2.0, border: BorderStyle::Solid, rotation: 0.0, pin, group: 0 }
    }

    fn close(p: [f32; 2], q: [f32; 2]) -> bool {
        (p[0] - q[0]).abs() < 1e-3 && (p[1] - q[1]).abs() < 1e-3
    }

    #[test]
    fn a_pin_maps_its_objects_and_back() {
        let pin = Pin { id: 1, center: [300.0, 200.0], scale: 2.0, angle: std::f32::consts::FRAC_PI_2, local: false };
        let anchor = [10.0, 10.0];
        let m = pin.rigid(anchor);
        let screen = |p: [f32; 2]| {
            let q = m.apply(p);
            [q[0] * pin.scale, q[1] * pin.scale]
        };
        assert!(close(screen(anchor), pin.center), "the objects' centre sits on the pin's");
        // A point right of the centre turns to below it (y down), twice as far.
        assert!(close(screen([15.0, 10.0]), [300.0, 210.0]));
        assert!(close(pin.canvas_point(anchor, [300.0, 210.0]), [15.0, 10.0]));
    }

    #[test]
    fn new_tags_get_a_place_where_they_show_and_untagged_slides_none() {
        let mut c = Canvas { pan: [50.0, 0.0], zoom: 2.0, ..Canvas::default() };
        c.layers[0].shapes.push(rect([0.0, 0.0], [10.0, 10.0], 0));
        c.layers[0].shapes.push(rect([20.0, 0.0], [30.0, 10.0], 7));
        let slides = vec![Some(Canvas::default()), Some(c)];
        let mut board = PinBoard::new(Vec::new());
        board.refresh(&slides);
        let pinned = board.pinned(&slides, 0);
        assert_eq!(pinned.len(), 1);
        let p = pinned[0];
        assert_eq!((p.pin.id, p.slide), (7, 1));
        assert_eq!(p.members, &[PinMember { layer: 0, obj: ObjRef::Shape(1) }]);
        // Box centre (25, 5) at pan 50, zoom 2.
        assert!(close(p.pin.center, [100.0, 10.0]));
        assert_eq!(p.pin.scale, 2.0);
    }

    #[test]
    fn objects_that_move_lists_are_found_again() {
        let mut c = Canvas::default();
        c.layers[0].shapes.push(rect([0.0, 0.0], [10.0, 10.0], 3));
        c.layers.push(Default::default());
        c.layers[1].shapes.push(rect([0.0, 0.0], [10.0, 10.0], 0));
        let mut slides = vec![Some(c)];
        let mut board = PinBoard::new(vec![Pin { id: 3, center: [5.0, 5.0], scale: 1.0, angle: 0.0, local: false }]);
        board.refresh(&slides);
        assert_eq!(board.members(3).unwrap().1, &[PinMember { layer: 0, obj: ObjRef::Shape(0) }]);
        // The two layers trade places: same counts, same revs, so the
        // slide's fingerprint is unchanged — the pin's object is still
        // found on its new layer.
        slides[0].as_mut().unwrap().layers.swap(0, 1);
        board.refresh(&slides);
        assert_eq!(board.members(3).unwrap().1, &[PinMember { layer: 1, obj: ObjRef::Shape(0) }]);
        slides[0].as_mut().unwrap().layers[1].shapes[0].set_pin(0);
        board.refresh(&slides);
        assert!(board.members(3).is_none(), "untagged: nothing left to show");
        assert!(board.pinned(&slides, 0).is_empty());
        // Its place is kept for an undo, but not written out.
        assert!(board.get_mut(3).is_some());
        assert_eq!(board.take_changed(true), Some(Vec::new()));
    }

    #[test]
    fn places_are_written_only_when_they_change_and_all_slides_are_read() {
        let mut c = Canvas::default();
        c.layers[0].shapes.push(rect([0.0, 0.0], [10.0, 10.0], 5));
        let slides = vec![Some(c)];
        let place = Pin { id: 5, center: [1.0, 2.0], scale: 1.0, angle: 0.0, local: false };
        let mut board = PinBoard::new(vec![place]);
        board.refresh(&slides);
        assert_eq!(board.take_changed(true), None, "as read from the file");
        board.get_mut(5).unwrap().center = [9.0, 9.0];
        assert_eq!(board.take_changed(false), None, "slides still loading");
        assert_eq!(board.take_changed(true).unwrap()[0].center, [9.0, 9.0]);
        assert_eq!(board.take_changed(true), None);
    }

    #[test]
    fn hidden_pins_neither_show_nor_grab_and_keep_their_places() {
        let mut c = Canvas::default();
        c.layers[0].shapes.push(rect([0.0, 0.0], [40.0, 40.0], 6));
        let slides = vec![Some(c)];
        let mut board = PinBoard::new(vec![Pin { id: 6, center: [50.0, 50.0], scale: 1.0, angle: 0.0, local: false }]);
        board.refresh(&slides);
        board.set_hidden(true);
        assert!(board.pinned(&slides, 0).is_empty());
        assert!(board.any(0), "still there, just hidden");
        let mut tool = PinTool::default();
        assert_eq!(tool.press([30.0, 50.0], &board.pinned(&slides, 0)), Press::Missed);
        assert_eq!(board.take_changed(true), None, "hiding is not saved");
        board.set_hidden(false);
        assert_eq!(board.pinned(&slides, 0)[0].pin.center, [50.0, 50.0]);
    }

    #[test]
    fn unusable_places_from_the_file_are_dropped() {
        let board = PinBoard::new(vec![
            Pin { id: 1, center: [f32::NAN, 0.0], scale: 1.0, angle: 0.0, local: false },
            Pin { id: 2, center: [0.0, 0.0], scale: 0.0, angle: 0.0, local: false },
            Pin { id: 0, center: [0.0, 0.0], scale: 1.0, angle: 0.0, local: false },
            Pin { id: 3, center: [0.0, 0.0], scale: 1.0, angle: 0.0, local: false },
            Pin { id: 3, center: [5.0, 0.0], scale: 1.0, angle: 0.0, local: false },
        ]);
        assert_eq!(board.pins.iter().map(|p| p.id).collect::<Vec<_>>(), vec![3]);
    }

    #[test]
    fn a_local_pin_shows_and_grabs_only_on_its_own_slide() {
        let mut c = Canvas::default();
        c.layers[0].shapes.push(rect([0.0, 0.0], [40.0, 40.0], 8));
        let slides = vec![Some(Canvas::default()), Some(c)];
        let mut board = PinBoard::new(vec![Pin { id: 8, center: [50.0, 50.0], scale: 1.0, angle: 0.0, local: true }]);
        board.refresh(&slides);
        assert!(board.pinned(&slides, 0).is_empty());
        assert!(!board.any(0), "nothing to hide on the other slide");
        let mut tool = PinTool::default();
        assert_eq!(tool.press([30.0, 50.0], &board.pinned(&slides, 0)), Press::Missed);
        assert_eq!(board.pinned(&slides, 1).len(), 1);
        assert!(board.any(1));
        // Made global, it shows on the other slide too, and that is saved.
        board.get_mut(8).unwrap().local = false;
        assert_eq!(board.pinned(&slides, 0).len(), 1);
        assert!(!board.take_changed(true).unwrap()[0].local);
    }

    #[test]
    fn places_saved_before_local_pins_read_as_global() {
        let old: Vec<Pin> = ron::from_str("[(id: 3, center: (1.0, 2.0), scale: 1.0, angle: 0.0)]").unwrap();
        assert!(!old[0].local);
        let back: Vec<Pin> = ron::from_str(&ron::ser::to_string(&vec![Pin { local: true, ..old[0] }]).unwrap()).unwrap();
        assert!(back[0].local);
    }

    #[test]
    fn dragging_moves_resizes_and_turns_only_the_pin() {
        let mut c = Canvas::default();
        c.layers[0].shapes.push(rect([0.0, 0.0], [100.0, 50.0], 9));
        let slides = vec![Some(c)];
        let mut board = PinBoard::new(vec![Pin { id: 9, center: [200.0, 200.0], scale: 1.0, angle: 0.0, local: false }]);
        board.refresh(&slides);
        let before = format!("{:?}", slides[0].as_ref().unwrap().layers[0].shapes);
        let mut tool = PinTool::default();

        // Press on the rectangle's outline, drag by (30, 10).
        assert_eq!(tool.press([200.0 - 51.0, 200.0], &board.pinned(&slides, 0)), Press::Took);
        tool.release([200.0 - 51.0 + 30.0, 210.0], false, &mut board);
        assert_eq!(board.get_mut(9).unwrap().center, [230.0, 210.0]);
        assert_eq!(tool.selected, Some(9));

        // Bottom-right corner out to twice the size: top-left stays.
        let frame = board.pinned(&slides, 0)[0].frame();
        let [tl, _, br, _] = frame.corners();
        assert_eq!(tool.press(br, &board.pinned(&slides, 0)), Press::Took);
        let far = [tl[0] + 2.0 * (br[0] - tl[0]), tl[1] + 2.0 * (br[1] - tl[1])];
        tool.release(far, false, &mut board);
        let pin = *board.get_mut(9).unwrap();
        assert!((pin.scale - 2.0).abs() < 1e-3);
        assert!(close(board.pinned(&slides, 0)[0].frame().corners()[0], tl));

        // The knob dragged a quarter turn round, snapped.
        let frame = board.pinned(&slides, 0)[0].frame();
        let (_, knob) = rotate_knob(&frame);
        assert_eq!(tool.press(knob, &board.pinned(&slides, 0)), Press::Took);
        let r = (knob[1] - pin.center[1]).abs();
        tool.release([pin.center[0] + r, pin.center[1] + 3.0], true, &mut board);
        assert!((board.get_mut(9).unwrap().angle - std::f32::consts::FRAC_PI_2).abs() < 1e-4);

        assert_eq!(format!("{:?}", slides[0].as_ref().unwrap().layers[0].shapes), before, "the objects never move");
    }

    #[test]
    fn a_quick_second_press_opens_the_pins_slide_and_empty_space_deselects() {
        let mut c = Canvas::default();
        c.layers[0].shapes.push(rect([0.0, 0.0], [40.0, 40.0], 4));
        let slides = vec![Some(c)];
        let mut board = PinBoard::new(vec![Pin { id: 4, center: [50.0, 50.0], scale: 1.0, angle: 0.0, local: false }]);
        board.refresh(&slides);
        let mut tool = PinTool::default();
        let on = [30.0, 50.0];
        assert_eq!(tool.press(on, &board.pinned(&slides, 0)), Press::Took);
        tool.release(on, false, &mut board);
        assert_eq!(tool.press(on, &board.pinned(&slides, 0)), Press::Open(4));
        // The empty middle of an unselected ink pin is not the pin…
        tool.clear();
        assert_eq!(tool.press([50.0, 50.0], &board.pinned(&slides, 0)), Press::Missed);
        // …but once it is selected, its frame is.
        tool.selected = Some(4);
        assert_eq!(tool.press([50.0, 50.0], &board.pinned(&slides, 0)), Press::Took);
        tool.release([50.0, 50.0], false, &mut board);
        assert_eq!(tool.press([400.0, 400.0], &board.pinned(&slides, 0)), Press::Missed);
        assert_eq!(tool.selected, None);
    }
}
