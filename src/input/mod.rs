//! Input subsystem: pen, global hotkeys, modifier-edge detection.
//!
//! The submodules are intentionally narrow:
//!   * [`pen`]       — pen tablet bridge (octotablet) + `PenEvent` enum.
//!   * [`hotkeys`]   — global system-wide shortcuts.
//!   * [`modifiers`] — track edges of the Alt key (and any other modifier
//!                     we add later).

pub mod filter;
pub mod hotkeys;
pub mod hotkeys_config;
pub mod modifiers;
pub mod pen;
pub mod picker;
