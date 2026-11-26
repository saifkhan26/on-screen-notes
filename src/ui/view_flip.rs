//! Flipping the view: the slide shown mirrored left to right and / or
//! upside down, the way paint programs flip the canvas to check a
//! drawing (anatomy, proportions) with fresh eyes.
//!
//! It is the view only: nothing on the slide changes and nothing is
//! saved; it goes back on its own when the slide changes or the app
//! restarts. Everything stays usable while flipped. Drawing works by
//! mirroring twice: the pen and the mouse are mirrored on their way in
//! (`ViewFlip::point`), so tools see the slide as it is, and what the
//! canvas painted is mirrored on its way out (`mirror_layer`), egui's
//! shapes here and the GPU ink in its own shader (`ink_gpu::set_view_flip`).
//! What follows the real pointer (the size ring, the spotlight, the
//! loupe) is drawn on its own layer and left alone.

use egui::epaint::{Mesh, Tessellator};
use egui::{LayerId, Pos2, Rect, Shape};

/// Which ways the view is mirrored, about the middle of the canvas.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ViewFlip {
    /// Left to right (a mirror on the vertical middle line).
    pub x: bool,
    /// Upside down (a mirror on the horizontal middle line).
    pub y: bool,
}

impl ViewFlip {
    pub fn any(self) -> bool {
        self.x || self.y
    }

    /// Screen point `p` mirrored as the view is, about the middle of
    /// `rect` (the canvas). Its own inverse.
    pub fn point(self, rect: Rect, p: Pos2) -> Pos2 {
        let c = rect.center();
        Pos2::new(if self.x { 2.0 * c.x - p.x } else { p.x }, if self.y { 2.0 * c.y - p.y } else { p.y })
    }

    /// A screen box mirrored as the view is.
    pub fn rect(self, rect: Rect, r: Rect) -> Rect {
        if !self.any() || !r.is_finite() {
            return r;
        }
        Rect::from_two_pos(self.point(rect, r.min), self.point(rect, r.max))
    }

    /// A screen move (drag) as it is in the slide's own, unmirrored view.
    pub fn delta(self, d: egui::Vec2) -> egui::Vec2 {
        egui::vec2(if self.x { -d.x } else { d.x }, if self.y { -d.y } else { d.y })
    }

    /// What the view shows, for a notice: `None` when not flipped.
    pub fn describe(self) -> Option<&'static str> {
        match (self.x, self.y) {
            (false, false) => None,
            (true, false) => Some("Mirrored left to right"),
            (false, true) => Some("Upside down"),
            (true, true) => Some("Mirrored and upside down"),
        }
    }
}

/// The shapes painted on `layer` from shape `from` on, mirrored as `flip`
/// says about the middle of `rect`. Text becomes a mirrored mesh, as on
/// a flipped canvas. Paint callbacks (the GPU ink) keep their place: they
/// mirror themselves.
pub fn mirror_layer(ctx: &egui::Context, layer: LayerId, from: usize, flip: ViewFlip, rect: Rect) {
    if !flip.any() {
        return;
    }
    // What tessellating text needs, read before the graphics are locked.
    let ppp = ctx.pixels_per_point();
    let options = ctx.tessellation_options(|o| *o);
    let (tex_size, discs) = ctx.fonts(|f| (f.font_image_size(), f.texture_atlas().lock().prepared_discs()));
    let mut tess = Tessellator::new(ppp, options, tex_size, discs);
    let m = |p: Pos2| flip.point(rect, p);
    ctx.graphics_mut(|g| {
        let list = g.entry(layer);
        let end = list.next_idx().0;
        for i in from..end {
            list.mutate_shape(egui::layers::ShapeIdx(i), |cs| {
                cs.clip_rect = flip.rect(rect, cs.clip_rect);
                mirror_shape(&mut cs.shape, &m, flip, &mut tess);
            });
        }
    });
}

/// `shape` mirrored point by point through `m`.
fn mirror_shape(shape: &mut Shape, m: &impl Fn(Pos2) -> Pos2, flip: ViewFlip, tess: &mut Tessellator) {
    match shape {
        Shape::Noop => {}
        Shape::Vec(shapes) => {
            for s in shapes {
                mirror_shape(s, m, flip, tess);
            }
        }
        Shape::Circle(c) => c.center = m(c.center),
        Shape::Ellipse(e) => e.center = m(e.center),
        Shape::LineSegment { points, .. } => *points = points.map(m),
        Shape::Path(p) => {
            for q in &mut p.points {
                *q = m(*q);
            }
        }
        Shape::Rect(r) => {
            r.rect = Rect::from_two_pos(m(r.rect.min), m(r.rect.max));
            let mut k = r.rounding;
            if flip.x {
                (k.nw, k.ne, k.sw, k.se) = (k.ne, k.nw, k.se, k.sw);
            }
            if flip.y {
                (k.nw, k.ne, k.sw, k.se) = (k.sw, k.se, k.nw, k.ne);
            }
            r.rounding = k;
            // A picture fill shows mirrored too.
            let uv = r.uv;
            r.uv = Rect::from_min_max(
                egui::pos2(if flip.x { uv.max.x } else { uv.min.x }, if flip.y { uv.max.y } else { uv.min.y }),
                egui::pos2(if flip.x { uv.min.x } else { uv.max.x }, if flip.y { uv.min.y } else { uv.max.y }),
            );
        }
        Shape::Text(t) => {
            let mut mesh = Mesh::default();
            tess.tessellate_text(t, &mut mesh);
            mirror_mesh(&mut mesh, m);
            *shape = Shape::Mesh(mesh);
        }
        Shape::Mesh(mesh) => mirror_mesh(mesh, m),
        Shape::QuadraticBezier(b) => b.points = b.points.map(m),
        Shape::CubicBezier(b) => b.points = b.points.map(m),
        Shape::Callback(cb) => cb.rect = Rect::from_two_pos(m(cb.rect.min), m(cb.rect.max)),
    }
}

fn mirror_mesh(mesh: &mut Mesh, m: &impl Fn(Pos2) -> Pos2) {
    for v in &mut mesh.vertices {
        v.pos = m(v.pos);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirroring_is_about_the_middle_and_undoes_itself() {
        let rect = Rect::from_min_max(egui::pos2(100.0, 50.0), egui::pos2(300.0, 250.0));
        let both = ViewFlip { x: true, y: true };
        let p = egui::pos2(120.0, 60.0);
        assert_eq!(ViewFlip { x: true, y: false }.point(rect, p), egui::pos2(280.0, 60.0));
        assert_eq!(ViewFlip { x: false, y: true }.point(rect, p), egui::pos2(120.0, 240.0));
        assert_eq!(both.point(rect, both.point(rect, p)), p);
        assert_eq!(ViewFlip::default().point(rect, p), p);
        assert_eq!(both.rect(rect, Rect::from_min_max(p, egui::pos2(130.0, 80.0))), Rect::from_min_max(egui::pos2(270.0, 220.0), egui::pos2(280.0, 240.0)));
        assert_eq!(ViewFlip { x: true, y: false }.delta(egui::vec2(3.0, 4.0)), egui::vec2(-3.0, 4.0));
    }

    /// Shapes painted while flipped land mirrored; text turns into a
    /// mirrored mesh; what was painted before is left alone.
    #[test]
    fn a_layers_shapes_are_mirrored_from_where_it_says() {
        let ctx = egui::Context::default();
        let rect = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(200.0, 100.0));
        let layer = LayerId::background();
        let _ = ctx.run(Default::default(), |ctx| {
            let painter = ctx.layer_painter(layer);
            painter.circle_filled(egui::pos2(10.0, 10.0), 3.0, egui::Color32::RED);
            let from = ctx.graphics(|g| g.get(layer).map_or(0, |l| l.next_idx().0));
            painter.circle_filled(egui::pos2(10.0, 10.0), 3.0, egui::Color32::RED);
            painter.text(egui::pos2(20.0, 20.0), egui::Align2::LEFT_TOP, "Hi", egui::FontId::proportional(14.0), egui::Color32::WHITE);
            mirror_layer(ctx, layer, from, ViewFlip { x: true, y: false }, rect);
            ctx.graphics(|g| {
                let shapes: Vec<&Shape> = g.get(layer).unwrap().all_entries().map(|c| &c.shape).collect();
                assert!(matches!(shapes[0], Shape::Circle(c) if c.center == egui::pos2(10.0, 10.0)), "before: kept");
                assert!(matches!(shapes[1], Shape::Circle(c) if c.center == egui::pos2(190.0, 10.0)), "mirrored");
                let Shape::Mesh(text) = shapes[2] else { panic!("text became a mesh: {:?}", shapes[2]) };
                assert!(!text.vertices.is_empty());
                assert!(text.vertices.iter().all(|v| v.pos.x <= 180.0 + 1e-3 && v.pos.x > 100.0), "right of the middle, mirrored");
            });
        });
    }
}
