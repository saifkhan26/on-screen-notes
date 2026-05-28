//! Helpers for tracking modifier-key state edges (changed-this-frame).
//!
//! egui exposes the current modifier set every frame; we want to *react* to
//! the moment Alt becomes held or released to send `MousePassthrough`
//! commands exactly once per state change (the OS doesn't like spam).

#[derive(Debug, Clone, Copy, Default)]
pub struct ModifierTracker {
    pub alt_held: bool,
}

impl ModifierTracker {
    /// Returns `Some(new_state)` only when Alt's state changed since the
    /// last call. Otherwise `None`. This is a classic "edge-triggered" guard.
    pub fn alt_edge(&mut self, alt_now: bool) -> Option<bool> {
        if alt_now != self.alt_held {
            self.alt_held = alt_now;
            Some(alt_now)
        } else {
            None
        }
    }
}
