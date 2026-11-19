//! Find (Ctrl+F): a search box that finds things on every slide by what
//! they say or how they are tagged: text labels by their text, and any
//! object by its tags (`Canvas::set_tags`) — a group as one when every
//! member matches. Matches are listed with their slide, in slide
//! order. `↑` / `↓` step through the list and show each match (the first
//! press shows the highlighted one), as does a click on one; `Enter` goes
//! to the highlighted match and closes, staying there, as does a click
//! outside; `Esc` closes and goes back to the slide and view find started
//! from. Going there is the app's job (`App::go_to_find_hit`): the slide,
//! the view gliding to the match, and a flash on it.
//!
//! A picture pasted (`Ctrl+V`) or dropped while the bar is open, or the
//! selected picture's "find similar", is looked for instead: the bar
//! lists the pictures, GIFs and videos like it (`tools::similar`),
//! copies first, each with a thumbnail and how alike it is. Or it lists
//! every set of copies on the slides ("find every duplicate picture"),
//! each set with a button that selects all of its copies on one slide
//! but one, for deleting, and each copy with a button that deletes it
//! where it is, whatever slide that is; the last copy of a set is kept.
//! Typing goes back to finding words.
//!
//! With picture search on, typed words also find the pictures that show
//! them (`canvas::picture_vectors`), listed after the labels and tags;
//! and a single `#tag` lists the untagged pictures that look like the
//! ones tagged so ("probably #tag", `tools::tag_suggest`), each with a
//! button to tag it and one to turn it down.
//!
//! With nothing typed, the bar lists every tag in the app with how many
//! slides and objects carry it (also from the command palette's "Every
//! tag"): a click finds what has it; each can be renamed or removed
//! everywhere.
//!
//! Only slides already read are searched: the bar says how many are
//! still opening. The query is kept between opens, selected so that
//! typing replaces it, as in a browser.

use crate::canvas::canvas::{Canvas, Layer};
use crate::canvas::units::{bounds_of, same_tag, tagged_by};
use crate::canvas::shape::{parse_tags, MediaType, RasterKind, Shape};
use crate::canvas::TagCount;
use crate::ui::media_filter::TAG_COLOUR;
use crate::tools::similar::{self, Kind, Likeness, Origin, Query, Signature};
use crate::ui::theme;
use egui::text::{CCursor, CCursorRange, LayoutJob, TextFormat};
use egui::{Color32, ColorImage, FontId, TextureHandle, TextureOptions, Vec2};
use std::collections::HashMap;
use std::sync::{Arc, Weak};

/// The colour a match is marked in, here and on the canvas.
pub const FOUND: Color32 = Color32::from_rgb(255, 196, 64);

/// What matched.
#[derive(Debug, Clone, PartialEq)]
pub enum Found {
    /// A text label, by its text: its index in the layer's shapes.
    Label(usize),
    /// Objects, by their tags: one object, or every member of a group.
    /// `what` says which ("Picture", "Group of 4").
    Tagged { strokes: Vec<usize>, shapes: Vec<usize>, what: String },
}

/// One match.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub slide: usize,
    pub layer: usize,
    pub found: Found,
    /// A label's text, or the tags as `#one  #two`, on one line, cut
    /// down around the match.
    pub snippet: String,
    /// Where the match is in `snippet`, in bytes: one range for a label,
    /// one per word found for tags. In order, not overlapping.
    pub marks: Vec<std::ops::Range<usize>>,
    /// It sits on a hidden layer, so it is not on screen.
    pub hidden: bool,
}

/// Longest snippet, in characters.
const SNIPPET: usize = 56;

/// Everything on `slides` that holds `query`, case aside, with line
/// breaks and runs of spaces read as one space: text labels by their
/// text, objects and groups by their tags — every word of the query
/// (`body sketch`, `#body, #sketch`) in some tag of theirs. In the order
/// given, then top to bottom and left to right on each slide. A lone
/// label found by its text is not listed again for its tags.
pub fn search<'a>(query: &str, slides: impl IntoIterator<Item = (usize, &'a Canvas)>) -> Vec<Hit> {
    let needle: Vec<char> = fold(&flat(query));
    if needle.is_empty() {
        return Vec::new();
    }
    let words = words(query);
    let mut out = Vec::new();
    for (slide, c) in slides {
        let mut here: Vec<([f32; 2], Hit)> = Vec::new();
        for (layer, l) in c.layers.iter().enumerate() {
            let mut labels = Vec::new();
            for (shape, s) in l.shapes.iter().enumerate() {
                let Shape::Text { content, .. } = s else { continue };
                let text: Vec<char> = flat(content).chars().collect();
                let Some(at) = find(&fold_chars(&text), &needle) else { continue };
                let (snippet, marks) = snippet(&text, &[(at, needle.len())]);
                let b = s.obb().aabb();
                labels.push(shape);
                here.push(([b[1], b[0]], Hit { slide, layer, found: Found::Label(shape), snippet, marks, hidden: !l.visible }));
            }
            // An object by its own tags; a group whole only when every
            // member matches (see `units::tagged_by`).
            let says = |tags: &[String]| find_each(&fold_chars(&tag_text(tags)), &words).is_some();
            for unit in tagged_by(l, says) {
                if unit.strokes.is_empty() && unit.shapes.len() == 1 && labels.contains(&unit.shapes[0]) {
                    continue;
                }
                let text = tag_text(&unit.tags);
                let Some(at) = find_each(&fold_chars(&text), &words) else { continue };
                let Some(b) = bounds_of(l, &unit.strokes, &unit.shapes) else { continue };
                let (snippet, marks) = snippet(&text, &at);
                let what = describe(l, &unit.strokes, &unit.shapes);
                let found = Found::Tagged { strokes: unit.strokes, shapes: unit.shapes, what };
                here.push(([b[1], b[0]], Hit { slide, layer, found, snippet, marks, hidden: !l.visible }));
            }
        }
        here.sort_by(|a, b| a.0[0].total_cmp(&b.0[0]).then(a.0[1].total_cmp(&b.0[1])));
        out.extend(here.into_iter().map(|(_, h)| h));
    }
    out
}

/// What these objects are, in a word or two.
fn describe(l: &Layer, strokes: &[usize], shapes: &[usize]) -> String {
    match (strokes, shapes) {
        ([_], []) => "Stroke".into(),
        ([], [i]) => match &l.shapes[*i] {
            s @ Shape::Raster { .. } => match s.media_type() {
                Some(MediaType::Picture) => "Picture",
                Some(MediaType::Gif) => "GIF",
                Some(MediaType::Video) => "Video",
                None => "Fill",
            }
            .into(),
            Shape::Rect { .. } => "Rectangle".into(),
            Shape::Ellipse { .. } => "Ellipse".into(),
            Shape::Line { .. } => "Line".into(),
            Shape::Arrow { .. } => "Arrow".into(),
            Shape::Text { .. } => "Label".into(),
        },
        _ => format!("Group of {}", strokes.len() + shapes.len()),
    }
}

/// `s` on one line: any run of white space becomes one space.
fn flat(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Lower case, one char for one char, so a match's place carries over.
fn fold(s: &str) -> Vec<char> {
    fold_chars(&s.chars().collect::<Vec<_>>())
}

fn fold_chars(s: &[char]) -> Vec<char> {
    s.iter().map(|c| c.to_lowercase().next().unwrap_or(*c)).collect()
}

/// The words of a query, folded, as tags are typed (`parse_tags`):
/// between spaces or commas. A `#` stays, so `#pose` finds tags that
/// start with it.
fn words(query: &str) -> Vec<Vec<char>> {
    query.split(|c: char| c == ',' || c.is_whitespace()).filter(|w| !w.is_empty()).map(fold).collect()
}

/// Tags as one line, `#one  #two`.
fn tag_text(tags: &[String]) -> Vec<char> {
    tags.iter().map(|t| format!("#{t}")).collect::<Vec<_>>().join("  ").chars().collect()
}

/// Where `needle` first starts in `hay`, in chars.
fn find(hay: &[char], needle: &[char]) -> Option<usize> {
    (0..=hay.len().checked_sub(needle.len())?).find(|&i| hay[i..i + needle.len()] == *needle)
}

/// Where each of `words` first is in `hay` (start, length, in chars);
/// `None` when one is missing, or there are none.
fn find_each(hay: &[char], words: &[Vec<char>]) -> Option<Vec<(usize, usize)>> {
    if words.is_empty() {
        return None;
    }
    words.iter().map(|w| find(hay, w).map(|at| (at, w.len()))).collect()
}

/// At most `SNIPPET` chars of `text` around the first of the matches
/// `found` (start, length, in chars), with an ellipsis where it was
/// cut, and the matches' byte ranges in it: in order, overlaps joined,
/// cut where the text is.
fn snippet(text: &[char], found: &[(usize, usize)]) -> (String, Vec<std::ops::Range<usize>>) {
    let n = text.len();
    let first = found.iter().map(|f| f.0).min().unwrap_or(0);
    let start = if n <= SNIPPET { 0 } else { first.saturating_sub(16).min(n - SNIPPET) };
    let end = (start + SNIPPET).min(n);
    let mut s = String::new();
    if start > 0 {
        s.push('…');
    }
    // Where each char from `start` to `end` begins in `s`.
    let mut byte = Vec::with_capacity(end - start + 1);
    for c in &text[start..end] {
        byte.push(s.len());
        s.push(*c);
    }
    byte.push(s.len());
    let mut spans: Vec<(usize, usize)> =
        found.iter().map(|&(at, len)| (at.clamp(start, end), (at + len).clamp(start, end))).filter(|(a, b)| a < b).collect();
    spans.sort_unstable();
    let mut marks: Vec<std::ops::Range<usize>> = Vec::new();
    for (a, b) in spans {
        let r = byte[a - start]..byte[b - start];
        match marks.last_mut() {
            Some(m) if r.start <= m.end => m.end = m.end.max(r.end),
            _ => marks.push(r),
        }
    }
    if end < n {
        s.push('…');
    }
    (s, marks)
}

/// A picture find looks for others like.
pub struct PictureQuery {
    query: Query,
    origin: Origin,
    /// What it is, for the bar: "the pasted picture", a file's name.
    label: String,
    thumb: ColorImage,
    texture: Option<TextureHandle>,
    /// Its matches, and what they were found from (`update_pictures`).
    results: Vec<PictureHit>,
    results_for: Option<(u64, usize)>,
    /// The picture shrunk for reading its vector (`ml::prep::shrink`,
    /// `canvas::picture_vectors`); gone once read or given up on.
    probe: Option<image::RgbaImage>,
    /// What it shows, once read: pictures of the same subject are listed.
    vector: Option<Arc<[f32]>>,
    /// The vector asked for, while it is read.
    asked: Option<u64>,
}

impl PictureQuery {
    /// Look for pictures like `img`. `None` for an empty picture.
    pub fn new(img: &image::RgbaImage, label: &str, origin: Origin) -> Option<Self> {
        let query = Query::of(img)?;
        let probe = crate::ml::prep::shrink(img);
        Some(Self {
            query,
            origin,
            label: label.to_string(),
            thumb: thumb_of(img),
            texture: None,
            results: Vec::new(),
            results_for: None,
            probe: Some(probe),
            vector: None,
            asked: None,
        })
    }

    pub fn origin(&self) -> &Origin {
        &self.origin
    }

    pub fn has_vector(&self) -> bool {
        self.vector.is_some()
    }

    /// What it shows, read: from now on the pictures listed besides its
    /// copies are those of the same subject.
    pub fn set_vector(&mut self, v: Arc<[f32]>) {
        self.vector = Some(v);
        self.probe = None;
        self.asked = None;
    }

    /// The picture to read its vector from, until read or given up on.
    pub fn probe(&self) -> Option<&image::RgbaImage> {
        self.probe.as_ref()
    }

    /// The vector asked for, while it is read.
    pub fn asked(&self) -> Option<u64> {
        self.asked
    }

    pub fn set_asked(&mut self, id: Option<u64>) {
        self.asked = id;
    }

    /// Its vector could not be read: list pictures that look alike.
    pub fn give_up_vector(&mut self) {
        self.probe = None;
        self.asked = None;
    }
}

/// A picture like the query.
#[derive(Clone)]
pub struct PictureHit {
    pub hit: Hit,
    pub likeness: Likeness,
    /// Its bytes, for the thumbnail.
    png: Arc<Vec<u8>>,
    /// A picture suggested for a tag: likely, not only maybe.
    likely: bool,
}

/// The pictures on `slides` like `q`'s, copies first. `signature` gives
/// a picture's signature from its bytes, once read; `vector` its vector.
/// Copies are told by how they look (`similar::search`). Once the
/// query's vector is known, the rest are the pictures of the same
/// subject (`similar::by_subject`), else those that look alike.
pub fn picture_search<'a>(
    q: &PictureQuery,
    slides: impl IntoIterator<Item = (usize, &'a Canvas)>,
    signature: impl Fn(&Arc<Vec<u8>>) -> Option<Signature>,
    vector: impl Fn(&Arc<Vec<u8>>) -> Option<Arc<[f32]>>,
) -> Vec<PictureHit> {
    let slides: Vec<(usize, &Canvas)> = slides.into_iter().collect();
    let mut found = similar::search(&q.query, &q.origin, slides.iter().copied(), signature);
    if let Some(qv) = &q.vector {
        found.retain(|m| m.likeness.kind != Kind::Alike);
        let listed: std::collections::HashSet<(usize, usize, usize)> = found.iter().map(|m| (m.slide, m.layer, m.shape)).collect();
        let mut places = Vec::new();
        let mut vectors = Vec::new();
        for (slide, c) in &slides {
            for (layer, l) in c.layers.iter().enumerate() {
                for (shape, s) in l.shapes.iter().enumerate() {
                    let Shape::Raster { png, kind: RasterKind::Image, .. } = s else { continue };
                    let at = (*slide, layer, shape);
                    if q.origin.at == Some(at) || listed.contains(&at) {
                        continue;
                    }
                    if let Some(v) = vector(png) {
                        places.push((at, png));
                        vectors.push(v);
                    }
                }
            }
        }
        for (i, score) in similar::by_subject(qv, vectors.iter().enumerate().map(|(i, v)| (i, &v[..]))) {
            let ((slide, layer, shape), png) = places[i];
            let likeness = Likeness { kind: Kind::Alike, score, way: Default::default(), other_crop: false };
            found.push(similar::Match { slide, layer, shape, likeness, png: Arc::clone(png) });
        }
    }
    found
        .into_iter()
        .filter_map(|m| {
            let c = slides.iter().find(|(i, _)| *i == m.slide)?.1;
            picture_hit(c, m.slide, m.layer, m.shape, m.likeness, m.png)
        })
        .collect()
}

/// The row for the picture at `(slide, layer, shape)` of `c`.
pub fn picture_hit(c: &Canvas, slide: usize, layer: usize, shape: usize, likeness: Likeness, png: Arc<Vec<u8>>) -> Option<PictureHit> {
    let l = c.layers.get(layer)?;
    let what = describe(l, &[], &[shape]);
    let found = Found::Tagged { strokes: Vec::new(), shapes: vec![shape], what };
    let hit = Hit { slide, layer, found, snippet: String::new(), marks: Vec::new(), hidden: !l.visible };
    Some(PictureHit { hit, likeness, png, likely: false })
}

/// The pictures on `slides` that show typed words whose vector is
/// `words` (`similar::by_words`); `vector` gives a picture's vector.
pub fn word_picture_search<'a>(
    words: &[f32],
    slides: impl IntoIterator<Item = (usize, &'a Canvas)>,
    vector: impl Fn(&Arc<Vec<u8>>) -> Option<Arc<[f32]>>,
) -> Vec<PictureHit> {
    let slides: Vec<(usize, &Canvas)> = slides.into_iter().collect();
    let mut places = Vec::new();
    let mut vectors = Vec::new();
    for (slide, c) in &slides {
        for (layer, l) in c.layers.iter().enumerate() {
            for (shape, s) in l.shapes.iter().enumerate() {
                let Shape::Raster { png, kind: RasterKind::Image, .. } = s else { continue };
                if let Some(v) = vector(png) {
                    places.push((*slide, layer, shape, png, *c));
                    vectors.push(v);
                }
            }
        }
    }
    similar::by_words(words, vectors.iter().enumerate().map(|(i, v)| (i, &v[..])))
        .into_iter()
        .filter_map(|(i, score)| {
            let (slide, layer, shape, png, c) = places[i];
            let likeness = Likeness { kind: Kind::Alike, score, way: Default::default(), other_crop: false };
            picture_hit(c, slide, layer, shape, likeness, Arc::clone(png))
        })
        .collect()
}

/// The pictures on the slides as `tools::tag_suggest` sees them for one
/// tag: those tagged so, those turned down for it, and the rest that may
/// be suggested (not on a locked slide).
pub struct TagPool {
    tagged: Vec<Arc<[f32]>>,
    turned_down: Vec<Arc<[f32]>>,
    places: Vec<Candidate>,
}

/// A picture that may be suggested: slide, layer, shape, bytes, vector.
type Candidate = (usize, usize, usize, Arc<Vec<u8>>, Arc<[f32]>);

impl TagPool {
    /// Look through `slides` for `tag`. `locked` says a slide is locked;
    /// `vector` gives a picture's vector, `rejected` whether it was
    /// turned down for the tag.
    pub fn gather<'a>(
        tag: &str,
        slides: impl IntoIterator<Item = (usize, &'a Canvas)>,
        locked: impl Fn(usize) -> bool,
        vector: impl Fn(&Arc<Vec<u8>>) -> Option<Arc<[f32]>>,
        rejected: impl Fn(&Arc<Vec<u8>>) -> bool,
    ) -> TagPool {
        let mut pool = TagPool { tagged: Vec::new(), turned_down: Vec::new(), places: Vec::new() };
        for (si, c) in slides {
            let locked = locked(si);
            for (li, l) in c.layers.iter().enumerate() {
                for (shi, s) in l.shapes.iter().enumerate() {
                    let Shape::Raster { png, kind: RasterKind::Image, .. } = s else { continue };
                    let Some(v) = vector(png) else { continue };
                    if s.tags().iter().any(|t| same_tag(t, tag)) {
                        pool.tagged.push(v);
                    } else if rejected(png) {
                        pool.turned_down.push(v);
                    } else if !locked {
                        pool.places.push((si, li, shi, Arc::clone(png), v));
                    }
                }
            }
        }
        pool
    }

    /// How many pictures carry the tag, and how many are turned down: the
    /// suggestions change when either does.
    pub fn counts(&self) -> (usize, usize) {
        (self.tagged.len(), self.turned_down.len())
    }

    /// The suggestions, most likely first. `slides` as given to `gather`.
    pub fn suggest<'a>(&self, slides: impl IntoIterator<Item = (usize, &'a Canvas)>) -> Vec<PictureHit> {
        let slides: Vec<(usize, &Canvas)> = slides.into_iter().collect();
        let t: Vec<&[f32]> = self.tagged.iter().map(|v| &v[..]).collect();
        let r: Vec<&[f32]> = self.turned_down.iter().map(|v| &v[..]).collect();
        crate::tools::tag_suggest::suggest(&t, &r, self.places.iter().enumerate().map(|(i, p)| (i, &p.4[..])))
            .into_iter()
            .filter_map(|s| {
                let (si, li, shi, png, _) = &self.places[s.id];
                let c = slides.iter().find(|(k, _)| k == si)?.1;
                let likeness = Likeness { kind: Kind::Alike, score: s.close, way: Default::default(), other_crop: false };
                let mut hit = picture_hit(c, *si, *li, *shi, likeness, Arc::clone(png))?;
                hit.likely = s.likely;
                Some(hit)
            })
            .collect()
    }
}

/// Pictures that show the words typed, or that probably carry the one
/// `#tag` typed, listed after the label and tag matches while picture
/// search is on.
#[derive(Default)]
struct WordPictures {
    /// What is typed that they are for.
    query: String,
    /// The tag they are suggested for: `None` for typed words.
    tag: Option<String>,
    results: Vec<PictureHit>,
    /// What they were found from (`set_word_pictures`).
    results_for: Option<(u64, usize)>,
    /// Said after the count while they are not ready: "Looking at
    /// pictures…", "Downloading the text model…".
    note: Option<String>,
}

/// Every set of copies on the slides, as the bar lists them.
#[derive(Default)]
pub struct Copies {
    /// Every copy, set after set; `sets` cuts the list into its sets.
    results: Vec<PictureHit>,
    sets: Vec<std::ops::Range<usize>>,
    /// What they were found from, once found (`set_copies`).
    results_for: Option<(u64, usize)>,
}

/// What a click on the bar asks of the app.
pub enum Request {
    /// Go to `slide` and select these pictures on `layer`: a set's
    /// copies there, all but one.
    SelectExtras { slide: usize, layer: usize, shapes: Vec<usize> },
    /// Change a tag everywhere.
    Tag(TagAction),
    /// Tag the picture at `slide`, `layer`, `shape` with `tag`: a
    /// suggestion taken.
    TagPicture { slide: usize, layer: usize, shape: usize, tag: String },
    /// The picture there is not `tag`: suggest it for it no more.
    NotThis { slide: usize, layer: usize, shape: usize, tag: String },
    /// Delete this copy where it is: `png` is its bytes, to make sure it
    /// is still the picture listed.
    DeleteCopy { slide: usize, layer: usize, shape: usize, png: Arc<Vec<u8>> },
}

/// A tag changed everywhere from the tag list.
pub enum TagAction {
    /// Put these tags in place of this one.
    Rename(String, Vec<String>),
    Remove(String),
}

/// A small copy of `img` to show, its long side at most `THUMB` pixels.
fn thumb_of(img: &image::RgbaImage) -> ColorImage {
    let (w, h) = img.dimensions();
    let s = (THUMB as f32 / w.max(h).max(1) as f32).min(1.0);
    let (tw, th) = (((w as f32 * s).round() as u32).max(1), ((h as f32 * s).round() as u32).max(1));
    let small = image::imageops::thumbnail(img, tw, th);
    ColorImage::from_rgba_unmultiplied([tw as usize, th as usize], small.as_raw())
}

/// Long side of a thumbnail, in pixels.
const THUMB: u32 = 64;
/// Thumbnails made per frame at most; the rest come in the next frames.
const THUMBS_PER_FRAME: usize = 3;

/// A result's thumbnail, by the address of its bytes.
struct Thumb {
    /// Keeps the address from going to other bytes while it is a key.
    _png: Weak<Vec<u8>>,
    texture: Option<TextureHandle>,
}

/// The thumbnail of a picture's bytes: made now if `budget` allows, else
/// in a later frame.
fn thumb(ctx: &egui::Context, cache: &mut HashMap<usize, Thumb>, png: &Arc<Vec<u8>>, budget: &mut usize) -> Option<TextureHandle> {
    let key = Arc::as_ptr(png) as usize;
    if let Some(t) = cache.get(&key) {
        return t.texture.clone();
    }
    if *budget == 0 {
        ctx.request_repaint();
        return None;
    }
    *budget -= 1;
    let texture = image::load_from_memory(png).ok().map(|img| ctx.load_texture(format!("osn_find_thumb_{key}"), thumb_of(&img.to_rgba8()), TextureOptions::LINEAR));
    cache.insert(key, Thumb { _png: Arc::downgrade(png), texture: texture.clone() });
    texture
}

/// The find bar's state between frames.
#[derive(Default)]
pub struct FindBar {
    open: bool,
    query: String,
    /// Set while looking for pictures like one.
    picture: Option<PictureQuery>,
    /// What is typed while looking for a picture: once there is any, it
    /// becomes the query.
    typed: String,
    thumbs: HashMap<usize, Thumb>,
    /// Textures no longer shown, freed next frame (`free_retired`).
    retired: Vec<TextureHandle>,
    /// Set while listing every set of copies.
    copies: Option<Copies>,
    /// Pictures that show the typed words.
    words: WordPictures,
    /// What a click asked of the app, until it takes it.
    request: Option<Request>,
    /// The tag being renamed in the tag list, and the name typed so far.
    tag_renaming: Option<(String, String)>,
    /// The rename field has yet to take the keyboard.
    tag_focus: bool,
    /// The tag whose removal waits for a yes.
    tag_removing: Option<String>,
    /// The highlighted row.
    cursor: usize,
    /// Esc closed it (`take_cancelled`).
    cancelled: bool,
    /// Go to the highlighted row in the next frame, as an arrow key would:
    /// the one shown was deleted and another took its place.
    go_to_cursor: bool,
    /// The highlighted row is the one last gone to, so the next `Enter`
    /// moves on.
    shown: bool,
    /// Put the cursor at the first match from the shown slide on, once
    /// there is a list to look in.
    restart: bool,
    /// The text field has yet to take the keyboard; with its text selected.
    focus_pending: bool,
}

/// Most rows shown at once, of words and of pictures.
const ROWS: usize = 8;
const PICTURE_ROWS: usize = 7;
/// With pictures that show the typed words: label and tag rows, then
/// picture rows.
const WORD_ROWS_WITH_PICTURES: usize = 4;
const WORD_PICTURE_ROWS: usize = 4;
/// Room beside a suggested picture's row for its two buttons, and
/// beside a copy's row for its delete button.
const SUGGESTION_BUTTONS: f32 = 112.0;
const DELETE_BUTTON: f32 = 60.0;
/// Height of a picture's row, and of its thumbnail.
const PICTURE_ROW: f32 = 40.0;
const PICTURE_THUMB: f32 = 32.0;

/// How each kind of likeness is marked.
fn kind_colour(k: Kind) -> Color32 {
    match k {
        Kind::Duplicate => FOUND,
        Kind::Edited => Color32::from_rgb(255, 226, 160),
        Kind::Alike => theme::TEXT_PRIMARY,
    }
}
const WIDTH: f32 = 520.0;

impl FindBar {
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Open it, with the last query selected.
    pub fn open(&mut self) {
        self.open = true;
        self.cancelled = false;
        self.shown = false;
        self.restart = true;
        self.focus_pending = true;
        if let Some(p) = &mut self.picture {
            p.results_for = None;
        }
        if let Some(c) = &mut self.copies {
            c.results_for = None;
        }
    }

    /// Close it. The picture looked for stays, as the query does; its
    /// results and thumbnails go.
    pub fn close(&mut self) {
        self.open = false;
        self.tag_renaming = None;
        self.tag_removing = None;
        self.retire_thumbs();
        if let Some(p) = &mut self.picture {
            p.results.clear();
            p.results_for = None;
            self.retired.extend(p.texture.take());
        }
        if let Some(c) = &mut self.copies {
            *c = Copies::default();
        }
        self.words = WordPictures::default();
    }

    /// Open it with nothing typed, so it lists every tag.
    pub fn open_tags(&mut self) {
        self.retire_picture();
        self.query.clear();
        self.open();
    }

    /// Open it listing every set of copies on the slides.
    pub fn find_copies(&mut self) {
        self.retire_picture();
        self.copies = Some(Copies::default());
        self.typed.clear();
        self.open();
    }

    /// It is open, listing every set of copies.
    pub fn has_copies(&self) -> bool {
        self.open && self.copies.is_some()
    }

    /// What the copies listed were found from; `None` before any are.
    pub fn copies_found_for(&self) -> Option<(u64, usize)> {
        self.copies.as_ref().and_then(|c| c.results_for)
    }

    /// The sets of copies found from `key` (see `update_pictures`). The
    /// highlighted copy stays highlighted.
    pub fn set_copies(&mut self, sets: Vec<Vec<PictureHit>>, key: (u64, usize)) {
        let Some(c) = self.copies.as_mut() else { return };
        let current = c.results.get(self.cursor).map(|h| (h.hit.slide, h.hit.layer, h.hit.found.clone()));
        c.results.clear();
        c.sets.clear();
        for set in sets {
            let start = c.results.len();
            c.results.extend(set);
            c.sets.push(start..c.results.len());
        }
        c.results_for = Some(key);
        if let Some(cur) = current {
            if let Some(i) = c.results.iter().position(|h| (h.hit.slide, h.hit.layer, &h.hit.found) == (cur.0, cur.1, &cur.2)) {
                self.cursor = i;
            }
        }
    }

    /// A copy listed was deleted (`Request::DeleteCopy`): take it out of
    /// its set, and move the copies after it on that layer down one, as
    /// the layer's shapes did. A set left with one copy goes. The
    /// highlight stays on the copy it was on; if that was the one deleted,
    /// it moves to the copy that took its place, and the bar goes there.
    pub fn forget_copy(&mut self, slide: usize, layer: usize, shape: usize) {
        let Some(c) = self.copies.as_mut() else { return };
        let cursor = self.cursor;
        let mut deleted_lit = false;
        // Each copy kept, its set, and where it was.
        let mut kept: Vec<(PictureHit, usize, usize)> = Vec::new();
        for (set_no, range) in c.sets.iter().enumerate() {
            for (k, h) in c.results[range.clone()].iter().enumerate() {
                let was = range.start + k;
                let mut h = h.clone();
                if let Found::Tagged { shapes, .. } = &mut h.hit.found {
                    if h.hit.slide == slide && h.hit.layer == layer {
                        if shapes.first() == Some(&shape) {
                            deleted_lit |= was == cursor;
                            continue;
                        }
                        shapes.iter_mut().filter(|s| **s > shape).for_each(|s| *s -= 1);
                    }
                }
                kept.push((h, set_no, was));
            }
        }
        c.results.clear();
        c.sets.clear();
        // Where each listed copy was, in the new list's order.
        let mut was_at: Vec<usize> = Vec::new();
        let mut i = 0;
        while i < kept.len() {
            let set_no = kept[i].1;
            let end = kept[i..].iter().position(|k| k.1 != set_no).map_or(kept.len(), |p| i + p);
            if end - i > 1 {
                let start = c.results.len();
                c.results.extend(kept[i..end].iter().map(|k| k.0.clone()));
                was_at.extend(kept[i..end].iter().map(|k| k.2));
                c.sets.push(start..c.results.len());
            }
            i = end;
        }
        // The copy the highlight was on, else the first after it, else
        // the last.
        self.cursor = was_at.iter().position(|&w| w >= cursor).unwrap_or(was_at.len().saturating_sub(1));
        if deleted_lit && !was_at.is_empty() {
            self.go_to_cursor = true;
        }
    }

    /// What a click on the bar asked for, once.
    pub fn take_request(&mut self) -> Option<Request> {
        self.request.take()
    }

    /// The pictures listed, when listing pictures rather than words.
    fn rows(&self) -> Option<&[PictureHit]> {
        self.picture.as_ref().map(|p| p.results.as_slice()).or_else(|| self.copies.as_ref().map(|c| c.results.as_slice()))
    }

    /// Open it looking for pictures like `q`.
    pub fn find_picture(&mut self, q: PictureQuery) {
        self.retire_picture();
        self.picture = Some(q);
        self.typed.clear();
        self.retire_thumbs();
        self.open();
    }

    /// Free the textures put aside in an earlier frame. Call once a
    /// frame, before the bar is drawn.
    pub fn free_retired(&mut self) {
        self.retired.clear();
    }

    /// Put the thumbnails aside rather than free them: one may have
    /// been drawn this frame, and a texture freed in the frame that
    /// draws it is gone before the frame is shown (wgpu then panics).
    fn retire_thumbs(&mut self) {
        self.retired.extend(self.thumbs.drain().filter_map(|(_, t)| t.texture));
    }

    /// Stop listing pictures (like one, or copies); the thumbnails are
    /// put aside as above.
    fn retire_picture(&mut self) {
        if let Some(p) = self.picture.take() {
            self.retired.extend(p.texture);
        }
        self.copies = None;
        self.retire_thumbs();
    }

    /// The words to find pictures of: what is typed, while open and
    /// finding words (not a picture, not copies, not only `#tags`).
    pub fn words_query(&self) -> Option<&str> {
        let q = self.query.trim();
        let tags_only = q.split_whitespace().all(|w| w.starts_with('#'));
        (self.open && self.rows().is_none() && !q.is_empty() && !tags_only).then_some(q)
    }

    /// The tag to suggest pictures for: a single `#tag` typed, while open
    /// and finding words.
    pub fn tag_query(&self) -> Option<String> {
        let q = self.query.trim();
        let one = q.split_whitespace().count() == 1;
        let tag = q.strip_prefix('#').filter(|t| one && !t.is_empty() && !t.contains('#'))?;
        (self.open && self.rows().is_none()).then(|| tag.to_string())
    }

    /// What the suggestions for `tag` were found from, if they are for it.
    pub fn tag_suggestions_for(&self, tag: &str) -> Option<(u64, usize)> {
        (self.words.tag.as_deref() == Some(tag)).then_some(self.words.results_for).flatten()
    }

    /// The pictures that probably carry `tag`, found from `key`; dropped
    /// if something else is typed by now.
    pub fn set_tag_suggestions(&mut self, tag: &str, key: (u64, usize), results: Vec<PictureHit>) {
        if self.tag_query().as_deref() != Some(tag) {
            return;
        }
        self.words.query = self.query.trim().to_string();
        self.words.tag = Some(tag.to_string());
        self.words.results = results;
        self.words.results_for = Some(key);
    }

    /// What the pictures for the typed words were found from, if they
    /// are for `query`.
    pub fn word_pictures_for(&self, query: &str) -> Option<(u64, usize)> {
        (self.words.query == query && self.words.tag.is_none()).then_some(self.words.results_for).flatten()
    }

    /// The pictures that show `query`, found from `key`; dropped if other
    /// words are typed by now. The highlighted row stays highlighted.
    pub fn set_word_pictures(&mut self, query: &str, key: (u64, usize), results: Vec<PictureHit>) {
        if self.query.trim() != query {
            return;
        }
        self.words.query = query.to_string();
        self.words.tag = None;
        self.words.results = results;
        self.words.results_for = Some(key);
    }

    /// Say `note` after the count (`None` once the pictures are in).
    pub fn set_words_note(&mut self, note: Option<String>) {
        self.words.note = note;
    }

    /// The typed words' pictures, if they are for what is typed.
    fn word_rows(&self) -> &[PictureHit] {
        if self.rows().is_none() && self.words.query == self.query.trim() {
            &self.words.results
        } else {
            &[]
        }
    }

    /// It is open, looking for pictures like one.
    pub fn has_picture(&self) -> bool {
        self.open && self.picture.is_some()
    }

    /// The picture looked for, while open.
    pub fn picture_mut(&mut self) -> Option<&mut PictureQuery> {
        self.picture.as_mut().filter(|_| self.open)
    }

    /// The picture looked for has results, maybe not the latest.
    pub fn has_picture_results(&self) -> bool {
        self.picture.as_ref().is_some_and(|p| p.results_for.is_some())
    }

    /// Bring the picture's results up to date, finding them with
    /// `search` when what they were found from (`key`: the index's
    /// generation, the slides read) has changed. The highlighted
    /// result stays highlighted.
    pub fn update_pictures(&mut self, key: (u64, usize), search: impl FnOnce(&PictureQuery) -> Vec<PictureHit>) {
        let Some(p) = self.picture.as_mut() else { return };
        if p.results_for == Some(key) {
            return;
        }
        let current = p.results.get(self.cursor).map(|h| (h.hit.slide, h.hit.layer, h.hit.found.clone()));
        p.results = search(p);
        p.results_for = Some(key);
        if let Some(cur) = current {
            if let Some(i) = p.results.iter().position(|h| (h.hit.slide, h.hit.layer, &h.hit.found) == (cur.0, cur.1, &cur.2)) {
                self.cursor = i;
            }
        }
    }

    /// The row an arrow key goes to, if one was pressed: `↓` the row
    /// below, `↑` (or `Shift+Enter`) the row above — or the highlighted
    /// row itself while it has not been gone to, so the first press shows
    /// the first match. Wraps around at the ends.
    fn step(&mut self, n: usize, up: bool, down: bool) -> Option<usize> {
        if n == 0 || !(up || down) {
            return None;
        }
        self.cursor = self.cursor.min(n - 1);
        if self.shown {
            self.cursor = if down { (self.cursor + 1) % n } else { (self.cursor + n - 1) % n };
        }
        self.shown = true;
        Some(self.cursor)
    }

    /// Esc closed it: the app goes back to where find started. Once.
    pub fn take_cancelled(&mut self) -> bool {
        std::mem::take(&mut self.cancelled)
    }

    /// Draw the bar while open; the match to go to, if one was picked.
    /// `search` finds the matches for a query; `unread` slides are not
    /// searched yet; `active` is the slide shown; `name` gives a slide's
    /// name ("" for none); `reading` is how many of the slides' pictures
    /// are read, of how many, while looking for a picture; `tags` gives
    /// every tag, for the list shown while nothing is typed.
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        search: impl Fn(&str) -> Vec<Hit>,
        unread: usize,
        active: usize,
        name: impl Fn(usize) -> String,
        reading: Option<(usize, usize)>,
        tags: impl FnOnce() -> Vec<TagCount>,
    ) -> Option<Hit> {
        if !self.open {
            return None;
        }
        let mut hits = if self.rows().is_some() { Vec::new() } else { search(&self.query) };
        // While a tag is renamed, Enter and Esc are the rename field's.
        let keys = |i: &mut egui::InputState| {
            // Shift+Enter first: a plain Enter pattern would take it too.
            let prev = i.consume_key(egui::Modifiers::SHIFT, egui::Key::Enter);
            (
                prev,
                i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
            )
        };
        let (prev, next, up, down, esc) = if self.tag_renaming.is_some() { Default::default() } else { ctx.input_mut(keys) };
        if esc {
            self.cancelled = true;
            self.close();
            return None;
        }
        // Words from the shown slide on; pictures from the most alike.
        let restart = |bar: &mut Self, hits: &[Hit]| {
            bar.cursor = if bar.rows().is_some() { 0 } else { hits.iter().position(|h| h.slide >= active).unwrap_or(0) };
            bar.shown = false;
            bar.restart = false;
        };
        if self.restart {
            restart(self, &hits);
        }
        let n = self.rows().map_or(hits.len() + self.word_rows().len(), <[PictureHit]>::len);
        let mut picked = self.step(n, up || prev, down);
        if std::mem::take(&mut self.go_to_cursor) && picked.is_none() && n > 0 {
            self.cursor = self.cursor.min(n - 1);
            self.shown = true;
            picked = Some(self.cursor);
        }
        // Enter: go to the highlighted match and stay there.
        let confirm = next && n > 0 && picked.is_none();
        if confirm {
            self.cursor = self.cursor.min(n - 1);
            self.shown = true;
            picked = Some(self.cursor);
        }
        let mut to_copies = false;

        let top = ctx.memory(|m| m.area_rect(egui::Id::new("osn_toolbar"))).map_or(70.0, |r| r.bottom() + 10.0);
        let x = ctx.screen_rect().center().x - WIDTH * 0.5;
        let area = egui::Area::new(egui::Id::new("osn_find_bar"))
            .fixed_pos(egui::pos2(x, top))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                theme::card_frame().inner_margin(egui::Margin::same(8.0)).show(ui, |ui| {
                    ui.set_width(WIDTH - 16.0);
                    let back = match (self.picture.as_mut(), &self.copies) {
                        (Some(p), _) => picture_header(ui, p),
                        (None, Some(_)) => copies_header(ui),
                        _ => false,
                    };
                    if back {
                        // Back to words.
                        self.retire_picture();
                        hits = search(&self.query);
                        restart(self, &hits);
                        picked = None;
                        self.focus_pending = true;
                    }
                    let in_picture = self.rows().is_some();
                    let (field, hint) = if in_picture {
                        (&mut self.typed, "Or type to find labels and tags instead")
                    } else {
                        (&mut self.query, "Find labels and tags — or paste or drop a picture")
                    };
                    let out = egui::TextEdit::singleline(field).hint_text(hint).desired_width(f32::INFINITY).show(ui);
                    if std::mem::take(&mut self.focus_pending) {
                        out.response.request_focus();
                        let mut state = out.state.clone();
                        let len = if in_picture { self.typed.chars().count() } else { self.query.chars().count() };
                        let all = CCursorRange::two(CCursor::new(0), CCursor::new(len));
                        state.cursor.set_char_range(Some(all));
                        state.store(ui.ctx(), out.response.id);
                    } else if !out.response.has_focus() && self.tag_renaming.is_none() {
                        out.response.request_focus();
                    }
                    if out.response.changed() && in_picture && !self.typed.trim().is_empty() {
                        // Typing goes back to finding words.
                        self.query = std::mem::take(&mut self.typed);
                        self.retire_picture();
                    }
                    if out.response.changed() && self.rows().is_none() {
                        hits = search(&self.query);
                        restart(self, &hits);
                        picked = None;
                        self.words.results.clear();
                        self.words.results_for = None;
                    }
                    ui.add_space(4.0);
                    let status = if let Some(c) = &self.copies {
                        let m = c.results.len();
                        if c.results_for.is_none() {
                            match reading.filter(|(r, t)| r < t) {
                                Some((r, t)) => format!("Reading pictures, {r} of {t}"),
                                None if unread > 0 => "Waiting for every slide to open".to_string(),
                                None => "Comparing…".to_string(),
                            }
                        } else if m == 0 {
                            "No duplicate pictures on the slides".to_string()
                        } else if self.shown {
                            format!("{} of {m}", self.cursor + 1)
                        } else {
                            let sets = c.sets.len();
                            format!("{sets} {} of copies, {m} pictures — arrow keys to look, Enter to stay", if sets == 1 { "set" } else { "sets" })
                        }
                    } else if let Some(p) = &self.picture {
                        let n = p.results.len();
                        let reading = reading.filter(|(r, t)| r < t);
                        let s = if n == 0 {
                            if reading.is_some() { "Looking…".to_string() } else { "No picture like this on the slides".to_string() }
                        } else if self.shown {
                            format!("{} of {n}", self.cursor + 1)
                        } else if n == 1 {
                            "1 picture like this — Enter goes there".to_string()
                        } else {
                            format!("{n} pictures like this — arrow keys to look, Enter to stay")
                        };
                        match reading {
                            Some((r, t)) => format!("{s} · reading pictures, {r} of {t}"),
                            None => s,
                        }
                    } else if self.query.trim().is_empty() {
                        "Type a label's words or a tag — or paste or drop a picture to find ones like it".to_string()
                    } else {
                        let total = hits.len() + self.word_rows().len();
                        let s = if total == 0 {
                            "No label says that, and nothing is tagged so".to_string()
                        } else if self.shown {
                            format!("{} of {total}", self.cursor + 1)
                        } else if total == 1 {
                            "1 found — Enter goes there".to_string()
                        } else {
                            format!("{total} found — arrow keys to look, Enter to stay")
                        };
                        match &self.words.note {
                            Some(note) => format!("{s} · {note}"),
                            None => s,
                        }
                    };
                    let status = match unread {
                        0 => status,
                        1 => format!("{status} · 1 slide still opening"),
                        n => format!("{status} · {n} slides still opening"),
                    };
                    if self.rows().is_none() && self.query.trim().is_empty() {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(status).small().color(theme::TEXT_MUTED));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                to_copies = ui.link(egui::RichText::new("Find every duplicate picture").small()).clicked();
                            });
                        });
                    } else {
                        ui.label(egui::RichText::new(status).small().color(theme::TEXT_MUTED));
                    }
                    if self.rows().is_none() && self.query.trim().is_empty() {
                        match tag_list(ui, &mut self.tag_renaming, &mut self.tag_focus, &mut self.tag_removing, &tags()) {
                            Some(TagPick::Find(tag)) => {
                                self.query = format!("#{tag}");
                                hits = search(&self.query);
                                restart(self, &hits);
                                picked = None;
                                self.focus_pending = true;
                            }
                            Some(TagPick::Change(a)) => self.request = Some(Request::Tag(a)),
                            None => {}
                        }
                        return;
                    }
                    if let Some(c) = &self.copies {
                        let first = self.cursor.saturating_sub(PICTURE_ROWS - 1);
                        let mut budget = THUMBS_PER_FRAME;
                        for (k, h) in c.results.iter().enumerate().skip(first).take(PICTURE_ROWS) {
                            let set = c.sets.iter().find(|r| r.contains(&k)).cloned().unwrap_or(k..k + 1);
                            if k == set.start || k == first {
                                if let Some(r) = set_header(ui, &c.results[set.clone()], active) {
                                    self.request = Some(r);
                                }
                            }
                            let texture = thumb(ui.ctx(), &mut self.thumbs, &h.png, &mut budget);
                            let label = format!("Copy {} of {}", k - set.start + 1, set.len());
                            let mut go = false;
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 4.0;
                                let w = (ui.available_width() - DELETE_BUTTON).max(120.0);
                                ui.allocate_ui(Vec2::new(w, PICTURE_ROW), |ui| go = picture_row(ui, h, k == self.cursor, texture.as_ref(), &name, Some(&label)));
                                let tip = format!("Delete this copy from slide {} — the other copies stay; Ctrl+Z there brings it back", h.hit.slide + 1);
                                if ui.button("Delete").on_hover_text(tip).clicked() {
                                    if let Found::Tagged { shapes, .. } = &h.hit.found {
                                        if let [shape] = shapes[..] {
                                            self.request = Some(Request::DeleteCopy { slide: h.hit.slide, layer: h.hit.layer, shape, png: Arc::clone(&h.png) });
                                        }
                                    }
                                }
                            });
                            if go {
                                self.cursor = k;
                                self.shown = true;
                                picked = Some(k);
                            }
                        }
                        if c.results.len() > first + PICTURE_ROWS {
                            let more = c.results.len() - first - PICTURE_ROWS;
                            ui.label(egui::RichText::new(format!("{more} more below")).small().color(theme::TEXT_MUTED));
                        }
                        return;
                    }
                    if let Some(p) = &self.picture {
                        let first = self.cursor.saturating_sub(PICTURE_ROWS - 1);
                        let mut budget = THUMBS_PER_FRAME;
                        for (k, h) in p.results.iter().enumerate().skip(first).take(PICTURE_ROWS) {
                            let texture = thumb(ui.ctx(), &mut self.thumbs, &h.png, &mut budget);
                            if picture_row(ui, h, k == self.cursor, texture.as_ref(), &name, None) {
                                self.cursor = k;
                                self.shown = true;
                                picked = Some(k);
                            }
                        }
                        if p.results.len() > first + PICTURE_ROWS {
                            let more = p.results.len() - first - PICTURE_ROWS;
                            ui.label(egui::RichText::new(format!("{more} more below")).small().color(theme::TEXT_MUTED));
                        }
                        return;
                    }
                    // With pictures too, each list gets a few rows.
                    let pictures = self.word_rows().len();
                    let rows = if pictures > 0 { WORD_ROWS_WITH_PICTURES } else { ROWS };
                    let first = if self.cursor < hits.len() { self.cursor.saturating_sub(rows - 1) } else { hits.len().saturating_sub(rows) };
                    for (k, h) in hits.iter().enumerate().skip(first).take(rows) {
                        let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 24.0), egui::Sense::click());
                        let lit = k == self.cursor;
                        if lit || resp.hovered() {
                            let fill = if lit { theme::ACCENT } else { Color32::from_white_alpha(16) };
                            ui.painter().rect_filled(rect, egui::Rounding::same(5.0), fill);
                        }
                        let y = rect.center().y;
                        let slide = place(h, &name);
                        let g = ui.painter().text(egui::pos2(rect.right() - 8.0, y), egui::Align2::RIGHT_CENTER, slide, FontId::proportional(11.0), theme::TEXT_MUTED);
                        let room = (g.left() - rect.left() - 24.0).max(40.0);
                        let galley = ui.painter().layout_job(row_text(h, room));
                        let at = egui::pos2(rect.left() + 8.0, y - galley.size().y * 0.5);
                        ui.painter().galley(at, galley, theme::TEXT_PRIMARY);
                        if resp.clicked() {
                            self.cursor = k;
                            self.shown = true;
                            picked = Some(k);
                        }
                    }
                    if hits.len() > first + rows {
                        let more = hits.len() - first - rows;
                        ui.label(egui::RichText::new(format!("{more} more below")).small().color(theme::TEXT_MUTED));
                    }
                    if pictures > 0 {
                        ui.add_space(4.0);
                        let title = match &self.words.tag {
                            Some(t) => format!("Could be #{t} — not tagged so yet"),
                            None => "Pictures that show it".to_string(),
                        };
                        ui.label(egui::RichText::new(title).small().color(theme::TEXT_MUTED));
                        let at = self.cursor.checked_sub(hits.len());
                        let first = at.map_or(0, |a| a.saturating_sub(WORD_PICTURE_ROWS - 1));
                        let mut budget = THUMBS_PER_FRAME;
                        let rows_here: Vec<(usize, PictureHit)> = self.word_rows().iter().cloned().enumerate().skip(first).take(WORD_PICTURE_ROWS).collect();
                        for (k, h) in rows_here {
                            let texture = thumb(ui.ctx(), &mut self.thumbs, &h.png, &mut budget);
                            let label = match (&self.words.tag, h.likely) {
                                (Some(_), true) => format!("Likely · {:.0}%", h.likeness.score * 100.0),
                                (Some(_), false) => format!("Maybe · {:.0}%", h.likeness.score * 100.0),
                                (None, _) => format!("{:.0}% match", h.likeness.score * 100.0),
                            };
                            let row = hits.len() + k;
                            let lit = row == self.cursor;
                            let go = match (self.words.tag.clone(), &h.hit.found) {
                                (Some(tag), Found::Tagged { shapes, .. }) if shapes.len() == 1 => {
                                    let (slide, layer, shape) = (h.hit.slide, h.hit.layer, shapes[0]);
                                    let mut go = false;
                                    ui.horizontal(|ui| {
                                        ui.spacing_mut().item_spacing.x = 4.0;
                                        let w = (ui.available_width() - SUGGESTION_BUTTONS).max(120.0);
                                        ui.allocate_ui(Vec2::new(w, PICTURE_ROW), |ui| go = picture_row(ui, &h, lit, texture.as_ref(), &name, Some(&label)));
                                        if ui.button("Tag").on_hover_text(format!("Tag it #{tag}")).clicked() {
                                            self.request = Some(Request::TagPicture { slide, layer, shape, tag: tag.clone() });
                                        }
                                        if ui.button("Not this").on_hover_text(format!("It is not #{tag}: don't suggest it for it again")).clicked() {
                                            self.request = Some(Request::NotThis { slide, layer, shape, tag: tag.clone() });
                                        }
                                    });
                                    go
                                }
                                _ => picture_row(ui, &h, lit, texture.as_ref(), &name, Some(&label)),
                            };
                            if go {
                                self.cursor = row;
                                self.shown = true;
                                picked = Some(row);
                            }
                        }
                        if pictures > first + WORD_PICTURE_ROWS {
                            let more = pictures - first - WORD_PICTURE_ROWS;
                            ui.label(egui::RichText::new(format!("{more} more below")).small().color(theme::TEXT_MUTED));
                        }
                    }
                });
            });
        // A click outside closes it.
        if ctx.input(|i| i.pointer.any_pressed()) && !ctx.input(|i| i.pointer.interact_pos()).is_some_and(|p| area.response.rect.contains(p)) {
            self.close();
        }
        if picked.is_some() {
            ctx.request_repaint();
        }
        if to_copies {
            self.find_copies();
            return None;
        }
        let hit = match self.rows() {
            Some(rows) => picked.and_then(|k| rows.get(k)).map(|h| h.hit.clone()),
            None => picked.and_then(|k| hits.get(k).cloned().or_else(|| self.word_rows().get(k - hits.len()).map(|h| h.hit.clone()))),
        };
        if confirm && hit.is_some() {
            self.close();
        }
        hit
    }
}

/// What a click in the tag list asks for.
enum TagPick {
    /// Find what has this tag.
    Find(String),
    Change(TagAction),
}

/// Most of the bar's height the tag list takes before it scrolls.
const TAG_LIST_HEIGHT: f32 = 300.0;

/// Every tag in `index`, a click finding what has it, and beside each its
/// counts, a rename (into `renaming`, `focus` for its first frame) and a
/// remove (which asks first: `removing`).
fn tag_list(ui: &mut egui::Ui, renaming: &mut Option<(String, String)>, focus: &mut bool, removing: &mut Option<String>, index: &[TagCount]) -> Option<TagPick> {
    ui.add_space(6.0);
    if index.is_empty() {
        ui.label(egui::RichText::new("No tags yet: select something, then the arrange bar's tag button").small().color(theme::TEXT_MUTED));
        return None;
    }
    let title = format!("Every tag · {} — a click finds what has it; the pen renames it everywhere, the bin removes it", index.len());
    ui.label(egui::RichText::new(title).small().color(theme::TEXT_MUTED));
    let mut out = None;
    // Sized by its rows: on the frame it first shows, a scroll area
    // only knows how much fitted last frame, and cut the last row off.
    let rows_h = index.len() as f32 * (22.0 + ui.spacing().item_spacing.y);
    let height = rows_h.min(TAG_LIST_HEIGHT);
    egui::ScrollArea::vertical().max_height(height).min_scrolled_height(height).auto_shrink([false, true]).show(ui, |ui| {
        for t in index {
            ui.horizontal(|ui| {
                if renaming.as_ref().is_some_and(|(from, _)| same_tag(from, &t.tag)) {
                    let Some((from, text)) = renaming.as_mut() else { return };
                    let r = ui.add(egui::TextEdit::singleline(text).desired_width(300.0).hint_text("new name (spaces: several)"));
                    if std::mem::take(focus) {
                        r.request_focus();
                        if let Some(mut st) = egui::text_edit::TextEditState::load(ui.ctx(), r.id) {
                            let end = CCursor::new(text.chars().count());
                            st.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(0), end)));
                            st.store(ui.ctx(), r.id);
                        }
                    } else if r.lost_focus() {
                        // Enter renames; Esc or a click elsewhere leaves it.
                        let to = parse_tags(text);
                        if ui.input(|i| i.key_pressed(egui::Key::Enter)) && !to.is_empty() && to != [from.clone()] {
                            out = Some(TagPick::Change(TagAction::Rename(from.clone(), to)));
                        }
                        *renaming = None;
                    }
                    return;
                }
                if removing.as_deref().is_some_and(|r| same_tag(r, &t.tag)) {
                    let things = if t.objects == 1 { "object" } else { "objects" };
                    ui.label(egui::RichText::new(format!("Take #{} off {} {things}?", t.tag, t.objects)).small());
                    if ui.small_button("Remove").clicked() {
                        out = Some(TagPick::Change(TagAction::Remove(t.tag.clone())));
                        *removing = None;
                    }
                    if ui.small_button("Keep").clicked() {
                        *removing = None;
                    }
                    return;
                }
                let label = egui::RichText::new(format!("#{}", t.tag)).color(TAG_COLOUR);
                if ui.add(egui::Button::new(label).frame(false)).on_hover_text("Find everything with this tag").clicked() {
                    out = Some(TagPick::Find(t.tag.clone()));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    let trash = egui::Image::new(egui::include_image!("../../assets/icons/trash.svg"));
                    if icon_btn(ui, trash, "Remove this tag from every object (on unlocked slides)") {
                        *removing = Some(t.tag.clone());
                        *renaming = None;
                    }
                    let pen = egui::Image::new(egui::include_image!("../../assets/icons/pen.svg"));
                    if icon_btn(ui, pen, "Rename this tag everywhere; to a tag that exists, they merge") {
                        *renaming = Some((t.tag.clone(), t.tag.clone()));
                        *focus = true;
                        *removing = None;
                    }
                    let slides = if t.slides == 1 { "slide" } else { "slides" };
                    ui.label(egui::RichText::new(format!("{} {slides} · {}", t.slides, t.objects)).small().color(theme::TEXT_MUTED))
                        .on_hover_text(format!("On {} {slides}, {} objects", t.slides, t.objects));
                });
            });
        }
    });
    out
}

/// A small icon button; whether it was clicked.
fn icon_btn(ui: &mut egui::Ui, img: egui::Image<'_>, tooltip: &str) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(22.0), egui::Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(rect, egui::Rounding::same(5.0), Color32::from_white_alpha(18));
    }
    let r = rect.shrink(4.0);
    img.tint(theme::TEXT_PRIMARY).fit_to_exact_size(r.size()).paint_at(ui, r);
    response.on_hover_text(tooltip).clicked()
}

/// Above the field while listing copies: what the list is, and a button
/// back to finding words. Returns whether that was clicked.
fn copies_header(ui: &mut egui::Ui) -> bool {
    let mut back = false;
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.label(egui::RichText::new("Duplicate pictures on every slide").color(theme::TEXT_PRIMARY));
            ui.label(egui::RichText::new("Resized, recompressed, mirrored or turned copies count too").small().color(theme::TEXT_MUTED));
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            back = ui.button(egui::RichText::new("×").size(16.0)).on_hover_text("Back to finding words").clicked();
        });
    });
    ui.add_space(4.0);
    back
}

/// The line over a set of copies: how many and on which slides, and,
/// where two or more sit on one layer of one slide (the shown slide's
/// first), a button that selects all of those but one.
fn set_header(ui: &mut egui::Ui, set: &[PictureHit], active: usize) -> Option<Request> {
    let mut slides: Vec<usize> = set.iter().map(|h| h.hit.slide).collect();
    slides.dedup();
    let on = slides.iter().map(|s| (s + 1).to_string()).collect::<Vec<_>>().join(", ");
    let text = format!("{} copies · {} {on}", set.len(), if slides.len() == 1 { "slide" } else { "slides" });
    let mut out = None;
    ui.horizontal(|ui| {
        ui.add_space(4.0);
        ui.label(egui::RichText::new(text).small().color(FOUND));
        if let Some(Request::SelectExtras { slide, shapes, .. }) = extras(set, active) {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let n = shapes.len();
                let tip = format!("Go to slide {} and select {n} of the {} copies there, keeping the one at the back — Delete then removes them", slide + 1, n + 1);
                if ui.small_button(format!("Select {n} extra")).on_hover_text(tip).clicked() {
                    out = extras(set, active);
                }
            });
        }
    });
    out
}

/// A set's copies to select for deleting: where two or more sit on one
/// layer of one slide (the shown slide's first), all of those but the
/// one at the back.
fn extras(set: &[PictureHit], active: usize) -> Option<Request> {
    let mut places: Vec<((usize, usize), Vec<usize>)> = Vec::new();
    for h in set {
        let Found::Tagged { shapes, .. } = &h.hit.found else { continue };
        let at = (h.hit.slide, h.hit.layer);
        match places.iter_mut().find(|(p, _)| *p == at) {
            Some((_, v)) => v.extend(shapes),
            None => places.push((at, shapes.clone())),
        }
    }
    places.retain(|(_, v)| v.len() >= 2);
    let i = places.iter().position(|((s, _), _)| *s == active).unwrap_or(0);
    let ((slide, layer), mut shapes) = places.into_iter().nth(i)?;
    shapes.sort_unstable();
    shapes.remove(0);
    Some(Request::SelectExtras { slide, layer, shapes })
}

/// Where a match is, for the right of its row: what it is (if not a
/// label), its slide, and whether its layer is hidden.
fn place(h: &Hit, name: &impl Fn(usize) -> String) -> String {
    let slide = match name(h.slide) {
        n if n.is_empty() => format!("Slide {}", h.slide + 1),
        n => format!("{} · {n}", h.slide + 1),
    };
    let slide = match &h.found {
        Found::Tagged { what, .. } => format!("{what} · {slide}"),
        Found::Label(_) => slide,
    };
    if h.hidden { format!("{slide} · hidden layer") } else { slide }
}

/// The picture looked for, above the field: its thumbnail, what it is,
/// and a button back to finding words. Returns whether that was clicked.
fn picture_header(ui: &mut egui::Ui, p: &mut PictureQuery) -> bool {
    let texture = p.texture.get_or_insert_with(|| ui.ctx().load_texture("osn_find_query", p.thumb.clone(), TextureOptions::LINEAR));
    let mut back = false;
    ui.horizontal(|ui| {
        let size = texture.size_vec2();
        ui.image((texture.id(), size * (40.0 / size.max_elem().max(1.0))));
        ui.vertical(|ui| {
            ui.label(egui::RichText::new(format!("Pictures like {}", p.label)).color(theme::TEXT_PRIMARY));
            ui.label(egui::RichText::new("Paste or drop another picture to look for it instead").small().color(theme::TEXT_MUTED));
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            back = ui.button(egui::RichText::new("×").size(16.0)).on_hover_text("Back to finding words").clicked();
        });
    });
    ui.add_space(4.0);
    back
}

/// One picture's row: thumbnail, how alike (or `copy`, which copy it
/// is), and where. Returns whether it was clicked.
fn picture_row(ui: &mut egui::Ui, h: &PictureHit, lit: bool, texture: Option<&TextureHandle>, name: &impl Fn(usize) -> String, copy: Option<&str>) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), PICTURE_ROW), egui::Sense::click());
    if lit || resp.hovered() {
        let fill = if lit { theme::ACCENT } else { Color32::from_white_alpha(16) };
        ui.painter().rect_filled(rect, egui::Rounding::same(5.0), fill);
    }
    let pad = (PICTURE_ROW - PICTURE_THUMB) * 0.5;
    let well = egui::Rect::from_min_size(rect.left_top() + egui::vec2(pad + 2.0, pad), Vec2::splat(PICTURE_THUMB));
    match texture {
        Some(t) => {
            let size = t.size_vec2();
            let shown = egui::Rect::from_center_size(well.center(), size * (PICTURE_THUMB / size.max_elem().max(1.0)));
            let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            ui.painter().image(t.id(), shown, uv, Color32::WHITE);
        }
        None => {
            ui.painter().rect_filled(well, egui::Rounding::same(4.0), Color32::from_white_alpha(12));
        }
    }
    let y = rect.center().y;
    let l = &h.likeness;
    let right = match copy {
        Some(_) => place(&h.hit, name),
        None => format!("{:.0}% · {}", l.score * 100.0, place(&h.hit, name)),
    };
    let g = ui.painter().text(egui::pos2(rect.right() - 8.0, y), egui::Align2::RIGHT_CENTER, right, FontId::proportional(11.0), theme::TEXT_MUTED);
    let mut job = LayoutJob::default();
    match copy {
        Some(label) => job.append(label, 0.0, TextFormat { font_id: FontId::proportional(13.5), color: theme::TEXT_PRIMARY, ..Default::default() }),
        None => {
            job.append(l.kind.label(), 0.0, TextFormat { font_id: FontId::proportional(13.5), color: kind_colour(l.kind), ..Default::default() });
            let notes: Vec<&str> = [l.way.note(), if l.other_crop { "another crop of it" } else { "" }].into_iter().filter(|n| !n.is_empty()).collect();
            if !notes.is_empty() {
                job.append(&format!(" · {}", notes.join(", ")), 0.0, TextFormat { font_id: FontId::proportional(12.0), color: theme::TEXT_MUTED, ..Default::default() });
            }
        }
    }
    job.wrap.max_width = (g.left() - well.right() - 20.0).max(40.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    let galley = ui.painter().layout_job(job);
    ui.painter().galley(egui::pos2(well.right() + 10.0, y - galley.size().y * 0.5), galley, theme::TEXT_PRIMARY);
    resp.clicked()
}

/// A row's text: the snippet, its matches in the found colour, cut to `width`.
fn row_text(h: &Hit, width: f32) -> LayoutJob {
    let plain = TextFormat { font_id: FontId::proportional(13.5), color: theme::TEXT_PRIMARY, ..Default::default() };
    let found = TextFormat { color: FOUND, ..plain.clone() };
    let mut job = LayoutJob::default();
    let s = &h.snippet;
    let mut from = 0;
    for m in &h.marks {
        job.append(&s[from..m.start], 0.0, plain.clone());
        job.append(&s[m.clone()], 0.0, found.clone());
        from = m.end;
    }
    job.append(&s[from..], 0.0, plain);
    job.wrap.max_width = width;
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real slides and vectors: takes the tag off every 5th picture tagged
    /// `OSN_TEST_TAG` (in memory only) and counts how many of those the
    /// suggestions bring back, and where.
    #[test]
    #[ignore = "needs a scratch OSN_DATA_DIR with slides and picture-vectors.bin, and OSN_MODEL_DIR"]
    fn tag_suggestions_on_real_slides() {
        use crate::persistence::{load_canvas, LoadOutcome, LoadProgress};
        assert!(std::env::var_os("OSN_DATA_DIR").is_some() && std::env::var_os("OSN_MODEL_DIR").is_some());
        let tag = std::env::var("OSN_TEST_TAG").unwrap_or_else(|_| "widisuuuu".into());
        let progress = LoadProgress::default();
        let mut slides: Vec<Canvas> = (0..400).filter_map(|i| if let LoadOutcome::Loaded(c) = load_canvas(i, 0, &progress) { Some(c) } else { None }).collect();
        let hide = std::env::var_os("OSN_TEST_NO_HIDE").is_none();
        let (mut hidden, mut seen) = (Vec::new(), 0);
        for (si, c) in slides.iter_mut().enumerate() {
            for (li, l) in c.layers.iter_mut().enumerate() {
                for (shi, s) in l.shapes.iter_mut().enumerate() {
                    if matches!(s, Shape::Raster { kind: RasterKind::Image, .. }) && s.tags().iter().any(|t| same_tag(t, &tag)) {
                        if hide && seen % 5 == 2 {
                            s.set_tags(&[]);
                            hidden.push((si, li, shi));
                        }
                        seen += 1;
                    }
                }
            }
        }
        assert!(!hide || !hidden.is_empty(), "no picture tagged #{tag}");
        println!("{} slides read, {} hidden", slides.len(), hidden.len());
        let mut vectors = crate::canvas::picture_vectors::PictureVectors::default();
        let start = std::time::Instant::now();
        loop {
            vectors.tick(slides.iter(), true, false);
            let (read, total) = vectors.progress();
            if total > 0 && read >= total {
                break;
            }
            assert!(start.elapsed().as_secs() < 900, "still reading: {}", vectors.status());
            if start.elapsed().as_millis() % 10_000 < 100 {
                println!("{} slides, {}", slides.len(), vectors.status());
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let all = || slides.iter().enumerate();
        let pool = TagPool::gather(&tag, all(), |_| false, |png| vectors.vector(png), |_| false);
        let found = pool.suggest(all());
        let t: Vec<&[f32]> = pool.tagged.iter().map(|v| &v[..]).collect();
        let (bar, maybe, median, top) = crate::tools::tag_suggest::explain(&t, pool.places.iter().enumerate().map(|(i, p)| (i, &p.4[..])), 8);
        let file_of = |si: usize, li: usize, shi: usize| match slides.get(si).and_then(|c| c.layers.get(li)?.shapes.get(shi)) {
            Some(Shape::Raster { picture: Some(p), .. }) => p.file.clone(),
            _ => "-".into(),
        };
        let top: Vec<String> = top.iter().map(|(i, c)| { let p = &pool.places[*i]; format!("{c:.3}@slide{}:{}", p.0 + 1, file_of(p.0, p.1, p.2)) }).collect();
        if std::env::var_os("OSN_TEST_NO_HIDE").is_some() {
            for (si, c) in slides.iter().enumerate() {
                for (li, l) in c.layers.iter().enumerate() {
                    for (shi, s) in l.shapes.iter().enumerate() {
                        if s.tags().iter().any(|t| same_tag(t, &tag)) {
                            println!("tagged {}", file_of(si, li, shi));
                        }
                    }
                }
            }
        }
        println!("bar {bar:.3}, maybe {maybe:.3}, candidates' median {median:.3}, {} candidates; closest: {}", pool.places.len(), top.join(" "));
        println!("suggested: {}", found.iter().map(|h| format!("{:.3}{}", h.likeness.score, if h.likely { "" } else { "?" })).collect::<Vec<_>>().join(" "));
        let ranks: Vec<usize> = found
            .iter()
            .enumerate()
            .filter(|(_, h)| matches!(&h.hit.found, Found::Tagged { shapes, .. } if hidden.contains(&(h.hit.slide, h.hit.layer, shapes[0]))))
            .map(|(r, _)| r + 1)
            .collect();
        println!("#{tag}: {} tagged, {} hidden; {} suggested, {} of them hidden ones, at ranks {ranks:?}", pool.counts().0, hidden.len(), found.len(), ranks.len());
        assert!(!hide || !ranks.is_empty(), "no hidden picture came back");
    }

    #[test]
    fn deleting_a_copy_takes_it_out_and_moves_the_rest_down() {
        let copy = Likeness { kind: Kind::Duplicate, score: 1.0, way: Default::default(), other_crop: false };
        let mut c = Canvas::default();
        c.layers[0].shapes.extend((0..6).map(|i| label([0.0, i as f32 * 10.0], "x")));
        let hit = |slide: usize, shape: usize| picture_hit(&c, slide, 0, shape, copy, Arc::new(Vec::new())).unwrap();
        let places = |bar: &FindBar| -> Vec<(usize, usize)> {
            bar.copies.as_ref().unwrap().results.iter().map(|h| match &h.hit.found {
                Found::Tagged { shapes, .. } => (h.hit.slide, shapes[0]),
                _ => unreachable!(),
            }).collect()
        };
        let mut bar = FindBar::default();
        bar.find_copies();
        bar.set_copies(vec![vec![hit(1, 1), hit(1, 4), hit(2, 0)], vec![hit(1, 2), hit(3, 5)]], (1, 1));
        // The highlight is on slide 2's copy (row 2).
        bar.cursor = 2;
        // Slide 1's shape 2 goes: its set is down to one and goes too;
        // slide 1's shape 4 is now shape 3. The highlight stays put.
        bar.forget_copy(1, 0, 2);
        assert_eq!(places(&bar), [(1, 1), (1, 3), (2, 0)]);
        assert_eq!(bar.copies.as_ref().unwrap().sets, [0..3]);
        assert_eq!(bar.cursor, 2);
        assert!(!bar.go_to_cursor, "another row went: nothing to go to");
        // The highlighted copy itself goes: the highlight moves to what
        // took its place (here the last row), and the bar goes there.
        bar.cursor = 1;
        bar.forget_copy(1, 0, 3);
        assert_eq!(places(&bar), [(1, 1), (2, 0)]);
        assert_eq!(bar.cursor, 1);
        assert!(bar.go_to_cursor);
        bar.go_to_cursor = false;
        bar.forget_copy(2, 0, 0);
        assert!(places(&bar).is_empty(), "the last copy is not a set of copies");
        assert!(!bar.go_to_cursor, "nothing left to go to");
    }

    fn label(at: [f32; 2], text: &str) -> Shape {
        Shape::Text {
            pos: at,
            content: text.into(),
            font_size: 20.0,
            color: [255, 255, 255, 255],
            rotation: 0.0,
            pin: 0,
            background: None,
            group: 0,
            tags: Vec::new(),
            turn: 0.0,
            link: 0,
        }
    }

    fn slide(labels: &[([f32; 2], &str)]) -> Canvas {
        let mut c = Canvas::default();
        c.layers[0].shapes.extend(labels.iter().map(|(at, t)| label(*at, t)));
        c
    }

    #[test]
    fn finds_labels_case_aside_across_lines_in_reading_order() {
        let a = slide(&[([0.0, 200.0], "second Apple"), ([300.0, 0.0], "an APPLE\npie"), ([0.0, 0.0], "pear")]);
        let b = slide(&[([0.0, 0.0], "Apple   pie")]);
        let hits = search("apple pie", [(0, &a), (1, &b)]);
        // "an APPLE\npie" reads as "an APPLE pie"; runs of spaces count as one.
        assert_eq!(hits.iter().map(|h| (h.slide, h.found.clone())).collect::<Vec<_>>(), vec![(0, Found::Label(1)), (1, Found::Label(0))]);
        assert_eq!(hits[0].snippet, "an APPLE pie");
        assert_eq!(hits[0].marks.iter().map(|m| &hits[0].snippet[m.clone()]).collect::<Vec<_>>(), ["APPLE pie"]);
        // Top to bottom on a slide.
        let apples = search("apple", [(0, &a)]);
        assert_eq!(apples.iter().map(|h| h.found.clone()).collect::<Vec<_>>(), vec![Found::Label(1), Found::Label(0)]);
        assert!(search("  ", [(0, &a)]).is_empty());
        assert!(search("plum", [(0, &a)]).is_empty());
    }

    fn picture(at: [f32; 2], tags: &[&str], group: u64) -> Shape {
        let mut s = crate::canvas::shape::tests_support::rect(at, [at[0] + 10.0, at[1] + 10.0]);
        s.set_group(group);
        s.set_tags(&tags.iter().map(|t| t.to_string()).collect::<Vec<_>>());
        s
    }

    #[test]
    fn tags_find_objects_and_a_group_as_one() {
        let mut c = slide(&[([0.0, 300.0], "walk notes")]);
        let l = &mut c.layers[0];
        l.shapes.push(picture([0.0, 0.0], &["Walk cycle", "ref"], 0)); // 1
        l.shapes.push(picture([50.0, 100.0], &[], 7)); // 2: in group 7, untagged
        l.shapes.push(picture([0.0, 100.0], &["walk cycle"], 7)); // 3: tagged alone in it
        l.shapes.push(picture([0.0, 200.0], &["pose"], 0)); // 4
        let hits = search("walk", [(0, &c)]);
        let found: Vec<Found> = hits.iter().map(|h| h.found.clone()).collect();
        // In group 7 only the tagged member is found, not the whole group.
        assert_eq!(found, vec![
            Found::Tagged { strokes: vec![], shapes: vec![1], what: "Rectangle".into() },
            Found::Tagged { strokes: vec![], shapes: vec![3], what: "Rectangle".into() },
            Found::Label(0),
        ]);
        // Tagged as one (every member has it), the group is found as one.
        c.layers[0].shapes[2].set_tags(&["walk cycle".to_string()]);
        let group = search("walk", [(0, &c)]).into_iter().map(|h| h.found).find(|f| matches!(f, Found::Tagged { shapes, .. } if shapes.len() == 2));
        assert_eq!(group, Some(Found::Tagged { strokes: vec![], shapes: vec![2, 3], what: "Group of 2".into() }));
        c.layers[0].shapes[2].set_tags(&[]);
        let hits = search("walk", [(0, &c)]);
        assert_eq!(hits[0].marks.iter().map(|m| &hits[0].snippet[m.clone()]).collect::<Vec<_>>(), ["Walk"]);
        assert_eq!(hits[0].snippet, "#Walk cycle  #ref");
        // A `#` typed finds tags only.
        assert_eq!(search("#pose", [(0, &c)]).len(), 1);
        // A label found by its text is not listed again for its tags.
        c.layers[0].shapes[0].set_tags(&["walk".to_string()]);
        assert_eq!(search("walk", [(0, &c)]).len(), 3);
    }

    #[test]
    fn several_words_find_what_has_every_one_among_its_tags() {
        let mut c = Canvas::default();
        let l = &mut c.layers[0];
        l.shapes.push(picture([0.0, 0.0], &["body", "sketch", "perspective"], 0)); // 0
        l.shapes.push(picture([0.0, 100.0], &["body", "pose"], 0)); // 1
        l.shapes.push(picture([0.0, 200.0], &["sketch"], 0)); // 2
        let shapes = |c: &Canvas, q: &str| -> Vec<Vec<usize>> {
            search(q, [(0, c)]).into_iter().map(|h| match h.found {
                Found::Tagged { shapes, .. } => shapes,
                f => panic!("{f:?}"),
            }).collect()
        };
        // Any order, with `#`, commas, extra spaces; each word somewhere.
        for q in ["body sketch", "sketch body", "#body #sketch", "body, sketch", "  Body   PERSP  "] {
            assert_eq!(shapes(&c, q), vec![vec![0]], "{q}");
        }
        assert_eq!(shapes(&c, "body sketch perspective"), vec![vec![0]]);
        assert_eq!(shapes(&c, "body"), vec![vec![0], vec![1]]);
        assert!(shapes(&c, "body walk").is_empty());
        assert!(shapes(&c, ",").is_empty());
        // Each word found is marked.
        let hit = &search("perspective body", [(0, &c)])[0];
        assert_eq!(hit.marks.iter().map(|m| &hit.snippet[m.clone()]).collect::<Vec<_>>(), ["body", "perspective"]);
        // A group tagged as one is found whole by several words too.
        let l = &mut c.layers[0];
        l.shapes.push(picture([300.0, 0.0], &["hand", "study"], 9)); // 3
        l.shapes.push(picture([300.0, 50.0], &["hand", "study", "ref"], 9)); // 4
        assert_eq!(shapes(&c, "study hand"), vec![vec![3, 4]]);
    }

    #[test]
    fn tags_are_saved_and_untagged_objects_save_as_before() {
        let mut c = Canvas::default();
        c.layers[0].shapes.push(picture([0.0, 0.0], &[], 0));
        let plain = crate::persistence::serialize_canvas(&c).unwrap();
        assert!(!plain.contains("tags"), "no tags, nothing new written");
        c.layers[0].shapes[0].set_tags(&["pose".to_string()]);
        let saved = crate::persistence::serialize_canvas(&c).unwrap();
        let back = crate::persistence::parse_canvas(&saved, 1).unwrap();
        assert_eq!(back.layers[0].shapes[0].tags(), ["pose".to_string()]);
        // A file from before tags reads as untagged.
        let old = crate::persistence::parse_canvas(&plain, 1).unwrap();
        assert!(old.layers[0].shapes[0].tags().is_empty());
    }

    #[test]
    fn tags_are_read_as_typed() {
        use crate::canvas::shape::parse_tags;
        // Spaces or commas between; one word each.
        assert_eq!(parse_tags(" #pose, walk   cycle,,Pose , ref walk-cycle"), vec!["pose", "walk", "cycle", "ref", "walk-cycle"]);
        assert!(parse_tags(" , ,").is_empty());
    }

    #[test]
    fn hidden_layers_are_found_and_said() {
        let mut c = slide(&[([0.0, 0.0], "note")]);
        c.layers[0].visible = false;
        assert!(search("note", [(0, &c)])[0].hidden);
    }

    #[test]
    fn long_labels_are_cut_around_the_match() {
        let text: Vec<char> = format!("{}needle{}", "a".repeat(80), "b".repeat(80)).chars().collect();
        let at = 80;
        let (s, marks) = snippet(&text, &[(at, 6)]);
        assert!(s.starts_with('…') && s.ends_with('…'));
        assert_eq!(&s[marks[0].clone()], "needle");
        assert!(s.chars().count() <= SNIPPET + 2);
        // A match at the very end keeps its tail.
        let text: Vec<char> = format!("{}end", "a".repeat(100)).chars().collect();
        let (s, marks) = snippet(&text, &[(100, 3)]);
        assert_eq!(&s[marks[0].clone()], "end");
        assert!(!s.ends_with('…'));
        // Several: in order, overlaps joined, one past the cut dropped.
        let text: Vec<char> = format!("ab cd{}xy", "-".repeat(100)).chars().collect();
        let (s, marks) = snippet(&text, &[(3, 2), (0, 2), (1, 1), (102, 2)]);
        assert_eq!(marks.iter().map(|m| &s[m.clone()]).collect::<Vec<_>>(), ["ab", "cd"]);
    }

    #[test]
    fn extras_keep_one_copy_per_layer_on_the_shown_slide_first() {
        let copy = Likeness { kind: Kind::Duplicate, score: 1.0, way: Default::default(), other_crop: false };
        let mut c = Canvas::default();
        c.layers[0].shapes.extend((0..6).map(|i| label([0.0, i as f32 * 10.0], "x")));
        let hit = |slide: usize, shape: usize| picture_hit(&c, slide, 0, shape, copy, Arc::new(Vec::new())).unwrap();
        // Slide 1: shapes 4 and 2; slide 3: shapes 5, 1 and 3; slide 2: one.
        let set = vec![hit(1, 4), hit(1, 2), hit(2, 0), hit(3, 5), hit(3, 1), hit(3, 3)];
        let got = |active| match extras(&set, active) {
            Some(Request::SelectExtras { slide, layer, shapes }) => Some((slide, layer, shapes)),
            _ => None,
        };
        assert_eq!(got(3), Some((3, 0, vec![3, 5])), "the shown slide's, the backmost kept");
        assert_eq!(got(0), Some((1, 0, vec![4])), "else the first slide with two");
        assert_eq!(extras(&set[2..4], 0).map(|_| ()), None, "one copy a slide: nothing to select");
    }

    #[test]
    fn the_first_arrow_shows_the_highlighted_match_then_arrows_move() {
        let mut bar = FindBar::default();
        // Down first shows the highlighted match itself, then moves on.
        assert_eq!(bar.step(3, false, true), Some(0));
        assert_eq!(bar.step(3, false, true), Some(1));
        assert_eq!(bar.step(3, false, true), Some(2));
        assert_eq!(bar.step(3, false, true), Some(0), "wraps round");
        assert_eq!(bar.step(3, true, false), Some(2));
        assert_eq!(bar.step(3, false, false), None);
        assert_eq!(bar.step(0, false, true), None);
        // Up first shows the highlighted one too.
        let mut bar = FindBar { cursor: 1, ..Default::default() };
        assert_eq!(bar.step(3, true, false), Some(1));
        assert_eq!(bar.step(3, true, false), Some(0));
    }
}
