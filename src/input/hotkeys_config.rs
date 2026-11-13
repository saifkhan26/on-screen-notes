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
            K::OpenBracket  => "[",
            K::CloseBracket => "]",
            K::Semicolon => ";",
            K::Quote     => "'",
            K::Slash     => "/",
            K::Backslash => "\\",
            K::Backtick  => "`",
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
        "pageup"   => K::PageUp,
        "pagedown" => K::PageDown,
        "insert"   => K::Insert,
        // Symbols
        "-" | "minus" => K::Minus,
        "=" | "equals" | "equal" => K::Equals,
        "," | "comma" => K::Comma,
        "." | "period" | "dot" => K::Period,
        "[" | "openbracket" | "bracketleft" => K::OpenBracket,
        "]" | "closebracket" | "bracketright" => K::CloseBracket,
        ";" | "semicolon" => K::Semicolon,
        "'" | "quote" => K::Quote,
        "/" | "slash" => K::Slash,
        "\\" | "backslash" => K::Backslash,
        "`" | "backtick" => K::Backtick,
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
    // Unbound until set: the empty string binds nothing.
    #[serde(default)] pub line: String,
    /// Dashed or solid border for new rectangles, ellipses, lines and
    /// arrows (the button beside those tools).
    #[serde(default)] pub dashed_border: String,
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
            line: String::new(), dashed_border: String::new(),
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
    /// Glide the view so everything on the slide shows.
    #[serde(default = "d_zoom_to_fit")]   pub zoom_to_fit: String,
    /// Glide the view so the selection fills it (the Select tool's).
    #[serde(default = "d_zoom_to_selection")] pub zoom_to_selection: String,
    /// Held: the view goes to the selection (the whole slide with
    /// nothing selected), and back when let go. A quick tap stays.
    #[serde(default = "d_hold_zoom_to_selection")] pub hold_zoom_to_selection: String,
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
    /// Move the active slide one place toward the first / the last, as
    /// the slide panel's menu does. Unbound until set.
    #[serde(default)] pub move_slide_up: String,
    #[serde(default)] pub move_slide_down: String,
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
    /// Move the selection 1 or 10 screen pixels. While objects are
    /// selected these win over the slide and layer keys, so they can
    /// share the arrows with them (the defaults do).
    #[serde(default = "d_nudge_left_1px")]   pub nudge_left_1px: String,
    #[serde(default = "d_nudge_right_1px")]  pub nudge_right_1px: String,
    #[serde(default = "d_nudge_up_1px")]     pub nudge_up_1px: String,
    #[serde(default = "d_nudge_down_1px")]   pub nudge_down_1px: String,
    #[serde(default = "d_nudge_left_10px")]  pub nudge_left_10px: String,
    #[serde(default = "d_nudge_right_10px")] pub nudge_right_10px: String,
    #[serde(default = "d_nudge_up_10px")]    pub nudge_up_10px: String,
    #[serde(default = "d_nudge_down_10px")]  pub nudge_down_10px: String,

    // Video / GIF playback. They act on the selected video or GIF, else
    // the one under the pointer, else the only one on the slide.
    #[serde(default = "d_media_play_pause")] pub media_play_pause: String,
    #[serde(default = "d_media_prev_frame")] pub media_prev_frame: String,
    #[serde(default = "d_media_next_frame")] pub media_next_frame: String,
    /// Loop the video or GIF, or stop looping it (the media bar's button).
    #[serde(default)] pub media_loop: String,

    // The slide panel's, layer panel's, slide window's and pin bar's
    // buttons. Unbound until set.
    /// A new slide after the last one.
    #[serde(default)] pub new_slide: String,
    /// Fold the slide panel, or unfold it.
    #[serde(default)] pub toggle_slide_panel: String,
    /// Ctrl+Tab steps through slides in order, or recently used first.
    #[serde(default)] pub toggle_slide_order: String,
    /// Show the active slide in the slide window, or close the window.
    #[serde(default)] pub slide_window: String,
    #[serde(default)] pub new_layer: String,
    /// The active layer goes, as with the layer panel's bin: no undo.
    #[serde(default)] pub delete_layer: String,
    /// The active layer one place up / down the stack. Like the panel's
    /// arrows, this clears the slide's undo history.
    #[serde(default)] pub move_layer_up: String,
    #[serde(default)] pub move_layer_down: String,
    /// With a pin selected: go to the slide its objects are on.
    #[serde(default)] pub pin_go_to_slide: String,
    /// With a pin selected: show it on that slide only, or on every slide.
    #[serde(default)] pub pin_this_slide_only: String,
}

/// The toolbar's buttons that are not tools. Unbound until set.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolbarSection {
    /// Pick the palette's first … eighth colour (recolours a selection).
    pub colour_1: String,
    pub colour_2: String,
    pub colour_3: String,
    pub colour_4: String,
    pub colour_5: String,
    pub colour_6: String,
    pub colour_7: String,
    pub colour_8: String,
    /// Pen size (or the spotlight's) up / down, a wheel notch at a time
    /// (resizes a selection).
    pub size_up: String,
    pub size_down: String,
    /// The Open dialog for pictures, videos, GIFs and PureRef boards.
    pub insert_media: String,
    /// Highlight every picture / GIF / video on the slide, or stop.
    pub highlight_pictures: String,
    pub highlight_gifs: String,
    pub highlight_videos: String,
    /// Select what is highlighted, ready to arrange.
    pub select_highlighted: String,
    /// Pin the overlay to a window (then click it), or unpin it.
    pub pin_to_window: String,
    /// Background opacity up / down by 10 %.
    pub background_up: String,
    pub background_down: String,
    /// Back to a black background.
    pub reset_background: String,
    /// Text labels in a box: new ones from the Text tool, and the
    /// selected ones.
    pub text_box: String,
}

/// The arrange bar's buttons, for the Select tool's selection. Unbound
/// until set.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ArrangeSection {
    pub pack: String,
    pub group_by_type: String,
    /// Aligning slides objects until they touch, or piles them up.
    pub keep_apart: String,
    pub align_left: String,
    pub align_center_x: String,
    pub align_right: String,
    pub align_top: String,
    pub align_center_y: String,
    pub align_bottom: String,
    pub distribute_x: String,
    pub distribute_y: String,
    pub match_narrowest_width: String,
    pub match_widest_width: String,
    pub match_shortest_height: String,
    pub match_tallest_height: String,
    /// Open "Move to slide…".
    pub move_to_slide: String,
    /// Mirror the selection left to right / top to bottom.
    pub flip_horizontal: String,
    pub flip_vertical: String,
    /// The selected picture's colours, to draw with or keep as the palette.
    pub picture_colours: String,
}

impl Default for CanvasSection {
    fn default() -> Self {
        Self {
            undo: d_undo(), redo: d_redo(),
            clear_layer: d_clear_layer(), delete_canvas: d_delete_canvas(),
            reset_view: d_reset_view(), reset_layer_pos: d_reset_layer_pos(),
            zoom_to_fit: d_zoom_to_fit(), zoom_to_selection: d_zoom_to_selection(),
            hold_zoom_to_selection: d_hold_zoom_to_selection(),
            reset_layer_zoom: d_reset_layer_zoom(),
            freeze_frame: d_freeze_frame(),
            prev_canvas: d_prev_canvas(), next_canvas: d_next_canvas(),
            cycle_next_slide: d_cycle_next_slide(), cycle_prev_slide: d_cycle_prev_slide(),
            first_slide: d_first_slide(), last_slide: d_last_slide(),
            rename_slide: d_rename_slide(),
            lock_slide: d_lock_slide(),
            move_slide_up: String::new(), move_slide_down: String::new(),
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
            nudge_left_1px: d_nudge_left_1px(),
            nudge_right_1px: d_nudge_right_1px(),
            nudge_up_1px: d_nudge_up_1px(),
            nudge_down_1px: d_nudge_down_1px(),
            nudge_left_10px: d_nudge_left_10px(),
            nudge_right_10px: d_nudge_right_10px(),
            nudge_up_10px: d_nudge_up_10px(),
            nudge_down_10px: d_nudge_down_10px(),
            media_play_pause: d_media_play_pause(),
            media_prev_frame: d_media_prev_frame(),
            media_next_frame: d_media_next_frame(),
            media_loop: String::new(),
            new_slide: String::new(),
            toggle_slide_panel: String::new(),
            toggle_slide_order: String::new(),
            slide_window: String::new(),
            new_layer: String::new(),
            delete_layer: String::new(),
            move_layer_up: String::new(),
            move_layer_down: String::new(),
            pin_go_to_slide: String::new(),
            pin_this_slide_only: String::new(),
        }
    }
}

fn d_undo()          -> String { "ctrl+z".into() }
fn d_redo()          -> String { "ctrl+shift+z".into() }
fn d_clear_layer()   -> String { "ctrl+delete".into() }
fn d_delete_canvas() -> String { "ctrl+shift+backspace".into() }
fn d_reset_view()    -> String { "ctrl+0".into() }
fn d_zoom_to_fit()   -> String { "ctrl+1".into() }
fn d_zoom_to_selection() -> String { "ctrl+2".into() }
fn d_hold_zoom_to_selection() -> String { "c".into() }
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
fn d_nudge_left_1px()   -> String { "arrowleft".into() }
fn d_nudge_right_1px()  -> String { "arrowright".into() }
fn d_nudge_up_1px()     -> String { "arrowup".into() }
fn d_nudge_down_1px()   -> String { "arrowdown".into() }
fn d_nudge_left_10px()  -> String { "shift+arrowleft".into() }
fn d_nudge_right_10px() -> String { "shift+arrowright".into() }
fn d_nudge_up_10px()    -> String { "shift+arrowup".into() }
fn d_nudge_down_10px()  -> String { "shift+arrowdown".into() }
fn d_media_play_pause() -> String { "k".into() }
fn d_media_prev_frame() -> String { "comma".into() }
fn d_media_next_frame() -> String { "period".into() }

/// Current file format. 2 moved the global defaults off Ctrl+Shift; 3
/// made every button bindable and lists each binding in the file; 4
/// added the nudge keys; 5 zoom to fit / to the selection (and the held
/// peek), flipping, the text box and a picture's colours; 6 moving
/// slides.
const FORMAT: u32 = 6;

/// Top-level hotkeys file shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotkeysConfig {
    /// Format the file was written in. Files from before it was
    /// recorded read as 0. An older file is upgraded once (`upgrade_text`,
    /// `add_missing_keys`); after that a binding set back to an old
    /// default, or a line deleted, stays so.
    #[serde(default)] pub version: u32,
    #[serde(default)] pub global: GlobalSection,
    #[serde(default)] pub tool:   ToolSection,
    #[serde(default)] pub toolbar: ToolbarSection,
    #[serde(default)] pub arrange: ArrangeSection,
    #[serde(default)] pub canvas: CanvasSection,
}

impl Default for HotkeysConfig {
    fn default() -> Self {
        Self {
            version: FORMAT,
            global: GlobalSection::default(),
            tool: ToolSection::default(),
            toolbar: ToolbarSection::default(),
            arrange: ArrangeSection::default(),
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
    pub line: Option<Binding>,
    pub dashed_border: Option<Binding>,
}

#[derive(Debug, Clone, Copy)]
pub struct ToolbarBindings {
    /// `colour[N]` picks the palette's colour N + 1.
    pub colour: [Option<Binding>; 8],
    pub size_up: Option<Binding>,
    pub size_down: Option<Binding>,
    pub insert_media: Option<Binding>,
    /// Indexed like `MediaType::ALL`: pictures, GIFs, videos.
    pub highlight: [Option<Binding>; 3],
    pub select_highlighted: Option<Binding>,
    pub pin_to_window: Option<Binding>,
    pub background_up: Option<Binding>,
    pub background_down: Option<Binding>,
    pub reset_background: Option<Binding>,
    pub text_box: Option<Binding>,
}

#[derive(Debug, Clone, Copy)]
pub struct ArrangeBindings {
    pub pack: Option<Binding>,
    pub group_by_type: Option<Binding>,
    pub keep_apart: Option<Binding>,
    pub align_left: Option<Binding>,
    pub align_center_x: Option<Binding>,
    pub align_right: Option<Binding>,
    pub align_top: Option<Binding>,
    pub align_center_y: Option<Binding>,
    pub align_bottom: Option<Binding>,
    pub distribute_x: Option<Binding>,
    pub distribute_y: Option<Binding>,
    pub match_narrowest_width: Option<Binding>,
    pub match_widest_width: Option<Binding>,
    pub match_shortest_height: Option<Binding>,
    pub match_tallest_height: Option<Binding>,
    pub move_to_slide: Option<Binding>,
    pub flip_horizontal: Option<Binding>,
    pub flip_vertical: Option<Binding>,
    pub picture_colours: Option<Binding>,
}

#[derive(Debug, Clone, Copy)]
pub struct CanvasBindings {
    pub undo: Option<Binding>,
    pub redo: Option<Binding>,
    pub clear_layer: Option<Binding>,
    pub delete_canvas: Option<Binding>,
    pub reset_view: Option<Binding>,
    pub zoom_to_fit: Option<Binding>,
    pub zoom_to_selection: Option<Binding>,
    pub hold_zoom_to_selection: Option<Binding>,
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
    pub move_slide_up: Option<Binding>,
    pub move_slide_down: Option<Binding>,
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
    /// Left, right, up, down; by 1 / by 10 pixels.
    pub nudge_1px: [Option<Binding>; 4],
    pub nudge_10px: [Option<Binding>; 4],
    pub media_play_pause: Option<Binding>,
    pub media_prev_frame: Option<Binding>,
    pub media_next_frame: Option<Binding>,
    pub media_loop: Option<Binding>,
    pub new_slide: Option<Binding>,
    pub toggle_slide_panel: Option<Binding>,
    pub toggle_slide_order: Option<Binding>,
    pub slide_window: Option<Binding>,
    pub new_layer: Option<Binding>,
    pub delete_layer: Option<Binding>,
    pub move_layer_up: Option<Binding>,
    pub move_layer_down: Option<Binding>,
    pub pin_go_to_slide: Option<Binding>,
    pub pin_this_slide_only: Option<Binding>,
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
    pub toolbar: ToolbarBindings,
    pub arrange: ArrangeBindings,
    pub canvas: CanvasBindings,
}

impl HotkeysConfig {
    /// Compile every string in the file into a `Binding`. Bad strings
    /// become `None` and silently disable that binding (logged at
    /// warn level); an empty one is a binding left unset.
    pub fn parsed(&self) -> ParsedHotkeys {
        let p = |label: &str, s: &str| -> Option<Binding> {
            if s.trim().is_empty() {
                return None;
            }
            match parse(s) {
                Some(b) => Some(b),
                None => {
                    log::warn!("hotkey '{label}' has invalid binding '{s}' — disabled");
                    None
                }
            }
        };
        let (tb, ar, cv) = (&self.toolbar, &self.arrange, &self.canvas);
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
                line:           p("tool.line",           &self.tool.line),
                dashed_border:  p("tool.dashed_border",  &self.tool.dashed_border),
            },
            toolbar: ToolbarBindings {
                colour: [
                    p("toolbar.colour_1", &tb.colour_1),
                    p("toolbar.colour_2", &tb.colour_2),
                    p("toolbar.colour_3", &tb.colour_3),
                    p("toolbar.colour_4", &tb.colour_4),
                    p("toolbar.colour_5", &tb.colour_5),
                    p("toolbar.colour_6", &tb.colour_6),
                    p("toolbar.colour_7", &tb.colour_7),
                    p("toolbar.colour_8", &tb.colour_8),
                ],
                size_up:            p("toolbar.size_up",            &tb.size_up),
                size_down:          p("toolbar.size_down",          &tb.size_down),
                insert_media:       p("toolbar.insert_media",       &tb.insert_media),
                highlight: [
                    p("toolbar.highlight_pictures", &tb.highlight_pictures),
                    p("toolbar.highlight_gifs",     &tb.highlight_gifs),
                    p("toolbar.highlight_videos",   &tb.highlight_videos),
                ],
                select_highlighted: p("toolbar.select_highlighted", &tb.select_highlighted),
                pin_to_window:      p("toolbar.pin_to_window",      &tb.pin_to_window),
                background_up:      p("toolbar.background_up",      &tb.background_up),
                background_down:    p("toolbar.background_down",    &tb.background_down),
                reset_background:   p("toolbar.reset_background",   &tb.reset_background),
                text_box:           p("toolbar.text_box",           &tb.text_box),
            },
            arrange: ArrangeBindings {
                pack:           p("arrange.pack",           &ar.pack),
                group_by_type:  p("arrange.group_by_type",  &ar.group_by_type),
                keep_apart:     p("arrange.keep_apart",     &ar.keep_apart),
                align_left:     p("arrange.align_left",     &ar.align_left),
                align_center_x: p("arrange.align_center_x", &ar.align_center_x),
                align_right:    p("arrange.align_right",    &ar.align_right),
                align_top:      p("arrange.align_top",      &ar.align_top),
                align_center_y: p("arrange.align_center_y", &ar.align_center_y),
                align_bottom:   p("arrange.align_bottom",   &ar.align_bottom),
                distribute_x:   p("arrange.distribute_x",   &ar.distribute_x),
                distribute_y:   p("arrange.distribute_y",   &ar.distribute_y),
                match_narrowest_width: p("arrange.match_narrowest_width", &ar.match_narrowest_width),
                match_widest_width:    p("arrange.match_widest_width",    &ar.match_widest_width),
                match_shortest_height: p("arrange.match_shortest_height", &ar.match_shortest_height),
                match_tallest_height:  p("arrange.match_tallest_height",  &ar.match_tallest_height),
                move_to_slide:  p("arrange.move_to_slide",  &ar.move_to_slide),
                flip_horizontal: p("arrange.flip_horizontal", &ar.flip_horizontal),
                flip_vertical:   p("arrange.flip_vertical",   &ar.flip_vertical),
                picture_colours: p("arrange.picture_colours", &ar.picture_colours),
            },
            canvas: CanvasBindings {
                undo:          p("canvas.undo",          &self.canvas.undo),
                redo:          p("canvas.redo",          &self.canvas.redo),
                clear_layer:   p("canvas.clear_layer",   &self.canvas.clear_layer),
                delete_canvas: p("canvas.delete_canvas", &self.canvas.delete_canvas),
                reset_view:    p("canvas.reset_view",    &self.canvas.reset_view),
                zoom_to_fit:   p("canvas.zoom_to_fit",   &cv.zoom_to_fit),
                zoom_to_selection: p("canvas.zoom_to_selection", &cv.zoom_to_selection),
                hold_zoom_to_selection: p("canvas.hold_zoom_to_selection", &cv.hold_zoom_to_selection),
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
                move_slide_up:   p("canvas.move_slide_up",   &cv.move_slide_up),
                move_slide_down: p("canvas.move_slide_down", &cv.move_slide_down),
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
                nudge_1px: [
                    p("canvas.nudge_left_1px",  &cv.nudge_left_1px),
                    p("canvas.nudge_right_1px", &cv.nudge_right_1px),
                    p("canvas.nudge_up_1px",    &cv.nudge_up_1px),
                    p("canvas.nudge_down_1px",  &cv.nudge_down_1px),
                ],
                nudge_10px: [
                    p("canvas.nudge_left_10px",  &cv.nudge_left_10px),
                    p("canvas.nudge_right_10px", &cv.nudge_right_10px),
                    p("canvas.nudge_up_10px",    &cv.nudge_up_10px),
                    p("canvas.nudge_down_10px",  &cv.nudge_down_10px),
                ],
                media_play_pause: p("canvas.media_play_pause", &self.canvas.media_play_pause),
                media_prev_frame: p("canvas.media_prev_frame", &self.canvas.media_prev_frame),
                media_next_frame: p("canvas.media_next_frame", &self.canvas.media_next_frame),
                media_loop:       p("canvas.media_loop",       &cv.media_loop),
                new_slide:          p("canvas.new_slide",          &cv.new_slide),
                toggle_slide_panel: p("canvas.toggle_slide_panel", &cv.toggle_slide_panel),
                toggle_slide_order: p("canvas.toggle_slide_order", &cv.toggle_slide_order),
                slide_window:       p("canvas.slide_window",       &cv.slide_window),
                new_layer:          p("canvas.new_layer",          &cv.new_layer),
                delete_layer:       p("canvas.delete_layer",       &cv.delete_layer),
                move_layer_up:      p("canvas.move_layer_up",      &cv.move_layer_up),
                move_layer_down:    p("canvas.move_layer_down",    &cv.move_layer_down),
                pin_go_to_slide:    p("canvas.pin_go_to_slide",    &cv.pin_go_to_slide),
                pin_this_slide_only: p("canvas.pin_this_slide_only", &cv.pin_this_slide_only),
            },
        }
    }

    /// Every pair of in-app bindings (`[tool]`, `[toolbar]`, `[arrange]`,
    /// `[canvas]`) set to the same keys, as `(name, name, keys)`. Both
    /// would fire on one press, so the file's loader warns about them —
    /// all but a nudge key on a slide or layer key, where one press does
    /// only one of them (`nudge_shares_with`).
    pub fn clashes(&self) -> Vec<(String, String, String)> {
        let Ok(value) = toml::Value::try_from(self) else { return Vec::new() };
        let mut seen: Vec<(String, Binding)> = Vec::new();
        let mut out = Vec::new();
        for section in ["tool", "toolbar", "arrange", "canvas"] {
            let Some(table) = value.get(section).and_then(|v| v.as_table()) else { continue };
            for (name, s) in table {
                let Some(b) = s.as_str().and_then(parse) else { continue };
                let name = format!("{section}.{name}");
                if let Some((other, _)) = seen.iter().find(|(o, ob)| *ob == b && !nudge_shares_with(o, &name)) {
                    out.push((other.clone(), name.clone(), b.label()));
                }
                seen.push((name, b));
            }
        }
        out
    }
}

/// A nudge key and a slide or layer key (`interaction::react_canvas_nav`,
/// `react_layer_nav`) may be the same keys, as the arrows are by default:
/// with objects selected the press nudges them, else it goes to the
/// slide or layer.
fn nudge_shares_with(a: &str, b: &str) -> bool {
    const NAV: [&str; 8] = [
        "canvas.prev_canvas", "canvas.next_canvas", "canvas.cycle_prev_slide", "canvas.cycle_next_slide",
        "canvas.first_slide", "canvas.last_slide", "canvas.prev_layer", "canvas.next_layer",
    ];
    let nudge = |n: &str| n.starts_with("canvas.nudge_");
    (nudge(a) && NAV.contains(&b)) || (nudge(b) && NAV.contains(&a))
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
        let loaded = Self::load(&exe_path()).or_else(|| user_config_path().and_then(|p| Self::load(&p)));
        if let Some(c) = loaded {
            for (a, b, keys) in c.clashes() {
                log::warn!("hotkeys {a} and {b} are both {keys}: one press does both");
            }
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
        let toml_str = annotate(&toml_str);
        let header = "# On-Screen Notes — hotkeys.toml\n\
                      # Strings are case-insensitive. Modifiers: ctrl, shift, alt.\n\
                      # Special keys: arrowleft/right/up/down, home, end, pageup,\n\
                      # pagedown, insert, backspace, delete, enter, escape, tab,\n\
                      # space, f1..f12, and [ ] ; ' / \\ ` , . - =.\n\
                      # \"\" leaves an action unbound.\n\n";
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
        let upgraded = add_missing_keys(&upgrade_text(&text, cfg.version));
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

/// Bring the text of a hotkeys file written in format `from` up to
/// `FORMAT`: from before 2, each `[global]` binding still on its old
/// default moves to the new one; the format is recorded so this happens
/// once. Comments, layout and the user's own bindings are kept as
/// written. (`add_missing_keys` does format 3's and 4's part.)
fn upgrade_text(text: &str, from: u32) -> String {
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
        if section == "global" && from < 2 {
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

/// What the binding `key` in `[section]` does, as the file says it next
/// to the key. Every binding has one (a test checks).
fn note(section: &str, key: &str) -> Option<String> {
    if let Some(n) = key.strip_prefix("opacity_").and_then(|n| n.parse::<u32>().ok()) {
        return Some(format!("Active layer's opacity to {} %", n * 10));
    }
    if let Some(n) = key.strip_prefix("colour_").and_then(|n| n.parse::<u32>().ok()) {
        return Some(format!("Palette colour {n} (recolours a selection)"));
    }
    let text = match (section, key) {
        ("global", "toggle_overlay") => "Show / hide the overlay, from any app",
        ("global", "screenshot")     => "Screenshot of the screen and the ink, from any app",
        ("global", "start_record")   => "Start / stop recording, from any app",
        ("global", "pause_record")   => "Pause / resume recording, from any app",

        ("tool", "pen")            => "Pen",
        ("tool", "pencil")         => "Pencil",
        ("tool", "marker")         => "Marker (highlighter)",
        ("tool", "airbrush")       => "Airbrush (spray)",
        ("tool", "fill")           => "Fill",
        ("tool", "laser")          => "Laser pointer",
        ("tool", "text")           => "Text",
        ("tool", "spotlight")      => "Spotlight (again: back to the pen)",
        ("tool", "lasso")          => "Lasso erase",
        ("tool", "lasso_select")   => "Lasso capture: screen pixels to a picture",
        ("tool", "select")         => "Select",
        ("tool", "eyedropper")     => "Eyedropper, for one pick",
        ("tool", "rect")           => "Rectangle",
        ("tool", "ellipse")        => "Ellipse",
        ("tool", "arrow")          => "Straight arrow",
        ("tool", "freehand_arrow") => "Freehand arrow",
        ("tool", "line")           => "Line",
        ("tool", "dashed_border")  => "Dashed / solid border for new rectangles, ellipses, lines, arrows",

        ("toolbar", "size_up")            => "Pen size up a wheel notch (Spotlight: its size; resizes a selection)",
        ("toolbar", "size_down")          => "Pen size down a wheel notch (Spotlight: its size; resizes a selection)",
        ("toolbar", "insert_media")       => "Insert a picture, video, GIF or PureRef board",
        ("toolbar", "highlight_pictures") => "Highlight the slide's pictures, or stop",
        ("toolbar", "highlight_gifs")     => "Highlight the slide's GIFs, or stop",
        ("toolbar", "highlight_videos")   => "Highlight the slide's videos, or stop",
        ("toolbar", "select_highlighted") => "Select what is highlighted, ready to arrange",
        ("toolbar", "pin_to_window")      => "Pin the overlay to a window (then click it), or unpin it",
        ("toolbar", "background_up")      => "Background opacity +10 %",
        ("toolbar", "background_down")    => "Background opacity -10 %",
        ("toolbar", "reset_background")   => "Black background",
        ("toolbar", "text_box")           => "Text in a box: new labels, and the selected ones",

        ("arrange", "pack")                  => "Pack the selection into a tight block, sizes kept",
        ("arrange", "group_by_type")         => "Group the selection: pictures, GIFs, videos, the rest",
        ("arrange", "keep_apart")            => "Keep-apart switch: aligning slides objects until they touch, or piles them up",
        ("arrange", "align_left")            => "Align the selection left",
        ("arrange", "align_center_x")        => "Align the selection's centres across",
        ("arrange", "align_right")           => "Align the selection right",
        ("arrange", "align_top")             => "Align the selection to the top",
        ("arrange", "align_center_y")        => "Align the selection's centres down",
        ("arrange", "align_bottom")          => "Align the selection to the bottom",
        ("arrange", "distribute_x")          => "Equal gaps across (3 or more selected)",
        ("arrange", "distribute_y")          => "Equal gaps down (3 or more selected)",
        ("arrange", "match_narrowest_width") => "Every selected object as wide as the narrowest",
        ("arrange", "match_widest_width")    => "Every selected object as wide as the widest",
        ("arrange", "match_shortest_height") => "Every selected object as tall as the shortest",
        ("arrange", "match_tallest_height")  => "Every selected object as tall as the tallest",
        ("arrange", "move_to_slide")         => "Open \"Move to slide...\" for the selection",
        ("arrange", "flip_horizontal")       => "Mirror the selection left to right",
        ("arrange", "flip_vertical")         => "Mirror the selection top to bottom",
        ("arrange", "picture_colours")       => "The selected picture's colours, to draw with or keep as the palette",

        ("canvas", "undo")                 => "Undo",
        ("canvas", "redo")                 => "Redo",
        ("canvas", "clear_layer")          => "Clear the active layer",
        ("canvas", "delete_canvas")        => "Delete the slide",
        ("canvas", "reset_view")           => "Reset zoom and pan",
        ("canvas", "zoom_to_fit")          => "Zoom to fit everything on the slide",
        ("canvas", "zoom_to_selection")    => "Zoom to the selection",
        ("canvas", "hold_zoom_to_selection") => "Hold: look at the selection (or the whole slide), back when let go",
        ("canvas", "reset_layer_pos")      => "Undo the active layer's moves",
        ("canvas", "reset_layer_zoom")     => "Undo the active layer's zooms",
        ("canvas", "freeze_frame")         => "Freeze the screen into a new layer",
        ("canvas", "prev_canvas")          => "Previous slide",
        ("canvas", "next_canvas")          => "Next slide (past the last: a new one)",
        ("canvas", "cycle_next_slide")     => "Next slide, wrapping round (or recently used)",
        ("canvas", "cycle_prev_slide")     => "Previous slide, wrapping round",
        ("canvas", "first_slide")          => "First slide",
        ("canvas", "last_slide")           => "Last slide",
        ("canvas", "rename_slide")         => "Name the slide",
        ("canvas", "lock_slide")           => "Lock / unlock the slide against edits",
        ("canvas", "move_slide_up")        => "Move the slide one place toward the first",
        ("canvas", "move_slide_down")      => "Move the slide one place toward the last",
        ("canvas", "prev_layer")           => "Layer below",
        ("canvas", "next_layer")           => "Layer above",
        ("canvas", "toggle_ui")            => "Hide / show the toolbar and panels",
        ("canvas", "opacity_dec")          => "Active layer's opacity -10 %",
        ("canvas", "opacity_inc")          => "Active layer's opacity +10 %",
        ("canvas", "preview_layer")        => "Preview the active layer",
        ("canvas", "toggle_layer_visible") => "Hide / show the active layer",
        ("canvas", "delete_selection")     => "Delete the selection",
        ("canvas", "duplicate")            => "Duplicate the selection",
        ("canvas", "copy_to_new_slide")    => "Copy the selection to a new slide",
        ("canvas", "pin_selection")        => "Pin the selection to every slide, or unpin it",
        ("canvas", "toggle_pins")          => "Hide / show every pin",
        ("canvas", "bring_front")          => "Selection to the front",
        ("canvas", "send_back")            => "Selection to the back",
        ("canvas", "nudge_left_1px")       => "Selection 1 px left (wins over slide / layer keys while selected)",
        ("canvas", "nudge_right_1px")      => "Selection 1 px right",
        ("canvas", "nudge_up_1px")         => "Selection 1 px up",
        ("canvas", "nudge_down_1px")       => "Selection 1 px down",
        ("canvas", "nudge_left_10px")      => "Selection 10 px left",
        ("canvas", "nudge_right_10px")     => "Selection 10 px right",
        ("canvas", "nudge_up_10px")        => "Selection 10 px up",
        ("canvas", "nudge_down_10px")      => "Selection 10 px down",
        ("canvas", "media_play_pause")     => "Play / pause the video or GIF",
        ("canvas", "media_prev_frame")     => "Video or GIF: previous frame",
        ("canvas", "media_next_frame")     => "Video or GIF: next frame",
        ("canvas", "media_loop")           => "Loop the video or GIF, or stop looping",
        ("canvas", "new_slide")            => "New slide after the last one",
        ("canvas", "toggle_slide_panel")   => "Fold / unfold the slide panel",
        ("canvas", "toggle_slide_order")   => "Ctrl+Tab: slides in order, or recently used first",
        ("canvas", "slide_window")         => "Show the slide in the slide window, or close it",
        ("canvas", "new_layer")            => "New layer",
        ("canvas", "delete_layer")         => "Delete the active layer (no undo)",
        ("canvas", "move_layer_up")        => "Active layer one place up (clears the undo history)",
        ("canvas", "move_layer_down")      => "Active layer one place down (clears the undo history)",
        ("canvas", "pin_go_to_slide")      => "Selected pin: go to the slide its objects are on",
        ("canvas", "pin_this_slide_only")  => "Selected pin: show it on its slide only, or on every slide",
        _ => return None,
    };
    Some(text.to_owned())
}

/// `lines`, one section's `key = value` lines, each followed by its
/// key's note (`note`), the notes lined up.
fn with_notes(section: &str, lines: &[String]) -> Vec<String> {
    let width = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    lines
        .iter()
        .map(|l| {
            let key = l.split_once('=').map(|(k, _)| k.trim().trim_matches('"'));
            match key.and_then(|k| note(section, k)) {
                Some(n) => format!("{l:<width$}  # {n}"),
                None => l.clone(),
            }
        })
        .collect()
}

/// `text`, a file as `toml` writes it, with a note after every binding.
fn annotate(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 3);
    let mut section = String::new();
    let mut block: Vec<String> = Vec::new();
    let flush = |out: &mut String, section: &str, block: &mut Vec<String>| {
        for l in with_notes(section, block) {
            out.push_str(&l);
            out.push('\n');
        }
        block.clear();
    };
    for line in text.lines() {
        if !section.is_empty() && table_header(line).is_none() && line.contains('=') {
            block.push(line.to_owned());
            continue;
        }
        flush(&mut out, &section, &mut block);
        if let Some(name) = table_header(line) {
            section = name.to_owned();
        }
        out.push_str(line);
        out.push('\n');
    }
    flush(&mut out, &section, &mut block);
    out
}

/// The name of the table a `[name]` line opens, if `line` is one.
fn table_header(line: &str) -> Option<&str> {
    let t = line.trim();
    let rest = t.strip_prefix('[')?;
    Some(rest[..rest.find(']')?].trim())
}

/// `text` with every binding it does not list added at its default —
/// `""`, unbound, for most — so each action the app can bind shows up in
/// the file, ready to be set. They go under a comment after the last
/// line of their section; a section the file lacks goes at its end.
/// Comments, layout and the user's own lines are kept as written. Run
/// once per format upgrade (format 3 on, every one of them adds keys),
/// so a line deleted later stays so until the next format.
fn add_missing_keys(text: &str) -> String {
    let Ok(have) = toml::from_str::<toml::Table>(text) else { return text.to_owned() };
    let Ok(defaults) = toml::to_string_pretty(&HotkeysConfig::default()) else { return text.to_owned() };
    // Each section's missing lines, in the order the app lists them.
    let mut missing: Vec<(String, Vec<String>)> = Vec::new();
    let mut section = "";
    for line in defaults.lines() {
        if let Some(name) = table_header(line) {
            section = name;
            continue;
        }
        let Some((key, _)) = line.split_once('=') else { continue };
        let key = key.trim().trim_matches('"');
        let listed = have.get(section).and_then(|v| v.as_table()).is_some_and(|t| t.contains_key(key));
        if section.is_empty() || listed {
            continue;
        }
        match missing.last_mut() {
            Some((s, lines)) if s == section => lines.push(line.trim().to_owned()),
            _ => missing.push((section.to_owned(), vec![line.trim().to_owned()])),
        }
    }
    if missing.is_empty() {
        return text.to_owned();
    }

    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let block = |section: &str, lines: &[String]| {
        let mut s = format!("# Added by an update; \"\" leaves an action unbound.{nl}");
        for l in with_notes(section, lines) {
            s.push_str(&l);
            s.push_str(nl);
        }
        s
    };
    let mut out = String::with_capacity(text.len() + 2048);
    let mut placed: Vec<&str> = Vec::new();
    // One section's lines at a time, its header first; the lines before
    // the first header belong to none.
    let mut flush = |out: &mut String, name: Option<&str>, lines: &[&str]| {
        let lines_for = name.and_then(|n| missing.iter().find(|(s, _)| s == n));
        let Some((name, add)) = lines_for else {
            lines.iter().for_each(|l| out.push_str(l));
            return;
        };
        // After the last line with something on it, before the blank
        // lines that part it from the next section.
        let split = lines.iter().rposition(|l| !l.trim().is_empty()).map_or(0, |i| i + 1);
        lines[..split].iter().for_each(|l| out.push_str(l));
        if !out.is_empty() && !out.ends_with('\n') {
            out.push_str(nl);
        }
        out.push_str(&block(name, add));
        lines[split..].iter().for_each(|l| out.push_str(l));
        placed.push(name);
    };
    let mut name: Option<&str> = None;
    let mut lines: Vec<&str> = Vec::new();
    for line in text.split_inclusive('\n') {
        if let Some(next) = table_header(line) {
            flush(&mut out, name, &lines);
            lines.clear();
            name = Some(next);
        }
        lines.push(line);
    }
    flush(&mut out, name, &lines);
    for (name, add) in &missing {
        if placed.contains(&name.as_str()) {
            continue;
        }
        if !out.is_empty() && !out.ends_with('\n') {
            out.push_str(nl);
        }
        out.push_str(&format!("{nl}[{name}]{nl}"));
        out.push_str(&block(name, add));
    }
    out
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
        // Every in-app default, found through serde so a binding added
        // later is checked without listing it here. Unbound is "".
        let cfg = HotkeysConfig::default();
        let value = toml::Value::try_from(&cfg).unwrap();
        let mut bound = 0;
        for section in ["tool", "toolbar", "arrange", "canvas"] {
            for (name, s) in value[section].as_table().unwrap() {
                let s = s.as_str().unwrap();
                if s.is_empty() { continue; }
                parse(s).unwrap_or_else(|| panic!("{section}.{name} = {s:?} does not parse"));
                bound += 1;
            }
        }
        assert!(bound > 50, "only {bound} default bindings found");
        assert_eq!(cfg.clashes(), vec![]);
        assert_eq!(cfg.parsed().canvas.toggle_layer_visible.map(|b| b.key), Some(egui::Key::H));
    }

    #[test]
    fn button_actions_start_unbound_and_take_any_key() {
        let keys = HotkeysConfig::default().parsed();
        assert!(keys.arrange.align_left.is_none() && keys.toolbar.colour[0].is_none() && keys.tool.line.is_none());
        let cfg: HotkeysConfig = toml::from_str(
            "[arrange]\nalign_left = \"alt+a\"\nkeep_apart = \"[\"\n\
             [toolbar]\ncolour_1 = \"shift+1\"\nsize_up = \"]\"\n\
             [canvas]\nnew_layer = \"ctrl+shift+n\"\n",
        )
        .unwrap();
        let keys = cfg.parsed();
        let b = |b: Option<Binding>| b.map(|b| (b.key, b.ctrl, b.shift, b.alt));
        assert_eq!(b(keys.arrange.align_left), Some((egui::Key::A, false, false, true)));
        assert_eq!(b(keys.arrange.keep_apart), Some((egui::Key::OpenBracket, false, false, false)));
        assert_eq!(b(keys.toolbar.colour[0]), Some((egui::Key::Num1, false, true, false)));
        assert_eq!(b(keys.toolbar.size_up), Some((egui::Key::CloseBracket, false, false, false)));
        assert_eq!(b(keys.canvas.new_layer), Some((egui::Key::N, true, true, false)));
        // The rest keep their defaults.
        assert_eq!(cfg.canvas.undo, "ctrl+z");
        assert_eq!(parse("]").unwrap().label(), "]");
        // Bound twice: reported.
        let cfg: HotkeysConfig = toml::from_str("[arrange]\npack = \"ctrl+z\"\n").unwrap();
        assert_eq!(cfg.clashes(), vec![("arrange.pack".into(), "canvas.undo".into(), "Ctrl+Z".into())]);
    }

    /// A hotkeys file as an older build wrote it: format 2, some keys
    /// rebound, a comment, no `[toolbar]` or `[arrange]`.
    const FORMAT_2: &str = "# On-Screen Notes — hotkeys.toml\n\n\
                            version = 2\n\n\
                            [global]\n\
                            toggle_overlay = \"ctrl+shift+n\"\n\
                            screenshot = \"ctrl+alt+shift+s\"\n\n\
                            [tool]\n\
                            pen = \"q\"  # mine\n\
                            pencil = \"b\"\n\n\
                            [canvas]\n\
                            undo = \"ctrl+z\"\n";

    #[test]
    fn format_2_files_keep_their_globals_and_gain_every_key() {
        // A global set to what was the default before format 2 is the
        // user's choice by now.
        let up = upgrade_text(FORMAT_2, 2);
        assert!(up.contains("toggle_overlay = \"ctrl+shift+n\""));
        assert!(up.contains("version = 6"));

        let new = add_missing_keys(&up);
        let cfg: HotkeysConfig = toml::from_str(&new).unwrap();
        assert_eq!(cfg.version, FORMAT);
        assert_eq!(cfg.global.toggle_overlay, "ctrl+shift+n");
        assert_eq!(cfg.tool.pen, "q");
        // What was there stays as written, the new lines go at the end of
        // their section and new sections at the end of the file.
        assert!(new.starts_with("# On-Screen Notes — hotkeys.toml\n\nversion = 6\n\n[global]\ntoggle_overlay = \"ctrl+shift+n\"\n"));
        assert!(new.contains("pencil = \"b\"\n# Added by an update; \"\" leaves an action unbound.\nmarker = \"m\" "));
        assert!(new.contains("pen = \"q\"  # mine\n"));
        assert!(new.contains("  # Dashed / solid border for new rectangles, ellipses, lines, arrows\n\n[canvas]\nundo = \"ctrl+z\"\n# Added"));
        // Each added line says what it does, the notes lined up in each
        // block; the user's own lines get none.
        let added = |key: &str| new.lines().find(|l| l.starts_with(&format!("{key} = "))).unwrap().to_owned();
        assert!(added("marker").ends_with("  # Marker (highlighter)"));
        assert!(added("align_left").ends_with("  # Align the selection left"));
        assert!(added("colour_3").ends_with("  # Palette colour 3 (recolours a selection)"));
        assert!(added("delete_layer").ends_with("# Delete the active layer (no undo)"));
        assert_eq!(added("pencil"), "pencil = \"b\"");
        let column = |key: &str| added(key).find("  # ").unwrap();
        assert_eq!(column("marker"), column("dashed_border"));
        assert_eq!(column("pack"), column("match_narrowest_width"));
        let toolbar = new.find("\n[toolbar]\n").expect("toolbar section added");
        let arrange = new.find("\n[arrange]\n").expect("arrange section added");
        assert!(new.find("\n[canvas]\n").unwrap() < toolbar && toolbar < arrange);
        // Every binding the app has is listed now, and a second run adds
        // nothing.
        let listed: toml::Table = toml::from_str(&new).unwrap();
        let all = toml::Value::try_from(HotkeysConfig::default()).unwrap();
        for section in ["global", "tool", "toolbar", "arrange", "canvas"] {
            for key in all[section].as_table().unwrap().keys() {
                assert!(listed[section].as_table().unwrap().contains_key(key), "{section}.{key} not listed");
            }
        }
        assert_eq!(add_missing_keys(&new), new);
        // Line endings follow the file's.
        let crlf = add_missing_keys(&up.replace('\n', "\r\n"));
        assert!(!crlf.replace("\r\n", "").contains('\n'));
    }

    #[test]
    fn format_3_files_gain_the_nudge_keys() {
        let old = "version = 3\n\n[canvas]\nprev_canvas = \"alt+a\"\nsend_back = \"\"\n\n[arrange]\nalign_left = \"ctrl+arrowleft\"\n";
        let new = add_missing_keys(&upgrade_text(old, 3));
        assert!(new.starts_with("version = 6\n\n[canvas]\nprev_canvas = \"alt+a\"\nsend_back = \"\"\n# Added by an update"));
        let cfg: HotkeysConfig = toml::from_str(&new).unwrap();
        assert_eq!(cfg.version, FORMAT);
        assert_eq!((cfg.canvas.prev_canvas.as_str(), cfg.canvas.send_back.as_str()), ("alt+a", ""));
        assert_eq!(cfg.arrange.align_left, "ctrl+arrowleft");
        let line = |key: &str| new.lines().find(|l| l.starts_with(&format!("{key} = "))).unwrap().to_owned();
        assert!(line("nudge_left_1px").starts_with("nudge_left_1px = \"arrowleft\""));
        assert!(line("nudge_down_10px").starts_with("nudge_down_10px = \"shift+arrowdown\""));
        assert!(line("nudge_up_10px").ends_with("# Selection 10 px up"));
    }

    #[test]
    fn nudge_keys_may_share_only_slide_and_layer_keys() {
        let keys = HotkeysConfig::default().parsed().canvas;
        let b = |b: Option<Binding>| b.map(|b| (b.key, b.shift));
        assert_eq!(b(keys.nudge_1px[0]), Some((egui::Key::ArrowLeft, false)));
        assert_eq!(b(keys.nudge_10px[3]), Some((egui::Key::ArrowDown, true)));
        // The arrows are slide and layer keys too by default: no clash.
        assert_eq!(keys.prev_canvas, keys.nudge_1px[0]);
        assert_eq!(HotkeysConfig::default().clashes(), vec![]);
        let cfg: HotkeysConfig = toml::from_str("[canvas]\nnudge_left_1px = \"ctrl+z\"\nnudge_up_1px = \"home\"\n").unwrap();
        assert_eq!(cfg.clashes(), vec![("canvas.nudge_left_1px".into(), "canvas.undo".into(), "Ctrl+Z".into())]);
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
        // As a fresh install writes it: every binding with its note, and
        // the notes do not change what it reads as.
        let noted = annotate(&text);
        let back: HotkeysConfig = toml::from_str(&noted).unwrap();
        assert_eq!(toml::to_string_pretty(&back).unwrap(), text);
        assert!(noted.contains("\nalign_left "));
        for line in noted.lines().filter(|l| l.contains(" = ") && !l.starts_with("version")) {
            assert!(line.contains("  # "), "no note: {line}");
        }
    }

    /// Bring a hotkeys file up to date in place, as the app does when it
    /// starts with it beside the exe: `OSN_HOTKEYS_FILE` names the file.
    #[test]
    #[ignore]
    fn upgrade_a_hotkeys_file() {
        let path = std::env::var_os("OSN_HOTKEYS_FILE").expect("set OSN_HOTKEYS_FILE to the file to upgrade");
        let cfg = HotkeysConfig::load(Path::new(&path)).expect("the file reads");
        assert_eq!(cfg.version, FORMAT);
        for (a, b, keys) in cfg.clashes() {
            println!("{a} and {b} are both {keys}");
        }
    }

    #[test]
    fn every_binding_has_a_note() {
        let all = toml::Value::try_from(HotkeysConfig::default()).unwrap();
        for section in ["global", "tool", "toolbar", "arrange", "canvas"] {
            for key in all[section].as_table().unwrap().keys() {
                assert!(note(section, key).is_some(), "{section}.{key} has no note");
            }
        }
        assert_eq!(note("canvas", "opacity_7").as_deref(), Some("Active layer's opacity to 70 %"));
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

        let new = upgrade_text(old, 0);
        assert_eq!(
            new,
            "# On-Screen Notes — hotkeys.toml\n\n\
             version = 6\n\n\
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
        let new = upgrade_text("[canvas]\r\nundo = \"ctrl+z\"\r\n", 0);
        assert_eq!(new, "version = 6\r\n\r\n[canvas]\r\nundo = \"ctrl+z\"\r\n");
        let cfg: HotkeysConfig = toml::from_str(&new).unwrap();
        assert_eq!(cfg.global.toggle_overlay, "ctrl+alt+shift+n");

        let new = upgrade_text("version = 1", 1);
        assert_eq!(new, "version = 6");
        assert_eq!(upgrade_text("", 0), "version = 6\n");
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
