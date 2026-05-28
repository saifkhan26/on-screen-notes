//! Global (system-wide) hotkey registration via `global-hotkey`.
//!
//! "Global" = the hotkey fires even when our window is not focused. We use
//! it for the screenshot trigger so the user can capture the current
//! screen without first clicking on our overlay.

use crate::config::AppConfig;
use crate::error::Result;
use global_hotkey::{
    hotkey::HotKey, GlobalHotKeyEvent, GlobalHotKeyManager,
};

/// Identity tags so we can tell *which* hotkey fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyId {
    ToggleOverlay,
    Screenshot,
    /// Start / stop screen recording. Single combo toggles state.
    ToggleRecord,
    /// Pause / resume an in-flight recording. No-op when idle.
    TogglePauseRecord,
}

pub struct Hotkeys {
    /// `_manager` is held to keep the registrations alive. The crate's
    /// global event channel is the actual delivery mechanism.
    _manager: GlobalHotKeyManager,
    toggle_id: u32,
    screenshot_id: u32,
    record_id: u32,
    pause_record_id: u32,
}

impl Hotkeys {
    /// Try to register both hotkeys. Failure on any single one is logged but
    /// not fatal: the rest of the app still runs, the user just doesn't get
    /// that particular shortcut. This makes the app robust to other
    /// applications already grabbing the same combo.
    pub fn register(cfg: &AppConfig) -> Result<Self> {
        let manager = GlobalHotKeyManager::new()?;

        // Try parsing then registering each hotkey. Both errors are non-fatal.
        let mut toggle_id = 0;
        match cfg.hotkey_toggle_overlay.parse::<HotKey>() {
            Ok(hk) => match manager.register(hk) {
                Ok(()) => toggle_id = hk.id(),
                Err(e) => log::warn!("could not register toggle hotkey: {e}"),
            },
            Err(e) => log::warn!("toggle hotkey parse error: {e}"),
        }

        let mut screenshot_id = 0;
        match cfg.hotkey_screenshot.parse::<HotKey>() {
            Ok(hk) => match manager.register(hk) {
                Ok(()) => screenshot_id = hk.id(),
                Err(e) => log::warn!("could not register screenshot hotkey: {e}"),
            },
            Err(e) => log::warn!("screenshot hotkey parse error: {e}"),
        }

        // Record toggle is hard-coded for now — no config field yet.
        // `ctrl+shift+KeyR` matches the format the other entries use
        // (parsed by `global_hotkey`).
        let mut record_id = 0;
        match "ctrl+shift+KeyR".parse::<HotKey>() {
            Ok(hk) => match manager.register(hk) {
                Ok(()) => record_id = hk.id(),
                Err(e) => log::warn!("could not register record hotkey: {e}"),
            },
            Err(e) => log::warn!("record hotkey parse error: {e}"),
        }

        // Pause / resume the in-flight recording. Distinct from
        // start/stop so the user can do both without modifier
        // gymnastics. Ctrl+Shift+P is free in the existing table.
        let mut pause_record_id = 0;
        match "ctrl+shift+KeyP".parse::<HotKey>() {
            Ok(hk) => match manager.register(hk) {
                Ok(()) => pause_record_id = hk.id(),
                Err(e) => log::warn!("could not register pause-record hotkey: {e}"),
            },
            Err(e) => log::warn!("pause-record hotkey parse error: {e}"),
        }

        Ok(Self { _manager: manager, toggle_id, screenshot_id, record_id, pause_record_id })
    }

    /// Drain any hotkey events that fired since last call. Use the global
    /// receiver provided by the crate.
    pub fn poll(&self) -> Vec<HotkeyId> {
        let mut out = Vec::new();
        let recv = GlobalHotKeyEvent::receiver();
        while let Ok(event) = recv.try_recv() {
            // Ignore "release" events — we only act on press to avoid
            // double-firing.
            if event.state != global_hotkey::HotKeyState::Pressed {
                continue;
            }
            let id = event.id;
            if id == self.toggle_id {
                out.push(HotkeyId::ToggleOverlay);
            } else if id == self.screenshot_id {
                out.push(HotkeyId::Screenshot);
            } else if id == self.record_id {
                out.push(HotkeyId::ToggleRecord);
            } else if id == self.pause_record_id {
                out.push(HotkeyId::TogglePauseRecord);
            }
        }
        out
    }
}
