//! Frame pacing: when the next frame is wanted, and holding drags to the display's rate.

use super::*;

impl OnScreenNotesApp {
    /// Whether frames should come as fast as they can be drawn, above the
    /// display's refresh rate: only while ink is going down (pen-to-pixel
    /// latency is what this app is for) and while a capture or recording
    /// counts frames.
    pub(super) fn needs_live_frames(&self) -> bool {
        // A pen gesture in another app's window is swallowed
        // (`pen_blocked`); it needs no frames here, let alone uncapped
        // ones while the user draws elsewhere.
        (self.pen.is_down() && !self.ws.pen_blocked)
            || self.ws.tool.in_progress_stroke.is_some()
            // Drawing in the slide window: its frames are the main
            // window's (an immediate viewport).
            || self.slide_window.as_ref().is_some_and(|w| w.busy())
            || self.window_ws.as_ref().is_some_and(|w| w.tool.in_progress_stroke.is_some() || w.tool.select.gesture.is_some())
            || self.pins_window.as_ref().is_some_and(|w| w.busy())
            || self.pins_ws.as_ref().is_some_and(|w| w.tool.in_progress_stroke.is_some() || w.tool.select.gesture.is_some() || w.pin_tool.busy())
            || self.pending_screenshot.is_some()
            || self.pending_freeze.is_some()
            || self.pending_lasso.is_some()
            || self.recorder.is_recording()
    }

    /// Whether frames should come every display refresh: a button is
    /// held or a gesture is under way (a drag, a pan, a selection), the
    /// pen is near (`pen_near`), something on screen changes by itself,
    /// or work in flight shows progress.
    pub(super) fn needs_steady_frames(&self, ctx: &egui::Context) -> bool {
        self.ws.mouse_gesture
            || ctx.input(|i| i.pointer.any_down())
            || self.ws.tool.select.gesture.is_some()
            || self.ws.layer_xform.is_some()
            || self.ws.pan_drag_anchor.is_some()
            || self.ws.middle_pan_anchor.is_some()
            || self.region_select.is_some()
            || self.loupe.is_some()
            // Polls the mouse button itself while the overlay is
            // click-through, so it gets no input events.
            || self.pin_picking
            || self.pen_near()
            || !self.ws.tool.laser_strokes.is_empty()
            // The lasso's dashes march.
            || !self.ws.tool.lasso.points().is_empty()
            || !self.pureref_jobs.is_empty()
            || !self.media_imports.is_empty()
            || self.canvases.is_loading()
            || self.overlay_cache.pictures_busy()
    }

    /// Whether the pen was over one of this app's windows within
    /// `PEN_NEARBY`. Wintab hands over the pen's packets wherever it is,
    /// so going by any packet drew every refresh, the whole window each
    /// time, while the pen was used in another app.
    pub(super) fn pen_near(&self) -> bool {
        self.pen_near_at.is_some_and(|t| t.elapsed() <= PEN_NEARBY)
    }

    /// Look at where the pen's newest packet landed, once per packet: over
    /// one of this app's windows (not covered there by another app's), the
    /// pen is near. `origin` is the client origin, in screen pixels, of
    /// the `poll` that read it.
    pub(super) fn note_pen_place(&mut self, origin: [i32; 2]) {
        let Some((at, pos)) = self.pen.last_packet() else { return };
        if self.pen_place_read == Some(at) {
            return;
        }
        self.pen_place_read = Some(at);
        let (x, y) = ((pos[0] + origin[0] as f32).round() as i32, (pos[1] + origin[1] as f32).round() as i32);
        let ours = crate::platform::is_own_window(crate::platform::top_level_window_at(x, y));
        log::trace!("pen packet at ({x}, {y}) on screen: {}", if ours { "over this app" } else { "elsewhere" });
        if ours {
            self.pen_near_at = Some(at);
        }
    }

    /// Whether a marching-ants border shows on the overlay's canvas.
    fn dashes_march(&self, ctx: &egui::Context) -> bool {
        self.overlay_visible && marching_in_view(self.canvases.active(), self.overlay_cache.canvas_rect(ctx))
    }

    /// Hold every frame that is not drawing ink to the display's refresh
    /// rate. Each input event (each mouse move of a drag) makes egui draw
    /// a frame on top of the ones paced here, and with Mailbox
    /// presentation nothing waits for the screen: panning drew 800+ full
    /// frames a second for a 99 Hz display and kept the GPU near 80 %. A
    /// frame that starts with a button or pen press is never held, and
    /// once ink is going down frames are not held either, so drawing
    /// latency is unchanged.
    ///
    /// Wintab wakes nothing, so a pen touching down mid-wait used to sit
    /// unread for the rest of it, up to a whole refresh before its first
    /// ink. With the pen in range the wait is taken in short steps, the
    /// pen read between them (kept for this frame, `early_pen`), and a
    /// touch-down ends it.
    pub(super) fn hold_to_refresh_rate(&mut self, ctx: &egui::Context) {
        let pressed = ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::PointerButton { pressed: true, .. })));
        if !pressed && !self.needs_live_frames() {
            if let Some(due) = self.last_frame_at.map(|t| t + self.idle_frame) {
                if self.pen_near() {
                    let origin = self.client_origin(ctx);
                    loop {
                        let now = Instant::now();
                        if now >= due {
                            break;
                        }
                        std::thread::sleep((due - now).min(PEN_WATCH_STEP));
                        let events = self.pen.poll(origin);
                        let down = events.iter().any(|e| matches!(e, crate::input::pen::PenEvent::Down(_)));
                        self.early_pen.extend(events);
                        if down {
                            break;
                        }
                    }
                } else if let Some(wait) = due.checked_duration_since(Instant::now()) {
                    std::thread::sleep(wait);
                }
            }
        }
        self.last_frame_at = Some(Instant::now());
    }

    /// How long until the next frame is due; `None` means at once. Ink
    /// going down gets every frame it can; gestures, a pen in range and
    /// things that move get one per display refresh; a playing GIF or
    /// video gets its own frame times, never above the refresh rate;
    /// marching dashes on screen get `MARCH_FRAME`; otherwise a slow
    /// idle tick.
    pub(super) fn next_frame_in(&self, ctx: &egui::Context) -> Option<Duration> {
        if self.needs_live_frames() {
            return None;
        }
        let refresh = self.idle_frame;
        let mut wait = IDLE_TICK;
        if self.needs_steady_frames(ctx) {
            wait = wait.min(refresh);
        }
        if self.dashes_march(ctx) {
            wait = wait.min(MARCH_FRAME.max(refresh));
        }
        if let Some(media) = self.media.next_frame_in() {
            wait = wait.min(media.max(refresh));
        }
        Some(wait)
    }

    /// Ask for the next frame: at once, or when `next_frame_in` says.
    pub(super) fn request_next_frame(&self, ctx: &egui::Context) {
        // Always request the next frame: pen events come from a polled
        // Wintab queue and global hotkeys are polled too, so frames have
        // to keep coming even when nothing on screen changes.
        //   * While the pen is in range or anything is in motion, at
        //     once. `request_repaint()` with no delay is what minimises
        //     pen-to-pixel latency — with Mailbox present mode each
        //     refresh shows the newest frame.
        //   * Otherwise as `next_frame_in` paces them: per refresh while
        //     the pen is in range or something moves, at a GIF's or
        //     video's own frame times, else a slow idle tick. Uncapped,
        //     an idle overlay drew 1000+ fps; at the refresh rate it
        //     still kept the GPU a quarter busy redrawing an unchanged
        //     window. The slow pace still drains Wintab well before a pen
        //     coming into range touches down (its driver moves the mouse
        //     too, which wakes egui at once), runs toasts and other
        //     timers, and input events still repaint immediately.
        match self.next_frame_in(ctx) {
            None => ctx.request_repaint(),
            Some(wait) => {
                // egui takes `predicted_dt` (1/60 s unless set) off every
                // delay to aim for the frame *before* the deadline, which
                // turns a one-refresh delay into no delay at all. Add it
                // back.
                let predicted = Duration::try_from_secs_f32(ctx.input(|i| i.predicted_dt)).unwrap_or_default();
                ctx.request_repaint_after(wait + predicted);
            }
        }
    }
}

/// Frame interval while marching-ants borders show and nothing else
/// moves: about 30 a second. The dashes march 30 canvas pixels a second,
/// so this still moves them a pixel a frame at 100 %. Each frame redraws
/// the whole window: at the display's rate (99 Hz here) one dashed shape
/// kept the GPU a quarter busy, invisible or off screen alike.
const MARCH_FRAME: Duration = Duration::from_millis(33);

/// Whether `canvas` has a marching-ants border, on a shown layer, with
/// any of it inside `view` (the canvas area, where `canvas`'s pan and
/// zoom place it).
fn marching_in_view(canvas: &crate::canvas::canvas::Canvas, view: egui::Rect) -> bool {
    let (pan, zoom) = (canvas.pan, canvas.zoom);
    let at = |x: f32, y: f32| egui::pos2(view.min.x + pan[0] + x * zoom, view.min.y + pan[1] + y * zoom);
    canvas.layers.iter().any(|l| {
        l.visible
            && l.shapes.iter().any(|s| {
                s.animates() && {
                    let b = crate::canvas::canvas::shape_bounds(s);
                    egui::Rect::from_two_pos(at(b[0], b[1]), at(b[2], b[3])).intersects(view)
                }
            })
    })
}

#[cfg(test)]
mod march_tests {
    use super::*;
    use crate::canvas::canvas::Canvas;
    use crate::canvas::shape::{BorderStyle, Shape};

    fn ellipse(a: [f32; 2], b: [f32; 2], border: BorderStyle) -> Shape {
        Shape::Ellipse { a, b, color: [255; 4], stroke_width: 4.0, border, filled: false, rotation: 0.0, pin: 0, group: 0, tags: Vec::new(), turn: 0.0 }
    }

    /// Only a dashed shape that shows keeps frames coming: not a solid
    /// one, not one with no size, not one panned out of view or on a
    /// hidden layer.
    #[test]
    fn only_dashes_that_show_march() {
        let view = egui::Rect::from_min_size(egui::pos2(0.0, 40.0), egui::vec2(800.0, 600.0));
        let with = |s: Shape| {
            let mut c = Canvas::default();
            c.add_shape(s);
            c
        };
        let dashed = with(ellipse([100.0, 100.0], [200.0, 160.0], BorderStyle::DashedAnimated));
        assert!(marching_in_view(&dashed, view));
        assert!(!marching_in_view(&with(ellipse([100.0, 100.0], [200.0, 160.0], BorderStyle::Solid)), view));
        // The one found on a real slide: an ellipse from a click.
        assert!(!marching_in_view(&with(ellipse([382.5, 779.0], [382.5, 779.0], BorderStyle::DashedAnimated)), view));
        let mut away = with(ellipse([100.0, 100.0], [200.0, 160.0], BorderStyle::DashedAnimated));
        away.pan = [-5000.0, 0.0];
        assert!(!marching_in_view(&away, view), "panned out of view");
        away.pan = [0.0, 0.0];
        away.zoom = 2.0;
        assert!(marching_in_view(&away, view), "zoomed, still in view");
        let mut hidden = with(ellipse([100.0, 100.0], [200.0, 160.0], BorderStyle::DashedAnimated));
        for l in &mut hidden.layers {
            l.visible = false;
        }
        assert!(!marching_in_view(&hidden, view), "on a hidden layer");
    }
}
