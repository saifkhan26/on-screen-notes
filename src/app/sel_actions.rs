//! Selection commands from keys: delete, duplicate, copy / cut / paste (objects and pictures
//! from other apps), order, pin.

use super::*;

impl OnScreenNotesApp {
    /// Run one Select-tool command against the current selection.
    pub(super) fn apply_sel_action(&mut self, action: SelAction, canvas_rect: egui::Rect, pixels_per_point: f32) {
        // One nudge so a copy does not sit exactly on its original. Kept
        // constant on screen, so it is divided by the zoom.
        let step = 16.0 / self.canvases.active().zoom.max(1e-3);
        let sel = self.ws.tool.select.sel.clone();
        let has_selection = self.ws.tool.kind == crate::tools::ToolKind::Select && !sel.is_empty();
        match action {
            // Deleting a selected pin unpins it; its objects stay.
            SelAction::Delete if self.ws.pin_tool.selected.is_some() => self.unpin_selected_pin(),
            SelAction::TogglePin if has_selection => self.toggle_pin_selection(),
            SelAction::TogglePin if self.ws.pin_tool.selected.is_some() => self.unpin_selected_pin(),
            // A large selection asks first (`app::confirm::BIG_DELETE`).
            SelAction::Delete if has_selection => self.request_objects_delete(sel.strokes, sel.shapes),
            SelAction::Duplicate if has_selection => {
                let (strokes, shapes) = self.selected_objects();
                self.insert_and_select(strokes, shapes, [step, step]);
            }
            SelAction::CopyToNewSlide if has_selection => self.copy_selection_to_new_slide(),
            SelAction::ToFront | SelAction::ToBack if has_selection => {
                let to_front = matches!(action, SelAction::ToFront);
                let c = self.canvases.active_mut();
                let li = c.active_layer;
                self.ws.tool.select.sel = c.reorder_objects(li, &sel.strokes, &sel.shapes, to_front);
            }
            SelAction::Copy | SelAction::Cut if has_selection => {
                self.obj_clipboard = self.selected_objects();
                self.paste_count = 0;
                // Only labels: their words go on the OS clipboard, to
                // paste as text anywhere. Anything else: a marker, so a
                // later paste knows this copy is newer than whatever
                // another app put there before, and a picture of it.
                self.clip_text = crate::clipboard::text_of(&self.obj_clipboard.0, &self.obj_clipboard.1);
                let text = self.clip_text.as_deref().unwrap_or(CLIP_MARKER);
                match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(text)) {
                    Ok(()) => {}
                    Err(e) => log::warn!("could not mark the clipboard: {e}"),
                }
                #[cfg(target_os = "windows")]
                if self.clip_text.is_none() {
                    self.copy_image_to_clipboard(pixels_per_point);
                }
                if matches!(action, SelAction::Cut) {
                    let c = self.canvases.active_mut();
                    let li = c.active_layer;
                    c.delete_objects(li, &sel.strokes, &sel.shapes);
                    self.ws.tool.select.sel.clear();
                }
            }
            SelAction::PasteObjects => self.paste_objects(step),
            SelAction::PasteImage => self.paste_clipboard_image(canvas_rect, pixels_per_point, step),
            SelAction::PasteText(text) => self.paste_text_label(&text, canvas_rect),
            // A selection command with nothing selected.
            _ => {}
        }
        self.ws.tool.select.rebind(self.canvases.active_index(), self.canvases.active());
    }

    /// Paste the in-app clipboard's objects, each paste a step further
    /// along so repeated pastes fan out. A copy holding pictures, GIFs
    /// or videos goes beside a selected one instead, sized to match, as
    /// the settings ask (`beside_selection`).
    pub(super) fn paste_objects(&mut self, step: f32) {
        let media = self.obj_clipboard.1
            .iter()
            .filter(|s| s.media_type().is_some())
            .map(crate::canvas::canvas::shape_bounds)
            .reduce(union_box);
        if let Some((a, b)) = media.and_then(|m| self.beside_selection(m)) {
            let (mut strokes, mut shapes) = self.obj_clipboard.clone();
            strokes.iter_mut().for_each(|s| s.transform(a, b));
            shapes.iter_mut().for_each(|s| s.transform(a, b));
            self.insert_and_select(strokes, shapes, [0.0, 0.0]);
            return;
        }
        self.paste_count += 1;
        let d = step * self.paste_count as f32;
        let (strokes, shapes) = self.obj_clipboard.clone();
        self.insert_and_select(strokes, shapes, [d, d]);
    }

    /// Put the objects just copied on the OS clipboard as a picture too,
    /// for pasting into other apps (see `crate::clipboard`). Built on a
    /// thread: decoding and encoding a big picture takes a moment, and
    /// until it lands the clipboard holds the text marker. Windows only:
    /// elsewhere pasting an image raises no event the app can catch
    /// (`platform::paste_chord_down`), and in-app paste needs the marker.
    #[cfg(target_os = "windows")]
    pub(super) fn copy_image_to_clipboard(&self, pixels_per_point: f32) {
        let (strokes, shapes) = self.obj_clipboard.clone();
        let zoom = self.canvases.active().zoom;
        let frames = self.media.current_frames();
        let media_dir = crate::persistence::paths::media_dir().ok();
        let mark = std::sync::Arc::clone(&self.clip_image);
        *mark.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let spawned = std::thread::Builder::new().name("osn-copy-image".into()).spawn(move || {
            let Some(img) = crate::clipboard::image_of(strokes, shapes, zoom, pixels_per_point, &frames, media_dir.as_deref()) else {
                return;
            };
            let (w, h) = (img.width() as usize, img.height() as usize);
            let bytes = img.into_raw();
            let Ok(mut cb) = arboard::Clipboard::new() else { return };
            // Whatever the user copied elsewhere in the meantime wins.
            if cb.get_text().ok().as_deref() != Some(CLIP_MARKER) {
                return;
            }
            // Marked first, so a paste right after the image lands
            // already knows it.
            let fp = crate::clipboard::fingerprint(w, h, &bytes);
            *mark.lock().unwrap_or_else(|e| e.into_inner()) = Some(fp);
            let image = arboard::ImageData { width: w, height: h, bytes: std::borrow::Cow::Owned(bytes) };
            if let Err(e) = cb.set_image(image) {
                log::warn!("could not put the copy on the clipboard as a picture: {e}");
                *mark.lock().unwrap_or_else(|e| e.into_inner()) = None;
            }
        });
        if let Err(e) = spawned {
            log::warn!("could not start copying a picture: {e}");
        }
    }

    /// How new objects whose pictures, GIFs and videos span `incoming` (a
    /// canvas box) are sized and placed beside the selected picture, GIF
    /// or video, as the settings ask (`tools::beside`): the `(a, b)` that
    /// `Shape::transform` / `Stroke::transform` take. `None` when that is
    /// off or no such object is selected: new objects then come in as
    /// they always did.
    pub(super) fn beside_selection(&self, incoming: [f32; 4]) -> Option<(f32, [f32; 2])> {
        use crate::tools::beside;
        if self.ws.tool.kind != crate::tools::ToolKind::Select {
            return None;
        }
        let c = self.canvases.active();
        let layer = c.layers.get(c.active_layer)?;
        let media = |i: usize| layer.shapes.get(i).filter(|s| s.media_type().is_some()).map(crate::canvas::canvas::shape_bounds);
        let side = self.config.beside_side;
        // In paint order, so a stack goes on the topmost selected one.
        let mut picked = self.ws.tool.select.sel.shapes.clone();
        picked.sort_unstable();
        let anchor = beside::anchor(picked.into_iter().filter_map(media), side)?;
        let others: Vec<[f32; 4]> = (0..layer.shapes.len()).filter_map(media).collect();
        beside::place(incoming, anchor, self.config.beside_fit, side, &others, 1.0 / c.zoom.max(1e-3))
    }

    /// Paste text from the clipboard as a new label in the middle of the
    /// view: at the size the Text tool's labels show at on screen, in the
    /// pen colour, boxed when the Text tool boxes its labels. Lines wider
    /// than most of the view are wrapped at spaces. Selected once placed.
    /// A picture on the clipboard as well wins: it pastes as a picture
    /// (`paste_clipboard_image`), as before.
    pub(super) fn paste_text_label(&mut self, text: &str, canvas_rect: egui::Rect) {
        /// More than this is cut, so a stray paste of a whole document
        /// does not make a label too big to draw.
        const MAX_CHARS: usize = 20_000;
        if arboard::Clipboard::new().and_then(|mut cb| cb.get_image()).is_ok() {
            return;
        }
        let mut text = crate::interaction::pasted_text(text.trim());
        if text.is_empty() {
            return;
        }
        let cut = text.chars().count() > MAX_CHARS;
        if cut {
            text = text.chars().take(MAX_CHARS).collect();
        }
        let (pan, zoom) = {
            let c = self.canvases.active();
            (c.pan, c.zoom.max(1e-3))
        };
        let tool = &self.ws.tool;
        let font_size = tool.label_font_size() / zoom;
        let text = crate::canvas::shape::wrap_label_text(&text, font_size, canvas_rect.width() / zoom * 0.6);
        let [w, h] = crate::canvas::shape::text_extent(&text, font_size);
        let center = [(canvas_rect.width() * 0.5 - pan[0]) / zoom, (canvas_rect.height() * 0.5 - pan[1]) / zoom];
        let label = crate::canvas::shape::Shape::Text {
            pos: [center[0] - w * 0.5, center[1] - h * 0.5],
            content: text,
            font_size,
            color: tool.color,
            rotation: 0.0,
            pin: 0,
            background: tool.text_box.then(|| crate::canvas::shape::auto_text_box(tool.color)),
            group: 0,
            tags: Vec::new(),
            turn: 0.0,
            link: 0,
        };
        self.insert_and_select(Vec::new(), vec![label], [0.0, 0.0]);
        let said = if cut { format!("Text pasted as a label (its first {MAX_CHARS} characters)") } else { "Text pasted as a label".to_string() };
        self.toast = Some((said, std::time::Instant::now()));
    }

    /// Paste an image from the OS clipboard onto the active layer as an
    /// image object, centred in the view at its natural on-screen size
    /// (shrunk to fit if it is larger than the view), or beside the
    /// selected picture, GIF or video at its size (`beside_selection`).
    /// Silently does nothing when the clipboard holds no image. The
    /// picture of an in-app copy pastes as the objects it was made from
    /// instead.
    pub(super) fn paste_clipboard_image(&mut self, canvas_rect: egui::Rect, pixels_per_point: f32, step: f32) {
        let img = match arboard::Clipboard::new().and_then(|mut cb| cb.get_image()) {
            Ok(img) => img,
            Err(_) => return,
        };
        let own = *self.clip_image.lock().unwrap_or_else(|e| e.into_inner());
        if own.is_some() && own == Some(crate::clipboard::fingerprint(img.width, img.height, &img.bytes)) {
            self.paste_objects(step);
            return;
        }
        let (w, h) = (img.width as u32, img.height as u32);
        let Some(rgba) = image::RgbaImage::from_raw(w, h, img.bytes.into_owned()) else { return };
        if w == 0 || h == 0 { return; }
        let mut png: Vec<u8> = Vec::new();
        if let Err(e) = rgba.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png) {
            log::warn!("could not encode pasted image: {e}");
            return;
        }
        let (pan, zoom) = {
            let c = self.canvases.active();
            (c.pan, c.zoom.max(1e-3))
        };
        // Natural size: one image pixel per physical screen pixel.
        let ppp = pixels_per_point.max(0.01);
        let mut size = [w as f32 / ppp / zoom, h as f32 / ppp / zoom];
        let view = [canvas_rect.width() / zoom, canvas_rect.height() / zoom];
        let fit = (view[0] * 0.9 / size[0]).min(view[1] * 0.9 / size[1]).min(1.0);
        if fit > 0.0 {
            size = [size[0] * fit, size[1] * fit];
        }
        let center = [
            (canvas_rect.width() * 0.5 - pan[0]) / zoom,
            (canvas_rect.height() * 0.5 - pan[1]) / zoom,
        ];
        let mut shape = crate::canvas::shape::Shape::Raster {
            pos: [center[0] - size[0] * 0.5, center[1] - size[1] * 0.5],
            size,
            png: std::sync::Arc::new(png),
            px_size: [w, h],
            color: [0, 0, 0, 0],
            rotation: 0.0,
            kind: crate::canvas::shape::RasterKind::Image,
            media: None,
            picture: None,
            pin: 0,
            flip: false,
            group: 0,
            tags: Vec::new(),
            turn: 0.0,
            link: 0,
            paper: None,
            grain: None,
        };
        if let Some((a, b)) = self.beside_selection(crate::canvas::canvas::shape_bounds(&shape)) {
            shape.transform(a, b);
        }
        self.insert_and_select(Vec::new(), vec![shape], [0.0, 0.0]);
        self.toast = Some(("Image pasted onto the active layer".to_string(), std::time::Instant::now()));
        // Already on a slide? Ringed if so (`app::dup_check`).
        self.dup_watch_selection();
    }
}
