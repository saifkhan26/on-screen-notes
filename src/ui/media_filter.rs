//! Media filter: highlight the pictures, GIFs and / or videos on the
//! slide, its groups, and what is tagged — everything tagged, or just the
//! tags picked. Matches get an outline and, where they show big enough
//! for one, a small label in their kind's colour (a tagged one's label
//! is its tags); everything else fades back.
//! A group is outlined whole, round its members (`cover::outline`), and
//! so is a group tagged as one (every member has the tag); in any other
//! group only the tagged members light up. On a slide opened from a PureRef board it can also light up
//! what is gone from the board since (a resync keeps those on the
//! slide). The toolbar's filter popup also offers "Select these", which
//! hands the matches to the Select tool so Arrange can sort them.

use crate::canvas::affine::Affine;
use crate::canvas::canvas::{Canvas, Layer};
use crate::canvas::cover;
use crate::canvas::hit::Selection;
use crate::canvas::shape::MediaType;
use crate::canvas::units::{self, same_tag, Unit};
use std::collections::{HashMap, HashSet};
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
    /// What the slide's PureRef board no longer has (`LinkedItem::gone`).
    pub gone: bool,
}

impl MediaFilter {
    pub fn any(&self) -> bool {
        self.pictures || self.gifs || self.videos || self.groups || self.tagged || !self.tags.is_empty() || self.gone
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
/// What is gone from the slide's PureRef board.
pub const GONE_COLOUR: Color32 = Color32::from_rgb(240, 96, 80);

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
    /// Shapes gone from the slide's PureRef board; `None` when the slide
    /// is not linked to a board.
    pub gone: Option<usize>,
}

fn shown(canvas: &Canvas) -> impl Iterator<Item = &Layer> {
    canvas.layers.iter().filter(|l| l.visible && l.opacity > 0.0)
}

/// How many of each the slide's shown layers hold. Runs every frame the
/// toolbar shows, so it goes over each object once: a slide of 1,500
/// pictures with a hundred tags among them must stay well under a frame.
pub fn counts(canvas: &Canvas) -> Counts {
    let gone = canvas.board.as_ref().map(|_| {
        let links = gone_links(canvas);
        shown(canvas).flat_map(|l| &l.shapes).filter(|s| links.contains(&s.link())).count()
    });
    let mut n = Counts { gone, ..Counts::default() };
    // Tag (by `tag_key`) → its spelling where first met, and its count.
    let mut tags: HashMap<String, (String, usize)> = HashMap::new();
    for layer in shown(canvas) {
        for t in layer.shapes.iter().filter_map(|s| s.media_type()) {
            n.media[MediaType::ALL.iter().position(|a| *a == t).unwrap_or(0)] += 1;
        }
        n.groups += units::groups(layer).len();
        n.tagged += units::tagged(layer).len();
        // Each tag counted as its toggle would light it up: a group
        // tagged as one (every member has it) once, else each member
        // with the tag; an object in no group once.
        // Group → (members, tag key → members with it).
        let mut groups: HashMap<u64, (usize, HashMap<String, usize>)> = HashMap::new();
        let objects = layer
            .strokes
            .iter()
            .map(|s| (s.group, s.tags.as_slice()))
            .chain(layer.shapes.iter().map(|s| (s.group(), s.tags())));
        for (group, own) in objects {
            let mut keys: Vec<String> = Vec::with_capacity(own.len());
            for t in own {
                let key = units::tag_key(t);
                if !keys.contains(&key) {
                    tags.entry(key.clone()).or_insert_with(|| (t.clone(), 0));
                    keys.push(key);
                }
            }
            if group == 0 {
                for key in keys {
                    if let Some(e) = tags.get_mut(&key) {
                        e.1 += 1;
                    }
                }
            } else {
                let g = groups.entry(group).or_default();
                g.0 += 1;
                for key in keys {
                    *g.1.entry(key).or_default() += 1;
                }
            }
        }
        for (members, with) in groups.into_values() {
            for (key, k) in with {
                if let Some(e) = tags.get_mut(&key) {
                    e.1 += if k == members { 1 } else { k };
                }
            }
        }
    }
    n.tags = tags.into_values().collect();
    n.tags.sort_by_cached_key(|(t, _)| units::tag_key(t));
    n
}

/// The link ids of the shapes whose item is gone from the slide's
/// PureRef board.
fn gone_links(canvas: &Canvas) -> HashSet<u64> {
    canvas.board.as_ref().map(|b| b.items.iter().filter(|i| i.gone).map(|i| i.link).collect()).unwrap_or_default()
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
    let gone = if filter.gone { gone_links(canvas) } else { HashSet::new() };
    let mut shapes: Vec<usize> = layer
        .shapes
        .iter()
        .enumerate()
        .filter(|(_, s)| s.media_type().is_some_and(|t| filter.shows(t)) || gone.contains(&s.link()))
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

/// Gestures in flight that show objects somewhere else than the canvas
/// has them until they commit; the highlights go along with them, as
/// the renderer previews them (`render::paint_canvas_egui`).
#[derive(Clone, Copy, Default)]
pub struct InFlight<'a> {
    /// The Select tool's move, resize or turn: `(layer index, selected
    /// objects, transform)`.
    pub sel: Option<(usize, &'a Selection, Affine)>,
    /// The Shift+Space layer drag: `(layer index, scale, offset)`, the
    /// similarity `p → scale · p + offset`.
    pub layer: Option<(usize, f32, [f32; 2])>,
}

/// Where a layer's objects show on screen this frame: through the view,
/// the layer's own drag, and the selection's gesture in flight.
struct Place<'a> {
    layer: &'a Layer,
    /// Where the canvas's (0, 0) is on screen.
    origin: Pos2,
    zoom: f32,
    /// The layer drag's scale and offset.
    scale: f32,
    off: [f32; 2],
    dragged: Option<(&'a Selection, Affine)>,
}

impl<'a> Place<'a> {
    fn new(rect: Rect, canvas: &'a Canvas, li: usize, moving: InFlight<'a>) -> Option<Self> {
        let layer = canvas.layers.get(li)?;
        let (scale, off) = match moving.layer {
            Some((l, a, off)) if l == li => (a, off),
            _ => (1.0, [0.0, 0.0]),
        };
        let dragged = moving.sel.filter(|(l, _, _)| *l == li).map(|(_, sel, m)| (sel, m));
        let origin = Pos2::new(rect.min.x + canvas.pan[0], rect.min.y + canvas.pan[1]);
        Some(Place { layer, origin, zoom: canvas.zoom, scale, off, dragged })
    }

    fn to_screen(&self, p: [f32; 2]) -> Pos2 {
        let [x, y] = [self.scale * p[0] + self.off[0], self.scale * p[1] + self.off[1]];
        Pos2::new(self.origin.x + x * self.zoom, self.origin.y + y * self.zoom)
    }

    /// Shape `i`'s corners on screen.
    fn shape(&self, i: usize) -> Option<[Pos2; 4]> {
        let s = self.layer.shapes.get(i)?;
        let c = match self.dragged {
            Some((sel, m)) if sel.shapes.binary_search(&i).is_ok() => {
                let mut moved = s.clone();
                moved.apply_affine(&m);
                moved.obb().corners()
            }
            _ => s.obb().corners(),
        };
        Some(c.map(|p| self.to_screen(p)))
    }

    /// The corners of the box round stroke `i`, on screen.
    fn stroke(&self, i: usize) -> Option<[Pos2; 4]> {
        let b = self.layer.strokes.get(i)?.bounds()?;
        let mut c = [[b[0], b[1]], [b[2], b[1]], [b[2], b[3]], [b[0], b[3]]];
        if let Some((_, m)) = self.dragged.filter(|(sel, _)| sel.strokes.binary_search(&i).is_ok()) {
            c = c.map(|p| m.apply(p));
        }
        Some(c.map(|p| self.to_screen(p)))
    }
}

/// Gaps narrower than this (screen px) between a group's members are
/// inside its outline (`cover::outline`); wider ones split it.
const BRIDGE_PX: f32 = 12.0;

/// A highlight's outline on screen: closed loops (see `cover::outline`).
/// One object's is its four corners.
type Outline = Vec<Vec<Pos2>>;

/// The box round all of an outline.
fn bounds(o: &Outline) -> Rect {
    let mut r = Rect::NOTHING;
    for p in o.iter().flatten() {
        r.extend_with(*p);
    }
    r
}

/// Twice a loop's area, positive for an island's edge and negative for
/// a hole's (`cover::outline` sends them round opposite ways).
fn signed_area(l: &[Pos2]) -> f32 {
    (0..l.len())
        .map(|i| {
            let (a, b) = (l[i], l[(i + 1) % l.len()]);
            a.x * b.y - b.x * a.y
        })
        .sum()
}

/// The two outlines are the same, within half a pixel.
fn same_outline(a: &Outline, b: &Outline) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(p, q)| p.len() == q.len() && p.iter().zip(q).all(|(x, y)| x.distance(*y) < 0.5))
}

/// What `paint` draws: each match's outline, colour and label, and the
/// screen boxes left out of the fade. Those are each highlighted
/// object's own box, so an object that only sits among a group's
/// members still fades; a group's outline follows its members too.
struct Marks {
    found: Vec<(Outline, Color32, String)>,
    holes: Vec<Rect>,
}

fn marks(rect: Rect, canvas: &Canvas, filter: &MediaFilter, moving: InFlight) -> Marks {
    let mut found: Vec<(Outline, Color32, String)> = Vec::new();
    let mut holes: Vec<Rect> = Vec::new();
    let gone = if filter.gone { gone_links(canvas) } else { HashSet::new() };
    // A group highlighted as a group and as tagged as one is outlined
    // once: (layer, group) → outline.
    let mut outlined: HashMap<(usize, u64), Outline> = HashMap::new();
    for li in 0..canvas.layers.len() {
        let Some(place) = Place::new(rect, canvas, li, moving) else { continue };
        let layer = place.layer;
        if !layer.visible || layer.opacity <= 0.0 {
            continue;
        }
        for (i, shape) in layer.shapes.iter().enumerate() {
            let is_gone = gone.contains(&shape.link());
            let media = shape.media_type().filter(|t| filter.shows(*t));
            if !is_gone && media.is_none() {
                continue;
            }
            let Some(quad) = place.shape(i) else { continue };
            holes.push(Rect::from_points(&quad));
            if is_gone {
                found.push((vec![quad.to_vec()], GONE_COLOUR, "GONE FROM BOARD".to_string()));
            }
            if let Some(t) = media {
                found.push((vec![quad.to_vec()], color(t), badge(t).to_string()));
            }
        }
        for (u, why) in units_on(layer, filter) {
            let members: Vec<[Pos2; 4]> =
                u.strokes.iter().filter_map(|&i| place.stroke(i)).chain(u.shapes.iter().filter_map(|&i| place.shape(i))).collect();
            let boxes: Vec<Rect> = members.iter().map(|q| Rect::from_points(q)).collect();
            if !boxes.iter().any(|b| b.intersects(rect)) {
                continue;
            }
            // One shape keeps its own (maybe turned) box; more are
            // outlined round their members, bridging narrow gaps.
            let outline = match (u.strokes.as_slice(), u.shapes.as_slice(), members.as_slice()) {
                ([], [_], [q]) => vec![q.to_vec()],
                _ if u.group != 0 => outlined.entry((li, u.group)).or_insert_with(|| cover::outline(&boxes, BRIDGE_PX)).clone(),
                _ => cover::outline(&boxes, BRIDGE_PX),
            };
            if outline.is_empty() {
                continue;
            }
            holes.extend(boxes);
            match why {
                Why::Group => found.push((outline, GROUP_COLOUR, format!("GROUP · {}", u.len()))),
                Why::Tags => found.push((outline, TAG_COLOUR, tags_text(&u.tags, 32))),
            }
        }
    }
    found.retain(|(o, _, _)| bounds(o).intersects(rect));
    // A group that is also tagged comes twice from the units: keep one of
    // each label. Earlier outlines are looked up by their first corner,
    // not gone through one by one: a slide of 1,500 pictures would make
    // that a million comparisons a frame.
    let first = |o: &Outline| o.first().and_then(|l| l.first()).copied().unwrap_or(Pos2::ZERO);
    let mut earlier: Near<usize> = Near::default();
    let mut keep: Vec<bool> = Vec::with_capacity(found.len());
    for (i, (o, _, b)) in found.iter().enumerate() {
        let dup = earlier.around(first(o)).any(|&(_, j)| {
            let (so, _, sb) = &found[j];
            sb == b && same_outline(so, o)
        });
        earlier.add(first(o), i);
        keep.push(!dup);
    }
    let mut keep = keep.into_iter();
    found.retain(|_| keep.next().unwrap_or(true));
    let holes = holes.into_iter().filter(|h| h.intersects(rect)).map(|h| h.intersect(rect)).collect();
    Marks { found, holes }
}

/// Fade everything on `rect` (the canvas area on screen) but `holes`.
pub fn fade(painter: &egui::Painter, rect: Rect, holes: &[Rect]) {
    let mut mesh = egui::Mesh::default();
    for r in cover::uncovered(rect, holes) {
        mesh.add_colored_rect(r, Color32::from_black_alpha(150));
    }
    painter.add(egui::Shape::mesh(mesh));
}

/// The screen boxes of the objects `sel` holds on layer `li`, as they
/// show this frame: what the Select tool's "dim the rest" leaves out of
/// the fade.
pub fn selection_holes(rect: Rect, canvas: &Canvas, li: usize, sel: &Selection, moving: InFlight) -> Vec<Rect> {
    let Some(place) = Place::new(rect, canvas, li, moving) else { return Vec::new() };
    sel.strokes
        .iter()
        .filter_map(|&i| place.stroke(i))
        .chain(sel.shapes.iter().filter_map(|&i| place.shape(i)))
        .map(|q| Rect::from_points(&q))
        .filter(|h| h.intersects(rect))
        .map(|h| h.intersect(rect))
        .collect()
}

/// Screen points looked up by what lies within half a pixel of a point:
/// kept in 1 px cells, so those are all in its cell or the eight round it.
struct Near<T> {
    cells: HashMap<(i32, i32), Vec<(Pos2, T)>>,
}

impl<T> Default for Near<T> {
    fn default() -> Self {
        Near { cells: HashMap::new() }
    }
}

impl<T> Near<T> {
    fn cell(p: Pos2) -> (i32, i32) {
        (p.x.floor() as i32, p.y.floor() as i32)
    }

    fn add(&mut self, p: Pos2, v: T) {
        self.cells.entry(Self::cell(p)).or_default().push((p, v));
    }

    /// What was added within half a pixel of `p`.
    fn around(&self, p: Pos2) -> impl Iterator<Item = &(Pos2, T)> {
        let (x, y) = Self::cell(p);
        (-1..=1)
            .flat_map(move |dx| (-1..=1).map(move |dy| (x.saturating_add(dx), y.saturating_add(dy))))
            .filter_map(|k| self.cells.get(&k))
            .flatten()
            .filter(move |(q, _)| q.distance(p) < 0.5)
    }
}

/// Whether a label at `tag` has room in the box it labels, `boxed`: it
/// ends inside the box, and the box is as wide as the label, or 64 px
/// (a long list of tags may run past a picture's edge). Zoomed far out,
/// labels on boxes smaller than that would only pile up over the
/// pictures, and cost a frame to draw on a slide of 1,500.
fn has_room(boxed: Rect, tag: Rect) -> bool {
    const MARGIN: f32 = 4.0;
    tag.bottom() + MARGIN <= boxed.bottom() && boxed.width() >= (tag.width() + 2.0 * MARGIN).min(64.0)
}

/// Fade everything on `rect` except the matches (unless `fade_too` is
/// off: the selection's own fade shows instead), then outline and label
/// each match. `rect` is the canvas area on screen. Labels for one box
/// (a tagged group is a group too) stack down its corner; a box too
/// small for a label (`has_room`) is only outlined. A group whose
/// members lie apart is labelled on its first island, top left first,
/// with room for it.
pub fn paint(painter: &egui::Painter, rect: Rect, canvas: &Canvas, filter: &MediaFilter, moving: InFlight, fade_too: bool) {
    let Marks { found, holes } = marks(rect, canvas, filter, moving);
    if fade_too {
        fade(painter, rect, &holes);
    }

    let under = Color32::from_black_alpha(150);
    let font = egui::FontId::proportional(10.0);
    let mut labelled: Near<f32> = Near::default();
    for (outline, col, text) in &found {
        for l in outline {
            painter.add(egui::Shape::closed_line(l.clone(), Stroke::new(4.0, under)));
            painter.add(egui::Shape::closed_line(l.clone(), Stroke::new(2.0, *col)));
        }
        let galley = painter.layout_no_wrap(text.clone(), font.clone(), Color32::BLACK);
        let size = galley.size() + egui::vec2(8.0, 4.0);
        // The islands' boxes, not the holes', in `outline`'s order.
        let islands = outline.iter().filter(|l| outline.len() == 1 || signed_area(l) > 0.0).map(|l| Rect::from_points(l));
        // Below the labels already on the island's corner. One left out
        // holds no place: a group's label is not pushed down by one its
        // small top-left member had no room for.
        let spot = islands
            .map(|boxed| {
                let below = labelled.around(boxed.min).map(|(_, h)| h + 3.0).sum::<f32>();
                (boxed, Rect::from_min_size(boxed.min + egui::vec2(4.0, 4.0 + below), size))
            })
            .find(|(boxed, tag)| has_room(*boxed, *tag));
        let Some((boxed, tag)) = spot else { continue };
        labelled.add(boxed.min, tag.height());
        if tag.intersects(rect) {
            painter.rect_filled(tag, Rounding::same(3.0), *col);
            painter.galley(tag.min + egui::vec2(4.0, 2.0), galley, Color32::BLACK);
        }
    }
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

    /// Screen box of an outline.
    fn box_of(m: &Marks, k: usize) -> Rect {
        bounds(&m.found[k].0)
    }

    fn near(a: Rect, b: [f32; 4]) -> bool {
        [a.min.x - b[0], a.min.y - b[1], a.max.x - b[2], a.max.y - b[3]].iter().all(|d| d.abs() < 1e-3)
    }

    /// Group 4 over two rows: top-left and bottom-right. Shape 1,
    /// bottom-left, is in no group but inside the group's box. No pan,
    /// zoom 1.
    fn two_rows() -> Canvas {
        let mut c = Canvas { pan: [0.0, 0.0], zoom: 1.0, ..Default::default() };
        c.layers[0].shapes = vec![shape([0.0, 0.0], 4, &[]), shape([0.0, 20.0], 0, &[]), shape([20.0, 20.0], 4, &[])];
        c
    }

    /// A group spanning two rows: only its members stay out of the fade,
    /// so an object that just sits inside the box round them fades like
    /// any other, and the outline goes round the members only.
    #[test]
    fn only_a_groups_own_members_stay_bright() {
        let mut c = two_rows();
        let area = r(0.0, 0.0, 400.0, 400.0);
        let groups = MediaFilter { groups: true, ..Default::default() };
        let m = marks(area, &c, &groups, InFlight::default());
        assert_eq!(m.found.len(), 1);
        let whole = units::bounds_of(&c.layers[0], &[], &[0, 2]).unwrap();
        assert!(near(box_of(&m, 0), whole), "the outline is round the whole group, as before");
        let bright = |p: [f32; 2]| m.holes.iter().any(|h| h.contains(Pos2::new(p[0], p[1])));
        assert!(bright([5.0, 5.0]) && bright([25.0, 25.0]), "members stay bright");
        assert!(!bright([5.0, 25.0]), "the object in no group fades");
        assert!(cover::uncovered(area, &m.holes).iter().any(|d| d.contains(Pos2::new(5.0, 25.0))));
        // The outline follows the members: 10 px apart corner to corner,
        // more than it bridges, they are two islands, and the object in
        // no group is in neither.
        let inside = |p: [f32; 2]| cover::tests::inside(&m.found[0].0, Pos2::new(p[0], p[1]));
        assert_eq!(m.found[0].0.len(), 2);
        assert!(inside([5.0, 5.0]) && inside([25.0, 25.0]) && !inside([5.0, 25.0]));
        // Tagged as one, likewise.
        for i in [0, 2] {
            c.layers[0].shapes[i].set_tags(&["pose".to_string()]);
        }
        let tagged = MediaFilter { tagged: true, ..Default::default() };
        let m = marks(area, &c, &tagged, InFlight::default());
        assert!(near(box_of(&m, 0), whole));
        assert!(!m.holes.iter().any(|h| h.contains(Pos2::new(5.0, 25.0))));
    }

    fn picture(pos: [f32; 2], size: [f32; 2], group: u64) -> crate::canvas::shape::Shape {
        use crate::canvas::shape::{RasterKind, Shape};
        Shape::Raster {
            pos,
            size,
            png: std::sync::Arc::new(Vec::new()),
            px_size: [1, 1],
            color: [0; 4],
            rotation: 0.0,
            kind: RasterKind::Image,
            media: None,
            picture: None,
            pin: 0,
            flip: false,
            group,
            tags: Vec::new(),
            turn: 0.0,
            link: 0,
            paper: None,
        }
    }

    /// What `paint` draws in a 1000 × 800 area: each label's text and
    /// where it goes, in paint order, and how many outlines.
    fn drawn(c: &Canvas, f: &MediaFilter) -> (Vec<(String, Pos2)>, usize) {
        let ctx = egui::Context::default();
        let area = r(0.0, 0.0, 1000.0, 800.0);
        let input = egui::RawInput { screen_rect: Some(area), ..Default::default() };
        let out = ctx.run(input, |ctx| paint(&ctx.layer_painter(egui::LayerId::background()), area, c, f, InFlight::default(), true));
        let mut labels = Vec::new();
        let mut outlines = 0;
        for s in out.shapes {
            match s.shape {
                egui::Shape::Text(t) => labels.push((t.galley.text().to_string(), t.pos)),
                egui::Shape::Path(_) => outlines += 1,
                _ => {}
            }
        }
        (labels, outlines / 2)
    }

    #[test]
    fn a_label_needs_room_in_its_box() {
        let tag = |w: f32| Rect::from_min_size(Pos2::new(4.0, 4.0), egui::vec2(w, 17.0));
        let boxed = |w: f32, h: f32| r(0.0, 0.0, w, h);
        assert!(has_room(boxed(100.0, 100.0), tag(52.0)));
        assert!(has_room(boxed(60.0, 25.0), tag(52.0)), "just fits");
        assert!(!has_room(boxed(59.0, 100.0), tag(52.0)), "too narrow");
        assert!(!has_room(boxed(100.0, 24.0), tag(52.0)), "too short");
        // A long list of tags runs past a box 64 px wide or more.
        assert!(has_room(boxed(64.0, 100.0), tag(170.0)));
        assert!(!has_room(boxed(63.0, 100.0), tag(170.0)));
        // A label stacked lower down needs its own room.
        let second = Rect::from_min_size(Pos2::new(4.0, 24.0), egui::vec2(52.0, 17.0));
        assert!(!has_room(boxed(100.0, 40.0), second) && has_room(boxed(100.0, 45.0), second));
    }

    /// Zoomed in, every box is labelled; a box too small for its label
    /// is only outlined, and holds no place in the stack on its corner.
    /// Zoomed far out, only outlines.
    #[test]
    fn small_boxes_are_outlined_without_a_label() {
        let mut c = Canvas { pan: [0.0, 0.0], zoom: 1.0, ..Default::default() };
        // Group 7: a small picture at its top-left corner and a big one
        // just below; another big picture on its own.
        c.layers[0].shapes = vec![
            picture([0.0, 0.0], [20.0, 15.0], 7),
            picture([0.0, 20.0], [200.0, 150.0], 7),
            picture([300.0, 0.0], [200.0, 150.0], 0),
        ];
        let corner = |i: usize| {
            let b = c.layers[0].shapes[i].obb().aabb();
            Pos2::new(b[0] + 8.0, b[1] + 6.0)
        };
        let f = MediaFilter { pictures: true, groups: true, ..Default::default() };
        let (labels, outlines) = drawn(&c, &f);
        assert_eq!(outlines, 4);
        let got: Vec<&str> = labels.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(got, ["PICTURE", "PICTURE", "GROUP · 2"]);
        assert!(labels[0].1.distance(corner(1)) < 0.6 && labels[1].1.distance(corner(2)) < 0.6);
        assert!(labels[2].1.distance(corner(0)) < 0.6, "the group's label takes the corner the small picture left");
        c.zoom = 0.2;
        let (labels, outlines) = drawn(&c, &f);
        assert_eq!((labels.len(), outlines), (0, 4));
    }

    /// The same box with the same label is outlined once, a box under
    /// half a pixel off counts as the same (across a pixel's edge too),
    /// and another label on the same box stays.
    #[test]
    fn a_box_given_twice_is_outlined_once() {
        let mut c = Canvas { pan: [0.0, 0.0], zoom: 1.0, ..Default::default() };
        c.layers[0].shapes = vec![
            shape([0.9, 5.0], 1, &[]),  // GROUP · 1
            shape([1.2, 5.0], 2, &[]),  // 0.3 px off, other side of x = 1: the same
            shape([3.0, 5.0], 3, &[]),  // 2 px further: its own
            shape([3.0, 5.0], 0, &["x"]), // on 3's box, but #x
        ];
        let area = r(0.0, 0.0, 100.0, 100.0);
        let f = MediaFilter { groups: true, tagged: true, ..Default::default() };
        let m = marks(area, &c, &f, InFlight::default());
        let labels: Vec<&str> = m.found.iter().map(|(_, _, t)| t.as_str()).collect();
        assert_eq!(labels, ["GROUP · 1", "GROUP · 1", "#x"]);
        assert!((box_of(&m, 1).min.x - 3.0).abs() < 1e-3);
    }

    #[test]
    fn near_finds_exactly_what_is_within_half_a_pixel() {
        let mut seed = 7u32;
        let mut rnd = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            ((seed >> 8) % 4000) as f32 / 400.0 - 5.0
        };
        let pts: Vec<Pos2> = (0..400).map(|_| Pos2::new(rnd(), rnd())).collect();
        let mut near = Near::default();
        for (i, p) in pts.iter().enumerate() {
            near.add(*p, i);
        }
        for _ in 0..400 {
            let q = Pos2::new(rnd(), rnd());
            let mut got: Vec<usize> = near.around(q).map(|&(_, i)| i).collect();
            got.sort_unstable();
            let want: Vec<usize> = (0..pts.len()).filter(|&i| pts[i].distance(q) < 0.5).collect();
            assert_eq!(got, want, "around {q:?}");
        }
    }

    /// A group being moved, resized or turned, or its layer dragged,
    /// takes its highlight along before the gesture commits.
    #[test]
    fn highlights_follow_a_gesture_in_flight() {
        let c = two_rows();
        let area = r(-400.0, -400.0, 400.0, 400.0);
        let at = |b: [f32; 4]| [b[0] - 400.0, b[1] - 400.0, b[2] - 400.0, b[3] - 400.0];
        let groups = MediaFilter { groups: true, ..Default::default() };
        let whole = units::bounds_of(&c.layers[0], &[], &[0, 2]).unwrap();
        // The whole group dragged by (100, 50).
        let sel = Selection { strokes: vec![], shapes: vec![0, 2] };
        let moving = InFlight { sel: Some((0, &sel, Affine::translate([100.0, 50.0]))), layer: None };
        let m = marks(area, &c, &groups, moving);
        assert!(near(box_of(&m, 0), at([whole[0] + 100.0, whole[1] + 50.0, whole[2] + 100.0, whole[3] + 50.0])));
        let bright = |m: &Marks, p: [f32; 2]| m.holes.iter().any(|h| h.contains(Pos2::new(p[0] - 400.0, p[1] - 400.0)));
        assert!(bright(&m, [105.0, 55.0]) && !bright(&m, [5.0, 5.0]), "the bright spots move too");
        // Only one member taken along (Alt+click): the box stretches to it.
        let one = Selection { strokes: vec![], shapes: vec![2] };
        let moving = InFlight { sel: Some((0, &one, Affine::translate([100.0, 0.0]))), layer: None };
        let m = marks(area, &c, &groups, moving);
        assert!(near(box_of(&m, 0), at([whole[0], whole[1], whole[2] + 100.0, whole[3]])));
        // Another layer's gesture leaves this one alone.
        let moving = InFlight { sel: Some((1, &sel, Affine::translate([100.0, 50.0]))), layer: Some((1, 2.0, [7.0, 0.0])) };
        assert!(near(box_of(&marks(area, &c, &groups, moving), 0), at(whole)));
        // The layer dragged and scaled with Shift+Space: p → 2p + (7, 0).
        let moving = InFlight { sel: None, layer: Some((0, 2.0, [7.0, 0.0])) };
        let m = marks(area, &c, &groups, moving);
        assert!(near(box_of(&m, 0), at([2.0 * whole[0] + 7.0, 2.0 * whole[1], 2.0 * whole[2] + 7.0, 2.0 * whole[3]])));
    }

    #[test]
    fn what_the_board_dropped_is_counted_and_matched() {
        use crate::canvas::board_link::{BoardLink, ItemState, LinkedItem};
        let text = |link: u64| crate::canvas::shape::Shape::Text {
            pos: [0.0, 0.0],
            content: "x".into(),
            font_size: 10.0,
            color: [255; 4],
            rotation: 0.0,
            pin: 0,
            group: 0,
            tags: Vec::new(),
            turn: 0.0,
            background: None,
            link,
        };
        let mut c = Canvas::default();
        c.layers[0].shapes = vec![text(1), text(2), text(0)];
        assert_eq!(counts(&c).gone, None, "not linked to a board: no row");
        let base = ItemState { at: [0.0; 2], size: [10.0, 0.0], rotation: 0.0, content: 0, style: 0, group: None, tags: Vec::new() };
        let item = |link, gone| LinkedItem { link, key: link as i64, base: base.clone(), gone };
        c.board = Some(Box::new(BoardLink { path: "b.pur".into(), scale: 1.0, offset: [0.0; 2], items: vec![item(1, false), item(2, true)], groups: Vec::new() }));
        assert_eq!(counts(&c).gone, Some(1));
        let gone = MediaFilter { gone: true, ..Default::default() };
        assert!(gone.any());
        assert_eq!(matches_on(&c, 0, &gone), (vec![], vec![1]));
        // Its box is marked, and follows it while it is dragged.
        let area = Rect::from_min_max(Pos2::new(-400.0, -400.0), Pos2::new(400.0, 400.0));
        let still = marks(area, &c, &gone, InFlight::default());
        assert_eq!(still.found.len(), 1);
        let sel = crate::canvas::hit::Selection { strokes: vec![], shapes: vec![1] };
        let moving = InFlight { sel: Some((0, &sel, Affine::translate([30.0, 0.0]))), layer: None };
        let moved = marks(area, &c, &gone, moving);
        assert!((box_of(&moved, 0).min - box_of(&still, 0).min - egui::vec2(30.0, 0.0)).length() < 1e-3);
        assert!(moved.holes.iter().any(|h| h.contains(box_of(&moved, 0).center())));
    }

    /// Times the per-frame work on a real slide: `OSN_PERF_SLIDE` names a
    /// copy of a slide file (only read).
    #[test]
    #[ignore]
    fn counts_on_a_real_slide() {
        let path = std::env::var("OSN_PERF_SLIDE").expect("set OSN_PERF_SLIDE to a copy of a slide file");
        let text = std::fs::read_to_string(path).unwrap();
        let c = crate::persistence::parse_canvas(&text, 1).unwrap();
        let shapes: usize = c.layers.iter().map(|l| l.shapes.len()).sum();
        let tagged = c.layers.iter().flat_map(|l| &l.shapes).filter(|s| !s.tags().is_empty()).count();
        println!("{shapes} shapes, {tagged} tagged, {} link items", c.board.as_ref().map_or(0, |b| b.items.len()));
        let n = 20;
        let t = std::time::Instant::now();
        for _ in 0..n {
            std::hint::black_box(counts(&c));
        }
        println!("counts: {:?} a frame", t.elapsed() / n);
        let t = std::time::Instant::now();
        for _ in 0..n {
            let links = gone_links(&c);
            std::hint::black_box(shown(&c).flat_map(|l| &l.shapes).filter(|s| links.contains(&s.link())).count());
        }
        println!("  of which gone: {:?}", t.elapsed() / n);
        let all = MediaFilter { tagged: true, gone: true, ..Default::default() };
        let t = std::time::Instant::now();
        for _ in 0..n {
            std::hint::black_box(matches_on(&c, 0, &all));
        }
        println!("matches_on (all tagged + gone): {:?}", t.elapsed() / n);
        // What `paint` works out each frame with the whole slide in view:
        // the outlines and the fade's cut-outs, one per highlighted object.
        let mut c = c;
        let area = Rect::from_min_size(Pos2::ZERO, egui::vec2(1920.0, 1080.0));
        let all: Vec<[f32; 4]> = c.layers.iter().flat_map(|l| &l.shapes).map(|s| s.obb().aabb()).collect();
        if let Some(b) = all.into_iter().reduce(|a, b| [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]) {
            c.zoom = (1920.0 / (b[2] - b[0])).min(1080.0 / (b[3] - b[1]));
            c.pan = [-b[0] * c.zoom, -b[1] * c.zoom];
        }
        // The groups' outlines alone, and what the two passes of
        // `uncovered` in them cost.
        let place = Place::new(area, &c, 0, InFlight::default()).unwrap();
        let groups: Vec<Vec<Rect>> = units::groups(&c.layers[0])
            .into_iter()
            .map(|u| u.shapes.iter().filter_map(|&i| place.shape(i)).map(|q| Rect::from_points(&q)).collect())
            .collect();
        let t = std::time::Instant::now();
        for _ in 0..n {
            for g in &groups {
                std::hint::black_box(cover::outline(g, BRIDGE_PX));
            }
        }
        println!("{} group outlines: {:?} a frame", groups.len(), t.elapsed() / n);
        let t = std::time::Instant::now();
        for _ in 0..n {
            for g in &groups {
                let all = g.iter().copied().reduce(|a, b| a.union(b)).unwrap();
                let area = all.expand(13.0);
                let grown: Vec<Rect> = g.iter().map(|b| b.expand(6.0)).collect();
                let outside: Vec<Rect> = cover::uncovered(area, &grown).into_iter().map(|o| o.expand(6.0)).collect();
                std::hint::black_box(cover::uncovered(area, &outside));
            }
        }
        println!("  of which the two uncovered passes: {:?}", t.elapsed() / n);
        let filters = [
            ("groups", MediaFilter { groups: true, ..Default::default() }),
            ("pictures", MediaFilter { pictures: true, ..Default::default() }),
            ("everything", MediaFilter { pictures: true, gifs: true, videos: true, groups: true, tagged: true, ..Default::default() }),
        ];
        for (name, f) in filters {
            let t = std::time::Instant::now();
            let mut rects = 0;
            for _ in 0..n {
                let m = marks(area, &c, &f, InFlight::default());
                rects = cover::uncovered(area, &m.holes).len();
                std::hint::black_box(&m.found);
            }
            println!("paint work, {name}: {:?} a frame, {rects} faded rects", t.elapsed() / n);
            let m = marks(area, &c, &f, InFlight::default());
            let t = std::time::Instant::now();
            for _ in 0..n {
                std::hint::black_box(cover::uncovered(area, &m.holes));
            }
            println!("  of which uncovered: {:?} ({} holes)", t.elapsed() / n, m.holes.len());
            // The whole frame's share: painting, and egui turning it into
            // triangles.
            let ctx = egui::Context::default();
            let frame = || {
                let input = egui::RawInput { screen_rect: Some(area), ..Default::default() };
                let out = ctx.run(input, |ctx| paint(&ctx.layer_painter(egui::LayerId::background()), area, &c, &f, InFlight::default(), true));
                ctx.tessellate(out.shapes, out.pixels_per_point)
            };
            frame();
            let t = std::time::Instant::now();
            for _ in 0..n {
                std::hint::black_box(frame());
            }
            println!("  paint + tessellate: {:?} a frame", t.elapsed() / n);
        }
    }
}
