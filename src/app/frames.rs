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
        (self.pen.is_down() && !self.pen_blocked)
            || self.tool.in_progress_stroke.is_some()
            // Drawing in the slide window: its frames are the main
            // window's (an immediate viewport).
            || self.slide_window.as_ref().is_some_and(|w| w.busy())
            || self.pending_screenshot.is_some()
            || self.pending_freeze.is_some()
            || self.pending_lasso.is_some()
            || self.recorder.is_recording()
    }

    /// Whether frames should come every display refresh: a button is
    /// held or a gesture is under way (a drag, a pan, a selection), the
    /// pen is in range, something on screen changes by itself, or work
    /// in flight shows progress.
    pub(super) fn needs_steady_frames(&self, ctx: &egui::Context) -> bool {
        let canvas = self.canvases.active();
        self.mouse_gesture
            || ctx.input(|i| i.pointer.any_down())
            || self.tool.select.gesture.is_some()
            || self.layer_xform.is_some()
            || self.pan_drag_anchor.is_some()
            || self.middle_pan_anchor.is_some()
            || self.region_select.is_some()
            || self.loupe.is_some()
            // Polls the mouse button itself while the overlay is
            // click-through, so it gets no input events.
            || self.pin_picking
            || self.pen.seen_within(PEN_NEARBY)
            || !self.tool.laser_strokes.is_empty()
            // The lasso's dashes march.
            || !self.tool.lasso.points().is_empty()
            || !self.pureref_jobs.is_empty()
            || !self.media_imports.is_empty()
            || self.canvases.is_loading()
            || self.overlay_cache.pictures_busy()
            || canvas.layers.iter().any(|l| l.visible && l.shapes.iter().any(crate::canvas::shape::Shape::animates))
    }

    /// Hold every frame that is not drawing ink to the display's refresh
    /// rate. Each input event (each mouse move of a drag) makes egui draw
    /// a frame on top of the ones paced here, and with Mailbox
    /// presentation nothing waits for the screen: panning drew 800+ full
    /// frames a second for a 99 Hz display and kept the GPU near 80 %. A
    /// frame that starts with a button or pen press is never held, and
    /// once ink is going down frames are not held either, so drawing
    /// latency is unchanged.
    pub(super) fn hold_to_refresh_rate(&mut self, ctx: &egui::Context) {
        let pressed = ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::PointerButton { pressed: true, .. })));
        if !pressed && !self.needs_live_frames() {
            if let Some(since) = self.last_frame_at.map(|t| t.elapsed()) {
                if since < self.idle_frame {
                    std::thread::sleep(self.idle_frame - since);
                }
            }
        }
        self.last_frame_at = Some(Instant::now());
    }

    /// How long until the next frame is due; `None` means at once. Ink
    /// going down gets every frame it can; gestures, a pen in range and
    /// things that move get one per display refresh; a playing GIF or
    /// video gets its own frame times, never above the refresh rate;
    /// otherwise a slow idle tick.
    pub(super) fn next_frame_in(&self, ctx: &egui::Context) -> Option<Duration> {
        if self.needs_live_frames() {
            return None;
        }
        let refresh = self.idle_frame;
        let mut wait = IDLE_TICK;
        if self.needs_steady_frames(ctx) {
            wait = wait.min(refresh);
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
