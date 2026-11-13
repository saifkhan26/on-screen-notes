//! Keys for the buttons: the arrange bar's, the toolbar's (other than
//! the tools), and the slide panel's, layer panel's, slide window's and
//! pin bar's. Each binding is in `hotkeys.toml` (`[arrange]`,
//! `[toolbar]`, `[canvas]`), unbound until set; a key only says which
//! button it presses, and the app runs it as the click would. The
//! Select tool's nudge keys come through here too (bound by default).

use crate::canvas::shape::MediaType;
use crate::input::hotkeys_config::{Binding, ParsedHotkeys};
use crate::tools::arrange::{Align, Arrange, Match};

/// A button pressed by its key.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum KeyAction {
    /// Pack, group by type or distribute the selection.
    Arrange(Arrange),
    /// Align it: lined up or kept apart, as the bar's switch says.
    Align(Align),
    Match(Match),
    /// Flip the arrange bar's "keep apart" switch.
    KeepApart,
    /// Open "Move to slide…".
    MoveToSlide,
    /// Pick the palette's colour at this index.
    Colour(usize),
    /// Pen size (or the spotlight's) one wheel notch up (+1) or down (−1).
    Size(i8),
    InsertMedia,
    Highlight(MediaType),
    SelectHighlighted,
    PinToWindow,
    /// Background opacity 10 % up (+1) or down (−1).
    Background(i8),
    ResetBackground,
    NewSlide,
    ToggleSlidePanel,
    ToggleSlideOrder,
    SlideWindow,
    NewLayer,
    DeleteLayer,
    /// Move the active layer one place up (+1) or down (−1).
    MoveLayer(i8),
    PinGoToSlide,
    PinThisSlideOnly,
    /// Move the Select tool's selection by this many screen pixels.
    Nudge([f32; 2]),
    /// Glide the view to show the whole slide.
    ZoomToFit,
    /// Glide the view to show the selection.
    ZoomToSelection,
    /// Mirror the selection left to right (`false`) or top to bottom.
    Flip(bool),
    /// Show the selected picture's colours.
    PictureColours,
    /// Text labels in a box, or not.
    TextBox,
}

/// The nudge keys' directions, in `CanvasBindings::nudge_1px` order.
const NUDGE_DIRS: [[f32; 2]; 4] = [[-1.0, 0.0], [1.0, 0.0], [0.0, -1.0], [0.0, 1.0]];

/// Every button `key` with `mods` presses, in a fixed order.
pub fn collect(key: egui::Key, mods: egui::Modifiers, keys: &ParsedHotkeys, out: &mut Vec<KeyAction>) {
    let hit = |b: Option<Binding>| b.is_some_and(|b| b.matches(key, mods));
    let (a, t, c) = (&keys.arrange, &keys.toolbar, &keys.canvas);
    let table = [
        (a.pack, KeyAction::Arrange(Arrange::Pack)),
        (a.group_by_type, KeyAction::Arrange(Arrange::GroupByType)),
        (a.keep_apart, KeyAction::KeepApart),
        (a.align_left, KeyAction::Align(Align::Left)),
        (a.align_center_x, KeyAction::Align(Align::CenterX)),
        (a.align_right, KeyAction::Align(Align::Right)),
        (a.align_top, KeyAction::Align(Align::Top)),
        (a.align_center_y, KeyAction::Align(Align::CenterY)),
        (a.align_bottom, KeyAction::Align(Align::Bottom)),
        (a.distribute_x, KeyAction::Arrange(Arrange::DistributeX)),
        (a.distribute_y, KeyAction::Arrange(Arrange::DistributeY)),
        (a.match_narrowest_width, KeyAction::Match(Match::NarrowestWidth)),
        (a.match_widest_width, KeyAction::Match(Match::WidestWidth)),
        (a.match_shortest_height, KeyAction::Match(Match::ShortestHeight)),
        (a.match_tallest_height, KeyAction::Match(Match::TallestHeight)),
        (a.move_to_slide, KeyAction::MoveToSlide),
        (a.flip_horizontal, KeyAction::Flip(false)),
        (a.flip_vertical, KeyAction::Flip(true)),
        (a.picture_colours, KeyAction::PictureColours),
    ];
    out.extend(table.into_iter().filter(|(b, _)| hit(*b)).map(|(_, act)| act));
    for (i, b) in t.colour.iter().enumerate() {
        if hit(*b) {
            out.push(KeyAction::Colour(i));
        }
    }
    for (b, ty) in t.highlight.iter().zip(MediaType::ALL) {
        if hit(*b) {
            out.push(KeyAction::Highlight(ty));
        }
    }
    let table = [
        (t.size_up, KeyAction::Size(1)),
        (t.size_down, KeyAction::Size(-1)),
        (t.insert_media, KeyAction::InsertMedia),
        (t.select_highlighted, KeyAction::SelectHighlighted),
        (t.pin_to_window, KeyAction::PinToWindow),
        (t.background_up, KeyAction::Background(1)),
        (t.background_down, KeyAction::Background(-1)),
        (t.reset_background, KeyAction::ResetBackground),
        (t.text_box, KeyAction::TextBox),
        (c.zoom_to_fit, KeyAction::ZoomToFit),
        (c.zoom_to_selection, KeyAction::ZoomToSelection),
        (c.new_slide, KeyAction::NewSlide),
        (c.toggle_slide_panel, KeyAction::ToggleSlidePanel),
        (c.toggle_slide_order, KeyAction::ToggleSlideOrder),
        (c.slide_window, KeyAction::SlideWindow),
        (c.new_layer, KeyAction::NewLayer),
        (c.delete_layer, KeyAction::DeleteLayer),
        (c.move_layer_up, KeyAction::MoveLayer(1)),
        (c.move_layer_down, KeyAction::MoveLayer(-1)),
        (c.pin_go_to_slide, KeyAction::PinGoToSlide),
        (c.pin_this_slide_only, KeyAction::PinThisSlideOnly),
    ];
    out.extend(table.into_iter().filter(|(b, _)| hit(*b)).map(|(_, act)| act));
    for (bindings, step) in [(&c.nudge_1px, 1.0), (&c.nudge_10px, 10.0)] {
        for (b, [x, y]) in bindings.iter().zip(NUDGE_DIRS) {
            if hit(*b) {
                out.push(KeyAction::Nudge([x * step, y * step]));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::hotkeys_config::HotkeysConfig;

    fn press(keys: &ParsedHotkeys, key: egui::Key, mods: egui::Modifiers) -> Vec<KeyAction> {
        let mut out = Vec::new();
        collect(key, mods, keys, &mut out);
        out
    }

    #[test]
    fn unbound_by_default() {
        let keys = HotkeysConfig::default().parsed();
        for key in [egui::Key::A, egui::Key::Num1, egui::Key::OpenBracket, egui::Key::F5] {
            for mods in [egui::Modifiers::NONE, egui::Modifiers::ALT, egui::Modifiers::SHIFT] {
                assert_eq!(press(&keys, key, mods), vec![], "{key:?} {mods:?}");
            }
        }
    }

    #[test]
    fn a_bound_key_presses_its_button() {
        let cfg: HotkeysConfig = toml::from_str(
            "[arrange]\nalign_left = \"alt+a\"\nmatch_tallest_height = \"alt+shift+h\"\n\
             [toolbar]\ncolour_3 = \"shift+3\"\nhighlight_gifs = \"alt+g\"\nsize_down = \"[\"\n\
             [canvas]\nmove_layer_down = \"alt+arrowdown\"\n",
        )
        .unwrap();
        let keys = cfg.parsed();
        let alt = egui::Modifiers::ALT;
        assert_eq!(press(&keys, egui::Key::A, alt), vec![KeyAction::Align(Align::Left)]);
        assert_eq!(press(&keys, egui::Key::A, egui::Modifiers::NONE), vec![], "modifiers must match");
        assert_eq!(press(&keys, egui::Key::H, alt | egui::Modifiers::SHIFT), vec![KeyAction::Match(Match::TallestHeight)]);
        assert_eq!(press(&keys, egui::Key::Num3, egui::Modifiers::SHIFT), vec![KeyAction::Colour(2)]);
        assert_eq!(press(&keys, egui::Key::G, alt), vec![KeyAction::Highlight(MediaType::Gif)]);
        assert_eq!(press(&keys, egui::Key::OpenBracket, egui::Modifiers::NONE), vec![KeyAction::Size(-1)]);
        assert_eq!(press(&keys, egui::Key::ArrowDown, alt), vec![KeyAction::MoveLayer(-1)]);
    }

    #[test]
    fn zoom_keys_are_bound_by_default() {
        let keys = HotkeysConfig::default().parsed();
        let ctrl = egui::Modifiers::CTRL;
        assert_eq!(press(&keys, egui::Key::Num1, ctrl), vec![KeyAction::ZoomToFit]);
        assert_eq!(press(&keys, egui::Key::Num2, ctrl), vec![KeyAction::ZoomToSelection]);
        assert_eq!(press(&keys, egui::Key::Num1, egui::Modifiers::NONE), vec![], "1 alone is the opacity key");
    }

    #[test]
    fn arrows_nudge_by_one_and_shift_arrows_by_ten() {
        let keys = HotkeysConfig::default().parsed();
        let shift = egui::Modifiers::SHIFT;
        assert_eq!(press(&keys, egui::Key::ArrowLeft, egui::Modifiers::NONE), vec![KeyAction::Nudge([-1.0, 0.0])]);
        assert_eq!(press(&keys, egui::Key::ArrowDown, egui::Modifiers::NONE), vec![KeyAction::Nudge([0.0, 1.0])]);
        assert_eq!(press(&keys, egui::Key::ArrowUp, shift), vec![KeyAction::Nudge([0.0, -10.0])]);
        assert_eq!(press(&keys, egui::Key::ArrowRight, shift), vec![KeyAction::Nudge([10.0, 0.0])]);
        assert_eq!(press(&keys, egui::Key::ArrowRight, egui::Modifiers::CTRL), vec![], "Ctrl+arrows are not nudges");
        let cfg: HotkeysConfig = toml::from_str("[canvas]\nnudge_left_1px = \"j\"\nnudge_left_10px = \"\"\n").unwrap();
        let keys = cfg.parsed();
        assert_eq!(press(&keys, egui::Key::J, egui::Modifiers::NONE), vec![KeyAction::Nudge([-1.0, 0.0])]);
        assert_eq!(press(&keys, egui::Key::ArrowLeft, egui::Modifiers::NONE), vec![], "rebound");
        assert_eq!(press(&keys, egui::Key::ArrowLeft, shift), vec![], "unbound");
    }
}
