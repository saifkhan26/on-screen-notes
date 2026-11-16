//! Pictures, videos, GIFs and PureRef boards coming in (drop, dialog, paste), their progress,
//! and the media bar's results.

use super::*;

impl OnScreenNotesApp {
    /// The Open dialog for pictures, videos, GIFs and PureRef boards,
    /// unless it is open already.
    pub(super) fn open_media_dialog(&mut self) {
        if self.media_dialog.is_some() { return; }
        let mut exts = vec!["gif"];
        exts.extend_from_slice(crate::media::import::VIDEO_EXTS);
        exts.extend_from_slice(crate::media::import::IMAGE_EXTS);
        exts.push("pur");
        self.media_dialog = Some(ui::media_controls::pick_files(
            self.osn_hwnd,
            "Insert picture, video, GIF or PureRef board",
            &[("Pictures, videos, GIFs and PureRef boards", &exts), ("PureRef boards", &["pur"])],
            true,
        ));
    }

    /// Start bringing in a picture, video or GIF. A video waits for
    /// FFmpeg to be set up first.
    pub(super) fn start_media_import(&mut self, path: std::path::PathBuf, at: Option<egui::Pos2>) {
        use crate::canvas::shape::MediaKind;
        if crate::media::import::media_kind(&path) == Some(MediaKind::Video) && crate::media::ffmpeg::tools().is_none() {
            self.media_waiting.push((path, at));
            self.media_ui.ffmpeg_window = true;
            return;
        }
        let mut job = crate::media::import::ImportJob::start(path, at);
        // Decided now, not when it lands: several files dropped with
        // nothing selected must not line up behind the first one in
        // (which is selected once in). Any box with a size asks.
        job.beside = self.beside_selection([0.0, 0.0, 1.0, 1.0]).is_some();
        self.toast = Some((format!("Inserting {}…", job.file_name()), Instant::now()));
        self.media_imports.push(job);
    }

    /// Where a file was dropped, in screen points. egui's pointer is not
    /// updated during an OS drag, so ask the OS where the cursor is.
    pub(super) fn drop_point(&self, ctx: &egui::Context) -> Option<egui::Pos2> {
        let ppp = ctx.pixels_per_point().max(0.01);
        if let (Some((cx, cy)), Some((left, top, _, _))) =
            (crate::platform::cursor_pos(), crate::platform::client_rect(self.osn_hwnd))
        {
            return Some(egui::pos2((cx - left) as f32 / ppp, (cy - top) as f32 / ppp));
        }
        ctx.input(|i| i.pointer.hover_pos())
    }

    /// Collect files from the Open dialog and from drag-and-drop, and
    /// turn finished imports into objects on the active layer.
    pub(super) fn poll_media_inputs(&mut self, ctx: &egui::Context, canvas_rect: egui::Rect) {
        use std::sync::mpsc::TryRecvError;
        if let Some(rx) = &self.media_dialog {
            match rx.try_recv() {
                Ok(paths) => {
                    self.media_dialog = None;
                    for p in paths {
                        if crate::media::pureref::is_pureref(&p) {
                            self.start_pureref_open(p);
                        } else {
                            self.start_media_import(p, None);
                        }
                    }
                }
                Err(TryRecvError::Disconnected) => self.media_dialog = None,
                Err(TryRecvError::Empty) => {}
            }
        }
        if let Some((slide, rx)) = &self.resync_dialog {
            match rx.try_recv() {
                Ok(paths) => {
                    let slide = *slide;
                    self.resync_dialog = None;
                    if let Some(p) = paths.into_iter().next() {
                        self.resync_from(slide, p);
                    }
                }
                Err(TryRecvError::Disconnected) => self.resync_dialog = None,
                Err(TryRecvError::Empty) => {}
            }
        }

        let (dropped, hovering) = ctx.input(|i| (i.raw.dropped_files.clone(), !i.raw.hovered_files.is_empty()));
        // Dropped on find: look for pictures like it, insert nothing.
        if !dropped.is_empty() && self.find.is_open() {
            if let Some(path) = dropped.into_iter().find_map(|f| f.path) {
                self.find_like_file(path);
            }
        } else if !dropped.is_empty() {
            let at = self.drop_point(ctx);
            for (n, file) in dropped.into_iter().enumerate() {
                let Some(path) = file.path else { continue };
                if crate::media::pureref::is_pureref(&path) {
                    self.start_pureref_open(path);
                    continue;
                }
                if !crate::media::import::is_supported(&path) {
                    self.toast = Some(("Only pictures, videos, GIFs and PureRef boards can be dropped here".into(), Instant::now()));
                    continue;
                }
                // Several files at once fan out instead of stacking.
                let at = at.map(|p| p + egui::vec2(24.0, 24.0) * n as f32);
                self.start_media_import(path, at);
            }
        }
        if hovering {
            egui::Area::new(egui::Id::new("osn_drop_hint"))
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .interactable(false)
                .show(ctx, |ui| {
                    ui::theme::card_frame().inner_margin(egui::Margin::symmetric(16.0, 10.0)).show(ui, |ui| {
                        ui.label(if self.find.is_open() {
                            "Drop a picture to find ones like it"
                        } else {
                            "Drop to insert picture / video / GIF, or open a PureRef board"
                        });
                    });
                });
        }

        let mut finished = Vec::new();
        self.media_imports.retain(|job| match job.take() {
            Some(r) => {
                finished.push((job.path.clone(), job.at, job.beside, job.file_name(), r));
                false
            }
            None => true,
        });
        for (path, at, beside, name, result) in finished {
            use crate::media::import::ImportError;
            match result {
                Ok(_) if self.canvases.is_locked(self.canvases.active_index()) => {
                    self.toast = Some((format!("Not inserted: this slide is locked — {} unlocks it", self.lock_key()), Instant::now()));
                }
                Ok(imported) => {
                    self.insert_media(imported, at, beside, canvas_rect, ctx.pixels_per_point());
                    self.toast = Some((format!("Inserted {name}"), Instant::now()));
                }
                Err(ImportError::NeedsFfmpeg) => {
                    self.media_waiting.push((path, at));
                    self.media_ui.ffmpeg_window = true;
                }
                Err(ImportError::Failed(e)) => {
                    log::warn!("import of {name} failed: {e}");
                    self.toast = Some((format!("Couldn't insert {name}: {e}"), Instant::now()));
                }
            }
        }

        self.show_pureref_progress(ctx);
        let mut boards = Vec::new();
        self.pureref_jobs.retain(|job| match job.take() {
            Some(r) => {
                boards.push((job.path.clone(), job.file_name(), job.resync.clone(), r));
                false
            }
            None => true,
        });
        for (path, name, resync, result) in boards {
            match result {
                Ok(board) if resync.is_some() => {
                    if let Some(target) = resync {
                        self.finish_resync(&target, &path, &name, board);
                    }
                }
                Ok(board) => {
                    let (pictures, notes) = board.counts();
                    let skipped = board.skipped;
                    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                    let canvas = crate::media::pureref::into_canvas(
                        board,
                        [canvas_rect.width(), canvas_rect.height()],
                        ctx.pixels_per_point(),
                        &stem,
                        &path,
                    );
                    let idx = self.canvases.push_canvas(canvas);
                    // The board's file name is the slide's first name.
                    self.canvases.rename(idx, &stem);
                    let count = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
                    let mut msg = format!(
                        "Opened {name} as slide {} — {}, {}",
                        idx + 1,
                        count(pictures, "picture", "pictures"),
                        count(notes, "note", "notes"),
                    );
                    if skipped > 0 {
                        msg.push_str(&format!(", {skipped} skipped"));
                    }
                    self.toast = Some((msg, Instant::now()));
                }
                Err(e) if e == "cancelled" => {
                    let what = if resync.is_some() { "resyncing from" } else { "opening" };
                    self.toast = Some((format!("Stopped {what} {name} — nothing changed"), Instant::now()));
                }
                Err(e) => {
                    log::warn!("opening {name} failed: {e}");
                    let what = if resync.is_some() { "resync from" } else { "open" };
                    self.toast = Some((format!("Couldn't {what} {name}: {e}"), Instant::now()));
                }
            }
        }
    }

    /// Start reading a PureRef board; it becomes a new slide once read.
    /// The board file is only read, never written.
    pub(crate) fn start_pureref_open(&mut self, path: std::path::PathBuf) {
        self.pureref_jobs.push(crate::media::pureref::OpenJob::start(path));
    }

    /// Resync slide `slide` from the PureRef board it was opened from
    /// (`media::pureref_sync`): from the board it remembers, or, for a
    /// slide opened before slides remembered it or a board since moved,
    /// from one picked in a dialog.
    pub(super) fn start_resync(&mut self, slide: usize) {
        if self.canvases.is_locked(slide) {
            self.toast = Some((format!("Not resynced: this slide is locked — {} unlocks it", self.lock_key()), Instant::now()));
            return;
        }
        if self.pureref_jobs.iter().any(|j| j.resync.as_ref().is_some_and(|t| t.slide == slide)) || self.resync_dialog.is_some() {
            self.toast = Some(("Already resyncing this slide".to_string(), Instant::now()));
            return;
        }
        let Some(c) = self.canvases.slide(slide) else {
            self.canvases.prefetch(slide);
            self.toast = Some(("That slide is still opening — try again in a moment".to_string(), Instant::now()));
            return;
        };
        match c.board.as_ref().map(|b| b.path.clone()) {
            Some(p) if p.is_file() => self.resync_from(slide, p),
            known => {
                let title = match &known {
                    Some(p) => format!(
                        "{} is not where it was — pick the board to resync from",
                        p.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned())
                    ),
                    None => "Pick the PureRef board this slide was opened from".to_string(),
                };
                let rx = ui::media_controls::pick_files(self.osn_hwnd, &title, &[("PureRef boards", &["pur"])], false);
                self.resync_dialog = Some((slide, rx));
            }
        }
    }

    /// Start reading `path` to resync slide `slide`. Only the pictures the
    /// slide does not show yet are decoded and stored.
    fn resync_from(&mut self, slide: usize, path: std::path::PathBuf) {
        let Some(c) = self.canvases.slide(slide) else { return };
        let target = crate::media::pureref::ResyncTarget {
            slide,
            name: self.canvases.name(slide).to_string(),
            linked: c.board.as_ref().map(|b| b.path.clone()),
        };
        let have = crate::media::pureref_sync::have(c);
        self.pureref_jobs.push(crate::media::pureref::OpenJob::resync(path, target, have));
    }

    /// Merge a board read for a resync into its slide, as one undo step,
    /// and select what came in new when that slide is shown.
    fn finish_resync(&mut self, target: &crate::media::pureref::ResyncTarget, path: &std::path::Path, name: &str, mut board: crate::media::pureref::Board) {
        use crate::media::pureref_sync::{merge, Merged};
        // What the read stored that the slide may not use. Only files this
        // read created are listed, and the cleanup keeps any in use.
        self.canvases.check_media(std::mem::take(&mut board.created));
        // The slide, wherever it is now: slides can move while a board is
        // read.
        let same = |i: usize, me: &Self| {
            me.canvases.slide(i).is_some_and(|c| c.board.as_ref().map(|b| &b.path) == target.linked.as_ref()) && me.canvases.name(i) == target.name
        };
        let slide = if same(target.slide, self) {
            Some(target.slide)
        } else {
            let found: Vec<usize> = (0..self.canvases.count()).filter(|&i| same(i, self)).collect();
            (found.len() == 1).then(|| found[0])
        };
        let Some(slide) = slide else {
            self.toast = Some(("Not resynced: the slide changed while the board was read — resync it again".to_string(), Instant::now()));
            return;
        };
        if self.canvases.is_locked(slide) {
            self.toast = Some((format!("Not resynced: this slide is locked — {} unlocks it", self.lock_key()), Instant::now()));
            return;
        }
        let Some(c) = self.canvases.slide(slide) else { return };
        let (pan, zoom, layer) = (c.pan, c.zoom, c.active_layer);
        let Merged { layers, link, added, report } = match merge(c, board, path, layer) {
            Ok(m) => m,
            Err(e) => {
                self.toast = Some((e, Instant::now()));
                return;
            }
        };
        let nothing = layers.is_empty();
        self.canvases.edit_slide_in_view(slide, pan, zoom, |c| {
            if nothing {
                // Only what the link records changed (say an item gone from
                // the board whose shape was deleted here): no undo step that
                // seems to do nothing.
                c.board = Some(Box::new(link));
                c.dirty = true;
            } else {
                c.apply_resync(layers, link);
            }
        });
        // What came in new, selected, to look over.
        let active = self.canvases.active_index() == slide;
        let li = self.canvases.active().active_layer;
        let shapes: Vec<usize> = added.iter().filter(|(l, _)| *l == li).map(|&(_, i)| i).collect();
        let selected = active && !shapes.is_empty();
        if selected {
            self.tool.kind = crate::tools::ToolKind::Select;
            self.tool.select.gesture = None;
            self.tool.select.sel = crate::canvas::hit::Selection { strokes: Vec::new(), shapes };
            self.tool.select.rebind(self.canvases.active_index(), self.canvases.active());
        }
        self.toast = Some((resync_message(name, slide, &report, selected), Instant::now()));
    }

    /// A card at the bottom of the screen for each board being opened:
    /// what it is doing, a progress bar, and a button to stop it.
    pub(super) fn show_pureref_progress(&mut self, ctx: &egui::Context) {
        use crate::media::pureref::Stage;
        if self.pureref_jobs.is_empty() {
            return;
        }
        let mut cancel = Vec::new();
        egui::Area::new(egui::Id::new("osn_pureref_progress"))
            .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -72.0))
            .show(ctx, |ui| {
                ui::theme::card_frame().inner_margin(egui::Margin::symmetric(14.0, 10.0)).show(ui, |ui| {
                    for (i, job) in self.pureref_jobs.iter().enumerate() {
                        let (stage, done, total) = job.progress();
                        let fraction = if total > 0 { (done as f32 / total as f32).clamp(0.0, 1.0) } else { 0.0 };
                        let name = job.file_name();
                        let text = match stage {
                            Stage::Reading if total > 0 => {
                                format!("Reading {name} — {} of {} MB", done >> 20, total >> 20)
                            }
                            Stage::Reading => format!("Reading {name}…"),
                            Stage::Pictures if job.resync.is_some() => format!("Resyncing from {name} — picture {} of {total}", done.min(total)),
                            Stage::Pictures => format!("Opening {name} — picture {} of {total}", done.min(total)),
                        };
                        if i > 0 {
                            ui.add_space(6.0);
                        }
                        ui.horizontal(|ui| {
                            ui.label(text);
                            if job.cancelling() {
                                ui.label("Stopping…");
                            } else if ui.button("Cancel").clicked() {
                                cancel.push(i);
                            }
                        });
                        ui.add(egui::ProgressBar::new(fraction).desired_width(360.0).show_percentage());
                    }
                });
            });
        for i in cancel {
            self.pureref_jobs[i].cancel();
            crate::platform::reclaim_foreground(self.osn_hwnd);
        }
    }

    /// Place an imported picture / video / GIF on the active layer at its
    /// natural on-screen size (shrunk to fit the view), centred on `at` or
    /// on the view — or, with `beside`, next to the selected picture, GIF
    /// or video at its size (`beside_selection`) — and select it (a
    /// video's playback bar then shows). Several dropped at once beside
    /// one each go beside the one in before them.
    pub(super) fn insert_media(
        &mut self,
        imported: crate::media::import::Imported,
        at: Option<egui::Pos2>,
        beside: bool,
        canvas_rect: egui::Rect,
        pixels_per_point: f32,
    ) {
        let (pan, zoom) = {
            let c = self.canvases.active();
            (c.pan, c.zoom.max(1e-3))
        };
        let ppp = pixels_per_point.max(0.01);
        let [w, h] = imported.size;
        let mut size = [w.max(1) as f32 / ppp / zoom, h.max(1) as f32 / ppp / zoom];
        let view = [canvas_rect.width() / zoom, canvas_rect.height() / zoom];
        let fit = (view[0] * 0.9 / size[0]).min(view[1] * 0.9 / size[1]).min(1.0);
        if fit > 0.0 {
            size = [size[0] * fit, size[1] * fit];
        }
        let screen = at.unwrap_or(canvas_rect.center()) - canvas_rect.min;
        let center = [(screen.x - pan[0]) / zoom, (screen.y - pan[1]) / zoom];
        let mut shape = crate::canvas::shape::Shape::Raster {
            pos: [center[0] - size[0] * 0.5, center[1] - size[1] * 0.5],
            size,
            png: std::sync::Arc::new(imported.png),
            px_size: imported.png_size,
            color: [0, 0, 0, 0],
            rotation: 0.0,
            kind: crate::canvas::shape::RasterKind::Image,
            media: imported.media.map(Box::new),
            picture: None,
            pin: 0,
            flip: false,
            group: 0,
            tags: Vec::new(),
            turn: 0.0,
            link: 0,
        };
        if let Some((a, b)) = beside.then(|| self.beside_selection(crate::canvas::canvas::shape_bounds(&shape))).flatten() {
            shape.transform(a, b);
        }
        self.insert_and_select(Vec::new(), vec![shape], [0.0, 0.0]);
    }

    /// Carry out what the playback bars and the FFmpeg window asked for.
    pub(super) fn apply_media_out(&mut self, out: ui::media_controls::MediaUiOut) {
        for (id, cmd) in out.commands {
            self.media.command(id, cmd);
        }
        if out.clicked {
            crate::platform::reclaim_foreground(self.osn_hwnd);
        }
        if let Some(p) = out.located {
            self.config.ffmpeg_path = Some(p);
        }
        if out.ffmpeg_ready {
            self.ffmpeg_available = true;
            self.media.ffmpeg_changed();
            for (path, at) in std::mem::take(&mut self.media_waiting) {
                self.start_media_import(path, at);
            }
        } else if !self.media_ui.ffmpeg_window {
            // Set-up window closed without FFmpeg: give up on those.
            self.media_waiting.clear();
        }
        if let Some(t) = out.toast {
            self.toast = Some((t, Instant::now()));
        }
        if out.toggle_onion {
            self.toggle_onion();
        }
    }

    /// Onion skin on or off for every GIF and video, saying which.
    pub(super) fn toggle_onion(&mut self) {
        let on = !self.media.onion();
        self.media.set_onion(on);
        let msg = if on {
            "Onion skin on: a paused GIF or video shows the frame before in red, the one after in green"
        } else {
            "Onion skin off"
        };
        self.toast = Some((msg.to_string(), Instant::now()));
    }
}

/// What a resync did, in a line: "Resynced slide 3 from refs.pur — 2
/// updated, 1 added (selected), 1 gone from the board (kept …)".
fn resync_message(name: &str, slide: usize, r: &crate::media::pureref_sync::Report, selected: bool) -> String {
    let mut parts = Vec::new();
    if let Some(n) = r.linked {
        parts.push(format!("{n} linked"));
    }
    if r.updated > 0 {
        parts.push(format!("{} updated", r.updated));
    }
    if r.added > 0 {
        parts.push(format!("{} added{}", r.added, if selected { " (selected)" } else { "" }));
    }
    if r.gone > 0 {
        parts.push(format!("{} gone from the board, kept here (the filter can highlight them)", r.gone));
    }
    if r.skipped > 0 {
        parts.push(format!("{} skipped", r.skipped));
    }
    if parts.is_empty() {
        format!("Slide {} is up to date with {name}", slide + 1)
    } else {
        format!("Resynced slide {} from {name} — {}", slide + 1, parts.join(", "))
    }
}
