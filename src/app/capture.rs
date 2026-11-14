//! Screenshots, the region picker, frozen frames, lasso captures and recording.

use super::*;

impl OnScreenNotesApp {
    /// Run the deferred screenshot once its frame countdown elapses. A
    /// `region_rect` captures only the user-selected rectangle (compositing
    /// the overlay tint back in); otherwise the whole monitor OSN is on
    /// is grabbed. Logs success/failure but never panics.
    pub(super) fn do_screenshot(&mut self, ps: &PendingShot) {
        let res = if let Some(rect) = ps.region_rect {
            crate::screenshot::capture_region_and_save(
                self.canvases.active(),
                &self.canvases.pinned(),
                &self.config,
                ps.pixels_per_point,
                self.osn_hwnd,
                rect,
                ps.bg_color,
                ps.bg_opacity_before,
                &self.media.current_frames(),
            )
        } else {
            crate::screenshot::capture_and_save(
                self.canvases.active(),
                &self.canvases.pinned(),
                &self.config,
                ps.pixels_per_point,
                self.osn_hwnd,
                &self.media.current_frames(),
            )
        };
        match res {
            Ok(r) => {
                log::info!("screenshot saved to {:?} (clipboard={})", r.path, r.clipboard);
                let msg = if r.clipboard {
                    format!("Screenshot saved: {} · clipboard ✓", r.path.display())
                } else {
                    format!("Screenshot saved: {}", r.path.display())
                };
                self.toast = Some((msg, std::time::Instant::now()));
            }
            Err(e) => log::warn!("screenshot failed: {e}"),
        }
    }

    /// Begin an interactive screenshot region selection. Hides the floating
    /// UI so it stays out of the way (and out of the capture); the actual
    /// grab is deferred until the user drags out a rectangle, handled in
    /// `update_region_select`. The background tint is left alone so the user
    /// aims over a still-visible desktop — it is dropped only at the moment
    /// the region is confirmed, then re-composited into the saved image.
    pub(super) fn begin_region_select(&mut self) {
        if self.region_select.is_some() || self.pending_screenshot.is_some() {
            return;
        }
        self.region_select = Some(RegionSelect {
            start: None,
            current: egui::Pos2::ZERO,
            ui_visible_before: self.ui_visible,
        });
        self.ui_visible = false;
    }

    /// Handle the interactive region-selection overlay for one frame: read
    /// the drag, draw the dimmed rubber-band, and on release turn the
    /// selection into a region `pending_screenshot` (or cancel). No-op when
    /// no selection is active.
    pub(super) fn update_region_select(&mut self, ctx: &egui::Context) {
        let Some(mut rs) = self.region_select else { return };

        let (pressed, released, esc, pos) = ctx.input(|i| {
            (
                i.pointer.primary_pressed(),
                i.pointer.primary_released(),
                i.key_pressed(egui::Key::Escape),
                i.pointer.latest_pos().or(i.pointer.interact_pos()),
            )
        });

        // Esc cancels: restore the chrome, leave the tint untouched.
        if esc {
            self.ui_visible = rs.ui_visible_before;
            self.region_select = None;
            return;
        }

        if let Some(p) = pos {
            rs.current = p;
            if pressed {
                rs.start = Some(p);
            }
        }

        if released {
            if let Some(start) = rs.start {
                let sel = egui::Rect::from_two_pos(start, rs.current);
                let ppp = ctx.pixels_per_point().max(0.01);
                // Reject a too-small drag (a stray click) — treat as cancel.
                if sel.width() * ppp >= 4.0 && sel.height() * ppp >= 4.0 {
                    self.pending_screenshot = Some(PendingShot {
                        ui_visible_before: rs.ui_visible_before,
                        bg_opacity_before: self.config.bg_opacity,
                        frames_to_wait: 2,
                        pixels_per_point: ppp,
                        bg_color: self.config.bg_color,
                        region_rect: Some([sel.min.x, sel.min.y, sel.max.x, sel.max.y]),
                    });
                    // Drop the live tint so xcap grabs a clean desktop; the
                    // capture re-applies `bg_color` at the stored opacity.
                    self.config.bg_opacity = 0.0;
                } else {
                    self.ui_visible = rs.ui_visible_before;
                }
                self.region_select = None;
                return;
            }
        }

        // Still selecting — persist state and draw the overlay on top.
        self.region_select = Some(rs);
        ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("osn_region_select"),
        ));
        let screen = ctx.screen_rect();
        let dim = egui::Color32::from_black_alpha(110);
        if let Some(start) = rs.start {
            let sel = egui::Rect::from_two_pos(start, rs.current);
            // Four dim bands around the selection (the selection stays clear).
            painter.rect_filled(
                egui::Rect::from_min_max(screen.left_top(), egui::pos2(screen.right(), sel.top())),
                0.0,
                dim,
            );
            painter.rect_filled(
                egui::Rect::from_min_max(egui::pos2(screen.left(), sel.bottom()), screen.right_bottom()),
                0.0,
                dim,
            );
            painter.rect_filled(
                egui::Rect::from_min_max(egui::pos2(screen.left(), sel.top()), egui::pos2(sel.left(), sel.bottom())),
                0.0,
                dim,
            );
            painter.rect_filled(
                egui::Rect::from_min_max(egui::pos2(sel.right(), sel.top()), egui::pos2(screen.right(), sel.bottom())),
                0.0,
                dim,
            );
            // Animated dashed border (reuses the marching-ants walker).
            let stroke = egui::Stroke::new(1.5, egui::Color32::WHITE);
            let pts = [sel.left_top(), sel.right_top(), sel.right_bottom(), sel.left_bottom(), sel.left_top()];
            crate::canvas::render::paint_dashed_polyline(&painter, &pts, stroke, 1.0);
            // Dimension label in physical pixels (what the PNG will be).
            let ppp = ctx.pixels_per_point().max(0.01);
            let wpx = (sel.width() * ppp).round() as i32;
            let hpx = (sel.height() * ppp).round() as i32;
            let label_pos = egui::pos2(sel.left(), (sel.top() - 6.0).max(screen.top() + 2.0));
            painter.text(
                label_pos,
                egui::Align2::LEFT_BOTTOM,
                format!("{wpx} × {hpx}"),
                egui::FontId::proportional(14.0),
                egui::Color32::WHITE,
            );
        } else {
            // Before the first drag: dim everything + a hint.
            painter.rect_filled(screen, 0.0, egui::Color32::from_black_alpha(70));
            painter.text(
                screen.center(),
                egui::Align2::CENTER_CENTER,
                "Drag to select a screenshot region  ·  Esc to cancel",
                egui::FontId::proportional(18.0),
                egui::Color32::WHITE,
            );
        }
        // Keep the frame live for the dashed animation + drag tracking.
        ctx.request_repaint();
    }

    /// Enqueue a deferred freeze-frame. Same UI-hide dance as a
    /// regular screenshot — we drop OSN's chrome + tint for two
    /// frames so the captured image is the pure desktop, then insert
    /// the bytes as a new bottom layer on the active canvas.
    pub(super) fn queue_freeze(&mut self, pixels_per_point: f32) {
        if self.pending_freeze.is_some() { return; }
        self.pending_freeze = Some(PendingShot {
            ui_visible_before: self.ui_visible,
            bg_opacity_before: self.config.bg_opacity,
            frames_to_wait: 2,
            pixels_per_point,
            bg_color: self.config.bg_color,
            region_rect: None,
        });
        self.ui_visible = false;
        self.config.bg_opacity = 0.0;
    }

    /// Capture the freeze frame and insert it as a new bottom layer.
    /// Toast on success / log on failure — never panics.
    pub(super) fn do_freeze(&mut self, pixels_per_point: f32) {
        // Snapshot the active canvas's pan/zoom so the captured
        // pixels can be positioned in canvas-local coords that land
        // under the user's current view — without rewriting their
        // pan/zoom state behind their back.
        let canvas = self.canvases.active();
        let pan  = canvas.pan;
        let zoom = canvas.zoom;
        match crate::screenshot::capture_freeze_layer(pixels_per_point, self.osn_hwnd, pan, zoom) {
            Ok(image) => {
                let label = format!("Frozen frame {}", crate::screenshot::chrono_like_timestamp());
                self.canvases.active_mut().add_image_layer_bottom(image, label);
                self.toast = Some((
                    "Frozen frame added as bottom layer".to_string(),
                    std::time::Instant::now(),
                ));
            }
            Err(e) => log::warn!("freeze frame failed: {e}"),
        }
    }

    /// Enqueue a deferred lasso-select capture. Snapshots the active
    /// canvas's pan/zoom alongside the polygon, then runs the same
    /// 2-frame chrome-hide as freeze so the captured pixels are the
    /// pure desktop behind the overlay.
    pub(super) fn queue_lasso_capture(&mut self, polygon: Vec<[f32; 2]>, pixels_per_point: f32) {
        if self.pending_lasso.is_some() { return; }
        let canvas = self.canvases.active();
        self.pending_lasso = Some(PendingLasso {
            ui_visible_before: self.ui_visible,
            bg_opacity_before: self.config.bg_opacity,
            frames_to_wait: 2,
            pixels_per_point,
            pan: canvas.pan,
            zoom: canvas.zoom,
            polygon,
        });
        self.ui_visible = false;
        self.config.bg_opacity = 0.0;
    }

    /// Capture the lasso region and drop it onto the **active layer** as
    /// an image object, selected and ready to move / resize / rotate.
    /// It paints under that layer's ink (a layer draws its shapes before
    /// its strokes) and is an ordinary undo step. Toast on success / log
    /// on failure — never panics.
    pub(super) fn do_lasso_capture(
        &mut self,
        pixels_per_point: f32,
        pan: [f32; 2],
        zoom: f32,
        polygon: &[[f32; 2]],
    ) {
        match crate::screenshot::capture_lasso_layer(
            pixels_per_point,
            self.osn_hwnd,
            pan,
            zoom,
            polygon,
        ) {
            Ok(image) => {
                let shape = crate::canvas::shape::Shape::Raster {
                    pos: image.canvas_origin,
                    size: image.logical_size,
                    png: std::sync::Arc::new(image.png),
                    px_size: image.size,
                    color: [0, 0, 0, 0],
                    rotation: 0.0,
                    kind: crate::canvas::shape::RasterKind::Image,
                    media: None,
                    picture: None,
                    pin: 0,
                    flip: false,
                    group: 0,
                };
                let c = self.canvases.active_mut();
                c.add_shape(shape);
                let placed = c.layers[c.active_layer].shapes.len() - 1;
                self.select_only_shape(placed);
                self.toast = Some((
                    "Captured onto the active layer — drag to move, handles to resize / rotate".to_string(),
                    std::time::Instant::now(),
                ));
            }
            Err(e) => log::warn!("lasso capture failed: {e}"),
        }
    }

    /// Start or stop screen recording. Hides the floating UI on
    /// start so the toolbar and status badge don't end up in the
    /// saved GIF; restores the user's previous UI visibility on
    /// stop. The actual capture + encode lives in `crate::recording`.
    pub(super) fn toggle_record(&mut self) {
        if self.recorder.is_recording() {
            // Stop: finalise encoder (writes GIF trailer) + clipboard
            // copy, then restore UI.
            self.recorder.stop_and_encode();
            self.ui_visible = self.ui_visible_before_record;
        } else {
            // Start: snapshot UI state, hide chrome, kick off
            // recorder with the destination dir captured up front.
            self.ui_visible_before_record = self.ui_visible;
            self.ui_visible = false;
            let dir = self
                .config
                .screenshot_dir
                .clone()
                .or_else(|| crate::persistence::paths::default_screenshot_dir().ok())
                .unwrap_or_else(|| std::path::PathBuf::from("."));
            let use_mp4 = matches!(
                self.config.recording_format,
                crate::config::RecordingFormat::Mp4IfAvailable
            ) && self.ffmpeg_available;
            self.recorder.start(dir, self.osn_hwnd, use_mp4);
        }
    }
}
