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
//! adjacent path so the user has something concrete to edit. A file
//! from an older format is upgraded in place on load (`upgrade_text`).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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

    /// How the binding reads to the user, e.g. "Ctrl+R" or "Ctrl+↑".
    pub fn label(&self) -> String {
        use egui::Key as K;
        let mut s = String::new();
        for (on, name) in [(self.ctrl, "Ctrl+"), (self.shift, "Shift+"), (self.alt, "Alt+")] {
            if on {
                s.push_str(name);
            }
        }
        s.push_str(match self.key {
            K::ArrowLeft  => "←",
            K::ArrowRight => "→",
            K::ArrowUp    => "↑",
            K::ArrowDown  => "↓",
            K::Comma  => ",",
            K::Period => ".",
            K::Minus  => "-",
            K::Equals => "=",
            k => k.name(),
        });
        s
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
    // A `-` at the end is the minus key itself ("-", "ctrl+-"), not a
    // separator; the split below would drop it.
    let s = s.trim();
    let (s, minus) = match s.strip_suffix('-') {
        Some(rest) if rest.is_empty() || rest.ends_with(['+', '-']) => (rest, true),
        _ => (s, false),
    };
    let mut key: Option<egui::Key> = minus.then_some(egui::Key::Minus);

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
        "home" => K::Home,
        "end"  => K::End,
        // Symbols
        "-" | "minus" => K::Minus,
        "=" | "equals" | "equal" => K::Equals,
        "," | "comma" => K::Comma,
        "." | "period" | "dot" => K::Period,
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

// A global hotkey is taken from every other app while OSN runs, so the
// defaults use Ctrl+Alt+Shift, which apps leave alone. They were
// Ctrl+Shift+N/S/R/P, which ate Save As, VS Code's command palette and
// the browser's hard reload; `upgrade_text` moves files still on those.
fn default_toggle_overlay() -> String { "ctrl+alt+shift+n".into() }
fn default_screenshot()     -> String { "ctrl+alt+shift+s".into() }
fn default_start_record()   -> String { "ctrl+alt+shift+r".into() }
fn default_pause_record()   -> String { "ctrl+alt+shift+p".into() }

type DefaultFn = fn() -> String;

/// Each `[global]` key with its default before format 2 and its default now.
const OLD_GLOBAL_DEFAULTS: [(&str, &str, DefaultFn); 4] = [
    ("toggle_overlay", "ctrl+shift+n", default_toggle_overlay),
    ("screenshot",     "ctrl+shift+s", default_screenshot),
    ("start_record",   "ctrl+shift+r", default_start_record),
    ("pause_record",   "ctrl+shift+p", default_pause_record),
];

/// True when two global-hotkey strings name the same combo, whatever
/// the case or the order of the modifiers.
fn same_global_combo(a: &str, b: &str) -> bool {
    let tokens = |s: &str| {
        to_global_hotkey_format(s).map(|f| {
            let mut t: Vec<String> = f.split('+').map(str::to_owned).collect();
            t.sort();
            t
        })
    };
    matches!((tokens(a), tokens(b)), (Some(x), Some(y)) if x == y)
}

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
    #[serde(default = "d_select")]         pub select: String,
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
            select: d_select(),
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
fn d_select()         -> String { "v".into() }
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
    /// Undoes every per-layer zoom applied to the active layer, in
    /// place (the layer keeps its centre). Same modifier-superset rule
    /// as `reset_layer_pos` — it is checked before `reset_view`.
    ///
    /// The default deliberately avoids `Alt`: on AltGr layouts
    /// `Ctrl+Alt+<digit>` types a character instead of reaching us.
    #[serde(default = "d_reset_layer_zoom")] pub reset_layer_zoom: String,
    #[serde(default = "d_freeze_frame")]  pub freeze_frame: String,
    #[serde(default = "d_prev_canvas")]   pub prev_canvas: String,
    #[serde(default = "d_next_canvas")]   pub next_canvas: String,
    /// Next / previous slide, wrapping at both ends. Unlike
    /// `next_canvas` this never starts a new slide past the last one.
    #[serde(default = "d_cycle_next_slide")] pub cycle_next_slide: String,
    #[serde(default = "d_cycle_prev_slide")] pub cycle_prev_slide: String,
    #[serde(default = "d_first_slide")]   pub first_slide: String,
    #[serde(default = "d_last_slide")]    pub last_slide: String,
    /// Name the active slide, in the slide panel.
    #[serde(default = "d_rename_slide")]  pub rename_slide: String,
    /// Lock the active slide against edits (it still pans and zooms), or
    /// unlock it.
    #[serde(default = "d_lock_slide")]    pub lock_slide: String,
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
    /// Hide the active layer, or show it again (the eye in the layer panel).
    #[serde(default = "d_toggle_layer_visible")] pub toggle_layer_visible: String,

    // Select-tool actions. They only do anything while objects are
    // selected, so `delete` and the arrow chords stay free otherwise.
    #[serde(default = "d_delete_selection")] pub delete_selection: String,
    #[serde(default = "d_duplicate")]        pub duplicate: String,
    /// Copy the selection onto a new slide (added after the last one).
    #[serde(default = "d_copy_to_new_slide")] pub copy_to_new_slide: String,
    /// Pin the selection to every slide, or unpin it if it all is.
    #[serde(default = "d_pin_selection")] pub pin_selection: String,
    /// Hide every pin, or show them again.
    #[serde(default = "d_toggle_pins")] pub toggle_pins: String,
    /// Paint the selection above everything else of its kind on the layer.
    #[serde(default = "d_bring_front")]      pub bring_front: String,
    /// Paint the selection below everything else of its kind on the layer.
    #[serde(default = "d_send_back")]        pub send_back: String,

    // Video / GIF playback. They act on the selected video or GIF, else
    // the one under the pointer, else the only one on the slide.
    #[serde(default = "d_media_play_pause")] pub media_play_pause: String,
    #[serde(default = "d_media_prev_frame")] pub media_prev_frame: String,
    #[serde(default = "d_media_next_frame")] pub media_next_frame: String,
}

impl Default for CanvasSection {
    fn default() -> Self {
        Self {
            undo: d_undo(), redo: d_redo(),
            clear_layer: d_clear_layer(), delete_canvas: d_delete_canvas(),
            reset_view: d_reset_view(), reset_layer_pos: d_reset_layer_pos(),
            reset_layer_zoom: d_reset_layer_zoom(),
            freeze_frame: d_freeze_frame(),
            prev_canvas: d_prev_canvas(), next_canvas: d_next_canvas(),
            cycle_next_slide: d_cycle_next_slide(), cycle_prev_slide: d_cycle_prev_slide(),
            first_slide: d_first_slide(), last_slide: d_last_slide(),
            rename_slide: d_rename_slide(),
            lock_slide: d_lock_slide(),
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
            toggle_layer_visible: d_toggle_layer_visible(),
            delete_selection: d_delete_selection(),
            duplicate: d_duplicate(),
            copy_to_new_slide: d_copy_to_new_slide(),
            pin_selection: d_pin_selection(),
            toggle_pins: d_toggle_pins(),
            bring_front: d_bring_front(),
            send_back: d_send_back(),
            media_play_pause: d_media_play_pause(),
            media_prev_frame: d_media_prev_frame(),
            media_next_frame: d_media_next_frame(),
        }
    }
}

fn d_undo()          -> String { "ctrl+z".into() }
fn d_redo()          -> String { "ctrl+shift+z".into() }
fn d_clear_layer()   -> String { "ctrl+delete".into() }
fn d_delete_canvas() -> String { "ctrl+shift+backspace".into() }
fn d_reset_view()    -> String { "ctrl+0".into() }
fn d_reset_layer_pos() -> String { "ctrl+shift+0".into() }
fn d_reset_layer_zoom() -> String { "ctrl+shift+9".into() }
fn d_freeze_frame()  -> String { "ctrl+shift+f".into() }
fn d_prev_canvas()   -> String { "arrowleft".into() }
fn d_next_canvas()   -> String { "arrowright".into() }
fn d_cycle_next_slide() -> String { "ctrl+tab".into() }
fn d_cycle_prev_slide() -> String { "ctrl+shift+tab".into() }
fn d_first_slide()   -> String { "home".into() }
fn d_last_slide()    -> String { "end".into() }
fn d_rename_slide()  -> String { "f2".into() }
fn d_lock_slide()    -> String { "ctrl+r".into() }
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
fn d_toggle_layer_visible() -> String { "h".into() }
fn d_delete_selection() -> String { "delete".into() }
fn d_duplicate()   -> String { "ctrl+d".into() }
fn d_copy_to_new_slide() -> String { "ctrl+shift+d".into() }
fn d_pin_selection() -> String { "ctrl+p".into() }
fn d_toggle_pins() -> String { "shift+p".into() }
fn d_bring_front() -> String { "ctrl+arrowup".into() }
fn d_send_back()   -> String { "ctrl+arrowdown".into() }
fn d_media_play_pause() -> String { "k".into() }
fn d_media_prev_frame() -> String { "comma".into() }
fn d_media_next_frame() -> String { "period".into() }

/// Current file format. 2 moved the global defaults off Ctrl+Shift.
const FORMAT: u32 = 2;

/// Top-level hotkeys file shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotkeysConfig {
    /// Format the file was written in. Files from before it was
    /// recorded read as 0 and get `upgrade_text` once; after that a
    /// binding set back to an old default stays.
    #[serde(default)] pub version: u32,
    #[serde(default)] pub global: GlobalSection,
    #[serde(default)] pub tool:   ToolSection,
    #[serde(default)] pub canvas: CanvasSection,
}

impl Default for HotkeysConfig {
    fn default() -> Self {
        Self {
            version: FORMAT,
            global: GlobalSection::default(),
            tool: ToolSection::default(),
            canvas: CanvasSection::default(),
        }
    }
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
    pub select: Option<Binding>,
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
    pub reset_layer_zoom: Option<Binding>,
    pub freeze_frame: Option<Binding>,
    pub prev_canvas: Option<Binding>,
    pub next_canvas: Option<Binding>,
    pub cycle_next_slide: Option<Binding>,
    pub cycle_prev_slide: Option<Binding>,
    pub first_slide: Option<Binding>,
    pub last_slide: Option<Binding>,
    pub rename_slide: Option<Binding>,
    pub lock_slide: Option<Binding>,
    pub prev_layer: Option<Binding>,
    pub next_layer: Option<Binding>,
    pub toggle_ui: Option<Binding>,
    /// Indexed 0..=9 — `opacity[N]` sets the active layer to N·10 %.
    pub opacity: [Option<Binding>; 10],
    pub opacity_dec: Option<Binding>,
    pub opacity_inc: Option<Binding>,
    pub preview_layer: Option<Binding>,
    pub toggle_layer_visible: Option<Binding>,
    pub delete_selection: Option<Binding>,
    pub duplicate: Option<Binding>,
    pub copy_to_new_slide: Option<Binding>,
    pub pin_selection: Option<Binding>,
    pub toggle_pins: Option<Binding>,
    pub bring_front: Option<Binding>,
    pub send_back: Option<Binding>,
    pub media_play_pause: Option<Binding>,
    pub media_prev_frame: Option<Binding>,
    pub media_next_frame: Option<Binding>,
}

/// The `[global]` bindings, for showing them in the help panel. The OS
/// hotkeys themselves are registered from the strings (`input::hotkeys`).
#[derive(Debug, Clone, Copy)]
pub struct GlobalBindings {
    pub toggle_overlay: Option<Binding>,
    pub screenshot: Option<Binding>,
    pub start_record: Option<Binding>,
    pub pause_record: Option<Binding>,
}

#[derive(Debug, Clone, Copy)]
pub struct ParsedHotkeys {
    pub global: GlobalBindings,
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
            // Not `p`: a global the OS takes but `parse` does not know
            // (F13 and up) is no reason to warn, it just goes unlisted.
            global: GlobalBindings {
                toggle_overlay: parse(&self.global.toggle_overlay),
                screenshot:     parse(&self.global.screenshot),
                start_record:   parse(&self.global.start_record),
                pause_record:   parse(&self.global.pause_record),
            },
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
                select:         p("tool.select",         &self.tool.select),
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
                reset_layer_zoom: p("canvas.reset_layer_zoom", &self.canvas.reset_layer_zoom),
                freeze_frame:  p("canvas.freeze_frame",  &self.canvas.freeze_frame),
                prev_canvas:   p("canvas.prev_canvas",   &self.canvas.prev_canvas),
                next_canvas:   p("canvas.next_canvas",   &self.canvas.next_canvas),
                cycle_next_slide: p("canvas.cycle_next_slide", &self.canvas.cycle_next_slide),
                cycle_prev_slide: p("canvas.cycle_prev_slide", &self.canvas.cycle_prev_slide),
                first_slide:   p("canvas.first_slide",   &self.canvas.first_slide),
                last_slide:    p("canvas.last_slide",    &self.canvas.last_slide),
                rename_slide:  p("canvas.rename_slide",  &self.canvas.rename_slide),
                lock_slide:    p("canvas.lock_slide",    &self.canvas.lock_slide),
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
                toggle_layer_visible: p("canvas.toggle_layer_visible", &self.canvas.toggle_layer_visible),
                delete_selection: p("canvas.delete_selection", &self.canvas.delete_selection),
                duplicate:   p("canvas.duplicate",   &self.canvas.duplicate),
                copy_to_new_slide: p("canvas.copy_to_new_slide", &self.canvas.copy_to_new_slide),
                pin_selection: p("canvas.pin_selection", &self.canvas.pin_selection),
                toggle_pins: p("canvas.toggle_pins", &self.canvas.toggle_pins),
                bring_front: p("canvas.bring_front", &self.canvas.bring_front),
                send_back:   p("canvas.send_back",   &self.canvas.send_back),
                media_play_pause: p("canvas.media_play_pause", &self.canvas.media_play_pause),
                media_prev_frame: p("canvas.media_prev_frame", &self.canvas.media_prev_frame),
                media_next_frame: p("canvas.media_next_frame", &self.canvas.media_next_frame),
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
        if let Some(c) = Self::load(&exe_path()) {
            return c;
        }
        if let Some(c) = user_config_path().and_then(|p| Self::load(&p)) {
            return c;
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
                      # Special keys: arrowleft/right/up/down, home, end, backspace,\n\
                      # delete, enter, escape, tab, space, f1..f12.\n\n";
        let full = format!("{header}{toml_str}");
        std::fs::write(&p, full)
    }

    /// Read and parse the file at `path`, bringing one from an older
    /// format up to date and writing it back. `None` when there is no
    /// file there or it does not parse.
    fn load(path: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(path).ok()?;
        let cfg = match toml::from_str::<Self>(&text) {
            Ok(c) => c,
            Err(e) => {
                log::warn!("hotkeys.toml at {} failed to parse: {e}", path.display());
                return None;
            }
        };
        log::info!("hotkeys loaded from {}", path.display());
        if cfg.version >= FORMAT {
            return Some(cfg);
        }
        let upgraded = upgrade_text(&text);
        match toml::from_str::<Self>(&upgraded) {
            Ok(c) => {
                // Unwritable (a read-only install dir): the upgrade
                // still applies, it just runs again next launch.
                match std::fs::write(path, &upgraded) {
                    Ok(()) => log::info!("hotkeys.toml at {} upgraded to format {FORMAT}", path.display()),
                    Err(e) => log::warn!("could not write upgraded hotkeys.toml at {}: {e}", path.display()),
                }
                Some(c)
            }
            Err(e) => {
                log::warn!("upgraded hotkeys.toml did not parse ({e}); using it as written");
                Some(cfg)
            }
        }
    }
}

/// Bring the text of a hotkeys file from before `FORMAT` up to date:
/// each `[global]` binding still on its old default moves to the new
/// one, and the format is recorded so this happens once. Comments,
/// layout and the user's own bindings are kept as written.
fn upgrade_text(text: &str) -> String {
    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let version_line = format!("version = {FORMAT}");
    let mut out = String::with_capacity(text.len() + 32);
    // Empty at the top level, before the first table.
    let mut section = String::new();
    let mut version_written = false;
    for line in text.split_inclusive('\n') {
        let body = line.trim_end_matches(['\r', '\n']);
        let ending = &line[body.len()..];
        let trimmed = body.trim();
        if trimmed.starts_with('[') {
            if !version_written {
                out.push_str(&format!("{version_line}{nl}{nl}"));
                version_written = true;
            }
            section = trimmed.trim_matches(['[', ']']).trim().to_owned();
            out.push_str(line);
            continue;
        }
        let key = trimmed.split_once('=').map(|(k, _)| k.trim().trim_matches('"'));
        if section.is_empty() && key == Some("version") {
            out.push_str(&version_line);
            out.push_str(ending);
            version_written = true;
            continue;
        }
        if section == "global" {
            if let Some(new) = upgrade_global_line(body) {
                out.push_str(&new);
                out.push_str(ending);
                continue;
            }
        }
        out.push_str(line);
    }
    if !version_written {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push_str(nl);
        }
        out.push_str(&version_line);
        out.push_str(nl);
    }
    out
}

/// `line` (in `[global]`, without its line ending) with its value moved
/// to the new default, when it binds one of `OLD_GLOBAL_DEFAULTS` to
/// that key's old default. Anything else around the value — spacing, a
/// trailing comment — is kept.
fn upgrade_global_line(line: &str) -> Option<String> {
    let (key, rest) = line.split_once('=')?;
    let (_, old, new) = OLD_GLOBAL_DEFAULTS
        .iter()
        .find(|(k, _, _)| *k == key.trim().trim_matches('"'))?;
    let open = rest.find(['"', '\''])?;
    let quote = &rest[open..open + 1];
    let len = rest[open + 1..].find(quote)?;
    let value = &rest[open + 1..open + 1 + len];
    if !same_global_combo(value, old) {
        return None;
    }
    let before = &rest[..open];
    let after = &rest[open + 1 + len + 1..];
    Some(format!("{key}={before}{quote}{}{quote}{after}", new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_keys_parse() {
        let c = HotkeysConfig::default().parsed().canvas;
        assert_eq!(c.media_play_pause.map(|b| b.key), Some(egui::Key::K));
        assert_eq!(c.media_prev_frame.map(|b| b.key), Some(egui::Key::Comma));
        assert_eq!(c.media_next_frame.map(|b| b.key), Some(egui::Key::Period));
        assert_eq!(parse(",").map(|b| b.key), Some(egui::Key::Comma));
        assert_eq!(parse("shift+.").map(|b| (b.key, b.shift)), Some((egui::Key::Period, true)));
    }

    #[test]
    fn slide_keys_parse() {
        let c = HotkeysConfig::default().parsed().canvas;
        let b = |b: Option<Binding>| b.map(|b| (b.key, b.ctrl, b.shift));
        assert_eq!(b(c.cycle_next_slide), Some((egui::Key::Tab, true, false)));
        assert_eq!(b(c.cycle_prev_slide), Some((egui::Key::Tab, true, true)));
        assert_eq!(b(c.first_slide), Some((egui::Key::Home, false, false)));
        assert_eq!(b(c.last_slide), Some((egui::Key::End, false, false)));
        assert_eq!(b(c.rename_slide), Some((egui::Key::F2, false, false)));
        assert_eq!(b(c.lock_slide), Some((egui::Key::R, true, false)));
        // Ctrl+R is not the rectangle tool (plain R), nor is Ctrl+Shift+R
        // (the recording hotkey before format 2) the lock.
        let ctrl = egui::Modifiers { ctrl: true, command: true, ..Default::default() };
        let tools = HotkeysConfig::default().parsed().tool;
        assert!(!tools.rect.unwrap().matches(egui::Key::R, ctrl));
        assert!(!c.lock_slide.unwrap().matches(egui::Key::R, egui::Modifiers { shift: true, ..ctrl }));
        // Ctrl+Tab must not also toggle the UI (plain Tab).
        let ctrl = egui::Modifiers { ctrl: true, command: true, ..Default::default() };
        assert!(!c.toggle_ui.unwrap().matches(egui::Key::Tab, ctrl));
    }

    #[test]
    fn default_bindings_parse_and_do_not_collide() {
        // Every `[tool]` and `[canvas]` default, found through serde so a
        // binding added later is checked without listing it here.
        let value = toml::Value::try_from(HotkeysConfig::default()).unwrap();
        let mut seen: Vec<(String, Binding)> = Vec::new();
        for section in ["tool", "canvas"] {
            for (name, s) in value[section].as_table().unwrap() {
                let s = s.as_str().unwrap();
                let b = parse(s).unwrap_or_else(|| panic!("{section}.{name} = {s:?} does not parse"));
                if let Some((other, _)) = seen.iter().find(|(_, o)| *o == b) {
                    panic!("{section}.{name} and {other} are both {s:?}");
                }
                seen.push((format!("{section}.{name}"), b));
            }
        }
        assert!(seen.iter().any(|(n, b)| n == "canvas.toggle_layer_visible" && b.key == egui::Key::H));
    }

    #[test]
    fn labels_read_as_the_keys_do() {
        let l = |s: &str| parse(s).unwrap().label();
        assert_eq!(l("ctrl+arrowup"), "Ctrl+↑");
        assert_eq!(l("-"), "-");
        assert_eq!(l("ctrl+-"), "Ctrl+-");
        assert_eq!(l("ctrl+minus"), "Ctrl+-");
        assert_eq!(l("comma"), ",");
        assert_eq!(l("="), "=");
        assert_eq!(l("ctrl+shift+backspace"), "Ctrl+Shift+Backspace");
        assert_eq!(l("ctrl+alt+shift+n"), "Ctrl+Shift+Alt+N");
        let g = HotkeysConfig::default().parsed().global;
        assert_eq!(g.screenshot.map(|b| (b.key, b.ctrl, b.shift)), Some((egui::Key::S, true, true)));
    }

    #[test]
    fn files_without_media_keys_get_the_defaults() {
        let cfg: HotkeysConfig = toml::from_str("[canvas]\nundo = \"ctrl+z\"\n").unwrap();
        assert_eq!(cfg.canvas.media_play_pause, "k");
    }

    #[test]
    fn written_defaults_read_back_current() {
        let text = toml::to_string_pretty(&HotkeysConfig::default()).unwrap();
        let cfg: HotkeysConfig = toml::from_str(&text).unwrap();
        assert_eq!(cfg.version, FORMAT);
        assert_eq!(cfg.global.screenshot, "ctrl+alt+shift+s");
    }

    #[test]
    fn upgrade_moves_only_untouched_global_defaults() {
        let old = "# On-Screen Notes — hotkeys.toml\n\n\
                   [global]\n\
                   toggle_overlay = \"ctrl+shift+n\"\n\
                   screenshot = \"F9\"\n\
                   start_record   = 'Shift+Ctrl+R'  # mine\n\
                   pause_record = \"ctrl+alt+p\"\n\
                   [tool]\n\
                   pen = \"ctrl+shift+n\"\n";
        let cfg: HotkeysConfig = toml::from_str(old).unwrap();
        assert_eq!(cfg.version, 0);

        let new = upgrade_text(old);
        assert_eq!(
            new,
            "# On-Screen Notes — hotkeys.toml\n\n\
             version = 2\n\n\
             [global]\n\
             toggle_overlay = \"ctrl+alt+shift+n\"\n\
             screenshot = \"F9\"\n\
             start_record   = 'ctrl+alt+shift+r'  # mine\n\
             pause_record = \"ctrl+alt+p\"\n\
             [tool]\n\
             pen = \"ctrl+shift+n\"\n"
        );
        let cfg: HotkeysConfig = toml::from_str(&new).unwrap();
        assert_eq!(cfg.version, FORMAT);
        assert_eq!(cfg.global.toggle_overlay, "ctrl+alt+shift+n");
        assert_eq!(cfg.global.screenshot, "F9");
        assert_eq!(cfg.global.start_record, "ctrl+alt+shift+r");
        assert_eq!(cfg.global.pause_record, "ctrl+alt+p");
        assert_eq!(cfg.tool.pen, "ctrl+shift+n");
    }

    #[test]
    fn upgrade_without_a_global_section_records_the_format() {
        let new = upgrade_text("[canvas]\r\nundo = \"ctrl+z\"\r\n");
        assert_eq!(new, "version = 2\r\n\r\n[canvas]\r\nundo = \"ctrl+z\"\r\n");
        let cfg: HotkeysConfig = toml::from_str(&new).unwrap();
        assert_eq!(cfg.global.toggle_overlay, "ctrl+alt+shift+n");

        let new = upgrade_text("version = 1");
        assert_eq!(new, "version = 2");
        assert_eq!(upgrade_text(""), "version = 2\n");
    }

    #[test]
    fn old_and_new_global_defaults_are_different_combos() {
        for (_, old, new) in OLD_GLOBAL_DEFAULTS {
            assert!(!same_global_combo(old, &new()));
            assert!(same_global_combo(old, &old.to_uppercase()));
        }
        // `cmd` is the Windows key to the global-hotkey crate, not Ctrl.
        assert!(!same_global_combo("cmd+shift+n", "ctrl+shift+n"));
    }
}
