//! Command palette: Ctrl+K opens a search box over the canvas that finds
//! any command by a few letters of its name — the tools, the slide and
//! layer commands, every toolbar, bar and panel button, and "go to" each
//! slide — shows the key it is bound to, and runs it. Recently run
//! commands come first.
//!
//! The list is rebuilt from the app's state each frame it is open
//! (`Context`), so what needs a selection (or a pin, or one picture) is
//! only offered when there is one. Running a command is the app's job:
//! the palette hands back the `Command`.

use crate::canvas::stroke::StrokeStyle;
use crate::input::hotkeys_config::{Binding, ParsedHotkeys};
use crate::interaction::action_keys::KeyAction;
use crate::tools::arrange::{Align, Arrange, Match};
use crate::tools::ToolKind;
use crate::ui::theme;
use egui::{Color32, Vec2};

/// What running an entry does.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Command {
    /// A button's action, as its key would press it.
    Key(KeyAction),
    /// Switch to a tool (and, for the pen, a brush style).
    Tool(ToolKind, Option<StrokeStyle>),
    /// The next tap picks a colour from the screen.
    Eyedropper,
    Undo,
    Redo,
    ClearLayer,
    DeleteSlide,
    LockSlide,
    RenameSlide,
    ResetView,
    FreezeFrame,
    ToggleUi,
    TogglePins,
    Duplicate,
    CopyToNewSlide,
    TogglePin,
    ToFront,
    ToBack,
    DeleteSelection,
    ShapeSnap,
    GoTo(usize),
}

/// One row: what it says, where it belongs, its keys, what it does.
#[derive(Debug, Clone)]
pub struct Entry {
    pub label: String,
    pub group: &'static str,
    pub keys: Option<String>,
    pub cmd: Command,
}

/// What the list is built from.
pub struct Context<'a> {
    pub keys: &'a ParsedHotkeys,
    /// Objects selected with the Select tool.
    pub selected: usize,
    /// The selection is one picture, video or GIF.
    pub picture: bool,
    /// Some of the selection is grouped.
    pub grouped: bool,
    /// A pin is selected.
    pub pin: bool,
    /// The palette's colours.
    pub palette: &'a [[u8; 4]],
    /// Every slide's name, in order.
    pub slides: Vec<String>,
    pub active_slide: usize,
}

/// The entries `cx` allows, in the order they show with no query.
pub fn entries(cx: &Context) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    let mut add = |label: &str, group: &'static str, key: Option<Binding>, cmd: Command| {
        out.push(Entry { label: label.to_string(), group, keys: key.map(|b| b.label()), cmd });
    };
    let (t, tb, a, c) = (&cx.keys.tool, &cx.keys.toolbar, &cx.keys.arrange, &cx.keys.canvas);

    use Command as C;
    use KeyAction as K;
    let tools: [(&str, Option<Binding>, ToolKind, Option<StrokeStyle>); 16] = [
        ("Pen", t.pen, ToolKind::Pen, Some(StrokeStyle::Default)),
        ("Pencil", t.pencil, ToolKind::Pen, Some(StrokeStyle::Pencil)),
        ("Marker (highlighter)", t.marker, ToolKind::Pen, Some(StrokeStyle::Marker)),
        ("Airbrush", t.airbrush, ToolKind::Pen, Some(StrokeStyle::Airbrush)),
        ("Fill", t.fill, ToolKind::Fill, None),
        ("Laser pointer", t.laser, ToolKind::Laser, None),
        ("Text", t.text, ToolKind::Text, None),
        ("Spotlight", t.spotlight, ToolKind::Spotlight, None),
        ("Lasso erase", t.lasso, ToolKind::LassoErase, None),
        ("Lasso capture (screen to picture)", t.lasso_select, ToolKind::LassoSelect, None),
        ("Select", t.select, ToolKind::Select, None),
        ("Rectangle", t.rect, ToolKind::Rect, None),
        ("Ellipse", t.ellipse, ToolKind::Ellipse, None),
        ("Line", t.line, ToolKind::Line, None),
        ("Arrow", t.arrow, ToolKind::Arrow, None),
        ("Freehand arrow", t.freehand_arrow, ToolKind::FreehandArrow, None),
    ];
    for (label, key, kind, style) in tools {
        add(label, "Tool", key, C::Tool(kind, style));
    }
    add("Eyedropper (pick a colour from the screen)", "Tool", t.eyedropper, C::Eyedropper);

    if cx.selected > 0 {
        let s = "Selection";
        add("Duplicate", s, c.duplicate, C::Duplicate);
        add("Delete the selection", s, c.delete_selection, C::DeleteSelection);
        add("Copy to a new slide", s, c.copy_to_new_slide, C::CopyToNewSlide);
        add("Move to slide…", s, a.move_to_slide, C::Key(K::MoveToSlide));
        add("Pin to every slide / unpin", s, c.pin_selection, C::TogglePin);
        add("Bring to front", s, c.bring_front, C::ToFront);
        add("Send to back", s, c.send_back, C::ToBack);
        add("Flip left to right", s, a.flip_horizontal, C::Key(K::Flip(false)));
        add("Flip top to bottom", s, a.flip_vertical, C::Key(K::Flip(true)));
        add("Turn left 90°", s, a.rotate_left, C::Key(K::Rotate(false)));
        add("Turn right 90°", s, a.rotate_right, C::Key(K::Rotate(true)));
        add("Zoom to the selection", s, c.zoom_to_selection, C::Key(K::ZoomToSelection));
        add("Tag the selection (find knows it by its tags)", s, a.tag, C::Key(K::Tag));
        if cx.selected >= 2 {
            add("Group", s, c.group, C::Key(K::Group(true)));
        }
        if cx.grouped {
            add("Ungroup", s, c.ungroup, C::Key(K::Group(false)));
        }
        if cx.picture {
            add("A picture's colours", s, a.picture_colours, C::Key(K::PictureColours));
        }
        if cx.selected >= 2 {
            let r = "Arrange";
            add("Pack", r, a.pack, C::Key(K::Arrange(Arrange::Pack)));
            add("Group by type", r, a.group_by_type, C::Key(K::Arrange(Arrange::GroupByType)));
            add("Align left", r, a.align_left, C::Key(K::Align(Align::Left)));
            add("Align centres across", r, a.align_center_x, C::Key(K::Align(Align::CenterX)));
            add("Align right", r, a.align_right, C::Key(K::Align(Align::Right)));
            add("Align top", r, a.align_top, C::Key(K::Align(Align::Top)));
            add("Align centres down", r, a.align_center_y, C::Key(K::Align(Align::CenterY)));
            add("Align bottom", r, a.align_bottom, C::Key(K::Align(Align::Bottom)));
            add("Same width as the narrowest", r, a.match_narrowest_width, C::Key(K::Match(Match::NarrowestWidth)));
            add("Same width as the widest", r, a.match_widest_width, C::Key(K::Match(Match::WidestWidth)));
            add("Same height as the shortest", r, a.match_shortest_height, C::Key(K::Match(Match::ShortestHeight)));
            add("Same height as the tallest", r, a.match_tallest_height, C::Key(K::Match(Match::TallestHeight)));
            if cx.selected >= 3 {
                add("Distribute across", r, a.distribute_x, C::Key(K::Arrange(Arrange::DistributeX)));
                add("Distribute down", r, a.distribute_y, C::Key(K::Arrange(Arrange::DistributeY)));
            }
            add("Keep apart on / off (how aligning moves things)", r, a.keep_apart, C::Key(K::KeepApart));
        }
    }
    if cx.pin {
        add("Pin: go to its slide", "Pin", c.pin_go_to_slide, C::Key(K::PinGoToSlide));
        add("Pin: this slide only / every slide", "Pin", c.pin_this_slide_only, C::Key(K::PinThisSlideOnly));
    }

    let e = "Edit";
    add("Undo", e, c.undo, C::Undo);
    add("Redo", e, c.redo, C::Redo);
    add("Clear the active layer", e, c.clear_layer, C::ClearLayer);
    add("Text in a box on / off", e, tb.text_box, C::Key(K::TextBox));
    add("Shape snap on / off", e, None, C::ShapeSnap);
    add("Insert a picture, video, GIF or PureRef board", e, tb.insert_media, C::Key(K::InsertMedia));
    add("Pen size up", e, tb.size_up, C::Key(K::Size(1)));
    add("Pen size down", e, tb.size_down, C::Key(K::Size(-1)));
    for (i, colour) in cx.palette.iter().enumerate().take(8) {
        let label = format!("Colour {} (#{:02X}{:02X}{:02X})", i + 1, colour[0], colour[1], colour[2]);
        add(&label, e, tb.colour[i], C::Key(K::Colour(i)));
    }

    let v = "View";
    add("Find text on the slides", v, c.find, C::Key(K::Find));
    add("Zoom to fit the slide", v, c.zoom_to_fit, C::Key(K::ZoomToFit));
    add("Reset zoom and pan", v, c.reset_view, C::ResetView);
    add("Hide / show the toolbar and panels", v, c.toggle_ui, C::ToggleUi);
    add("Hide / show every pin", v, c.toggle_pins, C::TogglePins);
    add("Onion skin on / off (GIFs and videos)", v, c.media_onion, C::Key(K::Onion));
    add("Highlight pictures", v, tb.highlight[0], C::Key(K::Highlight(crate::canvas::shape::MediaType::Picture)));
    add("Highlight GIFs", v, tb.highlight[1], C::Key(K::Highlight(crate::canvas::shape::MediaType::Gif)));
    add("Highlight videos", v, tb.highlight[2], C::Key(K::Highlight(crate::canvas::shape::MediaType::Video)));
    add("Highlight groups", v, tb.highlight_groups, C::Key(K::HighlightGroups));
    add("Highlight everything tagged", v, tb.highlight_tagged, C::Key(K::HighlightTagged));
    add("Select what is highlighted", v, tb.select_highlighted, C::Key(K::SelectHighlighted));
    add("Background opacity up", v, tb.background_up, C::Key(K::Background(1)));
    add("Background opacity down", v, tb.background_down, C::Key(K::Background(-1)));
    add("Black background", v, tb.reset_background, C::Key(K::ResetBackground));
    add("Pin the overlay to a window / unpin", v, tb.pin_to_window, C::Key(K::PinToWindow));

    let sl = "Slide";
    add("New slide", sl, c.new_slide, C::Key(K::NewSlide));
    add("Name the slide", sl, c.rename_slide, C::RenameSlide);
    add("Lock / unlock the slide", sl, c.lock_slide, C::LockSlide);
    add("Delete the slide", sl, c.delete_canvas, C::DeleteSlide);
    add("Move the slide up", sl, c.move_slide_up, C::Key(K::MoveSlide(-1)));
    add("Move the slide down", sl, c.move_slide_down, C::Key(K::MoveSlide(1)));
    add("Freeze the screen into a new layer", sl, c.freeze_frame, C::FreezeFrame);
    add("Open the slide window / close it", sl, c.slide_window, C::Key(K::SlideWindow));
    add("Fold / unfold the slide panel", sl, c.toggle_slide_panel, C::Key(K::ToggleSlidePanel));
    add("Every tag (filter slides, rename, remove)", sl, c.tag_list, C::Key(K::TagList));
    add("Ctrl+Tab: slides in order / recently used", sl, c.toggle_slide_order, C::Key(K::ToggleSlideOrder));

    let l = "Layer";
    add("New layer", l, c.new_layer, C::Key(K::NewLayer));
    add("Delete the active layer", l, c.delete_layer, C::Key(K::DeleteLayer));
    add("Move the layer up", l, c.move_layer_up, C::Key(K::MoveLayer(1)));
    add("Move the layer down", l, c.move_layer_down, C::Key(K::MoveLayer(-1)));

    for (i, name) in cx.slides.iter().enumerate() {
        if i == cx.active_slide {
            continue;
        }
        let label = if name.is_empty() { format!("Go to slide {}", i + 1) } else { format!("Go to slide {} — {name}", i + 1) };
        add(&label, "Go to", None, C::GoTo(i));
    }
    out
}

/// How well `query` matches `text`: every word of the query must be
/// found in it, its letters in order (gaps allowed), case aside. Higher
/// is better: letters in a row, letters at the start of a word, and
/// shorter texts score more. `None` when it does not match.
pub fn score(query: &str, text: &str) -> Option<i32> {
    let hay: Vec<char> = text.to_lowercase().chars().collect();
    let mut total = 0i32;
    for word in query.split_whitespace() {
        let needle: Vec<char> = word.to_lowercase().chars().collect();
        total += word_score(&needle, &hay)?;
    }
    Some(total - hay.len() as i32 / 8)
}

fn word_score(needle: &[char], hay: &[char]) -> Option<i32> {
    // Each start position tried, the best kept: "sl" should find "slide"
    // in "Name the slide", not the s of "the" then the l of "slide".
    let mut best: Option<i32> = None;
    for start in 0..hay.len() {
        if hay[start] != needle.first().copied()? {
            continue;
        }
        let (mut score, mut at, mut prev) = (0i32, start, None::<usize>);
        let mut ok = true;
        for &ch in needle {
            let Some(found) = (at..hay.len()).find(|&i| hay[i] == ch) else {
                ok = false;
                break;
            };
            let word_start = found == 0 || !hay[found - 1].is_alphanumeric();
            score += 1 + if word_start { 6 } else { 0 } + if prev == Some(found.wrapping_sub(1)) { 4 } else { 0 };
            score -= (found - at).min(6) as i32;
            prev = Some(found);
            at = found + 1;
        }
        if ok {
            best = Some(best.map_or(score, |b| b.max(score)));
        }
    }
    best
}

/// The palette's state between frames.
#[derive(Default)]
pub struct Palette {
    open: bool,
    query: String,
    /// The highlighted row in the filtered list.
    cursor: usize,
    /// Labels of commands run, most recent first.
    recent: Vec<String>,
    /// The text field has yet to take the keyboard.
    focus_pending: bool,
}

/// Most rows shown at once.
const ROWS: usize = 12;
const WIDTH: f32 = 520.0;

impl Palette {
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn open(&mut self) {
        self.open = true;
        self.query.clear();
        self.cursor = 0;
        self.focus_pending = true;
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    /// `all` filtered by the query and ordered: by score while typing,
    /// with recently run commands lifted a little; recent first, then
    /// the rest as given, while the query is empty.
    fn filtered<'e>(&self, all: &'e [Entry]) -> Vec<&'e Entry> {
        let recency = |e: &Entry| self.recent.iter().position(|r| *r == e.label);
        if self.query.trim().is_empty() {
            let mut recent: Vec<&Entry> = all.iter().filter(|e| recency(e).is_some()).collect();
            recent.sort_by_key(|e| recency(e));
            let rest = all.iter().filter(|e| recency(e).is_none());
            return recent.into_iter().chain(rest).collect();
        }
        let mut scored: Vec<(i32, usize, &Entry)> = all
            .iter()
            .enumerate()
            .filter_map(|(i, e)| {
                let s = score(&self.query, &e.label).or_else(|| score(&self.query, &format!("{} {}", e.group, e.label)).map(|s| s - 4))?;
                let lift = recency(e).map_or(0, |r| 6 - r.min(6) as i32);
                Some((s + lift, i, e))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.into_iter().map(|(_, _, e)| e).collect()
    }

    fn ran(&mut self, e: &Entry) {
        self.recent.retain(|r| *r != e.label);
        self.recent.insert(0, e.label.clone());
        self.recent.truncate(12);
    }

    /// Draw the palette while open; the command picked, if one was.
    /// ↑ / ↓ move, Enter runs, Esc or a click elsewhere closes.
    pub fn show(&mut self, ctx: &egui::Context, all: &[Entry]) -> Option<Command> {
        if !self.open {
            return None;
        }
        let list = self.filtered(all);
        let (up, down, enter, esc) = ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
            )
        });
        if esc {
            self.close();
            return None;
        }
        if down && !list.is_empty() {
            self.cursor = (self.cursor + 1).min(list.len() - 1);
        }
        if up {
            self.cursor = self.cursor.saturating_sub(1);
        }
        self.cursor = self.cursor.min(list.len().saturating_sub(1));
        let mut picked: Option<usize> = enter.then_some(self.cursor).filter(|_| !list.is_empty());

        let top = ctx.memory(|m| m.area_rect(egui::Id::new("osn_toolbar"))).map_or(70.0, |r| r.bottom() + 10.0);
        let x = ctx.screen_rect().center().x - WIDTH * 0.5;
        let area = egui::Area::new(egui::Id::new("osn_command_palette"))
            .fixed_pos(egui::pos2(x, top))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                theme::card_frame().inner_margin(egui::Margin::same(8.0)).show(ui, |ui| {
                    ui.set_width(WIDTH - 16.0);
                    let edit = egui::TextEdit::singleline(&mut self.query)
                        .hint_text("Type a command — a few letters will do")
                        .desired_width(f32::INFINITY);
                    let r = ui.add(edit);
                    if std::mem::take(&mut self.focus_pending) || !r.has_focus() {
                        r.request_focus();
                    }
                    if r.changed() {
                        self.cursor = 0;
                    }
                    ui.add_space(4.0);
                    if list.is_empty() {
                        ui.label(egui::RichText::new("No command matches").color(theme::TEXT_MUTED));
                        return;
                    }
                    // A window of rows around the cursor.
                    let first = self.cursor.saturating_sub(ROWS - 1);
                    for (k, e) in list.iter().enumerate().skip(first).take(ROWS) {
                        let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 24.0), egui::Sense::click());
                        let lit = k == self.cursor || resp.hovered();
                        if lit {
                            ui.painter().rect_filled(rect, egui::Rounding::same(5.0), if k == self.cursor { theme::ACCENT } else { Color32::from_white_alpha(16) });
                        }
                        let y = rect.center().y;
                        ui.painter().text(egui::pos2(rect.left() + 8.0, y), egui::Align2::LEFT_CENTER, &e.label, egui::FontId::proportional(13.5), theme::TEXT_PRIMARY);
                        let right = match &e.keys {
                            Some(k) => {
                                let g = ui.painter().text(egui::pos2(rect.right() - 8.0, y), egui::Align2::RIGHT_CENTER, k, egui::FontId::monospace(11.5), Color32::from_rgb(232, 200, 200));
                                g.left() - 10.0
                            }
                            None => rect.right() - 8.0,
                        };
                        ui.painter().text(egui::pos2(right, y), egui::Align2::RIGHT_CENTER, e.group, egui::FontId::proportional(11.0), theme::TEXT_MUTED);
                        if resp.clicked() {
                            picked = Some(k);
                        }
                    }
                    if list.len() > ROWS {
                        ui.label(egui::RichText::new(format!("{} more — keep typing", list.len() - ROWS.min(list.len()))).small().color(theme::TEXT_MUTED));
                    }
                });
            });
        // A click outside closes it.
        if ctx.input(|i| i.pointer.any_pressed()) && !ctx.input(|i| i.pointer.interact_pos()).is_some_and(|p| area.response.rect.contains(p)) {
            self.close();
            return None;
        }
        let e = picked.and_then(|k| list.get(k).copied())?.clone();
        self.ran(&e);
        self.close();
        Some(e.cmd)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::hotkeys_config::HotkeysConfig;

    #[test]
    fn letters_in_order_match_and_word_starts_win() {
        assert!(score("ztf", "Zoom to fit the slide").is_some());
        assert!(score("fit", "Zoom to fit the slide").is_some());
        assert_eq!(score("xyz", "Zoom to fit the slide"), None);
        // Every word of the query has to be found.
        assert!(score("zoom sel", "Zoom to the selection").is_some());
        assert_eq!(score("zoom paste", "Zoom to the selection"), None);
        // A word start beats letters scattered through words.
        assert!(score("pen", "Pen").unwrap() > score("pen", "Open the slide window").unwrap());
        assert!(score("dup", "Duplicate").unwrap() > score("dup", "Distribute down up").unwrap_or(i32::MIN));
    }

    fn context<'a>(keys: &'a ParsedHotkeys, selected: usize, palette: &'a [[u8; 4]]) -> Context<'a> {
        Context { keys, selected, picture: false, grouped: false, pin: false, palette, slides: vec!["".into(), "Intro".into()], active_slide: 0 }
    }

    #[test]
    fn selection_commands_only_show_with_a_selection_and_keys_are_labelled() {
        let keys = HotkeysConfig::default().parsed();
        let colours = [[255, 0, 0, 255]];
        let none = entries(&context(&keys, 0, &colours));
        assert!(!none.iter().any(|e| e.label == "Duplicate"));
        assert!(none.iter().any(|e| e.label == "Go to slide 2 — Intro"));
        assert!(!none.iter().any(|e| e.label.starts_with("Go to slide 1")), "not the slide shown");
        let two = entries(&context(&keys, 2, &colours));
        let group = two.iter().find(|e| e.label == "Group").expect("two selected can group");
        assert_eq!(group.keys.as_deref(), Some("Ctrl+G"));
        assert!(!two.iter().any(|e| e.label == "Distribute across"), "needs three");
        assert!(two.iter().any(|e| e.label.starts_with("Colour 1 (#FF0000)")));
    }

    #[test]
    fn recent_commands_come_first_and_typing_filters() {
        let keys = HotkeysConfig::default().parsed();
        let all = entries(&context(&keys, 0, &[]));
        let mut p = Palette::default();
        let redo = all.iter().find(|e| e.label == "Redo").unwrap().clone();
        p.ran(&redo);
        assert_eq!(p.filtered(&all)[0].label, "Redo");
        p.query = "zoom fit".into();
        assert_eq!(p.filtered(&all)[0].label, "Zoom to fit the slide");
        p.query = "pencil".into();
        assert_eq!(p.filtered(&all)[0].cmd, Command::Tool(ToolKind::Pen, Some(StrokeStyle::Pencil)));
    }
}
