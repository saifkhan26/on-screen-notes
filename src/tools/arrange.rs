//! Arrange: tidy a selection. Pack it into a compact block, line it up
//! along an edge or centre, space it evenly, or sort it into clusters by
//! media type.
//!
//! Pure geometry: each operation takes the objects' boxes and returns
//! how far to move each one. The app applies the moves as one undo step
//! (`Canvas::move_objects_each`). Nothing is resized or rotated.

use crate::canvas::canvas::{shape_bounds, Layer};
use crate::canvas::hit::Selection;
use crate::canvas::shape::MediaType;

/// Axis-aligned box `[x0, y0, x1, y1]`, canvas-local.
pub type Bounds = [f32; 4];

/// Per-object moves: `(index, offset)`, ascending by index.
pub type Moves = Vec<(usize, [f32; 2])>;

/// The moves `op` makes of the selected strokes and shapes on `layer`.
/// Pictures, GIFs and videos move one by one, each carrying the ink
/// drawn on it; other ink and shapes move in touching clusters.
/// Distances that should look the same at any zoom are screen pixels
/// divided by `zoom`.
pub fn plan(layer: &Layer, sel: &Selection, op: Arrange, zoom: f32) -> (Moves, Moves) {
    let zoom = zoom.max(1e-3);
    // Every selected object as (box, media group), plus which it is.
    let mut objects: Vec<(Bounds, Option<u8>)> = Vec::new();
    let mut owners: Vec<(bool, usize)> = Vec::new();
    for &i in &sel.strokes {
        let Some(b) = layer.strokes.get(i).and_then(|s| s.bounds().map(|b| (b, s.base_width * 0.5))) else { continue };
        let (b, pad) = b;
        objects.push(([b[0] - pad, b[1] - pad, b[2] + pad, b[3] + pad], None));
        owners.push((true, i));
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
    }
    let units = units(&objects, 8.0 / zoom);
    let items: Vec<Item> = units.iter().map(|u| u.item).collect();
    let offsets = offsets(&items, op, 40.0 / zoom);
    let (mut strokes, mut shapes) = (Vec::new(), Vec::new());
    for (u, d) in units.iter().zip(offsets) {
        for &m in &u.members {
            match owners[m] {
                (true, i) => strokes.push((i, d)),
                (false, i) => shapes.push((i, d)),
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
    /// Equal gaps left to right; the outermost two stay put.
    DistributeX,
    /// Equal gaps top to bottom; the outermost two stay put.
    DistributeY,
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

/// How far to move each item, in `items` order. `gap` is the space
/// left between clusters by `GroupByType`.
pub fn offsets(items: &[Item], op: Arrange, gap: f32) -> Vec<[f32; 2]> {
    let mut out = vec![[0.0, 0.0]; items.len()];
    if items.len() < op.min_items() { return out; }
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
    if sizes.is_empty() { return (Vec::new(), [0.0, 0.0]); }
    // Tallest first, so each row starts with the boxes that set its
    // height and the shorter ones fill in beside them.
    let mut order: Vec<usize> = (0..sizes.len()).collect();
    order.sort_by(|&a, &b| sizes[b][1].total_cmp(&sizes[a][1]).then(sizes[b][0].total_cmp(&sizes[a][0])));
    let widest = sizes.iter().map(|s| s[0]).fold(0.0, f32::max);
    let sum_w: f32 = sizes.iter().map(|s| s[0]).sum();
    let area: f32 = sizes.iter().map(|s| s[0] * s[1]).sum();
    let ideal = (area * TARGET_ASPECT).sqrt();
    const TRIES: usize = 24;
    let mut best: Option<(f32, Vec<[f32; 2]>, [f32; 2])> = None;
    for k in 0..TRIES {
        // Widths from 0.6× to 2× the ideal, never narrower than the
        // widest box nor wider than all of them side by side.
        let f = 0.6 * (2.0_f32 / 0.6).powf(k as f32 / (TRIES - 1) as f32);
        let width = (ideal * f).min(sum_w).max(widest);
        let (pos, extent) = skyline(sizes, &order, width);
        let aspect = extent[0].max(EPS) / extent[1].max(EPS);
        let score = (extent[0] * extent[1]).max(EPS) * (1.0 + 0.35 * (aspect / TARGET_ASPECT).ln().abs());
        if best.as_ref().map_or(true, |b| score < b.0) {
            best = Some((score, pos, extent));
        }
    }
    let (_, pos, extent) = best.expect("at least one try");
    (pos, extent)
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
        let after = moved(&items, &offsets(&items, Arrange::Pack, 0.0));
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

    #[test]
    fn group_by_type_puts_each_group_in_its_own_cluster() {
        let items = [
            item(0.0, 0.0, 100.0, 100.0, 2),
            item(500.0, 0.0, 100.0, 100.0, 0),
            item(900.0, 300.0, 100.0, 100.0, 2),
            item(50.0, 600.0, 100.0, 100.0, 1),
        ];
        let after = moved(&items, &offsets(&items, Arrange::GroupByType, 40.0));
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
        let crate::persistence::LoadOutcome::Loaded(canvas) = crate::persistence::load_canvas(index, 0) else {
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

    #[test]
    fn align_lines_up_edges_and_centres() {
        let items = [item(10.0, 0.0, 100.0, 50.0, 0), item(200.0, 80.0, 40.0, 20.0, 0)];
        let left = moved(&items, &offsets(&items, Arrange::Align(Align::Left), 0.0));
        assert_eq!((left[0][0], left[1][0]), (10.0, 10.0));
        assert_eq!(left[1][1], 80.0, "aligning left leaves y alone");
        let right = moved(&items, &offsets(&items, Arrange::Align(Align::Right), 0.0));
        assert_eq!((right[0][2], right[1][2]), (240.0, 240.0));
        let mid = moved(&items, &offsets(&items, Arrange::Align(Align::CenterY), 0.0));
        assert_eq!(mid[0][1] + mid[0][3], mid[1][1] + mid[1][3]);
    }

    #[test]
    fn distribute_evens_the_gaps_and_keeps_the_ends() {
        let items = [
            item(0.0, 0.0, 100.0, 10.0, 0),
            item(110.0, 0.0, 50.0, 10.0, 0),
            item(400.0, 0.0, 100.0, 10.0, 0),
        ];
        let after = moved(&items, &offsets(&items, Arrange::DistributeX, 0.0));
        assert_eq!(after[0][0], 0.0);
        assert_eq!(after[2][2], 500.0);
        let g1 = after[1][0] - after[0][2];
        let g2 = after[2][0] - after[1][2];
        assert!((g1 - g2).abs() < 1e-3 && (g1 - 125.0).abs() < 1e-3);
        // Two objects: nothing to distribute.
        assert_eq!(offsets(&items[..2], Arrange::DistributeX, 0.0), vec![[0.0, 0.0]; 2]);
    }
}
