//! 2-D affine transform in canvas-local pixels.
//!
//! The layer gestures only ever need a similarity (`p → a · p + b`, see
//! `Layer::transform`). Per-object editing also rotates and stretches one
//! axis at a time, which needs the full 2×3 form, so it lives here.

/// `x' = a·x + c·y + tx`, `y' = b·x + d·y + ty`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Affine {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub tx: f32,
    pub ty: f32,
}

impl Affine {
    pub const IDENTITY: Affine = Affine { a: 1.0, b: 0.0, c: 0.0, d: 1.0, tx: 0.0, ty: 0.0 };

    pub fn translate(d: [f32; 2]) -> Self {
        Affine { tx: d[0], ty: d[1], ..Self::IDENTITY }
    }

    /// Rotate by `angle` radians about `pivot`. Canvas y grows downward,
    /// so a positive angle turns clockwise on screen — the same sense as
    /// `epaint::TextShape::angle`.
    pub fn rotate_about(pivot: [f32; 2], angle: f32) -> Self {
        let (s, c) = angle.sin_cos();
        Affine {
            a: c,
            b: s,
            c: -s,
            d: c,
            tx: pivot[0] - c * pivot[0] + s * pivot[1],
            ty: pivot[1] - s * pivot[0] - c * pivot[1],
        }
    }

    /// Scale by `(sx, sy)` along the canvas axes, keeping `pivot` fixed.
    pub fn scale_about(pivot: [f32; 2], sx: f32, sy: f32) -> Self {
        Affine {
            a: sx,
            b: 0.0,
            c: 0.0,
            d: sy,
            tx: (1.0 - sx) * pivot[0],
            ty: (1.0 - sy) * pivot[1],
        }
    }

    /// The transform that applies `self` first and `next` second.
    pub fn then(self, next: Affine) -> Affine {
        Affine {
            a: next.a * self.a + next.c * self.b,
            b: next.b * self.a + next.d * self.b,
            c: next.a * self.c + next.c * self.d,
            d: next.b * self.c + next.d * self.d,
            tx: next.a * self.tx + next.c * self.ty + next.tx,
            ty: next.b * self.tx + next.d * self.ty + next.ty,
        }
    }

    pub fn apply(&self, p: [f32; 2]) -> [f32; 2] {
        [
            self.a * p[0] + self.c * p[1] + self.tx,
            self.b * p[0] + self.d * p[1] + self.ty,
        ]
    }

    /// Apply only the linear part — for directions and extents, which
    /// must not pick up the translation.
    pub fn apply_vec(&self, v: [f32; 2]) -> [f32; 2] {
        [self.a * v[0] + self.c * v[1], self.b * v[0] + self.d * v[1]]
    }

    /// Factor by which lengths with no direction of their own (stroke
    /// widths, font sizes) should scale: the square root of the area
    /// scale. Equals the plain scale factor for a similarity.
    pub fn linear_scale(&self) -> f32 {
        (self.a * self.d - self.b * self.c).abs().sqrt()
    }

    pub fn is_identity(&self) -> bool {
        const E: f32 = 1e-6;
        (self.a - 1.0).abs() < E
            && self.b.abs() < E
            && self.c.abs() < E
            && (self.d - 1.0).abs() < E
            && self.tx.abs() < E
            && self.ty.abs() < E
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(p: [f32; 2], q: [f32; 2]) -> bool {
        (p[0] - q[0]).abs() < 1e-3 && (p[1] - q[1]).abs() < 1e-3
    }

    #[test]
    fn rotate_about_keeps_the_pivot_and_turns_clockwise_on_screen() {
        let m = Affine::rotate_about([10.0, 10.0], std::f32::consts::FRAC_PI_2);
        assert!(close(m.apply([10.0, 10.0]), [10.0, 10.0]));
        // A point to the right of the pivot swings to below it (y down).
        assert!(close(m.apply([15.0, 10.0]), [10.0, 15.0]));
    }

    #[test]
    fn scale_about_keeps_the_pivot() {
        let m = Affine::scale_about([4.0, -2.0], 3.0, 0.5);
        assert!(close(m.apply([4.0, -2.0]), [4.0, -2.0]));
        assert!(close(m.apply([6.0, 2.0]), [10.0, 0.0]));
        assert!((m.linear_scale() - 1.5_f32.sqrt()).abs() < 1e-4);
    }

    #[test]
    fn then_applies_left_to_right() {
        let m = Affine::translate([5.0, 0.0]).then(Affine::scale_about([0.0, 0.0], 2.0, 2.0));
        assert!(close(m.apply([1.0, 1.0]), [12.0, 2.0]));
    }

    #[test]
    fn identity_check() {
        assert!(Affine::IDENTITY.is_identity());
        assert!(Affine::translate([0.0, 0.0]).is_identity());
        assert!(!Affine::translate([0.5, 0.0]).is_identity());
    }
}
