//! Input subsystem: pen, global hotkeys.
//!
//! The submodules are intentionally narrow:
//!   * [`pen`]       — pen tablet bridge (octotablet) + `PenEvent` enum.
//!   * [`hotkeys`]   — global system-wide shortcuts.

pub mod filter;
pub mod hotkeys;
pub mod hotkeys_config;
pub mod pen;
pub mod picker;
