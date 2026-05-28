//! Shape tool helpers (Rect / Ellipse). Most of the action is already in
//! [`crate::tools::ActiveTool::end`], which constructs the right `Shape`
//! variant from the two stored corners. This file exists so the module
//! tree mirrors the plan's directory layout and so future features (e.g.
//! "hold Shift to constrain to a square") have an obvious home.

/// Snap a free corner `b` to a perfect square / circle relative to corner
/// `a` by extending the dominant axis. Call from a "constraint modifier
/// held" branch in the future. Currently unused but documented for clarity.
#[allow(dead_code)]
pub fn snap_to_uniform(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    let dx = b[0] - a[0];
    let dy = b[1] - a[1];
    let m  = dx.abs().max(dy.abs());
    [a[0] + m * dx.signum(), a[1] + m * dy.signum()]
}
