//! Finding pictures like one pasted, dropped or selected (`tools::similar`):
//! where the picture comes from, and the index and results kept up while
//! find has one.

use super::*;
use crate::canvas::shape::{PictureRef, RasterKind, Shape};
use crate::persistence::copy_ignores::{CopyIgnores, Ignore};
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

/// A tag as first spelled, its pictures' vectors, and what they are
/// (`TagBars::print`, being worked out).
type TagPictures = (String, Vec<Arc<[f32]>>, std::hash::DefaultHasher);

/// Most tags the tag row offers as likely for a picture.
const LIKELY_TAGS: usize = 3;
/// One picture in this many goes into the sample a tag's typical
/// closeness is taken from, for the tag row.
const SAMPLE_EVERY: usize = 16;

/// Each tag's bars, for the likely tags the tag row offers
/// (`tag_suggest::tag_bars`): the slow part, and the same for every
/// picture, so kept from one picture to the next and worked out off the UI
/// thread. Bars out of date still serve until new ones come.
#[derive(Default)]
pub(super) struct TagBars {
    /// By tag (`units::tag_key`).
    known: HashMap<String, BarsFor>,
    /// Tags' bars being worked out.
    job: Option<mpsc::Receiver<Vec<(String, BarsFor)>>>,
    /// Counts the jobs done: the likely tags are worked out again after
    /// each.
    generation: u64,
}

/// A tag's bars (`None`: too few pictures to learn from), and what they
/// were worked out from: the tag's pictures (`TagBars::print`) and how
/// big the sample was.
#[derive(Debug, Clone, PartialEq)]
struct BarsFor {
    print: u64,
    sample: usize,
    bars: Option<crate::tools::tag_suggest::Bars>,
}

/// A tag whose bars are to be worked out: its key, what its pictures are
/// (`TagBars::print`) and their vectors.
type BarsTodo = (String, u64, Vec<Arc<[f32]>>);

impl TagBars {
    /// Add one of a tag's pictures, known by `key` (`PictureVectors::key`),
    /// to what its pictures are: bars are worked out again once it changes.
    fn print(h: &mut std::hash::DefaultHasher, key: &str) {
        use std::hash::Hash;
        key.hash(h);
    }

    /// The tag's bars, if any are known, and whether they are up to date
    /// for its pictures (`print`) and a sample of `sample`: they go out of
    /// date when the sample has grown or shrunk by more than a quarter.
    fn get(&self, tag: &str, print: u64, sample: usize) -> (Option<&crate::tools::tag_suggest::Bars>, bool) {
        match self.known.get(tag) {
            Some(b) => (b.bars.as_ref(), b.print == print && b.sample.abs_diff(sample) * 4 <= b.sample),
            None => (None, false),
        }
    }

    /// Keep the bars a finished job worked out. True when there were some.
    fn take_done(&mut self) -> bool {
        let Some(rx) = &self.job else { return false };
        match rx.try_recv() {
            Ok(done) => {
                self.known.extend(done);
                self.job = None;
                self.generation += 1;
                true
            }
            Err(TryRecvError::Disconnected) => {
                self.job = None;
                false
            }
            Err(TryRecvError::Empty) => false,
        }
    }

    /// Work out the bars of the tags in `todo` against `sample`, off the UI
    /// thread, unless a job is under way. Repaints once it is done.
    fn start(&mut self, mut todo: Vec<BarsTodo>, sample: Vec<Arc<[f32]>>, ctx: &egui::Context) {
        if self.job.is_some() || todo.is_empty() {
            return;
        }
        // Biggest first, dealt out in turn, so each thread gets as much.
        todo.sort_by(|a, b| b.2.len().cmp(&a.2.len()));
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get().saturating_sub(1)).clamp(1, 8);
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new().name("osn-tag-bars".into()).spawn(move || {
            let t = Instant::now();
            let sample: Vec<&[f32]> = sample.iter().map(|x| &x[..]).collect();
            let work = |first: usize| -> Vec<(String, BarsFor)> {
                todo.iter()
                    .skip(first)
                    .step_by(threads)
                    .map(|(tag, print, vs)| {
                        let refs: Vec<&[f32]> = vs.iter().map(|x| &x[..]).collect();
                        let bars = crate::tools::tag_suggest::tag_bars(&refs, &sample);
                        (tag.clone(), BarsFor { print: *print, sample: sample.len(), bars })
                    })
                    .collect()
            };
            let done: Vec<(String, BarsFor)> = std::thread::scope(|s| {
                let work = &work;
                let parts: Vec<_> = (1..threads).map(|i| s.spawn(move || work(i))).collect();
                let mut done = work(0);
                for p in parts {
                    done.extend(p.join().unwrap_or_default());
                }
                done
            });
            log::info!("bars of {} tags against {} pictures in {} ms", done.len(), sample.len(), t.elapsed().as_millis());
            let _ = tx.send(done);
            // The main window: the slide window is drawn in its frame.
            ctx.request_repaint_of(egui::ViewportId::ROOT);
        });
        match spawned {
            Ok(_) => self.job = Some(rx),
            Err(e) => log::warn!("could not start working out the tags' bars: {e}"),
        }
    }
}

/// What a stored picture is known by in the duplicates list, here and in
/// what was ignored of it (`persistence::copy_ignores`): its file and how
/// it is shown. A video or GIF is known by its file (`media:…`), any
/// other picture by its bytes (`bytes_id`).
fn picture_id(p: &PictureRef) -> String {
    format!("file:{}|{:?}|{}", p.file, p.crop, p.mirror)
}

/// What a picture kept in its slide is known by: its bytes' hash, which
/// stays the same from run to run.
fn bytes_id(png: &[u8]) -> String {
    format!("bytes:{:016x}:{}", crate::canvas::board_link::Fnv::of(png), png.len())
}

/// Typing must pause this long before the words are read.
const WORD_PAUSE: std::time::Duration = std::time::Duration::from_millis(350);

/// A dropped file being read for find, off the UI thread.
pub(super) struct FindReading {
    name: String,
    rx: mpsc::Receiver<Option<image::RgbaImage>>,
}

/// What gathering copies comes back with: the sets, and each picture's
/// key (`picture_id`).
type CopiesFound = (Vec<Vec<usize>>, Vec<Arc<str>>);

/// Every picture on the slides being gathered into sets of copies, off
/// the UI thread (`similar::copies_apart`).
pub(super) struct CopiesJob {
    rx: mpsc::Receiver<CopiesFound>,
    /// `(slide, layer, shape, bytes)` of each picture the job was given.
    places: Vec<(usize, usize, usize, Arc<Vec<u8>>)>,
    /// What the job was started from (see `FindBar::update_pictures`).
    key: (u64, usize),
}

impl OnScreenNotesApp {
    /// Find pictures like the selected picture, else like the picture on
    /// the clipboard; with neither, open find and say how to give one.
    pub(super) fn find_similar(&mut self) {
        self.ws.commands.close();
        if let Some(at) = self.selected_picture() {
            self.find_like_shape(at);
        } else if !self.find_like_clipboard() {
            self.ws.find.open();
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
            Some(q) => self.ws.find.find_picture(q),
            None => self.toast = Some(("Couldn't read this picture".to_string(), Instant::now())),
        }
    }

    /// Find pictures like the one on the clipboard, if there is one.
    fn find_like_clipboard(&mut self) -> bool {
        let Ok(img) = arboard::Clipboard::new().and_then(|mut cb| cb.get_image()) else { return false };
        let Some(rgba) = image::RgbaImage::from_raw(img.width as u32, img.height as u32, img.bytes.into_owned()) else { return false };
        match PictureQuery::new(&rgba, "the pasted picture", Origin::default()) {
            Some(q) => {
                self.ws.find.find_picture(q);
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
        let pressed = chord && !self.ws.find_paste_prev;
        self.ws.find_paste_prev = chord;
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
            Ok(_) => self.ws.find_reading = Some(FindReading { name, rx }),
            Err(e) => log::warn!("could not read {name} for find: {e}"),
        }
    }

    /// Take in a dropped file once read.
    pub(super) fn poll_find_reading(&mut self, ctx: &egui::Context) {
        let Some(r) = &self.ws.find_reading else { return };
        match r.rx.try_recv() {
            Ok(img) => {
                let name = r.name.clone();
                self.ws.find_reading = None;
                match img.and_then(|img| PictureQuery::new(&img, &name, Origin::default())) {
                    Some(q) => self.ws.find.find_picture(q),
                    None => self.toast = Some((format!("Couldn't read {name}"), Instant::now())),
                }
            }
            Err(TryRecvError::Disconnected) => self.ws.find_reading = None,
            Err(TryRecvError::Empty) => ctx.request_repaint_after(std::time::Duration::from_millis(30)),
        }
    }

    /// While find has a picture: read the slides' pictures (the shown
    /// slide's first) and bring the matches up to date. Returns how many
    /// are read, of how many.
    pub(super) fn update_picture_results(&mut self, ctx: &egui::Context) -> Option<(usize, usize)> {
        if !self.ws.find.has_picture() {
            return None;
        }
        let canvases = &self.canvases;
        let active = canvases.active_index();
        let order = std::iter::once(active).chain((0..canvases.count()).filter(|&i| i != active));
        let slides: Vec<&crate::canvas::canvas::Canvas> = order.filter_map(|i| canvases.slide(i)).collect();
        self.picture_index.tick(slides.iter().copied(), !self.ws.find.has_picture_results());
        let read_slides = slides.len();
        let has_vector = self.query_vector();
        let canvases = &self.canvases;
        let (index, vectors) = (&self.picture_index, &self.picture_vectors);
        // Fresh results when a signature or vector comes in, or the
        // query's own vector does.
        let key = (index.generation() ^ vectors.generation().rotate_left(32) ^ (u64::from(has_vector) << 63), read_slides);
        self.ws.find.update_pictures(key, |q| {
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
        let Some(q) = self.ws.find.picture_mut() else { return false };
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
        let words = match self.ws.find.words_query() {
            Some(w) if self.picture_vectors.is_on() => w.to_string(),
            _ => {
                self.ws.word_search = None;
                self.ws.find.set_words_note(None);
                return;
            }
        };
        if self.ws.word_search.as_ref().is_none_or(|w| w.words != words) {
            self.ws.word_search = Some(WordSearch { words: words.clone(), since: Instant::now(), asked: None, vector: None, failed: false });
        }
        let vectors = &mut self.picture_vectors;
        let w = self.ws.word_search.as_mut().expect("set above");
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
        self.ws.find.set_words_note(note);
        let Some(v) = w.vector.clone() else {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            return;
        };
        let canvases = &self.canvases;
        let read = (0..canvases.count()).filter(|&i| canvases.slide(i).is_some()).count();
        let key = (vectors.generation(), read);
        if self.ws.find.word_pictures_for(&words) != Some(key) {
            let found = ui::find_bar::word_picture_search(&v, (0..canvases.count()).filter_map(|i| canvases.slide(i).map(|c| (i, c))), |png| vectors.vector(png));
            self.ws.find.set_word_pictures(&words, key, found);
        }
    }

    /// While find has a single `#tag` typed and picture search is on:
    /// list the untagged pictures that look like the ones tagged so
    /// (`tools::tag_suggest`), leaving out those turned down for it and
    /// those on locked slides.
    pub(super) fn update_tag_suggestions(&mut self) {
        let Some(tag) = self.ws.find.tag_query() else { return };
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
        if self.ws.find.tag_suggestions_for(&tag) == Some(key) {
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
        self.ws.find.set_words_note(note);
        self.ws.find.set_tag_suggestions(&tag, key, hits);
    }

    /// While the tag row is open on one picture and picture search is on:
    /// offer the tags it probably has (`tools::tag_suggest::fits_bars`),
    /// worked out once for that picture, and again as the tags' bars come
    /// (`TagBars`).
    pub(super) fn fill_likely_tags(&mut self, ctx: &egui::Context) {
        self.tag_bars.take_done();
        let Some(at) = self.selected_picture().filter(|_| self.ws.tag_edit.is_some() && self.picture_vectors.is_on()) else {
            self.ws.likely_for = None;
            return;
        };
        let png = match self.canvases.slide(at.0).and_then(|c| c.layers.get(at.1)?.shapes.get(at.2)) {
            Some(Shape::Raster { png, .. }) => Arc::clone(png),
            _ => return,
        };
        let vectors = &self.picture_vectors;
        let Some(v) = vectors.vector(&png) else { return };
        let key = (at, vectors.generation(), self.tag_bars.generation);
        if self.ws.likely_for == Some(key) {
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
                let picture = vectors.key(p);
                for t in s.tags() {
                    let e = by_tag.entry(crate::canvas::units::tag_key(t)).or_insert_with(|| (t.clone(), Vec::new(), Default::default()));
                    e.1.push(Arc::clone(&pv));
                    TagBars::print(&mut e.2, picture.as_deref().unwrap_or_default());
                }
            }
        }
        // Each tag with bars known is checked now; those with none, or
        // out of date, are worked out (off the UI thread) to check again.
        let mut likely: Vec<(String, f32)> = Vec::new();
        let mut todo: Vec<BarsTodo> = Vec::new();
        for (k, (t, vs, print)) in by_tag {
            if own.iter().any(|o| crate::canvas::units::same_tag(o, &t)) {
                continue;
            }
            let print = std::hash::Hasher::finish(&print);
            let (bars, fresh) = self.tag_bars.get(&k, print, sample.len());
            if let Some(b) = bars {
                let refs: Vec<&[f32]> = vs.iter().map(|x| &x[..]).collect();
                if let Some(s) = crate::tools::tag_suggest::fits_bars(b, &refs, &v) {
                    likely.push((t, s.close));
                }
            }
            if !fresh {
                todo.push((k, print, vs));
            }
        }
        self.tag_bars.start(todo, sample, ctx);
        likely.sort_by(|a, b| b.1.total_cmp(&a.1));
        likely.truncate(LIKELY_TAGS);
        if let Some(e) = self.ws.tag_edit.as_mut() {
            e.likely = likely;
        }
        self.ws.likely_for = Some(key);
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
            self.ws.tool.select.sel = crate::canvas::hit::Selection::default();
            self.ws.tool.select.rebind(slide, self.canvases.active());
        }
        self.ws.find.forget_copy(slide, layer, shape);
        self.toast = Some((format!("Deleted from slide {} — Ctrl+Z there brings it back", slide + 1), Instant::now()));
    }

    /// Something in the duplicates list ignored: list it no more, from
    /// now on. The list is gathered again (`update_copies`).
    pub(super) fn ignore_copies(&mut self, ignore: Ignore) {
        let what = match &ignore {
            Ignore::Set(_) => "Set ignored",
            Ignore::Picture { .. } => "Taken out of its set",
        };
        self.copy_ignores.get_or_insert_with(CopyIgnores::load).add(ignore);
        self.toast = Some((format!("{what} — Ignored, at the top of the list, brings it back"), Instant::now()));
    }

    /// Something ignored in the duplicates list, listed again.
    pub(super) fn show_copies_again(&mut self, ignore: Ignore) {
        self.copy_ignores.get_or_insert_with(CopyIgnores::load).remove(&ignore);
        self.toast = Some(("Listed as copies again".to_string(), Instant::now()));
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
        if !self.ws.find.has_copies() {
            self.ws.copies_job = None;
            return None;
        }
        let canvases = &self.canvases;
        let active = canvases.active_index();
        let order = std::iter::once(active).chain((0..canvases.count()).filter(|&i| i != active));
        let slides: Vec<&crate::canvas::canvas::Canvas> = order.filter_map(|i| canvases.slide(i)).collect();
        let unread = canvases.count() - slides.len();
        let first_look = self.ws.find.copies_found_for().is_none() && self.ws.copies_job.is_none();
        self.picture_index.tick(slides.iter().copied(), first_look);
        let ignores: &CopyIgnores = self.copy_ignores.get_or_insert_with(CopyIgnores::load);
        let index = &self.picture_index;
        // Gathered again when pictures are read, or something is ignored
        // or shown again.
        let key = (index.generation() ^ ignores.generation().rotate_left(40), slides.len());
        let (read, total) = index.progress();

        let done = self.ws.copies_job.as_ref().map(|j| j.rx.try_recv());
        match done {
            Some(Ok((sets, ids))) => {
                let job = self.ws.copies_job.take().expect("a job is running");
                let copy = similar::Likeness { kind: similar::Kind::Duplicate, score: 1.0, way: Default::default(), other_crop: false };
                let hit = |i: usize| {
                    let (s, l, sh, png) = &job.places[i];
                    ui::find_bar::picture_hit(canvases.slide(*s)?, *s, *l, *sh, copy, Arc::clone(png))
                };
                let sets: Vec<Vec<(ui::find_bar::PictureHit, Arc<str>)>> = sets
                    .into_iter()
                    .map(|set| set.into_iter().filter_map(|i| Some((hit(i)?, Arc::clone(&ids[i])))).collect::<Vec<_>>())
                    .filter(|set| set.len() > 1)
                    .collect();
                // What was ignored, newest first, with its pictures here.
                let mut at: HashMap<&str, Vec<usize>> = HashMap::new();
                for (i, id) in ids.iter().enumerate() {
                    at.entry(&**id).or_default().push(i);
                }
                let places_of = |keys: &[String]| -> Vec<usize> {
                    let mut v: Vec<usize> = keys.iter().flat_map(|k| at.get(k.as_str()).into_iter().flatten().copied()).collect();
                    v.sort_unstable();
                    v.dedup();
                    v
                };
                let ignored: Vec<ui::find_bar::IgnoredCopies> = ignores
                    .list()
                    .iter()
                    .rev()
                    .map(|ignore| {
                        let (own, of) = match ignore {
                            Ignore::Set(keys) => (Vec::new(), places_of(keys)),
                            Ignore::Picture { picture, of } => {
                                let own = places_of(std::slice::from_ref(picture));
                                let of = places_of(of).into_iter().filter(|i| !own.contains(i)).collect();
                                (own, of)
                            }
                        };
                        let mut rows: Vec<ui::find_bar::PictureHit> = own.into_iter().filter_map(hit).collect();
                        let lead = rows.len();
                        rows.extend(of.into_iter().filter_map(hit));
                        ui::find_bar::IgnoredCopies { ignore: ignore.clone(), rows, lead }
                    })
                    .collect();
                self.ws.find.set_copies(sets, ignored, job.key);
            }
            Some(Err(TryRecvError::Disconnected)) => self.ws.copies_job = None,
            Some(Err(TryRecvError::Empty)) | None => {}
        }

        let settled = read == total && unread == 0;
        if settled && self.ws.copies_job.is_none() && self.ws.find.copies_found_for() != Some(key) {
            // In slide order, so each set lists its copies that way.
            let mut items = Vec::new();
            let mut places = Vec::new();
            // Each picture's key where it is known without its bytes.
            let mut known: Vec<Option<String>> = Vec::new();
            for si in 0..canvases.count() {
                let Some(c) = canvases.slide(si) else { continue };
                for (li, l) in c.layers.iter().enumerate() {
                    for (shi, s) in l.shapes.iter().enumerate() {
                        let Shape::Raster { png, kind: RasterKind::Image, picture, media, .. } = s else { continue };
                        let mut keys = vec![format!("bytes:{}", Arc::as_ptr(png) as usize)];
                        if let Some(p) = picture {
                            keys.push(picture_id(p));
                        }
                        if let Some(m) = media {
                            keys.push(format!("media:{}", m.file));
                        }
                        known.push(match (picture, media) {
                            (Some(p), _) => Some(picture_id(p)),
                            (None, Some(m)) => Some(format!("media:{}", m.file)),
                            (None, None) => None,
                        });
                        items.push(similar::Item { print: index.print(png), keys });
                        places.push((si, li, shi, Arc::clone(png)));
                    }
                }
            }
            let pngs: Vec<Arc<Vec<u8>>> = places.iter().map(|p| Arc::clone(&p.3)).collect();
            let apart = ignores.pairs().clone();
            let (tx, rx) = mpsc::channel();
            let spawned = std::thread::Builder::new().name("osn-copies".into()).spawn(move || {
                let t = Instant::now();
                let ids: Vec<Arc<str>> = known.into_iter().zip(&pngs).map(|(k, png)| k.unwrap_or_else(|| bytes_id(png)).into()).collect();
                let sets = {
                    // Each key as a number, and the pairs kept apart among
                    // the pictures here.
                    let mut no: HashMap<&str, u32> = HashMap::new();
                    let nos: Vec<u32> = ids
                        .iter()
                        .map(|id| {
                            let n = no.len() as u32;
                            *no.entry(&**id).or_insert(n)
                        })
                        .collect();
                    let pairs: std::collections::HashSet<(u32, u32)> = apart
                        .iter()
                        .filter_map(|(a, b)| {
                            let (a, b) = (*no.get(a.as_str())?, *no.get(b.as_str())?);
                            Some((a.min(b), a.max(b)))
                        })
                        .collect();
                    similar::copies_apart(&items, |i, j| !pairs.is_empty() && pairs.contains(&(nos[i].min(nos[j]), nos[i].max(nos[j]))))
                };
                log::info!("{} pictures gathered into {} sets of copies in {} ms", items.len(), sets.len(), t.elapsed().as_millis());
                let _ = tx.send((sets, ids));
            });
            match spawned {
                Ok(_) => self.ws.copies_job = Some(CopiesJob { rx, places, key }),
                Err(e) => log::warn!("could not start looking for copies: {e}"),
            }
        }
        if !settled || self.ws.copies_job.is_some() {
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
        self.ws.tool.kind = crate::tools::ToolKind::Select;
        self.ws.tool.select.gesture = None;
        self.ws.tool.select.sel = crate::canvas::hit::Selection { strokes: Vec::new(), shapes };
        self.ws.tool.select.rebind(self.canvases.active_index(), self.canvases.active());
        self.ws.find.close();
        self.zoom_to_selection(ctx);
        let what = if n == 1 { "1 extra copy" } else { "extra copies" };
        let msg = if n == 1 { format!("{what} selected — Delete removes it") } else { format!("{n} {what} selected — Delete removes them") };
        self.toast = Some((msg, Instant::now()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(axis: usize) -> Arc<[f32]> {
        let mut v = vec![0.0f32; 8];
        v[axis] = 1.0;
        v.into()
    }

    /// Wait for the job under way to finish.
    fn finish(b: &mut TagBars) {
        let t = Instant::now();
        while !b.take_done() {
            assert!(b.job.is_some(), "the job went without an answer");
            assert!(t.elapsed().as_secs() < 10, "the job never finished");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn tag_bars_are_worked_out_once_and_kept_until_out_of_date() {
        let ctx = egui::Context::default();
        let mut b = TagBars::default();
        let pictures: Vec<Arc<[f32]>> = (0..3).map(unit).collect();
        let sample: Vec<Arc<[f32]>> = (3..8).map(unit).collect();
        let few: Vec<Arc<[f32]>> = vec![unit(0)];
        assert_eq!(b.get("cat", 1, 5), (None, false));
        b.start(vec![("cat".into(), 1, pictures.clone()), ("dog".into(), 2, few)], sample.clone(), &ctx);
        assert!(b.job.is_some());
        // One job at a time: more waits for it.
        b.start(vec![("owl".into(), 3, pictures.clone())], sample.clone(), &ctx);
        finish(&mut b);
        assert_eq!(b.generation, 1);
        assert!(!b.known.contains_key("owl"));
        let refs: Vec<&[f32]> = pictures.iter().map(|x| &x[..]).collect();
        let s: Vec<&[f32]> = sample.iter().map(|x| &x[..]).collect();
        let want = crate::tools::tag_suggest::tag_bars(&refs, &s).expect("three pictures to learn from");
        assert_eq!(b.get("cat", 1, 5), (Some(&want), true));
        // Too few pictures: known to have none.
        assert_eq!(b.get("dog", 2, 5), (None, true));
        // Other pictures, or a sample grown by more than a quarter: out of
        // date, but still there until new ones come.
        assert_eq!(b.get("cat", 9, 5), (Some(&want), false));
        assert_eq!(b.get("cat", 1, 6), (Some(&want), true));
        assert_eq!(b.get("cat", 1, 7), (Some(&want), false));
        assert_eq!(b.get("cat", 1, 3), (Some(&want), false));
        assert!(!b.take_done(), "nothing more under way");
    }

    #[test]
    fn a_tags_pictures_print_follows_which_pictures_they_are() {
        let print = |keys: &[&str]| {
            let mut h = std::hash::DefaultHasher::default();
            keys.iter().for_each(|k| TagBars::print(&mut h, k));
            std::hash::Hasher::finish(&h)
        };
        assert_eq!(print(&["a", "b"]), print(&["a", "b"]));
        assert_ne!(print(&["a", "b"]), print(&["a"]));
        assert_ne!(print(&["a", "b"]), print(&["a", "c"]));
        assert_ne!(print(&["ab"]), print(&["a", "b"]));
    }
}
