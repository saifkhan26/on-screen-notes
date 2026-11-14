//! Arrange: tidy a selection. Pack it into a compact block, rows or
//! columns that fit the screen, line it up along an edge or centre
//! (overlapping or not), space it evenly, sort it into clusters by media
//! type, or give it one width or height.
//!
//! Pure geometry: each operation takes the objects' boxes and returns
//! how far to move each one (`offsets`, `pack_offsets`) or how much to
//! scale it (`scales`). The app applies the result as one undo step
//! (`Canvas::move_objects_each` / `Canvas::transform_objects_each`).
//! Nothing is rotated.

use serde::{Deserialize, Serialize};

use crate::canvas::affine::Affine;
use crate::canvas::canvas::{shape_bounds, Layer};
use crate::canvas::hit::Selection;
use crate::canvas::shape::MediaType;

/// Axis-aligned box `[x0, y0, x1, y1]`, canvas-local.
pub type Bounds = [f32; 4];

/// Per-object moves: `(index, offset)`, ascending by index.
pub type Moves = Vec<(usize, [f32; 2])>;

/// Per-object transforms: `(index, transform)`, ascending by index.
pub type Transforms = Vec<(usize, Affine)>;

/// The moves `op` makes of the selected strokes and shapes on `layer`.
/// Pictures, GIFs and videos move one by one, each carrying the ink
/// drawn on it; other ink and shapes move in touching clusters.
/// Distances that should look the same at any zoom are screen pixels
/// divided by `zoom`.
pub fn plan(layer: &Layer, sel: &Selection, op: Arrange, zoom: f32) -> (Moves, Moves) {
    let zoom = zoom.max(1e-3);
    let (units, owners) = gather(layer, sel, zoom);
    let items: Vec<Item> = units.iter().map(|u| u.item).collect();
    let offsets = offsets(&items, op, 1.0 / zoom);
    per_object(&units, &owners, &offsets)
}

/// The moves that pack the selected strokes and shapes on `layer` the
/// way `mode` says into canvas box `view` (what the screen shows), and
/// the box the packed block lands in. Objects move in units, as `plan`
/// moves them. See `pack_offsets` for where the block goes.
pub fn plan_pack(layer: &Layer, sel: &Selection, mode: PackMode, view: Bounds, zoom: f32) -> (Moves, Moves, Bounds) {
    let (units, owners) = gather(layer, sel, zoom.max(1e-3));
    let items: Vec<Item> = units.iter().map(|u| u.item).collect();
    let (offsets, block) = pack_offsets(&items, mode, view);
    let (strokes, shapes) = per_object(&units, &owners, &offsets);
    (strokes, shapes, block)
}

/// The transforms that give the selected strokes and shapes on `layer`
/// the size `m` picks. Each unit (see `plan`) scales as a whole, by the
/// same factor across and down, so pictures keep their proportions; its
/// top-left corner stays put.
pub fn plan_sizes(layer: &Layer, sel: &Selection, m: Match, zoom: f32) -> (Transforms, Transforms) {
    let (units, owners) = gather(layer, sel, zoom.max(1e-3));
    let items: Vec<Item> = units.iter().map(|u| u.item).collect();
    let transforms: Vec<Affine> = items
        .iter()
        .zip(scales(&items, m))
        .map(|(it, s)| Affine::scale_about([it.bounds[0], it.bounds[1]], s, s))
        .collect();
    per_object(&units, &owners, &transforms)
}

/// The selected objects gathered into units, and which stroke (`true`)
/// or shape each object `units` refers to is. Objects grouped together
/// (`Canvas::set_group`) are one unit whatever else they touch.
fn gather(layer: &Layer, sel: &Selection, zoom: f32) -> (Vec<Unit>, Vec<(bool, usize)>) {
    // Every selected object as (box, media group), plus which it is and
    // the group it is in.
    let mut objects: Vec<(Bounds, Option<u8>)> = Vec::new();
    let mut owners: Vec<(bool, usize)> = Vec::new();
    let mut groups: Vec<u64> = Vec::new();
    for &i in &sel.strokes {
        let Some(s) = layer.strokes.get(i) else { continue };
        let Some(b) = s.bounds() else { continue };
        let pad = s.base_width * 0.5;
        objects.push(([b[0] - pad, b[1] - pad, b[2] + pad, b[3] + pad], None));
        owners.push((true, i));
        groups.push(s.group);
    }
    for &i in &sel.shapes {
        let Some(s) = layer.shapes.get(i) else { continue };
        let group = s.media_type().map(|t| match t {
            MediaType::Picture => 0,
            MediaType::Gif => 1,
            MediaType::Video => 2,
        });
        objects.push((shape_bounds(s), group));
        owners.push((false, i));
        groups.push(s.group());
    }
    (join_groups(units(&objects, 8.0 / zoom), &groups), owners)
}

/// Merge the units holding objects of one group into one unit, boxed
/// around all of them. `groups` has each object's group (0: none). Its
/// `GroupByType` cluster is the lowest of its parts', so a group with a
/// picture in it goes with the pictures.
fn join_groups(units: Vec<Unit>, groups: &[u64]) -> Vec<Unit> {
    let mut parent: Vec<usize> = (0..units.len()).collect();
    fn root(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    let mut first: std::collections::HashMap<u64, usize> = std::collections::HashMap::new();
    for (u, unit) in units.iter().enumerate() {
        for &m in &unit.members {
            let g = groups.get(m).copied().unwrap_or(0);
            if g == 0 {
                continue;
            }
            match first.get(&g) {
                Some(&v) => {
                    let (a, b) = (root(&mut parent, u), root(&mut parent, v));
                    if a != b {
                        parent[a] = b;
                    }
                }
                None => {
                    first.insert(g, u);
                }
            }
        }
    }
    let mut out: Vec<Unit> = Vec::new();
    let mut slot = vec![usize::MAX; units.len()];
    for (u, unit) in units.into_iter().enumerate() {
        let r = root(&mut parent, u);
        if slot[r] == usize::MAX {
            slot[r] = out.len();
            out.push(unit);
        } else {
            let into = &mut out[slot[r]];
            into.item.bounds = union([into.item.bounds, unit.item.bounds].into_iter());
            into.item.group = into.item.group.min(unit.item.group);
            into.members.extend(unit.members);
        }
    }
    out
}

/// Hand each unit's value to every object in it, split into strokes and
/// shapes, each ascending by index.
fn per_object<T: Copy>(units: &[Unit], owners: &[(bool, usize)], values: &[T]) -> (Vec<(usize, T)>, Vec<(usize, T)>) {
    let (mut strokes, mut shapes) = (Vec::new(), Vec::new());
    for (u, &v) in units.iter().zip(values) {
        for &m in &u.members {
            match owners[m] {
                (true, i) => strokes.push((i, v)),
                (false, i) => shapes.push((i, v)),
            }
        }
    }
    strokes.sort_by_key(|e| e.0);
    shapes.sort_by_key(|e| e.0);
    (strokes, shapes)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    CenterX,
    Right,
    Top,
    CenterY,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arrange {
    /// Pack into a compact block with no gaps, keeping every size.
    Pack,
    /// Pack each group on its own and set the groups side by side.
    GroupByType,
    /// Line every object up with the selection's edge or centre.
    Align(Align),
    /// Slide every object toward the selection's edge or centre until
    /// it gets there or bumps into another one: objects that would
    /// overlap end up edge to edge instead. Each keeps its place on the
    /// other axis, so a row aligned left stays a row. Objects only a
    /// hair into each other's row pass (see `in_way`).
    Slide(Align),
    /// Equal gaps left to right; the outermost two stay put.
    DistributeX,
    /// Equal gaps top to bottom; the outermost two stay put.
    DistributeY,
}

/// How Pack lays the selection out. Every way keeps each object's size
/// and fits the screen when it can (see `pack_offsets`). Picked on the
/// arrange bar and kept in the config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PackMode {
    /// The tightest block, shaped like the screen; order not kept.
    #[default]
    Optimal,
    /// Rows: left to right in the objects' reading order, edge to edge,
    /// a new row where the next object would cross the screen's edge.
    Horizontal,
    /// Columns: top to bottom, a new column at the screen's bottom.
    Vertical,
}

impl PackMode {
    pub const ALL: [PackMode; 3] = [PackMode::Optimal, PackMode::Horizontal, PackMode::Vertical];

    pub fn label(self) -> &'static str {
        match self {
            PackMode::Optimal => "Optimal",
            PackMode::Horizontal => "Horizontal",
            PackMode::Vertical => "Vertical",
        }
    }

    /// What packing this way does, for tooltips.
    pub fn about(self) -> &'static str {
        match self {
            PackMode::Optimal => "the tightest block, shaped like the screen",
            PackMode::Horizontal => "rows, left to right in reading order, wrapping at the screen's edge",
            PackMode::Vertical => "columns, top to bottom in reading order, wrapping at the screen's bottom",
        }
    }
}

/// The width or height every object takes from the selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Match {
    /// The narrowest object's width.
    NarrowestWidth,
    /// The shortest object's height.
    ShortestHeight,
    /// The widest object's width.
    WidestWidth,
    /// The tallest object's height.
    TallestHeight,
}

/// Each item's scale factor for `m`, in `items` order. Items with no
/// extent on that axis (a straight line along the other one) keep their
/// size and set none.
pub fn scales(items: &[Item], m: Match) -> Vec<f32> {
    let ax = match m {
        Match::NarrowestWidth | Match::WidestWidth => 0,
        Match::ShortestHeight | Match::TallestHeight => 1,
    };
    let len = |it: &Item| it.bounds[ax + 2] - it.bounds[ax];
    let lens = items.iter().map(len).filter(|&l| l > EPS);
    let target = match m {
        Match::NarrowestWidth | Match::ShortestHeight => lens.fold(f32::INFINITY, f32::min),
        Match::WidestWidth | Match::TallestHeight => lens.fold(0.0, f32::max),
    };
    if items.len() < 2 || !target.is_finite() || target <= EPS {
        return vec![1.0; items.len()];
    }
    items.iter().map(|it| if len(it) > EPS { target / len(it) } else { 1.0 }).collect()
}

impl Arrange {
    /// Fewest objects the operation does anything with.
    pub fn min_items(self) -> usize {
        match self {
            Arrange::DistributeX | Arrange::DistributeY => 3,
            _ => 2,
        }
    }
}

/// One object to arrange.
#[derive(Debug, Clone, Copy)]
pub struct Item {
    pub bounds: Bounds,
    /// Cluster for `GroupByType`; lower groups go further left.
    pub group: u8,
}

/// Objects that Arrange moves as one: a picture / GIF / video with the
/// ink drawn on it, or a cluster of touching ink and shapes. Moving
/// strokes one by one would pull handwriting apart.
#[derive(Debug, Clone)]
pub struct Unit {
    pub item: Item,
    /// Indices into the object list given to `units`.
    pub members: Vec<usize>,
}

/// Gather `objects` into units. Each object is its box plus, for media,
/// its group. A non-media object whose centre lies on a media object
/// rides along with it (the smallest one, if several); the rest are
/// clustered by boxes that touch, within `tol`. A media unit is sized
/// by the media alone, so pictures pack edge to edge.
pub fn units(objects: &[(Bounds, Option<u8>)], tol: f32) -> Vec<Unit> {
    let area = |b: Bounds| (b[2] - b[0]) * (b[3] - b[1]);
    let mut out: Vec<Unit> = Vec::new();
    let mut unit_of_media = vec![usize::MAX; objects.len()];
    for (i, (b, g)) in objects.iter().enumerate() {
        if let Some(g) = g {
            unit_of_media[i] = out.len();
            out.push(Unit { item: Item { bounds: *b, group: *g }, members: vec![i] });
        }
    }
    let mut loose = Vec::new();
    for (i, (b, g)) in objects.iter().enumerate() {
        if g.is_some() { continue; }
        let c = [(b[0] + b[2]) * 0.5, (b[1] + b[3]) * 0.5];
        let host = objects
            .iter()
            .enumerate()
            .filter(|(_, (m, g))| g.is_some() && m[0] <= c[0] && c[0] <= m[2] && m[1] <= c[1] && c[1] <= m[3])
            .min_by(|(_, (p, _)), (_, (q, _))| area(*p).total_cmp(&area(*q)))
            .map(|(m, _)| m);
        match host {
            Some(m) => out[unit_of_media[m]].members.push(i),
            None => loose.push(i),
        }
    }
    // Loose objects joined by chains of touching boxes (union-find).
    let touch = |a: Bounds, b: Bounds| a[0] <= b[2] + tol && b[0] <= a[2] + tol && a[1] <= b[3] + tol && b[1] <= a[3] + tol;
    let mut parent: Vec<usize> = (0..loose.len()).collect();
    fn root(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    for a in 0..loose.len() {
        for b in a + 1..loose.len() {
            if touch(objects[loose[a]].0, objects[loose[b]].0) {
                let (ra, rb) = (root(&mut parent, a), root(&mut parent, b));
                if ra != rb { parent[rb] = ra; }
            }
        }
    }
    let mut cluster_of_root = vec![usize::MAX; loose.len()];
    for k in 0..loose.len() {
        let r = root(&mut parent, k);
        let i = loose[k];
        if cluster_of_root[r] == usize::MAX {
            cluster_of_root[r] = out.len();
            out.push(Unit { item: Item { bounds: objects[i].0, group: u8::MAX }, members: vec![i] });
        } else {
            let u = &mut out[cluster_of_root[r]];
            u.item.bounds = union([u.item.bounds, objects[i].0].into_iter());
            u.members.push(i);
        }
    }
    out
}

/// How far to move each item, in `items` order. `px` is one screen
/// pixel in canvas units: it sizes the space `GroupByType` leaves
/// between clusters, and how far into each other two boxes must reach
/// for `Slide` to count them as in each other's way (see `in_way`).
pub fn offsets(items: &[Item], op: Arrange, px: f32) -> Vec<[f32; 2]> {
    let mut out = vec![[0.0, 0.0]; items.len()];
    if items.len() < op.min_items() { return out; }
    let gap = 40.0 * px;
    let all = union(items.iter().map(|it| it.bounds));
    match op {
        Arrange::Pack => {
            let idx: Vec<usize> = (0..items.len()).collect();
            let (pos, _) = pack(&sizes(items, &idx));
            for (k, &i) in idx.iter().enumerate() {
                out[i] = to(items[i].bounds, [all[0] + pos[k][0], all[1] + pos[k][1]]);
            }
        }
        Arrange::GroupByType => {
            let mut groups: Vec<u8> = items.iter().map(|it| it.group).collect();
            groups.sort_unstable();
            groups.dedup();
            let mut x = all[0];
            for g in groups {
                let idx: Vec<usize> = (0..items.len()).filter(|&i| items[i].group == g).collect();
                let (pos, extent) = pack(&sizes(items, &idx));
                for (k, &i) in idx.iter().enumerate() {
                    out[i] = to(items[i].bounds, [x + pos[k][0], all[1] + pos[k][1]]);
                }
                x += extent[0] + gap;
            }
        }
        Arrange::Align(a) => {
            for (o, it) in out.iter_mut().zip(items) {
                let b = it.bounds;
                *o = match a {
                    Align::Left => [all[0] - b[0], 0.0],
                    Align::CenterX => [(all[0] + all[2] - b[0] - b[2]) * 0.5, 0.0],
                    Align::Right => [all[2] - b[2], 0.0],
                    Align::Top => [0.0, all[1] - b[1]],
                    Align::CenterY => [0.0, (all[1] + all[3] - b[1] - b[3]) * 0.5],
                    Align::Bottom => [0.0, all[3] - b[3]],
                };
            }
        }
        Arrange::Slide(a) => {
            // Along axis `ax`; boxes are in each other's way when they
            // share some of the other axis `oth`.
            let (ax, oth) = match a {
                Align::Left | Align::CenterX | Align::Right => (0, 1),
                Align::Top | Align::CenterY | Align::Bottom => (1, 0),
            };
            let line = match a {
                Align::Left | Align::Top => all[ax],
                Align::Right | Align::Bottom => all[ax + 2],
                Align::CenterX | Align::CenterY => (all[ax] + all[ax + 2]) * 0.5,
            };
            let len = |b: Bounds| b[ax + 2] - b[ax];
            // Where a box's near edge goes with nothing in the way, and
            // whether getting there is a move up the axis.
            let goal = |b: Bounds| match a {
                Align::Left | Align::Top => line,
                Align::Right | Align::Bottom => line - len(b),
                Align::CenterX | Align::CenterY => line - len(b) * 0.5,
            };
            let rising = |b: Bounds| match a {
                Align::Left | Align::Top => false,
                Align::Right | Align::Bottom => true,
                Align::CenterX | Align::CenterY => b[ax] + b[ax + 2] < 2.0 * line,
            };
            // Nearest the line first: it gets there, and the rest pile
            // up behind it.
            let mut order: Vec<usize> = (0..items.len()).collect();
            let dist = |i: usize| (goal(items[i].bounds) - items[i].bounds[ax]).abs();
            order.sort_by(|&p, &q| dist(p).total_cmp(&dist(q)));
            let mut placed: Vec<Bounds> = Vec::with_capacity(items.len());
            for i in order {
                let b = items[i].bounds;
                let ahead = placed.iter().filter(|p| in_way(**p, b, oth, px));
                // Stop short of every placed box in the way, which also
                // pushes a box out of one it already overlapped.
                let at = if rising(b) {
                    ahead.fold(goal(b) + len(b), |end, p| end.min(p[ax])) - len(b)
                } else {
                    ahead.fold(goal(b), |start, p| start.max(p[ax + 2]))
                };
                out[i][ax] = at - b[ax];
                let mut now = b;
                now[ax] = at;
                now[ax + 2] = at + len(b);
                placed.push(now);
            }
        }
        Arrange::DistributeX | Arrange::DistributeY => {
            let ax = if op == Arrange::DistributeX { 0 } else { 1 };
            let b = |i: usize| items[i].bounds;
            let len = |i: usize| b(i)[ax + 2] - b(i)[ax];
            let mut order: Vec<usize> = (0..items.len()).collect();
            order.sort_by(|&p, &q| (b(p)[ax] + b(p)[ax + 2]).total_cmp(&(b(q)[ax] + b(q)[ax + 2])));
            let first = b(order[0])[ax];
            let last = b(order[order.len() - 1])[ax + 2];
            let total: f32 = order.iter().map(|&i| len(i)).sum();
            let step = (last - first - total) / (order.len() - 1) as f32;
            let mut at = first;
            for &i in &order {
                out[i][ax] = at - b(i)[ax];
                at += len(i) + step;
            }
        }
    }
    out
}

/// Whether boxes `a` and `b` block each other when one slides along the
/// axis other than `oth`: they share more of axis `oth` than a hair.
/// A hair is four screen pixels (`px` is one) or 4% of the smaller box
/// on that axis, whichever is more — boards laid out edge to edge, such
/// as PureRef's, overlap their neighbours by that much — but never over
/// a quarter of it, so a thin line still blocks.
pub(crate) fn in_way(a: Bounds, b: Bounds, oth: usize, px: f32) -> bool {
    let shared = a[oth + 2].min(b[oth + 2]) - a[oth].max(b[oth]);
    let small = (a[oth + 2] - a[oth]).min(b[oth + 2] - b[oth]);
    shared > (4.0 * px).max(0.04 * small).min(0.25 * small)
}

/// How far to move each item to pack them the way `mode` says into
/// canvas box `view`, in `items` order, and the box the packed block
/// lands in. Sizes are kept.
///
/// A block that fits `view` stays as close to where the selection was
/// as it can while staying inside it. One that cannot fit is shaped like
/// `view` (for rows and columns: wrapping at `view`'s edge scaled up as
/// little as needed) and centred on it, for the app to zoom out to.
pub fn pack_offsets(items: &[Item], mode: PackMode, view: Bounds) -> (Vec<[f32; 2]>, Bounds) {
    let all = union(items.iter().map(|it| it.bounds));
    if items.len() < Arrange::Pack.min_items() {
        return (vec![[0.0, 0.0]; items.len()], all);
    }
    let room = [(view[2] - view[0]).max(EPS), (view[3] - view[1]).max(EPS)];
    let idx: Vec<usize> = (0..items.len()).collect();
    let sz = sizes(items, &idx);
    let (pos, extent) = match mode {
        PackMode::Optimal => pack_shaped(&sz, room[0] / room[1], Some(room)),
        PackMode::Horizontal => lines(&sz, &reading_order(items, 0), room, 0),
        PackMode::Vertical => lines(&sz, &reading_order(items, 1), room, 1),
    };
    let fits = within(extent, room, 1.0);
    let at = |a: usize| {
        if fits {
            all[a].min(view[a + 2] - extent[a]).max(view[a])
        } else {
            (view[a] + view[a + 2] - extent[a]) * 0.5
        }
    };
    let top_left = [at(0), at(1)];
    let out = idx.iter().map(|&i| to(items[i].bounds, [top_left[0] + pos[i][0], top_left[1] + pos[i][1]])).collect();
    (out, [top_left[0], top_left[1], top_left[0] + extent[0], top_left[1] + extent[1]])
}

/// Whether a block of size `extent` fits in `room` scaled by `s`, give
/// or take f32 rounding.
fn within(extent: [f32; 2], room: [f32; 2], s: f32) -> bool {
    (0..2).all(|a| extent[a] <= s * room[a] * (1.0 + 1e-4) + EPS)
}

/// `items` in reading order for lines along axis `ax` (0: rows, 1:
/// columns): the first line first — the top row, or the left column —
/// and along each line from its start. Two items share a line when they
/// overlap across it by half the thinner one or more, so a row a hair
/// into the next (PureRef boards) still reads as two.
fn reading_order(items: &[Item], ax: usize) -> Vec<usize> {
    let oth = 1 - ax;
    let b = |i: usize| items[i].bounds;
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by(|&p, &q| b(p)[oth].total_cmp(&b(q)[oth]).then(b(p)[ax].total_cmp(&b(q)[ax])));
    let mut out = Vec::with_capacity(items.len());
    let mut line: Vec<usize> = Vec::new();
    // The nearest far edge of the line so far, and its thinnest item:
    // a newcomer must overlap every item already in it.
    let (mut end, mut thin) = (0.0_f32, 0.0_f32);
    let flush = |line: &mut Vec<usize>, out: &mut Vec<usize>| {
        line.sort_by(|&p, &q| b(p)[ax].total_cmp(&b(q)[ax]));
        out.append(line);
    };
    for i in order {
        let (start, len) = (b(i)[oth], b(i)[oth + 2] - b(i)[oth]);
        if !line.is_empty() && end - start < 0.5 * len.min(thin) {
            flush(&mut line, &mut out);
        }
        if line.is_empty() {
            (end, thin) = (b(i)[oth + 2], len);
        } else {
            (end, thin) = (end.min(b(i)[oth + 2]), thin.min(len));
        }
        line.push(i);
    }
    flush(&mut line, &mut out);
    out
}

/// Lay boxes of `sizes` out in lines along axis `ax` (0: rows, left to
/// right; 1: columns, top to bottom), in `order`, edge to edge. A line
/// is as deep as its deepest box and wraps where the next box would
/// reach past `room` along `ax` — scaled up as little as needed for the
/// whole block to fit `room` scaled alike. Returns each box's top-left
/// relative to the block's, and the block's size.
fn lines(sizes: &[[f32; 2]], order: &[usize], room: [f32; 2], ax: usize) -> (Vec<[f32; 2]>, [f32; 2]) {
    let oth = 1 - ax;
    let fits = |s: f32| within(wrap(sizes, order, s * room[ax], ax).1, room, s);
    let s = if fits(1.0) {
        1.0
    } else {
        // All on one line fits at this scale.
        let sum: f32 = sizes.iter().map(|s| s[ax]).sum();
        let deepest = sizes.iter().map(|s| s[oth]).fold(0.0, f32::max);
        let hi = (sum / room[ax]).max(deepest / room[oth]).max(1.0) * 1.001;
        // The first of a run of scales from 1 to `hi` that fits, then
        // narrowed down between it and the one before. Wrapping is not
        // quite monotone, so this finds a small scale that fits rather
        // than the smallest.
        const STEPS: usize = 32;
        let (mut lo, mut found) = (1.0_f32, hi);
        for k in 1..=STEPS {
            let s = hi.powf(k as f32 / STEPS as f32);
            if fits(s) {
                found = s;
                break;
            }
            lo = s;
        }
        for _ in 0..24 {
            let mid = (lo + found) * 0.5;
            if fits(mid) { found = mid } else { lo = mid }
        }
        found
    };
    wrap(sizes, order, s * room[ax], ax)
}

/// `lines` at one wrap length `len`.
fn wrap(sizes: &[[f32; 2]], order: &[usize], len: f32, ax: usize) -> (Vec<[f32; 2]>, [f32; 2]) {
    let oth = 1 - ax;
    let mut pos = vec![[0.0, 0.0]; sizes.len()];
    let mut extent = [0.0_f32, 0.0_f32];
    // Where the next box goes along the line, where the line starts
    // across it, and how deep the line is so far.
    let (mut along, mut line, mut depth) = (0.0_f32, 0.0_f32, 0.0_f32);
    for &i in order {
        let s = sizes[i];
        if along > 0.0 && along + s[ax] > len * (1.0 + 1e-5) + EPS {
            line += depth;
            along = 0.0;
            depth = 0.0;
        }
        pos[i][ax] = along;
        pos[i][oth] = line;
        along += s[ax];
        depth = depth.max(s[oth]);
        extent[ax] = extent[ax].max(along);
        extent[oth] = extent[oth].max(line + depth);
    }
    (pos, extent)
}

fn union(boxes: impl Iterator<Item = Bounds>) -> Bounds {
    boxes.fold([f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY], |a, b| {
        [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]
    })
}

fn sizes(items: &[Item], idx: &[usize]) -> Vec<[f32; 2]> {
    idx.iter()
        .map(|&i| {
            let b = items[i].bounds;
            [(b[2] - b[0]).max(0.0), (b[3] - b[1]).max(0.0)]
        })
        .collect()
}

/// The offset that moves box `b` so its top-left lands on `top_left`.
fn to(b: Bounds, top_left: [f32; 2]) -> [f32; 2] {
    [top_left[0] - b[0], top_left[1] - b[1]]
}

/// Shape a packed block aims for: a little wider than tall, like a
/// screen.
const TARGET_ASPECT: f32 = 16.0 / 10.0;
const EPS: f32 = 1e-3;

/// Pack boxes of `sizes` into a compact block with no gaps between
/// neighbours, keeping every size. Returns each box's top-left relative
/// to the block's, and the block's size.
///
/// Runs a skyline packer at a range of block widths and keeps the layout
/// that wastes least space while staying near `TARGET_ASPECT`.
pub fn pack(sizes: &[[f32; 2]]) -> (Vec<[f32; 2]>, [f32; 2]) {
    pack_shaped(sizes, TARGET_ASPECT, None)
}

/// Each box's top-left relative to its block's, and the block's size.
type Layout = (Vec<[f32; 2]>, [f32; 2]);

/// `pack`, aiming for blocks `aspect` times as wide as tall. Given a
/// `room`, a layout that fits inside it beats any that does not.
fn pack_shaped(sizes: &[[f32; 2]], aspect: f32, room: Option<[f32; 2]>) -> Layout {
    if sizes.is_empty() { return (Vec::new(), [0.0, 0.0]); }
    // Tallest first, so each row starts with the boxes that set its
    // height and the shorter ones fill in beside them.
    let mut order: Vec<usize> = (0..sizes.len()).collect();
    order.sort_by(|&a, &b| sizes[b][1].total_cmp(&sizes[a][1]).then(sizes[b][0].total_cmp(&sizes[a][0])));
    let widest = sizes.iter().map(|s| s[0]).fold(0.0, f32::max);
    let sum_w: f32 = sizes.iter().map(|s| s[0]).sum();
    let area: f32 = sizes.iter().map(|s| s[0] * s[1]).sum();
    let ideal = (area * aspect).sqrt();
    const TRIES: usize = 24;
    // Widths from 0.6× to 2× the ideal, and the room's own width.
    let widths = (0..TRIES)
        .map(|k| ideal * 0.6 * (2.0_f32 / 0.6).powf(k as f32 / (TRIES - 1) as f32))
        .chain(room.map(|r| r[0]));
    // Whether it fits, its score, and the layout.
    let mut best: Option<(bool, f32, Layout)> = None;
    for width in widths {
        // Never narrower than the widest box nor wider than all of them
        // side by side.
        let width = width.min(sum_w).max(widest);
        let (pos, extent) = skyline(sizes, &order, width);
        let shape = extent[0].max(EPS) / extent[1].max(EPS);
        let score = (extent[0] * extent[1]).max(EPS) * (1.0 + 0.35 * (shape / aspect).ln().abs());
        let fits = room.map_or(true, |r| within(extent, r, 1.0));
        if best.as_ref().map_or(true, |b| (fits && !b.0) || (fits == b.0 && score < b.1)) {
            best = Some((fits, score, (pos, extent)));
        }
    }
    best.expect("at least one try").2
}

/// Bottom-left skyline packing of the boxes in `order` into a strip
/// `width` wide (at least as wide as the widest box).
fn skyline(sizes: &[[f32; 2]], order: &[usize], width: f32) -> (Vec<[f32; 2]>, [f32; 2]) {
    // The upper edge of everything placed so far, as `(x, y, width)`
    // segments, left to right.
    let mut sky: Vec<(f32, f32, f32)> = vec![(0.0, 0.0, width)];
    let mut pos = vec![[0.0, 0.0]; sizes.len()];
    let mut extent = [0.0_f32, 0.0_f32];
    for &i in order {
        let [w, h] = sizes[i];
        // The lowest spot that fits, leftmost on ties.
        let mut best: Option<(f32, f32)> = None;
        for s in 0..sky.len() {
            let x = sky[s].0;
            if x + w > width + EPS { break; }
            let (mut y, mut covered, mut t) = (sky[s].1, sky[s].2, s + 1);
            while covered < w - EPS && t < sky.len() {
                y = y.max(sky[t].1);
                covered += sky[t].2;
                t += 1;
            }
            if best.map_or(true, |(by, bx)| y < by - EPS || ((y - by).abs() <= EPS && x < bx)) {
                best = Some((y, x));
            }
        }
        let (y, x) = best.unwrap_or((extent[1], 0.0));
        pos[i] = [x, y];
        extent = [extent[0].max(x + w), extent[1].max(y + h)];
        raise(&mut sky, x, w, y + h);
    }
    (pos, extent)
}

/// Put a box's top edge `[x, x + w)` at height `top` into the skyline.
fn raise(sky: &mut Vec<(f32, f32, f32)>, x: f32, w: f32, top: f32) {
    if w <= EPS { return; }
    let end = x + w;
    let mut next = Vec::with_capacity(sky.len() + 2);
    for &(sx, sy, sw) in sky.iter() {
        let se = sx + sw;
        if se <= x + EPS || sx >= end - EPS {
            next.push((sx, sy, sw));
            continue;
        }
        if sx < x - EPS { next.push((sx, sy, x - sx)); }
        if se > end + EPS { next.push((end, sy, se - end)); }
    }
    next.push((x, top, w));
    next.sort_by(|a, b| a.0.total_cmp(&b.0));
    sky.clear();
    for s in next {
        match sky.last_mut() {
            Some(last) if (last.1 - s.1).abs() <= EPS => last.2 += s.2,
            _ => sky.push(s),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(x: f32, y: f32, w: f32, h: f32, group: u8) -> Item {
        Item { bounds: [x, y, x + w, y + h], group }
    }

    fn moved(items: &[Item], off: &[[f32; 2]]) -> Vec<Bounds> {
        items.iter().zip(off).map(|(it, d)| {
            let b = it.bounds;
            [b[0] + d[0], b[1] + d[1], b[2] + d[0], b[3] + d[1]]
        }).collect()
    }

    fn overlap(a: Bounds, b: Bounds) -> bool {
        a[0] < b[2] - EPS && b[0] < a[2] - EPS && a[1] < b[3] - EPS && b[1] < a[3] - EPS
    }

    #[test]
    fn pack_never_overlaps_and_stays_compact() {
        // A scattered mix of sizes, like a reference board.
        let sizes = [(300.0, 200.0), (120.0, 400.0), (500.0, 280.0), (90.0, 90.0), (260.0, 180.0),
                     (200.0, 340.0), (150.0, 100.0), (400.0, 220.0), (80.0, 160.0), (330.0, 330.0)];
        let items: Vec<Item> = sizes.iter().enumerate()
            .map(|(i, &(w, h))| item(i as f32 * 700.0, (i % 3) as f32 * 900.0, w, h, 0))
            .collect();
        let after = moved(&items, &offsets(&items, Arrange::Pack, 1.0));
        for i in 0..after.len() {
            for j in i + 1..after.len() {
                assert!(!overlap(after[i], after[j]), "{i} and {j} overlap");
            }
        }
        let block = union(after.iter().copied());
        let area: f32 = sizes.iter().map(|(w, h)| w * h).sum();
        let used = (block[2] - block[0]) * (block[3] - block[1]);
        assert!(used < area * 1.6, "block wastes too much: {used} for {area}");
        // It starts where the selection did.
        assert_eq!([block[0], block[1]], [0.0, 0.0]);
    }

    fn no_overlaps(after: &[Bounds]) {
        for i in 0..after.len() {
            for j in i + 1..after.len() {
                assert!(!overlap(after[i], after[j]), "{i} {:?} and {j} {:?} overlap", after[i], after[j]);
            }
        }
    }

    fn inside(b: Bounds, view: Bounds) -> bool {
        b[0] >= view[0] - 1e-3 && b[1] >= view[1] - 1e-3 && b[2] <= view[2] + 1e-3 && b[3] <= view[3] + 1e-3
    }

    #[test]
    fn pack_rows_keep_reading_order_and_wrap_at_the_screen_edge() {
        // Two rows of three, the second listed first; packed into a
        // screen 250 wide: rows of two, in reading order.
        let items = [
            item(0.0, 500.0, 100.0, 50.0, 0),   // row 2, first
            item(200.0, 500.0, 100.0, 50.0, 0), // row 2, second
            item(400.0, 0.0, 100.0, 60.0, 0),   // row 1, third
            item(0.0, 0.0, 100.0, 50.0, 0),     // row 1, first
            item(200.0, 10.0, 100.0, 50.0, 0),  // row 1, second (a little lower)
        ];
        assert_eq!(reading_order(&items, 0), vec![3, 4, 2, 0, 1]);
        let view = [0.0, 0.0, 250.0, 1000.0];
        let (off, block) = pack_offsets(&items, PackMode::Horizontal, view);
        let after = moved(&items, &off);
        no_overlaps(&after);
        let tl = |i: usize| [after[i][0], after[i][1]];
        assert_eq!(tl(3), [0.0, 0.0]);
        assert_eq!(tl(4), [100.0, 0.0]);
        // A row is as tall as its tallest object.
        assert_eq!(tl(2), [0.0, 50.0]);
        assert_eq!(tl(0), [100.0, 50.0]);
        assert_eq!(tl(1), [0.0, 110.0]);
        assert_eq!(block, [0.0, 0.0, 200.0, 160.0]);
    }

    #[test]
    fn pack_columns_wrap_at_the_screen_bottom() {
        let items: Vec<Item> = (0..5).map(|i| item(i as f32 * 300.0, 0.0, 50.0, 100.0, 0)).collect();
        let view = [0.0, 0.0, 1000.0, 250.0];
        let (off, block) = pack_offsets(&items, PackMode::Vertical, view);
        let after = moved(&items, &off);
        no_overlaps(&after);
        // Left to right becomes top to bottom, two to a column.
        let tl: Vec<[f32; 2]> = after.iter().map(|b| [b[0], b[1]]).collect();
        assert_eq!(tl, vec![[0.0, 0.0], [0.0, 100.0], [50.0, 0.0], [50.0, 100.0], [100.0, 0.0]]);
        assert!(inside(block, view));
    }

    #[test]
    fn a_pack_that_fits_stays_on_screen_near_where_it_was() {
        let sizes = [(300.0, 200.0), (120.0, 400.0), (500.0, 280.0), (90.0, 90.0), (260.0, 180.0), (200.0, 340.0)];
        // Scattered right of the screen and below it.
        let items: Vec<Item> = sizes.iter().enumerate()
            .map(|(i, &(w, h))| item(1500.0 + i as f32 * 400.0, 900.0 + (i % 2) as f32 * 500.0, w, h, 0))
            .collect();
        let view = [0.0, 0.0, 2000.0, 1200.0];
        for mode in PackMode::ALL {
            let (off, block) = pack_offsets(&items, mode, view);
            let after = moved(&items, &off);
            no_overlaps(&after);
            assert!(inside(block, view), "{mode:?}: {block:?} is off screen");
            for b in &after {
                assert!(inside(*b, block), "{mode:?}: {b:?} outside its block {block:?}");
            }
            // Pulled in only as far as needed: against the right and
            // bottom edges.
            assert!((block[2] - view[2]).abs() < 1e-3 && (block[3] - view[3]).abs() < 1e-3, "{mode:?}: {block:?}");
        }
        // Already on screen: nothing moves the block's corner.
        let items: Vec<Item> = sizes.iter().enumerate().map(|(i, &(w, h))| item(100.0 + i as f32 * 10.0, 50.0, w, h, 0)).collect();
        let (_, block) = pack_offsets(&items, PackMode::Optimal, view);
        assert_eq!([block[0], block[1]], [100.0, 50.0]);
    }

    #[test]
    fn a_pack_too_big_for_the_screen_is_shaped_like_it_and_centred() {
        // Twenty 100 × 100 squares on a 400 × 200 screen: room for eight.
        let items: Vec<Item> = (0..20).map(|i| item(i as f32 * 150.0, 0.0, 100.0, 100.0, 0)).collect();
        let view = [0.0, 0.0, 400.0, 200.0];
        for mode in PackMode::ALL {
            let (off, block) = pack_offsets(&items, mode, view);
            let after = moved(&items, &off);
            no_overlaps(&after);
            let (w, h) = (block[2] - block[0], block[3] - block[1]);
            // Zoomed out to fit, the block fills the screen well.
            let s = (w / 400.0).max(h / 200.0);
            assert!(s > 1.0, "{mode:?}");
            assert!(w * h >= 0.6 * s * s * 400.0 * 200.0, "{mode:?}: {w} × {h} wastes the screen (scale {s})");
            let c = [(block[0] + block[2]) * 0.5, (block[1] + block[3]) * 0.5];
            assert!((c[0] - 200.0).abs() < 1e-3 && (c[1] - 100.0).abs() < 1e-3, "{mode:?}: centred at {c:?}");
        }
        // Rows of a screen twice as wide as tall: 7 × 3 holds twenty.
        let (_, block) = pack_offsets(&items, PackMode::Horizontal, view);
        assert_eq!((block[2] - block[0], block[3] - block[1]), (700.0, 300.0));
    }

    #[test]
    fn group_by_type_puts_each_group_in_its_own_cluster() {
        let items = [
            item(0.0, 0.0, 100.0, 100.0, 2),
            item(500.0, 0.0, 100.0, 100.0, 0),
            item(900.0, 300.0, 100.0, 100.0, 2),
            item(50.0, 600.0, 100.0, 100.0, 1),
        ];
        let after = moved(&items, &offsets(&items, Arrange::GroupByType, 1.0));
        // Group 0 leftmost, then 1, then 2, a gap between each.
        assert_eq!(after[1][0], 0.0);
        assert_eq!(after[3][0], 140.0);
        assert!(after[0][0] >= 280.0 && after[2][0] >= 280.0);
        assert!(!overlap(after[0], after[2]));
    }

    #[test]
    fn ink_rides_with_its_picture_and_touching_ink_stays_together() {
        let objects = [
            ([0.0, 0.0, 100.0, 100.0], Some(0)),     // picture
            ([20.0, 20.0, 40.0, 30.0], None),        // note on the picture
            ([300.0, 0.0, 340.0, 20.0], None),       // a word…
            ([338.0, 2.0, 380.0, 22.0], None),       // …and the next, touching
            ([600.0, 0.0, 620.0, 20.0], None),       // ink on its own
            ([700.0, 0.0, 800.0, 50.0], Some(2)),    // video
        ];
        let mut u = units(&objects, 4.0);
        u.sort_by_key(|u| u.members[0]);
        let members: Vec<Vec<usize>> = u.iter().map(|u| u.members.clone()).collect();
        assert_eq!(members, vec![vec![0, 1], vec![2, 3], vec![4], vec![5]]);
        assert_eq!(u[0].item.bounds, [0.0, 0.0, 100.0, 100.0], "sized by the picture alone");
        assert_eq!(u[1].item.bounds, [300.0, 0.0, 380.0, 22.0]);
        assert_eq!(u[3].item.group, 2);
    }

    /// Arrange a whole real slide. Run with `OSN_DATA_DIR` pointing at a
    /// *copy* of the data folder: slide `OSN_TEST_SLIDE` (default 0) is
    /// read, packed and grouped, and the results are written back as the
    /// next two slides for a look in the app.
    #[test]
    #[ignore]
    fn arrange_a_real_slide() {
        // It writes slides: never into the real data folder.
        assert!(
            std::env::var_os("OSN_DATA_DIR").is_some_and(|d| !d.is_empty()),
            "set OSN_DATA_DIR to a copy of the data folder; this test overwrites slides"
        );
        let index: usize = std::env::var("OSN_TEST_SLIDE").ok().and_then(|s| s.parse().ok()).unwrap_or(0);
        let crate::persistence::LoadOutcome::Loaded(canvas) = crate::persistence::load_canvas(index, 0, &Default::default()) else {
            panic!("could not load slide {index}")
        };
        let li = canvas.active_layer;
        let layer = &canvas.layers[li];
        let sel = Selection { strokes: (0..layer.strokes.len()).collect(), shapes: (0..layer.shapes.len()).collect() };
        for (k, op) in [Arrange::Pack, Arrange::GroupByType].into_iter().enumerate() {
            let t = std::time::Instant::now();
            let (strokes, shapes) = plan(layer, &sel, op, canvas.zoom);
            let planned = t.elapsed();
            let mut out = canvas.clone();
            out.move_objects_each(li, &strokes, &shapes);
            let media: Vec<Bounds> = out.layers[li].shapes.iter().filter(|s| s.media_type().is_some()).map(shape_bounds).collect();
            // f32 rounding at real canvas coordinates leaves edges a
            // hair (~0.001 px) apart or into each other.
            let shrink = |b: Bounds| [b[0] + 0.01, b[1] + 0.01, b[2] - 0.01, b[3] - 0.01];
            for i in 0..media.len() {
                for j in i + 1..media.len() {
                    assert!(!overlap(shrink(media[i]), shrink(media[j])), "{op:?}: media {i} {:?} and {j} {:?} overlap", media[i], media[j]);
                }
            }
            let block = union(media.iter().copied());
            println!("{op:?}: {} objects planned in {planned:?}; block {:.0} × {:.0}", sel.strokes.len() + sel.shapes.len(), block[2] - block[0], block[3] - block[1]);
            crate::persistence::save_canvas(index + 1 + k, &out).expect("save");
        }
    }

    /// Keep-apart aligning and size matching on a whole real slide, set
    /// up like `arrange_a_real_slide`: slide `OSN_TEST_SLIDE` is read and
    /// each result is written back as one of the next five slides, then
    /// the slide as it was as the sixth, each viewed whole in a
    /// 1200 × 800 window (what a fresh `OSN_DATA_DIR` opens).
    #[test]
    #[ignore]
    fn slide_and_match_a_real_slide() {
        assert!(
            std::env::var_os("OSN_DATA_DIR").is_some_and(|d| !d.is_empty()),
            "set OSN_DATA_DIR to a copy of the data folder; this test overwrites slides"
        );
        let index: usize = std::env::var("OSN_TEST_SLIDE").ok().and_then(|s| s.parse().ok()).unwrap_or(0);
        let crate::persistence::LoadOutcome::Loaded(canvas) = crate::persistence::load_canvas(index, 0, &Default::default()) else {
            panic!("could not load slide {index}")
        };
        let li = canvas.active_layer;
        let layer = &canvas.layers[li];
        let sel = Selection { strokes: (0..layer.strokes.len()).collect(), shapes: (0..layer.shapes.len()).collect() };
        for (i, l) in canvas.layers.iter().enumerate() {
            let mark = if i == li { " (arranged)" } else { "" };
            println!("layer {i}{mark}: {} strokes, {} shapes", l.strokes.len(), l.shapes.len());
        }
        let media = |c: &crate::canvas::canvas::Canvas| -> Vec<Bounds> {
            c.layers[li].shapes.iter().filter(|s| s.media_type().is_some()).map(shape_bounds).collect()
        };
        let fit = |c: &mut crate::canvas::canvas::Canvas| {
            let l = &c.layers[li];
            let all = Selection { strokes: (0..l.strokes.len()).collect(), shapes: (0..l.shapes.len()).collect() };
            let b = union(gather(l, &all, c.zoom).0.iter().map(|u| u.item.bounds));
            // Below the toolbar, clear of the slide panel.
            let (x0, y0, x1, y1) = (220.0, 70.0, 1180.0, 790.0);
            c.zoom = ((x1 - x0) / (b[2] - b[0])).min((y1 - y0) / (b[3] - b[1]));
            c.pan = [(x0 + x1) * 0.5 - (b[0] + b[2]) * 0.5 * c.zoom, (y0 + y1) * 0.5 - (b[1] + b[3]) * 0.5 * c.zoom];
        };
        let mut before = canvas.clone();
        fit(&mut before);
        crate::persistence::save_canvas(index + 6, &before).expect("save");
        let ops = [Ok(Align::Left), Ok(Align::Top), Ok(Align::CenterX), Err(Match::TallestHeight), Err(Match::NarrowestWidth)];
        for (k, op) in ops.into_iter().enumerate() {
            let mut out = canvas.clone();
            match op {
                Ok(a) => {
                    let (strokes, shapes) = plan(layer, &sel, Arrange::Slide(a), canvas.zoom);
                    out.move_objects_each(li, &strokes, &shapes);
                    // Boxes a hair into each other only touch (see
                    // `in_way`); f32 rounding at canvas coordinates
                    // leaves edges ~0.001 px into each other.
                    let oth = if matches!(a, Align::Left | Align::CenterX | Align::Right) { 1 } else { 0 };
                    let shrink = |b: Bounds| [b[0] + 0.01, b[1] + 0.01, b[2] - 0.01, b[3] - 0.01];
                    let after = media(&out);
                    for i in 0..after.len() {
                        for j in i + 1..after.len() {
                            let (p, q) = (after[i], after[j]);
                            assert!(
                                !(overlap(shrink(p), shrink(q)) && in_way(p, q, oth, 1.0 / canvas.zoom)),
                                "{a:?}: media {i} {p:?} and {j} {q:?} overlap"
                            );
                        }
                    }
                }
                Err(m) => {
                    let (units, _) = gather(layer, &sel, canvas.zoom);
                    let items: Vec<Item> = units.iter().map(|u| u.item).collect();
                    let ax = if matches!(m, Match::NarrowestWidth | Match::WidestWidth) { 0 } else { 1 };
                    let len = |b: Bounds| b[ax + 2] - b[ax];
                    let target = items.iter().zip(scales(&items, m)).map(|(it, s)| len(it.bounds) * s).find(|&l| l > EPS).expect("something to size");
                    let (strokes, shapes) = plan_sizes(layer, &sel, m, canvas.zoom);
                    out.transform_objects_each(li, &strokes, &shapes);
                    for (before, after) in media(&canvas).into_iter().zip(media(&out)) {
                        assert!((len(after) - target).abs() < target * 1e-3, "{m:?}: {after:?} is not {target}");
                        assert!((after[0] - before[0]).abs() < 0.05 && (after[1] - before[1]).abs() < 0.05, "{m:?}: top-left moved");
                    }
                    println!("{m:?}: every picture {target:.1} px");
                }
            }
            fit(&mut out);
            crate::persistence::save_canvas(index + 1 + k, &out).expect("save");
        }
    }

    #[test]
    fn align_lines_up_edges_and_centres() {
        let items = [item(10.0, 0.0, 100.0, 50.0, 0), item(200.0, 80.0, 40.0, 20.0, 0)];
        let left = moved(&items, &offsets(&items, Arrange::Align(Align::Left), 1.0));
        assert_eq!((left[0][0], left[1][0]), (10.0, 10.0));
        assert_eq!(left[1][1], 80.0, "aligning left leaves y alone");
        let right = moved(&items, &offsets(&items, Arrange::Align(Align::Right), 1.0));
        assert_eq!((right[0][2], right[1][2]), (240.0, 240.0));
        let mid = moved(&items, &offsets(&items, Arrange::Align(Align::CenterY), 1.0));
        assert_eq!(mid[0][1] + mid[0][3], mid[1][1] + mid[1][3]);
    }

    #[test]
    fn distribute_evens_the_gaps_and_keeps_the_ends() {
        let items = [
            item(0.0, 0.0, 100.0, 10.0, 0),
            item(110.0, 0.0, 50.0, 10.0, 0),
            item(400.0, 0.0, 100.0, 10.0, 0),
        ];
        let after = moved(&items, &offsets(&items, Arrange::DistributeX, 1.0));
        assert_eq!(after[0][0], 0.0);
        assert_eq!(after[2][2], 500.0);
        let g1 = after[1][0] - after[0][2];
        let g2 = after[2][0] - after[1][2];
        assert!((g1 - g2).abs() < 1e-3 && (g1 - 125.0).abs() < 1e-3);
        // Two objects: nothing to distribute.
        assert_eq!(offsets(&items[..2], Arrange::DistributeX, 1.0), vec![[0.0, 0.0]; 2]);
    }

    fn slid(items: &[Item], a: Align) -> Vec<Bounds> {
        moved(items, &offsets(items, Arrange::Slide(a), 1.0))
    }

    #[test]
    fn slide_left_keeps_a_row_a_row() {
        let items = [
            item(0.0, 0.0, 30.0, 20.0, 0),
            item(180.0, 5.0, 20.0, 20.0, 0),
            item(80.0, 0.0, 50.0, 20.0, 0),
            // Another row, with nothing in its way.
            item(150.0, 100.0, 40.0, 20.0, 0),
        ];
        let after = slid(&items, Align::Left);
        let xs: Vec<[f32; 2]> = after.iter().map(|b| [b[0], b[2]]).collect();
        assert_eq!(xs, vec![[0.0, 30.0], [80.0, 100.0], [30.0, 80.0], [0.0, 40.0]]);
        for (b, a) in items.iter().zip(&after) {
            assert_eq!((b.bounds[1], b.bounds[3]), (a[1], a[3]), "sliding left leaves y alone");
        }
    }

    #[test]
    fn slide_right_and_down_pile_up_against_the_far_edge() {
        let items = [item(0.0, 0.0, 30.0, 20.0, 0), item(100.0, 0.0, 50.0, 20.0, 0)];
        let right = slid(&items, Align::Right);
        assert_eq!((right[0][0], right[0][2]), (70.0, 100.0));
        assert_eq!((right[1][0], right[1][2]), (100.0, 150.0));
        // A column, sliding down.
        let items = [item(0.0, 0.0, 30.0, 20.0, 0), item(10.0, 50.0, 30.0, 40.0, 0)];
        let down = slid(&items, Align::Bottom);
        assert_eq!((down[0][1], down[0][3]), (30.0, 50.0));
        assert_eq!((down[1][1], down[1][3]), (50.0, 90.0));
    }

    #[test]
    fn slide_to_the_centre_gathers_round_the_nearest() {
        let items = [item(0.0, 0.0, 20.0, 10.0, 0), item(100.0, 0.0, 40.0, 10.0, 0), item(300.0, 0.0, 20.0, 10.0, 0)];
        let after = slid(&items, Align::CenterX);
        // The middle one is nearest the centre (160) and lands on it;
        // the others close up on either side.
        assert_eq!((after[1][0], after[1][2]), (140.0, 180.0));
        assert_eq!((after[0][0], after[0][2]), (120.0, 140.0));
        assert_eq!((after[2][0], after[2][2]), (180.0, 200.0));
    }

    #[test]
    fn slide_pushes_overlapping_objects_apart_but_not_touching_rows() {
        let items = [item(0.0, 0.0, 100.0, 50.0, 0), item(20.0, 10.0, 100.0, 50.0, 0)];
        let after = slid(&items, Align::Left);
        assert_eq!((after[1][0], after[1][2]), (100.0, 200.0));
        // Rows edge to edge, a hair into each other, don't block.
        let items = [item(50.0, 0.0, 20.0, 20.0, 0), item(100.0, 19.9, 20.0, 20.0, 0)];
        let after = slid(&items, Align::Left);
        assert_eq!(after[1][0], 50.0);
        // Nor do a PureRef block's, two canvas pixels into each other at
        // 200 %; a real row, a fifth into the next one, does.
        let items = [
            item(0.0, 0.0, 100.0, 100.0, 0),
            item(300.0, 98.0, 100.0, 100.0, 0),
            item(600.0, 80.0, 100.0, 100.0, 0),
        ];
        let after = moved(&items, &offsets(&items, Arrange::Slide(Align::Left), 0.5));
        assert_eq!(after[1][0], 0.0);
        assert_eq!(after[2][0], 100.0);
        // A thin line in a picture's row is in its way.
        let items = [item(0.0, 0.0, 100.0, 100.0, 0), item(300.0, 50.0, 60.0, 2.0, 0)];
        assert_eq!(slid(&items, Align::Left)[1][0], 100.0);
    }

    #[test]
    fn slide_never_overlaps_anything() {
        let sizes = [(300.0, 200.0), (120.0, 400.0), (500.0, 280.0), (90.0, 90.0), (260.0, 180.0),
                     (200.0, 340.0), (150.0, 100.0), (400.0, 220.0), (80.0, 160.0), (330.0, 330.0)];
        // Scattered, some on top of each other.
        let items: Vec<Item> = sizes.iter().enumerate()
            .map(|(i, &(w, h))| item((i * 7 % 10) as f32 * 230.0, (i * 3 % 10) as f32 * 170.0, w, h, 0))
            .collect();
        for a in [Align::Left, Align::CenterX, Align::Right, Align::Top, Align::CenterY, Align::Bottom] {
            let after = slid(&items, a);
            let oth = if matches!(a, Align::Left | Align::CenterX | Align::Right) { 1 } else { 0 };
            for i in 0..after.len() {
                for j in i + 1..after.len() {
                    assert!(!(overlap(after[i], after[j]) && in_way(after[i], after[j], oth, 1.0)), "{a:?}: {i} and {j} overlap");
                }
            }
            for (b, a) in items.iter().zip(&after) {
                assert_eq!((b.bounds[oth], b.bounds[oth + 2]), (a[oth], a[oth + 2]));
            }
        }
    }

    #[test]
    fn a_group_arranges_as_one_unit() {
        // Two far-apart strokes in one group and a third on its own: two
        // units, the group boxed around both of its strokes.
        let objects = [([0.0, 0.0, 10.0, 10.0], None), ([100.0, 0.0, 110.0, 10.0], None), ([300.0, 0.0, 310.0, 10.0], None)];
        let joined = join_groups(units(&objects, 1.0), &[7, 7, 0]);
        assert_eq!(joined.len(), 2);
        let g = joined.iter().find(|u| u.members.len() == 2).unwrap();
        assert_eq!(g.item.bounds, [0.0, 0.0, 110.0, 10.0]);
        // A grouped picture keeps the picture cluster for group-by-type.
        let objects = [([0.0, 0.0, 10.0, 10.0], Some(0)), ([50.0, 0.0, 60.0, 10.0], None)];
        let joined = join_groups(units(&objects, 1.0), &[3, 3]);
        assert_eq!((joined.len(), joined[0].item.group), (1, 0));
    }

    #[test]
    fn match_sizes_scales_whole_objects_from_their_top_left() {
        let items = [
            item(0.0, 0.0, 100.0, 50.0, 0),
            item(200.0, 10.0, 50.0, 80.0, 0),
            item(400.0, 0.0, 200.0, 20.0, 0),
            item(700.0, 0.0, 0.0, 30.0, 0), // a vertical line: no width
        ];
        assert_eq!(scales(&items, Match::NarrowestWidth), vec![0.5, 1.0, 0.25, 1.0]);
        assert_eq!(scales(&items, Match::WidestWidth), vec![2.0, 4.0, 1.0, 1.0]);
        assert_eq!(scales(&items, Match::ShortestHeight), vec![0.4, 0.25, 1.0, 20.0 / 30.0]);
        assert_eq!(scales(&items, Match::TallestHeight), vec![1.6, 1.0, 4.0, 80.0 / 30.0]);
        // As `plan_sizes` applies them.
        let s = scales(&items, Match::WidestWidth);
        for (it, s) in items.iter().zip(s).take(3) {
            let b = it.bounds;
            let m = Affine::scale_about([b[0], b[1]], s, s);
            let (p, q) = (m.apply([b[0], b[1]]), m.apply([b[2], b[3]]));
            assert_eq!(p, [b[0], b[1]], "top-left stays");
            assert!((q[0] - p[0] - 200.0).abs() < 1e-3);
            assert!(((q[1] - p[1]) / (q[0] - p[0]) - (b[3] - b[1]) / (b[2] - b[0])).abs() < 1e-5, "proportions kept");
        }
        // One object: nothing to match.
        assert_eq!(scales(&items[..1], Match::WidestWidth), vec![1.0]);
    }
}
