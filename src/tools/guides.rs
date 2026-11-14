//! Smart guides: while a selection is dragged, its left / middle / right
//! and top / middle / bottom line up with those of the objects around it
//! when they come close, and a thin line shows each one that did.
//!
//! The boxes to line up with are taken once, when the drag starts
//! (`Guides::new`); each frame only `snap` runs, which is a pass over
//! them per axis.

/// A box `[x0, y0, x1, y1]` in canvas units.
pub type Bounds = [f32; 4];

/// The selection's box as the drag began, and the boxes it can line up
/// with.
#[derive(Debug, Clone, Default)]
pub struct Guides {
    start: Bounds,
    targets: Vec<Bounds>,
}

/// A move lined up: the move to make instead, and the guide lines to
/// show, each from one end to the other in canvas units.
#[derive(Debug, Clone, PartialEq)]
pub struct Snap {
    pub d: [f32; 2],
    pub lines: Vec<[[f32; 2]; 2]>,
}

/// A box's three lines along one axis: its low edge, middle and high edge.
fn lines(b: Bounds, axis: usize) -> [f32; 3] {
    let (lo, hi) = (b[axis], b[axis + 2]);
    [lo, (lo + hi) * 0.5, hi]
}

impl Guides {
    /// `start` is the selection's box as the drag begins; `targets` the
    /// boxes of everything else it may line up with.
    pub fn new(start: Bounds, targets: Vec<Bounds>) -> Self {
        Guides { start, targets }
    }

    /// The move `d` changed so the box lands lined up where it nearly
    /// is: on each axis, the closest pair of one of its lines and one of
    /// a target's, if no further apart than `reach`, meet. Then a guide
    /// along each line it met, from the moved box to every target on
    /// that line (`reach` / 8 counts as on it).
    pub fn snap(&self, d: [f32; 2], reach: f32) -> Snap {
        let mut d = d;
        let mut met: [Option<f32>; 2] = [None, None];
        for axis in 0..2 {
            let moved = [self.start[0] + d[0], self.start[1] + d[1], self.start[2] + d[0], self.start[3] + d[1]];
            let mut best: Option<(f32, f32)> = None; // (shift, line it lands on)
            for t in &self.targets {
                for want in lines(*t, axis) {
                    for have in lines(moved, axis) {
                        let shift = want - have;
                        if shift.abs() <= reach && best.map_or(true, |(b, _)| shift.abs() < b.abs()) {
                            best = Some((shift, want));
                        }
                    }
                }
            }
            if let Some((shift, line)) = best {
                d[axis] += shift;
                met[axis] = Some(line);
            }
        }
        let moved = [self.start[0] + d[0], self.start[1] + d[1], self.start[2] + d[0], self.start[3] + d[1]];
        let on = reach / 8.0;
        let mut out = Vec::new();
        for (axis, line) in met.iter().enumerate() {
            let Some(at) = *line else { continue };
            // The span the guide covers, across the other axis: the moved
            // box and every target with a line here.
            let across = 1 - axis;
            let (mut lo, mut hi) = (moved[across], moved[across + 2]);
            for t in &self.targets {
                if lines(*t, axis).iter().any(|v| (v - at).abs() <= on) {
                    lo = lo.min(t[across]);
                    hi = hi.max(t[across + 2]);
                }
            }
            out.push(if axis == 0 { [[at, lo], [at, hi]] } else { [[lo, at], [hi, at]] });
        }
        Snap { d, lines: out }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edges_and_middles_line_up_within_reach() {
        // A 10×10 box at the origin; a target with x lines 100 / 120 / 140
        // and y lines 200 / 230 / 260.
        let g = Guides::new([0.0, 0.0, 10.0, 10.0], vec![[100.0, 200.0, 140.0, 260.0]]);
        // Right edge 98 is 2 off its left edge 100; y is nowhere near.
        let s = g.snap([88.0, 0.0], 5.0);
        assert_eq!(s.d, [90.0, 0.0]);
        assert_eq!(s.lines, vec![[[100.0, 0.0], [100.0, 260.0]]]);
        // Middle y 228 is 2 off its middle 230.
        let s = g.snap([0.0, 223.0], 5.0);
        assert_eq!(s.d, [0.0, 225.0]);
        // Too far: left as it is, no lines.
        let s = g.snap([70.0, 20.0], 5.0);
        assert_eq!(s, Snap { d: [70.0, 20.0], lines: vec![] });
    }

    #[test]
    fn the_nearest_line_wins_and_the_guide_spans_both_boxes() {
        // Box x lines after the move: 1 / 6 / 11. The first target's left
        // edge (0) is 1 off; the second's lines (3.5 / 8.5 / 13.5) are
        // 2.5 off at best.
        let g = Guides::new([0.0, 0.0, 10.0, 10.0], vec![[0.0, 100.0, 20.0, 120.0], [3.5, -50.0, 13.5, -40.0]]);
        let s = g.snap([1.0, 30.0], 5.0);
        assert_eq!(s.d, [0.0, 30.0]);
        // A vertical guide at x = 0 from the box (y 30..40) down to the
        // first target (y 100..120).
        assert_eq!(s.lines, vec![[[0.0, 30.0], [0.0, 120.0]]]);
    }
}
