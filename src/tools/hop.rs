//! Moving the selection from object to object with the keyboard: to the
//! nearest one the way a key points, and into and out of a group.
//!
//! A group counts as one object, the way a click takes it, until the
//! selection steps inside it (Enter, or an Alt+click on a member); then
//! hops go from member to member of that group only, and stepping out
//! takes the whole group again.

use crate::canvas::canvas::Layer;
use crate::canvas::hit::{ObjRef, Selection};
use crate::canvas::units::bounds_of;

/// Which way a hop goes, on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

impl Dir {
    /// In the order of `CanvasBindings::hop`.
    pub const ALL: [Dir; 4] = [Dir::Left, Dir::Right, Dir::Up, Dir::Down];

    pub fn name(self) -> &'static str {
        match self {
            Dir::Left => "left",
            Dir::Right => "right",
            Dir::Up => "up",
            Dir::Down => "down",
        }
    }
}

/// The selection, `dir`'s nearest object on `l` (see `pick`): another
/// member of the group the selection is inside, else another group or
/// object outside a group. `None` when nothing is that way.
pub fn hop(l: &Layer, sel: &Selection, dir: Dir) -> Option<Selection> {
    let from = bounds_of(l, &sel.strokes, &sel.shapes)?;
    let targets = targets(l, sel);
    let i = pick(from, targets.iter().map(|(_, b)| *b), dir)?;
    targets.into_iter().nth(i).map(|(s, _)| s)
}

/// The group the selection is inside: everything selected is in it, and
/// some of its members are not.
pub fn inside_group(l: &Layer, sel: &Selection) -> Option<u64> {
    let g = only_group(l, sel)?;
    (count(&l.group_members(g)) > count(sel)).then_some(g)
}

/// The group the selection is: all of it and nothing else.
pub fn whole_group(l: &Layer, sel: &Selection) -> Option<u64> {
    let g = only_group(l, sel)?;
    (count(&l.group_members(g)) == count(sel)).then_some(g)
}

/// Inside the selected group: the member nearest its top-left corner,
/// where reading it starts.
pub fn step_in(l: &Layer, sel: &Selection) -> Option<Selection> {
    let g = whole_group(l, sel)?;
    let b = bounds_of(l, &sel.strokes, &sel.shapes)?;
    let members = l.group_members(g);
    let each = members
        .strokes
        .iter()
        .map(|&i| one(ObjRef::Stroke(i)))
        .chain(members.shapes.iter().map(|&i| one(ObjRef::Shape(i))))
        .filter_map(|s| Some((bounds_of(l, &s.strokes, &s.shapes)?, s)));
    nearest_box([b[0], b[1]], each)
}

/// Out of the group the selection is inside: the whole group.
pub fn step_out(l: &Layer, sel: &Selection) -> Option<Selection> {
    inside_group(l, sel).map(|g| l.group_members(g))
}

/// The group or object outside a group nearest canvas point `p`: the
/// one under it, the smallest of those under it, or else the closest.
pub fn nearest(l: &Layer, p: [f32; 2]) -> Option<Selection> {
    nearest_box(p, units(l).into_iter().filter_map(|s| Some((bounds_of(l, &s.strokes, &s.shapes)?, s))))
}

/// Of `boxes`, the one a move from box `from` the way of `dir` goes to,
/// as a TV remote picks: one in line with `from` first (overlapping it
/// across the way of the move), else the nearest, distance along the
/// way counting 13 times as much as distance across it (squared).
/// Only boxes that lie further that way than `from` count.
pub fn pick(from: [f32; 4], boxes: impl IntoIterator<Item = [f32; 4]>, dir: Dir) -> Option<usize> {
    let across = matches!(dir, Dir::Left | Dir::Right);
    let s = facing_right(from, dir);
    let mut best: Option<(usize, [f32; 4])> = None;
    for (i, b) in boxes.into_iter().enumerate() {
        let d = facing_right(b, dir);
        if !is_candidate(s, d) {
            continue;
        }
        let better = match best {
            None => true,
            Some((_, have)) => {
                beam_beats(s, d, have, across) || (!beam_beats(s, have, d, across) && weighted(s, d) < weighted(s, have))
            }
        };
        if better {
            best = Some((i, d));
        }
    }
    best.map(|(i, _)| i)
}

// ---- inside ----------------------------------------------------------------

/// The group every selected object is in, when they share one.
fn only_group(l: &Layer, sel: &Selection) -> Option<u64> {
    let strokes = sel.strokes.iter().filter_map(|&i| l.strokes.get(i).map(|s| s.group));
    let shapes = sel.shapes.iter().filter_map(|&i| l.shapes.get(i).map(|s| s.group()));
    let mut groups = strokes.chain(shapes);
    let g = groups.next()?;
    (g != 0 && groups.all(|h| h == g)).then_some(g)
}

fn count(s: &Selection) -> usize {
    s.strokes.len() + s.shapes.len()
}

fn one(obj: ObjRef) -> Selection {
    let mut s = Selection::default();
    s.insert(obj);
    s
}

/// Every group on `l` whole, and every object outside a group alone.
fn units(l: &Layer) -> Vec<Selection> {
    let groups = crate::canvas::units::groups(l).into_iter().map(|u| Selection { strokes: u.strokes, shapes: u.shapes });
    let strokes = l.strokes.iter().enumerate().filter(|(_, s)| s.group == 0).map(|(i, _)| one(ObjRef::Stroke(i)));
    let shapes = l.shapes.iter().enumerate().filter(|(_, s)| s.group() == 0).map(|(i, _)| one(ObjRef::Shape(i)));
    groups.chain(strokes).chain(shapes).collect()
}

/// What a hop from `sel` may land on, with its box: inside a group its
/// other members one by one, else every group and lone object with
/// nothing selected.
fn targets(l: &Layer, sel: &Selection) -> Vec<(Selection, [f32; 4])> {
    let picked = |s: &Selection| {
        s.strokes.iter().any(|&i| sel.contains(ObjRef::Stroke(i))) || s.shapes.iter().any(|&i| sel.contains(ObjRef::Shape(i)))
    };
    let pool: Vec<Selection> = match inside_group(l, sel) {
        Some(g) => {
            let m = l.group_members(g);
            m.strokes.iter().map(|&i| one(ObjRef::Stroke(i))).chain(m.shapes.iter().map(|&i| one(ObjRef::Shape(i)))).collect()
        }
        None => units(l),
    };
    pool.into_iter().filter(|s| !picked(s)).filter_map(|s| Some((bounds_of(l, &s.strokes, &s.shapes)?, s)).map(|(b, s)| (s, b))).collect()
}

/// Of `each`, the one whose box is nearest `p`; of boxes `p` is in, the
/// smallest.
fn nearest_box(p: [f32; 2], each: impl Iterator<Item = ([f32; 4], Selection)>) -> Option<Selection> {
    let key = |b: &[f32; 4]| {
        let dx = (b[0] - p[0]).max(p[0] - b[2]).max(0.0);
        let dy = (b[1] - p[1]).max(p[1] - b[3]).max(0.0);
        (dx * dx + dy * dy, (b[2] - b[0]) * (b[3] - b[1]))
    };
    each.min_by(|(a, _), (b, _)| key(a).partial_cmp(&key(b)).unwrap_or(std::cmp::Ordering::Equal)).map(|(_, s)| s)
}

// ---- the way a key points --------------------------------------------------

/// Box `b` (`[left, top, right, bottom]`) turned so that `dir` points
/// right: the rest of the rules are written for a move right only.
fn facing_right(b: [f32; 4], dir: Dir) -> [f32; 4] {
    match dir {
        Dir::Right => b,
        Dir::Left => [-b[2], b[1], -b[0], b[3]],
        Dir::Down => [b[1], b[0], b[3], b[2]],
        Dir::Up => [-b[3], b[0], -b[1], b[2]],
    }
}

/// `d` lies further right than `s`: it starts right of where `s` starts
/// (or of where it ends) and ends right of where `s` ends.
fn is_candidate(s: [f32; 4], d: [f32; 4]) -> bool {
    (s[0] < d[0] || s[2] <= d[0]) && s[2] < d[2]
}

/// `d` overlaps `s` top to bottom: in line with it. Only touching is
/// not in line, so a row packed tight below is not in this row.
fn in_beam(s: [f32; 4], d: [f32; 4]) -> bool {
    d[3].min(s[3]) - d[1].max(s[1]) > 0.0
}

/// How far right of `s` `d` starts (0 when they overlap).
fn major(s: [f32; 4], d: [f32; 4]) -> f32 {
    (d[0] - s[2]).max(0.0)
}

/// How far right of `s` `d` ends.
fn major_to_far_edge(s: [f32; 4], d: [f32; 4]) -> f32 {
    (d[2] - s[2]).max(1.0)
}

/// How far apart their middles are, top to bottom.
fn minor(s: [f32; 4], d: [f32; 4]) -> f32 {
    ((d[1] + d[3]) - (s[1] + s[3])).abs() * 0.5
}

fn weighted(s: [f32; 4], d: [f32; 4]) -> f32 {
    let (a, b) = (major(s, d), minor(s, d));
    13.0 * a * a + b * b
}

/// `r1` wins over `r2` for being in line with `s` when `r2` is not: on
/// a move across always, when `r2` is not wholly past `s` always, and
/// on a move up or down when `r1` starts before `r2` ends.
fn beam_beats(s: [f32; 4], r1: [f32; 4], r2: [f32; 4], across: bool) -> bool {
    if in_beam(s, r2) || !in_beam(s, r1) {
        return false;
    }
    if s[2] > r2[0] || across {
        return true;
    }
    major(s, r1) < major_to_far_edge(s, r2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::shape::tests_support::rect;

    fn pick_of(from: [f32; 4], boxes: &[[f32; 4]], dir: Dir) -> Option<usize> {
        pick(from, boxes.iter().copied(), dir)
    }

    #[test]
    fn in_line_wins_across() {
        //  [A]        [B]
        //       [C]        [D]
        let a = [0.0, 0.0, 40.0, 20.0];
        let others = [[110.0, 0.0, 150.0, 20.0], [50.0, 30.0, 90.0, 50.0], [160.0, 30.0, 200.0, 50.0]];
        assert_eq!(pick_of(a, &others, Dir::Right), Some(0), "B is in line, C nearer but below");
        assert_eq!(pick_of(a, &others, Dir::Down), Some(1), "C is the nearest below");
        assert_eq!(pick_of(a, &others, Dir::Left), None, "nothing left of A");
        assert_eq!(pick_of(a, &others, Dir::Up), None);
        let b = others[0];
        let rest = [a, others[1], others[2]];
        assert_eq!(pick_of(b, &rest, Dir::Left), Some(0));
        assert_eq!(pick_of(b, &rest, Dir::Down), Some(2), "D is nearer across than C");
    }

    #[test]
    fn a_row_packed_tight_below_is_not_in_line() {
        let a = [0.0, 0.0, 10.0, 10.0];
        // Touching A's bottom edge, but right of it: not in A's row.
        let below = [12.0, 10.0, 22.0, 20.0];
        let far_right = [200.0, 2.0, 210.0, 8.0];
        assert_eq!(pick_of(a, &[below, far_right], Dir::Right), Some(1));
    }

    #[test]
    fn up_and_down_take_in_line_only_when_it_is_not_much_further() {
        let a = [0.0, 0.0, 10.0, 10.0];
        let straight_below_far = [0.0, 500.0, 10.0, 510.0];
        let slightly_aside_near = [12.0, 20.0, 22.0, 30.0];
        assert_eq!(pick_of(a, &[straight_below_far, slightly_aside_near], Dir::Down), Some(1));
        let straight_below_near = [0.0, 15.0, 10.0, 25.0];
        assert_eq!(pick_of(a, &[straight_below_near, slightly_aside_near], Dir::Down), Some(0));
    }

    /// Shapes 0 and 1 are group 7, side by side at the left; 2 is alone
    /// further right; 3 is alone below.
    fn layer() -> Layer {
        let mut g0 = rect([0.0, 0.0], [20.0, 20.0]);
        let mut g1 = rect([30.0, 0.0], [50.0, 20.0]);
        g0.set_group(7);
        g1.set_group(7);
        Layer {
            shapes: vec![g0, g1, rect([100.0, 0.0], [120.0, 20.0]), rect([0.0, 100.0], [20.0, 120.0])],
            ..Layer::default()
        }
    }

    fn shapes(i: &[usize]) -> Selection {
        Selection { strokes: vec![], shapes: i.to_vec() }
    }

    #[test]
    fn a_group_is_one_stop_from_outside() {
        let l = layer();
        assert_eq!(hop(&l, &shapes(&[2]), Dir::Left), Some(shapes(&[0, 1])), "lands on the whole group");
        assert_eq!(hop(&l, &shapes(&[0, 1]), Dir::Right), Some(shapes(&[2])), "leaves it whole, past its own member");
        assert_eq!(hop(&l, &shapes(&[3]), Dir::Up), Some(shapes(&[0, 1])));
        assert_eq!(hop(&l, &shapes(&[2]), Dir::Right), None);
    }

    #[test]
    fn inside_a_group_hops_stay_in_it() {
        let l = layer();
        assert_eq!(inside_group(&l, &shapes(&[0])), Some(7));
        assert_eq!(inside_group(&l, &shapes(&[0, 1])), None, "the whole group is not inside it");
        assert_eq!(inside_group(&l, &shapes(&[0, 2])), None, "not all in the group");
        assert_eq!(hop(&l, &shapes(&[0]), Dir::Right), Some(shapes(&[1])));
        assert_eq!(hop(&l, &shapes(&[1]), Dir::Right), None, "2 is outside the group");
        assert_eq!(hop(&l, &shapes(&[0]), Dir::Down), None, "3 is outside the group");
    }

    #[test]
    fn step_in_takes_the_first_member_and_step_out_the_group() {
        let l = layer();
        assert_eq!(step_in(&l, &shapes(&[0, 1])), Some(shapes(&[0])), "the top-left one");
        assert_eq!(step_in(&l, &shapes(&[2])), None, "not a group");
        assert_eq!(step_in(&l, &shapes(&[0])), None, "already inside");
        assert_eq!(step_out(&l, &shapes(&[1])), Some(shapes(&[0, 1])));
        assert_eq!(step_out(&l, &shapes(&[0, 1])), None, "not inside");
    }

    #[test]
    fn nearest_is_the_unit_under_the_point_or_closest() {
        let l = layer();
        assert_eq!(nearest(&l, [110.0, 10.0]), Some(shapes(&[2])));
        assert_eq!(nearest(&l, [25.0, 10.0]), Some(shapes(&[0, 1])), "inside the group's gap: the group");
        assert_eq!(nearest(&l, [10.0, 80.0]), Some(shapes(&[3])));
        assert_eq!(nearest(&Layer::default(), [0.0, 0.0]), None);
    }
}
