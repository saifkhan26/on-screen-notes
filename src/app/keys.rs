//! A frame's keys and mouse wheel: read once (`take_keys`), and the commands that
//! must wait for the rest of the frame run after it (`run_keys`).

use super::*;

/// What this frame's keys asked for that runs after the rest of the frame.
pub(super) struct FrameKeys {
    text_consumed: bool,
    sel_actions: Vec<SelAction>,
    media_keys: Vec<crate::media::Cmd>,
    toggle_pins: bool,
    rename_slide: bool,
    toggle_lock: bool,
    delete_slide: bool,
    key_actions: Vec<interaction::action_keys::KeyAction>,
    refused: bool,
}

impl OnScreenNotesApp {
    /// This frame's keys and wheel. Shortcuts that need nothing else run here;
    /// the commands that must wait for the rest of the frame come back for `run_keys`.
    pub(super) fn take_keys(&mut self, ctx: &egui::Context, slides: &ui::slide_panel::Shown, space_held: bool, canvas_rect: egui::Rect) -> FrameKeys {
        // ---- Keyboard shortcuts --------------------------------------
        // Both Ctrl+0 (reset view) and Ctrl+Shift+F (freeze frame)
        // used to be matched via `ctx.input_mut(consume_shortcut)`
        // with hardcoded `KeyboardShortcut::new` values. They now
        // come from `HotkeysConfig`, so we build the shortcut from
        // the parsed binding and feed it to the same consumer —
        // egui still does the atomic modifier+key match for us.
        //
        // `reset_layer_pos` (Ctrl+Shift+0) is tested *before*
        // `reset_view` (Ctrl+0) and deliberately so: `consume_shortcut`
        // matches modifiers logically, i.e. a held Shift does not
        // disqualify a Ctrl-only pattern. Checking Ctrl+0 first would
        // let it swallow Ctrl+Shift+0 and reset the view instead.
        //
        // Layer pans are baked into the geometry, so "reset" is just
        // a move by the negated running total — which also means it
        // lands in the undo history like any other move.
        if let Some(b) = self.hotkeys_cfg.canvas.reset_layer_pos {
            let sc = egui::KeyboardShortcut::new(b.to_modifiers(), b.key);
            if !slides.typing && ctx.input_mut(|i| i.consume_shortcut(&sc)) && self.may_edit() {
                let c = self.canvases.active_mut();
                let li = c.active_layer;
                if let Some(m) = c.layers.get(li).map(|l| l.moved_by) {
                    c.move_layer_by(li, [-m[0], -m[1]]);
                }
            }
        }
        // Layer zooms bake into the geometry the same way pans do,
        // so "reset" divides the running total back out. The pivot
        // is the layer's own bounding-box centre, which keeps the
        // layer where the user can see it — scaling about the
        // canvas origin instead would fling a far-from-origin layer
        // off-screen. `moved_by` is rewritten by the same maths, so
        // a later `reset_layer_pos` still lands on the layer's
        // original coordinates exactly.
        if let Some(b) = self.hotkeys_cfg.canvas.reset_layer_zoom {
            let sc = egui::KeyboardShortcut::new(b.to_modifiers(), b.key);
            if !slides.typing && ctx.input_mut(|i| i.consume_shortcut(&sc)) && self.may_edit() {
                let c = self.canvases.active_mut();
                let li = c.active_layer;
                if let Some(layer) = c.layers.get(li) {
                    let a = layer.scaled_by;
                    if a > 1e-6 && (a - 1.0).abs() > 1e-6 {
                        let pivot = layer
                            .content_bounds()
                            .map(|bb| [(bb[0] + bb[2]) * 0.5, (bb[1] + bb[3]) * 0.5])
                            .unwrap_or([0.0, 0.0]);
                        c.scale_layer_about(li, 1.0 / a, pivot);
                    }
                }
            }
        }
        if let Some(b) = self.hotkeys_cfg.canvas.reset_view {
            let sc = egui::KeyboardShortcut::new(b.to_modifiers(), b.key);
            if !slides.typing && ctx.input_mut(|i| i.consume_shortcut(&sc)) {
                let (pan, zoom) = self.canvases.active_view_mut();
                *zoom = 1.0;
                *pan  = [0.0, 0.0];
            }
        }
        if let Some(b) = self.hotkeys_cfg.canvas.freeze_frame {
            let sc = egui::KeyboardShortcut::new(b.to_modifiers(), b.key);
            if !slides.typing && ctx.input_mut(|i| i.consume_shortcut(&sc)) && self.may_edit() {
                self.queue_freeze(ctx.pixels_per_point());
            }
        }

        // Text tool steals keyboard input while a caret is open.
        // Returns true when the events were consumed — in that
        // case we skip the tool-shortcut / undo / canvas-nav
        // dispatcher so typing "t" / "Ctrl+Z" lands in the label,
        // not as a tool change or an undo.
        // A slide name being typed owns the keys the same way.
        let text_consumed = slides.typing || interaction::route_text_input(ctx, &mut self.tool, &mut self.canvases);

        // Select-tool commands found in this frame's events. They
        // are only collected inside the input closure and run after
        // it, because several of them need `&mut self` as a whole.
        let mut sel_actions: Vec<SelAction> = Vec::new();
        // Play / step keys, applied after the input closure. Not while
        // a text field has the keyboard.
        let mut media_keys: Vec<crate::media::Cmd> = Vec::new();
        let media_keys_live = !ctx.wants_keyboard_input();
        // Shift+P, applied after the input closure.
        let mut toggle_pins = false;
        // F2 (name the active slide), likewise.
        let mut rename_slide = false;
        // Ctrl+R (lock or unlock it), likewise.
        let mut toggle_lock = false;
        // Ctrl+Shift+Backspace (delete it, once confirmed), likewise.
        let mut delete_slide = false;
        // Keys bound to buttons (`interaction::action_keys`), likewise.
        let mut key_actions: Vec<interaction::action_keys::KeyAction> = Vec::new();
        // With objects selected a nudge key moves them and does
        // nothing else, even when it is a slide or layer key too (the
        // arrows are, by default).
        let nudge_live = self.tool.kind == crate::tools::ToolKind::Select
            && !self.tool.select.sel.is_empty()
            && self.tool.select.gesture.is_none();
        // An edit key on a slide that takes no edits, to say why
        // nothing happened.
        let mut refused = false;

        ctx.input(|i| {
            let mods = i.modifiers;
            // Locals retained for the scroll handler below, which
            // takes raw bools (not Modifiers).
            let ctrl  = mods.ctrl || mods.command;
            let shift = mods.shift;
            // "Delete this slide?" and the command palette take the
            // keys while they are up.
            if text_consumed || self.confirm_delete.is_some() || self.commands.is_open() {
                // Still allow scroll-wheel events (handled below
                // outside this for-loop) but drop key events.
                // Falling through to the wheel block below means
                // we skip the per-event match by emptying the
                // iterator.
                return;
            }
            let keys = &self.hotkeys_cfg;
            for ev in &i.events {
                match ev {
                    // egui-winit swallows the Ctrl+C / X / V key
                    // presses and hands us these instead.
                    egui::Event::Copy => sel_actions.push(SelAction::Copy),
                    egui::Event::Cut => sel_actions.push(SelAction::Cut),
                    egui::Event::Paste(text) if text == CLIP_MARKER => {
                        sel_actions.push(SelAction::PasteObjects);
                    }
                    // Elsewhere there is no key-state fallback (see
                    // `platform::paste_chord_down`), so a paste of
                    // anything that is not our own marker is the
                    // only cue to look for an image.
                    #[cfg(not(target_os = "windows"))]
                    egui::Event::Paste(_) => sel_actions.push(SelAction::PasteImage),
                    egui::Event::Key { key, pressed: true, .. } => {
                        let c = &keys.canvas;
                        let hit = |b: Option<crate::input::hotkeys_config::Binding>| {
                            b.is_some_and(|b| b.matches(*key, mods))
                        };
                        // Plain Backspace doubles as Delete: on a
                        // tablet-side keyboard it is the nearer key.
                        let bare_backspace = *key == egui::Key::Backspace
                            && !(mods.ctrl || mods.command || mods.shift || mods.alt);
                        if hit(c.delete_selection) || bare_backspace {
                            sel_actions.push(SelAction::Delete);
                        } else if hit(c.duplicate) {
                            sel_actions.push(SelAction::Duplicate);
                        } else if hit(c.copy_to_new_slide) {
                            sel_actions.push(SelAction::CopyToNewSlide);
                        } else if hit(c.pin_selection) {
                            sel_actions.push(SelAction::TogglePin);
                        } else if hit(c.bring_front) {
                            sel_actions.push(SelAction::ToFront);
                        } else if hit(c.send_back) {
                            sel_actions.push(SelAction::ToBack);
                        }
                        if media_keys_live {
                            if hit(c.media_play_pause) {
                                media_keys.push(crate::media::Cmd::TogglePlay);
                            } else if hit(c.media_prev_frame) {
                                media_keys.push(crate::media::Cmd::Step(-1));
                            } else if hit(c.media_next_frame) {
                                media_keys.push(crate::media::Cmd::Step(1));
                            } else if hit(c.media_loop) {
                                media_keys.push(crate::media::Cmd::ToggleLoop);
                            }
                        }
                    }
                    _ => {}
                }
                if let egui::Event::Key { key, pressed: true, .. } = ev {
                    // Tool switch shortcuts.
                    interaction::react_tool_shortcut(*key, mods, &mut self.tool, keys);
                    let collected = key_actions.len();
                    interaction::action_keys::collect(*key, mods, keys, &mut key_actions);
                    let nudged = nudge_live
                        && key_actions[collected..]
                            .iter()
                            .any(|a| matches!(a, interaction::action_keys::KeyAction::Nudge(_)));
                    // Every key passes through here: only an edit may
                    // flag the slide for the autosave. Checked per key:
                    // the one before may have gone to another slide.
                    let editable = self.canvases.active_editable();
                    if editable {
                        self.canvases.edit_active(|c| interaction::react_lasso_keys(*key, mods, &mut self.tool, c));
                    }
                    // Canvas + layer navigation. A slide's active layer is
                    // part of it, so it stays put on a locked slide.
                    if !nudged {
                        interaction::react_canvas_nav(*key, mods, &mut self.canvases, keys, self.config.slide_tab_order);
                        if editable {
                            interaction::react_layer_nav(*key, mods, &mut self.canvases, keys);
                        }
                    }
                    // Toggle floating UI chrome.
                    if let Some(b) = keys.canvas.toggle_ui {
                        if b.matches(*key, mods) {
                            self.ui_visible = !self.ui_visible;
                        }
                    }
                    if keys.canvas.toggle_pins.is_some_and(|b| b.matches(*key, mods)) {
                        toggle_pins = !toggle_pins;
                    }
                    if keys.canvas.rename_slide.is_some_and(|b| b.matches(*key, mods)) {
                        rename_slide = true;
                    }
                    if keys.canvas.lock_slide.is_some_and(|b| b.matches(*key, mods)) {
                        toggle_lock = !toggle_lock;
                    }
                    // Undo, redo, clear, delete the slide and the layer
                    // opacity and visibility keys all edit the slide.
                    let kc = &keys.canvas;
                    let edit_key = [
                        kc.undo, kc.redo, kc.clear_layer, kc.delete_canvas,
                        kc.opacity_dec, kc.opacity_inc, kc.toggle_layer_visible,
                    ]
                        .into_iter()
                        .chain(kc.opacity)
                        .flatten()
                        .any(|b| b.matches(*key, mods));
                    if !editable && edit_key {
                        refused = true;
                        continue;
                    }
                    // Undo / redo / clear / delete-canvas.
                    if let Some(b) = keys.canvas.undo {
                        if b.matches(*key, mods) {
                            self.canvases.active_mut().undo();
                            // The entry a size-slider run or a run of
                            // nudges was being folded into may be gone now.
                            self.restyle_run = None;
                            self.nudge_run = None;
                        }
                    }
                    if let Some(b) = keys.canvas.redo {
                        if b.matches(*key, mods) {
                            self.canvases.active_mut().redo();
                            self.restyle_run = None;
                            self.nudge_run = None;
                        }
                    }
                    if let Some(b) = keys.canvas.clear_layer {
                        if b.matches(*key, mods) { self.canvases.active_mut().clear(); }
                    }
                    if keys.canvas.delete_canvas.is_some_and(|b| b.matches(*key, mods)) {
                        delete_slide = true;
                    }
                    // Layer opacity: 1..9 set absolute N·10 %,
                    // 0 sets 0 %. `-` / `=` step ±10 %. All
                    // clamped to [0, 1]. Default bindings live in
                    // `HotkeysConfig::CanvasSection::opacity_*`.
                    for (n, b) in keys.canvas.opacity.iter().enumerate() {
                        if let Some(b) = b {
                            if b.matches(*key, mods) {
                                let v = (n as f32) * 0.10;
                                let c = self.canvases.active_mut();
                                c.active_layer_mut().opacity = v;
                                c.mark_dirty();
                            }
                        }
                    }
                    if let Some(b) = keys.canvas.opacity_dec {
                        if b.matches(*key, mods) {
                            let c = self.canvases.active_mut();
                            let l = c.active_layer_mut();
                            l.opacity = (l.opacity - 0.10).clamp(0.0, 1.0);
                            c.mark_dirty();
                        }
                    }
                    if let Some(b) = keys.canvas.opacity_inc {
                        if b.matches(*key, mods) {
                            let c = self.canvases.active_mut();
                            let l = c.active_layer_mut();
                            l.opacity = (l.opacity + 0.10).clamp(0.0, 1.0);
                            c.mark_dirty();
                        }
                    }
                    if let Some(b) = keys.canvas.preview_layer {
                        if b.matches(*key, mods) {
                            self.show_layer_preview = !self.show_layer_preview;
                        }
                    }
                    // Same as the layer panel's eye. Said in a toast
                    // too: an empty layer shows no change on screen.
                    if let Some(b) = keys.canvas.toggle_layer_visible {
                        if b.matches(*key, mods) {
                            let c = self.canvases.active_mut();
                            let li = c.active_layer;
                            c.toggle_layer_visible(li);
                            // Numbered from the bottom, as the panel's
                            // hover preview counts them.
                            let msg = if c.layers[li].visible {
                                format!("Layer {} shown", li + 1)
                            } else {
                                format!("Layer {} hidden — {} shows it again", li + 1, b.label())
                            };
                            self.toast = Some((msg, Instant::now()));
                        }
                    }
                }
            }
            // Scroll wheel — count discrete notches per frame.
            //
            // We must NOT use `smooth_scroll_delta` or `zoom_delta`
            // here: both accumulate across frames during egui's
            // smooth-scroll animation, so a single wheel click
            // ends up firing the handler 10-30 times → the user
            // sees one click jump bg_opacity from 0 to 1.
            //
            // `Event::MouseWheel` is fired exactly once per real
            // wheel event by the windowing backend, with the
            // user's actual modifier state attached. Summing the
            // *signs* gives an integer notch count immune to
            // platform-specific unit/magnitude differences (Line
            // vs Point, ±1 vs ±120).
            let mut notches: i32 = 0;
            let mut raw_delta_y: f32 = 0.0;
            for ev in &i.events {
                if let egui::Event::MouseWheel { delta, modifiers, .. } = ev {
                    raw_delta_y += delta.y;
                    if delta.y > 0.0 { notches += 1; }
                    else if delta.y < 0.0 { notches -= 1; }
                    let _ = modifiers; // documented use
                }
            }
            // Also pull egui's accumulated zoom-delta. Trackpad
            // pinches and `Ctrl+wheel` smooth-scroll feed this
            // each frame even when no discrete MouseWheel
            // event fires — including it keeps zoom truly
            // realtime on hi-res input devices.
            let zoom_delta_extra = i.zoom_delta();
            if (zoom_delta_extra - 1.0).abs() > 1e-4 {
                // Convert multiplicative zoom factor into a
                // wheel-delta equivalent matching the 1.10× /
                // unit mapping used inside `handle_scroll`.
                raw_delta_y += zoom_delta_extra.ln() / 1.10_f32.ln();
            }
            // Over the slide panel the wheel scrolls its list instead.
            if (notches != 0 || raw_delta_y.abs() > 1e-4) && !slides.hovered {
                if let Some(l) = self.loupe.as_mut() {
                    if notches != 0 {
                        l.zoom = (l.zoom + notches as f32 * 0.25).clamp(1.5, 12.0);
                    }
                } else if space_held && shift && self.canvases.active_editable() {
                    // Shift+Space + wheel zooms the *active layer*
                    // about the cursor — the wheel counterpart of
                    // the Shift+Space drag that pans it. Same
                    // 1.10×-per-raw-unit feel as the canvas zoom so
                    // one notch is ≈ ±10 % either way.
                    if raw_delta_y.abs() >= 0.001 {
                        let c = self.canvases.active();
                        let cursor = i.pointer.hover_pos().unwrap_or_default();
                        let pivot = [
                            ((cursor.x - canvas_rect.min.x) - c.pan[0]) / c.zoom,
                            ((cursor.y - canvas_rect.min.y) - c.pan[1]) / c.zoom,
                        ];
                        self.apply_layer_zoom(1.10_f32.powf(raw_delta_y), pivot);
                    }
                } else {
                    let cursor = i.pointer.hover_pos().unwrap_or_default();
                    let cursor_screen = [
                        (cursor.x - canvas_rect.min.x).max(0.0),
                        (cursor.y - canvas_rect.min.y).max(0.0),
                    ];
                    interaction::zoom_pan::handle_scroll(
                        notches,
                        raw_delta_y,
                        cursor_screen,
                        ctrl,
                        shift,
                        space_held,
                        &mut self.tool,
                        &mut self.canvases,
                        &mut self.config.bg_opacity,
                    );
                }
            }
        });
        FrameKeys { text_consumed, sel_actions, media_keys, toggle_pins, rename_slide, toggle_lock, delete_slide, key_actions, refused }
    }

    /// Run what `take_keys` collected, and the slide panel's pick, in that order.
    pub(super) fn run_keys(&mut self, ctx: &egui::Context, keys: FrameKeys, slides: ui::slide_panel::Shown, canvas_rect: egui::Rect, media_objs: &[ui::media_controls::OnScreen], media_selected: Option<u64>) {
        let FrameKeys { text_consumed, mut sel_actions, media_keys, toggle_pins, rename_slide, toggle_lock, delete_slide, key_actions, refused } = keys;
        // ---- Select-tool commands -----------------------------------
        // Ctrl+V with only an image on the clipboard produces no egui
        // event at all, so watch the physical chord for that case.
        // When the clipboard holds our own marker text this fires
        // alongside `PasteObjects` and finds no image — harmless.
        let chord = crate::platform::paste_chord_down() && ctx.input(|i| i.focused);
        if chord && !self.paste_chord_prev && !text_consumed {
            sel_actions.push(SelAction::PasteImage);
        }
        self.paste_chord_prev = chord;
        if !sel_actions.is_empty() {
            let ppp = ctx.pixels_per_point();
            for action in sel_actions {
                // Copying changes nothing here; the rest wait for a
                // slide that takes edits.
                if !matches!(action, SelAction::Copy | SelAction::CopyToNewSlide) && !self.may_edit() {
                    continue;
                }
                self.apply_sel_action(action, canvas_rect, ppp);
            }
        }
        for action in key_actions {
            self.apply_key_action(action, ctx);
        }
        if delete_slide {
            self.request_slide_delete();
        }
        if toggle_lock {
            self.toggle_lock();
        }
        if refused {
            self.may_edit();
        }
        if toggle_pins {
            self.set_pins_hidden(!self.canvases.pins_hidden());
        }
        match slides.action {
            Some(ui::slide_panel::SlideAction::GoTo(i)) => self.canvases.go_to(i),
            Some(ui::slide_panel::SlideAction::Rename(i, name)) => self.canvases.rename(i, &name),
            Some(ui::slide_panel::SlideAction::Add) => {
                self.canvases.push_canvas(crate::canvas::canvas::Canvas::default());
            }
            Some(ui::slide_panel::SlideAction::Move(from, to)) => self.move_slide(from, to),
            Some(ui::slide_panel::SlideAction::OpenWindow(i)) => match self.slide_window.as_mut() {
                // One slide window: asking again shows the slide there.
                Some(win) => {
                    win.set_slide(i);
                    ctx.send_viewport_cmd_to(ui::slide_window::SlideWindow::viewport_id(), egui::ViewportCommand::Focus);
                }
                None => self.slide_window = Some(ui::slide_window::SlideWindow::new(i, &self.config)),
            },
            None => {}
        }
        if rename_slide {
            // The name field lives in the panel, so bring it up.
            let i = self.canvases.active_index();
            self.slide_panel.start_rename(i, self.canvases.name(i));
            self.config.slide_panel_open = true;
            self.ui_visible = true;
        }
        if !media_keys.is_empty() {
            let pointer = ctx.input(|i| i.pointer.hover_pos());
            if let Some(id) = ui::media_controls::hotkey_target(media_objs, media_selected, pointer) {
                for cmd in media_keys {
                    self.media.command(id, cmd);
                }
            }
        }
    }
}
