//! Cross-platform path resolution.
//!
//! `directories::ProjectDirs` returns the right OS-specific config / data
//! folder for our app:
//!   * Windows  → `%APPDATA%\com.onscreennotes.OnScreenNotes\`
//!   * macOS    → `~/Library/Application Support/com.onscreennotes.OnScreenNotes/`
//!   * Linux    → `$XDG_CONFIG_HOME/on-screen-notes/`  (defaults to `~/.config/…`)
//!
//! These are the conventions OS vendors expect us to follow.

use crate::error::{anyhow, Result};
use std::path::PathBuf;

/// Returns the directory we store config + canvases in.
///
/// The directory is created if it does not yet exist, so callers can pass the
/// returned path straight to `std::fs::write` etc. without worrying.
pub fn data_dir() -> Result<PathBuf> {
    // ProjectDirs uses a "qualifier.organization.application" tuple to derive
    // a stable path. Pick a unique qualifier so we don't clash with another
    // app of the same name.
    let dirs = directories::ProjectDirs::from("dev", "OnScreenNotes", "on-screen-notes")
        .ok_or_else(|| anyhow!("could not determine user config directory"))?;
    let dir = dirs.data_dir().to_path_buf();
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Path to the AppConfig file inside `data_dir()`.
pub fn config_file() -> Result<PathBuf> {
    Ok(data_dir()?.join("config.ron"))
}

/// Directory under `data_dir()` where individual canvas files live.
pub fn canvases_dir() -> Result<PathBuf> {
    let dir = data_dir()?.join("canvases");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Default screenshot output directory (Pictures/on-screen-notes/).
pub fn default_screenshot_dir() -> Result<PathBuf> {
    let user = directories::UserDirs::new()
        .ok_or_else(|| anyhow!("could not determine user pictures dir"))?;
    let pics = user
        .picture_dir()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::env::temp_dir());
    let dir = pics.join("on-screen-notes");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}
