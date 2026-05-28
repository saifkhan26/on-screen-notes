//! Pen tool helpers.
//!
//! All the per-event behaviour for the pen lives in `tools::ActiveTool` —
//! this file holds standalone helpers we may want to reuse (e.g. resampling
//! a stroke, decimating duplicate samples).

use crate::canvas::stroke::{PenSample, Stroke};

/// Returns true if `b` is at most `epsilon` pixels from `a`. We use this to
/// drop duplicate samples a tablet driver sometimes emits at high frequency,
/// which would otherwise bloat saved files for no visual gain.
#[allow(dead_code)]
pub fn close_enough(a: PenSample, b: PenSample, epsilon: f32) -> bool {
    let dx = a.pos[0] - b.pos[0];
    let dy = a.pos[1] - b.pos[1];
    (dx * dx + dy * dy) <= epsilon * epsilon
}

/// Push a sample into a stroke, skipping near-duplicates. Used by the
/// pen tool's `update` path.
#[allow(dead_code)]
pub fn push_dedup(stroke: &mut Stroke, sample: PenSample, epsilon: f32) {
    if let Some(last) = stroke.samples.last() {
        if close_enough(*last, sample, epsilon) {
            return;
        }
    }
    stroke.push(sample);
}
