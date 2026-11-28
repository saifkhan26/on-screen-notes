//! Resync: bring a slide opened from a PureRef board up to date with the
//! board, as a three-way merge (`BoardLink`).
//!
//! For each board item the slide has, every part the board changed since
//! the last sync — where it is, what it shows, a note's colours, its
//! group — is brought over; every part the board left alone keeps the
//! slide's own edits. Items new on the board are added, each at its
//! board depth (after the item below it on the board).
//!
//! A resync deletes nothing. What was added on the slide — ink, shapes,
//! tags typed in — is never touched. An item deleted from the board
//! stays on the slide, marked gone (`LinkedItem::gone`) for the media
//! filter to highlight. Group tags are only added: a group PureRef
//! renamed leaves its old name on its members too. An item deleted on
//! the slide stays deleted, and is not brought back. The whole resync is
//! one undo step.
//!
//! A slide opened before slides kept their link is linked on its first
//! resync by matching: pictures by the stored file they show (crop and
//! mirror included), notes by their text; where those repeat, and for
//! anything edited since, by where it is. Where it is comes from a
//! board-to-slide mapping fitted to the unambiguous matches. Matched
//! items keep the slide's version (it cannot be told what changed on
//! which side) but join their board groups and take their group tags.
//! A board group the slide has never had a group for is made on any
//! resync; once made, ungrouping it on the slide is the slide's edit.

use super::pureref::{Board, BoardItem, Have, Shown, StoredAs};
use crate::canvas::board_link::{fresh_link, BoardLink, ItemState, LinkedItem};
use crate::canvas::canvas::Canvas;
use crate::canvas::shape::{fresh_media_id, RasterKind, Shape};
use crate::canvas::units::same_tag;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

/// What a resync did, for the toast.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    /// Shapes the board changed.
    pub updated: usize,
    /// Board items new to the slide, added.
    pub added: usize,
    /// Shapes whose item went from the board since the last sync.
    pub gone: usize,
    /// On a first resync, shapes matched to board items.
    pub linked: Option<usize>,
    /// Board pictures that could not be shown (see `Board::skipped`).
    pub skipped: usize,
}

/// The merged slide, to put in place as one undo step.
pub struct Merged {
    /// Each changed layer's new shape list.
    pub layers: Vec<(usize, Vec<Shape>)>,
    pub link: BoardLink,
    /// The added shapes, as (layer, index) in the new lists.
    pub added: Vec<(usize, usize)>,
    pub report: Report,
}

/// What `canvas` shows that a resync need not decode and store again.
pub fn have(canvas: &Canvas) -> Have {
    let mut have = Have::default();
    for layer in &canvas.layers {
        for s in &layer.shapes {
            if let Some(f) = StoredAs::of(s) {
                have.files.insert(f);
            }
            if let (Some(item), Shape::Raster { .. }) = (canvas.board.as_ref().and_then(|b| b.item(s.link())), s) {
                have.contents.insert(item.base.content);
            }
        }
    }
    have
}

/// Merge `board`, read from `path`, into `canvas` (see the module doc).
/// `layer` takes added items when nothing on the slide places them. An
/// error when the slide was never linked and too little of it is on
/// the board: it was most likely opened from another one.
pub fn merge(canvas: &Canvas, board: Board, path: &Path, layer: usize) -> Result<Merged, String> {
    let sizes = board.group_sizes();
    let states: Vec<ItemState> = board.items.iter().map(|i| i.state(&sizes)).collect();
    let mut m = Merge {
        layers: canvas.layers.iter().map(|l| l.shapes.clone()).collect(),
        changed: HashSet::new(),
        report: Report { skipped: board.skipped, ..Report::default() },
    };
    let layer = layer.min(m.layers.len().saturating_sub(1));

    let (mut link, matched) = match canvas.board.as_deref() {
        Some(old) => {
            let mut link = old.clone();
            link.path = path.to_path_buf();
            let matched = match_linked(&link, &board, &states, &m.layers);
            (link, matched)
        }
        None => first_link(&board, &states, &mut m, path)?,
    };

    // Board item → (its linked item, where its shape is).
    let at = shape_places(&link, &m.layers);
    let donors = Donors::of(&link, &m.layers);
    // Board groups the slide has had a group for. One it never had (a
    // slide opened before groups came over, or linked before they were
    // put in place) is made now; one the slide has had stays as the
    // slide has it unless the board changes it.
    let placed: HashSet<i64> = link.groups.iter().map(|(r, _)| *r).collect();
    let mut on_board = vec![false; link.items.len()];
    let mut place_of: Vec<Option<(usize, usize)>> = vec![None; board.items.len()];
    for (bi, li) in matched.iter().enumerate() {
        let Some(li) = *li else { continue };
        on_board[li] = true;
        let item = &mut link.items[li];
        item.gone = false;
        item.key = board.items[bi].key();
        let base = std::mem::replace(&mut item.base, states[bi].clone());
        let id = item.link;
        let Some(&(l, i)) = at.get(&id) else { continue };
        place_of[bi] = Some((l, i));
        let shape = &mut m.layers[l][i];
        let regroup = states[bi].group.is_some_and(|r| !placed.contains(&r));
        if update(shape, &board.items[bi], &base, &states[bi], regroup, &mut link, &donors) {
            m.report.updated += 1;
            m.changed.insert(l);
        }
    }
    for (li, item) in link.items.iter_mut().enumerate() {
        if !on_board[li] && !item.gone {
            item.gone = true;
            if at.contains_key(&item.link) {
                m.report.gone += 1;
            }
        }
    }

    // New on the board: each goes in just above the nearest board item
    // below it that the slide shows, else just under the nearest above.
    let mut inserts: Vec<Insert> = Vec::new();
    for bi in (0..board.items.len()).filter(|&bi| matched[bi].is_none()) {
        let group = link.group_for(states[bi].group);
        let id = fresh_link();
        let shape = match board.items[bi].to_shape(&link, group, id) {
            Some(s) => s,
            None => match donors.lend(&board.items[bi]) {
                Some(shown) => with_shown(&board.items[bi], shown, &link, group, id),
                None => {
                    m.report.skipped += 1;
                    continue;
                }
            },
        };
        link.items.push(LinkedItem { link: id, key: board.items[bi].key(), base: states[bi].clone(), gone: false });
        let below = (0..bi).rev().find_map(|j| place_of[j]);
        let above = || (bi + 1..board.items.len()).find_map(|j| place_of[j]);
        let anchor = match (below, above()) {
            (Some((l, i)), _) => Anchor::After(l, i),
            (None, Some((l, i))) => Anchor::Before(l, i),
            (None, None) => Anchor::End(layer),
        };
        inserts.push(Insert { anchor, shape });
        m.report.added += 1;
    }
    let added = m.insert(inserts);

    let layers = m.changed.iter().map(|&l| (l, std::mem::take(&mut m.layers[l]))).collect();
    Ok(Merged { layers, link, added, report: m.report })
}

struct Merge {
    layers: Vec<Vec<Shape>>,
    changed: HashSet<usize>,
    report: Report,
}

enum Anchor {
    After(usize, usize),
    Before(usize, usize),
    End(usize),
}

struct Insert {
    anchor: Anchor,
    shape: Shape,
}

impl Merge {
    /// Put the inserts in, in order; where each one landed.
    fn insert(&mut self, inserts: Vec<Insert>) -> Vec<(usize, usize)> {
        let mut by_layer: HashMap<usize, Vec<Insert>> = HashMap::new();
        for ins in inserts {
            let l = match ins.anchor {
                Anchor::After(l, _) | Anchor::Before(l, _) | Anchor::End(l) => l,
            };
            by_layer.entry(l).or_default().push(ins);
        }
        let mut added = Vec::new();
        for (l, list) in by_layer {
            let Some(old) = self.layers.get_mut(l).map(std::mem::take) else { continue };
            let n = old.len();
            let mut before: Vec<Vec<Shape>> = (0..n).map(|_| Vec::new()).collect();
            let mut after: Vec<Vec<Shape>> = (0..n).map(|_| Vec::new()).collect();
            let mut end = Vec::new();
            for ins in list {
                match ins.anchor {
                    Anchor::After(_, i) if i < n => after[i].push(ins.shape),
                    Anchor::Before(_, i) if i < n => before[i].push(ins.shape),
                    _ => end.push(ins.shape),
                }
            }
            let mut out = Vec::with_capacity(n + end.len());
            for ((s, b), a) in old.into_iter().zip(before).zip(after) {
                for x in b {
                    added.push((l, out.len()));
                    out.push(x);
                }
                out.push(s);
                for x in a {
                    added.push((l, out.len()));
                    out.push(x);
                }
            }
            for x in end {
                added.push((l, out.len()));
                out.push(x);
            }
            self.layers[l] = out;
            self.changed.insert(l);
        }
        added.sort_unstable();
        added
    }
}

/// Link id → where its shape is (the first, should a copy share it).
fn shape_places(link: &BoardLink, layers: &[Vec<Shape>]) -> HashMap<u64, (usize, usize)> {
    let ids: HashSet<u64> = link.items.iter().map(|i| i.link).collect();
    let mut at = HashMap::new();
    for (l, shapes) in layers.iter().enumerate() {
        for (i, s) in shapes.iter().enumerate() {
            if ids.contains(&s.link()) {
                at.entry(s.link()).or_insert((l, i));
            }
        }
    }
    at
}

/// For each board item, the linked item it is: by its id on the board,
/// else — should PureRef have renumbered — one gone from the board that
/// shows the same.
fn match_linked(link: &BoardLink, board: &Board, states: &[ItemState], layers: &[Vec<Shape>]) -> Vec<Option<usize>> {
    let at = shape_places(link, layers);
    let kind_of = |li: usize| at.get(&link.items[li].link).map(|&(l, i)| matches!(layers[l][i], Shape::Text { .. }));
    let by_key: HashMap<i64, usize> = link.items.iter().enumerate().map(|(i, it)| (it.key, i)).collect();
    let mut taken = vec![false; link.items.len()];
    let mut out: Vec<Option<usize>> = vec![None; board.items.len()];
    for (bi, item) in board.items.iter().enumerate() {
        if let Some(&li) = by_key.get(&item.key()) {
            // An id that came to mean another kind of item is not it.
            if kind_of(li).map_or(true, |note| note == item.is_note()) && !taken[li] {
                taken[li] = true;
                out[bi] = Some(li);
            }
        }
    }
    // Items whose id the board no longer has, by what they show.
    let on_board: HashSet<i64> = board.items.iter().map(BoardItem::key).collect();
    let mut orphans: HashMap<u64, Vec<usize>> = HashMap::new();
    for (li, it) in link.items.iter().enumerate().rev() {
        if !taken[li] && !on_board.contains(&it.key) {
            orphans.entry(it.base.content).or_default().push(li);
        }
    }
    for bi in 0..board.items.len() {
        if out[bi].is_some() {
            continue;
        }
        let Some(list) = orphans.get_mut(&states[bi].content) else { continue };
        let note = board.items[bi].is_note();
        if let Some(k) = list.iter().rposition(|&li| kind_of(li).map_or(true, |n| n == note)) {
            let li = list.remove(k);
            taken[li] = true;
            out[bi] = Some(li);
        }
    }
    out
}

/// Bring the board's changes since `base` (now `new`) to `shape`.
/// Whether anything changed.
/// `regroup`: put it in its board group even if the board did not
/// change that (the slide never had the group).
fn update(shape: &mut Shape, item: &BoardItem, base: &ItemState, new: &ItemState, regroup: bool, link: &mut BoardLink, donors: &Donors) -> bool {
    let mut changed = false;
    if !base.same_place(new) {
        place(shape, item, link);
        changed = true;
    }
    if base.content != new.content {
        match item {
            BoardItem::Note { text, .. } => {
                if let Shape::Text { content, .. } = shape {
                    *content = text.clone();
                    changed = true;
                }
            }
            BoardItem::Picture { shown, .. } => {
                if let Some(shown) = shown.as_deref().cloned().or_else(|| donors.lend(item)) {
                    show(shape, &shown);
                    changed = true;
                }
            }
        }
    }
    if base.style != new.style {
        if let (BoardItem::Note { color: c, background: b, .. }, Shape::Text { color, background, .. }) = (item, &mut *shape) {
            *color = *c;
            *background = *b;
            changed = true;
        }
    }
    if base.group != new.group || regroup {
        let g = link.group_for(new.group);
        if shape.group() != g {
            shape.set_group(g);
            changed = true;
        }
    }
    if base.tags != new.tags {
        // Only added: a resync deletes nothing, so a tag a group had
        // before PureRef renamed it stays.
        let mut tags: Vec<String> = shape.tags().to_vec();
        for t in &new.tags {
            if !tags.iter().any(|x| same_tag(x, t)) {
                tags.push(t.clone());
            }
        }
        if tags != shape.tags() {
            shape.set_tags(&tags);
            changed = true;
        }
    }
    changed
}

/// Put `shape` where the board has `item`.
fn place(shape: &mut Shape, item: &BoardItem, link: &BoardLink) {
    let s = link.scale;
    match (item, shape) {
        (BoardItem::Picture { center, half, rotation: r, .. }, Shape::Raster { pos, size, rotation, turn, .. }) => {
            let c = link.to_slide(*center);
            let h = [(half[0] * s) as f32, (half[1] * s) as f32];
            *pos = [c[0] - h[0], c[1] - h[1]];
            *size = [h[0] * 2.0, h[1] * 2.0];
            *rotation = *r as f32;
            *turn = 0.0;
        }
        (BoardItem::Note { pos: p, font_size: f, rotation: r, .. }, Shape::Text { pos, font_size, rotation, turn, .. }) => {
            *pos = link.to_slide(*p);
            *font_size = (f * s) as f32;
            *rotation = *r as f32;
            *turn = 0.0;
        }
        _ => {}
    }
}

/// Make `shape` show `shown`. A flip made on the slide stays.
fn show(shape: &mut Shape, shown: &Shown) {
    if let Shape::Raster { png, px_size, picture, media, kind, .. } = shape {
        *png = Arc::clone(&shown.png);
        *px_size = shown.px_size;
        *picture = shown.picture.clone().map(Box::new);
        *media = shown.media.clone().map(|mut m| {
            m.id = fresh_media_id();
            Box::new(m)
        });
        *kind = RasterKind::Image;
    }
}

/// `item`'s shape, showing `shown` (it was not loaded).
fn with_shown(item: &BoardItem, shown: Shown, link: &BoardLink, group: u64, id: u64) -> Shape {
    let mut s = Shape::Raster {
        pos: [0.0; 2],
        size: [1.0; 2],
        png: Arc::new(Vec::new()),
        px_size: [1, 1],
        color: [0, 0, 0, 0],
        rotation: 0.0,
        kind: RasterKind::Image,
        media: None,
        picture: None,
        pin: 0,
        flip: false,
        group,
        tags: item.tags().to_vec(),
        turn: 0.0,
        link: id,
        paper: None,
        grain: None,
    };
    place(&mut s, item, link);
    show(&mut s, &shown);
    s
}

/// Shapes on the slide that show what a board picture not loaded shows,
/// to borrow its stored form from.
struct Donors {
    by_content: HashMap<u64, Shown>,
    by_file: HashMap<StoredAs, Shown>,
}

impl Donors {
    fn of(link: &BoardLink, layers: &[Vec<Shape>]) -> Self {
        let mut d = Donors { by_content: HashMap::new(), by_file: HashMap::new() };
        for s in layers.iter().flatten() {
            let Some(shown) = shown_of(s) else { continue };
            if let Some(item) = link.item(s.link()) {
                d.by_content.entry(item.base.content).or_insert_with(|| shown.clone());
            }
            if let Some(f) = StoredAs::of(s) {
                d.by_file.entry(f).or_insert(shown);
            }
        }
        d
    }

    fn lend(&self, item: &BoardItem) -> Option<Shown> {
        let BoardItem::Picture { content, file, .. } = item else { return None };
        self.by_content.get(content).or_else(|| file.as_ref().and_then(|f| self.by_file.get(f))).cloned()
    }
}

fn shown_of(s: &Shape) -> Option<Shown> {
    match s {
        Shape::Raster { png, px_size, picture, media, .. } if picture.is_some() || media.is_some() => Some(Shown {
            png: Arc::clone(png),
            px_size: *px_size,
            picture: picture.as_deref().cloned(),
            media: media.as_deref().cloned(),
        }),
        _ => None,
    }
}

// ---------------------------------------------------------------------
// First resync of a slide opened before slides kept their link
// ---------------------------------------------------------------------

/// What a board item or a shape is matched by.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Ident {
    File(StoredAs),
    Text(String),
}

fn ident_of_item(item: &BoardItem) -> Option<Ident> {
    match item {
        BoardItem::Picture { file, shown, .. } => file
            .clone()
            .or_else(|| {
                let s = shown.as_ref()?;
                match (&s.media, &s.picture) {
                    (Some(m), _) => Some(StoredAs { file: m.file.clone(), crop: None, mirror: false }),
                    (None, Some(p)) => Some(StoredAs { file: p.file.clone(), crop: p.crop, mirror: p.mirror }),
                    _ => None,
                }
            })
            .map(Ident::File),
        BoardItem::Note { text, .. } => Some(Ident::Text(text.clone())),
    }
}

fn ident_of_shape(s: &Shape) -> Option<Ident> {
    match s {
        Shape::Text { content, .. } => Some(Ident::Text(content.clone())),
        _ => StoredAs::of(s).map(Ident::File),
    }
}

/// An item's centre and size (a picture's half width; a note's font
/// size), in board units.
fn board_frame(item: &BoardItem) -> ([f64; 2], f64) {
    match item {
        BoardItem::Picture { center, half, .. } => (*center, half[0]),
        BoardItem::Note { pos, font_size, .. } => (*pos, *font_size),
    }
}

/// The same for a shape, in slide units.
fn slide_frame(s: &Shape) -> ([f64; 2], f64) {
    match s {
        Shape::Raster { pos, size, .. } => ([f64::from(pos[0] + size[0] * 0.5), f64::from(pos[1] + size[1] * 0.5)], f64::from(size[0] * 0.5)),
        Shape::Text { pos, font_size, .. } => ([f64::from(pos[0]), f64::from(pos[1])], f64::from(*font_size)),
        _ => ([0.0; 2], 0.0),
    }
}

fn median(mut v: Vec<f64>) -> Option<f64> {
    v.retain(|x| x.is_finite());
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    Some(v[v.len() / 2])
}

/// Link a slide to `board` by matching (see the module doc), linking
/// the matched shapes in `m.layers`. The link, and for each board item
/// the linked item it is.
fn first_link(board: &Board, states: &[ItemState], m: &mut Merge, path: &Path) -> Result<(BoardLink, Vec<Option<usize>>), String> {
    let name = path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    // Shapes by what they show.
    let mut shapes: HashMap<Ident, Vec<(usize, usize)>> = HashMap::new();
    let mut candidates = 0;
    for (l, list) in m.layers.iter().enumerate() {
        for (i, s) in list.iter().enumerate() {
            if let Some(id) = ident_of_shape(s) {
                shapes.entry(id).or_default().push((l, i));
                candidates += 1;
            }
        }
    }
    let idents: Vec<Option<Ident>> = board.items.iter().map(ident_of_item).collect();
    let mut on_board: HashMap<&Ident, usize> = HashMap::new();
    for id in idents.iter().flatten() {
        *on_board.entry(id).or_default() += 1;
    }

    // Fit board → slide on what matches one to one.
    let (mut scales, mut pairs) = (Vec::new(), Vec::new());
    for (bi, id) in idents.iter().enumerate() {
        let Some(id) = id else { continue };
        let Some([(l, i)]) = shapes.get(id).map(Vec::as_slice) else { continue };
        if on_board.get(id) != Some(&1) {
            continue;
        }
        let (bc, bs) = board_frame(&board.items[bi]);
        let (sc, ss) = slide_frame(&m.layers[*l][*i]);
        if bs > 1e-9 {
            scales.push(ss / bs);
        }
        pairs.push((bc, sc));
    }
    let scale = median(scales).filter(|s| *s > 1e-9).ok_or_else(|| format!("Nothing on this slide is on {name} — pick the board it was opened from"))?;
    let offset = [
        median(pairs.iter().map(|(b, s)| s[0] - b[0] * scale).collect()).unwrap_or(0.0),
        median(pairs.iter().map(|(b, s)| s[1] - b[1] * scale).collect()).unwrap_or(0.0),
    ];
    let mut link = BoardLink { path: path.to_path_buf(), scale, offset, items: Vec::new(), groups: Vec::new() };
    let to_slide = |p: [f64; 2]| [p[0] * scale + offset[0], p[1] * scale + offset[1]];
    let dist = |bi: usize, (l, i): (usize, usize), layers: &[Vec<Shape>]| {
        let (bc, _) = board_frame(&board.items[bi]);
        let (sc, _) = slide_frame(&layers[l][i]);
        let p = to_slide(bc);
        (p[0] - sc[0]).hypot(p[1] - sc[1])
    };

    // Same thing shown: nearest first.
    let mut pairs: Vec<(f64, usize, (usize, usize))> = Vec::new();
    for (bi, id) in idents.iter().enumerate() {
        for &at in id.as_ref().and_then(|id| shapes.get(id)).into_iter().flatten() {
            pairs.push((dist(bi, at, &m.layers), bi, at));
        }
    }
    let mut matched: Vec<Option<(usize, usize)>> = vec![None; board.items.len()];
    let mut used: HashSet<(usize, usize)> = HashSet::new();
    pairs.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (_, bi, at) in pairs {
        if matched[bi].is_none() && !used.contains(&at) {
            matched[bi] = Some(at);
            used.insert(at);
        }
    }
    // Edited since (a note's text, a re-encoded picture): the same kind
    // of thing, about as big, about where the board has it.
    let mut near: Vec<(f64, usize, (usize, usize))> = Vec::new();
    for bi in (0..board.items.len()).filter(|&bi| matched[bi].is_none()) {
        let (_, bs) = board_frame(&board.items[bi]);
        for (l, list) in m.layers.iter().enumerate() {
            for (i, s) in list.iter().enumerate() {
                let kind_ok = matches!(
                    (&board.items[bi], s),
                    (BoardItem::Note { .. }, Shape::Text { .. }) | (BoardItem::Picture { .. }, Shape::Raster { kind: RasterKind::Image, .. })
                );
                if !kind_ok || used.contains(&(l, i)) || s.link() != 0 {
                    continue;
                }
                let (_, ss) = slide_frame(s);
                let ratio = ss / (bs * scale).max(1e-9);
                let d = dist(bi, (l, i), &m.layers);
                if (0.8..=1.25).contains(&ratio) && d <= (bs * scale).max(4.0) {
                    near.push((d, bi, (l, i)));
                }
            }
        }
    }
    near.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (_, bi, at) in near {
        if matched[bi].is_none() && !used.contains(&at) {
            matched[bi] = Some(at);
            used.insert(at);
        }
    }

    let count = matched.iter().flatten().count();
    if count * 2 < candidates.min(board.items.len()) {
        return Err(format!("Only {count} of this slide's {candidates} pictures and notes are on {name} — pick the board it was opened from"));
    }

    // Link what matched. The slide's version stays (base = the board as
    // it is now), except the board groups and their tags, which old
    // slides may not have had.
    let mut out = vec![None; board.items.len()];
    for (bi, at) in matched.iter().enumerate() {
        let Some((l, i)) = *at else { continue };
        let id = fresh_link();
        let shape = &mut m.layers[l][i];
        shape.set_link(id);
        m.changed.insert(l);
        let state = &states[bi];
        if let Some(root) = state.group.filter(|_| shape.group() != 0) {
            if !link.groups.iter().any(|(r, _)| *r == root) {
                link.groups.push((root, shape.group()));
            }
        }
        let base = ItemState { tags: Vec::new(), group: None, ..state.clone() };
        out[bi] = Some(link.items.len());
        link.items.push(LinkedItem { link: id, key: board.items[bi].key(), base, gone: false });
    }
    m.report.linked = Some(count);
    Ok((link, out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::shape::PictureRef;

    fn pic(key: i64, file: &str, center: [f64; 2], group: i64, tags: &[&str]) -> BoardItem {
        let picture = PictureRef { file: file.into(), size: [10, 10], crop: None, mirror: false };
        let content = crate::canvas::board_link::Fnv::of(file.as_bytes());
        BoardItem::Picture {
            shown: Some(Box::new(Shown { png: Arc::new(vec![1, 2, 3]), px_size: [10, 10], picture: Some(picture), media: None })),
            file: Some(StoredAs { file: file.into(), crop: None, mirror: false }),
            content,
            center,
            half: [5.0, 5.0],
            rotation: 0.0,
            group,
            key,
            tags: tags.iter().map(|t| t.to_string()).collect(),
        }
    }

    fn note(key: i64, text: &str, pos: [f64; 2]) -> BoardItem {
        BoardItem::Note { text: text.into(), pos, font_size: 20.0, color: [255; 4], rotation: 0.0, background: None, group: key, key, tags: Vec::new() }
    }

    fn board(items: Vec<BoardItem>) -> Board {
        Board { items, skipped: 0, created: Vec::new() }
    }

    /// A slide opened from `items`, laid out 1:1 with no offset.
    fn opened(items: Vec<BoardItem>) -> Canvas {
        let link = BoardLink { path: "b.pur".into(), scale: 1.0, offset: [0.0, 0.0], items: Vec::new(), groups: Vec::new() };
        super::super::pureref::lay_out(&board(items), link, "b")
    }

    fn apply(c: &mut Canvas, merged: Merged) {
        let entry = c.swap_resynced(merged.layers, Some(Box::new(merged.link)));
        c.undo_log.push(entry);
    }

    fn center(s: &Shape) -> [f32; 2] {
        match s {
            Shape::Raster { pos, size, .. } => [pos[0] + size[0] * 0.5, pos[1] + size[1] * 0.5],
            Shape::Text { pos, .. } => *pos,
            _ => [0.0; 2],
        }
    }

    #[test]
    fn board_changes_win_slide_changes_stay_and_additions_are_kept() {
        let mut c = opened(vec![pic(1, "a.png", [0.0, 0.0], 1, &[]), pic(2, "b.png", [100.0, 0.0], 2, &[]), note(3, "hi", [0.0, 50.0])]);
        // On the slide: move picture 2, retext the note, tag picture 1,
        // and draw something new.
        c.layers[0].shapes[1].translate([0.0, 30.0]);
        if let Shape::Text { content, .. } = &mut c.layers[0].shapes[2] {
            *content = "hello".into();
        }
        c.layers[0].shapes[0].set_tags(&["mine".to_string()]);
        c.layers[0].shapes.push(crate::canvas::shape::tests_support::rect([500.0, 500.0], [510.0, 510.0]));
        // On the board: picture 1 moves, picture 2 is left alone, the note
        // is untouched, a new picture appears on top.
        let b = board(vec![pic(1, "a.png", [-40.0, 0.0], 1, &[]), pic(2, "b.png", [100.0, 0.0], 2, &[]), note(3, "hi", [0.0, 50.0]), pic(4, "c.png", [200.0, 0.0], 4, &[])]);
        let merged = merge(&c, b, Path::new("b.pur"), 0).unwrap();
        assert_eq!(merged.report, Report { updated: 1, added: 1, ..Report::default() });
        apply(&mut c, merged);
        let shapes = &c.layers[0].shapes;
        assert_eq!(shapes.len(), 5);
        assert_eq!(center(&shapes[0]), [-40.0, 0.0], "the board moved it");
        assert_eq!(shapes[0].tags(), ["mine".to_string()], "the slide's tag stays");
        assert_eq!(center(&shapes[1]), [100.0, 30.0], "the slide moved it, the board did not");
        assert!(matches!(&shapes[2], Shape::Text { content, .. } if content == "hello"));
        assert_eq!(center(&shapes[3]), [200.0, 0.0], "new, above the note it comes after");
        assert!(matches!(shapes[4], Shape::Rect { .. }), "the slide's own shape is kept");

        // One undo puts it all back.
        c.undo();
        assert_eq!(c.layers[0].shapes.len(), 4);
        assert_eq!(center(&c.layers[0].shapes[0]), [0.0, 0.0]);
        c.redo();
        assert_eq!(c.layers[0].shapes.len(), 5);
    }

    #[test]
    fn gone_from_the_board_stays_and_deleted_on_the_slide_stays_deleted() {
        let mut c = opened(vec![pic(1, "a.png", [0.0, 0.0], 1, &[]), pic(2, "b.png", [100.0, 0.0], 2, &[])]);
        c.layers[0].shapes.remove(1);
        let gone_link = c.layers[0].shapes[0].link();
        // The board drops picture 1 and moves picture 2.
        let merged = merge(&c, board(vec![pic(2, "b.png", [150.0, 0.0], 2, &[])]), Path::new("b.pur"), 0).unwrap();
        assert_eq!(merged.report, Report { gone: 1, ..Report::default() });
        apply(&mut c, merged);
        assert_eq!(c.layers[0].shapes.len(), 1, "picture 2 is not brought back");
        assert!(c.board.as_ref().unwrap().is_gone(gone_link), "picture 1 is marked gone");
        // Back on the board: no longer gone.
        let merged = merge(&c, board(vec![pic(1, "a.png", [0.0, 0.0], 1, &[])]), Path::new("b.pur"), 0).unwrap();
        apply(&mut c, merged);
        assert!(!c.board.as_ref().unwrap().is_gone(gone_link));
    }

    #[test]
    fn group_tags_follow_the_board_and_leave_the_slides_own() {
        let mut c = opened(vec![pic(1, "a.png", [0.0, 0.0], 9, &["walk"]), pic(2, "b.png", [100.0, 0.0], 9, &["walk"])]);
        assert_eq!(c.layers[0].shapes[0].tags(), ["walk".to_string()], "opened with its group's name");
        let g = c.layers[0].shapes[0].group();
        assert!(g != 0 && g == c.layers[0].shapes[1].group());
        c.layers[0].shapes[0].set_tags(&["walk".to_string(), "mine".to_string()]);
        // The group is renamed on the board, and a new picture joins it.
        let b = board(vec![pic(1, "a.png", [0.0, 0.0], 9, &["run"]), pic(2, "b.png", [100.0, 0.0], 9, &["run"]), pic(3, "c.png", [50.0, 0.0], 9, &["run"])]);
        let merged = merge(&c, b, Path::new("b.pur"), 0).unwrap();
        apply(&mut c, merged);
        let shapes = &c.layers[0].shapes;
        let tags = |s: &[&str]| s.iter().map(|t| t.to_string()).collect::<Vec<_>>();
        assert_eq!(shapes[0].tags(), tags(&["walk", "mine", "run"]), "the new name joins; nothing is taken away");
        assert_eq!(shapes[1].tags(), tags(&["walk", "run"]));
        assert_eq!(shapes[2].group(), g, "joins the slide group its board group is");
    }

    /// A board group the slide never had is made: on a first link, and on
    /// a slide linked before groups were put in place. Ungrouped on the
    /// slide after that, it stays ungrouped.
    #[test]
    fn board_groups_the_slide_never_had_are_made() {
        let items = || vec![pic(1, "a.png", [0.0, 0.0], 9, &["walk"]), pic(2, "b.png", [100.0, 0.0], 9, &["walk"]), pic(3, "c.png", [0.0, 100.0], 3, &[])];
        // An old slide: nothing linked, nothing grouped.
        let mut c = opened(items());
        c.board = None;
        for s in &mut c.layers[0].shapes {
            s.set_link(0);
            s.set_group(0);
            s.set_tags(&[]);
        }
        let merged = merge(&c, board(items()), Path::new("b.pur"), 0).unwrap();
        apply(&mut c, merged);
        let g = c.layers[0].shapes[0].group();
        assert!(g != 0 && c.layers[0].shapes[1].group() == g, "the named group is a group");
        assert_eq!(c.layers[0].shapes[2].group(), 0, "a lone item stays alone");

        // Linked, but the group never put in place (as slides linked by
        // the first version of resync).
        for s in &mut c.layers[0].shapes {
            s.set_group(0);
        }
        c.board.as_mut().unwrap().groups.clear();
        let merged = merge(&c, board(items()), Path::new("b.pur"), 0).unwrap();
        assert_eq!(merged.report.updated, 2);
        apply(&mut c, merged);
        let g = c.layers[0].shapes[0].group();
        assert!(g != 0 && c.layers[0].shapes[1].group() == g);

        // Ungrouped on the slide since: left so.
        for s in &mut c.layers[0].shapes {
            s.set_group(0);
        }
        let merged = merge(&c, board(items()), Path::new("b.pur"), 0).unwrap();
        assert_eq!(merged.report, Report::default());
    }

    /// Board ids start at 0, so a group can be rooted at item 0.
    #[test]
    fn a_group_rooted_at_item_0_is_a_group() {
        let mut c = opened(vec![pic(0, "a.png", [0.0, 0.0], 0, &[]), pic(1, "b.png", [100.0, 0.0], 0, &[])]);
        let g = c.layers[0].shapes[0].group();
        assert!(g != 0 && g == c.layers[0].shapes[1].group());
        // A third joins it on the board.
        let b = board(vec![pic(0, "a.png", [0.0, 0.0], 0, &[]), pic(1, "b.png", [100.0, 0.0], 0, &[]), pic(2, "c.png", [50.0, 0.0], 0, &[])]);
        let merged = merge(&c, b, Path::new("b.pur"), 0).unwrap();
        apply(&mut c, merged);
        assert!(c.layers[0].shapes.iter().all(|s| s.group() == g));
    }

    /// Should PureRef give its items new ids, they are still the same
    /// items: matched by what they show, not added again or marked gone.
    #[test]
    fn renumbered_items_are_still_theirs() {
        let mut c = opened(vec![pic(1, "a.png", [0.0, 0.0], 1, &[]), note(2, "hi", [0.0, 50.0])]);
        let b = board(vec![pic(7, "a.png", [10.0, 0.0], 7, &[]), note(8, "hi", [0.0, 50.0])]);
        let merged = merge(&c, b, Path::new("b.pur"), 0).unwrap();
        assert_eq!(merged.report, Report { updated: 1, ..Report::default() }, "the move, nothing added or gone");
        apply(&mut c, merged);
        assert_eq!(c.layers[0].shapes.len(), 2);
        let keys: Vec<i64> = c.board.as_ref().unwrap().items.iter().map(|i| i.key).collect();
        assert_eq!(keys, vec![7, 8]);
    }

    #[test]
    fn unloaded_pictures_borrow_from_the_slide() {
        let mut c = opened(vec![pic(1, "a.png", [0.0, 0.0], 1, &[])]);
        let have = have(&c);
        let mut dup = pic(2, "a.png", [100.0, 0.0], 2, &[]);
        assert!(have.contents.len() == 1 && have.files.len() == 1);
        // A copy of picture 1 added on the board: not loaded.
        if let BoardItem::Picture { shown, .. } = &mut dup {
            *shown = None;
        }
        let merged = merge(&c, board(vec![pic(1, "a.png", [0.0, 0.0], 1, &[]), dup]), Path::new("b.pur"), 0).unwrap();
        assert_eq!(merged.report.added, 1);
        apply(&mut c, merged);
        let Shape::Raster { picture, png, .. } = &c.layers[0].shapes[1] else { panic!("a picture") };
        assert_eq!(picture.as_ref().unwrap().file, "a.png");
        assert_eq!(**png, vec![1, 2, 3]);
    }

    /// Links real slides to their real boards: `OSN_TEST_RESYNC` lists
    /// `slide.ron=board.pur` pairs separated by `;` (copies of slide
    /// files: they are only read). Pictures that need storing go to a
    /// scratch folder. Checks nothing on the slide is lost, then that a
    /// second resync changes nothing.
    #[test]
    #[ignore]
    fn resyncs_real_slides() {
        let list = std::env::var("OSN_TEST_RESYNC").expect("set OSN_TEST_RESYNC to slide.ron=board.pur pairs, separated by ;");
        let media = std::env::temp_dir().join(format!("osn-resync-real-{}", std::process::id()));
        std::fs::create_dir_all(&media).unwrap();
        for pair in list.split(';').filter(|p| !p.trim().is_empty()) {
            let (slide, board_path) = pair.split_once('=').expect("slide.ron=board.pur");
            let board_path = Path::new(board_path.trim());
            let text = std::fs::read_to_string(slide.trim()).unwrap();
            let mut c = crate::persistence::parse_canvas(&text, 1).unwrap();
            let before: Vec<usize> = c.layers.iter().map(|l| l.shapes.len()).collect();
            let strokes: usize = c.layers.iter().map(|l| l.strokes.len()).sum();
            let tags_before: usize = c.layers.iter().flat_map(|l| &l.shapes).map(|s| s.tags().len()).sum();
            let files_before: HashSet<String> = c.layers.iter().flat_map(|l| &l.shapes).filter_map(|s| s.media_file().map(str::to_string)).collect();
            let mut total_added = 0;
            for round in 0..2 {
                let t = std::time::Instant::now();
                let have = have(&c);
                let progress = super::super::pureref::Progress::default();
                let board = super::super::pureref::open(board_path, &media, &progress, &have).unwrap();
                let read = t.elapsed();
                let (pictures, notes) = board.counts();
                let loaded = board.items.iter().filter(|i| matches!(i, BoardItem::Picture { shown: Some(_), .. })).count();
                let merged = merge(&c, board, board_path, c.active_layer).unwrap_or_else(|e| panic!("{}: {e}", board_path.display()));
                println!(
                    "{} round {round}: board {pictures} pictures + {notes} notes, {loaded} decoded, read in {read:?}; {:?}",
                    board_path.display(),
                    merged.report
                );
                total_added += merged.report.added;
                if !merged.layers.is_empty() {
                    c.apply_resync(merged.layers, merged.link);
                } else {
                    c.board = Some(Box::new(merged.link));
                }
                // Nothing lost: every layer has what it had and the added.
                let after: Vec<usize> = c.layers.iter().map(|l| l.shapes.len()).collect();
                assert_eq!(after.iter().sum::<usize>(), before.iter().sum::<usize>() + total_added);
                assert!(after.iter().zip(&before).all(|(a, b)| a >= b));
                assert_eq!(c.layers.iter().map(|l| l.strokes.len()).sum::<usize>(), strokes, "ink untouched");
                assert!(c.layers.iter().flat_map(|l| &l.shapes).map(|s| s.tags().len()).sum::<usize>() >= tags_before, "no tag removed");
                let files_after: HashSet<String> = c.layers.iter().flat_map(|l| &l.shapes).filter_map(|s| s.media_file().map(str::to_string)).collect();
                assert!(files_before.is_subset(&files_after), "every picture the slide showed it still shows");
                if round == 1 {
                    assert_eq!((merged.report.updated, merged.report.added, merged.report.gone), (0, 0, 0), "a second resync changes nothing");
                    assert_eq!(loaded, 0, "and decodes nothing");
                }
            }
            let mut tags: Vec<&String> = c.layers.iter().flat_map(|l| &l.shapes).flat_map(|s| s.tags()).collect();
            tags.sort();
            tags.dedup();
            println!("  {} distinct tags, e.g. {:?}", tags.len(), &tags[..tags.len().min(6)]);
            // Survives a save round trip with its link.
            let saved = crate::persistence::serialize_canvas(&c).unwrap();
            let back = crate::persistence::parse_canvas(&saved, 1).unwrap();
            assert_eq!(back.board.as_ref().map(|b| b.items.len()), c.board.as_ref().map(|b| b.items.len()));
            println!("  slide file {:.1} MB → {:.1} MB", text.len() as f64 / 1e6, saved.len() as f64 / 1e6);
        }
        let _ = std::fs::remove_dir_all(&media);
    }

    #[test]
    fn old_slides_link_by_matching() {
        let items = || vec![pic(1, "a.png", [0.0, 0.0], 1, &[]), pic(2, "a.png", [100.0, 0.0], 2, &[]), pic(3, "b.png", [0.0, 100.0], 3, &["pose"]), note(4, "hi", [50.0, 50.0])];
        let mut c = opened(items());
        // As an old slide: no link, laid out at twice the size, shifted.
        c.board = None;
        for s in &mut c.layers[0].shapes {
            s.set_link(0);
            s.set_tags(&[]);
            match s {
                Shape::Raster { pos, size, .. } => {
                    *pos = [pos[0] * 2.0 + 10.0, pos[1] * 2.0 + 10.0];
                    *size = [size[0] * 2.0, size[1] * 2.0];
                }
                Shape::Text { pos, font_size, .. } => {
                    *pos = [pos[0] * 2.0 + 10.0, pos[1] * 2.0 + 10.0];
                    *font_size *= 2.0;
                }
                _ => {}
            }
        }
        // The note was edited, and one copy of a.png deleted on the slide.
        if let Shape::Text { content, .. } = &mut c.layers[0].shapes[3] {
            *content = "hi there".into();
        }
        c.layers[0].shapes.remove(1);
        let merged = merge(&c, board(items()), Path::new("b.pur"), 0).unwrap();
        assert_eq!(merged.report.linked, Some(3));
        assert_eq!(merged.report.added, 1, "the deleted copy comes back, to look over");
        let link = &merged.link;
        assert!((link.scale - 2.0).abs() < 1e-6 && (link.offset[0] - 10.0).abs() < 1e-4, "{} {:?}", link.scale, link.offset);
        apply(&mut c, merged);
        let shapes = &c.layers[0].shapes;
        assert_eq!(shapes.len(), 4);
        assert!(shapes.iter().all(|s| s.link() != 0));
        assert_eq!(center(&shapes[1]), [210.0, 10.0], "the copy, where the board has it, above the one below it");
        assert!(matches!(&shapes[3], Shape::Text { content, .. } if content == "hi there"), "the slide's text stays");
        assert_eq!(shapes[2].tags(), ["pose".to_string()], "group tags come in");

        // Another board entirely: refused.
        let mut c2 = opened(vec![pic(1, "x.png", [0.0, 0.0], 1, &[]), pic(2, "y.png", [10.0, 0.0], 2, &[])]);
        c2.board = None;
        assert!(merge(&c2, board(items()), Path::new("b.pur"), 0).is_err());
    }
}
