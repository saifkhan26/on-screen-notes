//! Media filter: highlight the pictures, GIFs and / or videos on the
//! slide. Matches get an outline and a small label in their type's
//! colour; everything else fades back. The toolbar's filter popup also
//! offers "Select these", which hands the matches to the Select tool so
//! Arrange can sort them.

use crate::canvas::canvas::Canvas;
use crate::canvas::shape::MediaType;
use egui::{Color32, Pos2, Rect, Rounding, Stroke};

/// Which media types are highlighted. Runtime only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MediaFilter {
    pub pictures: bool,
    pub gifs: bool,
    pub videos: bool,
}

impl MediaFilter {
    pub fn any(self) -> bool {
        self.pictures || self.gifs || self.videos
    }

    pub fn shows(self, t: MediaType) -> bool {
        match t {
            MediaType::Picture => self.pictures,
            MediaType::Gif => self.gifs,
            MediaType::Video => self.videos,
        }
    }

    pub fn flag(&mut self, t: MediaType) -> &mut bool {
        match t {
            MediaType::Picture => &mut self.pictures,
            MediaType::Gif => &mut self.gifs,
            MediaType::Video => &mut self.videos,
        }
    }
}

pub fn color(t: MediaType) -> Color32 {
    match t {
        MediaType::Picture => Color32::from_rgb(72, 199, 142),
        MediaType::Gif => Color32::from_rgb(255, 176, 59),
        MediaType::Video => Color32::from_rgb(236, 92, 168),
    }
}

pub fn label(t: MediaType) -> &'static str {
    match t {
        MediaType::Picture => "Pictures",
        MediaType::Gif => "GIFs",
        MediaType::Video => "Videos",
    }
}

fn badge(t: MediaType) -> &'static str {
    match t {
        MediaType::Picture => "PICTURE",
        MediaType::Gif => "GIF",
        MediaType::Video => "VIDEO",
    }
}

/// How many pictures, GIFs and videos (in `MediaType::ALL` order) sit
/// on the slide's visible layers.
pub fn counts(canvas: &Canvas) -> [usize; 3] {
    let mut n = [0; 3];
    for layer in canvas.layers.iter().filter(|l| l.visible && l.opacity > 0.0) {
        for t in layer.shapes.iter().filter_map(|s| s.media_type()) {
            n[MediaType::ALL.iter().position(|a| *a == t).unwrap_or(0)] += 1;
        }
    }
    n
}

/// Indices of the shapes on layer `li` that `filter` highlights.
pub fn matches_on(canvas: &Canvas, li: usize, filter: MediaFilter) -> Vec<usize> {
    let Some(layer) = canvas.layers.get(li) else { return Vec::new() };
    layer
        .shapes
        .iter()
        .enumerate()
        .filter(|(_, s)| s.media_type().is_some_and(|t| filter.shows(t)))
        .map(|(i, _)| i)
        .collect()
}

/// Fade everything on `rect` except the matches, then outline and label
/// each match. `rect` is the canvas area on screen.
pub fn paint(painter: &egui::Painter, rect: Rect, canvas: &Canvas, filter: MediaFilter) {
    let zoom = canvas.zoom;
    let to_screen = |p: [f32; 2]| Pos2::new(rect.min.x + canvas.pan[0] + p[0] * zoom, rect.min.y + canvas.pan[1] + p[1] * zoom);
    let mut found: Vec<([Pos2; 4], MediaType)> = Vec::new();
    for layer in canvas.layers.iter().filter(|l| l.visible && l.opacity > 0.0) {
        for shape in &layer.shapes {
            let Some(t) = shape.media_type().filter(|t| filter.shows(*t)) else { continue };
            let c = shape.obb().corners();
            let quad = [to_screen(c[0]), to_screen(c[1]), to_screen(c[2]), to_screen(c[3])];
            if Rect::from_points(&quad).intersects(rect) {
                found.push((quad, t));
            }
        }
    }

    let holes: Vec<Rect> = found.iter().map(|(q, _)| Rect::from_points(q).intersect(rect)).collect();
    let mut mesh = egui::Mesh::default();
    for r in uncovered(rect, &holes) {
        mesh.add_colored_rect(r, Color32::from_black_alpha(150));
    }
    painter.add(egui::Shape::mesh(mesh));

    let under = Color32::from_black_alpha(150);
    let font = egui::FontId::proportional(10.0);
    for (quad, t) in &found {
        let col = color(*t);
        painter.add(egui::Shape::closed_line(quad.to_vec(), Stroke::new(4.0, under)));
        painter.add(egui::Shape::closed_line(quad.to_vec(), Stroke::new(2.0, col)));
        let top_left = Rect::from_points(quad).min;
        let galley = painter.layout_no_wrap(badge(*t).to_string(), font.clone(), Color32::BLACK);
        let tag = Rect::from_min_size(top_left + egui::vec2(4.0, 4.0), galley.size() + egui::vec2(8.0, 4.0));
        if tag.intersects(rect) {
            painter.rect_filled(tag, Rounding::same(3.0), col);
            painter.galley(tag.min + egui::vec2(4.0, 2.0), galley, Color32::BLACK);
        }
    }
}

/// `area` minus the union of `holes`, as non-overlapping rectangles.
/// Cut into horizontal bands at every hole's top and bottom; within a
/// band the holes are plain x-intervals.
fn uncovered(area: Rect, holes: &[Rect]) -> Vec<Rect> {
    let holes: Vec<Rect> = holes.iter().copied().filter(|h| h.is_positive()).collect();
    let mut ys: Vec<f32> = vec![area.top(), area.bottom()];
    for h in &holes {
        ys.push(h.top().clamp(area.top(), area.bottom()));
        ys.push(h.bottom().clamp(area.top(), area.bottom()));
    }
    ys.sort_by(f32::total_cmp);
    ys.dedup();
    let mut out = Vec::new();
    for band in ys.windows(2) {
        let (y0, y1) = (band[0], band[1]);
        if y1 <= y0 { continue; }
        let mid = (y0 + y1) * 0.5;
        let mut spans: Vec<(f32, f32)> =
            holes.iter().filter(|h| h.top() <= mid && mid < h.bottom()).map(|h| (h.left(), h.right())).collect();
        spans.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut x = area.left();
        for (l, r) in spans {
            if l > x { out.push(Rect::from_min_max(Pos2::new(x, y0), Pos2::new(l.min(area.right()), y1))); }
            x = x.max(r);
        }
        if x < area.right() { out.push(Rect::from_min_max(Pos2::new(x, y0), Pos2::new(area.right(), y1))); }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x0: f32, y0: f32, x1: f32, y1: f32) -> Rect {
        Rect::from_min_max(Pos2::new(x0, y0), Pos2::new(x1, y1))
    }

    #[test]
    fn uncovered_leaves_exactly_the_holes_out() {
        let area = r(0.0, 0.0, 100.0, 100.0);
        let holes = [r(10.0, 10.0, 40.0, 40.0), r(30.0, 30.0, 60.0, 50.0), r(90.0, 90.0, 150.0, 150.0)];
        let dim = uncovered(area, &holes);
        let total: f32 = dim.iter().map(|d| d.area()).sum();
        // 10000 − (900 + 600 + 600 overlap-corrected: 900 + 30·20 − 10·10) − 100 inside the area.
        assert!((total - (10000.0 - (900.0 + 600.0 - 100.0) - 100.0)).abs() < 1e-2, "{total}");
        for d in &dim {
            for h in &holes {
                assert!(d.intersect(*h).area() < 1e-3 || !d.intersect(*h).is_positive());
            }
        }
    }
}
