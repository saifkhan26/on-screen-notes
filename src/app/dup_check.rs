//! A picture pasted or dropped in that is a copy of one already on the
//! slides (`similar::Print::duplicate_of`: the same picture, maybe resized,
//! re-saved, mirrored or turned) gets a ring, and a bar says where the
//! other is: show it, remove the new one, or keep both — remembered like
//! "ignore" in find's duplicates list (`copy-ignores.ron`), so that pair is
//! never flagged again.
//!
//! The rings are a highlight like the toolbar's: they stay until hidden —
//! the bar's ×, the highlight menu's "Duplicates just added" switch or its
//! "Unhighlight all" (`MediaFilter::duplicates`). The next paste or drop
//! starts a new check.
//!
//! Every picture on the slides is read once a run for this
//! (`PictureIndex`, as find does, on its worker threads), so the first
//! check takes a few seconds. The check itself runs on a thread of its own
//! (`match_items`): the UI thread only lists the pictures (`gather`) and
//! takes the answer, so a paste never holds up a frame.

use super::*;
use crate::canvas::shape::{MediaKind, PictureRef, RasterKind, Shape};
use crate::persistence::copy_ignores::{CopyIgnores, Ignore};
use crate::tools::similar::Print;
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, TryRecvError};
use std::sync::Arc;

/// Pictures put in this soon after the last are checked with it, as one
/// lot (files dropped together come in over a few frames).
const BATCH: Duration = Duration::from_secs(3);
/// Screen points between a picture and its ring.
const RING_PAD: f32 = 6.0;

/// A picture followed while it moves: its slide and layer, its place in
/// the layer when last seen, and its bytes' address (`Arc::as_ptr`).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Spot {
    slide: usize,
    layer: usize,
    shape: usize,
    key: usize,
}

impl Spot {
    fn of(slide: usize, layer: usize, shape: usize, png: &Arc<Vec<u8>>) -> Spot {
        Spot { slide, layer, shape, key: Arc::as_ptr(png) as usize }
    }

    /// The picture, where it is now on its layer (at its old place if it
    /// is still there); `None` once it is gone.
    fn locate<'a>(&mut self, canvases: &'a crate::canvas::CanvasManager) -> Option<&'a Shape> {
        let l = canvases.slide(self.slide)?.layers.get(self.layer)?;
        let is = |s: &Shape| matches!(s, Shape::Raster { png, .. } if Arc::as_ptr(png) as usize == self.key);
        if !l.shapes.get(self.shape).is_some_and(is) {
            self.shape = l.shapes.iter().position(is)?;
        }
        l.shapes.get(self.shape)
    }

    /// The picture store moved these bytes to the media folder: follow
    /// them to the slide's new bytes, as the pad does.
    fn follow(&mut self, swaps: &[(Arc<Vec<u8>>, PictureRef)], canvases: &crate::canvas::CanvasManager) {
        if !swaps.iter().any(|(from, _)| Arc::as_ptr(from) as usize == self.key) {
            return;
        }
        if let Some(Shape::Raster { png, .. }) = canvases.slide(self.slide).and_then(|c| c.layers.get(self.layer)).and_then(|l| l.shapes.get(self.shape)) {
            self.key = Arc::as_ptr(png) as usize;
        }
    }
}

/// One picture just put in, and the copies of it found.
#[derive(Debug, Clone)]
struct Flagged {
    new: Spot,
    copies: Vec<Spot>,
}

/// The pictures pasted or dropped last, being checked or found to have
/// copies.
pub(super) struct DupCheck {
    new: Vec<Spot>,
    /// When the last of them came in.
    last: Instant,
    /// Put in through the slide window: its bar shows there.
    in_window: bool,
    /// Once checked: those with copies.
    flagged: Option<Vec<Flagged>>,
    /// The copy "Show it" went to last, counting through every flagged
    /// picture's copies in turn.
    shown: Option<usize>,
    /// The check under way on its thread.
    job: Option<Job>,
}

/// A picture on the slides as the check's thread sees it: where it is,
/// its bytes, its print once read, and whether its bytes are kept in the
/// slide (it is then known by them, `similar::bytes_id`).
struct Item {
    spot: Spot,
    png: Arc<Vec<u8>>,
    print: Option<Print>,
    inline: bool,
}

/// The check under way on its thread, and where each picture it was
/// given is (by its index there).
struct Job {
    rx: mpsc::Receiver<Found>,
    spots: Vec<Spot>,
}

/// What the thread found: each new picture with copies and theirs (by
/// index), the bytes ids of those kept in their slide, and what was
/// ignored in the duplicates list, if it had to be read.
struct Found {
    pairs: Vec<(usize, Vec<usize>)>,
    bytes_ids: HashMap<usize, String>,
    ignores: Option<CopyIgnores>,
}

impl DupCheck {
    fn copies(&self) -> Vec<Spot> {
        self.flagged.iter().flatten().flat_map(|f| f.copies.iter().copied()).collect()
    }
}

/// What the bar was asked to do.
enum BarAction {
    Show,
    BackToNew,
    Remove,
    Keep,
    Hide,
}

/// What a picture is known by from run to run, in the duplicates list
/// and what was ignored of it (`app::similar`).
fn persistent_id(shape: &Shape) -> Option<String> {
    let Shape::Raster { png, picture, media, .. } = shape else { return None };
    Some(match (picture, media) {
        (Some(p), _) => similar::picture_id(p),
        (None, Some(m)) => format!("media:{}", m.file),
        (None, None) => similar::bytes_id(png),
    })
}

/// A picture or GIF (not a video): what is checked for copies.
fn still_or_gif(shape: &Shape) -> Option<&Arc<Vec<u8>>> {
    match shape {
        Shape::Raster { png, kind: RasterKind::Image, media, .. } if !media.as_ref().is_some_and(|m| m.kind == MediaKind::Video) => Some(png),
        _ => None,
    }
}

/// The bar over the bottom of the canvas: `text`, then its buttons —
/// `show` the copies, back to the new picture when `away` from its slide,
/// remove or keep it (`one`) or them, and hide.
fn bar(ctx: &egui::Context, text: &str, show: &str, away: bool, one: bool) -> Option<BarAction> {
    let mut action = None;
    egui::Area::new(egui::Id::new("osn_dup_bar"))
        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -72.0))
        .show(ctx, |ui| {
            ui::theme::card_frame().show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(text);
                    ui.add_space(8.0);
                    if ui.button(show).on_hover_text("Go to the picture already there").clicked() {
                        action = Some(BarAction::Show);
                    }
                    if away && ui.button("Back to the new one").clicked() {
                        action = Some(BarAction::BackToNew);
                    }
                    let remove = if one { "Remove this one" } else { "Remove the new ones" };
                    if ui.button(remove).on_hover_text("Delete what was just added; undo brings it back").clicked() {
                        action = Some(BarAction::Remove);
                    }
                    let keep = if one { "Keep both" } else { "Keep them" };
                    if ui.button(keep).on_hover_text("They are not copies: never flag them again (find's duplicates list leaves them out too)").clicked() {
                        action = Some(BarAction::Keep);
                    }
                    if ui.add(egui::Button::new(egui::RichText::new("×").size(16.0)).frame(false)).on_hover_text("Hide the rings — the highlight menu shows them again").clicked() {
                        action = Some(BarAction::Hide);
                    }
                });
            });
        });
    action
}

/// The bar as it looks, for the offscreen snapshots (`ui::snapshot`).
#[cfg(test)]
pub(crate) fn snapshot_bar(ctx: &egui::Context) {
    bar(ctx, "Duplicate of a picture on slide 47 \u{201c}Random Inspo 7\u{201d}", "Show it", false, true);
}

/// Every picture and GIF on the slides, with its print as `print` has it,
/// and which of them are `new` (by index). The one part of a check on the
/// UI thread: it reads no file and compares nothing — a pointer, a print
/// copied and a lookup per picture.
fn gather(canvases: &crate::canvas::CanvasManager, new: &[Spot], print: impl Fn(&Arc<Vec<u8>>) -> Option<Print>) -> (Vec<Item>, Vec<usize>) {
    // Sized first: growing it would copy every print again on each step.
    let count = (0..canvases.count()).filter_map(|si| canvases.slide(si)).flat_map(|c| &c.layers).flat_map(|l| &l.shapes).filter(|s| still_or_gif(s).is_some()).count();
    let mut items = Vec::with_capacity(count);
    for si in 0..canvases.count() {
        let Some(c) = canvases.slide(si) else { continue };
        for (li, l) in c.layers.iter().enumerate() {
            for (shi, s) in l.shapes.iter().enumerate() {
                let Some(png) = still_or_gif(s) else { continue };
                let inline = matches!(s, Shape::Raster { picture: None, media: None, .. });
                items.push(Item { spot: Spot::of(si, li, shi, png), png: Arc::clone(png), print: print(png), inline });
            }
        }
    }
    let new = new
        .iter()
        .filter_map(|n| items.iter().position(|it| (it.spot.slide, it.spot.layer, it.spot.key) == (n.slide, n.layer, n.key)))
        .collect();
    (items, new)
}

/// For each of `new` (indexes into `items`), the others that are the same
/// picture — maybe resized, re-saved, mirrored or turned
/// (`Print::duplicate_of`). Shared bytes are a copy made in the app, not
/// one brought in again: left out. Runs on the check's own thread.
fn match_items(items: &[Item], new: &[usize]) -> Vec<(usize, Vec<usize>)> {
    new.iter()
        .filter_map(|&n| {
            let it = items.get(n)?;
            let p = it.print?;
            let copies: Vec<usize> = items
                .iter()
                .enumerate()
                .filter(|&(j, o)| j != n && !Arc::ptr_eq(&o.png, &it.png) && o.print.is_some_and(|q| p.duplicate_of(&q)))
                .map(|(j, _)| j)
                .collect();
            (!copies.is_empty()).then_some((n, copies))
        })
        .collect()
}

/// `pairs` without the copies kept apart from their new picture in the
/// duplicates list (`apart`, each pair `(smaller, larger)`); `id` gives
/// what a picture is known by there. A new picture left with no copies goes.
fn drop_apart(pairs: Vec<(usize, Vec<usize>)>, id: impl Fn(usize) -> Option<String>, apart: &HashSet<(String, String)>) -> Vec<(usize, Vec<usize>)> {
    if apart.is_empty() {
        return pairs;
    }
    pairs
        .into_iter()
        .filter_map(|(n, copies)| {
            let Some(a) = id(n) else { return Some((n, copies)) };
            let copies: Vec<usize> = copies
                .into_iter()
                .filter(|&c| id(c).is_none_or(|b| !apart.contains(&if a <= b { (a.clone(), b) } else { (b, a.clone()) })))
                .collect();
            (!copies.is_empty()).then_some((n, copies))
        })
        .collect()
}

impl OnScreenNotesApp {
    /// Pictures just pasted or dropped onto the active slide — the
    /// selection `insert_and_select` left — are checked for copies. A lot
    /// that came in moments ago is checked again with them.
    pub(super) fn dup_watch_selection(&mut self) {
        let slide = self.canvases.active_index();
        let c = self.canvases.active();
        let layer = c.active_layer;
        let Some(l) = c.layers.get(layer) else { return };
        let spots: Vec<Spot> =
            self.ws.tool.select.sel.shapes.iter().filter_map(|&i| Some(Spot::of(slide, layer, i, still_or_gif(l.shapes.get(i)?)?))).collect();
        if spots.is_empty() {
            return;
        }
        let in_window = self.canvases.in_view();
        match self.dup_check.as_mut() {
            Some(d) if d.last.elapsed() < BATCH && d.in_window == in_window => {
                d.new.extend(spots);
                d.last = Instant::now();
                d.flagged = None;
                d.shown = None;
                // A check under way is for fewer: its answer is let go.
                d.job = None;
            }
            _ => self.dup_check = Some(DupCheck { new: spots, last: Instant::now(), in_window, flagged: None, shown: None, job: None }),
        }
    }

    /// The picture store moved pictures' bytes to the media folder.
    pub(super) fn dup_follow_store(&mut self, swaps: &[(Arc<Vec<u8>>, PictureRef)]) {
        let Some(d) = self.dup_check.as_mut() else { return };
        let canvases = &self.canvases;
        for s in &mut d.new {
            s.follow(swaps, canvases);
        }
        for f in d.flagged.iter_mut().flatten() {
            f.new.follow(swaps, canvases);
            for s in &mut f.copies {
                s.follow(swaps, canvases);
            }
        }
        for s in d.job.iter_mut().flat_map(|j| &mut j.spots) {
            s.follow(swaps, canvases);
        }
    }

    /// How many pictures just put in have copies, for the highlight menu;
    /// `None` when none do (no row there).
    pub(super) fn dup_flagged_count(&self) -> Option<usize> {
        self.dup_check.as_ref()?.flagged.as_ref().filter(|f| !f.is_empty()).map(Vec::len)
    }

    /// Once a frame: follow the pictures, drop what has gone, and check
    /// the new ones once every picture on the slides is read.
    pub(super) fn update_dup_check(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.dup_check.take() else { return };
        let canvases = &self.canvases;
        d.new.retain_mut(|s| s.locate(canvases).is_some());
        if let Some(f) = d.flagged.as_mut() {
            for g in f.iter_mut() {
                g.copies.retain_mut(|s| s.locate(canvases).is_some());
            }
            f.retain_mut(|g| !g.copies.is_empty() && g.new.locate(canvases).is_some());
        }
        if d.new.is_empty() || d.flagged.as_ref().is_some_and(Vec::is_empty) {
            return;
        }
        if d.flagged.is_none() {
            if d.job.is_none() {
                d.job = self.dup_start(&d.new, ctx);
            }
            // Never waits: the answer is taken on the frame it is in.
            match d.job.as_ref().map(|j| j.rx.try_recv()) {
                Some(Ok(found)) => {
                    let job = d.job.take().expect("a check is running");
                    let f = self.dup_found(&job, found);
                    if f.is_empty() {
                        return;
                    }
                    d.flagged = Some(f);
                    // A highlight like the menu's: on until hidden.
                    self.media_filter.duplicates = true;
                }
                Some(Err(TryRecvError::Disconnected)) => {
                    log::warn!("the duplicate check stopped without an answer");
                    return;
                }
                Some(Err(TryRecvError::Empty)) | None => ctx.request_repaint_after(Duration::from_millis(100)),
            }
        }
        self.dup_check = Some(d);
    }

    /// Start checking `new` on a thread of its own, once every slide is in
    /// and every picture on them read (until then `None`, tried again next
    /// frame). On the UI thread only the pictures are listed (`gather`).
    fn dup_start(&mut self, new: &[Spot], ctx: &egui::Context) -> Option<Job> {
        let canvases = &self.canvases;
        let active = canvases.active_index();
        let order: Vec<usize> = std::iter::once(active).chain((0..canvases.count()).filter(|&i| i != active)).collect();
        let slides: Vec<&crate::canvas::canvas::Canvas> = order.iter().filter_map(|&i| canvases.slide(i)).collect();
        if slides.len() < canvases.count() {
            return None;
        }
        // Hands new pictures to the index's worker threads, at most every
        // `picture_index::LOOK_EVERY`; reading them happens there.
        let index = &mut self.picture_index;
        let first = index.progress().1 == 0;
        index.tick(slides.iter().copied(), first);
        let (read, total) = index.progress();
        if read < total || total == 0 {
            return None;
        }
        let index = &*index;
        let (items, new) = gather(canvases, new, |png| index.print(png));
        if new.is_empty() {
            return None;
        }
        let spots = items.iter().map(|it| it.spot).collect();
        // The duplicates list's ignored pairs, read there if not yet.
        let read_ignores = self.copy_ignores.is_none();
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new().name("osn-dup-check".into()).spawn(move || {
            let t = Instant::now();
            let pairs = match_items(&items, &new);
            let mut bytes_ids = HashMap::new();
            for &i in pairs.iter().flat_map(|(n, copies)| std::iter::once(n).chain(copies)) {
                if items[i].inline {
                    bytes_ids.entry(i).or_insert_with(|| similar::bytes_id(&items[i].png));
                }
            }
            let ignores = (read_ignores && !pairs.is_empty()).then(CopyIgnores::load);
            log::info!("{} new pictures checked against {} for copies in {} ms", new.len(), items.len(), t.elapsed().as_millis());
            let _ = tx.send(Found { pairs, bytes_ids, ignores });
            ctx.request_repaint_of(egui::ViewportId::ROOT);
        });
        match spawned {
            Ok(_) => Some(Job { rx, spots }),
            Err(e) => {
                log::warn!("could not start checking for copies: {e}");
                None
            }
        }
    }

    /// The thread's answer as pictures to ring: each new one with its
    /// copies, less those kept apart in the duplicates list (Keep both).
    fn dup_found(&mut self, job: &Job, found: Found) -> Vec<Flagged> {
        if let Some(read) = found.ignores {
            self.copy_ignores.get_or_insert(read);
        }
        let canvases = &self.canvases;
        // What each is known by there: its stored file, or its video or
        // GIF file, else its bytes (hashed on the thread).
        let id = |i: usize| -> Option<String> {
            let mut s = *job.spots.get(i)?;
            match s.locate(canvases)? {
                Shape::Raster { picture: Some(p), .. } => Some(similar::picture_id(p)),
                Shape::Raster { media: Some(m), .. } => Some(format!("media:{}", m.file)),
                _ => found.bytes_ids.get(&i).cloned(),
            }
        };
        let none = HashSet::new();
        let apart = self.copy_ignores.as_ref().map_or(&none, |i| i.pairs());
        drop_apart(found.pairs, id, apart)
            .into_iter()
            .filter_map(|(n, copies)| Some(Flagged { new: *job.spots.get(n)?, copies: copies.into_iter().filter_map(|c| job.spots.get(c).copied()).collect() }))
            .collect()
    }

    /// The rings round the pictures just put in that have copies and
    /// round those copies on the slide shown, and the bar about them —
    /// while the duplicates highlight is on.
    pub(super) fn show_dup_check(&mut self, ctx: &egui::Context, canvas_rect: egui::Rect) {
        let Some(d) = self.dup_check.as_ref() else { return };
        let Some(flagged) = d.flagged.as_ref().filter(|f| !f.is_empty()) else { return };
        if !self.media_filter.duplicates {
            return;
        }
        let slide = self.canvases.active_index();
        let c = self.canvases.active();
        let to_screen = |p: [f32; 2]| egui::pos2(canvas_rect.min.x + c.pan[0] + p[0] * c.zoom, canvas_rect.min.y + c.pan[1] + p[1] * c.zoom);
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("osn_dup_rings")));
        let ring = |s: &Spot, width: f32| {
            if s.slide != slide {
                return;
            }
            let Some(shape) = c.layers.get(s.layer).and_then(|l| l.shapes.get(s.shape)) else { return };
            let o = shape.obb();
            let half = [o.half[0] * c.zoom + RING_PAD, o.half[1] * c.zoom + RING_PAD];
            let pts = crate::canvas::render::box_ring(to_screen(o.center), half, o.angle, RING_PAD);
            // A light ring over a dark halo, as find's: it shows on a white page too.
            painter.add(egui::Shape::closed_line(pts.clone(), egui::Stroke::new(width + 3.5, egui::Color32::BLACK.gamma_multiply(0.45))));
            painter.add(egui::Shape::closed_line(pts, egui::Stroke::new(width, ui::find_bar::FOUND)));
        };
        for f in flagged {
            ring(&f.new, 2.5);
            for s in &f.copies {
                ring(s, 1.5);
            }
        }
        if !self.ws.ui_visible || d.in_window != self.canvases.in_view() {
            return;
        }
        let action = self.dup_bar(ctx, d, flagged);
        if let Some(a) = action {
            self.dup_action(a, ctx);
        }
    }

    /// The bar: where the copies are, and what to do about them.
    fn dup_bar(&self, ctx: &egui::Context, d: &DupCheck, flagged: &[Flagged]) -> Option<BarAction> {
        let canvases = &self.canvases;
        let place = |s: &Spot, from: usize| {
            if s.slide == from {
                "on this slide".to_string()
            } else {
                match canvases.name(s.slide) {
                    "" => format!("on slide {}", s.slide + 1),
                    name => format!("on slide {} \u{201c}{name}\u{201d}", s.slide + 1),
                }
            }
        };
        let copies = d.copies();
        let text = match flagged {
            [f] if f.copies.len() == 1 => format!("Duplicate of a picture {}", place(&f.copies[0], f.new.slide)),
            [f] => format!("Duplicate: {} copies already, the first {}", f.copies.len(), place(&f.copies[0], f.new.slide)),
            many if many.len() == d.new.len() => format!("All {} just added are duplicates", many.len()),
            many => format!("{} of the {} just added are duplicates", many.len(), d.new.len()),
        };
        let show = match (copies.len(), d.shown) {
            (1, _) => "Show it".to_string(),
            (n, Some(i)) => format!("Next copy ({}/{n})", (i + 1) % n + 1),
            (n, None) => format!("Show it (1/{n})"),
        };
        let away = flagged.iter().all(|f| f.new.slide != canvases.active_index());
        bar(ctx, &text, &show, away, flagged.len() == 1)
    }

    fn dup_action(&mut self, action: BarAction, ctx: &egui::Context) {
        match action {
            BarAction::Hide => self.media_filter.duplicates = false,
            BarAction::Show => {
                let Some(d) = self.dup_check.as_mut() else { return };
                let copies = d.copies();
                if copies.is_empty() {
                    return;
                }
                let i = d.shown.map_or(0, |i| (i + 1) % copies.len());
                d.shown = Some(i);
                self.dup_go_to(copies[i], ctx);
            }
            BarAction::BackToNew => {
                let Some(first) = self.dup_check.as_ref().and_then(|d| d.flagged.as_ref()?.first().map(|f| f.new)) else { return };
                self.dup_go_to(first, ctx);
            }
            BarAction::Remove => self.dup_remove_new(),
            BarAction::Keep => self.dup_keep_both(),
        }
    }

    /// Go to picture `s` and flash it, as find does.
    fn dup_go_to(&mut self, mut s: Spot, ctx: &egui::Context) {
        let Some(Shape::Raster { png, .. }) = s.locate(&self.canvases) else { return };
        let png = Arc::clone(png);
        let copy = crate::tools::similar::Likeness { kind: crate::tools::similar::Kind::Duplicate, score: 1.0, way: Default::default(), other_crop: false };
        let Some(hit) = self.canvases.slide(s.slide).and_then(|c| ui::find_bar::picture_hit(c, s.slide, s.layer, s.shape, copy, png)) else { return };
        self.go_to_find_hit(&hit.hit, ctx);
    }

    /// Delete every picture just put in that has copies: one undo step on
    /// its slide each.
    fn dup_remove_new(&mut self) {
        let Some(d) = self.dup_check.take() else { return };
        let mut removed = Vec::new();
        for f in d.flagged.into_iter().flatten() {
            let mut s = f.new;
            let Some(Shape::Raster { png, .. }) = s.locate(&self.canvases) else { continue };
            let png = Arc::clone(png);
            if self.canvases.delete_picture(s.slide, s.layer, s.shape, &png) {
                removed.push(s.slide);
            }
        }
        if removed.contains(&self.canvases.active_index()) {
            self.ws.tool.select.sel = crate::canvas::hit::Selection::default();
            self.ws.tool.select.rebind(self.canvases.active_index(), self.canvases.active());
        }
        let undo = self.undo_key();
        let msg = match removed.as_slice() {
            [] => "Nothing removed: that slide is locked".to_string(),
            [s] if *s == self.canvases.active_index() => format!("Removed the new copy — {undo} brings it back"),
            [s] => format!("Removed the new copy — {undo} on slide {} brings it back", s + 1),
            many => format!("Removed {} new copies — {undo} brings them back", many.len()),
        };
        self.toast = Some((msg, Instant::now()));
    }

    /// The pictures just put in are not copies of what was found: remember
    /// each pair, as find's duplicates list does when one is ignored.
    fn dup_keep_both(&mut self) {
        let Some(d) = self.dup_check.take() else { return };
        let canvases = &self.canvases;
        let ignores = self.copy_ignores.get_or_insert_with(CopyIgnores::load);
        for f in d.flagged.into_iter().flatten() {
            let mut n = f.new;
            let Some(picture) = n.locate(canvases).and_then(persistent_id) else { continue };
            let of: Vec<String> = f.copies.into_iter().filter_map(|mut s| s.locate(canvases).and_then(persistent_id)).collect();
            ignores.add(Ignore::Picture { picture, of });
        }
        self.toast = Some(("Kept — they won't be flagged as copies again".to_string(), Instant::now()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::canvas::Canvas;
    use crate::canvas::shape::MediaRef;
    use crate::canvas::CanvasManager;

    /// A picture `w` × `h`: stripes and a blob, so it has a shape to
    /// fingerprint; `seed` makes another picture.
    fn png(w: u32, h: u32, seed: u32) -> Arc<Vec<u8>> {
        let img = image::RgbaImage::from_fn(w, h, |x, y| {
            let (u, v) = (x * 64 / w, y * 64 / h);
            let on = (u / 8 + v / 16 + seed).is_multiple_of(2) ^((u as i32 - 40).pow(2) + (v as i32 - 20).pow(2) < 150 + seed as i32 * 40);
            if on { image::Rgba([230, 40 + (seed * 50 % 200) as u8, 30, 255]) } else { image::Rgba([20, 20, 60 + (seed * 30 % 150) as u8, 255]) }
        });
        let mut out = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
        Arc::new(out)
    }

    fn flat(w: u32, h: u32) -> Arc<Vec<u8>> {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([200, 200, 200, 255]));
        let mut out = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
        Arc::new(out)
    }

    fn picture(png: &Arc<Vec<u8>>) -> Shape {
        Shape::Raster {
            pos: [0.0, 0.0],
            size: [100.0, 100.0],
            png: Arc::clone(png),
            px_size: [1, 1],
            color: [0; 4],
            rotation: 0.0,
            kind: RasterKind::Image,
            media: None,
            picture: None,
            pin: 0,
            flip: false,
            group: 0,
            tags: Vec::new(),
            turn: 0.0,
            link: 0,
            paper: None,
        }
    }

    fn slide(shapes: Vec<Shape>) -> Canvas {
        let mut c = Canvas::default();
        c.layers[0].shapes = shapes;
        c
    }

    fn print(png: &Arc<Vec<u8>>) -> Option<Print> {
        Print::of_bytes(png)
    }

    /// Where each flagged picture's copies are, `(slide, shape)`: listed
    /// as the UI thread does, matched as the check's thread does, less the
    /// pairs kept `apart` (pictures known as "slide:shape" here).
    fn found(m: &CanvasManager, new: &[Spot], apart: &HashSet<(String, String)>) -> Vec<Vec<(usize, usize)>> {
        let (items, new) = gather(m, new, print);
        let id = |i: usize| Some(format!("{}:{}", items[i].spot.slide, items[i].spot.shape));
        drop_apart(match_items(&items, &new), id, apart)
            .into_iter()
            .map(|(_, copies)| copies.into_iter().map(|c| (items[c].spot.slide, items[c].spot.shape)).collect())
            .collect()
    }

    fn pairs(p: &[(&str, &str)]) -> HashSet<(String, String)> {
        p.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    #[test]
    fn a_picture_brought_in_again_resized_is_found_on_another_slide() {
        let (a, other) = (png(400, 300, 1), png(400, 300, 2));
        let again = png(200, 150, 1);
        let m = CanvasManager::for_tests(vec![slide(vec![picture(&other), picture(&a)]), slide(vec![picture(&again)])]);
        let new = [Spot::of(1, 0, 0, &again)];
        assert_eq!(found(&m, &new, &HashSet::new()), vec![vec![(0, 1)]]);
        // Kept both before: not flagged again.
        assert!(found(&m, &new, &pairs(&[("0:1", "1:0")])).is_empty());
    }

    /// How long the UI thread's part (`gather`) and the check's thread
    /// (`match_items`) take over 10,000 pictures on 77 slides.
    #[test]
    #[ignore = "timing; run with --ignored --nocapture"]
    fn timings_over_ten_thousand_pictures() {
        let prints: Vec<Print> = (0..8).map(|s| print(&png(64, 48, s)).unwrap()).collect();
        let slides: Vec<Canvas> = (0..77).map(|_| slide((0..130).map(|_| picture(&Arc::new(vec![0u8; 8]))).collect())).collect();
        let m = CanvasManager::for_tests(slides);
        let keys: Vec<Arc<Vec<u8>>> = (0..10).map(|i| match &m.slide(76).unwrap().layers[0].shapes[i] {
            Shape::Raster { png, .. } => Arc::clone(png),
            _ => unreachable!(),
        }).collect();
        let new: Vec<Spot> = keys.iter().enumerate().map(|(i, k)| Spot::of(76, 0, i, k)).collect();
        let fake = |png: &Arc<Vec<u8>>| Some(prints[(Arc::as_ptr(png) as usize >> 4) % prints.len()]);
        let t = std::time::Instant::now();
        let (items, idx) = gather(&m, &new, fake);
        let ui = t.elapsed();
        let t = std::time::Instant::now();
        let pairs = match_items(&items, &idx);
        let thread = t.elapsed();
        println!("{} pictures, {} new: gather (UI thread) {:.2} ms, match_items (own thread) {:.2} ms, {} with copies",
            items.len(), idx.len(), ui.as_secs_f64() * 1e3, thread.as_secs_f64() * 1e3, pairs.len());
    }

    #[test]
    fn keeping_one_pair_apart_leaves_the_other_copies() {
        let a = png(400, 300, 1);
        let (again, third) = (png(200, 150, 1), png(300, 225, 1));
        let m = CanvasManager::for_tests(vec![slide(vec![picture(&a), picture(&third)]), slide(vec![picture(&again)])]);
        let new = [Spot::of(1, 0, 0, &again)];
        assert_eq!(found(&m, &new, &HashSet::new()), vec![vec![(0, 0), (0, 1)]]);
        assert_eq!(found(&m, &new, &pairs(&[("0:0", "1:0")])), vec![vec![(0, 1)]]);
    }

    #[test]
    fn another_picture_an_in_app_copy_a_flat_fill_or_a_video_is_no_duplicate() {
        let (a, b) = (png(400, 300, 1), png(400, 300, 2));
        let (fill, fill_again) = (flat(300, 200), flat(150, 100));
        let mut video = picture(&png(400, 300, 1));
        if let Shape::Raster { media, .. } = &mut video {
            *media = Some(Box::new(MediaRef { file: "v.mp4".into(), kind: MediaKind::Video, source_name: String::new(), meta: None, id: crate::canvas::shape::fresh_media_id() }));
        }
        // Slide 0: picture a, its in-app copy (same bytes), a grey fill and a
        // video showing a's frame. Slide 1, just put in: b and another fill.
        let m = CanvasManager::for_tests(vec![slide(vec![picture(&a), picture(&a), picture(&fill), video]), slide(vec![picture(&b), picture(&fill_again)])]);
        assert!(found(&m, &[Spot::of(1, 0, 0, &b), Spot::of(1, 0, 1, &fill_again)], &HashSet::new()).is_empty());
        // a's copy made in the app shares its bytes: not a picture brought in again.
        assert!(found(&m, &[Spot::of(0, 0, 1, &a)], &HashSet::new()).is_empty());
    }

    #[test]
    fn a_picture_is_followed_when_it_moves_and_its_bytes_are_stored() {
        let (a, b) = (png(40, 30, 1), png(40, 30, 2));
        let mut m = CanvasManager::for_tests(vec![slide(vec![picture(&a), picture(&b)])]);
        let mut s = Spot::of(0, 0, 1, &b);
        // Something before it goes: found one place down.
        m.active_mut().layers[0].shapes.remove(0);
        assert!(s.locate(&m).is_some());
        assert_eq!(s.shape, 0);
        // The store swaps its bytes for a preview: followed.
        let preview = Arc::new(vec![9u8; 4]);
        let file = PictureRef { file: "0123456789abcdef.png".into(), size: [40, 30], crop: None, mirror: false };
        if let Shape::Raster { png, picture, .. } = &mut m.active_mut().layers[0].shapes[0] {
            *png = Arc::clone(&preview);
            *picture = Some(Box::new(file.clone()));
        }
        assert!(s.locate(&m).is_none(), "its old bytes are gone");
        s.follow(&[(Arc::clone(&b), file)], &m);
        assert!(s.locate(&m).is_some());
        // Deleted: gone.
        m.active_mut().layers[0].shapes.clear();
        assert!(s.locate(&m).is_none());
    }
}
