//! Eraser-tool helpers. The actual deletion happens inside
//! [`crate::canvas::canvas::Canvas::erase_at`] for ergonomic reasons (the
//! canvas owns its data and exposes the mutation), so this module is
//! intentionally tiny.

/// Eraser radius scales with the user's `size` slider. We expose the formula
/// here so a future "thicker eraser than pen" preference can override it.
#[allow(dead_code)]
pub fn radius_for(size: f32) -> f32 {
    // 1.4× the visual pen width — eraser feels noticeably bigger than a pen
    // line of the same nominal size, which matches user expectation from
    // commercial drawing apps.
    size * 1.4
}
