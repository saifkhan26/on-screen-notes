//! Finding pictures like one pasted, dropped or selected (`tools::similar`):
//! where the picture comes from, and the index and results kept up while
//! find has one.

use super::*;
use crate::canvas::shape::{RasterKind, Shape};
use crate::tools::similar::{self, Origin};
use std::sync::mpsc::{self, TryRecvError};
use std::collections::HashMap;
use std::sync::Arc;
use ui::find_bar::PictureQuery;

/// Finding the pictures that show the words typed in find.
pub(super) struct WordSearch {
    words: String,
    /// When they were typed: they are read once typing pauses.
    since: Instant,
    asked: Option<u64>,
    vector: Option<Arc<[f32]>>,
    /// Their vector could not be read.
    failed: bool,
}

/// A tag as first spelled, and its pictures' vectors.
type TagPictures = (String, Vec<Arc<[f32]>>);

/// Most tags the tag row offers as likely for a picture.
const LIKELY_TAGS: usize = 3;
/// One picture in this many goes into the sample a tag's typical
/// closeness is taken from, for the tag row.
const SAMPLE_EVERY: usize = 16;

/// Typing must pause this long before the words are read.
const WORD_PAUSE: std::time::Duration = std::time::Duration::from_millis(350);

/// A dropped file being read for find, off the UI thread.
pub(super) struct FindReading {
    name: String,
    rx: mpsc::Receiver<Option<image::RgbaImage>>,
}

/// Every picture on the slides being gathered into sets of copies, off
/// the UI thread (`similar::copies`).
pub(super) struct CopiesJob {
    rx: mpsc::Receiver<Vec<Vec<usize>>>,
    /// `(slide, layer, shape, bytes)` of each picture the job was given.
    places: Vec<(usize, usize, usize, Arc<Vec<u8>>)>,
    /// What the job was started from (see `FindBar::update_pictures`).
    key: (u64, usize),
}

impl OnScreenNotesApp {
    /// Find pictures like the selected picture, else like the picture on
    /// the clipboard; with neither, open find and say how to give one.
    pub(super) fn find_similar(&mut self) {
        self.commands.close();
        if let Some(at) = self.selected_picture() {
            self.find_like_shape(at);
        } else if !self.find_like_clipboard() {
            self.find.open();
            self.toast = Some(("Select, paste or drop a picture to find ones like it".to_string(), Instant::now()));
        }
    }

    /// Find pictures like the one at `(slide, layer, shape)`, from the
    /// bytes its slide keeps, as every other picture is read.
    fn find_like_shape(&mut self, at: (usize, usize, usize)) {
        let Some(c) = self.canvases.slide(at.0) else { return };
        let Some(Shape::Raster { png, picture, .. }) = c.layers.get(at.1).and_then(|l| l.shapes.get(at.2)) else { return };
        let origin = Origin { at: Some(at), file: picture.as_deref().cloned() };
        let query = image::load_from_memory(png).ok().and_then(|img| PictureQuery::new(&img.to_rgba8(), "the selected picture", origin));
        match query {
            Some(q) => self.find.find_picture(q),
            None => self.toast = Some(("Couldn't read this picture".to_string(), Instant::now())),
        }
    }

    /// Find pictures like the one on the clipboard, if there is one.
    fn find_like_clipboard(&mut self) -> bool {
        let Ok(img) = arboard::Clipboard::new().and_then(|mut cb| cb.get_image()) else { return false };
        let Some(rgba) = image::RgbaImage::from_raw(img.width as u32, img.height as u32, img.bytes.into_owned()) else { return false };
        match PictureQuery::new(&rgba, "the pasted picture", Origin::default()) {
            Some(q) => {
                self.find.find_picture(q);
                true
            }
            None => false,
        }
    }

    /// While find is open, Ctrl+V with a picture on the clipboard looks
    /// for pictures like it instead of pasting text into the field.
    /// Ctrl+V with only a picture there makes no egui event at all, so
    /// the chord itself is watched, as `run_keys` does.
    pub(super) fn take_find_paste(&mut self, ctx: &egui::Context) {
        let chord = crate::platform::paste_chord_down() && ctx.input(|i| i.focused);
        let pressed = chord && !self.find_paste_prev;
        self.find_paste_prev = chord;
        // Elsewhere the paste event is the only cue.
        let event = !cfg!(target_os = "windows") && ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Paste(_))));
        if (pressed || event) && self.find_like_clipboard() {
            ctx.input_mut(|i| i.events.retain(|e| !matches!(e, egui::Event::Paste(_))));
        }
    }

    /// Find pictures like the one in a file dropped while find is open.
    /// Only pictures and GIFs (their first frame) can be read.
    pub(super) fn find_like_file(&mut self, path: std::path::PathBuf) {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if !crate::media::import::is_image(&path) && crate::media::import::media_kind(&path) != Some(crate::canvas::shape::MediaKind::Gif) {
            self.toast = Some((format!("Find looks for pictures like a picture or GIF — {name} is neither"), Instant::now()));
            return;
        }
        let (tx, rx) = mpsc::channel();
        let spawned = std::thread::Builder::new().name("osn-find-read".into()).spawn(move || {
            let _ = tx.send(image::open(&path).ok().map(|i| i.to_rgba8()));
        });
        match spawned {
            Ok(_) => self.find_reading = Some(FindReading { name, rx }),
            Err(e) => log::warn!("could not read {name} for find: {e}"),
        }
    }

    /// Take in a dropped file once read.
    pub(super) fn poll_find_reading(&mut self, ctx: &egui::Context) {
        let Some(r) = &self.find_reading else { return };
        match r.rx.try_recv() {
            Ok(img) => {
                let name = r.name.clone();
                self.find_reading = None;
                match img.and_then(|img| PictureQuery::new(&img, &name, Origin::default())) {
                    Some(q) => self.find.find_picture(q),
                    None => self.toast = Some((format!("Couldn't read {name}"), Instant::now())),
                }
            }
            Err(TryRecvError::Disconnected) => self.find_reading = None,
            Err(TryRecvError::Empty) => ctx.request_repaint_after(std::time::Duration::from_millis(30)),
        }
    }

    /// While find has a picture: read the slides' pictures (the shown
    /// slide's first) and bring the matches up to date. Returns how many
    /// are read, of how many.
    pub(super) fn update_picture_results(&mut self, ctx: &egui::Context) -> Option<(usize, usize)> {
        if !self.find.has_picture() {
            return None;
        }
        let canvases = &self.canvases;
        let active = canvases.active_index();
        let order = std::iter::once(active).chain((0..canvases.count()).filter(|&i| i != active));
        let slides: Vec<&crate::canvas::canvas::Canvas> = order.filter_map(|i| canvases.slide(i)).collect();
        self.picture_index.tick(slides.iter().copied(), !self.find.has_picture_results());
        let read_slides = slides.len();
        let has_vector = self.query_vector();
        let canvases = &self.canvases;
        let (index, vectors) = (&self.picture_index, &self.picture_vectors);
        // Fresh results when a signature or vector comes in, or the
        // query's own vector does.
        let key = (index.generation() ^ vectors.generation().rotate_left(32) ^ (u64::from(has_vector) << 63), read_slides);
        self.find.update_pictures(key, |q| {
            ui::find_bar::picture_search(q, (0..canvases.count()).filter_map(|i| canvases.slide(i).map(|c| (i, c))), |png| index.get(png), |png| vectors.vector(png))
        });
        let (read, total) = index.progress();
        let (vread, vtotal) = vectors.progress();
        if read < total || (vectors.is_on() && !vectors.settled()) {
            ctx.request_repaint_after(std::time::Duration::from_millis(150));
        }
        // Say how far reading is: the vectors once the signatures are read.
        Some(if read < total || !vectors.is_on() { (read, total) } else { (vread, vtotal) })
    }

    /// Get the vector of find's picture, while picture search is on: the
    /// picture's own if it is on a slide and read, else read now, ahead
    /// of the pictures waiting. Whether it is known.
    fn query_vector(&mut self) -> bool {
        let Some(q) = self.find.picture_mut() else { return false };
        if q.has_vector() {
            return true;
        }
        if !self.picture_vectors.is_on() {
            return false;
        }
        if let Some(id) = q.asked() {
            match self.picture_vectors.answer(id) {
                None => {}
                Some(Some(v)) => q.set_vector(v),
                Some(None) => q.give_up_vector(),
            }
            return q.has_vector();
        }
        let own = q.origin().at.and_then(|(s, l, sh)| match self.canvases.slide(s)?.layers.get(l)?.shapes.get(sh)? {
            Shape::Raster { png, .. } => self.picture_vectors.vector(png),
            _ => None,
        });
        if let Some(v) = own {
            q.set_vector(v);
            return true;
        }
        if let Some(img) = q.probe() {
            let id = self.picture_vectors.ask(img);
            q.set_asked(id);
        }
        false
    }

    /// While find has words typed and picture search is on: read the
    /// words' vector once typing pauses, and list the pictures that show
    /// them after the label and tag matches.
    pub(super) fn update_word_pictures(&mut self, ctx: &egui::Context) {
        let words = match self.find.words_query() {
            Some(w) if self.picture_vectors.is_on() => w.to_string(),
            _ => {
                self.word_search = None;
                self.find.set_words_note(None);
                return;
            }
        };
        if self.word_search.as_ref().is_none_or(|w| w.words != words) {
            self.word_search = Some(WordSearch { words: words.clone(), since: Instant::now(), asked: None, vector: None, failed: false });
        }
        let vectors = &mut self.picture_vectors;
        let w = self.word_search.as_mut().expect("set above");
        if w.vector.is_none() && !w.failed {
            match w.asked {
                None if w.since.elapsed() >= WORD_PAUSE => w.asked = vectors.ask_words(&words),
                None => ctx.request_repaint_after(WORD_PAUSE),
                Some(id) => match vectors.answer(id) {
                    None => {}
                    Some(Some(v)) => w.vector = Some(v),
                    Some(None) => w.failed = true,
                },
            }
        }
        let note = vectors.words_status().or_else(|| (w.vector.is_none() && !w.failed).then(|| "looking at pictures…".to_string()));
        self.find.set_words_note(note);
        let Some(v) = w.vector.clone() else {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            return;
        };
        let canvases = &self.canvases;
        let read = (0..canvases.count()).filter(|&i| canvases.slide(i).is_some()).count();
        let key = (vectors.generation(), read);
        if self.find.word_pictures_for(&words) != Some(key) {
            let found = ui::find_bar::word_picture_search(&v, (0..canvases.count()).filter_map(|i| canvases.slide(i).map(|c| (i, c))), |png| vectors.vector(png));
            self.find.set_word_pictures(&words, key, found);
        }
    }

    /// While find has a single `#tag` typed and picture search is on:
    /// list the untagged pictures that look like the ones tagged so
    /// (`tools::tag_suggest`), leaving out those turned down for it and
    /// those on locked slides.
    pub(super) fn update_tag_suggestions(&mut self) {
        let Some(tag) = self.find.tag_query() else { return };
        if !self.picture_vectors.is_on() {
            return;
        }
        let rejections = self.tag_rejections.get_or_insert_with(crate::persistence::tag_rejections::Rejections::load);
        let (canvases, vectors) = (&self.canvases, &self.picture_vectors);
        let slides = || (0..canvases.count()).filter_map(|i| canvases.slide(i).map(|c| (i, c)));
        let pool = ui::find_bar::TagPool::gather(
            &tag,
            slides(),
            |i| canvases.is_locked(i),
            |png| vectors.vector(png),
            |png| vectors.key(png).is_some_and(|k| rejections.contains(&tag, &k)),
        );
        let (tagged, turned_down) = pool.counts();
        let key = (vectors.generation() ^ ((tagged as u64) << 40) ^ ((turned_down as u64) << 20), slides().count());
        if self.find.tag_suggestions_for(&tag) == Some(key) {
            return;
        }
        let hits = pool.suggest(slides());
        let (read, total) = vectors.progress();
        let note = if read < total {
            Some(format!("reading pictures, {read} of {total}"))
        } else if hits.is_empty() && tagged > 0 {
            Some(format!("no untagged picture looks like your #{tag} ones"))
        } else {
            None
        };
        self.find.set_words_note(note);
        self.find.set_tag_suggestions(&tag, key, hits);
    }

    /// While the tag row is open on one picture and picture search is on:
    /// offer the tags it probably has (`tools::tag_suggest::fits`), worked
    /// out once for that picture.
    pub(super) fn fill_likely_tags(&mut self) {
        let Some(at) = self.selected_picture().filter(|_| self.tag_edit.is_some() && self.picture_vectors.is_on()) else {
            self.likely_for = None;
            return;
        };
        let png = match self.canvases.slide(at.0).and_then(|c| c.layers.get(at.1)?.shapes.get(at.2)) {
            Some(Shape::Raster { png, .. }) => Arc::clone(png),
            _ => return,
        };
        let vectors = &self.picture_vectors;
        let Some(v) = vectors.vector(&png) else { return };
        let key = (at, vectors.generation());
        if self.likely_for == Some(key) {
            return;
        }
        let own: Vec<String> = self.canvases.slide(at.0).and_then(|c| c.layers.get(at.1)?.shapes.get(at.2)).map(|s| s.tags().to_vec()).unwrap_or_default();
        // Every tag's pictures, by tag (case aside), and an even sample
        // of all pictures, to tell how close a typical one is.
        let mut by_tag: HashMap<String, TagPictures> = HashMap::new();
        let mut sample: Vec<Arc<[f32]>> = Vec::new();
        let canvases = &self.canvases;
        let mut n = 0usize;
        for c in (0..canvases.count()).filter_map(|i| canvases.slide(i)) {
            for s in c.layers.iter().flat_map(|l| &l.shapes) {
                let Shape::Raster { png: p, kind: RasterKind::Image, .. } = s else { continue };
                if Arc::ptr_eq(p, &png) {
                    continue;
                }
                let Some(pv) = vectors.vector(p) else { continue };
                n += 1;
                if n.is_multiple_of(SAMPLE_EVERY) {
                    sample.push(Arc::clone(&pv));
                }
                if s.tags().is_empty() {
                    continue;
                }
                for t in s.tags() {
                    by_tag.entry(crate::canvas::units::tag_key(t)).or_insert_with(|| (t.clone(), Vec::new())).1.push(Arc::clone(&pv));
                }
            }
        }
        let sample: Vec<&[f32]> = sample.iter().map(|x| &x[..]).collect();
        let mut likely: Vec<(String, f32)> = by_tag
            .into_values()
            .filter(|(t, _)| !own.iter().any(|o| crate::canvas::units::same_tag(o, t)))
            .filter_map(|(t, vs)| {
                let refs: Vec<&[f32]> = vs.iter().map(|x| &x[..]).collect();
                crate::tools::tag_suggest::fits(&refs, &v, &sample).map(|s| (t, s.close))
            })
            .collect();
        likely.sort_by(|a, b| b.1.total_cmp(&a.1));
        likely.truncate(LIKELY_TAGS);
        if let Some(e) = self.tag_edit.as_mut() {
            e.likely = likely;
        }
        self.likely_for = Some(key);
    }

    /// Delete one copy from the duplicates list, wherever it is.
    pub(super) fn delete_copy(&mut self, slide: usize, layer: usize, shape: usize, png: &Arc<Vec<u8>>) {
        if self.canvases.is_locked(slide) {
            self.toast = Some((format!("Slide {} is locked", slide + 1), Instant::now()));
            return;
        }
        if !self.canvases.delete_picture(slide, layer, shape, png) {
            self.toast = Some(("That copy has moved since — find the duplicates again".to_string(), Instant::now()));
            return;
        }
        // A selection on that slide may name shapes that moved down one.
        if slide == self.canvases.active_index() {
            self.tool.select.sel = crate::canvas::hit::Selection::default();
            self.tool.select.rebind(slide, self.canvases.active());
        }
        self.find.forget_copy(slide, layer, shape);
        self.toast = Some((format!("Deleted from slide {} — Ctrl+Z there brings it back", slide + 1), Instant::now()));
    }

    /// A suggestion taken: tag the picture.
    pub(super) fn tag_suggested(&mut self, slide: usize, layer: usize, shape: usize, tag: &str) {
        let msg = if self.canvases.add_tag(slide, layer, shape, tag) {
            format!("Tagged #{tag}")
        } else if self.canvases.is_locked(slide) {
            "That slide is locked".to_string()
        } else {
            "That picture has moved since — find it again".to_string()
        };
        self.toast = Some((msg, Instant::now()));
    }

    /// A suggestion turned down: never suggest that picture for `tag`.
    pub(super) fn reject_suggested(&mut self, slide: usize, layer: usize, shape: usize, tag: &str) {
        let png = match self.canvases.slide(slide).and_then(|c| c.layers.get(layer)?.shapes.get(shape)) {
            Some(Shape::Raster { png, .. }) => Arc::clone(png),
            _ => return,
        };
        let Some(key) = self.picture_vectors.key(&png) else { return };
        self.tag_rejections.get_or_insert_with(crate::persistence::tag_rejections::Rejections::load).add(tag, &key);
        self.toast = Some((format!("It won't be suggested for #{tag} again"), Instant::now()));
    }

    /// Keep the picture vectors up to date while picture search is on (in
    /// the background, the shown slide first), and stop them once off.
    pub(super) fn tick_picture_vectors(&mut self, ctx: &egui::Context) {
        let on = self.config.picture_search;
        if !on && !self.picture_vectors.is_on() {
            return;
        }
        let canvases = &self.canvases;
        let active = canvases.active_index();
        let order = std::iter::once(active).chain((0..canvases.count()).filter(|&i| i != active));
        let slides: Vec<&crate::canvas::canvas::Canvas> = order.filter_map(|i| canvases.slide(i)).collect();
        // Reading waits while the pen or mouse is down, so drawing stays smooth.
        let drawing = self.pen.is_down() || ctx.input(|i| i.pointer.any_down());
        self.picture_vectors.tick(slides, on, drawing);
        if self.picture_vectors.busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }

    /// While find lists every set of copies: read every slide's pictures,
    /// then, once all slides are open and every picture read, gather them
    /// into sets off the UI thread, again whenever pictures come or go.
    /// Returns how many pictures are read, of how many.
    pub(super) fn update_copies(&mut self, ctx: &egui::Context) -> Option<(usize, usize)> {
        if !self.find.has_copies() {
            self.copies_job = None;
            return None;
        }
        let canvases = &self.canvases;
        let active = canvases.active_index();
        let order = std::iter::once(active).chain((0..canvases.count()).filter(|&i| i != active));
        let slides: Vec<&crate::canvas::canvas::Canvas> = order.filter_map(|i| canvases.slide(i)).collect();
        let unread = canvases.count() - slides.len();
        let first_look = self.find.copies_found_for().is_none() && self.copies_job.is_none();
        self.picture_index.tick(slides.iter().copied(), first_look);
        let index = &self.picture_index;
        let key = (index.generation(), slides.len());
        let (read, total) = index.progress();

        let done = self.copies_job.as_ref().map(|j| j.rx.try_recv());
        match done {
            Some(Ok(sets)) => {
                let job = self.copies_job.take().expect("a job is running");
                let copy = similar::Likeness { kind: similar::Kind::Duplicate, score: 1.0, way: Default::default(), other_crop: false };
                let sets: Vec<Vec<ui::find_bar::PictureHit>> = sets
                    .into_iter()
                    .map(|set| {
                        set.into_iter()
                            .filter_map(|i| {
                                let (s, l, sh, png) = &job.places[i];
                                ui::find_bar::picture_hit(canvases.slide(*s)?, *s, *l, *sh, copy, Arc::clone(png))
                            })
                            .collect::<Vec<_>>()
                    })
                    .filter(|set| set.len() > 1)
                    .collect();
                self.find.set_copies(sets, job.key);
            }
            Some(Err(TryRecvError::Disconnected)) => self.copies_job = None,
            Some(Err(TryRecvError::Empty)) | None => {}
        }

        let settled = read == total && unread == 0;
        if settled && self.copies_job.is_none() && self.find.copies_found_for() != Some(key) {
            // In slide order, so each set lists its copies that way.
            let mut items = Vec::new();
            let mut places = Vec::new();
            for si in 0..canvases.count() {
                let Some(c) = canvases.slide(si) else { continue };
                for (li, l) in c.layers.iter().enumerate() {
                    for (shi, s) in l.shapes.iter().enumerate() {
                        let Shape::Raster { png, kind: RasterKind::Image, picture, media, .. } = s else { continue };
                        let mut keys = vec![format!("bytes:{}", Arc::as_ptr(png) as usize)];
                        if let Some(p) = picture {
                            keys.push(format!("file:{}|{:?}|{}", p.file, p.crop, p.mirror));
                        }
                        if let Some(m) = media {
                            keys.push(format!("media:{}", m.file));
                        }
                        items.push(similar::Item { print: index.print(png), keys });
                        places.push((si, li, shi, Arc::clone(png)));
                    }
                }
            }
            let (tx, rx) = mpsc::channel();
            let spawned = std::thread::Builder::new().name("osn-copies".into()).spawn(move || {
                let t = Instant::now();
                let sets = similar::copies(&items);
                log::info!("{} pictures gathered into {} sets of copies in {} ms", items.len(), sets.len(), t.elapsed().as_millis());
                let _ = tx.send(sets);
            });
            match spawned {
                Ok(_) => self.copies_job = Some(CopiesJob { rx, places, key }),
                Err(e) => log::warn!("could not start looking for copies: {e}"),
            }
        }
        if !settled || self.copies_job.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        Some((read, total))
    }

    /// Go to `slide` and select these pictures on `layer` (a set's extra
    /// copies there), and close find so Delete reaches the canvas.
    pub(super) fn select_extras(&mut self, slide: usize, layer: usize, shapes: Vec<usize>, ctx: &egui::Context) {
        let Some(c) = self.canvases.slide(slide) else { return };
        let still = c.layers.get(layer).is_some_and(|l| shapes.iter().all(|&i| matches!(l.shapes.get(i), Some(Shape::Raster { kind: RasterKind::Image, .. }))));
        if !still || shapes.is_empty() {
            self.toast = Some(("Those copies have moved since — find them again".to_string(), Instant::now()));
            return;
        }
        if slide != self.canvases.active_index() {
            self.canvases.go_to(slide);
        }
        self.canvases.active_mut().active_layer = layer;
        let n = shapes.len();
        self.tool.kind = crate::tools::ToolKind::Select;
        self.tool.select.gesture = None;
        self.tool.select.sel = crate::canvas::hit::Selection { strokes: Vec::new(), shapes };
        self.tool.select.rebind(self.canvases.active_index(), self.canvases.active());
        self.find.close();
        self.zoom_to_selection(ctx);
        let what = if n == 1 { "1 extra copy" } else { "extra copies" };
        let msg = if n == 1 { format!("{what} selected — Delete removes it") } else { format!("{n} {what} selected — Delete removes them") };
        self.toast = Some((msg, Instant::now()));
    }
}
