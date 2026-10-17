//! Lasso path collection, shared by Lasso erase and Lasso capture.
//!
//! A drag draws a freehand loop that closes on release. A click (press
//! and release without moving) drops a polygon corner instead, and the
//! path then stays open between presses
//! so the user can lay down as many corners as the shape needs. A drag
//! made while a polygon is open adds a freehand run to it. The polygon
//! closes on a click back on the first corner, a second click on the
//! last one (a double click) or Enter; Backspace takes back the last
//! corner and Esc drops the lot.
//!
//! Coordinates are canvas-local. Distances that should feel the same at
//! any zoom are given in screen pixels and divided by the zoom.

use super::ToolKind;

/// Pointer travel (screen px) that turns a press into a drag. Keeps a
/// tap with a slightly wobbly pen a click.
const CLICK_SLOP_PX: f32 = 5.0;
/// How close (screen px) a click must land to the first corner — or to
/// the last one, for a double click — to close the polygon.
const CLOSE_RADIUS_PX: f32 = 10.0;

#[derive(Debug, Clone, Default)]
pub struct LassoPath {
    /// The path so far.
    points: Vec<[f32; 2]>,
    /// Where each corner or freehand run of a polygon begins in
    /// `points`, oldest first. Empty while the path is not a polygon.
    runs: Vec<usize>,
    /// Where the press in flight began in `points`.
    press_start: Option<usize>,
    /// Tool and slide the path was drawn with.
    bound: Option<(ToolKind, usize)>,
}

impl LassoPath {
    pub fn points(&self) -> &[[f32; 2]] {
        &self.points
    }

    /// A polygon is open: the path survives pen-up.
    pub fn is_polygon(&self) -> bool {
        !self.runs.is_empty()
    }

    pub fn pressing(&self) -> bool {
        self.press_start.is_some()
    }

    /// The points a polygon's corners and freehand runs start at.
    pub fn corners(&self) -> impl Iterator<Item = [f32; 2]> + '_ {
        self.runs.iter().map(|&i| self.points[i])
    }

    /// The corner a click at `pos` would close the polygon on — the
    /// first one, or the last one for a double click — if it would.
    pub fn closes_at(&self, pos: [f32; 2], zoom: f32) -> Option<[f32; 2]> {
        if !self.is_polygon() || self.pressing() || self.points.len() < 3 {
            return None;
        }
        let r = CLOSE_RADIUS_PX / zoom.max(1e-3);
        [self.points[0], self.points[self.points.len() - 1]]
            .into_iter()
            .find(|&c| dist(pos, c) <= r)
    }

    /// Called once per frame. Drops the path when the tool or the slide
    /// changes under it: a polygon outlives pen-up, so it would
    /// otherwise turn up again on the next lasso press — and an outline
    /// begun for a capture must not end as an erase, or the reverse.
    pub fn sync(&mut self, kind: ToolKind, canvas_idx: usize) {
        let now = Some((kind, canvas_idx));
        if !matches!(kind, ToolKind::LassoErase | ToolKind::LassoSelect) || now != self.bound {
            self.clear();
        }
        self.bound = now;
    }

    pub fn clear(&mut self) {
        self.points.clear();
        self.runs.clear();
        self.press_start = None;
    }

    /// Pen-down. Starts a fresh path, or the next corner of an open
    /// polygon.
    pub fn begin(&mut self, pos: [f32; 2]) {
        if !self.is_polygon() {
            self.points.clear();
        }
        self.press_start = Some(self.points.len());
        self.points.push(pos);
    }

    /// Pen-move while down.
    pub fn update(&mut self, pos: [f32; 2]) {
        if self.press_start.is_none() {
            return;
        }
        // Skip duplicate samples (Wintab often re-emits the same coord
        // at idle). Keeps the rendered polyline smooth and the polygon
        // vertex count bounded.
        if self.points.last().map_or(true, |&p| dist(p, pos) >= 1.0) {
            self.points.push(pos);
        }
    }

    /// Pen-up. Returns the closed polygon once the path is finished.
    pub fn end(&mut self, pos: [f32; 2], zoom: f32) -> Option<Vec<[f32; 2]>> {
        let start = self.press_start.take()?;
        self.points.push(pos);
        let zoom = zoom.max(1e-3);
        let press = self.points[start];
        let moved = self.points[start..]
            .iter()
            .any(|&p| dist(p, press) > CLICK_SLOP_PX / zoom);

        if !moved {
            // A click: one corner at the press point, the pen's wobble
            // dropped.
            self.points.truncate(start + 1);
            if start > 0 && self.click_closes_before(start, press, zoom) {
                self.points.truncate(start);
                // Fewer than three corners is no shape yet: ignore the
                // click and leave the polygon open.
                if self.points.len() < 3 {
                    return None;
                }
                return self.close();
            }
            self.runs.push(start);
            return None;
        }
        if self.is_polygon() {
            // A freehand run inside a polygon. Ending it back on the
            // first corner closes the shape.
            self.runs.push(start);
            if self.points.len() >= 3 && dist(pos, self.points[0]) <= CLOSE_RADIUS_PX / zoom {
                return self.close();
            }
            return None;
        }
        // A freehand loop closes on release.
        self.close()
    }

    /// `click_closes` for a click whose corner sits at `points[start]`,
    /// measured against the path as it stood before that click.
    fn click_closes_before(&self, start: usize, pos: [f32; 2], zoom: f32) -> bool {
        let r = CLOSE_RADIUS_PX / zoom;
        dist(pos, self.points[0]) <= r || dist(pos, self.points[start - 1]) <= r
    }

    /// Enter: close an open polygon. Keeps it open when it has fewer
    /// than three corners, or a press is in flight.
    pub fn finish(&mut self) -> Option<Vec<[f32; 2]>> {
        if !self.is_polygon() || self.pressing() || self.points.len() < 3 {
            return None;
        }
        self.close()
    }

    /// Backspace: take back the polygon's last corner (or freehand run).
    pub fn undo_corner(&mut self) {
        if self.pressing() {
            return;
        }
        if let Some(start) = self.runs.pop() {
            self.points.truncate(start);
        }
    }

    /// Roll back the press in flight, leaving the corners before it.
    pub fn cancel_press(&mut self) {
        if let Some(start) = self.press_start.take() {
            self.points.truncate(start);
        }
    }

    fn close(&mut self) -> Option<Vec<[f32; 2]>> {
        let pts = std::mem::take(&mut self.points);
        self.clear();
        (pts.len() >= 3).then_some(pts)
    }
}

fn dist(a: [f32; 2], b: [f32; 2]) -> f32 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn click(l: &mut LassoPath, p: [f32; 2]) -> Option<Vec<[f32; 2]>> {
        l.begin(p);
        // A tap's wobble stays inside the slop.
        l.update([p[0] + 1.5, p[1] + 1.0]);
        l.end([p[0] + 1.0, p[1]], 1.0)
    }

    fn drag(l: &mut LassoPath, pts: &[[f32; 2]]) -> Option<Vec<[f32; 2]>> {
        l.begin(pts[0]);
        for &p in &pts[1..pts.len() - 1] {
            l.update(p);
        }
        l.end(pts[pts.len() - 1], 1.0)
    }

    const SQUARE: [[f32; 2]; 4] = [[0.0, 0.0], [100.0, 0.0], [100.0, 100.0], [0.0, 100.0]];

    #[test]
    fn a_drag_closes_on_release() {
        let mut l = LassoPath::default();
        let poly = drag(&mut l, &[[0.0, 0.0], [50.0, 0.0], [50.0, 50.0], [0.0, 50.0]]);
        assert_eq!(poly.map(|p| p.len()), Some(4));
        assert!(l.points().is_empty());
    }

    #[test]
    fn clicks_lay_corners_and_the_first_corner_closes() {
        let mut l = LassoPath::default();
        for p in SQUARE {
            assert_eq!(click(&mut l, p), None);
        }
        assert!(l.is_polygon());
        assert_eq!(l.corners().count(), 4);
        let poly = click(&mut l, [3.0, -2.0]).expect("closed");
        assert_eq!(poly, SQUARE.to_vec(), "exactly the corners, no wobble");
        assert!(!l.is_polygon());
    }

    #[test]
    fn a_double_click_closes() {
        let mut l = LassoPath::default();
        for p in &SQUARE[..3] {
            click(&mut l, *p);
        }
        assert_eq!(click(&mut l, [102.0, 98.0]).map(|p| p.len()), Some(3));
    }

    #[test]
    fn closing_needs_three_corners() {
        let mut l = LassoPath::default();
        click(&mut l, SQUARE[0]);
        click(&mut l, SQUARE[1]);
        assert_eq!(click(&mut l, SQUARE[0]), None);
        assert_eq!(l.corners().count(), 2, "the click is ignored, the polygon stays");
        assert_eq!(l.finish(), None);
        click(&mut l, SQUARE[2]);
        assert_eq!(l.finish().map(|p| p.len()), Some(3));
    }

    #[test]
    fn backspace_takes_back_the_last_corner() {
        let mut l = LassoPath::default();
        for p in &SQUARE[..3] {
            click(&mut l, *p);
        }
        l.undo_corner();
        assert_eq!(l.points(), &SQUARE[..2]);
        l.undo_corner();
        l.undo_corner();
        assert!(!l.is_polygon() && l.points().is_empty());
    }

    #[test]
    fn a_drag_in_a_polygon_adds_a_freehand_run() {
        let mut l = LassoPath::default();
        click(&mut l, SQUARE[0]);
        click(&mut l, SQUARE[1]);
        assert_eq!(drag(&mut l, &[[100.0, 50.0], [80.0, 80.0], [100.0, 100.0]]), None);
        assert_eq!(l.corners().count(), 3);
        assert_eq!(l.points().len(), 5);
        // A run that ends on the first corner closes the shape.
        let poly = drag(&mut l, &[[50.0, 100.0], [20.0, 50.0], [2.0, 3.0]]).expect("closed");
        assert_eq!(poly.len(), 8);
        // And Backspace drops a whole run, not one sample of it.
        let mut l = LassoPath::default();
        click(&mut l, SQUARE[0]);
        drag(&mut l, &[[100.0, 50.0], [80.0, 80.0], [100.0, 100.0]]);
        l.undo_corner();
        assert_eq!(l.points(), &SQUARE[..1]);
    }

    #[test]
    fn cancelling_a_press_keeps_the_corners_before_it() {
        let mut l = LassoPath::default();
        click(&mut l, SQUARE[0]);
        click(&mut l, SQUARE[1]);
        l.begin(SQUARE[2]);
        l.update([90.0, 90.0]);
        l.cancel_press();
        assert_eq!(l.points(), &SQUARE[..2]);
        assert!(l.is_polygon() && !l.pressing());
    }

    #[test]
    fn the_click_slop_is_on_screen() {
        // Zoomed in 4×, a 3-unit wobble is 12 screen px: a drag.
        let mut l = LassoPath::default();
        l.begin([0.0, 0.0]);
        l.update([3.0, 0.0]);
        l.update([3.0, 3.0]);
        assert!(l.end([0.0, 3.0], 4.0).is_some());
        assert!(!l.is_polygon());
    }

    #[test]
    fn a_polygon_is_dropped_by_a_tool_or_slide_change() {
        let mut l = LassoPath::default();
        l.sync(ToolKind::LassoSelect, 0);
        click(&mut l, SQUARE[0]);
        l.sync(ToolKind::LassoSelect, 0);
        assert!(l.is_polygon());
        l.sync(ToolKind::LassoSelect, 1);
        assert!(!l.is_polygon());
        click(&mut l, SQUARE[0]);
        l.sync(ToolKind::LassoErase, 1);
        assert!(l.points().is_empty(), "a capture outline never becomes an erase");
        l.sync(ToolKind::LassoErase, 1);
        click(&mut l, SQUARE[0]);
        l.sync(ToolKind::Pen, 1);
        assert!(l.points().is_empty());
    }
}
