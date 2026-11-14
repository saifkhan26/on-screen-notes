//! Media filter: highlight the pictures, GIFs and / or videos on the
//! slide, its groups, and what is tagged — everything tagged, or just the
//! tags picked. Matches get an outline and a small label in their kind's
//! colour (a tagged one's label is its tags); everything else fades back.
//! A group is outlined whole, and so is a group tagged as one (every
//! member has the tag); in any other group only the tagged members
//! light up. The toolbar's filter
//! popup also offers "Select these", which hands the matches to the
//! Select tool so Arrange can sort them.

use crate::canvas::canvas::{Canvas, Layer};
use crate::canvas::shape::MediaType;
use crate::canvas::units::{self, same_tag, Unit};
use egui::{Color32, Pos2, Rect, Rounding, Stroke};

/// What is highlighted. Runtime only.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MediaFilter {
    pub pictures: bool,
    pub gifs: bool,
    pub videos: bool,
    /// Every group, outlined whole.
    pub groups: bool,
    /// Everything with a tag.
    pub tagged: bool,
    /// What has one of these tags (case aside).
    pub tags: Vec<String>,
}

impl MediaFilter {
    pub fn any(&self) -> bool {
        self.pictures || self.gifs || self.videos || self.groups || self.tagged || !self.tags.is_empty()
    }

    pub fn shows(&self, t: MediaType) -> bool {
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

    /// `tag` is one of those picked.
    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.iter().any(|t| same_tag(t, tag))
    }

    /// Pick `tag`, or put it back.
    pub fn toggle_tag(&mut self, tag: &str) {
        if self.has_tag(tag) {
            self.tags.retain(|t| !same_tag(t, tag));
        } else {
            self.tags.push(tag.to_string());
        }
    }

    /// Something with these tags is highlighted.
    fn shows_tags(&self, tags: &[String]) -> bool {
        !tags.is_empty() && (self.tagged || tags.iter().any(|t| self.has_tag(t)))
    }
}

pub fn color(t: MediaType) -> Color32 {
    match t {
        MediaType::Picture => Color32::from_rgb(72, 199, 142),
        MediaType::Gif => Color32::from_rgb(255, 176, 59),
        MediaType::Video => Color32::from_rgb(236, 92, 168),
    }
}

/// Groups' colour, and tags' (here and on the arrange bar).
pub const GROUP_COLOUR: Color32 = Color32::from_rgb(150, 132, 255);
pub const TAG_COLOUR: Color32 = Color32::from_rgb(72, 196, 236);

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

/// Tags as a badge or chip shows them, `#pose #ref`, cut to `max` chars.
pub fn tags_text(tags: &[String], max: usize) -> String {
    let all = tags.iter().map(|t| format!("#{t}")).collect::<Vec<_>>().join(" ");
    if all.chars().count() > max {
        all.chars().take(max.saturating_sub(1)).chain(['…']).collect()
    } else {
        all
    }
}

/// What the slide's shown layers hold, for the popup's counts.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Counts {
    /// Pictures, GIFs and videos, in `MediaType::ALL` order.
    pub media: [usize; 3],
    pub groups: usize,
    /// Tagged objects, a group counted once.
    pub tagged: usize,
    /// Each tag, how many tagged objects have it (a group once), in
    /// name order.
    pub tags: Vec<(String, usize)>,
}

fn shown(canvas: &Canvas) -> impl Iterator<Item = &Layer> {
    canvas.layers.iter().filter(|l| l.visible && l.opacity > 0.0)
}

/// How many of each the slide's shown layers hold.
pub fn counts(canvas: &Canvas) -> Counts {
    let mut n = Counts::default();
    for layer in shown(canvas) {
        for t in layer.shapes.iter().filter_map(|s| s.media_type()) {
            n.media[MediaType::ALL.iter().position(|a| *a == t).unwrap_or(0)] += 1;
        }
        n.groups += units::groups(layer).len();
        n.tagged += units::tagged(layer).len();
        // Each tag counted as its toggle would light it up: a group
        // tagged as one once, else each member with the tag.
        let mut here: Vec<String> = Vec::new();
        for tag in layer.strokes.iter().flat_map(|s| s.tags.iter()).chain(layer.shapes.iter().flat_map(|s| s.tags().iter())) {
            if !here.iter().any(|t| same_tag(t, tag)) {
                here.push(tag.clone());
            }
        }
        for tag in here {
            let k = units::tagged_by(layer, |tags| tags.iter().any(|t| same_tag(t, &tag))).len();
            match n.tags.iter_mut().find(|(t, _)| same_tag(t, &tag)) {
                Some((_, have)) => *have += k,
                None => n.tags.push((tag, k)),
            }
        }
    }
    n.tags.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    n
}

/// What `filter` highlights on one layer: the media shapes, and the
/// groups and tagged objects whole, as units.
/// Why a unit lights up: as a group, or for its tags.
#[derive(Clone, Copy, PartialEq)]
enum Why {
    Group,
    Tags,
}

fn units_on(layer: &Layer, filter: &MediaFilter) -> Vec<(Unit, Why)> {
    let mut out: Vec<(Unit, Why)> = Vec::new();
    if filter.groups {
        out.extend(units::groups(layer).into_iter().map(|u| (u, Why::Group)));
    }
    if filter.tagged || !filter.tags.is_empty() {
        out.extend(units::tagged_by(layer, |tags| filter.shows_tags(tags)).into_iter().map(|u| (u, Why::Tags)));
    }
    out
}

/// The strokes and shapes on layer `li` that `filter` highlights, each
/// once, in index order.
pub fn matches_on(canvas: &Canvas, li: usize, filter: &MediaFilter) -> (Vec<usize>, Vec<usize>) {
    let Some(layer) = canvas.layers.get(li) else { return (Vec::new(), Vec::new()) };
    let mut shapes: Vec<usize> = layer
        .shapes
        .iter()
        .enumerate()
        .filter(|(_, s)| s.media_type().is_some_and(|t| filter.shows(t)))
        .map(|(i, _)| i)
        .collect();
    let mut strokes: Vec<usize> = Vec::new();
    for (u, _) in units_on(layer, filter) {
        strokes.extend(u.strokes);
        shapes.extend(u.shapes);
    }
    strokes.sort_unstable();
    strokes.dedup();
    shapes.sort_unstable();
    shapes.dedup();
    (strokes, shapes)
}

/// Fade everything on `rect` except the matches, then outline and label
/// each match. `rect` is the canvas area on screen. Labels for one box
/// (a tagged group is a group too) stack down its corner.
pub fn paint(painter: &egui::Painter, rect: Rect, canvas: &Canvas, filter: &MediaFilter) {
    let zoom = canvas.zoom;
    let to_screen = |p: [f32; 2]| Pos2::new(rect.min.x + canvas.pan[0] + p[0] * zoom, rect.min.y + canvas.pan[1] + p[1] * zoom);
    let mut found: Vec<([Pos2; 4], Color32, String)> = Vec::new();
    for layer in shown(canvas) {
        for shape in &layer.shapes {
            let Some(t) = shape.media_type().filter(|t| filter.shows(*t)) else { continue };
            let c = shape.obb().corners();
            found.push(([to_screen(c[0]), to_screen(c[1]), to_screen(c[2]), to_screen(c[3])], color(t), badge(t).to_string()));
        }
        for (u, why) in units_on(layer, filter) {
            // One shape keeps its own (maybe turned) box.
            let corners = match (u.strokes.as_slice(), u.shapes.as_slice()) {
                ([], [i]) => layer.shapes[*i].obb().corners(),
                _ => {
                    let Some(b) = units::bounds_of(layer, &u.strokes, &u.shapes) else { continue };
                    [[b[0], b[1]], [b[2], b[1]], [b[2], b[3]], [b[0], b[3]]]
                }
            };
            let quad = corners.map(to_screen);
            match why {
                Why::Group => found.push((quad, GROUP_COLOUR, format!("GROUP · {}", u.len()))),
                Why::Tags => found.push((quad, TAG_COLOUR, tags_text(&u.tags, 32))),
            }
        }
    }
    found.retain(|(q, _, _)| Rect::from_points(q).intersects(rect));
    // A group that is also tagged comes twice from the units: keep one of
    // each label.
    let mut seen: Vec<([Pos2; 4], String)> = Vec::new();
    found.retain(|(q, _, b)| {
        let dup = seen.iter().any(|(sq, sb)| sb == b && sq.iter().zip(q).all(|(a, b)| a.distance(*b) < 0.5));
        seen.push((*q, b.clone()));
        !dup
    });

    let holes: Vec<Rect> = found.iter().map(|(q, _, _)| Rect::from_points(q).intersect(rect)).collect();
    let mut mesh = egui::Mesh::default();
    for r in uncovered(rect, &holes) {
        mesh.add_colored_rect(r, Color32::from_black_alpha(150));
    }
    painter.add(egui::Shape::mesh(mesh));

    let under = Color32::from_black_alpha(150);
    let font = egui::FontId::proportional(10.0);
    let mut labelled: Vec<(Pos2, f32)> = Vec::new();
    for (quad, col, text) in &found {
        painter.add(egui::Shape::closed_line(quad.to_vec(), Stroke::new(4.0, under)));
        painter.add(egui::Shape::closed_line(quad.to_vec(), Stroke::new(2.0, *col)));
        let top_left = Rect::from_points(quad).min;
        // Below the labels already on this corner.
        let below = labelled.iter().filter(|(p, _)| p.distance(top_left) < 0.5).map(|(_, h)| h + 3.0).sum::<f32>();
        let galley = painter.layout_no_wrap(text.clone(), font.clone(), Color32::BLACK);
        let tag = Rect::from_min_size(top_left + egui::vec2(4.0, 4.0 + below), galley.size() + egui::vec2(8.0, 4.0));
        labelled.push((top_left, tag.height()));
        if tag.intersects(rect) {
            painter.rect_filled(tag, Rounding::same(3.0), *col);
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

    fn shape(at: [f32; 2], group: u64, tags: &[&str]) -> crate::canvas::shape::Shape {
        let mut s = crate::canvas::shape::tests_support::rect(at, [at[0] + 10.0, at[1] + 10.0]);
        s.set_group(group);
        s.set_tags(&tags.iter().map(|t| t.to_string()).collect::<Vec<_>>());
        s
    }

    #[test]
    fn groups_and_tags_are_counted_and_matched() {
        let mut c = Canvas::default();
        let l = &mut c.layers[0];
        l.shapes = vec![
            shape([0.0, 0.0], 0, &["pose"]),
            shape([20.0, 0.0], 4, &[]),
            shape([40.0, 0.0], 4, &["Pose", "ref"]),
            shape([60.0, 0.0], 8, &[]),
            shape([80.0, 0.0], 8, &[]),
            shape([100.0, 0.0], 0, &[]),
        ];
        let mut s = crate::canvas::stroke::Stroke::with_style([255; 4], 3.0, Default::default());
        s.push(crate::canvas::stroke::PenSample::from_pos([60.0, 5.0]));
        s.push(crate::canvas::stroke::PenSample::from_pos([80.0, 5.0]));
        s.group = 8;
        l.strokes.push(s);
        let n = counts(&c);
        assert_eq!((n.groups, n.tagged), (2, 2));
        assert_eq!(n.tags, vec![("pose".to_string(), 2), ("ref".to_string(), 1)]);
        let groups = MediaFilter { groups: true, ..Default::default() };
        assert_eq!(matches_on(&c, 0, &groups), (vec![0], vec![1, 2, 3, 4]));
        // One tag, case aside. In group 4 only shape 2 has it, so only
        // shape 2 lights up, not its whole group.
        let mut pose = MediaFilter::default();
        pose.toggle_tag("POSE");
        assert!(pose.any() && pose.has_tag("pose"));
        assert_eq!(matches_on(&c, 0, &pose), (vec![], vec![0, 2]));
        let mut refs = MediaFilter::default();
        refs.toggle_tag("ref");
        assert_eq!(matches_on(&c, 0, &refs), (vec![], vec![2]));
        refs.toggle_tag("REF");
        assert!(!refs.any(), "picked again, put back");
        let all = MediaFilter { tagged: true, ..Default::default() };
        assert_eq!(matches_on(&c, 0, &all), (vec![], vec![0, 2]));
        // Tag group 8 as one (every member): it lights up, and counts, whole.
        let l = &mut c.layers[0];
        let pose_tag = ["pose".to_string()];
        l.shapes[3].set_tags(&pose_tag);
        l.shapes[4].set_tags(&pose_tag);
        l.strokes[0].tags = pose_tag.to_vec();
        assert_eq!(matches_on(&c, 0, &pose), (vec![0], vec![0, 2, 3, 4]));
        assert_eq!(counts(&c).tags[0], ("pose".to_string(), 3), "shape 0, shape 2, group 8");
        assert_eq!(tags_text(&["walk cycle".into(), "ref".into()], 32), "#walk cycle #ref");
        assert_eq!(tags_text(&["a".repeat(40)], 10).chars().count(), 10);
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
