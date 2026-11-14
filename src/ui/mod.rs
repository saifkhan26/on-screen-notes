//! UI subsystem: overlay (main canvas), toolbar, status, floating button.
//!
//! Each submodule is one self-contained widget. The top-level `App::update`
//! composes them.

pub mod arrange_bar;
pub mod command_palette;
pub mod find_bar;
pub mod layer_panel;
pub mod loupe;
pub mod media_controls;
pub mod media_filter;
pub mod overlay;
pub mod picture_cache;
pub mod slide_panel;
pub mod slide_edit;
pub mod slide_window;
pub mod theme;
pub mod toolbar;
