//! UI subsystem: overlay (main canvas), toolbar, status, floating button.
//!
//! Each submodule is one self-contained widget. The top-level `App::update`
//! composes them.

pub mod layer_panel;
pub mod loupe;
pub mod overlay;
pub mod status;
pub mod theme;
pub mod toolbar;
