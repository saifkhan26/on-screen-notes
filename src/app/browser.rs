//! The browser extension's side of the app (`browser_link`): the server
//! on or off as the settings say, what it sends inserted as a dropped
//! file is, the "may it connect?" question, and the tags offered for
//! pictures because of who posted them (`arrange_bar::FromPost`) —
//! offered only, never put on by themselves.

use super::*;
use crate::browser_link::{Event, Extra, Link, Source};
use crate::tools::similar::Print;
use crate::persistence::artist_tags::ArtistTags;
use crate::persistence::sources::Sources;
use std::collections::HashSet;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Kept bytes keys before the cache starts over.
const MAX_KEYS: usize = 4096;
/// Slides offered to the extension to send to: the open one, then the
/// most recently visited.
const OFFERED_SLIDES: usize = 10;
/// The slides' prints, for "already on a slide?", are taken again after
/// this long (pictures come and go).
const PRINTS_KEPT: std::time::Duration = std::time::Duration::from_secs(15);
/// A file for a slide that is still being read waits this long for it,
/// then goes on the open slide.
const SLIDE_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

#[derive(Default)]
pub(super) struct BrowserState {
    link: Option<Link>,
    /// The port it was started on; `None` while it is off.
    running: Option<u16>,
    /// Files sent from the browser, being imported: where each came from,
    /// and what the user asked for with it.
    sent: HashMap<PathBuf, (Source, Extra)>,
    /// Imported files waiting for their slide to be read, since when.
    waiting: Vec<(PathBuf, crate::media::import::Imported, String, Instant)>,
    /// "Already on a slide?" questions: the picture, where the answer
    /// goes, and when it was asked.
    checks: Vec<(Print, std::sync::mpsc::Sender<Vec<(usize, String)>>, Instant)>,
    /// Every picture's print on the slides, where it is, and when taken.
    prints: Option<(Instant, Arc<Vec<PrintSpot>>)>,
    /// Which tags name an artist; read once needed.
    artist_tags: Option<ArtistTags>,
    /// The same, shared with the find bar's tag list; made again on change.
    artist_set: Option<Arc<HashSet<String>>>,
    /// An extension asking to connect: what it calls itself, and where
    /// the answer goes.
    asking: Option<(String, std::sync::mpsc::Sender<bool>)>,
    /// Where sent pictures came from; read once needed.
    sources: Option<Sources>,
    /// `browser_link::bytes_key` of pictures kept in their slides, by
    /// their bytes' address and length.
    keys: HashMap<(usize, usize), String>,
    /// The offer last worked out, and a stamp of what it came from.
    offer: Option<(u64, Vec<ui::arrange_bar::FromPost>)>,
    /// "Install yt-dlp?" is showing (a stream video was sent without it).
    offer_ytdlp: bool,
    /// The user said not now: not asked again this run.
    ytdlp_declined: bool,
    /// `winget install yt-dlp.yt-dlp`, while it runs.
    ytdlp_install: Option<crate::media::ytdlp::Installer>,
}

/// A picture on the slides and its print (`Print`), for finding copies.
#[derive(Clone, Copy)]
struct PrintSpot {
    slide: usize,
    layer: usize,
    shape: usize,
    print: Print,
}

impl BrowserState {
    /// The artists' tags, read once needed.
    fn artists(&mut self) -> &mut ArtistTags {
        self.artist_tags.get_or_insert_with(ArtistTags::load)
    }

    /// Something about artists changed: the find bar's copy is made again.
    fn artists_changed(&mut self) {
        self.artist_set = None;
    }

    /// The artists' tags for the find bar's switches, once known.
    pub(super) fn artist_set(&mut self) -> Option<Arc<HashSet<String>>> {
        if self.artist_set.is_none() {
            let a = self.artists();
            if !a.is_seeded() {
                return None;
            }
            self.artist_set = Some(Arc::new(a.artists().clone()));
        }
        self.artist_set.clone()
    }

    /// The settings' line about it; empty while it is off.
    pub(super) fn status(&self) -> String {
        self.link.as_ref().map(Link::status).unwrap_or_default()
    }

    pub(super) fn stop(&mut self) {
        self.link = None;
        self.running = None;
    }

    /// Settings' "Disconnect all".
    pub(super) fn forget_browsers(&self) {
        if let Some(link) = &self.link {
            link.forget_browsers();
        }
    }

    /// Whether `path` is a file sent from the browser being imported.
    pub(super) fn sent(&self, path: &Path) -> bool {
        self.sent.contains_key(path)
    }
}

/// What `shape` is known by in the sources (`browser_link::shape_key`),
/// the bytes of a picture kept in its slide hashed only once.
fn key_of(shape: &Shape, cache: &mut HashMap<(usize, usize), String>) -> Option<String> {
    match shape {
        Shape::Raster { media: None, picture: None, png, .. } => {
            if cache.len() > MAX_KEYS {
                cache.clear();
            }
            let k = (Arc::as_ptr(png) as usize, png.len());
            Some(cache.entry(k).or_insert_with(|| crate::browser_link::bytes_key(png)).clone())
        }
        s => crate::browser_link::shape_key(s),
    }
}

impl OnScreenNotesApp {
    /// Once a frame: start or stop the server as the settings say, take
    /// in what it heard, and ask about an extension waiting to connect.
    pub(super) fn poll_browser_link(&mut self, ctx: &egui::Context, canvas_rect: egui::Rect) {
        let want = self.config.browser_link.then_some(self.config.browser_link_port);
        if self.browser.running != want {
            self.browser.link = None;
            if let Some(port) = want {
                match crate::persistence::paths::data_dir() {
                    Ok(dir) => {
                        let tokens = crate::browser_link::tokens::Tokens::load();
                        self.browser.link = Some(Link::start(port, ctx.clone(), dir.join("incoming"), tokens));
                    }
                    Err(e) => log::warn!("browser link: no data folder: {e}"),
                }
            }
            self.browser.running = want;
        }
        let Some(link) = &self.browser.link else { return };
        let events: Vec<Event> = std::iter::from_fn(|| link.try_event()).collect();
        for e in events {
            match e {
                Event::Media { path, source, extra } => {
                    self.browser.sent.insert(path.clone(), (source, extra));
                    self.start_media_import(path, None);
                }
                Event::Check { print, reply } => self.browser.checks.push((print, reply, Instant::now())),
                Event::Find(path) => {
                    crate::platform::reclaim_foreground(self.osn_hwnd);
                    self.ws.commands.close();
                    self.find_like_file(path);
                    self.toast = Some(("Looking for pictures like it on your slides…".to_string(), Instant::now()));
                }
                Event::Pair { client, reply } => {
                    if self.browser.asking.is_some() {
                        let _ = reply.send(false);
                    } else {
                        self.browser.asking = Some((client, reply));
                        // The question is for now: the browser has the
                        // keyboard, and the user is looking at it.
                        crate::platform::reclaim_foreground(self.osn_hwnd);
                    }
                }
                Event::Note(text) => self.toast = Some((text, Instant::now())),
                Event::Artist { slide, key, source } => self.tag_found_artist(slide, &key, source),
                Event::NeedYtdlp => {
                    if !self.browser.ytdlp_declined && self.browser.ytdlp_install.is_none() {
                        self.browser.offer_ytdlp = true;
                    }
                }
            }
        }
        self.offer_slides();
        self.answer_checks();
        self.seed_artists();
        let waiting = std::mem::take(&mut self.browser.waiting);
        for (path, imported, name, since) in waiting {
            self.insert_sent(&path, imported, &name, false, canvas_rect, ctx.pixels_per_point(), Some(since));
        }
        self.ask_to_connect(ctx);
        self.offer_ytdlp(ctx);
        self.show_finding(ctx, canvas_rect);
    }

    /// Tell the server which slides to offer the extension.
    fn offer_slides(&self) {
        let Some(link) = &self.browser.link else { return };
        let c = &self.canvases;
        let active = c.active_index();
        // Then the newest slides, so a fresh run offers more than the
        // open one.
        let mut order: Vec<usize> = Vec::with_capacity(OFFERED_SLIDES);
        for i in std::iter::once(active).chain(c.recent().iter().copied()).chain((0..c.count()).rev()) {
            if i < c.count() && !order.contains(&i) {
                order.push(i);
            }
            if order.len() == OFFERED_SLIDES {
                break;
            }
        }
        let slides: Vec<(usize, String)> = order.into_iter().map(|i| (i, c.name(i).to_string())).collect();
        link.set_slides(active, &slides);
    }

    /// Every picture's print on the slides (as `PictureIndex` has them),
    /// with where it is, once all slides are read and every picture on
    /// them; until then `None`, tried again next frame. `fresh`: not the
    /// list kept a while (the places in it may have moved).
    fn slide_prints(&mut self, fresh: bool) -> Option<Arc<Vec<PrintSpot>>> {
        if let Some((at, list)) = &self.browser.prints {
            if !fresh && at.elapsed() < PRINTS_KEPT {
                return Some(Arc::clone(list));
            }
        }
        let canvases = &self.canvases;
        let slides: Vec<&crate::canvas::canvas::Canvas> = (0..canvases.count()).filter_map(|i| canvases.slide(i)).collect();
        if slides.len() < canvases.count() {
            return None;
        }
        let index = &mut self.picture_index;
        let first = index.progress().1 == 0;
        index.tick(slides.iter().copied(), first);
        let (read, total) = index.progress();
        if read < total {
            return None;
        }
        let mut list = Vec::new();
        for (si, c) in slides.iter().enumerate() {
            for (li, l) in c.layers.iter().enumerate() {
                for (shi, s) in l.shapes.iter().enumerate() {
                    let Shape::Raster { png, kind: crate::canvas::shape::RasterKind::Image, media, .. } = s else { continue };
                    if media.as_ref().is_some_and(|m| m.kind == crate::canvas::shape::MediaKind::Video) {
                        continue;
                    }
                    if let Some(print) = index.print(png) {
                        list.push(PrintSpot { slide: si, layer: li, shape: shi, print });
                    }
                }
            }
        }
        let list = Arc::new(list);
        self.browser.prints = Some((Instant::now(), Arc::clone(&list)));
        Some(list)
    }

    /// Answer "already on a slide?": the slides holding a copy of each
    /// picture asked about (at most five), once the slides' prints are in.
    fn answer_checks(&mut self) {
        if self.browser.checks.is_empty() {
            return;
        }
        let Some(list) = self.slide_prints(false) else {
            // The server stops waiting after a while: so does this.
            self.browser.checks.retain(|c| c.2.elapsed() < std::time::Duration::from_secs(25));
            return;
        };
        for (print, reply, _) in std::mem::take(&mut self.browser.checks) {
            let mut slides: Vec<usize> = list.iter().filter(|p| print.duplicate_of(&p.print)).map(|p| p.slide).collect();
            slides.sort_unstable();
            slides.dedup();
            let named = slides.into_iter().take(5).map(|s| (s, self.canvases.name(s).to_string())).collect();
            let _ = reply.send(named);
        }
    }

    /// Where a file sent with `extra` goes: the slide asked for (found by
    /// its name if slides moved since), else the open one.
    fn target_slide(&self, extra: &Extra) -> usize {
        let c = &self.canvases;
        let active = c.active_index();
        let Some((i, name)) = &extra.slide else { return active };
        if *i < c.count() && (name.is_empty() || c.name(*i) == name) {
            return *i;
        }
        (0..c.count()).find(|&k| !name.is_empty() && c.name(k) == name).unwrap_or(active)
    }

    /// Insert a file sent from the browser once imported: on the slide it
    /// was sent to (the open one unless the extension said), with the
    /// tags the user gave it. `since`: it has waited for its slide to be
    /// read since then.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn insert_sent(&mut self, path: &Path, imported: crate::media::import::Imported, name: &str, beside: bool, canvas_rect: egui::Rect, ppp: f32, since: Option<Instant>) {
        let Some((_, extra)) = self.browser.sent.get(path).cloned() else { return };
        let mut slide = self.target_slide(&extra);
        let active = self.canvases.active_index();
        if slide != active && self.canvases.slide(slide).is_none() {
            let since = since.unwrap_or_else(Instant::now);
            if since.elapsed() < SLIDE_WAIT {
                self.canvases.prefetch(slide);
                self.browser.waiting.push((path.to_path_buf(), imported, name.to_string(), since));
                return;
            }
            slide = active;
        }
        if self.canvases.is_locked(slide) {
            self.toast = Some((format!("Not inserted: slide {} is locked — {} unlocks it", slide + 1, self.lock_key()), Instant::now()));
            self.browser_import_done(path, None, name, slide);
            return;
        }
        // Who made it is an artist's tag; what else was given while sending
        // is about what it shows.
        if let Some((source, _)) = self.browser.sent.get(path) {
            let author = source.author.clone();
            for t in &extra.tags {
                if crate::canvas::units::same_tag(t, &author) {
                    self.browser.artists().set(t, true);
                } else {
                    self.browser.artists().note_subject(t);
                }
            }
            if !extra.tags.is_empty() {
                self.browser.artists_changed();
            }
        }
        let key = crate::browser_link::imported_key(&imported);
        if slide == active {
            self.insert_media(imported, None, beside, canvas_rect, ppp, extra.tags.clone());
        } else {
            let (pan, zoom) = self.canvases.slide(slide).map(|c| (c.pan, c.zoom.max(1e-3))).unwrap_or(([0.0, 0.0], 1.0));
            let shape = super::media::media_shape(imported, None, pan, zoom, canvas_rect, ppp, extra.tags.clone());
            self.canvases.edit_slide_in_view(slide, pan, zoom, |c| {
                let li = c.active_layer;
                c.insert_objects(li, Vec::new(), vec![shape]);
            });
        }
        self.browser_import_done(path, Some(key), name, slide);
    }

    /// "Find artist": hand the selected pictures to the browser extension,
    /// which looks each one up (Google Lens) and sends back who made it,
    /// and open the page it takes over in the browser.
    pub(super) fn find_artists(&mut self) {
        let Some(link) = self.browser.link.as_ref().filter(|l| l.is_up()) else {
            self.toast = Some(("Find artist needs the browser extension: turn on Settings → Browser extension → Receive from the browser".to_string(), Instant::now()));
            return;
        };
        let page = link.queue_page();
        let media_dir = crate::persistence::paths::media_dir().ok();
        let slide = self.canvases.active_index();
        let li = self.canvases.active().active_layer;
        let artists = self.browser.artist_tags.get_or_insert_with(ArtistTags::load).artists().clone();
        let is_artist = |t: &String| artists.contains(&crate::canvas::units::tag_key(t));
        // The selected pictures: each once, and whether it has an artist.
        let mut picked: Vec<(usize, String, bool)> = Vec::new();
        {
            let c = self.canvases.active();
            let Some(layer) = c.layers.get(li) else { return };
            for &i in &self.ws.tool.select.sel.shapes {
                let Some(s @ Shape::Raster { kind: crate::canvas::shape::RasterKind::Image, .. }) = layer.shapes.get(i) else { continue };
                let Some(key) = key_of(s, &mut self.browser.keys) else { continue };
                if !picked.iter().any(|(_, k, _)| *k == key) {
                    picked.push((i, key, s.tags().iter().any(is_artist)));
                }
            }
        }
        if picked.is_empty() {
            self.toast = Some(("Select a picture, GIF or video to find its artist".to_string(), Instant::now()));
            return;
        }
        // Every one has an artist: they were picked to be looked up again.
        let all_done = picked.iter().all(|(_, _, done)| *done);
        let skipped = if all_done { 0 } else { picked.iter().filter(|(_, _, d)| *d).count() };
        if !all_done {
            picked.retain(|(_, _, done)| !done);
        }
        // A copy elsewhere has an artist's tag: that tag, without a lookup.
        let mut from_copies = 0;
        if let Some(prints) = self.slide_prints(true) {
            let mut left = Vec::new();
            for (i, key, done) in picked {
                match self.copy_artist(slide, li, i, &prints, &artists) {
                    Some(tag) if !done => {
                        if self.canvases.add_tag(slide, li, i, &tag) {
                            from_copies += 1;
                        }
                    }
                    _ => left.push((i, key, done)),
                }
            }
            picked = left;
        }
        let mut list: Vec<(usize, String, crate::browser_link::queue::Picture)> = Vec::new();
        {
            let c = self.canvases.active();
            let Some(layer) = c.layers.get(li) else { return };
            for (i, key, _) in &picked {
                let Some(Shape::Raster { png, picture, .. }) = layer.shapes.get(*i) else { continue };
                use crate::browser_link::queue::Picture;
                let pic = match (picture, &media_dir) {
                    (Some(p), Some(dir)) => Picture::Stored(dir.join(&p.file), (**p).clone()),
                    // Kept in the slide, or a video's or GIF's poster.
                    _ => Picture::Bytes(Arc::clone(png)),
                };
                list.push((slide, key.clone(), pic));
            }
        }
        let mut notes: Vec<String> = Vec::new();
        if skipped > 0 {
            notes.push(format!("{skipped} with an artist's tag passed over"));
        }
        if from_copies > 0 {
            notes.push(format!("{from_copies} tagged from their copies"));
        }
        let notes = if notes.is_empty() { String::new() } else { format!(" ({})", notes.join(", ")) };
        if list.is_empty() {
            self.toast = Some((format!("Nothing left to look up{notes}"), Instant::now()));
            return;
        }
        let n = list.len();
        if let Some(link) = &self.browser.link {
            link.find_artists(list);
        }
        // The browser opens it as a link opened anywhere would.
        let opened = std::process::Command::new("explorer").arg(&page).spawn();
        let text = match opened {
            Ok(_) if n == 1 => format!("Finding the artist in your browser…{notes}"),
            Ok(_) => format!("Finding the artists of {n} pictures in your browser…{notes}"),
            Err(e) => format!("Couldn't open the browser ({e}) — open {page} in it"),
        };
        self.toast = Some((text, Instant::now()));
    }

    /// The artist's tag a copy of the picture at `slide`, `layer`, `shape`
    /// has (on any slide), if one has.
    fn copy_artist(&self, slide: usize, layer: usize, shape: usize, prints: &[PrintSpot], artists: &HashSet<String>) -> Option<String> {
        let me = prints.iter().find(|p| (p.slide, p.layer, p.shape) == (slide, layer, shape))?;
        prints.iter().filter(|p| (p.slide, p.layer, p.shape) != (slide, layer, shape) && me.print.duplicate_of(&p.print)).find_map(|p| {
            let s = self.canvases.slide(p.slide)?.layers.get(p.layer)?.shapes.get(p.shape)?;
            s.tags().iter().find(|t| artists.contains(&crate::canvas::units::tag_key(t))).cloned()
        })
    }

    /// Tag every copy of the picture at `slide`, `layer`, `shape` (on any
    /// slide) `tag` too, and let "Find artist" pass over those waiting.
    /// How many were tagged.
    fn tag_copies(&mut self, slide: usize, layer: usize, shape: usize, tag: &str) -> usize {
        let Some(prints) = self.slide_prints(true) else { return 0 };
        let Some(me) = prints.iter().find(|p| (p.slide, p.layer, p.shape) == (slide, layer, shape)).copied() else { return 0 };
        let mut tagged = 0;
        for p in prints.iter().filter(|p| p.slide != slide || p.layer != layer || p.shape != shape) {
            if !me.print.duplicate_of(&p.print) {
                continue;
            }
            if self.canvases.add_tag(p.slide, p.layer, p.shape, tag) {
                tagged += 1;
            }
            let key = self.canvases.slide(p.slide).and_then(|c| c.layers.get(p.layer)?.shapes.get(p.shape)).and_then(|s| key_of(s, &mut self.browser.keys));
            if let (Some(link), Some(key)) = (&self.browser.link, key) {
                link.found_already(p.slide, &key, tag);
            }
        }
        tagged
    }

    /// Every tag on the slides is an artist's to start with (they were
    /// nearly all PureRef group names), once every slide is read.
    fn seed_artists(&mut self) {
        if self.browser.artists().is_seeded() {
            return;
        }
        let c = &self.canvases;
        if (0..c.count()).any(|i| c.slide(i).is_none()) {
            return;
        }
        let (index, _) = c.tag_index();
        self.browser.artists().seed(index.iter().map(|t| t.tag.as_str()));
        self.browser.artists_changed();
    }

    /// The tag list's switch: `tag` names an artist, or not.
    pub(super) fn mark_artist_tag(&mut self, tag: &str, artist: bool) {
        self.browser.artists().set(tag, artist);
        self.browser.artists_changed();
        let text = if artist {
            format!("#{tag} is an artist: Find artist passes over pictures tagged so")
        } else {
            format!("#{tag} isn't an artist: Find artist looks pictures tagged so up")
        };
        self.toast = Some((text, Instant::now()));
    }

    /// A tag was renamed everywhere: whether it names an artist carries over.
    pub(super) fn rename_artist_tag(&mut self, from: &str, to: &[String]) {
        self.browser.artists().rename(from, to);
        self.browser.artists_changed();
    }

    /// The extension says who made the picture known by `key` on `slide`:
    /// tag it so (each copy of it there), spelled as a tag the app has
    /// already if one matches. One undo step each.
    fn tag_found_artist(&mut self, slide: usize, key: &str, source: Source) {
        use crate::canvas::units::same_tag;
        let (index, _) = self.canvases.tag_index();
        let tag = index.iter().find(|t| same_tag(&t.tag, &source.author)).map_or_else(|| source.author.clone(), |t| t.tag.clone());
        let mut spots: Vec<(usize, usize)> = Vec::new();
        if let Some(c) = self.canvases.slide(slide) {
            for (li, l) in c.layers.iter().enumerate() {
                for (si, s) in l.shapes.iter().enumerate() {
                    if key_of(s, &mut self.browser.keys).as_deref() == Some(key) {
                        spots.push((li, si));
                    }
                }
            }
        }
        let tagged = spots.iter().filter(|&&(li, si)| self.canvases.add_tag(slide, li, si, &tag)).count();
        self.browser.artists().set(&tag, true);
        self.browser.artists_changed();
        // Its copies on other slides are the same art: same artist.
        let copies = spots.first().map_or(0, |&(li, si)| self.tag_copies(slide, li, si, &tag));
        let also = match copies {
            0 => String::new(),
            1 => " and its copy".to_string(),
            n => format!(" and its {n} copies"),
        };
        let text = if tagged > 0 {
            format!("Tagged #{tag}{also} — the artist, found in the browser (undo takes it off)")
        } else if self.canvases.is_locked(slide) {
            format!("Not tagged #{tag}: slide {} is locked", slide + 1)
        } else if spots.is_empty() {
            format!("Not tagged #{tag}: that picture is gone from its slide")
        } else {
            format!("#{tag} was on it already")
        };
        self.browser.sources.get_or_insert_with(Sources::load).put(key.to_string(), source);
        self.toast = Some((text, Instant::now()));
    }

    /// While pictures wait for the extension: a ring round the one being
    /// looked up (each copy of it on its slide, when that slide is shown),
    /// and a card saying how far it is, where that picture is, with ways to
    /// go to it and to stop.
    fn show_finding(&mut self, ctx: &egui::Context, canvas_rect: egui::Rect) {
        let Some(f) = self.browser.link.as_ref().and_then(|l| l.finding()) else { return };
        let mut spots: Vec<(usize, usize)> = Vec::new();
        if let Some(c) = self.canvases.slide(f.slide) {
            for (li, l) in c.layers.iter().enumerate() {
                for (si, s) in l.shapes.iter().enumerate() {
                    if key_of(s, &mut self.browser.keys).as_deref() == Some(f.key.as_str()) {
                        spots.push((li, si));
                    }
                }
            }
        }
        let active = self.canvases.active_index();
        if f.slide == active && !self.canvases.in_view() {
            let c = self.canvases.active();
            let to_screen = |p: [f32; 2]| egui::pos2(canvas_rect.min.x + c.pan[0] + p[0] * c.zoom, canvas_rect.min.y + c.pan[1] + p[1] * c.zoom);
            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("osn_finding_ring")));
            const PAD: f32 = 7.0;
            for &(li, si) in &spots {
                let Some(shape) = c.layers.get(li).filter(|l| l.visible).and_then(|l| l.shapes.get(si)) else { continue };
                let o = shape.obb();
                let half = [o.half[0] * c.zoom + PAD, o.half[1] * c.zoom + PAD];
                let pts = crate::canvas::render::box_ring(to_screen(o.center), half, o.angle, PAD);
                // A light ring over a dark halo, as find's: it shows on a white page too.
                painter.add(egui::Shape::closed_line(pts.clone(), egui::Stroke::new(6.5, egui::Color32::BLACK.gamma_multiply(0.45))));
                painter.add(egui::Shape::closed_line(pts.clone(), egui::Stroke::new(3.0, ui::find_bar::FOUND)));
                // "Looking up" over its top left corner.
                let top = pts.iter().copied().fold(egui::pos2(f32::MAX, f32::MAX), |a, p| egui::pos2(a.x.min(p.x), a.y.min(p.y)));
                let galley = painter.layout_no_wrap("Looking up in the browser".to_string(), egui::FontId::proportional(12.0), egui::Color32::BLACK);
                let tag = egui::Rect::from_min_size(egui::pos2(top.x, top.y - galley.size().y - 8.0), galley.size() + egui::vec2(12.0, 6.0));
                painter.rect_filled(tag, egui::Rounding::same(4.0), ui::find_bar::FOUND);
                painter.galley(tag.min + egui::vec2(6.0, 3.0), galley, egui::Color32::BLACK);
            }
        }
        let mut stop = false;
        let mut show = false;
        let elsewhere = (f.slide != active).then(|| {
            let n = self.canvases.name(f.slide);
            format!(" — it's on slide {}{}", f.slide + 1, if n.is_empty() { String::new() } else { format!(" · {n}") })
        });
        egui::Area::new(egui::Id::new("osn_finding_artists"))
            .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -112.0))
            .show(ctx, |ui| {
                ui::theme::card_frame().inner_margin(egui::Margin::symmetric(14.0, 8.0)).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let at = (f.answered + 1).min(f.total);
                        ui.label(format!("Finding artists in your browser — {at} of {}{}", f.total, elsewhere.clone().unwrap_or_default()));
                        if !spots.is_empty() && ui.button("Show it").on_hover_text("Go to the picture being looked up").clicked() {
                            show = true;
                        }
                        if ui.button("Stop").clicked() {
                            stop = true;
                        }
                    });
                });
            });
        if show {
            if let Some(&(li, si)) = spots.first() {
                self.go_to_lookup(f.slide, li, si, ctx);
            }
        }
        if stop {
            if let Some(link) = &self.browser.link {
                link.stop_finding();
            }
            self.toast = Some(("Stopped finding artists".to_string(), Instant::now()));
        }
    }

    /// Bring the picture at `slide`, `layer`, `shape` into view, as find
    /// goes to what it found.
    fn go_to_lookup(&mut self, slide: usize, layer: usize, shape: usize, ctx: &egui::Context) {
        let Some(c) = self.canvases.slide(slide) else { return };
        let Some(Shape::Raster { png, .. }) = c.layers.get(layer).and_then(|l| l.shapes.get(shape)) else { return };
        let png = Arc::clone(png);
        let same = crate::tools::similar::Likeness { kind: crate::tools::similar::Kind::Duplicate, score: 1.0, way: Default::default(), other_crop: false };
        let Some(hit) = ui::find_bar::picture_hit(c, slide, layer, shape, same, png) else { return };
        self.go_to_find_hit(&hit.hit, ctx);
    }

    /// "Download stream videos with yt-dlp?" after one was sent without
    /// it (the extension recorded it as it played instead), and the
    /// install's outcome once winget is done.
    fn offer_ytdlp(&mut self, ctx: &egui::Context) {
        if let Some(r) = self.browser.ytdlp_install.as_ref().and_then(|i| i.take_result()) {
            self.browser.ytdlp_install = None;
            let text = match r {
                Ok(true) => "yt-dlp is installed — stream videos now come in whole, from their posts".to_string(),
                Ok(false) => "yt-dlp still can't be found — was the install finished?".to_string(),
                Err(e) => format!("Couldn't install yt-dlp: {e}"),
            };
            self.toast = Some((text, Instant::now()));
        }
        if self.browser.ytdlp_install.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }
        if !self.browser.offer_ytdlp {
            return;
        }
        let mut answer: Option<bool> = None;
        ctx.input_mut(|i| {
            if i.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
                answer = Some(false);
            }
        });
        egui::Window::new("Download stream videos whole?")
            .id(egui::Id::new("osn_ytdlp_offer"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.set_max_width(380.0);
                ui.label("Instagram, X and most video players stream their videos, so the extension can only record them as they play.");
                ui.label("yt-dlp, a free downloader, fetches the whole video from its post instead — sharper, and at once. It installs with WinGet, in a window of its own.");
                ui.label(egui::RichText::new("The site's cookies go to yt-dlp for that one download, so posts you can see while logged in work too; they are deleted right after.").small().color(ui::theme::TEXT_MUTED));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let label = egui::RichText::new("Install yt-dlp").color(ui::theme::accent_ink());
                    if ui.add_enabled(crate::media::ffmpeg::can_install(), egui::Button::new(label).fill(ui::theme::accent())).clicked() {
                        answer = Some(true);
                    }
                    if ui.button("Not now").clicked() {
                        answer = Some(false);
                    }
                });
            });
        match answer {
            Some(true) => {
                self.browser.offer_ytdlp = false;
                self.browser.ytdlp_install = Some(crate::media::ytdlp::Installer::start());
                self.toast = Some(("Installing yt-dlp — see the WinGet window".to_string(), Instant::now()));
            }
            Some(false) => {
                self.browser.offer_ytdlp = false;
                self.browser.ytdlp_declined = true;
            }
            None => {}
        }
    }

    /// "Connect the browser extension?" while one is asking.
    fn ask_to_connect(&mut self, ctx: &egui::Context) {
        let Some((client, _)) = &self.browser.asking else { return };
        let client = client.clone();
        let mut decision: Option<bool> = None;
        ctx.input_mut(|i| {
            if i.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
                decision = Some(false);
            } else if i.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
                decision = Some(true);
            }
        });
        egui::Window::new("Connect the browser extension?")
            .id(egui::Id::new("osn_browser_pair"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.set_max_width(360.0);
                ui.label(format!("The On-Screen Notes extension in {client} asks to send pictures, GIFs and videos to your slides."));
                let note = "Say yes only if you just added the extension or pressed Connect in it. It can add pictures to your slides and see their names, and ask whether a picture is on one; it can't change or delete anything.";
                ui.label(egui::RichText::new(note).small().color(ui::theme::TEXT_MUTED));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let label = egui::RichText::new("Connect").color(ui::theme::accent_ink());
                    if ui.add(egui::Button::new(label).fill(ui::theme::accent())).clicked() {
                        decision = Some(true);
                    }
                    if ui.button("Don't connect").clicked() {
                        decision = Some(false);
                    }
                });
            });
        let Some(yes) = decision else { return };
        let Some((_, reply)) = self.browser.asking.take() else { return };
        let text = match (yes, reply.send(yes).is_ok()) {
            (_, false) => "The extension stopped waiting — press Connect in it again".to_string(),
            (true, true) => format!("Connected — {client} can send pictures here now"),
            (false, true) => format!("Not connected — {client} can't send pictures here"),
        };
        self.toast = Some((text, Instant::now()));
    }

    /// An import of a file sent from the browser is over: keep where it
    /// came from for what it became (`key`, `None` when it was not
    /// inserted), and remove the sent file, which was read in or copied
    /// to the media folder by now.
    pub(super) fn browser_import_done(&mut self, path: &Path, key: Option<String>, name: &str, slide: usize) {
        let Some((source, extra)) = self.browser.sent.remove(path) else { return };
        if let Some(key) = key {
            let mut text = format!("Inserted {name}");
            if slide != self.canvases.active_index() {
                let n = self.canvases.name(slide);
                text.push_str(&format!(" on slide {}{}", slide + 1, if n.is_empty() { String::new() } else { format!(" · {n}") }));
            }
            if !extra.tags.is_empty() {
                text.push_str(&format!(", tagged {}", extra.tags.iter().map(|t| format!("#{t}")).collect::<Vec<_>>().join(" ")));
            } else if !source.author.is_empty() {
                let site = if source.site.is_empty() { String::new() } else { format!(" on {}", source.site) };
                text.push_str(&format!(" — posted by @{}{site}", source.author));
            }
            self.toast = Some((text, Instant::now()));
            self.browser.sources.get_or_insert_with(Sources::load).put(key, source);
        }
        // Its folder goes once empty: a download can bring several files.
        if let Some(dir) = path.parent().filter(|d| d.parent().is_some_and(|p| p.ends_with("incoming"))) {
            if let Err(e) = std::fs::remove_file(path) {
                log::warn!("could not remove {}: {e}", path.display());
            }
            let _ = std::fs::remove_dir(dir);
        }
    }

    /// The tags offered for the selection because of who posted its
    /// pictures: one per handle, with how many selected pictures it was
    /// posted with and do not have it, spelled as a tag the app has
    /// already where one matches. Worked out again only when the
    /// selection, its tags or the sources change.
    pub(super) fn post_tag_offers(&mut self) -> Vec<ui::arrange_bar::FromPost> {
        use crate::canvas::units::same_tag;
        let sel = &self.ws.tool.select.sel;
        if sel.shapes.is_empty() {
            return Vec::new();
        }
        let BrowserState { sources, keys, offer, .. } = &mut self.browser;
        let sources = sources.get_or_insert_with(Sources::load);
        if sources.is_empty() {
            return Vec::new();
        }
        let c = self.canvases.active();
        let Some(layer) = c.layers.get(c.active_layer) else { return Vec::new() };
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (sources.generation, self.canvases.active_index(), c.active_layer).hash(&mut h);
        for &i in &sel.shapes {
            if let Some(s @ Shape::Raster { png, .. }) = layer.shapes.get(i) {
                (i, Arc::as_ptr(png) as usize, s.tags()).hash(&mut h);
            }
        }
        let stamp = h.finish();
        if let Some((_, o)) = offer.as_ref().filter(|(s, _)| *s == stamp) {
            return o.clone();
        }
        let mut out: Vec<ui::arrange_bar::FromPost> = Vec::new();
        for &i in &sel.shapes {
            let Some(s) = layer.shapes.get(i) else { continue };
            let Some(e) = key_of(s, keys).and_then(|k| sources.get(&k)) else { continue };
            let author = &e.source.author;
            if e.not_artist || author.is_empty() || s.tags().iter().any(|t| same_tag(t, author)) {
                continue;
            }
            match out.iter_mut().find(|f| same_tag(&f.tag, author)) {
                Some(f) => f.count += 1,
                None => out.push(ui::arrange_bar::FromPost { tag: author.clone(), site: e.source.site.clone(), count: 1, new: true }),
            }
        }
        if !out.is_empty() {
            let (index, _) = self.canvases.tag_index();
            for f in &mut out {
                if let Some(t) = index.iter().find(|t| same_tag(&t.tag, &f.tag)) {
                    f.tag = t.tag.clone();
                    f.new = false;
                }
            }
            out.sort_by(|a, b| b.count.cmp(&a.count));
        }
        *offer = Some((stamp, out.clone()));
        out
    }

    /// The selected pictures posted by `tag` (and its offer not turned
    /// down), as `(shape, key)`.
    fn posted_by(&mut self, tag: &str) -> Vec<(usize, String)> {
        use crate::canvas::units::same_tag;
        let BrowserState { sources, keys, .. } = &mut self.browser;
        let Some(sources) = sources.as_ref() else { return Vec::new() };
        let c = self.canvases.active();
        let Some(layer) = c.layers.get(c.active_layer) else { return Vec::new() };
        let mut out: Vec<(usize, String)> = Vec::new();
        for &i in &self.ws.tool.select.sel.shapes {
            let Some(key) = layer.shapes.get(i).and_then(|s| key_of(s, keys)) else { continue };
            if sources.get(&key).is_some_and(|e| !e.not_artist && same_tag(&e.source.author, tag)) {
                out.push((i, key));
            }
        }
        out.sort_by_key(|e| e.0);
        out
    }

    /// Whether the selection holds a picture, GIF or video.
    pub(super) fn selection_has_pictures(&self) -> bool {
        let c = self.canvases.active();
        let Some(layer) = c.layers.get(c.active_layer) else { return false };
        self.ws.tool.select.sel.shapes.iter().any(|&i| matches!(layer.shapes.get(i), Some(Shape::Raster { kind: crate::canvas::shape::RasterKind::Image, .. })))
    }

    /// Put `tag` on the selected pictures posted by it: one undo step.
    pub(super) fn tag_from_post(&mut self, tag: &str) {
        use crate::canvas::units::same_tag;
        if !self.may_edit() {
            return;
        }
        let posted = self.posted_by(tag);
        let c = self.canvases.active_mut();
        let li = c.active_layer;
        let Some(layer) = c.layers.get(li) else { return };
        let mut list: Vec<(usize, Vec<String>)> = Vec::new();
        for (i, _) in &posted {
            let Some(tags) = layer.shapes.get(*i).map(|s| s.tags()) else { continue };
            if !tags.iter().any(|t| same_tag(t, tag)) {
                list.push((*i, tags.iter().cloned().chain([tag.to_string()]).collect()));
            }
        }
        let n = list.len();
        if n > 0 && c.set_tags_each(li, &[], &list) {
            self.browser.artists().set(tag, true);
            self.browser.artists_changed();
            let what = if n == 1 { "1 picture".to_string() } else { format!("{n} pictures") };
            self.toast = Some((format!("Tagged {what} #{tag}"), Instant::now()));
        }
    }

    /// The selected pictures posted by `tag` were not made by them: stop
    /// offering it for them.
    pub(super) fn not_from_post(&mut self, tag: &str) {
        let keys: Vec<String> = self.posted_by(tag).into_iter().map(|(_, k)| k).collect();
        if keys.is_empty() {
            return;
        }
        if let Some(s) = self.browser.sources.as_mut() {
            s.mark_not_artist(&keys);
        }
        let what = if keys.len() == 1 { "this picture" } else { "these pictures" };
        self.toast = Some((format!("#{tag} won't be offered for {what} again"), Instant::now()));
    }
}
