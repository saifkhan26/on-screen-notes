//! The Shift+Space layer gesture: panning and zooming one layer, previewed, then baked in.

use super::*;

impl OnScreenNotesApp {
    /// Begin a Space-drag. `layer_mode` is whether Shift was down at the
    /// instant the pointer went down; latching it here (rather than
    /// re-reading it every frame) means the gesture keeps doing what it
    /// started doing even if the user's fingers slip off Shift halfway
    /// through a drag.
    pub(super) fn begin_pan_gesture(&mut self, layer_mode: bool) {
        if !layer_mode {
            // A plain Space drag pans the viewport. Anything the user
            // already aimed at the layer (a wheel zoom before touching
            // the pen) is a finished gesture — bake it rather than
            // dropping it on the floor.
            self.commit_layer_xform();
            return;
        }
        let canvas = self.canvases.active_index();
        let layer = self.canvases.active().active_layer;
        match self.layer_xform {
            // Wheel-zoomed first, now dragging: same target, so keep
            // accumulating into the gesture already in flight instead
            // of throwing its scale away.
            Some(x) if x.canvas == canvas && x.layer == layer => {}
            Some(_) => {
                self.commit_layer_xform();
                self.layer_xform = Some(LayerXform::new(canvas, layer));
            }
            None => self.layer_xform = Some(LayerXform::new(canvas, layer)),
        }
    }

    /// Apply one frame of drag motion, given in window-client pixels.
    /// Routes to the layer transform or the canvas viewport depending on
    /// which gesture is in flight.
    pub(super) fn apply_pan_delta(&mut self, dx: f32, dy: f32) {
        let zoom = self.canvases.active().zoom.max(1e-6);
        if let Some(x) = self.layer_xform.as_mut() {
            // A layer delta ends up added to canvas-local geometry, so
            // the view zoom has to come back out of it — otherwise the
            // layer would race ahead of the cursor at zoom > 1. Canvas
            // `pan` needs no such division because it is applied
            // *after* the zoom scale.
            x.pan([dx / zoom, dy / zoom]);
        } else {
            let (pan, _) = self.canvases.active_view_mut();
            pan[0] += dx;
            pan[1] += dy;
        }
    }

    /// Zoom the gesture's layer by `s` about `pivot` (canvas-local),
    /// starting a gesture on the active layer if the wheel arrived
    /// before any drag. The accumulated scale is clamped so the preview
    /// can't run away past what `transform_layer` will accept.
    pub(super) fn apply_layer_zoom(&mut self, s: f32, pivot: [f32; 2]) {
        if self.layer_xform.is_none() {
            let canvas = self.canvases.active_index();
            let layer = self.canvases.active().active_layer;
            self.layer_xform = Some(LayerXform::new(canvas, layer));
        }
        let Some(x) = self.layer_xform.as_mut() else { return };
        // Clamp against the layer's *total* scale, not just this
        // gesture's, so ten gestures of 2× stop at the same ceiling one
        // long gesture would.
        let already = self
            .canvases
            .active()
            .layers
            .get(x.layer)
            .map(|l| l.scaled_by)
            .unwrap_or(1.0)
            .max(1e-6);
        let want = (already * x.scale * s).clamp(LAYER_ZOOM_MIN, LAYER_ZOOM_MAX);
        let s = want / (already * x.scale).max(1e-6);
        if (s - 1.0).abs() < 1e-6 { return; }
        x.zoom(s, pivot);
    }

    /// End a layer gesture, baking the accumulated transform into the
    /// layer's geometry. No-op unless a gesture is actually in flight,
    /// so it is safe to call from every gesture-end path.
    pub(super) fn commit_layer_xform(&mut self) {
        let Some(x) = self.layer_xform.take() else { return };
        // Canvas nav (←/→) is not gated on Space, so the user can walk
        // off the sheet mid-gesture. The preview was showing on a
        // canvas they have since left — drop the transform rather than
        // bake it into whichever sheet happens to be active now.
        if x.canvas != self.canvases.active_index() { return; }
        if (x.scale - 1.0).abs() < 1e-6 {
            // Pure pan: keep it in the history as a `LayerMoved` so
            // "reset layer position" and the existing undo path see
            // exactly what they saw before layer zoom existed.
            self.canvases.active_mut().move_layer_by(x.layer, x.offset);
        } else {
            self.canvases.active_mut().transform_layer(x.layer, x.scale, x.offset);
        }
    }

    /// The in-flight layer gesture in the form the renderer wants, or
    /// `None` when there is no gesture or it belongs to another canvas.
    pub(super) fn layer_xform_for_render(&self) -> Option<(usize, f32, [f32; 2])> {
        self.layer_xform
            .filter(|x| x.canvas == self.canvases.active_index())
            .map(|x| (x.layer, x.scale, x.offset))
    }
}
