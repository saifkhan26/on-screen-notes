//! Pictures that look like a given one, for find (`ui::find_bar`): the
//! same picture again (resized, saved as JPEG instead of PNG), the same
//! with its colours changed or mirrored or turned, or one of a like look
//! (about the same colours, laid out about the same way).
//!
//! Every picture is first shrunk the way slides shrink their previews
//! (`pureref::PREVIEW_SIDE`), so a full-size picture and a slide's
//! 96 px preview of it read alike, then to 32 × 32 over mid grey. Two
//! things are kept of it:
//!   * a perceptual hash: the 63 lowest frequencies of its grey levels
//!     (a DCT), one bit each, set when above their median. Resizing,
//!     recompressing and colour changes leave it nearly as it was; a
//!     different picture differs in about half the bits.
//!   * its colour layout: the mean Oklab colour of each cell of a 4 × 4
//!     grid, Oklab so that equal gaps look about equally different.
//!
//! The query is read all 8 ways a picture can be mirrored and turned by
//! quarter turns, so a flipped or turned copy is found too. Global
//! hashes like these do not see a crop of a bigger picture as the same
//! picture; a stored picture's crops share its file, which `search`
//! checks for.

use crate::canvas::canvas::Canvas;
use crate::canvas::shape::{PictureRef, RasterKind, Shape};
use image::RgbaImage;
use std::sync::Arc;

/// Side of the square every picture is read at.
const SIDE: usize = 32;
/// Lowest frequencies kept, along each axis.
const FREQ: usize = 8;
/// Cells along each side of the colour layout.
const GRID: usize = 4;
const CELL: usize = SIDE / GRID;
/// A DCT with no frequency stronger than this is a flat picture: its
/// bits would be noise, so its hash is all zeros.
const FLAT: f32 = 0.5;

/// Bits, colour gap and aspect a duplicate may differ by at most.
const DUPLICATE_BITS: u32 = 6;
const DUPLICATE_COLOUR: f32 = 0.05;
const DUPLICATE_ASPECT: f32 = 1.08;
/// Bits and aspect an edited copy (colours changed, small changes,
/// stretched) may differ by at most. Measured on 5,500 real reference
/// pictures (mostly figures on white): at 10 bits and up, most pairs
/// were different pictures laid out alike.
const EDITED_BITS: u32 = 8;
const EDITED_ASPECT: f32 = 1.4;
/// Least score of a picture that is only similar, and how many of those
/// are listed at most: the closest few are what is worth a look; past
/// them, on the same pictures, nearly every pair scored about the same.
const ALIKE_SCORE: f32 = 0.6;
const MOST_ALIKE: usize = 20;
/// Colour gap at which colours count as nothing alike.
const COLOUR_SPAN: f32 = 0.25;
/// Share of the score that is the hash's; the rest is the colours'.
const SHAPE_SHARE: f32 = 0.55;

/// How alike two pictures are, most alike first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// The same picture: maybe resized, recompressed, mirrored or turned.
    Duplicate,
    /// The same picture changed: other colours, a small edit, stretched,
    /// or another crop of the same stored file.
    Edited,
    /// Another picture of about the same colours and layout.
    Alike,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Duplicate => "Duplicate",
            Kind::Edited => "Near-duplicate",
            Kind::Alike => "Similar",
        }
    }
}

/// How one picture is turned from another: mirrored left to right
/// first (or not), then turned clockwise by `turns` quarter turns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Way {
    pub mirror: bool,
    pub turns: u8,
}

impl Way {
    const ALL: [Way; 8] = [
        Way { mirror: false, turns: 0 },
        Way { mirror: false, turns: 1 },
        Way { mirror: false, turns: 2 },
        Way { mirror: false, turns: 3 },
        Way { mirror: true, turns: 0 },
        Way { mirror: true, turns: 1 },
        Way { mirror: true, turns: 2 },
        Way { mirror: true, turns: 3 },
    ];

    /// In words, for a result row; empty when not turned at all.
    pub fn note(self) -> &'static str {
        match (self.mirror, self.turns % 4) {
            (false, 0) => "",
            (false, 1) => "turned right",
            (false, 2) => "upside down",
            (false, 3) => "turned left",
            (true, 0) => "mirrored",
            (true, 1) => "mirrored, turned right",
            (true, 2) => "flipped top to bottom",
            _ => "mirrored, turned left",
        }
    }

    /// The pixel of the unturned `n` × `n` square that lands on `(x, y)`.
    fn source(self, mut x: usize, mut y: usize, n: usize) -> (usize, usize) {
        // Undo each clockwise turn: what is at (x, y) after one came
        // from (y, n - 1 - x).
        for _ in 0..self.turns % 4 {
            (x, y) = (y, n - 1 - x);
        }
        if self.mirror {
            x = n - 1 - x;
        }
        (x, y)
    }
}

/// What is kept of a picture to compare it with others.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Signature {
    hash: u64,
    colours: [[f32; 3]; GRID * GRID],
    /// Width over height.
    aspect: f32,
}

#[cfg(test)]
impl Signature {
    /// `img` read as it is. `None` for an empty picture.
    pub fn of(img: &RgbaImage) -> Option<Signature> {
        Small::of(img).map(|s| s.signature())
    }

    /// The picture whose encoded bytes (PNG, JPEG, …) these are.
    pub fn of_bytes(bytes: &[u8]) -> Option<Signature> {
        Signature::of(&image::load_from_memory(bytes).ok()?.to_rgba8())
    }
}

/// What the index keeps of a picture: its hash read all 8 ways, and its
/// colour layout and aspect as it is (the other ways' are the same cells
/// moved about). Enough to compare it either way round with any other.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Print {
    hashes: [u64; 8],
    colours: [[f32; 3]; GRID * GRID],
    aspect: f32,
}

impl Print {
    /// `img` read all 8 ways. `None` for an empty picture.
    pub fn of(img: &RgbaImage) -> Option<Print> {
        let small = Small::of(img)?;
        let hashes = Way::ALL.map(|w| hash(&small.turned(w).grey));
        let s = small.signature();
        Some(Print { hashes, colours: s.colours, aspect: s.aspect })
    }

    /// The picture whose encoded bytes (PNG, JPEG, …) these are.
    pub fn of_bytes(bytes: &[u8]) -> Option<Print> {
        Print::of(&image::load_from_memory(bytes).ok()?.to_rgba8())
    }

    /// The picture as it is.
    pub fn signature(&self) -> Signature {
        self.way(0)
    }

    /// The picture turned `Way::ALL[i]`.
    fn way(&self, i: usize) -> Signature {
        let way = Way::ALL[i];
        let mut colours = [[0.0; 3]; GRID * GRID];
        for y in 0..GRID {
            for x in 0..GRID {
                let (sx, sy) = way.source(x, y, GRID);
                colours[y * GRID + x] = self.colours[sy * GRID + sx];
            }
        }
        let aspect = if way.turns % 2 == 1 { 1.0 / self.aspect } else { self.aspect };
        Signature { hash: self.hashes[i], colours, aspect }
    }

    /// Nothing but one colour: its hash says nothing, so only its
    /// colours could match, and plain fills are not worth calling copies.
    fn flat(&self) -> bool {
        self.hashes[0] == 0
    }

    /// `other` is this picture again, maybe resized, recompressed,
    /// mirrored or turned.
    pub fn is_copy_of(&self, other: &Print) -> bool {
        let theirs = other.signature();
        (0..Way::ALL.len()).any(|i| {
            if (self.hashes[i] ^ theirs.hash).count_ones() > DUPLICATE_BITS {
                return false;
            }
            let mine = self.way(i);
            colour_gap(&mine, &theirs) <= DUPLICATE_COLOUR && mine.aspect.max(theirs.aspect) / mine.aspect.min(theirs.aspect).max(1e-6) <= DUPLICATE_ASPECT
        })
    }
}

/// A picture on the slides, for `copies`.
#[derive(Debug, Clone, Default)]
pub struct Item {
    /// `None` while not read, or when it could not be.
    pub print: Option<Print>,
    /// Two pictures sharing any of these are copies whatever their
    /// prints say: the same bytes, the same stored file shown the same
    /// way, the same video or GIF file.
    pub keys: Vec<String>,
}

/// Bits of a hash in each of the chunks `copies` buckets pictures by.
/// Hashes that differ in at most `DUPLICATE_BITS` of their 63 bits agree
/// on at least one of `63 / CHUNK` chunks, so only pictures sharing a
/// chunk need comparing.
const CHUNK: u32 = 9;
const CHUNKS: u32 = 63 / CHUNK;
const _: () = assert!(CHUNKS > DUPLICATE_BITS, "a copy must share a chunk");

/// The sets of copies among `items`: each set's items in the order given,
/// the sets in the order of their first item. Items in no set are left out.
pub fn copies(items: &[Item]) -> Vec<Vec<usize>> {
    let mut parent: Vec<usize> = (0..items.len()).collect();
    fn root(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    let join = |parent: &mut Vec<usize>, a: usize, b: usize| {
        let (ra, rb) = (root(parent, a), root(parent, b));
        if ra != rb {
            parent[ra.max(rb)] = ra.min(rb);
        }
    };
    let mut first: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for (i, item) in items.iter().enumerate() {
        for k in &item.keys {
            match first.get(k.as_str()) {
                Some(&j) => join(&mut parent, i, j),
                None => {
                    first.insert(k, i);
                }
            }
        }
    }
    let chunk = |h: u64, k: u32| ((h >> (1 + k * CHUNK)) & ((1 << CHUNK) - 1)) as u16;
    let mut buckets: std::collections::HashMap<(u32, u16), Vec<usize>> = std::collections::HashMap::new();
    for (i, item) in items.iter().enumerate() {
        if let Some(p) = item.print.filter(|p| !p.flat()) {
            for k in 0..CHUNKS {
                buckets.entry((k, chunk(p.hashes[0], k))).or_default().push(i);
            }
        }
    }
    for (i, item) in items.iter().enumerate() {
        let Some(p) = item.print.filter(|p| !p.flat()) else { continue };
        for h in p.hashes {
            for k in 0..CHUNKS {
                for &j in buckets.get(&(k, chunk(h, k))).map_or(&[][..], Vec::as_slice) {
                    if j != i && root(&mut parent, i) != root(&mut parent, j) && items[j].print.is_some_and(|q| p.is_copy_of(&q)) {
                        join(&mut parent, i, j);
                    }
                }
            }
        }
    }
    let mut sets: std::collections::BTreeMap<usize, Vec<usize>> = std::collections::BTreeMap::new();
    for i in 0..items.len() {
        let r = root(&mut parent, i);
        sets.entry(r).or_default().push(i);
    }
    // A root is its set's smallest item, so the sets come in order.
    sets.into_values().filter(|s| s.len() > 1).collect()
}

/// A picture shrunk to `SIDE` × `SIDE`: each pixel's grey level and
/// Oklab colour.
struct Small {
    grey: Vec<f32>,
    lab: Vec<[f32; 3]>,
    aspect: f32,
}

impl Small {
    fn of(img: &RgbaImage) -> Option<Small> {
        let (w, h) = img.dimensions();
        if w == 0 || h == 0 {
            return None;
        }
        let preview;
        let side = crate::media::pureref::PREVIEW_SIDE;
        let img = if w.max(h) > side {
            let s = f64::from(side) / f64::from(w.max(h));
            let (tw, th) = (((f64::from(w) * s).round() as u32).max(1), ((f64::from(h) * s).round() as u32).max(1));
            preview = image::imageops::thumbnail(img, tw, th);
            &preview
        } else {
            img
        };
        let tiny = image::imageops::resize(img, SIDE as u32, SIDE as u32, image::imageops::FilterType::Triangle);
        let mut grey = Vec::with_capacity(SIDE * SIDE);
        let mut lab = Vec::with_capacity(SIDE * SIDE);
        for p in tiny.pixels() {
            // Over mid grey, so a cut-out reads the same wherever it came from.
            let a = f32::from(p[3]) / 255.0;
            let rgb = [0, 1, 2].map(|c| (f32::from(p[c]) * a + 128.0 * (1.0 - a)).round().clamp(0.0, 255.0) as u8);
            grey.push((0.299 * f32::from(rgb[0]) + 0.587 * f32::from(rgb[1]) + 0.114 * f32::from(rgb[2])) / 255.0);
            lab.push(crate::tools::picture_colours::oklab(rgb));
        }
        Some(Small { grey, lab, aspect: w as f32 / h as f32 })
    }

    /// This picture turned `way`.
    fn turned(&self, way: Way) -> Small {
        let mut grey = Vec::with_capacity(SIDE * SIDE);
        let mut lab = Vec::with_capacity(SIDE * SIDE);
        for y in 0..SIDE {
            for x in 0..SIDE {
                let (sx, sy) = way.source(x, y, SIDE);
                grey.push(self.grey[sy * SIDE + sx]);
                lab.push(self.lab[sy * SIDE + sx]);
            }
        }
        let aspect = if way.turns % 2 == 1 { 1.0 / self.aspect } else { self.aspect };
        Small { grey, lab, aspect }
    }

    fn signature(&self) -> Signature {
        let mut colours = [[0.0; 3]; GRID * GRID];
        for (i, c) in self.lab.iter().enumerate() {
            let cell = (i / SIDE / CELL) * GRID + (i % SIDE) / CELL;
            for k in 0..3 {
                colours[cell][k] += c[k];
            }
        }
        let n = (CELL * CELL) as f32;
        for c in &mut colours {
            *c = c.map(|v| v / n);
        }
        Signature { hash: hash(&self.grey), colours, aspect: self.aspect }
    }
}

/// `cos((2x + 1) u π / 2N)` for the kept frequencies `u`.
fn cosines() -> &'static [[f32; SIDE]; FREQ] {
    static TABLE: std::sync::OnceLock<[[f32; SIDE]; FREQ]> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        let mut t = [[0.0; SIDE]; FREQ];
        for (u, row) in t.iter_mut().enumerate() {
            for (x, v) in row.iter_mut().enumerate() {
                *v = ((2 * x + 1) as f32 * u as f32 * std::f32::consts::PI / (2 * SIDE) as f32).cos();
            }
        }
        t
    })
}

/// The perceptual hash of `SIDE` × `SIDE` grey levels: bit `v * 8 + u`
/// is set when frequency `(u, v)` is above the median of the 63 kept
/// (bit 0, the mean, never is).
fn hash(grey: &[f32]) -> u64 {
    let c = cosines();
    let mut rows = [[0.0f32; FREQ]; SIDE];
    for (y, row) in rows.iter_mut().enumerate() {
        let line = &grey[y * SIDE..(y + 1) * SIDE];
        for (u, v) in row.iter_mut().enumerate() {
            *v = line.iter().zip(&c[u]).map(|(g, k)| g * k).sum();
        }
    }
    let mut coef = [0.0f32; FREQ * FREQ];
    for v in 0..FREQ {
        for u in 0..FREQ {
            coef[v * FREQ + u] = (0..SIDE).map(|y| c[v][y] * rows[y][u]).sum();
        }
    }
    let ac = &coef[1..];
    if ac.iter().all(|v| v.abs() < FLAT) {
        return 0;
    }
    let mut sorted = ac.to_vec();
    sorted.sort_by(f32::total_cmp);
    let median = sorted[sorted.len() / 2];
    coef.iter().enumerate().skip(1).filter(|(_, v)| **v > median).fold(0, |h, (i, _)| h | 1 << i)
}

/// Mean gap between two colour layouts, cell by cell, in Oklab.
fn colour_gap(a: &Signature, b: &Signature) -> f32 {
    let sum: f32 = a
        .colours
        .iter()
        .zip(&b.colours)
        .map(|(p, q)| ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2) + (p[2] - q[2]).powi(2)).sqrt())
        .sum();
    sum / (GRID * GRID) as f32
}

/// How alike, from 0 (nothing alike) to 1 (the same).
fn score(bits: u32, colour: f32) -> f32 {
    // Unrelated pictures differ in about half the 63 bits.
    let shape = (1.0 - bits as f32 / 32.0).clamp(0.0, 1.0);
    let colour = (1.0 - colour / COLOUR_SPAN).clamp(0.0, 1.0);
    SHAPE_SHARE * shape + (1.0 - SHAPE_SHARE) * colour
}

/// What comparing a picture with the query found.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Likeness {
    pub kind: Kind,
    /// 0 to 1, for sorting and showing as a percentage.
    pub score: f32,
    /// How the picture is turned from the query.
    pub way: Way,
    /// It shows another part of the query's own stored file.
    pub other_crop: bool,
}

/// The picture to find others like.
#[derive(Debug, Clone)]
pub struct Query {
    ways: Vec<(Way, Signature)>,
}

impl Query {
    /// `img` read all 8 ways. `None` for an empty picture.
    pub fn of(img: &RgbaImage) -> Option<Query> {
        let small = Small::of(img)?;
        Some(Query { ways: Way::ALL.iter().map(|&w| (w, small.turned(w).signature())).collect() })
    }

    /// How alike `s` is, if it is alike enough to list.
    pub fn compare(&self, s: &Signature) -> Option<Likeness> {
        let mut best: Option<(f32, u32, f32, f32, Way)> = None;
        for (way, q) in &self.ways {
            let bits = (q.hash ^ s.hash).count_ones();
            let colour = colour_gap(q, s);
            let score = score(bits, colour);
            let aspect = q.aspect.max(s.aspect) / q.aspect.min(s.aspect).max(1e-6);
            // The first way wins a tie, so an unturned copy says so.
            if best.map_or(true, |b| score > b.0) {
                best = Some((score, bits, colour, aspect, *way));
            }
        }
        let (score, bits, colour, aspect, way) = best?;
        let kind = if bits <= DUPLICATE_BITS && colour <= DUPLICATE_COLOUR && aspect <= DUPLICATE_ASPECT {
            Kind::Duplicate
        } else if bits <= EDITED_BITS && aspect <= EDITED_ASPECT {
            Kind::Edited
        } else if score >= ALIKE_SCORE {
            Kind::Alike
        } else {
            return None;
        };
        Some(Likeness { kind, score, way, other_crop: false })
    }
}

/// A picture on a slide that looks like the query.
#[derive(Debug, Clone)]
pub struct Match {
    pub slide: usize,
    pub layer: usize,
    pub shape: usize,
    pub likeness: Likeness,
    /// Its bytes (a preview, poster or the picture), for a thumbnail.
    pub png: Arc<Vec<u8>>,
}

/// Where the query came from, when it is a picture on a slide.
#[derive(Debug, Clone, Default)]
pub struct Origin {
    /// `(slide, layer, shape)` of the picture itself, left out.
    pub at: Option<(usize, usize, usize)>,
    /// Its stored file: another shape showing the same file is the same
    /// picture whatever its hash says.
    pub file: Option<PictureRef>,
}

/// The pictures, GIFs and videos (by their posters) on `slides` that look
/// like `query`, most alike first, then in slide order. `signature`
/// gives a picture's signature from its bytes, `None` while it is not
/// read yet.
pub fn search<'a>(
    query: &Query,
    from: &Origin,
    slides: impl IntoIterator<Item = (usize, &'a Canvas)>,
    signature: impl Fn(&Arc<Vec<u8>>) -> Option<Signature>,
) -> Vec<Match> {
    let mut out = Vec::new();
    for (slide, c) in slides {
        for (layer, l) in c.layers.iter().enumerate() {
            for (shape, s) in l.shapes.iter().enumerate() {
                let Shape::Raster { png, kind: RasterKind::Image, picture, .. } = s else { continue };
                if from.at == Some((slide, layer, shape)) {
                    continue;
                }
                let same_file = match (&from.file, picture) {
                    (Some(a), Some(b)) if a.file == b.file => Some(a.crop == b.crop && a.mirror == b.mirror),
                    _ => None,
                };
                let likeness = match same_file {
                    Some(true) => Some(Likeness { kind: Kind::Duplicate, score: 1.0, way: Way::default(), other_crop: false }),
                    Some(false) => Some(Likeness { kind: Kind::Edited, score: 1.0, way: Way::default(), other_crop: true }),
                    None => signature(png).and_then(|sig| query.compare(&sig)),
                };
                if let Some(likeness) = likeness {
                    out.push(Match { slide, layer, shape, likeness, png: Arc::clone(png) });
                }
            }
        }
    }
    out.sort_by(|a, b| {
        a.likeness.kind.cmp(&b.likeness.kind).then(b.likeness.score.total_cmp(&a.likeness.score)).then((a.slide, a.layer, a.shape).cmp(&(b.slide, b.layer, b.shape)))
    });
    // Every copy, but only the closest of the merely similar.
    let copies = out.iter().take_while(|m| m.likeness.kind != Kind::Alike).count();
    out.truncate(copies + MOST_ALIKE);
    out
}

/// Most pictures listed by subject, or for typed words.
const MOST_BY_SUBJECT: usize = 24;
const MOST_BY_WORDS: usize = 24;
/// How far from the typical picture towards the closest one a picture
/// must be to be listed. How close "typical" is depends on the query:
/// measured 2026-11-18 on 7,186 reference pictures, a picture's median
/// likeness to the rest ran from 0.36 to 0.68 and its closest from 0.63
/// to 0.94, so no fixed bar fits every query. Words stand out less
/// (closest 0.11–0.16 over a median of 0.03–0.09), so their bar is
/// higher up.
const SUBJECT_CUT: f32 = 0.5;
const WORDS_CUT: f32 = 0.6;
/// Least likeness the closest picture must have for any to be listed for
/// typed words: below it the model gives the words under 0.1% odds of
/// showing in it. Real words on 1,639 references reached 0.109 to 0.158;
/// "the" 0.082, "spreadsheet invoice total" 0.055. (Some nonsense, such
/// as "zzz", reaches 0.111 and still lists its closest.)
const WORDS_LEAST: f32 = 0.09;
/// Likeness at which two listed pictures are the same picture: only the
/// first is listed.
const SAME_PICTURE: f32 = 0.985;

/// The pictures among `others` (an id and a vector of length 1 each,
/// from `canvas::picture_vectors`) that show what `query` shows, most
/// alike first, each with a score from 0 (typical) to 1 (the same). A
/// picture that is the same as one listed already is left out.
pub fn by_subject<'v>(query: &[f32], others: impl IntoIterator<Item = (usize, &'v [f32])>) -> Vec<(usize, f32)> {
    closest(query, others, SUBJECT_CUT, f32::MIN, MOST_BY_SUBJECT, |like, median, _| (like - median) / (1.0 - median).max(1e-6))
}

/// The pictures among `others` that show typed words whose vector is
/// `words`, most likely first, each with a score from 0 (typical) to 1
/// (the closest). A picture the same as one listed already is left out.
pub fn by_words<'v>(words: &[f32], others: impl IntoIterator<Item = (usize, &'v [f32])>) -> Vec<(usize, f32)> {
    closest(words, others, WORDS_CUT, WORDS_LEAST, MOST_BY_WORDS, |like, median, top| (like - median) / (top - median).max(1e-6))
}

/// The `most` of `others` closest to `query`, past `cut` of the way from
/// the median to the closest, none unless the closest reaches `least`;
/// `score(likeness, median, closest)` for each.
fn closest<'v>(
    query: &[f32],
    others: impl IntoIterator<Item = (usize, &'v [f32])>,
    cut: f32,
    least: f32,
    most: usize,
    score: impl Fn(f32, f32, f32) -> f32,
) -> Vec<(usize, f32)> {
    let dot = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
    let mut all: Vec<(usize, &[f32], f32)> = others.into_iter().map(|(id, v)| (id, v, dot(query, v))).collect();
    all.sort_by(|a, b| b.2.total_cmp(&a.2));
    let Some(&(_, _, top)) = all.first() else { return Vec::new() };
    if top < least {
        return Vec::new();
    }
    let median = all[all.len() / 2].2;
    let bar = median + cut * (top - median);
    let mut kept: Vec<(usize, &[f32], f32)> = Vec::new();
    for (id, v, like) in all {
        if like < bar || like <= median || kept.len() == most {
            break;
        }
        if kept.iter().any(|k| dot(k.1, v) >= SAME_PICTURE) {
            continue;
        }
        kept.push((id, v, like));
    }
    kept.into_iter().map(|(id, _, like)| (id, score(like, median, top).clamp(0.0, 1.0))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    /// A vector of length 1 whose likeness to `toward(0, 0.0)` is `like`,
    /// the rest of it along its own `axis` (so pictures differ).
    fn liking(like: f32, axis: usize) -> Vec<f32> {
        let mut v = vec![0.0; 16];
        v[0] = like;
        v[axis] = (1.0 - like * like).sqrt();
        v
    }

    #[test]
    fn by_words_lists_the_pictures_that_stand_out() {
        let mut w = vec![0.0; 16];
        w[0] = 1.0;
        // Typical pictures near 0.05; two stand out.
        let likes = [0.04, 0.05, 0.05, 0.06, 0.05, 0.15, 0.13, 0.08];
        let others: Vec<Vec<f32>> = likes.iter().enumerate().map(|(i, &l)| liking(l, i + 1)).collect();
        let found = by_words(&w, others.iter().enumerate().map(|(i, v)| (i, v.as_slice())));
        assert_eq!(found.iter().map(|f| f.0).collect::<Vec<_>>(), [5, 6]);
        assert!((found[0].1 - 1.0).abs() < 1e-6 && found[1].1 < 1.0);
        // Words nothing shows: none, however the rest compare.
        let weak: Vec<Vec<f32>> = [0.02, 0.08, 0.03].iter().enumerate().map(|(i, &l)| liking(l, i + 1)).collect();
        assert!(by_words(&w, weak.iter().enumerate().map(|(i, v)| (i, v.as_slice()))).is_empty());
    }

    /// A vector of length 1 pointing mostly along `axis`, `lean` of the
    /// way towards the next one.
    fn toward(axis: usize, lean: f32) -> Vec<f32> {
        let mut v = vec![0.0; 8];
        v[axis] = 1.0 - lean;
        v[(axis + 1) % 8] = lean;
        crate::ml::prep::unit(&v)
    }

    #[test]
    fn by_subject_lists_the_close_ones_first_and_stops_at_the_typical() {
        let q = toward(0, 0.0);
        let others: Vec<Vec<f32>> = vec![
            toward(0, 0.1), // close
            toward(3, 0.0), // unrelated
            toward(0, 0.3), // fairly close
            toward(4, 0.0),
            toward(5, 0.0),
            toward(6, 0.0),
        ];
        let found = by_subject(&q, others.iter().enumerate().map(|(i, v)| (i, v.as_slice())));
        assert_eq!(found.iter().map(|f| f.0).collect::<Vec<_>>(), [0, 2]);
        assert!(found[0].1 > found[1].1 && found[0].1 <= 1.0);
        assert!(by_subject(&q, std::iter::empty()).is_empty());
    }

    #[test]
    fn by_subject_lists_a_picture_once_however_many_copies() {
        let q = toward(0, 0.0);
        let others = [toward(0, 0.2), toward(0, 0.2), toward(0, 0.21), toward(2, 0.0), toward(3, 0.0), toward(4, 0.0)];
        let found = by_subject(&q, others.iter().enumerate().map(|(i, v)| (i, v.as_slice())));
        assert_eq!(found.iter().map(|f| f.0).collect::<Vec<_>>(), [0]);
    }

    /// A made-up picture: a gradient with a few blocks and a disc, laid
    /// out and coloured by `seed`.
    fn scene(seed: u32, w: u32, h: u32) -> RgbaImage {
        let mut rng = seed.wrapping_mul(2_654_435_761).max(1);
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 17;
            rng ^= rng << 5;
            rng
        };
        let base = [next() % 256, next() % 256, next() % 256].map(|v| v as f32);
        let mut img = RgbaImage::from_fn(w, h, |x, y| {
            let t = (x as f32 / w as f32 + y as f32 / h as f32) * 0.5;
            let [r, g, b] = [0, 1, 2].map(|c| (base[c] * (1.0 - t) + (255.0 - base[c]) * t) as u8);
            Rgba([r, g, b, 255])
        });
        for _ in 0..4 {
            let (x0, y0) = (next() % w, next() % h);
            let (bw, bh) = (w / 6 + next() % (w / 3), h / 6 + next() % (h / 3));
            let col = Rgba([(next() % 256) as u8, (next() % 256) as u8, (next() % 256) as u8, 255]);
            for y in y0..(y0 + bh).min(h) {
                for x in x0..(x0 + bw).min(w) {
                    img.put_pixel(x, y, col);
                }
            }
        }
        let (cx, cy, r) = ((next() % w) as f32, (next() % h) as f32, (h / 5) as f32);
        let col = Rgba([(next() % 256) as u8, (next() % 256) as u8, (next() % 256) as u8, 255]);
        for y in 0..h {
            for x in 0..w {
                if (x as f32 - cx).powi(2) + (y as f32 - cy).powi(2) < r * r {
                    img.put_pixel(x, y, col);
                }
            }
        }
        img
    }

    fn jpeg(img: &RgbaImage, quality: u8) -> RgbaImage {
        let (bytes, _) = crate::media::pureref::encode_picture(img, quality).unwrap();
        image::load_from_memory(&bytes).unwrap().to_rgba8()
    }

    fn found(q: &RgbaImage, other: &RgbaImage) -> Option<Likeness> {
        Query::of(q).unwrap().compare(&Signature::of(other).unwrap())
    }

    #[test]
    fn a_resized_recompressed_copy_is_a_duplicate() {
        let a = scene(1, 900, 600);
        let small = image::imageops::resize(&a, 413, 275, image::imageops::FilterType::CatmullRom);
        let l = found(&a, &jpeg(&small, 60)).unwrap();
        assert_eq!((l.kind, l.way), (Kind::Duplicate, Way::default()));
        assert!(l.score > 0.9, "{l:?}");
        // As a slide keeps it: a 96 px preview, JPEG at 80.
        let whole = PictureRef { file: String::new(), size: [900, 600], crop: None, mirror: false };
        let preview = jpeg(&whole.view(&a, crate::media::pureref::PREVIEW_SIDE), 80);
        assert_eq!(found(&a, &preview).unwrap().kind, Kind::Duplicate);
    }

    #[test]
    fn a_mirrored_or_turned_copy_is_found_and_says_how() {
        let a = scene(2, 640, 400);
        let l = found(&a, &image::imageops::flip_horizontal(&a)).unwrap();
        assert_eq!((l.kind, l.way), (Kind::Duplicate, Way { mirror: true, turns: 0 }));
        let l = found(&a, &image::imageops::rotate90(&a)).unwrap();
        assert_eq!((l.kind, l.way), (Kind::Duplicate, Way { mirror: false, turns: 1 }));
        let l = found(&a, &image::imageops::rotate270(&a)).unwrap();
        assert_eq!(l.way, Way { mirror: false, turns: 3 });
        let l = found(&a, &image::imageops::flip_vertical(&a)).unwrap();
        assert_eq!(l.way, Way { mirror: true, turns: 2 });
        assert_eq!(l.way.note(), "flipped top to bottom");
    }

    #[test]
    fn other_colours_make_a_near_duplicate() {
        let a = scene(3, 500, 500);
        let grey = image::DynamicImage::ImageRgba8(a.clone()).grayscale().to_rgba8();
        assert_eq!(found(&a, &grey).unwrap().kind, Kind::Edited);
        let mut warm = a.clone();
        for p in warm.pixels_mut() {
            p[0] = p[0].saturating_add(70);
            p[2] = p[2].saturating_sub(70);
        }
        assert_eq!(found(&a, &warm).unwrap().kind, Kind::Edited);
    }

    #[test]
    fn different_pictures_are_not_listed() {
        let mut listed = 0;
        for seed in 10..40 {
            if found(&scene(seed, 400, 300), &scene(seed + 100, 400, 300)).is_some() {
                listed += 1;
            }
        }
        // Made-up scenes share a style, so a few may look alike; never
        // more than that.
        assert!(listed <= 3, "{listed} of 30 unrelated pairs listed");
        for seed in 10..40 {
            let l = found(&scene(seed, 400, 300), &scene(seed + 100, 400, 300));
            assert!(l.map_or(true, |l| l.kind == Kind::Alike), "{seed}: {l:?}");
        }
    }

    #[test]
    fn flat_pictures_match_by_colour() {
        let white = RgbaImage::from_pixel(50, 50, Rgba([250, 250, 250, 255]));
        let white2 = RgbaImage::from_pixel(80, 80, Rgba([248, 249, 250, 255]));
        let black = RgbaImage::from_pixel(50, 50, Rgba([5, 5, 5, 255]));
        assert_eq!(found(&white, &white2).unwrap().kind, Kind::Duplicate);
        assert_ne!(found(&white, &black).map(|l| l.kind), Some(Kind::Duplicate));
        assert!(Query::of(&RgbaImage::new(0, 0)).is_none());
    }

    #[test]
    fn transparency_reads_as_mid_grey() {
        let a = scene(4, 300, 300);
        let mut cut = a.clone();
        for (x, _, p) in cut.enumerate_pixels_mut() {
            if x < 60 {
                *p = Rgba([0, 0, 0, 0]);
            }
        }
        let mut grey = a.clone();
        for (x, _, p) in grey.enumerate_pixels_mut() {
            if x < 60 {
                *p = Rgba([128, 128, 128, 255]);
            }
        }
        assert_eq!(found(&cut, &grey).unwrap().kind, Kind::Duplicate);
    }

    fn raster(png: Arc<Vec<u8>>, picture: Option<PictureRef>) -> Shape {
        Shape::Raster {
            pos: [0.0, 0.0],
            size: [10.0, 10.0],
            png,
            px_size: [10, 10],
            color: [0; 4],
            rotation: 0.0,
            flip: false,
            kind: RasterKind::Image,
            media: None,
            picture: picture.map(Box::new),
            pin: 0,
            group: 0,
            tags: Vec::new(),
            turn: 0.0,
            link: 0,
            paper: None,
        }
    }

    fn png_of(img: &RgbaImage) -> Arc<Vec<u8>> {
        let mut out = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
        Arc::new(out)
    }

    #[test]
    fn search_lists_alike_pictures_best_first_and_skips_the_query() {
        let a = scene(5, 300, 200);
        let mut s1 = Canvas::default();
        s1.layers[0].shapes.push(raster(png_of(&scene(77, 300, 200)), None)); // 0: unrelated
        s1.layers[0].shapes.push(raster(png_of(&image::DynamicImage::ImageRgba8(a.clone()).grayscale().to_rgba8()), None)); // 1
        s1.layers[0].shapes.push(raster(png_of(&a), None)); // 2: the query itself
        let mut s2 = Canvas::default();
        s2.layers[0].shapes.push(raster(png_of(&jpeg(&a, 70)), None)); // 0: a copy
        let crop = |c: Option<[u32; 4]>| PictureRef { file: "f.jpg".into(), size: [300, 200], crop: c, mirror: false };
        // Another crop of the query's own file, with pixels nothing like it.
        s2.layers[0].shapes.push(raster(png_of(&scene(99, 50, 50)), Some(crop(Some([0, 0, 50, 50]))))); // 1
        let from = Origin { at: Some((0, 0, 2)), file: Some(crop(None)) };
        let q = Query::of(&a).unwrap();
        let hits = search(&q, &from, [(0, &s1), (1, &s2)], |png| Signature::of_bytes(png));
        let got: Vec<_> = hits.iter().map(|m| (m.slide, m.shape, m.likeness.kind, m.likeness.other_crop)).collect();
        assert_eq!(got, vec![(1, 0, Kind::Duplicate, false), (1, 1, Kind::Edited, true), (0, 1, Kind::Edited, false)]);
        // Not read yet: not listed (but the same file still is).
        assert_eq!(search(&q, &from, [(1, &s2)], |_| None).len(), 1);
    }

    /// For a GUI check: in a scratch data folder (`OSN_DATA_DIR`, never
    /// the real one), the first slide becomes one picture that has
    /// copies on the other slides, so selecting it and asking for
    /// pictures like it lists them. The old first slide moves to the end.
    #[test]
    #[ignore]
    fn write_find_similar_test_slide() {
        let dir = std::path::PathBuf::from(std::env::var_os("OSN_DATA_DIR").expect("set OSN_DATA_DIR to a scratch copy")).join("canvases");
        assert!(!dir.to_string_lossy().contains("OnScreenNotes"), "not the real data folder");
        let mut slides = Vec::new();
        for i in 0.. {
            let Ok(text) = std::fs::read_to_string(dir.join(format!("canvas_{i}.ron"))) else { break };
            slides.push(crate::persistence::parse_canvas(&text, 1).unwrap());
        }
        let pictures: Vec<(usize, Shape)> = slides
            .iter()
            .enumerate()
            .flat_map(|(i, c)| c.layers.iter().flat_map(|l| &l.shapes).filter(|s| matches!(s, Shape::Raster { kind: RasterKind::Image, media: None, .. })).map(move |s| (i, s.clone())))
            .collect();
        let sig = |s: &Shape| match s {
            Shape::Raster { png, .. } => Signature::of_bytes(png),
            _ => None,
        };
        let sigs: Vec<Option<Signature>> = pictures.iter().map(|(_, s)| sig(s)).collect();
        let (pick, _) = pictures
            .iter()
            .enumerate()
            .find(|(k, (i, s))| {
                let Shape::Raster { png, .. } = s else { return false };
                let Some(q) = image::load_from_memory(png).ok().and_then(|img| Query::of(&img.to_rgba8())) else { return false };
                // Copies on other slides, not just this one.
                pictures.iter().zip(&sigs).enumerate().filter(|(j, ((js, _), sj))| j != k && js != i && sj.is_some_and(|sj| q.compare(&sj).is_some_and(|l| l.kind == Kind::Duplicate))).count() >= 2
            })
            .expect("a picture with copies on two other slides");
        let mut shape = pictures[pick].1.clone();
        if let Shape::Raster { pos, size, .. } = &mut shape {
            let s = 360.0 / size[0].max(size[1]);
            *size = [size[0] * s, size[1] * s];
            *pos = [300.0, 200.0];
        }
        let mut one = Canvas::default();
        one.layers[0].shapes.push(shape);
        let n = slides.len();
        std::fs::rename(dir.join("canvas_0.ron"), dir.join(format!("canvas_{n}.ron"))).unwrap();
        std::fs::write(dir.join("canvas_0.ron"), crate::persistence::serialize_canvas(&one).unwrap()).unwrap();
        println!("slide 1 is picture {pick} from slide {}", pictures[pick].0 + 1);
    }

    #[test]
    fn a_print_turned_reads_as_the_picture_turned() {
        let a = scene(6, 320, 200);
        let p = Print::of(&a).unwrap();
        let q = Query::of(&a).unwrap();
        for (i, (_, s)) in q.ways.iter().enumerate() {
            let t = p.way(i);
            assert_eq!(t.hash, s.hash, "way {i}");
            assert!((t.aspect - s.aspect).abs() < 1e-5);
            assert!(colour_gap(&t, s) < 1e-4, "way {i}");
        }
    }

    #[test]
    fn copies_are_gathered_into_sets() {
        let a = scene(7, 400, 300);
        let b = scene(8, 300, 300);
        let item = |img: &RgbaImage, keys: &[&str]| Item { print: Print::of(img), keys: keys.iter().map(|k| k.to_string()).collect() };
        let white = RgbaImage::from_pixel(40, 40, Rgba([250, 250, 250, 255]));
        let items = vec![
            item(&a, &[]),                                     // 0
            item(&b, &["bytes:1"]),                            // 1
            item(&jpeg(&image::imageops::resize(&a, 200, 150, image::imageops::FilterType::Triangle), 70), &[]), // 2: a, smaller
            item(&scene(9, 400, 300), &[]),                    // 3: alone
            item(&image::imageops::rotate90(&a), &[]),         // 4: a, turned
            Item { print: None, keys: vec!["bytes:1".into()] }, // 5: b's bytes again, not read
            item(&white, &[]),                                 // 6: flat: never by print
            item(&white, &[]),                                 // 7
            item(&white, &["file:w.png"]),                     // 8
            item(&white, &["file:w.png"]),                     // 9: same file
            item(&image::DynamicImage::ImageRgba8(b.clone()).grayscale().to_rgba8(), &[]), // 10: b recoloured: not a copy
        ];
        assert_eq!(copies(&items), vec![vec![0, 2, 4], vec![1, 5], vec![8, 9]]);
        assert!(copies(&[]).is_empty());
    }

    /// Every picture on a folder of slides (a copy: `OSN_TEST_SLIDES_DIR`)
    /// against every other: how long reading them takes, how many pairs
    /// each kind finds, and sheets of sample pairs written to
    /// `OSN_SIMILAR_OUT` to look at, best first, one pair a row.
    #[test]
    #[ignore]
    #[allow(clippy::needless_range_loop)]
    fn real_slides_similar() {
        let dir = std::path::PathBuf::from(std::env::var_os("OSN_TEST_SLIDES_DIR").expect("set OSN_TEST_SLIDES_DIR"));
        let out = std::env::var_os("OSN_SIMILAR_OUT").map(std::path::PathBuf::from);
        let mut files: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|e| e == "ron")).collect();
        files.sort();
        // One entry per distinct bytes: (slide file, picture file, bytes).
        let mut pics: Vec<(String, Option<String>, Arc<Vec<u8>>)> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for path in &files {
            let Ok(c) = crate::persistence::parse_canvas(&std::fs::read_to_string(path).unwrap(), 1) else { continue };
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            for s in c.layers.iter().flat_map(|l| &l.shapes) {
                if let Shape::Raster { png, kind: RasterKind::Image, picture, .. } = s {
                    if seen.insert(Arc::as_ptr(png) as usize) {
                        pics.push((name.clone(), picture.as_ref().map(|p| p.file.clone()), Arc::clone(png)));
                    }
                }
            }
        }
        let t = std::time::Instant::now();
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
        let chunk = pics.len().div_ceil(threads).max(1);
        let read: Vec<Option<(Query, Signature, Print, RgbaImage)>> = std::thread::scope(|s| {
            let parts: Vec<_> = pics
                .chunks(chunk)
                .map(|part| {
                    s.spawn(move || {
                        part.iter()
                            .map(|(_, _, png)| {
                                let img = image::load_from_memory(png).ok()?.to_rgba8();
                                Some((Query::of(&img)?, Signature::of(&img)?, Print::of(&img)?, img))
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            parts.into_iter().flat_map(|h| h.join().unwrap()).collect()
        });
        let one_thread_ms = t.elapsed().as_secs_f64() * 1e3 * threads as f64;
        println!("{} slides, {} distinct pictures, read in {:.0} ms on {threads} threads (~{:.2} ms each on one)", files.len(), pics.len(), t.elapsed().as_secs_f64() * 1e3, one_thread_ms / pics.len().max(1) as f64);
        let t = std::time::Instant::now();
        let mut pairs: Vec<(Kind, f32, usize, usize, u32, f32)> = Vec::new();
        let mut best_other = vec![0.0f32; read.len()];
        for i in 0..read.len() {
            let Some((q, _, _, _)) = &read[i] else { continue };
            for j in i + 1..read.len() {
                let Some((_, sj, _, _)) = &read[j] else { continue };
                if let Some(l) = q.compare(sj) {
                    let (bits, colour) = q.ways.iter().map(|(_, s)| ((s.hash ^ sj.hash).count_ones(), colour_gap(s, sj))).min_by_key(|(b, _)| *b).unwrap();
                    pairs.push((l.kind, l.score, i, j, bits, colour));
                }
                // The best score each picture gets from anything, listed or not.
                let s = q.ways.iter().map(|(_, s)| score((s.hash ^ sj.hash).count_ones(), colour_gap(s, sj))).fold(0.0, f32::max);
                best_other[i] = best_other[i].max(s);
                best_other[j] = best_other[j].max(s);
            }
        }
        println!("compared every pair in {:.0} ms", t.elapsed().as_secs_f64() * 1e3);
        // Sets of copies, as find lists them.
        let items: Vec<Item> = (0..pics.len())
            .map(|i| Item { print: read[i].as_ref().map(|r| r.2), keys: pics[i].1.iter().map(|f| format!("file:{f}")).collect() })
            .collect();
        let t = std::time::Instant::now();
        let sets = copies(&items);
        let mut sizes: Vec<usize> = sets.iter().map(Vec::len).collect();
        sizes.sort_unstable_by(|a, b| b.cmp(a));
        println!("{} sets of copies ({} pictures) in {:.0} ms; biggest sets: {:?}", sets.len(), sizes.iter().sum::<usize>(), t.elapsed().as_secs_f64() * 1e3, &sizes[..sizes.len().min(12)]);
        if let Some(out) = std::env::var_os("OSN_SIMILAR_OUT").map(std::path::PathBuf::from) {
            // The biggest sets, up to six of each, one set a row.
            let mut big: Vec<&Vec<usize>> = sets.iter().collect();
            big.sort_by_key(|s| std::cmp::Reverse(s.len()));
            let h = 80u32;
            let rows: Vec<&Vec<usize>> = big.into_iter().take(16).collect();
            let mut sheet = RgbaImage::from_pixel((h + 8) * 6, (h + 6) * rows.len().max(1) as u32, Rgba([40, 40, 40, 255]));
            for (r, set) in rows.iter().enumerate() {
                for (k, &i) in set.iter().take(6).enumerate() {
                    let src = &read[i].as_ref().unwrap().3;
                    let (w, hh) = (src.width() as f32, src.height() as f32);
                    let s = (h as f32 / w.max(hh)).min(1.0);
                    let small = image::imageops::resize(src, ((w * s) as u32).max(1), ((hh * s) as u32).max(1), image::imageops::FilterType::Triangle);
                    image::imageops::overlay(&mut sheet, &small, i64::from(k as u32 * (h + 8)), i64::from(r as u32 * (h + 6)));
                }
            }
            std::fs::create_dir_all(&out).unwrap();
            sheet.save(out.join("copies.png")).unwrap();
        }
        let same_file = |i: usize, j: usize| pics[i].1.is_some() && pics[i].1 == pics[j].1;
        for kind in [Kind::Duplicate, Kind::Edited, Kind::Alike] {
            let of: Vec<_> = pairs.iter().filter(|p| p.0 == kind).collect();
            let files = of.iter().filter(|p| same_file(p.2, p.3)).count();
            let across = of.iter().filter(|p| pics[p.2].0 != pics[p.3].0).count();
            println!("{}: {} pairs ({files} the same stored file, {across} across slides)", kind.label(), of.len());
        }
        let mut hist = [0usize; 10];
        for s in &best_other {
            hist[((s * 10.0) as usize).min(9)] += 1;
        }
        println!("best score each picture gets, by tenths: {hist:?}");
        let Some(out) = out else { return };
        std::fs::create_dir_all(&out).unwrap();
        // Sample sheets: each kind, and pairs just under the alike line.
        let near: Vec<_> = {
            let mut v = Vec::new();
            for i in 0..read.len() {
                let Some((q, _, _, _)) = &read[i] else { continue };
                for j in i + 1..read.len() {
                    let Some((_, sj, _, _)) = &read[j] else { continue };
                    if q.compare(sj).is_none() {
                        let (s, bits, colour) = q.ways.iter().map(|(_, s)| { let b = (s.hash ^ sj.hash).count_ones(); let c = colour_gap(s, sj); (score(b, c), b, c) }).fold((0.0, 0, 0.0), |a, b| if b.0 > a.0 { b } else { a });
                        if s >= ALIKE_SCORE - 0.06 {
                            v.push((Kind::Alike, s, i, j, bits, colour));
                        }
                    }
                }
            }
            v
        };
        let sheet = |name: &str, mut rows: Vec<(Kind, f32, usize, usize, u32, f32)>| {
            rows.sort_by(|a, b| b.1.total_cmp(&a.1));
            // Spread over the range: every n-th.
            let step = (rows.len() / 24).max(1);
            let rows: Vec<_> = rows.into_iter().step_by(step).take(24).collect();
            let h = 96u32;
            let mut img = RgbaImage::from_pixel(h * 2 * 2 + 12, (h + 6) * rows.len().max(1) as u32, Rgba([40, 40, 40, 255]));
            println!("{name}:");
            for (r, (_, s, i, j, bits, colour)) in rows.iter().enumerate() {
                println!("  row {r}: score {s:.2} bits {bits} colour {colour:.3}  {} / {}", pics[*i].0, pics[*j].0);
                for (k, idx) in [*i, *j].iter().enumerate() {
                    let src = &read[*idx].as_ref().unwrap().3;
                    let w = (src.width() as f32 * h as f32 / src.height() as f32).round().clamp(1.0, (h * 2) as f32) as u32;
                    let small = image::imageops::resize(src, w, h, image::imageops::FilterType::Triangle);
                    image::imageops::overlay(&mut img, &small, i64::from(k as u32 * (h * 2 + 12)), i64::from(r as u32 * (h + 6)));
                }
            }
            img.save(out.join(format!("{name}.png"))).unwrap();
        };
        let not_file: Vec<_> = pairs.iter().copied().filter(|p| !same_file(p.2, p.3)).collect();
        sheet("duplicate", not_file.iter().copied().filter(|p| p.0 == Kind::Duplicate).collect());
        sheet("edited", not_file.iter().copied().filter(|p| p.0 == Kind::Edited).collect());
        sheet("alike", not_file.iter().copied().filter(|p| p.0 == Kind::Alike).collect());
        sheet("below-alike", near);
    }
}
