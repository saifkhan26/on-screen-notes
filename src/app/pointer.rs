//! A frame's pointer input: dragging to pan, the pen and the mouse handed to the tools,
//! the middle-button pan, and the tool work a pen-up leaves for the end of the frame.

use super::*;
use crate::input::pen::PenEvent;

impl OnScreenNotesApp {
    /// When a drag pans (Space held, or a slide that takes no edits), pan by the pen or the
    /// mouse, Shift moving the active layer instead; otherwise set the cursor the tool wants.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn drag_pan(&mut self, ctx: &egui::Context, pen_events: &[crate::input::pen::PenEvent], client_origin: [i32; 2], drag_pans: bool, space_held: bool, editable: bool, pen_owns_pointer: bool, toolbar_clicked: bool, canvas_rect: egui::Rect) {
        if drag_pans {
            // Holding Shift as well retargets the drag from the
            // viewport to the *active layer*: its ink slides while
            // every other layer stays nailed down. The mode is
            // latched at pointer-down by `begin_pan_gesture`, so
            // this per-frame read only drives the cursor hint.
            let layer_mode = ctx.input(|i| i.modifiers.shift) && editable;
            // Without Space it is the slide's own drag: an open hand
            // until it grabs, and the arrow over the toolbar.
            let over_ui = |p: egui::Pos2| ctx.layer_id_at(p).is_some_and(|l| l.order > egui::Order::Background);
            let hover_ui = ctx.input(|i| i.pointer.hover_pos()).is_some_and(over_ui);
            if layer_mode || self.layer_xform.is_some() {
                ctx.set_cursor_icon(egui::CursorIcon::Move);
            } else if space_held || self.pan_drag_anchor.is_some() {
                ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
            } else if !hover_ui {
                ctx.set_cursor_icon(egui::CursorIcon::Grab);
            }

            use crate::input::pen::PenEvent;
            let mut had_pen = false;
            for ev in pen_events {
                had_pen = true;
                let s = match ev {
                    PenEvent::Down(s) | PenEvent::Move(s) | PenEvent::Up(s) => s,
                };
                // Pen samples are already in window-client logical
                // pixels, same space as `canvas.pan`. No offset needed.
                let here = egui::pos2(s.pos[0], s.pos[1]);
                match ev {
                    PenEvent::Down(_) => {
                        // A tap on the toolbar, or on another app's
                        // window over ours, is not a pan.
                        let elsewhere = {
                            let w = crate::platform::top_level_window_at(client_origin[0] + s.pos[0] as i32, client_origin[1] + s.pos[1] as i32);
                            w != 0 && w != self.osn_hwnd
                        };
                        if over_ui(here) || elsewhere {
                            self.pen_blocked = true;
                            continue;
                        }
                        self.pen_blocked = false;
                        self.begin_pan_gesture(layer_mode);
                        self.pan_drag_anchor = Some(here);
                    }
                    PenEvent::Move(_) => {
                        if self.pen_blocked {
                            continue;
                        }
                        if let Some(anchor) = self.pan_drag_anchor {
                            self.apply_pan_delta(here.x - anchor.x, here.y - anchor.y);
                        }
                        self.pan_drag_anchor = Some(here);
                    }
                    PenEvent::Up(_) => {
                        if std::mem::take(&mut self.pen_blocked) {
                            continue;
                        }
                        // Only a real drag ends here. A wheel-only
                        // layer zoom stays open until Space is
                        // released, so a stray pen tap mid-zoom
                        // doesn't chop the gesture into two undo
                        // steps.
                        if self.pan_drag_anchor.take().is_some() {
                            self.commit_layer_xform();
                        }
                    }
                }
            }

            // Mouse path — only run when no pen events fired this frame
            // so the two sources don't double-add motion. The pointer
            // state is read out of `ctx.input` first and acted on
            // afterwards, so no canvas mutation happens while egui's
            // input lock is held.
            // Without Space, the mouse pans only when the pen is not the
            // one moving it (the tablet driver presses the button too).
            if !had_pen && (space_held || !pen_owns_pointer) {
                let (primary, pressed, cursor) = ctx.input(|i| {
                    (
                        i.pointer.primary_down(),
                        i.pointer.primary_pressed(),
                        i.pointer
                            .hover_pos()
                            .or(i.pointer.interact_pos())
                            .or(i.pointer.latest_pos()),
                    )
                });
                if primary {
                    if let Some(p) = cursor {
                        // Without Space a drag starts only with a press
                        // here on the slide: one on the toolbar, or
                        // held since before, is not a pan.
                        let may_start = space_held || (pressed && !toolbar_clicked && !over_ui(p));
                        match self.pan_drag_anchor {
                            Some(anchor) => self.apply_pan_delta(p.x - anchor.x, p.y - anchor.y),
                            // No anchor yet: the button just went
                            // down, so this is the gesture start.
                            None if may_start => self.begin_pan_gesture(layer_mode),
                            None => {}
                        }
                        if self.pan_drag_anchor.is_some() || may_start {
                            self.pan_drag_anchor = Some(p);
                        }
                    }
                } else if self.pan_drag_anchor.take().is_some() {
                    // Button-up ends a drag; same reasoning as the
                    // pen path — a wheel-only zoom keeps going
                    // until Space is released.
                    self.commit_layer_xform();
                }
            }
        } else {
            // Pan released — drop anchor, restore default cursor.
            self.pan_drag_anchor = None;
            // Letting go of Space mid-gesture still ends it, so bake
            // any pending layer move / zoom rather than losing it.
            // This is also the only end-point a wheel-only layer
            // zoom has.
            self.commit_layer_xform();

            // Hide the OS cursor while the Pen / FreehandArrow tools
            // are active so it doesn't sit on top of the pen tip and
            // distract from the drawn line. Skip the hide when the
            // pointer is over a floating UI layer (toolbar, popup)
            // so the user can still see what they're about to click.
            let tool_hides_cursor = matches!(
                self.tool.kind,
                crate::tools::ToolKind::Pen
                    | crate::tools::ToolKind::FreehandArrow
                    | crate::tools::ToolKind::Laser,
            );
            // The Fill tool needs a visible pointer because the user
            // has to land the stroke's starting point precisely on
            // the edge of the area they want to enclose. Showing the
            // crosshair instead of the regular arrow matches what
            // the user gets while picking a colour with Shift — a
            // "precision aim" cue rather than a generic arrow.
            let tool_wants_crosshair = matches!(
                self.tool.kind,
                crate::tools::ToolKind::Fill
                    | crate::tools::ToolKind::LassoErase
                    | crate::tools::ToolKind::LassoSelect,
            );
            let hover = ctx.input(|i| i.pointer.hover_pos());
            let on_ui = hover
                .and_then(|p| ctx.layer_id_at(p))
                .map(|l| l.order > egui::Order::Background)
                .unwrap_or(false);
            if tool_hides_cursor && !on_ui {
                ctx.set_cursor_icon(egui::CursorIcon::None);
            } else if tool_wants_crosshair && !on_ui {
                ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
            } else if self.tool.kind == crate::tools::ToolKind::Select && !on_ui {
                // Say what a press here would do: grab a handle,
                // move the selection, or pick the object underneath.
                if let Some(p) = hover {
                    let c = self.canvases.active();
                    let local = [
                        ((p.x - canvas_rect.min.x) - c.pan[0]) / c.zoom,
                        ((p.y - canvas_rect.min.y) - c.pan[1]) / c.zoom,
                    ];
                    // Pins sit on top, so they answer first — unless
                    // the slide's own objects are being dragged.
                    let q = [p.x - canvas_rect.min.x, p.y - canvas_rect.min.y];
                    let icon = if self.tool.select.gesture.is_some() {
                        select_cursor(&self.tool.select, local, c)
                    } else {
                        pin_cursor(&self.pin_tool, q, &self.canvases.pinned())
                            .or_else(|| select_cursor(&self.tool.select, local, c))
                    };
                    if let Some(icon) = icon {
                        ctx.set_cursor_icon(icon);
                    }
                }
            }
        }
    }

    /// Hand this frame's pen samples to the active tool (or a pin).
    pub(super) fn dispatch_pen(&mut self, ctx: &egui::Context, pen_events: &[crate::input::pen::PenEvent], client_origin: [i32; 2], drag_pans: bool, editable: bool, canvas_rect: egui::Rect) {
        // ---- Dispatch pen events (window-client → canvas-local) -----
        // Apply the canvas's pan + zoom + the canvas-rect offset to each
        // sample, then dispatch into the active tool. Suppressed when
        // Space is held so a pan gesture doesn't leave ink behind.
        //
        // Also block dispatch when the pen is touching a floating UI
        // layer (toolbar, settings popup, color picker). Wintab does
        // not flow through the egui pointer, so the usual
        // "click-through ignored on widgets" behaviour does not kick
        // in automatically — we have to compare the pen position
        // against `ctx.layer_id_at` ourselves. Block on a UI Down
        // and stay blocked until the next Up so the gesture cannot
        // accidentally start a stroke half-way through tapping a
        // menu item.
        // The tablet driver's synthetic mouse press can reach us a frame
        // before the pen's own packet does. If the mouse path began a
        // gesture for it, drop that now — the pen events below are the
        // real thing, and the stroke must not be drawn twice.
        if !pen_events.is_empty() && self.mouse_gesture {
            self.tool.cancel();
            self.eraser_override.cancel();
            // One tap, not two presses: no double-click.
            self.pin_tool.forget_press();
            self.mouse_gesture = false;
            self.pen_using_eraser = false;
        }
        if !drag_pans && self.loupe.is_none() && self.region_select.is_none() {
            // Mirror the current tool's size onto the eraser
            // override so the temporary erase uses the same brush
            // size as the active pen — feels natural while letting
            // the user keep their pen tool selected.
            self.eraser_override.size  = self.tool.size;
            self.eraser_override.color = self.tool.color;

            let ctrl_held  = ctx.input(|i| i.modifiers.ctrl);
            let shift_held = ctx.input(|i| i.modifiers.shift);
            // In the Select tool Shift means "add to / remove from
            // the selection" (and "snap rotation"), and Ctrl is left
            // for the copy / duplicate chords — so neither the
            // Shift colour-pick nor the Ctrl eraser applies there.
            // The `I` eyedropper still works.
            let select_active = self.tool.kind == crate::tools::ToolKind::Select;
            self.tool.select.shift = shift_held;
            self.tool.select.alt = ctx.input(|i| i.modifiers.alt);

            // Shift-pick: while Shift is held over the canvas, give the
            // user a crosshair so they know the next tap will sample a
            // pixel instead of drawing ink. Toolbar / popups stay
            // visible — `set_cursor_icon` does not affect drawing
            // dispatch on its own, only the OS cursor glyph.
            if shift_held && !select_active && !self.pen_blocked {
                ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
            }

            for ev in pen_events {
                let s = match ev {
                    PenEvent::Down(s) | PenEvent::Move(s) | PenEvent::Up(s) => *s,
                };
                let pen_pos = egui::pos2(s.pos[0], s.pos[1]);
                let on_ui = ctx.layer_id_at(pen_pos)
                    .map(|l| l.order > egui::Order::Background)
                    .unwrap_or(false);
                match ev {
                    PenEvent::Down(_) => {
                        if on_ui {
                            self.pen_blocked = true;
                            continue;
                        }
                        // Wintab runs a *system* context, so it hands
                        // us pen-down packets even when the tip is over
                        // another app's window — OSN is not
                        // always-on-top, so another window may sit
                        // above it.
                        // Without this guard every such tap falls
                        // through to `reclaim_foreground` below and
                        // yanks focus back to OSN: the user clicks a
                        // different app with the pen and OSN keeps
                        // stealing focus. Compare the top-level window
                        // under the pen tip against our own HWND; if
                        // it's a real, different window the user is
                        // drawing/clicking *there*, so swallow the
                        // whole gesture (Move + Up too, via
                        // `pen_blocked`) and don't reclaim.
                        let sx = client_origin[0] + s.pos[0] as i32;
                        let sy = client_origin[1] + s.pos[1] as i32;
                        let win_under = crate::platform::top_level_window_at(sx, sy);
                        if win_under != 0 && win_under != self.osn_hwnd {
                            self.pen_blocked = true;
                            continue;
                        }
                        self.pen_blocked = false;
                        // Pen landed on the canvas — the user wants
                        // to draw in OSN. If another app has the
                        // foreground, reclaim it so keyboard
                        // shortcuts (P, B, T…) keep flowing here.
                        // No-op on non-Win
                        // targets and when OSN is already foreground.
                        crate::platform::reclaim_foreground(self.osn_hwnd);
                        // Shift-pick wins over the eraser modifier:
                        // sample the pixel at the pen tip on Down,
                        // set the pen colour, and lock the rest of
                        // the gesture out of dispatch.
                        if (shift_held && !select_active) || self.tool.eyedropper_pending {
                            self.pen_picking = true;
                            let sx = client_origin[0] + s.pos[0] as i32;
                            let sy = client_origin[1] + s.pos[1] as i32;
                            if let Some(c) = crate::input::picker::pick_screen_color(sx, sy) {
                                self.take_picked_color(c);
                            }
                            // One-shot eyedropper: clear the pending
                            // flag so the next click draws normally.
                            self.tool.eyedropper_pending = false;
                            continue;
                        }
                        self.pen_picking = false;
                        // Lock the dispatch target for the rest of
                        // this gesture: if Ctrl was held at Down,
                        // route every Move + Up to the eraser
                        // override regardless of Ctrl state later.
                        self.pen_using_eraser = ctrl_held && !select_active && editable;
                    }
                    PenEvent::Move(_) => {
                        if self.pen_blocked || self.pen_picking { continue; }
                    }
                    PenEvent::Up(_) => {
                        if self.pen_blocked {
                            self.pen_blocked = false;
                            continue;
                        }
                        if self.pen_picking {
                            self.pen_picking = false;
                            continue;
                        }
                    }
                }
                // A selection let go over the slide window moves there.
                if matches!(ev, PenEvent::Up(_)) && !self.pen_using_eraser {
                    let screen = [client_origin[0] as f32 + s.pos[0], client_origin[1] as f32 + s.pos[1]];
                    if self.drop_on_slide_window(screen, ctx.pixels_per_point()) {
                        continue;
                    }
                }
                // Pins sit over the slide; see `pin_pointer`.
                let q = [s.pos[0] - canvas_rect.min.x, s.pos[1] - canvas_rect.min.y];
                if editable && !self.pen_using_eraser && self.pin_pointer(*ev, q, shift_held, canvas_rect.size()) {
                    continue;
                }
                let dispatched = transform_pen_event(*ev, canvas_rect, self.canvases.active());
                let target_tool: &mut ActiveTool = if self.pen_using_eraser {
                    &mut self.eraser_override
                } else {
                    &mut self.tool
                };
                self.canvases.edit_active(|c| crate::input::pen::dispatch(dispatched, target_tool, c));
                if matches!(ev, PenEvent::Up(_)) {
                    // Gesture done — release the lock so the next
                    // Down can re-evaluate Ctrl state.
                    self.pen_using_eraser = false;
                }
            }
        }
    }

    /// Hand the mouse to the active tool (or a pin) when the pen is not the one driving it.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn dispatch_mouse(&mut self, ctx: &egui::Context, client_origin: [i32; 2], drag_pans: bool, pen_owns_pointer: bool, editable: bool, toolbar_clicked: bool, canvas_rect: egui::Rect) {
        // ---- Mouse ----------------------------------------------------
        // The mouse draws whenever the pen is not the one driving the
        // pointer. A tablet driver moves the system cursor and presses
        // the left button on the pen's behalf, so while the pen is
        // down, or was used a moment ago, those synthetic mouse
        // events must be ignored or every pen stroke would be drawn
        // twice. With the pen out of range — or no tablet at all —
        // the mouse has the canvas to itself.
        if !drag_pans && !pen_owns_pointer
            && self.region_select.is_none() && self.loupe.is_none()
        {
            let (shift_held, ctrl_held, primary_now, cursor) = ctx.input(|i| {
                (
                    i.modifiers.shift,
                    i.modifiers.ctrl || i.modifiers.command,
                    i.pointer.primary_down(),
                    i.pointer.hover_pos().or(i.pointer.interact_pos()),
                )
            });
            // Same rule as the pen path: Shift belongs to the Select
            // tool while it is active.
            let select_active = self.tool.kind == crate::tools::ToolKind::Select;
            self.tool.select.shift = shift_held;
            self.tool.select.alt = ctx.input(|i| i.modifiers.alt);
            // Shift + click = colour pick. Resolve here so the
            // sampling click does not also start a stroke.
            let picking = (shift_held && !select_active) || self.tool.eyedropper_pending;
            if picking {
                ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
            }
            if picking && primary_now && !self.mouse_was_down {
                if let Some(p) = cursor {
                    let ppp = ctx.pixels_per_point();
                    let sx = client_origin[0] + (p.x * ppp) as i32;
                    let sy = client_origin[1] + (p.y * ppp) as i32;
                    if let Some(c) = crate::input::picker::pick_screen_color(sx, sy) {
                        self.take_picked_color(c);
                    }
                }
                // One-shot eyedropper: clear after a successful pick.
                self.tool.eyedropper_pending = false;
                // Mark down so the next frame's still-held click
                // does not trigger a stroke or a second pick.
                self.mouse_was_down = true;
            }
            if !primary_now && !self.mouse_gesture {
                self.mouse_was_down = false;
            }
            if picking {
                // Skip the rest of the mouse-drawing path entirely
                // while picking; otherwise a release would synthesise
                // a pen-up event over the freshly picked colour.
            } else if let Some(p) = cursor {
                let cursor_screen = [
                    (p.x - canvas_rect.min.x).max(0.0),
                    (p.y - canvas_rect.min.y).max(0.0),
                ];
                if let Some(ev) = interaction::mouse_to_pen_event(
                    primary_now,
                    &mut self.mouse_was_down,
                    cursor_screen,
                    self.canvases.active(),
                ) {
                    if matches!(ev, PenEvent::Down(_)) {
                        // A press on the toolbar, the layer panel or a
                        // popup is a click on that widget, not ink on
                        // the canvas underneath it. Decided once at
                        // press; the rest of the drag follows it.
                        let on_ui = toolbar_clicked
                            || ctx
                                .layer_id_at(p)
                                .map(|l| l.order > egui::Order::Background)
                                .unwrap_or(false);
                        self.mouse_gesture = !on_ui;
                        if self.mouse_gesture {
                            // Same reasons as the pen path: keep
                            // keyboard shortcuts flowing here, and
                            // let Ctrl turn this drag into an erase.
                            crate::platform::reclaim_foreground(self.osn_hwnd);
                            self.pen_using_eraser = ctrl_held && !select_active && editable;
                        }
                    }
                    if self.mouse_gesture {
                        // A selection let go over the slide window
                        // moves there.
                        let dropped = matches!(ev, PenEvent::Up(_)) && !self.pen_using_eraser && {
                            let ppp = ctx.pixels_per_point();
                            let screen = [client_origin[0] as f32 + p.x * ppp, client_origin[1] as f32 + p.y * ppp];
                            self.drop_on_slide_window(screen, ppp)
                        };
                        // Pins sit over the slide; see `pin_pointer`.
                        let to_pin = !dropped && editable && !self.pen_using_eraser && self.pin_pointer(ev, cursor_screen, shift_held, canvas_rect.size());
                        if !to_pin && !dropped {
                            let target_tool: &mut ActiveTool = if self.pen_using_eraser {
                                &mut self.eraser_override
                            } else {
                                &mut self.tool
                            };
                            self.canvases.edit_active(|c| crate::input::pen::dispatch(ev, target_tool, c));
                        }
                        if matches!(ev, PenEvent::Up(_)) {
                            self.mouse_gesture = false;
                            self.pen_using_eraser = false;
                        }
                    }
                }
            }
        } else if !self.mouse_gesture {
            // Someone else has the pointer. Follow the button so that
            // when the mouse gets it back, drawing needs a fresh
            // press rather than picking up one that was never its
            // own (the pen's synthetic press, a pan drag…). A mouse
            // drag that is merely paused — Space held mid-stroke —
            // keeps its state so its release still ends the stroke.
            self.mouse_was_down = ctx.input(|i| i.pointer.primary_down());
        }
    }

    /// A middle-button drag pans the view.
    pub(super) fn middle_button_pan(&mut self, ctx: &egui::Context, space_held: bool) {
        // ---- Middle-button pan ----------------------------------------
        // Middle drag pans the viewport without touching the keyboard.
        // Drawing only reads the primary button, so the two never
        // compete. Runs after the tool cursors above so the grab hand
        // wins. Space owns the pan while held; a press on the toolbar
        // or a popup is not a pan.
        let (middle_down, middle_pressed, cursor) = ctx.input(|i| {
            (
                i.pointer.middle_down(),
                i.pointer.button_pressed(egui::PointerButton::Middle),
                i.pointer
                    .hover_pos()
                    .or(i.pointer.interact_pos())
                    .or(i.pointer.latest_pos()),
            )
        });
        if !middle_down || space_held
            || self.loupe.is_some() || self.region_select.is_some()
        {
            self.middle_pan_anchor = None;
        } else if let Some(p) = cursor {
            match self.middle_pan_anchor {
                Some(anchor) => self.apply_pan_delta(p.x - anchor.x, p.y - anchor.y),
                None if middle_pressed => {
                    let on_ui = ctx
                        .layer_id_at(p)
                        .map(|l| l.order > egui::Order::Background)
                        .unwrap_or(false);
                    if !on_ui {
                        self.begin_pan_gesture(false);
                        self.middle_pan_anchor = Some(p);
                    }
                }
                None => {}
            }
            if self.middle_pan_anchor.is_some() {
                self.middle_pan_anchor = Some(p);
                ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
            }
        }
    }

    /// Run the fill and the lasso capture a pen-up asked for, now that the view's size and DPI are known.
    pub(super) fn run_deferred_tools(&mut self, ctx: &egui::Context, canvas_rect: egui::Rect) {
        // ---- Deferred flood-fill --------------------------------------
        // The Fill tool's pen-down arms `tool.pending_fill_at`; we
        // run the actual flood here so it has access to the
        // viewport size + DPI.
        if let Some(p) = self.tool.pending_fill_at.take().filter(|_| self.canvases.active_editable()) {
            let viewport = [canvas_rect.width(), canvas_rect.height()];
            let ppp = ctx.pixels_per_point();
            let result = crate::flood_fill::flood_fill_active(
                self.canvases.active(),
                p,
                self.tool.color,
                viewport,
                ppp,
            );
            match result {
                crate::flood_fill::FillResult::Filled(shape) => {
                    self.canvases.active_mut().add_shape(shape);
                }
                crate::flood_fill::FillResult::OpenRegion => {
                    self.toast = Some((
                        "Fill cancelled: no closed region under cursor".to_string(),
                        std::time::Instant::now(),
                    ));
                }
                crate::flood_fill::FillResult::Empty => {}
            }
        }

        // ---- Deferred lasso-select capture ----------------------------
        // The Lasso-Select tool stashes the closed polygon on pen-up;
        // run the (chrome-hidden) screen capture here where the HWND +
        // DPI are available, then insert the masked region as a bottom
        // image layer.
        if let Some(poly) = self.tool.pending_lasso_capture.take().filter(|_| self.canvases.active_editable()) {
            self.queue_lasso_capture(poly, ctx.pixels_per_point());
        }
    }
}
