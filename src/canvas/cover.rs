//! Boxes taken together, on screen or on the canvas: what is left of an
//! area once some boxes are cut out of it (the media filter's and the
//! selection's fade), and the outline round a set of boxes that follows
//! them instead of the one rectangle round them all (a group's
//! highlight, its box in the selection).

use egui::{Pos2, Rect, Vec2};

/// `area` minus the union of `holes`, as non-overlapping rectangles.
/// Cut into horizontal bands at every hole's top and bottom; within a
/// band the holes are plain x-intervals. The sweep down the bands only
/// looks at the holes the band crosses, and a gap the band below has
/// too grows down instead of starting a new rectangle: a slide of
/// 1,500 highlighted pictures stays a few thousand rectangles, cheap to
/// work out and to draw each frame.
pub fn uncovered(area: Rect, holes: &[Rect]) -> Vec<Rect> {
    let mut holes: Vec<Rect> = holes.iter().copied().filter(|h| h.is_positive()).collect();
    // By top; and a box given twice (a picture also in a group) once.
    let key = |h: &Rect| [h.top(), h.left(), h.bottom(), h.right()];
    holes.sort_by(|a, b| {
        key(a).iter().zip(key(b).iter()).map(|(x, y)| x.total_cmp(y)).find(|o| o.is_ne()).unwrap_or(std::cmp::Ordering::Equal)
    });
    holes.dedup();
    let mut ys: Vec<f32> = vec![area.top(), area.bottom()];
    for h in &holes {
        ys.push(h.top().clamp(area.top(), area.bottom()));
        ys.push(h.bottom().clamp(area.top(), area.bottom()));
    }
    ys.sort_by(f32::total_cmp);
    ys.dedup();
    let mut out = Vec::new();
    // The holes the band crosses, left to right, and the next hole (by
    // top) to come in.
    let mut active: Vec<Rect> = Vec::new();
    let mut next = 0;
    // The band above's gaps `(left, right, top)`, left to right, the top
    // where each began.
    let mut open: Vec<(f32, f32, f32)> = Vec::new();
    let close = |out: &mut Vec<Rect>, (l, r, top): (f32, f32, f32), y: f32| {
        out.push(Rect::from_min_max(Pos2::new(l, top), Pos2::new(r, y)));
    };
    let mut gaps: Vec<(f32, f32)> = Vec::new();
    let mut carried: Vec<(f32, f32, f32)> = Vec::new();
    let mut bottom = area.top();
    for band in ys.windows(2) {
        let (y0, y1) = (band[0], band[1]);
        if y1 <= y0 { continue; }
        let mid = (y0 + y1) * 0.5;
        while next < holes.len() && holes[next].top() <= mid {
            let h = holes[next];
            active.insert(active.partition_point(|a| a.left() <= h.left()), h);
            next += 1;
        }
        active.retain(|h| mid < h.bottom());
        gaps.clear();
        let mut x = area.left();
        for h in &active {
            let (l, r) = (h.left(), h.right());
            if l.min(area.right()) > x { gaps.push((x, l.min(area.right()))); }
            x = x.max(r);
        }
        if x < area.right() { gaps.push((x, area.right())); }
        // Gaps the band above had too carry on down; its others end.
        carried.clear();
        let mut k = 0;
        for &(l, r) in &gaps {
            while k < open.len() && open[k].0 < l {
                close(&mut out, open[k], y0);
                k += 1;
            }
            match open.get(k) {
                Some(&o) if o.0 == l && o.1 == r => {
                    carried.push(o);
                    k += 1;
                }
                Some(&o) if o.0 == l => {
                    close(&mut out, o, y0);
                    carried.push((l, r, y0));
                    k += 1;
                }
                _ => carried.push((l, r, y0)),
            }
        }
        for &o in &open[k..] {
            close(&mut out, o, y0);
        }
        std::mem::swap(&mut open, &mut carried);
        bottom = y1;
    }
    for o in open {
        close(&mut out, o, bottom);
    }
    out
}

/// The outline round `boxes` taken together, as closed loops: each
/// island's edge, and the edge of any hole in one. It is the edge of
/// their union once every gap or notch narrower than `bridge` is filled
/// (the union grown by half of `bridge`, then shrunk back), so it runs
/// along the boxes' own edges everywhere else: a group packed in two
/// uneven rows is one L-shaped outline, and an object beside its short
/// row stays out of it. Each loop starts at its top-left corner and goes
/// round clockwise on screen (y down), a hole's the other way; the loops
/// come top to bottom, then left to right, so the first is the top-left
/// island's.
pub fn outline(boxes: &[Rect], bridge: f32) -> Vec<Vec<Pos2>> {
    let boxes: Vec<Rect> = boxes.iter().copied().filter(|b| b.is_positive()).collect();
    let Some(all) = boxes.iter().copied().reduce(|a, b| a.union(b)) else { return Vec::new() };
    let r = (bridge * 0.5).max(0.0);
    // Room round the grown boxes, so what lies outside them all reaches
    // back in to `all`'s edge when it grows.
    let area = all.expand(2.0 * r + 1.0);
    let grown: Vec<Rect> = boxes.iter().map(|b| b.expand(r)).collect();
    let outside: Vec<Rect> = uncovered(area, &grown).into_iter().map(|o| o.expand(r)).collect();
    trace(&uncovered(area, &outside))
}

/// -0 as 0, so equal coordinates sort, and key, as one.
fn n(v: f32) -> f32 {
    v + 0.0
}

/// Sorted intervals `(from, to)` into `out`, with those that touch or
/// overlap made one.
fn merge_into(out: &mut Vec<(f32, f32)>, sorted: impl Iterator<Item = (f32, f32)>) {
    out.clear();
    for (a, b) in sorted {
        match out.last_mut() {
            Some(last) if a <= last.1 => last.1 = last.1.max(b),
            _ => out.push((a, b)),
        }
    }
}

/// Each part of `a` not in `b`, to `f`; both sorted and apart
/// (`merge_into`).
fn minus_each(a: &[(f32, f32)], b: &[(f32, f32)], mut f: impl FnMut(f32, f32)) {
    let mut j = 0;
    for &(start, end) in a {
        while j < b.len() && b[j].1 <= start {
            j += 1;
        }
        let mut s = start;
        let mut k = j;
        while s < end {
            match b.get(k) {
                Some(&(bs, be)) if bs < end => {
                    if bs > s {
                        f(s, bs);
                    }
                    s = s.max(be);
                    k += 1;
                }
                _ => {
                    f(s, end);
                    break;
                }
            }
        }
    }
}

/// The edge pieces along every line of `sides`: `(line, 0 for a side
/// the inside lies after along the line / 1 before it, from, to)`. A
/// side with no other side against it is edge; `edge(line, from, to,
/// after)` gets each piece.
fn edge_pieces(mut sides: Vec<(f32, u8, f32, f32)>, mut edge: impl FnMut(f32, f32, f32, bool)) {
    sides.sort_unstable_by(|p, q| p.0.total_cmp(&q.0).then(p.1.cmp(&q.1)).then(p.2.total_cmp(&q.2)));
    let (mut after, mut before) = (Vec::new(), Vec::new());
    for run in sides.chunk_by(|p, q| p.0 == q.0) {
        let line = run[0].0;
        merge_into(&mut after, run.iter().filter(|s| s.1 == 0).map(|s| (s.2, s.3)));
        merge_into(&mut before, run.iter().filter(|s| s.1 == 1).map(|s| (s.2, s.3)));
        minus_each(&after, &before, |a, b| edge(line, a, b, true));
        minus_each(&before, &after, |a, b| edge(line, a, b, false));
    }
}

/// The edge of the union of `rects`, which must not overlap (as
/// `uncovered` gives them), as `outline` returns it.
fn trace(rects: &[Rect]) -> Vec<Vec<Pos2>> {
    let rects = || rects.iter().filter(|r| r.is_positive());
    // The edge in pieces, each from its start to its end with the inside
    // on its right.
    let mut edges: Vec<(Pos2, Pos2)> = Vec::new();
    // Tops (inside below) and bottoms on their y. A top with nothing
    // ending on it from above goes right; a bottom with nothing starting
    // under it, left.
    let rows = rects().flat_map(|r| [(n(r.top()), 0, n(r.left()), n(r.right())), (n(r.bottom()), 1, n(r.left()), n(r.right()))]).collect();
    edge_pieces(rows, |y, a, b, top| {
        edges.push(if top { (Pos2::new(a, y), Pos2::new(b, y)) } else { (Pos2::new(b, y), Pos2::new(a, y)) });
    });
    // Left sides (inside to the right) going up; right sides, down.
    let cols = rects().flat_map(|r| [(n(r.left()), 0, n(r.top()), n(r.bottom())), (n(r.right()), 1, n(r.top()), n(r.bottom()))]).collect();
    edge_pieces(cols, |x, a, b, left| {
        edges.push(if left { (Pos2::new(x, b), Pos2::new(x, a)) } else { (Pos2::new(x, a), Pos2::new(x, b)) });
    });
    // Each piece by the point it starts at.
    let point = |p: Pos2| (u64::from(n(p.x).to_bits()) << 32) | u64::from(n(p.y).to_bits());
    let mut from: Vec<(u64, usize)> = edges.iter().enumerate().map(|(i, (s, _))| (point(*s), i)).collect();
    from.sort_unstable();
    let leaving = |p: Pos2| {
        let k = point(p);
        from[from.partition_point(|e| e.0 < k)..].iter().take_while(move |e| e.0 == k).map(|e| e.1)
    };
    let dir = |i: usize| {
        let d = edges[i].1 - edges[i].0;
        let sign = |v: f32| if v > 0.0 { 1.0 } else if v < 0.0 { -1.0 } else { 0.0 };
        Vec2::new(sign(d.x), sign(d.y))
    };
    // Where two corners touch, two pieces leave the point: the sharpest
    // right turn keeps each loop round its own inside.
    let turn = |d: Vec2, n: Vec2| 2.0 * (d.x * n.y - d.y * n.x) + (d.x * n.x + d.y * n.y);
    let mut used = vec![false; edges.len()];
    let mut loops: Vec<Vec<Pos2>> = Vec::new();
    for first in 0..edges.len() {
        if used[first] {
            continue;
        }
        used[first] = true;
        let mut pts = vec![edges[first].0];
        let mut cur = first;
        loop {
            let at = edges[cur].1;
            let d = dir(cur);
            let next = leaving(at)
                .filter(|&j| j == first || !used[j])
                .max_by(|&a, &b| turn(d, dir(a)).total_cmp(&turn(d, dir(b))));
            match next {
                Some(n) if n != first => {
                    pts.push(at);
                    used[n] = true;
                    cur = n;
                }
                _ => break,
            }
        }
        // From the top-left corner.
        let start = (0..pts.len()).min_by(|&a, &b| (pts[a].y, pts[a].x).partial_cmp(&(pts[b].y, pts[b].x)).unwrap_or(std::cmp::Ordering::Equal)).unwrap_or(0);
        pts.rotate_left(start);
        loops.push(pts);
    }
    loops.sort_by(|a, b| (a[0].y, a[0].x).partial_cmp(&(b[0].y, b[0].x)).unwrap_or(std::cmp::Ordering::Equal));
    loops
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn r(x0: f32, y0: f32, x1: f32, y1: f32) -> Rect {
        Rect::from_min_max(Pos2::new(x0, y0), Pos2::new(x1, y1))
    }

    /// `p` is inside the loops (even-odd: a hole's inside is out).
    pub(crate) fn inside(loops: &[Vec<Pos2>], p: Pos2) -> bool {
        let mut n = 0;
        for l in loops {
            for (i, a) in l.iter().enumerate() {
                let b = l[(i + 1) % l.len()];
                if (a.y > p.y) != (b.y > p.y) && p.x < a.x + (p.y - a.y) / (b.y - a.y) * (b.x - a.x) {
                    n += 1;
                }
            }
        }
        n % 2 == 1
    }

    fn rnd(seed: &mut u32, n: u32) -> u32 {
        *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (*seed >> 8) % n
    }

    #[test]
    fn uncovered_leaves_exactly_the_holes_out() {
        let area = r(0.0, 0.0, 100.0, 100.0);
        let holes = [r(10.0, 10.0, 40.0, 40.0), r(30.0, 30.0, 60.0, 50.0), r(90.0, 90.0, 150.0, 150.0)];
        let dim = uncovered(area, &holes);
        let total: f32 = dim.iter().map(|d| d.area()).sum();
        // 10000 − (900 + 600 overlap-corrected: 900 + 30·20 − 10·10) − 100 inside the area.
        assert!((total - (10000.0 - (900.0 + 600.0 - 100.0) - 100.0)).abs() < 1e-2, "{total}");
        for d in &dim {
            for h in &holes {
                assert!(d.intersect(*h).area() < 1e-3 || !d.intersect(*h).is_positive());
            }
        }
    }

    /// Every point of the area is faded exactly once, unless a hole is on
    /// it: then not at all. Random holes, some past the area's edges,
    /// some lined up so gaps run on down.
    #[test]
    fn uncovered_fades_each_point_once_outside_the_holes() {
        let mut seed = 12345u32;
        let area = r(0.0, 0.0, 60.0, 40.0);
        for _ in 0..300 {
            let n = rnd(&mut seed, 14);
            let mut holes: Vec<Rect> = Vec::new();
            for _ in 0..n {
                let (x, y) = (rnd(&mut seed, 70) as f32 - 5.0, rnd(&mut seed, 50) as f32 - 5.0);
                holes.push(r(x, y, x + 1.0 + rnd(&mut seed, 20) as f32, y + 1.0 + rnd(&mut seed, 15) as f32));
            }
            // A row of same-height holes, as pictures in a row are.
            let y = rnd(&mut seed, 40) as f32;
            for k in 0..rnd(&mut seed, 5) {
                let x = 12.0 * k as f32 + rnd(&mut seed, 3) as f32;
                holes.push(r(x, y, x + 8.0, y + 6.0));
            }
            let dim = uncovered(area, &holes);
            assert!(dim.iter().all(|d| d.is_positive() && area.contains_rect(*d)));
            for gx in 0..60 {
                for gy in 0..40 {
                    let p = Pos2::new(gx as f32 + 0.5, gy as f32 + 0.5);
                    let want = usize::from(!holes.iter().any(|h| h.contains(p)));
                    assert_eq!(dim.iter().filter(|d| d.contains(p)).count(), want, "at {p:?}, holes {holes:?}");
                }
            }
        }
        // A column of holes cut into bands by the one beside it: the gap
        // right of them is the same in every band, and is one rectangle
        // all the way down, not one per band.
        let mut cols: Vec<Rect> = Vec::new();
        for k in 0..10 {
            let y = 4.0 * k as f32;
            cols.push(r(0.0, y, 4.0, y + 2.0));
            cols.push(r(10.0, y + 1.0, 14.0, y + 5.0));
        }
        let dim = uncovered(r(0.0, 0.0, 20.0, 43.0), &cols);
        let right: Vec<&Rect> = dim.iter().filter(|d| d.left() == 14.0).collect();
        assert_eq!(right.len(), 1, "{right:?}");
        assert_eq!((right[0].top(), right[0].bottom(), right[0].right()), (1.0, 41.0, 20.0));
    }

    fn near(a: Pos2, b: Pos2) -> bool {
        a.distance(b) < 1e-3
    }

    #[test]
    fn one_box_is_its_own_outline() {
        let b = r(12.37, -4.5, 80.0, 33.3);
        let loops = outline(&[b], 12.0);
        assert_eq!(loops.len(), 1);
        let want = [b.left_top(), b.right_top(), b.right_bottom(), b.left_bottom()];
        assert_eq!(loops[0].len(), 4);
        assert!(loops[0].iter().zip(want).all(|(a, b)| near(*a, b)), "{:?}", loops[0]);
    }

    /// The case that asked for it: a group over two rows, the second
    /// shorter, and an object that is not in the group beside its short
    /// row, 3 px off. One L-shaped outline, the stranger out of it.
    #[test]
    fn a_two_row_group_is_one_outline_round_its_members_only() {
        let mut members = Vec::new();
        for i in 0..5 {
            members.push(r(23.0 * i as f32, 0.0, 23.0 * i as f32 + 20.0, 16.0));
        }
        for i in 0..3 {
            members.push(r(23.0 * i as f32, 19.0, 23.0 * i as f32 + 20.0, 35.0));
        }
        let stranger = r(69.0, 19.0, 89.0, 35.0);
        let loops = outline(&members, 12.0);
        assert_eq!(loops.len(), 1, "{loops:?}");
        let want = [(0.0, 0.0), (112.0, 0.0), (112.0, 16.0), (66.0, 16.0), (66.0, 35.0), (0.0, 35.0)];
        assert_eq!(loops[0].len(), 6, "{:?}", loops[0]);
        assert!(loops[0].iter().zip(want).all(|(a, (x, y))| near(*a, Pos2::new(x, y))), "{:?}", loops[0]);
        assert!(!inside(&loops, stranger.center()));
        assert!(members.iter().all(|m| inside(&loops, m.center())));
        // The gaps between the members are inside: the outline does not
        // weave round each one.
        assert!(inside(&loops, Pos2::new(21.5, 8.0)) && inside(&loops, Pos2::new(10.0, 17.5)));
        // Without bridging, each member is its own box.
        assert_eq!(outline(&members, 0.0).len(), 8);
    }

    #[test]
    fn far_apart_boxes_and_rings_make_several_loops() {
        let loops = outline(&[r(0.0, 50.0, 10.0, 60.0), r(100.0, 0.0, 110.0, 10.0)], 12.0);
        assert_eq!(loops.len(), 2);
        assert!(near(loops[0][0], Pos2::new(100.0, 0.0)), "top first");
        // Four boxes round a hole wider than the bridge: an outer loop and
        // the hole's.
        let ring = [r(0.0, 0.0, 60.0, 10.0), r(0.0, 50.0, 60.0, 60.0), r(0.0, 10.0, 10.0, 50.0), r(50.0, 10.0, 60.0, 50.0)];
        let loops = outline(&ring, 12.0);
        assert_eq!(loops.len(), 2, "{loops:?}");
        assert!(!inside(&loops, Pos2::new(30.0, 30.0)) && inside(&loops, Pos2::new(5.0, 30.0)));
        // Two boxes touching at a corner only: two loops, not a figure 8.
        let loops = outline(&[r(0.0, 0.0, 10.0, 10.0), r(10.0, 10.0, 20.0, 20.0)], 0.0);
        assert_eq!(loops.iter().map(|l| l.len()).collect::<Vec<_>>(), [4, 4]);
    }

    /// Without bridging the outline is exactly the union's edge; with it,
    /// it holds every box, stays in the box round them all, and keeps
    /// out of what lies further than half the bridge from them.
    #[test]
    fn the_outline_holds_the_union_and_bridges_only_narrow_gaps() {
        let mut seed = 99u32;
        for _ in 0..200 {
            let mut boxes = Vec::new();
            for _ in 0..1 + rnd(&mut seed, 8) {
                let (x, y) = (rnd(&mut seed, 40) as f32, rnd(&mut seed, 30) as f32);
                boxes.push(r(x, y, x + 1.0 + rnd(&mut seed, 12) as f32, y + 1.0 + rnd(&mut seed, 10) as f32));
            }
            let exact = outline(&boxes, 0.0);
            let bridged = outline(&boxes, 4.0);
            let all = boxes.iter().copied().reduce(|a, b| a.union(b)).unwrap();
            for gx in -3..58 {
                for gy in -3..45 {
                    let p = Pos2::new(gx as f32 + 0.5, gy as f32 + 0.5);
                    let in_box = boxes.iter().any(|b| b.contains(p));
                    assert_eq!(inside(&exact, p), in_box, "at {p:?}, boxes {boxes:?}");
                    if in_box {
                        assert!(inside(&bridged, p), "at {p:?}, boxes {boxes:?}");
                    }
                    let far = !boxes.iter().any(|b| b.expand(2.0).contains(p));
                    if far || !all.contains(p) {
                        assert!(!inside(&bridged, p), "at {p:?}, boxes {boxes:?}");
                    }
                }
            }
        }
    }
}
