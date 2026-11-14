//! The phases of a frame (`eframe::App::update`), each its own method.

use super::*;

impl OnScreenNotesApp {
    /// Take in the slides the background loader finished, keep pins up to date
    /// with them, and say so on screen if any could not be read.
    pub(super) fn take_in_slides(&mut self, ctx: &egui::Context) {
        // ---- Slides still loading ---------------------------------------
        // Take in what the background loader finished. A slide that failed
        // to load is the user's work in a state we could not read — say
        // so on screen rather than only in the log (reported once every
        // slide is in).
        self.canvases.poll_loads();
        let stored = self.canvases.store_pictures();
        self.overlay_cache.carry_pictures(&stored);
        self.canvases.refresh_pins();
        // The global pins are on the canvas, or in the slide window when
        // its "Pins" toggle is on; never both.
        self.canvases.set_globals_away(self.slide_window.as_ref().is_some_and(|w| w.shows_pins()));
        // Every pin shows wholly on the canvas area, whatever its size now.
        let area = self.overlay_cache.canvas_rect(ctx).size();
        self.canvases.set_pin_area([area.x, area.y]);
        let errs = self.canvases.take_load_errors();
        if !errs.is_empty() {
            let msg = match errs.len() {
                1 => errs[0].clone(),
                n => format!("{n} slides could not be read — originals kept beside them (*.unreadable-…)"),
            };
            self.toast = Some((msg, std::time::Instant::now()));
        }
    }

    /// Global hotkeys, the deferred screenshot / freeze / lasso captures (which
    /// wait a couple of frames for the chrome to go), and the recorder's result.
    pub(super) fn advance_captures(&mut self, ctx: &egui::Context) {
        // ---- Always-on subsystems ----------------------------------------
        // Global hotkey events.
        if let Some(h) = self.hotkeys.as_ref() {
            for ev in h.poll() {
                match ev {
                    HotkeyId::ToggleOverlay => self.overlay_visible = !self.overlay_visible,
                    // Screenshot: enter interactive region-select; the grab
                    // is deferred until the user drags out a rectangle.
                    HotkeyId::Screenshot   => self.begin_region_select(),
                    HotkeyId::ToggleRecord => self.toggle_record(),
                    HotkeyId::TogglePauseRecord => self.recorder.toggle_pause(),
                }
            }
        }

        // Drive the deferred screenshot state machine. Two frames of
        // wait lets the OS compositor repaint OSN without its chrome
        // before xcap snapshots the desktop. When the counter hits
        // zero, we capture, then restore the chrome the next pass.
        let do_capture = if let Some(ps) = self.pending_screenshot.as_mut() {
            if ps.frames_to_wait > 0 {
                ps.frames_to_wait -= 1;
                ctx.request_repaint();
                false
            } else {
                true
            }
        } else {
            false
        };
        if do_capture {
            if let Some(ps) = self.pending_screenshot.take() {
                self.do_screenshot(&ps);
                self.ui_visible = ps.ui_visible_before;
                self.config.bg_opacity = ps.bg_opacity_before;
            }
        }

        // Same state machine for the freeze-frame-to-layer action.
        let do_freeze = if let Some(ps) = self.pending_freeze.as_mut() {
            if ps.frames_to_wait > 0 {
                ps.frames_to_wait -= 1;
                ctx.request_repaint();
                None
            } else {
                Some((ps.pixels_per_point, ps.ui_visible_before, ps.bg_opacity_before))
            }
        } else {
            None
        };
        if let Some((ppp, ui_prev, bg_prev)) = do_freeze {
            self.do_freeze(ppp);
            self.ui_visible = ui_prev;
            self.config.bg_opacity = bg_prev;
            self.pending_freeze = None;
        }

        // Same state machine for the lasso-select capture. The polygon
        // is owned (not Copy), so we `take()` the whole pending struct
        // once the countdown elapses rather than copying fields out.
        let do_lasso = self
            .pending_lasso
            .as_mut()
            .map(|ps| {
                if ps.frames_to_wait > 0 {
                    ps.frames_to_wait -= 1;
                    false
                } else {
                    true
                }
            })
            .unwrap_or(false);
        if self.pending_lasso.is_some() && !do_lasso {
            ctx.request_repaint();
        }
        if do_lasso {
            if let Some(ps) = self.pending_lasso.take() {
                self.do_lasso_capture(ps.pixels_per_point, ps.pan, ps.zoom, &ps.polygon);
                self.ui_visible = ps.ui_visible_before;
                self.config.bg_opacity = ps.bg_opacity_before;
            }
        }

        // Recording capture+encode runs on a dedicated worker thread
        // spawned in `Recorder::start()`. The UI thread does nothing
        // per-frame except poll for the final result below.
        // Surface the last encode result as a toast (one-shot).
        if let Some(res) = self.recorder.take_result() {
            self.toast = Some((
                format!(
                    "Recording saved: {} ({} frame{}{})",
                    res.path.display(),
                    res.frame_count,
                    if res.frame_count == 1 { "" } else { "s" },
                    if res.copied_to_clipboard { ", clipboard ✓" } else { "" }
                ),
                std::time::Instant::now(),
            ));
        }
    }

    /// This frame's pen events, smoothed for the active smoothing mode (the
    /// ones a gesture over the slide window owns taken out), and where the window's
    /// client area is on screen.
    pub(super) fn smoothed_pen_events(&mut self, ctx: &egui::Context) -> (Vec<crate::input::pen::PenEvent>, [i32; 2]) {
        // 3) Pen events (no-op when tablet missing).
        //    * `client_origin` = screen-pixel coordinate of our window's
        //      client area's top-left. The Wintab backend reports packets
        //      in screen pixels; subtracting `client_origin` converts to
        //      window-client coordinates.
        //    * After polling, we transform each sample from window-client
        //      to canvas-local (subtract canvas rect, pan, divide zoom),
        //      then dispatch to tools.
        let client_origin = ctx.input(|i| {
            i.viewport()
                .inner_rect
                .map(|r| [r.min.x as i32, r.min.y as i32])
                .unwrap_or([0, 0])
        });
        let raw_pen_events = self.pen.poll(client_origin);

        // ---- Smooth raw pen samples --------------------------------------
        // Wintab packets carry digitizer noise; at low pen speed the noise
        // dominates and the Catmull-Rom subdivision amplifies it into
        // visible jagged wobble. Run each sample through a One Euro filter
        // (speed-adaptive low-pass: tight at rest, near-passthrough at
        // speed) and drop near-duplicate samples.
        //
        //   * `Down` resets the filter and force-accepts the sample so the
        //     stroke starts exactly where the user pressed.
        //   * `Move` is filtered + min-distance gated; samples that don't
        //     move enough are dropped.
        //   * `Up` is force-accepted so pen-up lands exactly.
        // Sync the active tool's smoothing snapshot from the live
        // config. A new stroke begun this frame will lock-in the
        // current options at pen-down; an in-progress stroke keeps the
        // snapshot it was started with.
        self.tool.smoothing = self.config.smoothing;
        // Push the render-side fan-corner toggle so the ribbon mesh
        // builder can read it lock-free on the hot path.
        crate::canvas::render::set_fan_corners(
            self.config.smoothing.fan_corners,
            self.config.smoothing.fan_corners_step,
        );

        use crate::input::pen::PenEvent;
        let mut pen_events: Vec<PenEvent> = Vec::with_capacity(raw_pen_events.len());
        // Krita modes (Stabilizer / Weighted / Simple / None) run their
        // own algorithm on raw input — the upstream PenFilter would be
        // a second-stage smoother the user did not ask for. Only
        // Adaptive mode keeps it.
        let use_pen_filter = self.config.smoothing.use_upstream_pen_filter();
        self.pen_filter.begin_batch(raw_pen_events.len());
        for ev in raw_pen_events {
            match ev {
                PenEvent::Down(s) => {
                    self.pen_filter.reset();
                    if use_pen_filter {
                        if let Some(fs) = self.pen_filter.filter(s, true) {
                            pen_events.push(PenEvent::Down(fs));
                        }
                    } else {
                        pen_events.push(PenEvent::Down(s));
                    }
                }
                PenEvent::Move(s) => {
                    if use_pen_filter {
                        if let Some(fs) = self.pen_filter.filter(s, false) {
                            pen_events.push(PenEvent::Move(fs));
                        }
                    } else {
                        pen_events.push(PenEvent::Move(s));
                    }
                }
                PenEvent::Up(s) => {
                    if use_pen_filter {
                        if let Some(fs) = self.pen_filter.filter(s, true) {
                            pen_events.push(PenEvent::Up(fs));
                        }
                    } else {
                        pen_events.push(PenEvent::Up(s));
                    }
                }
            }
        }
        // A pen gesture that starts over the slide window is the window's:
        // it replays it on the slide it shows (`ui::slide_edit`).
        if let Some(win) = self.slide_window.as_mut() {
            win.claim_pen(&mut pen_events, client_origin);
        }
        (pen_events, client_origin)
    }

    /// Pin to window: pick the target window with a click through the overlay,
    /// then keep the overlay over it. Esc also cancels the pick, closes the layer
    /// preview and lets go of a selected pin.
    pub(super) fn follow_pinned_window(&mut self, ctx: &egui::Context) {
        // ---- Pin-to-window follow ---------------------------------------
        // Two phases:
        //   (a) `pin_picking` — overlay is click-through so the user
        //       can target a window; the next external LMB-down
        //       captures the top-level HWND under the cursor.
        //   (b) `pinned_hwnd` is Some — each frame we read the
        //       target's rect and reposition the overlay to match.
        //       If the rect query fails (window minimised or
        //       destroyed), we drop the pin and toast the user.
        if self.pin_picking {
            // Esc cancels pin-picking without locking us into
            // click-through mode forever.
            if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.pin_picking = false;
                ctx.send_viewport_cmd(egui::ViewportCommand::MousePassthrough(false));
            }
        }
        // Esc also closes the layer-preview popup, and lets go of a pin.
        if self.show_layer_preview && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.show_layer_preview = false;
        }
        if self.pin_tool.selected.is_some() && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.pin_tool.clear();
        }
        if self.pin_picking {
            // Force click-through so the visible click lands on the
            // target window, not on OSN.
            ctx.send_viewport_cmd(egui::ViewportCommand::MousePassthrough(true));
            let lmb_now = crate::platform::lmb_down();
            if lmb_now && !self.pin_prev_lmb {
                if let Some((cx, cy)) = crate::platform::cursor_pos() {
                    let hwnd = crate::platform::top_level_window_at(cx, cy);
                    // Our own windows are no pin targets.
                    let ours = hwnd == self.osn_hwnd || self.slide_window.as_ref().is_some_and(|w| w.hwnd() == hwnd);
                    if hwnd != 0 && !ours {
                        self.pinned_hwnd = Some(hwnd);
                        self.pinned_last_rect = None;
                        self.toast = Some((
                            "Pinned to window".to_string(),
                            std::time::Instant::now(),
                        ));
                    }
                }
                self.pin_picking = false;
                // Restore normal click handling.
                ctx.send_viewport_cmd(egui::ViewportCommand::MousePassthrough(false));
            }
            self.pin_prev_lmb = lmb_now;
        } else {
            self.pin_prev_lmb = false;
        }

        if let Some(hwnd) = self.pinned_hwnd {
            match crate::platform::window_rect(hwnd) {
                None => {
                    self.pinned_hwnd = None;
                    self.pinned_last_rect = None;
                    self.toast = Some((
                        "Pin lost: target window minimised or closed".to_string(),
                        std::time::Instant::now(),
                    ));
                }
                Some(rect) => {
                    if Some(rect) != self.pinned_last_rect {
                        let ppp = ctx.pixels_per_point().max(0.01);
                        let (l, t, r, b) = rect;
                        let w = (r - l).max(1) as f32 / ppp;
                        let h = (b - t).max(1) as f32 / ppp;
                        let pos = egui::pos2(l as f32 / ppp, t as f32 / ppp);
                        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos));
                        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(w, h)));
                        self.pinned_last_rect = Some(rect);
                    }
                }
            }
        }
    }

    /// The magnifier loupe while Z is held (not Ctrl+Z): start it, feed it the
    /// screen behind the window, drop it on release.
    pub(super) fn update_loupe(&mut self, ctx: &egui::Context) {
        // ---- Magnifier loupe (Z-held modal) -----------------------------
        // Rising edge → start the live loupe, which captures the screen
        // under the window on a worker thread and uploads each frame as
        // a texture. Falling edge → drop the state so the GPU memory
        // is reclaimed. Captures cost ~30 ms; keeping them off the UI
        // thread keeps the loupe at full overlay refresh while held.
        //
        // Gated on !ctrl so Ctrl+Z (undo) and Ctrl+Shift+Z (redo) do
        // not accidentally arm the loupe.
        {
            let z_held = ctx.input(|i| i.key_down(egui::Key::Z));
            let z_ctrl = ctx.input(|i| i.modifiers.ctrl || i.modifiers.command);
            // A "z" typed into a slide name is not the loupe.
            let z_modal = z_held && !z_ctrl && !self.slide_panel.is_renaming();
            if z_modal && self.loupe.is_none() {
                match ui::loupe::LoupeState::start_live(ctx, self.osn_hwnd) {
                    Ok(l) => self.loupe = Some(l),
                    Err(e) => log::warn!("loupe capture failed: {e}"),
                }
            }
            if !z_modal && self.loupe.is_some() {
                self.loupe = None;
            }
            // Drain any new frame the worker has produced and push
            // it into the GPU texture for this frame's paint.
            if let Some(l) = self.loupe.as_mut() {
                l.poll_and_update();
            }
        }
    }

    /// The REC badge while recording, and the toast (gone after 5 s).
    pub(super) fn show_rec_and_toast(&mut self, ctx: &egui::Context) {
        // ---- Recording indicator + toast ---------------------------------
        // REC dot painted while capturing. We use a tiny floating
        // Area in the top-left so the user always knows the recorder
        // is on, even with `ui_visible = false`. Pen events pass
        // through it (the area is render-only).
        if self.recorder.is_recording() {
            let elapsed = self.recorder.elapsed().as_secs();
            egui::Area::new(egui::Id::new("osn_rec_indicator"))
                .anchor(egui::Align2::LEFT_TOP, egui::vec2(12.0, 12.0))
                .interactable(false)
                .show(ctx, |ui| {
                    egui::Frame::none()
                        .fill(egui::Color32::from_rgba_unmultiplied(0, 0, 0, 140))
                        .rounding(6.0)
                        .inner_margin(egui::Margin::symmetric(8.0, 4.0))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                // Pulsing red dot — alpha modulated by
                                // the system clock so it reads as "live".
                                let pulse = {
                                    let ms = ctx.input(|i| i.time);
                                    (((ms * 2.0).sin() * 0.5 + 0.5) * 255.0) as u8
                                };
                                let (rect, _) = ui.allocate_exact_size(
                                    egui::vec2(10.0, 10.0),
                                    egui::Sense::hover(),
                                );
                                ui.painter().circle_filled(
                                    rect.center(),
                                    4.5,
                                    egui::Color32::from_rgba_unmultiplied(232, 71, 71, pulse),
                                );
                                ui.label(match &self.record_hotkey {
                                    Some(k) => format!("REC  {elapsed:02}s  · {k} stop"),
                                    None => format!("REC  {elapsed:02}s"),
                                });
                            });
                        });
                });
        }
        // Toast — clears after 5 s.
        if let Some((msg, t)) = self.toast.clone() {
            if t.elapsed() > Duration::from_secs(5) {
                self.toast = None;
            } else {
                egui::Area::new(egui::Id::new("osn_toast"))
                    .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -16.0))
                    .interactable(false)
                    .show(ctx, |ui| {
                        egui::Frame::none()
                            .fill(egui::Color32::from_rgba_unmultiplied(15, 18, 24, 220))
                            .rounding(8.0)
                            .inner_margin(egui::Margin::symmetric(12.0, 8.0))
                            .show(ui, |ui| {
                                ui.label(msg);
                            });
                    });
            }
        }
    }

    /// The slide window, after the overlay drew this frame (it shares the
    /// overlay's texture cache), looking like the overlay and following it live.
    pub(super) fn show_slide_window(&mut self, ctx: &egui::Context, space_held: bool) {
        // ---- Slide window ------------------------------------------------
        // After the overlay drew this frame: the window shares its
        // texture cache, whose once-a-frame start the overlay ran — or,
        // with the overlay hidden, runs here.
        if let Some(win) = self.slide_window.as_mut() {
            if !self.overlay_visible {
                self.overlay_cache.begin_frame(ctx);
            }
            // It looks like the overlay, and follows it live.
            let look = ui::slide_window::Look {
                background: ui::slide_window::Look::background(self.config.bg_color, self.config.bg_opacity),
                chrome: self.ui_visible,
                owner: self.osn_hwnd,
            };
            // Where the canvas is on screen, for a selection dragged out of
            // the window onto it; and whether one dragged on the canvas is
            // over the window now.
            let ppp = ctx.pixels_per_point();
            let canvas_min = self.overlay_cache.canvas_rect(ctx).min;
            let main = crate::platform::client_rect(self.osn_hwnd).map(|(l, t, _, _)| ui::slide_edit::MainCanvas {
                hwnd: self.osn_hwnd,
                origin_px: [l as f32 + canvas_min.x * ppp, t as f32 + canvas_min.y * ppp],
                ppp,
            });
            let dragging_selection = self.tool.kind == crate::tools::ToolKind::Select
                && !self.tool.select.sel.is_empty()
                && matches!(self.tool.select.gesture, Some(crate::tools::select_tool::Gesture::Move { .. }));
            let drop_hover = dragging_selection
                && win.hwnd() != 0
                && crate::platform::cursor_pos().is_some_and(|(x, y)| crate::platform::top_level_window_at(x, y) == win.hwnd());
            let input = ui::slide_edit::Input {
                space: space_held,
                pen_owns_pointer: self.pen.is_down() || self.pen.used_within(PEN_OWNS_POINTER),
                keys: &self.hotkeys_cfg,
                main,
                drop_hover,
                main_area: {
                    let s = self.overlay_cache.canvas_rect(ctx).size();
                    [s.x, s.y]
                },
            };
            let open = ui::slide_window::show(
                ctx,
                win,
                &mut self.overlay_cache,
                &mut self.canvases,
                self.config.smoothing_level.position_passes(),
                &self.media,
                &look,
                &mut self.tool,
                &input,
            );
            if let Some(msg) = win.take_notice() {
                self.toast = Some((msg, Instant::now()));
            }
            if let Some(id) = win.take_open_pin() {
                let area = self.overlay_cache.canvas_rect(ctx).size();
                self.open_pin_slide(id, area);
            }
            if !open {
                self.slide_window = None;
            }
        }
    }

    /// Every 5 s: save what changed (in the background) and the config.
    pub(super) fn autosave(&mut self) {
        // ---- Auto-save (debounced) ---------------------------------------
        // Bumped from 1500 ms to 5000 ms because the save itself is
        // now (a) background-threaded, (b) dirty-gated so most ticks
        // are no-ops, and (c) using compact RON. Worst-case data loss
        // on a crash is now 5 s instead of 1.5 s — acceptable for an
        // annotation tool, and the UI stays smooth even with 9+
        // PNG-bearing canvases.
        if self.last_save_at.elapsed() > Duration::from_millis(5000) {
            // Media the in-app clipboard holds must survive a slide
            // delete; while an import or a board is being opened (its
            // pictures may be files a deleted slide also used), media
            // cleanup waits.
            let clipboard_media: std::collections::HashSet<String> = self
                .obj_clipboard
                .1
                .iter()
                .filter_map(|s| s.media_file().map(str::to_string))
                .collect();
            let media_in_use = (self.media_imports.is_empty() && self.pureref_jobs.is_empty()).then_some(&clipboard_media);
            self.canvases.save_all(media_in_use);
            if let Err(e) = crate::persistence::save_config(&self.config) {
                log::warn!("config save failed: {e}");
            }
            // The overlay may have moved to another monitor.
            self.idle_frame = refresh_period(self.osn_hwnd);
            self.last_save_at = Instant::now();
        }
    }

    /// The layer panel, and the large preview of the active layer when it is up.
    pub(super) fn show_layer_panel(&mut self, ctx: &egui::Context) {
        // ---- Layer panel ---------------------------------------------
        // Hidden when the master UI toggle is off.
        if self.ui_visible {
            let canvas_idx = self.canvases.active_index();
            // Pass the manager, not `active_mut()`: taking the mutable
            // canvas borrow every frame just to paint would flag it
            // dirty ~60×/s and put the autosave back into a permanent
            // rewrite loop. The panel takes it only on a real click.
            ui::layer_panel::show(
                ctx,
                &mut self.overlay_cache,
                canvas_idx,
                &mut self.canvases,
            );
        }
        // Layer preview popup — large centred thumbnail of the
        // active layer. Toggled by the `canvas.preview_layer`
        // hotkey (`q` by default). Esc closes (handled earlier in
        // this frame).
        if self.show_layer_preview {
            let canvas_idx = self.canvases.active_index();
            let active_li = self.canvases.active().active_layer;
            let layer_clone = self.canvases.active().layers[active_li].clone();
            let tex = self.overlay_cache.ensure_layer_thumb(
                ctx, canvas_idx, active_li, &layer_clone,
            );
            let screen = ctx.screen_rect();
            egui::Area::new(egui::Id::new("osn_layer_preview_popup"))
                .fixed_pos(egui::pos2(
                    screen.center().x - 260.0,
                    screen.center().y - 220.0,
                ))
                .show(ctx, |ui| {
                    egui::Frame {
                        inner_margin: egui::Margin::same(12.0),
                        outer_margin: egui::Margin::ZERO,
                        rounding: egui::Rounding::same(12.0),
                        shadow: ui::theme::card_shadow(),
                        fill: ui::theme::GLASS_FILL,
                        stroke: egui::Stroke::NONE,
                    }
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new(format!(
                                "Layer {} — {} strokes · {} shapes",
                                active_li + 1,
                                layer_clone.strokes.len(),
                                layer_clone.shapes.len(),
                            ))
                            .color(ui::theme::TEXT_PRIMARY),
                        );
                        ui.add_space(6.0);
                        match tex {
                            Some(tex_id) => {
                                let size = egui::Vec2::new(480.0, 360.0);
                                ui.add(
                                    egui::Image::new((tex_id, size))
                                        .fit_to_exact_size(size),
                                );
                            }
                            None => {
                                ui.label(
                                    egui::RichText::new("(empty layer)")
                                        .color(ui::theme::TEXT_MUTED),
                                );
                            }
                        }
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new("Esc or hotkey to close")
                                .small()
                                .color(ui::theme::TEXT_MUTED),
                        );
                    });
                });
        }
    }
}
