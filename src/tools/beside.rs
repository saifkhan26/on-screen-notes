//! New media beside the selection. A picture, GIF or video dropped,
//! pasted or inserted while one is selected comes in at that one's
//! height (or width) and lands right next to it, instead of at its own
//! size wherever it fell — so a row of references grows one even piece
//! at a time. What it matches and which side it goes are settings
//! (`AppConfig::beside_fit`, `beside_side`), saved with the config.
//!
//! Pure geometry: `place` says how to scale and move the new objects;
//! the app picks the selected object they go beside and applies it.

use serde::{Deserialize, Serialize};

use crate::tools::arrange::{in_way, Bounds};

/// What a new object takes from the selected picture, GIF or video.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BesideFit {
    /// Nothing: it comes in at its own size, where it was dropped.
    Off,
    /// Its height, so the two sit in a row.
    #[default]
    SameHeight,
    /// Its width.
    SameWidth,
}

/// Which side of the selected object a new one lands on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BesideSide {
    Left,
    #[default]
    Right,
}

impl BesideFit {
    pub const ALL: [BesideFit; 3] = [BesideFit::Off, BesideFit::SameHeight, BesideFit::SameWidth];

    pub fn label(self) -> &'static str {
        match self {
            BesideFit::Off => "Off",
            BesideFit::SameHeight => "Same height",
            BesideFit::SameWidth => "Same width",
        }
    }
}

impl BesideSide {
    pub const ALL: [BesideSide; 2] = [BesideSide::Left, BesideSide::Right];

    pub fn label(self) -> &'static str {
        match self {
            BesideSide::Left => "Left",
            BesideSide::Right => "Right",
        }
    }
}

/// Of the selected media boxes, the one new objects go beside: the
/// furthest out on `side`.
pub fn anchor(selected: impl Iterator<Item = Bounds>, side: BesideSide) -> Option<Bounds> {
    selected.reduce(|a, b| {
        let further = match side {
            BesideSide::Right => b[2] > a[2],
            BesideSide::Left => b[0] < a[0],
        };
        if further { b } else { a }
    })
}

/// The similarity `(a, b)` — every point `p` goes to `a · p + b`, see
/// `Shape::transform` — that sizes new objects whose media spans
/// `incoming` the way `fit` says to match `anchor`, and sets them on
/// `side` of it, tops level. Where that spot is taken by one of
/// `obstacles` (the layer's other media), they slide further out until
/// clear. `px` is one screen pixel in canvas units: boxes only a hair
/// into each other are not in each other's way (see `in_way`). `None`
/// when `fit` is off or either box has no size.
pub fn place(incoming: Bounds, anchor: Bounds, fit: BesideFit, side: BesideSide, obstacles: &[Bounds], px: f32) -> Option<(f32, [f32; 2])> {
    let len = |b: Bounds, ax: usize| b[ax + 2] - b[ax];
    let ax = match fit {
        BesideFit::Off => return None,
        BesideFit::SameHeight => 1,
        BesideFit::SameWidth => 0,
    };
    let s = len(anchor, ax) / len(incoming, ax);
    if !s.is_finite() || s <= 0.0 || len(incoming, 1 - ax) <= 0.0 {
        return None;
    }
    let (w, h) = (len(incoming, 0) * s, len(incoming, 1) * s);
    let y = anchor[1];
    let mut x = match side {
        BesideSide::Right => anchor[2],
        BesideSide::Left => anchor[0] - w,
    };
    // Each pass jumps past every box in the way, never back, so a box
    // passed once is never met again.
    for _ in 0..=obstacles.len() {
        let at = [x, y, x + w, y + h];
        let hit = obstacles.iter().filter(|o| in_way(**o, at, 0, px) && in_way(**o, at, 1, px));
        let next = match side {
            BesideSide::Right => hit.map(|o| o[2]).reduce(f32::max),
            BesideSide::Left => hit.map(|o| o[0] - w).reduce(f32::min),
        };
        match next {
            Some(n) => x = n,
            None => break,
        }
    }
    Some((s, [x - s * incoming[0], y - s * incoming[1]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply((a, b): (f32, [f32; 2]), r: Bounds) -> Bounds {
        [a * r[0] + b[0], a * r[1] + b[1], a * r[2] + b[0], a * r[3] + b[1]]
    }

    #[track_caller]
    fn assert_near(got: Bounds, want: Bounds) {
        assert!(got.iter().zip(want).all(|(g, w)| (g - w).abs() < 1e-3), "{got:?} is not {want:?}");
    }

    const PIC: Bounds = [100.0, 50.0, 300.0, 200.0]; // 200 × 150
    const BIG: Bounds = [-500.0, -400.0, 1500.0, 600.0]; // 2000 × 1000

    #[test]
    fn same_height_lands_beside_with_tops_level() {
        let right = place(BIG, PIC, BesideFit::SameHeight, BesideSide::Right, &[PIC], 1.0).unwrap();
        assert_near(apply(right, BIG), [300.0, 50.0, 600.0, 200.0]);
        let left = place(BIG, PIC, BesideFit::SameHeight, BesideSide::Left, &[PIC], 1.0).unwrap();
        assert_near(apply(left, BIG), [-200.0, 50.0, 100.0, 200.0]);
    }

    #[test]
    fn same_width_matches_the_width() {
        let p = place(BIG, PIC, BesideFit::SameWidth, BesideSide::Right, &[], 1.0).unwrap();
        assert_near(apply(p, BIG), [300.0, 50.0, 500.0, 150.0]);
        assert_eq!(place(BIG, PIC, BesideFit::Off, BesideSide::Right, &[], 1.0), None);
        // Something with no height cannot be given one.
        assert_eq!(place([0.0, 5.0, 10.0, 5.0], PIC, BesideFit::SameHeight, BesideSide::Right, &[], 1.0), None);
    }

    #[test]
    fn it_slides_past_whatever_already_sits_there() {
        // A row: the selected picture, then two more edge to edge (the
        // first a hair into it, as PureRef boards are), a gap too narrow
        // for the new one, and one more.
        let row = [PIC, [299.5, 50.0, 400.0, 200.0], [400.0, 60.0, 450.0, 190.0], [700.0, 0.0, 800.0, 100.0]];
        let p = place(BIG, PIC, BesideFit::SameHeight, BesideSide::Right, &row, 1.0).unwrap();
        assert_near(apply(p, BIG), [800.0, 50.0, 1100.0, 200.0]);
        // Leftwards, past the row's first picture only.
        let p = place(BIG, row[1], BesideFit::SameHeight, BesideSide::Left, &row, 1.0).unwrap();
        assert_near(apply(p, BIG), [-200.0, 50.0, 100.0, 200.0]);
        // A row above or below, not in the way, is passed by.
        let p = place(BIG, PIC, BesideFit::SameHeight, BesideSide::Right, &[PIC, [300.0, 200.0, 600.0, 400.0]], 1.0).unwrap();
        assert_near(apply(p, BIG), [300.0, 50.0, 600.0, 200.0]);
    }

    #[test]
    fn the_anchor_is_the_one_furthest_out() {
        let boxes = [[0.0, 0.0, 10.0, 10.0], [50.0, 0.0, 60.0, 10.0], [-20.0, 30.0, 5.0, 40.0]];
        assert_eq!(anchor(boxes.into_iter(), BesideSide::Right), Some(boxes[1]));
        assert_eq!(anchor(boxes.into_iter(), BesideSide::Left), Some(boxes[2]));
        assert_eq!(anchor(std::iter::empty(), BesideSide::Right), None);
    }
}
