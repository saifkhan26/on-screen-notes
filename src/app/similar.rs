//! Finding pictures like one pasted, dropped or selected (`tools::similar`):
//! where the picture comes from, and the index and results kept up while
//! find has one.

use super::*;
use crate::canvas::shape::{RasterKind, Shape};
use crate::tools::similar::{self, Origin};
use std::sync::mpsc::{self, TryRecvError};
use std::sync::Arc;
use ui::find_bar::PictureQuery;

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
        let index = &self.picture_index;
        let key = (index.generation(), slides.len());
        self.find.update_pictures(key, |q| {
            ui::find_bar::picture_search(q, (0..canvases.count()).filter_map(|i| canvases.slide(i).map(|c| (i, c))), |png| index.get(png))
        });
        let (read, total) = index.progress();
        if read < total {
            ctx.request_repaint_after(std::time::Duration::from_millis(150));
        }
        Some((read, total))
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
