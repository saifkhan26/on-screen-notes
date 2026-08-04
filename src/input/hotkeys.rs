//! Global (system-wide) hotkey registration via `global-hotkey`.
//!
//! "Global" = the hotkey fires even when our window is not focused. We use
//! it for the screenshot trigger so the user can capture the current
//! screen without first clicking on our overlay.

use crate::error::Result;
use crate::input::hotkeys_config::{self, GlobalSection};
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
    /// Register the four global hotkeys defined in `HotkeysConfig`.
    /// Each registration is independent — failure on one (e.g. another
    /// app already owns the combo, or the user's binding is malformed)
    /// is logged + skipped, the rest still register.
    pub fn register(global: &GlobalSection) -> Result<Self> {
        let manager = GlobalHotKeyManager::new()?;

        // Helper: translate our friendly string → global-hotkey format,
        // parse, register, return the assigned id (0 on failure).
        let register_one = |label: &str, raw: &str| -> u32 {
            let Some(s) = hotkeys_config::to_global_hotkey_format(raw) else {
                log::warn!("{label} binding '{raw}' has unrecognised token");
                return 0;
            };
            match s.parse::<HotKey>() {
                Ok(hk) => match manager.register(hk) {
                    Ok(()) => hk.id(),
                    Err(e) => {
                        log::warn!("could not register {label} hotkey: {e}");
                        0
                    }
                },
                Err(e) => {
                    log::warn!("{label} hotkey parse error ({s}): {e}");
                    0
                }
            }
        };

        let toggle_id       = register_one("toggle_overlay", &global.toggle_overlay);
        let screenshot_id   = register_one("screenshot",     &global.screenshot);
        let record_id       = register_one("start_record",   &global.start_record);
        let pause_record_id = register_one("pause_record",   &global.pause_record);

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
