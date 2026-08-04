//! Hand-editable hotkeys file (`hotkeys.toml`).
//!
//! Defaults live in `HotkeysConfig::default`. Each field is wrapped in
//! `#[serde(default = "...")]` so users can delete any line they don't
//! want to override — missing fields fall back to defaults.
//!
//! Resolution order on load:
//!   1. `<exe_dir>/hotkeys.toml`           — portable mode (preferred).
//!   2. `<user_config_dir>/hotkeys.toml`   — installed mode.
//!   3. Built-in defaults                  — first run, no file anywhere.
//!
//! On the very first run we also write the defaults to the binary-
//! adjacent path so the user has something concrete to edit.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ============================================================================
// Binding — one parsed hotkey (modifiers + key).
// ============================================================================

/// Parsed (key, modifiers) tuple. Cached at app start so per-event
/// shortcut dispatch is a flat compare instead of a string parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    pub key:   egui::Key,
    pub ctrl:  bool,
    pub shift: bool,
    pub alt:   bool,
}

impl Binding {
    /// Test whether this binding fires for the given egui key event.
    /// Modifier match is exact — `ctrl+s` does NOT fire for `ctrl+shift+s`,
    /// preventing accidental overlap between bindings.
    pub fn matches(&self, key: egui::Key, mods: egui::Modifiers) -> bool {
        // egui reports the Cmd-on-mac / Ctrl-on-everywhere-else under
        // both `ctrl` and `command`. Folding them lets a `"ctrl+z"`
        // string fire on macOS without forcing the user to write
        // `cmd+z` separately.
        let ctrl_held = mods.ctrl || mods.command;
        self.key == key
            && self.ctrl  == ctrl_held
            && self.shift == mods.shift
            && self.alt   == mods.alt
    }

    /// Build the `egui::Modifiers` flag set for this binding so callers
    /// can hand it to `KeyboardShortcut::new` / `consume_shortcut`.
    pub fn to_modifiers(&self) -> egui::Modifiers {
        egui::Modifiers {
            alt:      self.alt,
            ctrl:     self.ctrl,
            shift:    self.shift,
            mac_cmd:  false,
            command:  self.ctrl,
        }
    }
}

/// Parse a `"ctrl+shift+n"` style string into a `Binding`. Case-
/// insensitive, accepts `+` or `-` between parts. Returns `None` if any
/// token is unrecognised so a typo in the file silently disables the
/// one binding instead of crashing the app.
pub fn parse(s: &str) -> Option<Binding> {
    let mut ctrl = false;
    let mut shift = false;
    let mut alt = false;
    let mut key: Option<egui::Key> = None;

    for raw in s.split(|c: char| c == '+' || c == '-') {
        let tok = raw.trim().to_ascii_lowercase();
        if tok.is_empty() { continue; }
        match tok.as_str() {
            "ctrl" | "control" | "cmd" | "meta" | "command" => ctrl  = true,
            "shift"                                          => shift = true,
            "alt" | "option"                                 => alt   = true,
            other => {
                if key.is_some() {
                    // Two key tokens — definitely malformed.
                    return None;
                }
                key = key_from_name(other);
                if key.is_none() { return None; }
            }
        }
    }
    Some(Binding { key: key?, ctrl, shift, alt })
}

/// Map a lowercase token to an `egui::Key`. Covers every key we
/// reference in the default hotkeys plus the common aliases.
fn key_from_name(name: &str) -> Option<egui::Key> {
    use egui::Key as K;
    Some(match name {
        // Letters
        "a" => K::A, "b" => K::B, "c" => K::C, "d" => K::D, "e" => K::E,
        "f" => K::F, "g" => K::G, "h" => K::H, "i" => K::I, "j" => K::J,
        "k" => K::K, "l" => K::L, "m" => K::M, "n" => K::N, "o" => K::O,
        "p" => K::P, "q" => K::Q, "r" => K::R, "s" => K::S, "t" => K::T,
        "u" => K::U, "v" => K::V, "w" => K::W, "x" => K::X, "y" => K::Y,
        "z" => K::Z,
        // Digits
        "0" => K::Num0, "1" => K::Num1, "2" => K::Num2, "3" => K::Num3,
        "4" => K::Num4, "5" => K::Num5, "6" => K::Num6, "7" => K::Num7,
        "8" => K::Num8, "9" => K::Num9,
        // Navigation
        "arrowleft"  | "left"  => K::ArrowLeft,
        "arrowright" | "right" => K::ArrowRight,
        "arrowup"    | "up"    => K::ArrowUp,
        "arrowdown"  | "down"  => K::ArrowDown,
        // Symbols
        "-" | "minus" => K::Minus,
        "=" | "equals" | "equal" => K::Equals,
        // Editing
        "backspace" => K::Backspace,
        "delete" | "del" => K::Delete,
        "enter" | "return" => K::Enter,
        "escape" | "esc" => K::Escape,
        "tab"   => K::Tab,
        "space" => K::Space,
        // Function keys
        "f1"  => K::F1,  "f2"  => K::F2,  "f3"  => K::F3,  "f4"  => K::F4,
        "f5"  => K::F5,  "f6"  => K::F6,  "f7"  => K::F7,  "f8"  => K::F8,
        "f9"  => K::F9,  "f10" => K::F10, "f11" => K::F11, "f12" => K::F12,
        _ => return None,
    })
}

// ============================================================================
// File schema.
// ============================================================================

/// Global OS-level hotkeys (work even when overlay is unfocused). The
/// `global-hotkey` crate parses the strings directly — its format and
/// ours diverge slightly, so we expose them as raw strings here and
/// let `input::hotkeys` translate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalSection {
    #[serde(default = "default_toggle_overlay")]
    pub toggle_overlay: String,
    #[serde(default = "default_screenshot")]
    pub screenshot: String,
    #[serde(default = "default_start_record")]
    pub start_record: String,
    #[serde(default = "default_pause_record")]
    pub pause_record: String,
}

impl Default for GlobalSection {
    fn default() -> Self {
        Self {
            toggle_overlay: default_toggle_overlay(),
            screenshot:     default_screenshot(),
            start_record:   default_start_record(),
            pause_record:   default_pause_record(),
        }
    }
}

fn default_toggle_overlay() -> String { "ctrl+shift+n".into() }
fn default_screenshot()     -> String { "ctrl+shift+s".into() }
fn default_start_record()   -> String { "ctrl+shift+r".into() }
fn default_pause_record()   -> String { "ctrl+shift+p".into() }

/// Translate our `"ctrl+shift+n"` style into the format the
/// `global-hotkey` crate expects (`"ctrl+shift+KeyN"`, `"shift+Digit0"`,
/// arrows verbatim). Returns `None` for unrecognised tokens so the
/// caller can log + skip.
pub fn to_global_hotkey_format(s: &str) -> Option<String> {
    let mut out = String::with_capacity(s.len() + 8);
    let mut first = true;
    for raw in s.split(|c: char| c == '+' || c == '-') {
        let tok = raw.trim();
        if tok.is_empty() { continue; }
        let lower = tok.to_ascii_lowercase();
        let translated: String = match lower.as_str() {
            "ctrl" | "control" => "ctrl".into(),
            "cmd" | "command" | "meta" => "meta".into(),
            "shift" => "shift".into(),
            "alt" | "option" => "alt".into(),
            other if other.len() == 1 && other.chars().next().unwrap().is_ascii_alphabetic() => {
                format!("Key{}", other.to_ascii_uppercase())
            }
            other if other.len() == 1 && other.chars().next().unwrap().is_ascii_digit() => {
                format!("Digit{other}")
            }
            "-" | "minus" => "Minus".into(),
            "=" | "equals" | "equal" => "Equal".into(),
            "arrowleft" | "left"   => "ArrowLeft".into(),
            "arrowright" | "right" => "ArrowRight".into(),
            "arrowup" | "up"       => "ArrowUp".into(),
            "arrowdown" | "down"   => "ArrowDown".into(),
            "backspace" => "Backspace".into(),
            "delete" | "del" => "Delete".into(),
            "enter" | "return" => "Enter".into(),
            "escape" | "esc" => "Escape".into(),
            "tab"   => "Tab".into(),
            "space" => "Space".into(),
            f if f.starts_with('f') && f[1..].chars().all(|c| c.is_ascii_digit()) => {
                let n: u32 = f[1..].parse().ok()?;
                if (1..=24).contains(&n) { format!("F{n}") } else { return None; }
            }
            _ => return None,
        };
        if !first { out.push('+'); }
        out.push_str(&translated);
        first = false;
    }
    if out.is_empty() { None } else { Some(out) }
}

/// Per-tool letter shortcuts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSection {
    #[serde(default = "d_pen")]            pub pen: String,
    #[serde(default = "d_pencil")]         pub pencil: String,
    #[serde(default = "d_marker")]         pub marker: String,
    #[serde(default = "d_airbrush")]       pub airbrush: String,
    #[serde(default = "d_fill")]           pub fill: String,
    #[serde(default = "d_laser")]          pub laser: String,
    #[serde(default = "d_text")]           pub text: String,
    #[serde(default = "d_spotlight")]      pub spotlight: String,
    #[serde(default = "d_lasso")]          pub lasso: String,
    #[serde(default = "d_lasso_select")]   pub lasso_select: String,
    #[serde(default = "d_eyedropper")]     pub eyedropper: String,
    #[serde(default = "d_rect")]           pub rect: String,
    #[serde(default = "d_ellipse")]        pub ellipse: String,
    #[serde(default = "d_arrow")]          pub arrow: String,
    #[serde(default = "d_freehand_arrow")] pub freehand_arrow: String,
}

impl Default for ToolSection {
    fn default() -> Self {
        Self {
            pen: d_pen(), pencil: d_pencil(), marker: d_marker(),
            airbrush: d_airbrush(), fill: d_fill(), laser: d_laser(),
            text: d_text(), spotlight: d_spotlight(), lasso: d_lasso(),
            lasso_select: d_lasso_select(),
            eyedropper: d_eyedropper(), rect: d_rect(), ellipse: d_ellipse(),
            arrow: d_arrow(), freehand_arrow: d_freehand_arrow(),
        }
    }
}

fn d_pen()            -> String { "p".into() }
fn d_pencil()         -> String { "b".into() }
fn d_marker()         -> String { "m".into() }
fn d_airbrush()       -> String { "g".into() }
fn d_fill()           -> String { "f".into() }
fn d_laser()          -> String { "l".into() }
fn d_text()           -> String { "t".into() }
fn d_spotlight()      -> String { "s".into() }
fn d_lasso()          -> String { "x".into() }
fn d_lasso_select()   -> String { "shift+x".into() }
fn d_eyedropper()     -> String { "i".into() }
fn d_rect()           -> String { "r".into() }
fn d_ellipse()        -> String { "o".into() }
fn d_arrow()          -> String { "a".into() }
fn d_freehand_arrow() -> String { "shift+a".into() }

/// Canvas-level shortcuts (undo / nav / view / freeze / layer opacity).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanvasSection {
    #[serde(default = "d_undo")]          pub undo: String,
    #[serde(default = "d_redo")]          pub redo: String,
    #[serde(default = "d_clear_layer")]   pub clear_layer: String,
    #[serde(default = "d_delete_canvas")] pub delete_canvas: String,
    #[serde(default = "d_reset_view")]    pub reset_view: String,
    /// Undoes every per-layer pan applied to the active layer. Must be
    /// a strict superset of `reset_view`'s modifiers (it is checked
    /// first) or `reset_view` swallows it — see the dispatch site.
    #[serde(default = "d_reset_layer_pos")] pub reset_layer_pos: String,
    #[serde(default = "d_freeze_frame")]  pub freeze_frame: String,
    #[serde(default = "d_prev_canvas")]   pub prev_canvas: String,
    #[serde(default = "d_next_canvas")]   pub next_canvas: String,
    #[serde(default = "d_prev_layer")]    pub prev_layer: String,
    #[serde(default = "d_next_layer")]    pub next_layer: String,
    #[serde(default = "d_toggle_ui")]     pub toggle_ui: String,

    // Absolute layer-opacity shortcuts. Each digit sets the active
    // layer's opacity to N · 10 %. 0 reads as 0 % (fully transparent)
    // following the user's spec — Photoshop's "0 = 100 %" idiom is not
    // followed; users who want 100 % use the increment key from 90 %.
    #[serde(default = "d_op_0")] pub opacity_0: String,
    #[serde(default = "d_op_1")] pub opacity_1: String,
    #[serde(default = "d_op_2")] pub opacity_2: String,
    #[serde(default = "d_op_3")] pub opacity_3: String,
    #[serde(default = "d_op_4")] pub opacity_4: String,
    #[serde(default = "d_op_5")] pub opacity_5: String,
    #[serde(default = "d_op_6")] pub opacity_6: String,
    #[serde(default = "d_op_7")] pub opacity_7: String,
    #[serde(default = "d_op_8")] pub opacity_8: String,
    #[serde(default = "d_op_9")] pub opacity_9: String,
    /// `-` key — relative −10 %. Clamped at 0.
    #[serde(default = "d_op_dec")] pub opacity_dec: String,
    /// `=` key — relative +10 %. Clamped at 1.
    #[serde(default = "d_op_inc")] pub opacity_inc: String,
    /// Toggles a centred popup showing a large thumbnail of the
    /// active layer's content. Helpful for "what's on this layer?"
    /// when many layers exist.
    #[serde(default = "d_preview_layer")] pub preview_layer: String,
}

impl Default for CanvasSection {
    fn default() -> Self {
        Self {
            undo: d_undo(), redo: d_redo(),
            clear_layer: d_clear_layer(), delete_canvas: d_delete_canvas(),
            reset_view: d_reset_view(), reset_layer_pos: d_reset_layer_pos(),
            freeze_frame: d_freeze_frame(),
            prev_canvas: d_prev_canvas(), next_canvas: d_next_canvas(),
            prev_layer: d_prev_layer(), next_layer: d_next_layer(),
            toggle_ui: d_toggle_ui(),
            opacity_0: d_op_0(), opacity_1: d_op_1(),
            opacity_2: d_op_2(), opacity_3: d_op_3(),
            opacity_4: d_op_4(), opacity_5: d_op_5(),
            opacity_6: d_op_6(), opacity_7: d_op_7(),
            opacity_8: d_op_8(), opacity_9: d_op_9(),
            opacity_dec: d_op_dec(),
            opacity_inc: d_op_inc(),
            preview_layer: d_preview_layer(),
        }
    }
}

fn d_undo()          -> String { "ctrl+z".into() }
fn d_redo()          -> String { "ctrl+shift+z".into() }
fn d_clear_layer()   -> String { "ctrl+delete".into() }
fn d_delete_canvas() -> String { "ctrl+shift+backspace".into() }
fn d_reset_view()    -> String { "ctrl+0".into() }
fn d_reset_layer_pos() -> String { "ctrl+shift+0".into() }
fn d_freeze_frame()  -> String { "ctrl+shift+f".into() }
fn d_prev_canvas()   -> String { "arrowleft".into() }
fn d_next_canvas()   -> String { "arrowright".into() }
fn d_prev_layer()    -> String { "arrowdown".into() }
fn d_next_layer()    -> String { "arrowup".into() }
fn d_toggle_ui()     -> String { "tab".into() }
fn d_op_0()   -> String { "0".into() }
fn d_op_1()   -> String { "1".into() }
fn d_op_2()   -> String { "2".into() }
fn d_op_3()   -> String { "3".into() }
fn d_op_4()   -> String { "4".into() }
fn d_op_5()   -> String { "5".into() }
fn d_op_6()   -> String { "6".into() }
fn d_op_7()   -> String { "7".into() }
fn d_op_8()   -> String { "8".into() }
fn d_op_9()   -> String { "9".into() }
fn d_op_dec() -> String { "-".into() }
fn d_op_inc() -> String { "=".into() }
fn d_preview_layer() -> String { "q".into() }

/// Top-level hotkeys file shape.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HotkeysConfig {
    #[serde(default)] pub global: GlobalSection,
    #[serde(default)] pub tool:   ToolSection,
    #[serde(default)] pub canvas: CanvasSection,
}

// ============================================================================
// Parsed bindings — built once at app start, hot-path-cheap to compare.
// ============================================================================

#[derive(Debug, Clone, Copy)]
pub struct ToolBindings {
    pub pen: Option<Binding>,
    pub pencil: Option<Binding>,
    pub marker: Option<Binding>,
    pub airbrush: Option<Binding>,
    pub fill: Option<Binding>,
    pub laser: Option<Binding>,
    pub text: Option<Binding>,
    pub spotlight: Option<Binding>,
    pub lasso: Option<Binding>,
    pub lasso_select: Option<Binding>,
    pub eyedropper: Option<Binding>,
    pub rect: Option<Binding>,
    pub ellipse: Option<Binding>,
    pub arrow: Option<Binding>,
    pub freehand_arrow: Option<Binding>,
}

#[derive(Debug, Clone, Copy)]
pub struct CanvasBindings {
    pub undo: Option<Binding>,
    pub redo: Option<Binding>,
    pub clear_layer: Option<Binding>,
    pub delete_canvas: Option<Binding>,
    pub reset_view: Option<Binding>,
    pub reset_layer_pos: Option<Binding>,
    pub freeze_frame: Option<Binding>,
    pub prev_canvas: Option<Binding>,
    pub next_canvas: Option<Binding>,
    pub prev_layer: Option<Binding>,
    pub next_layer: Option<Binding>,
    pub toggle_ui: Option<Binding>,
    /// Indexed 0..=9 — `opacity[N]` sets the active layer to N·10 %.
    pub opacity: [Option<Binding>; 10],
    pub opacity_dec: Option<Binding>,
    pub opacity_inc: Option<Binding>,
    pub preview_layer: Option<Binding>,
}

#[derive(Debug, Clone, Copy)]
pub struct ParsedHotkeys {
    pub tool:   ToolBindings,
    pub canvas: CanvasBindings,
}

impl HotkeysConfig {
    /// Compile every string in the file into a `Binding`. Bad strings
    /// become `None` and silently disable that binding (logged at
    /// warn level).
    pub fn parsed(&self) -> ParsedHotkeys {
        let p = |label: &str, s: &str| -> Option<Binding> {
            match parse(s) {
                Some(b) => Some(b),
                None => {
                    log::warn!("hotkey '{label}' has invalid binding '{s}' — disabled");
                    None
                }
            }
        };
        ParsedHotkeys {
            tool: ToolBindings {
                pen:            p("tool.pen",            &self.tool.pen),
                pencil:         p("tool.pencil",         &self.tool.pencil),
                marker:         p("tool.marker",         &self.tool.marker),
                airbrush:       p("tool.airbrush",       &self.tool.airbrush),
                fill:           p("tool.fill",           &self.tool.fill),
                laser:          p("tool.laser",          &self.tool.laser),
                text:           p("tool.text",           &self.tool.text),
                spotlight:      p("tool.spotlight",      &self.tool.spotlight),
                lasso:          p("tool.lasso",          &self.tool.lasso),
                lasso_select:   p("tool.lasso_select",   &self.tool.lasso_select),
                eyedropper:     p("tool.eyedropper",     &self.tool.eyedropper),
                rect:           p("tool.rect",           &self.tool.rect),
                ellipse:        p("tool.ellipse",        &self.tool.ellipse),
                arrow:          p("tool.arrow",          &self.tool.arrow),
                freehand_arrow: p("tool.freehand_arrow", &self.tool.freehand_arrow),
            },
            canvas: CanvasBindings {
                undo:          p("canvas.undo",          &self.canvas.undo),
                redo:          p("canvas.redo",          &self.canvas.redo),
                clear_layer:   p("canvas.clear_layer",   &self.canvas.clear_layer),
                delete_canvas: p("canvas.delete_canvas", &self.canvas.delete_canvas),
                reset_view:    p("canvas.reset_view",    &self.canvas.reset_view),
                reset_layer_pos: p("canvas.reset_layer_pos", &self.canvas.reset_layer_pos),
                freeze_frame:  p("canvas.freeze_frame",  &self.canvas.freeze_frame),
                prev_canvas:   p("canvas.prev_canvas",   &self.canvas.prev_canvas),
                next_canvas:   p("canvas.next_canvas",   &self.canvas.next_canvas),
                prev_layer:    p("canvas.prev_layer",    &self.canvas.prev_layer),
                next_layer:    p("canvas.next_layer",    &self.canvas.next_layer),
                toggle_ui:     p("canvas.toggle_ui",     &self.canvas.toggle_ui),
                opacity: [
                    p("canvas.opacity_0", &self.canvas.opacity_0),
                    p("canvas.opacity_1", &self.canvas.opacity_1),
                    p("canvas.opacity_2", &self.canvas.opacity_2),
                    p("canvas.opacity_3", &self.canvas.opacity_3),
                    p("canvas.opacity_4", &self.canvas.opacity_4),
                    p("canvas.opacity_5", &self.canvas.opacity_5),
                    p("canvas.opacity_6", &self.canvas.opacity_6),
                    p("canvas.opacity_7", &self.canvas.opacity_7),
                    p("canvas.opacity_8", &self.canvas.opacity_8),
                    p("canvas.opacity_9", &self.canvas.opacity_9),
                ],
                opacity_dec: p("canvas.opacity_dec", &self.canvas.opacity_dec),
                opacity_inc: p("canvas.opacity_inc", &self.canvas.opacity_inc),
                preview_layer: p("canvas.preview_layer", &self.canvas.preview_layer),
            },
        }
    }
}

// ============================================================================
// File I/O.
// ============================================================================

/// Path next to the executable. Falls back to current dir if exe path
/// resolution fails (e.g. cargo run in an odd env).
fn exe_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("hotkeys.toml")))
        .unwrap_or_else(|| PathBuf::from("hotkeys.toml"))
}

/// Path inside the platform's user-config dir. Mirrors the strategy in
/// `persistence::paths`.
fn user_config_path() -> Option<PathBuf> {
    use directories::ProjectDirs;
    let dirs = ProjectDirs::from("", "", "on-screen-notes")?;
    Some(dirs.config_dir().join("hotkeys.toml"))
}

impl HotkeysConfig {
    /// Try exe-adjacent → user config → defaults, in that order. On
    /// fresh installs writes the defaults to the exe-adjacent path so
    /// the user has something concrete to edit.
    pub fn load_or_default() -> Self {
        let exe_p = exe_path();
        if let Ok(s) = std::fs::read_to_string(&exe_p) {
            match toml::from_str::<HotkeysConfig>(&s) {
                Ok(c) => {
                    log::info!("hotkeys loaded from {}", exe_p.display());
                    return c;
                }
                Err(e) => log::warn!("hotkeys.toml at {} failed to parse: {e}", exe_p.display()),
            }
        }
        if let Some(user_p) = user_config_path() {
            if let Ok(s) = std::fs::read_to_string(&user_p) {
                match toml::from_str::<HotkeysConfig>(&s) {
                    Ok(c) => {
                        log::info!("hotkeys loaded from {}", user_p.display());
                        return c;
                    }
                    Err(e) => log::warn!(
                        "hotkeys.toml at {} failed to parse: {e}", user_p.display()
                    ),
                }
            }
        }
        let defaults = Self::default();
        // Write defaults so user has a template — best-effort, swallow errors.
        if let Err(e) = defaults.save_to_exe_adjacent() {
            log::debug!("could not write default hotkeys.toml: {e}");
        }
        defaults
    }

    /// Write the current config to the exe-adjacent path. Best path
    /// for portability — moving the binary moves its config.
    pub fn save_to_exe_adjacent(&self) -> std::io::Result<()> {
        let p = exe_path();
        let toml_str = toml::to_string_pretty(self).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())
        })?;
        let header = "# On-Screen Notes — hotkeys.toml\n\
                      # Strings are case-insensitive. Modifiers: ctrl, shift, alt.\n\
                      # Special keys: arrowleft/right/up/down, backspace, delete,\n\
                      # enter, escape, tab, space, f1..f12.\n\n";
        let full = format!("{header}{toml_str}");
        std::fs::write(&p, full)
    }
}
