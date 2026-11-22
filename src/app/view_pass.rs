//! One window's frame: its canvas, its tool and its UI — what
//! `update` once did inline for the overlay. Everything here reads and
//! changes `self.ws` (the window's own state, `app::workspace`) and the
//! active slide; the rest of `update` is the app's, once a frame.

use super::*;

impl OnScreenNotesApp {
    /// Run one frame of the window whose state is in `self.ws`: the pad
    /// and crop checked, the view's glide and peek, then — while its
    /// canvas shows (`canvas_shown`) — toolbar, panels, the canvas, pen
    /// and mouse, keys. `pen_events` are this frame's pen events in the
    /// window's client points, from `client_origin` (screen pixels).
    /// Returns whether Space is held to pan.
    pub(super) fn view_pass(&mut self, ctx: &egui::Context, pen_events: &[crate::input::pen::PenEvent], client_origin: [i32; 2], canvas_shown: bool) -> bool {
        // The drawing pad: put away if it must be, shown live if open.
        self.pad_frame_start();
        self.crop_frame_start();

        // ---- Zoom to fit / selection: glide and peek ----------------------
        self.update_peek(ctx);
        self.step_view_glide(ctx);

        // ---- Shape snap: time a held pen, run the morph --------------------
        // A pen held still sends nothing, so the frame it is due is asked for.
        if let Some(wait) = self.ws.tool.tick_snap(Instant::now()) {
            ctx.request_repaint_after(wait);
        }

        // ---- Space-hold pan check ---------------------------------------
        // We treat Space as a *modal* pan key: while it is held, all pen +
        // mouse drags pan the active canvas instead of drawing. This is the
        // same convention used by Photoshop, Figma, Rnote, etc.
        // Not while a text field has the keys: a space typed there is
        // not a pan.
        let space_held = ctx.input(|i| i.key_down(egui::Key::Space)) && !self.ws.slide_panel.is_renaming() && !ctx.wants_keyboard_input();

        // ---- Main overlay ------------------------------------------------
        if canvas_shown {
            // Toolbar first so it's drawn under the central canvas's hover
            // rect (egui draws Windows on top of CentralPanel anyway).
            let prev_level = self.config.smoothing_level;
            let prev_pressure_curve = self.config.pressure_curve;
            // Keep the Select tool's selection honest before anything
            // this frame reads it: it is dropped the moment the tool,
            // the slide, the layer or the layer's object count changes
            // underneath it.
            // A locked slide, or one still opening, keeps no selection.
            self.ws.tool.select.sync(
                self.ws.tool.kind == crate::tools::ToolKind::Select && self.canvases.active_editable(),
                self.canvases.active_index(),
                self.canvases.active(),
            );
            self.ws.tool.lasso.sync(self.ws.tool.kind, self.canvases.active_index());
            // Pins are picked with the Select tool: leaving it lets go of
            // the pin, and so does unpinning it some other way (an undo).
            if self.ws.tool.kind != crate::tools::ToolKind::Select || !self.canvases.active_editable() {
                self.ws.pin_tool.clear();
            } else if let Some(id) = self.ws.pin_tool.selected {
                if !self.canvases.pinned().iter().any(|p| p.pin.id == id) {
                    self.ws.pin_tool.clear();
                }
            }
            let (prev_color, prev_size) = (self.ws.tool.color, self.ws.tool.size);
            let prev_text_box = self.ws.tool.text_box;
            let text_selected = !self.selected_text().is_empty();
            let mut insert_media = false;
            let mut select_filtered = false;
            let media_counts = ui::media_filter::counts(self.canvases.active());
            let mut pins_hidden = self.canvases.pins_hidden();
            let has_pins = self.canvases.has_pins();
            let picture_status = self.picture_vectors.status();
            // The slide window has no toolbar, only its bar's colours (see
            // `ui::slide_window::chrome`); every tool and action is the
            // keys', the palette's, the arrange bar's, as here.
            if let Some(c) = self.ws.picked_colour.take() {
                self.ws.tool.color = c;
            }
            let toolbar_clicked = if self.ws.ui_visible && !self.canvases.in_view() {
                ui::toolbar::show(
                    ctx,
                    &mut self.ws.tool,
                    &mut self.config.bg_opacity,
                    &mut self.config.bg_color,
                    &mut self.config.palette,
                    &mut self.config.smoothing_level,
                    &mut self.config.smoothing,
                    &mut self.config.pressure_curve,
                    &mut self.config.ui_scale,
                    &mut self.config.accent,
                    (&mut self.config.shape_snap, &mut self.config.smart_guides, &mut self.config.keep_proportions),
                    (&mut self.config.beside_fit, &mut self.config.beside_side),
                    (&mut self.config.picture_search, picture_status.as_str()),
                    &mut self.pinned_hwnd,
                    &mut self.pin_picking,
                    &mut insert_media,
                    &mut self.media_filter,
                    &media_counts,
                    &mut select_filtered,
                    has_pins.then_some(&mut pins_hidden),
                    text_selected,
                    &self.hotkeys_cfg,
                )
            } else {
                false
            };
            self.set_pins_hidden(pins_hidden);
            self.ws.tool.shape_snap = self.config.shape_snap;
            self.ws.tool.select.guides_on = self.config.smart_guides;
            self.ws.tool.select.keep_proportions = self.config.keep_proportions;
            if self.ws.tool.text_box != prev_text_box {
                self.apply_text_box_to_selection();
            }
            // Recently-used slide history: the active slide counts as
            // visited, and letting go of Ctrl ends a Ctrl+Tab walk.
            if !ctx.input(|i| interaction::slide_walk_held(i.modifiers, &self.hotkeys_cfg)) {
                self.canvases.end_recent_walk();
            }
            self.canvases.note_visit();
            // Slide panel. Going to a slide waits until after this frame's
            // keys, where the arrow keys switch slides too: everything
            // drawn before then still belongs to the current slide.
            // The slide window has none: its card picks the slide.
            let slides = if self.ws.ui_visible && !self.canvases.in_view() {
                // No higher than the REC badge (top-left) while it shows.
                let top = if self.recorder.is_recording() { 44.0 } else { 10.0 };
                ui::slide_panel::show(ctx, &mut self.ws.slide_panel, &mut self.config, top, &self.canvases)
            } else {
                Default::default()
            };
            if select_filtered && self.may_edit() {
                self.select_media_matches();
            }
            // Arrange bar for a selection (only its copy and pin buttons
            // for a single object); hidden mid-gesture so it doesn't
            // flicker over a drag. A selected pin gets its own bar there.
            let selected_count = self.ws.tool.select.sel.strokes.len() + self.ws.tool.select.sel.shapes.len();
            // A picture's colours belong to that picture: another
            // selection puts them away.
            let picture = self.selected_picture();
            if self.ws.picture_colours.as_ref().is_some_and(|p| Some(p.at) != picture) {
                self.ws.picture_colours = None;
            }
            let mut use_colour = None;
            if self.ws.ui_visible
                && self.ws.tool.kind == crate::tools::ToolKind::Select
                && selected_count >= 1
                && self.ws.tool.select.gesture.is_none()
            {
                use ui::arrange_bar::BarAction;
                let pinned = self.selection_pinned();
                let pin_local = if pinned { self.selection_pin_local() } else { None };
                let grouped = self.selection_grouped();
                let open_move = std::mem::take(&mut self.ws.move_picker_pending);
                self.fill_likely_tags(ctx);
                let keys = &self.hotkeys_cfg;
                let row = picture.map(|_| ui::arrange_bar::ColourRow {
                    shown: self.ws.picture_colours.as_ref().map(|p| p.colours.as_slice()),
                    can_restore: self.ws.palette_before.is_some(),
                });
                let tags = self.selection_tags();
                if let Some(e) = self.ws.tag_edit.as_mut() {
                    let (index, _) = self.canvases.tag_index();
                    e.known = index.into_iter().map(|t| (t.tag, t.objects)).collect();
                }
                match ui::arrange_bar::show(
                    ctx,
                    selected_count,
                    pinned,
                    pin_local,
                    &self.canvases,
                    &mut self.config.arrange_apart,
                    &mut self.config.dim_unselected,
                    &mut self.config.pack_mode,
                    keys,
                    open_move,
                    row,
                    grouped,
                    &tags,
                    &mut self.ws.tag_edit,
                ) {
                    Some(BarAction::Group(on)) => self.group_selection(on),
                    Some(BarAction::Arrange(op)) => self.arrange_selection(op, ctx),
                    Some(BarAction::Match(m)) => self.match_selection_size(m),
                    Some(BarAction::CopyToNewSlide) => self.copy_selection_to_new_slide(),
                    Some(BarAction::MoveToSlide(to)) => self.move_selection_to(to),
                    Some(BarAction::TogglePin) => self.toggle_pin_selection(),
                    Some(BarAction::PinScope(local)) => self.set_selection_pin_local(local),
                    Some(BarAction::Flip(vertical)) => self.flip_selection(vertical),
                    Some(BarAction::Rotate(right)) => self.rotate_selection(right),
                    Some(BarAction::ZoomToSelection) => self.zoom_to_selection(ctx),
                    Some(BarAction::OpenInWindow) => self.show_selection_in_window(ctx),
                    Some(BarAction::PictureColours) => self.toggle_picture_colours(),
                    Some(BarAction::FindSimilar) => self.find_similar(),
                    Some(BarAction::Crop) => self.start_crop(),
                    // Set after the restyle below, so the picture is
                    // not "recoloured" into an empty undo step.
                    Some(BarAction::UseColour(c)) => use_colour = Some(c),
                    Some(BarAction::KeepColours) => self.keep_picture_colours(),
                    Some(BarAction::RestorePalette) => self.restore_palette(),
                    Some(BarAction::SetTags { was, now }) => self.tag_selection(was, now),
                    Some(BarAction::FindTag(tag)) => {
                        self.ws.commands.close();
                        self.ws.find.find_words(format!("#{tag}"));
                    }
                    None => {}
                }
            } else {
                // No bar, no tag row.
                self.ws.tag_edit = None;
            }
            self.show_pin_bar(ctx);
            if insert_media {
                self.open_media_dialog();
            }
            // A colour or size picked in the toolbar while objects are
            // selected restyles those objects.
            self.restyle_selection(prev_color, prev_size);
            if let Some(c) = use_colour {
                self.ws.tool.color = c;
                self.toast = Some((format!("Drawing colour #{:02X}{:02X}{:02X}", c[0], c[1], c[2]), Instant::now()));
            }
            // If the user switched smoothing levels via the toolbar, push
            // the new value through the input filter, the tool's pass
            // budget, and rebuild every stroke cache so existing strokes
            // visually match the new level immediately.
            if self.config.smoothing_level != prev_level {
                self.pen_filter.set_level(self.config.smoothing_level);
                let passes = self.config.smoothing_level.position_passes();
                self.ws.tool.position_passes = passes;
                self.canvases.rebuild_all_caches(passes);
            }
            // Pressure-curve change: push to the global atomic that
            // every cache builder reads, then rebuild caches so already-
            // drawn strokes pick up the new curve.
            if (self.config.pressure_curve - prev_pressure_curve).abs() > 1e-4 {
                crate::canvas::stroke::set_pressure_curve(self.config.pressure_curve);
                let passes = self.config.smoothing_level.position_passes();
                self.canvases.rebuild_all_caches(passes);
            }

            // Render canvas + texture upload. The layer-drag preview is
            // resolved up front because `show` takes `&mut
            // self.overlay_cache`, which rules out reading `self` again
            // inside the argument list.
            //
            // Note this paint runs *before* the drag block below updates
            // the delta, so the previewed layer trails the cursor by one
            // frame — the same one-frame lag the canvas pan has always
            // had.
            let layer_xform = self.layer_xform_for_render();
            // Advance video / GIF playback and upload new frames before
            // the canvas paints them.
            // Once a frame, for both windows: the overlay's pass does it,
            // the slide window's draws what it left.
            if !self.canvases.in_view() {
                let view = self.overlay_cache.canvas_rect(ctx);
                self.sync_media(ctx, view);
            }
            // Selection handles must stay out of anything that grabs the
            // screen.
            let show_selection = self.pending_screenshot.is_none()
                && self.pending_freeze.is_none()
                && self.pending_lasso.is_none()
                && self.region_select.is_none();
            let canvas_rect = ui::overlay::show(
                ctx,
                &mut self.overlay_cache,
                &self.canvases,
                &self.ws.tool,
                self.config.smoothing_level.position_passes(),
                self.ws.ui_visible,
                self.loupe.as_ref(),
                layer_xform,
                show_selection,
                &self.media,
                &self.media_filter,
                ui::overlay::PinMarks {
                    selected: self.ws.pin_tool.selected,
                    outlines: self.ws.ui_visible && self.ws.tool.kind == crate::tools::ToolKind::Select,
                },
                self.config.dim_unselected,
            );
            self.pad_chrome(ctx, canvas_rect);
            self.crop_chrome(ctx, canvas_rect);

            // ---- Videos & GIFs ------------------------------------------
            // New files from the dialog / drag-and-drop, finished imports,
            // then the playback bars. Drawn before the pen dispatch below
            // so a tap on a bar counts as UI, never as ink.
            // Files dropped, dialogs and imports land on the overlay's slide.
            if !self.canvases.in_view() {
                self.poll_media_inputs(ctx, canvas_rect);
            }
            let mut media_objs = ui::media_controls::media_on_screen(self.canvases.active(), canvas_rect);
            let pinned = self.canvases.pinned();
            media_objs.extend(ui::media_controls::pinned_media_on_screen(&pinned, canvas_rect));
            let media_selected = if self.ws.tool.kind == crate::tools::ToolKind::Select {
                ui::media_controls::selected_media(self.canvases.active(), &self.ws.tool.select.sel)
                    .or_else(|| self.ws.pin_tool.selected.and_then(|id| ui::media_controls::pinned_media(&pinned, id)))
            } else {
                None
            };
            drop(pinned);
            let window_ui = self.config.slide_window_ui;
            let in_window = self.canvases.in_view();
            let show_media_bars = self.ws.ui_visible
                && !(in_window && window_ui.hide_media_bars)
                && show_selection
                && !self.recorder.is_recording()
                && self.ws.tool.in_progress_stroke.is_none()
                && self.ws.tool.select.gesture.is_none()
                && !self.ws.pin_tool.dragging();
            let mut media_out =
                self.ws.media_ui.show(ctx, &self.media, &media_objs, media_selected, canvas_rect, show_media_bars);
            if !self.canvases.in_view() {
                self.ws.media_ui.ffmpeg_window(ctx, self.osn_hwnd, &mut media_out);
            }
            self.apply_media_out(media_out);
            self.show_delete_confirm(ctx);
            if self.ws.commands.is_open() {
                let picked = {
                    let cx = ui::command_palette::Context {
                        keys: &self.hotkeys_cfg,
                        selected: if self.ws.tool.kind == crate::tools::ToolKind::Select {
                            self.ws.tool.select.sel.strokes.len() + self.ws.tool.select.sel.shapes.len()
                        } else {
                            0
                        },
                        picture: self.selected_picture().is_some(),
                        grouped: self.selection_grouped(),
                        inside_group: {
                            let c = self.canvases.active();
                            c.layers.get(c.active_layer).is_some_and(|l| crate::tools::hop::inside_group(l, &self.ws.tool.select.sel).is_some())
                        },
                        pin: self.ws.pin_tool.selected.is_some() && self.ws.tool.kind == crate::tools::ToolKind::Select,
                        palette: &self.config.palette,
                        slides: (0..self.canvases.count()).map(|i| self.canvases.name(i).to_string()).collect(),
                        active_slide: self.canvases.active_index(),
                    };
                    let all = ui::command_palette::entries(&cx);
                    self.ws.commands.show(ctx, &all)
                };
                if let Some(cmd) = picked {
                    self.run_command(cmd, ctx, canvas_rect);
                }
            }
            if !self.canvases.in_view() {
                self.tick_picture_vectors(ctx);
            }
            self.show_find_bar(ctx);
            self.show_find_flash(ctx, canvas_rect);

            // ---- Screenshot region selection --------------------------------
            // Draws the dimmed rubber-band over the canvas and, on release,
            // turns the drag into a deferred region capture. While active it
            // suppresses the pen/mouse drawing dispatch below (guarded on
            // `region_select.is_none()`).
            if !self.canvases.in_view() {
                self.update_region_select(ctx);
            }

            // ---- Space-hold pan: consume drags before drawing -----------
            // While Space is held, the cursor turns into a grab hand and any
            // primary-button drag (pen OR mouse) pans the canvas. Drawing
            // dispatch is skipped this frame so pen samples don't accidentally
            // commit a stroke during a pan gesture.
            //
            // Two sources of motion:
            //   1. Pen events from Wintab (in window-client pixels). These
            //      DO NOT go through the egui pointer on Windows when a
            //      tablet driver is intercepting input — they only reach us
            //      via the polled Wintab queue. So we route them explicitly.
            //   2. Mouse / trackpad via egui's pointer state. Used when
            //      there is no tablet (mouse fallback path).
            //
            // On a slide that takes no edits (locked, or still opening) a
            // plain drag pans too. The laser and the spotlight still work
            // on a locked slide: they never change it.
            let editable = self.canvases.active_editable();
            let passive_tool = matches!(self.ws.tool.kind, crate::tools::ToolKind::Laser | crate::tools::ToolKind::Spotlight);
            let tool_works_here = editable || (passive_tool && self.canvases.opening().is_none());
            let drag_pans = space_held || !tool_works_here;
            let pen_owns_pointer = !pen_events.is_empty()
                || self.pen.is_down()
                || self.pen.used_within(PEN_OWNS_POINTER);
            let press = self.press_log_before(
                ctx,
                pen_events,
                &press_log::Gates { drag_pans, space_held, editable, pen_owns_pointer, toolbar_clicked, canvas_rect },
            );
            self.drag_pan(ctx, pen_events, client_origin, drag_pans, space_held, editable, pen_owns_pointer, toolbar_clicked, canvas_rect);

            self.dispatch_pen(ctx, pen_events, client_origin, drag_pans, editable, canvas_rect);

            self.dispatch_mouse(ctx, client_origin, drag_pans, pen_owns_pointer, editable, toolbar_clicked, canvas_rect);
            self.press_log_after(press);

            self.middle_button_pan(ctx, space_held);

            self.run_deferred_tools(ctx, canvas_rect);

            // This frame's stamps on the open pad, and its Esc / undo keys
            // before anything else reads the keys.
            self.pad_flush();
            self.pad_keys(ctx);
            self.crop_keys(ctx);

            let keys = self.take_keys(ctx, &slides, space_held, canvas_rect);

            self.run_keys(ctx, keys, slides, canvas_rect, &media_objs, media_selected);

            // The slide window's View menu can leave it out.
            if !(self.canvases.in_view() && self.config.slide_window_ui.hide_layers) {
                self.show_layer_panel(ctx);
            }
        }

        // ---- Laser-stroke pruning ----------------------------------------
        // Drop laser strokes whose fade has run out. The render path
        // skips strokes with alpha <= 0 already, but holding onto them
        // would grow the Vec forever during long sessions.
        if !self.ws.tool.laser_strokes.is_empty() {
            let fade = crate::canvas::render::LASER_FADE_SECS;
            self.ws.tool
                .laser_strokes
                .retain(|(_, up_at)| up_at.elapsed().as_secs_f32() < fade);
        }
        space_held
    }
}
