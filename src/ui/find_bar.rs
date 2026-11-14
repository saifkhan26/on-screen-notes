//! Find (Ctrl+F): a search box that finds things on every slide by what
//! they say or how they are tagged: text labels by their text, and any
//! object by its tags (`Canvas::set_tags`) — a group as one when every
//! member matches. Matches are listed with their slide, in slide
//! order; `Enter` goes to the next one (`Shift+Enter` the one before),
//! `↑` / `↓` step through the list and go there, a click goes to that
//! one, `Esc` closes. Going there is the app's job
//! (`App::go_to_find_hit`): the slide, the view gliding to the match,
//! and a flash on it.
//!
//! Only slides already read are searched: the bar says how many are
//! still opening. The query is kept between opens, selected so that
//! typing replaces it, as in a browser.

use crate::canvas::canvas::{Canvas, Layer};
use crate::canvas::units::{bounds_of, tagged_by};
use crate::canvas::shape::{MediaType, Shape};
use crate::ui::theme;
use egui::text::{CCursor, CCursorRange, LayoutJob, TextFormat};
use egui::{Color32, FontId, Vec2};

/// The colour a match is marked in, here and on the canvas.
pub const FOUND: Color32 = Color32::from_rgb(255, 196, 64);

/// What matched.
#[derive(Debug, Clone, PartialEq)]
pub enum Found {
    /// A text label, by its text: its index in the layer's shapes.
    Label(usize),
    /// Objects, by their tags: one object, or every member of a group.
    /// `what` says which ("Picture", "Group of 4").
    Tagged { strokes: Vec<usize>, shapes: Vec<usize>, what: String },
}

/// One match.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub slide: usize,
    pub layer: usize,
    pub found: Found,
    /// A label's text, or the tags as `#one  #two`, on one line, cut
    /// down around the match.
    pub snippet: String,
    /// Where the match is in `snippet`, in bytes.
    pub mark: std::ops::Range<usize>,
    /// It sits on a hidden layer, so it is not on screen.
    pub hidden: bool,
}

/// Longest snippet, in characters.
const SNIPPET: usize = 56;

/// Everything on `slides` that holds `query`, case aside, with line
/// breaks and runs of spaces read as one space: text labels by their
/// text, objects and groups by their tags. In the order given, then top
/// to bottom and left to right on each slide. A lone label found by its
/// text is not listed again for its tags.
pub fn search<'a>(query: &str, slides: impl IntoIterator<Item = (usize, &'a Canvas)>) -> Vec<Hit> {
    let needle: Vec<char> = fold(&flat(query));
    if needle.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (slide, c) in slides {
        let mut here: Vec<([f32; 2], Hit)> = Vec::new();
        for (layer, l) in c.layers.iter().enumerate() {
            let mut labels = Vec::new();
            for (shape, s) in l.shapes.iter().enumerate() {
                let Shape::Text { content, .. } = s else { continue };
                let text: Vec<char> = flat(content).chars().collect();
                let Some(at) = find(&fold_chars(&text), &needle) else { continue };
                let (snippet, mark) = snippet(&text, at, needle.len());
                let b = s.obb().aabb();
                labels.push(shape);
                here.push(([b[1], b[0]], Hit { slide, layer, found: Found::Label(shape), snippet, mark, hidden: !l.visible }));
            }
            // An object by its own tags; a group whole only when every
            // member matches (see `units::tagged_by`).
            let says = |tags: &[String]| {
                let text: Vec<char> = tags.iter().map(|t| format!("#{t}")).collect::<Vec<_>>().join("  ").chars().collect();
                find(&fold_chars(&text), &needle).is_some()
            };
            for unit in tagged_by(l, says) {
                if unit.strokes.is_empty() && unit.shapes.len() == 1 && labels.contains(&unit.shapes[0]) {
                    continue;
                }
                let text: Vec<char> = unit.tags.iter().map(|t| format!("#{t}")).collect::<Vec<_>>().join("  ").chars().collect();
                let Some(at) = find(&fold_chars(&text), &needle) else { continue };
                let Some(b) = bounds_of(l, &unit.strokes, &unit.shapes) else { continue };
                let (snippet, mark) = snippet(&text, at, needle.len());
                let what = describe(l, &unit.strokes, &unit.shapes);
                let found = Found::Tagged { strokes: unit.strokes, shapes: unit.shapes, what };
                here.push(([b[1], b[0]], Hit { slide, layer, found, snippet, mark, hidden: !l.visible }));
            }
        }
        here.sort_by(|a, b| a.0[0].total_cmp(&b.0[0]).then(a.0[1].total_cmp(&b.0[1])));
        out.extend(here.into_iter().map(|(_, h)| h));
    }
    out
}

/// What these objects are, in a word or two.
fn describe(l: &Layer, strokes: &[usize], shapes: &[usize]) -> String {
    match (strokes, shapes) {
        ([_], []) => "Stroke".into(),
        ([], [i]) => match &l.shapes[*i] {
            s @ Shape::Raster { .. } => match s.media_type() {
                Some(MediaType::Picture) => "Picture",
                Some(MediaType::Gif) => "GIF",
                Some(MediaType::Video) => "Video",
                None => "Fill",
            }
            .into(),
            Shape::Rect { .. } => "Rectangle".into(),
            Shape::Ellipse { .. } => "Ellipse".into(),
            Shape::Line { .. } => "Line".into(),
            Shape::Arrow { .. } => "Arrow".into(),
            Shape::Text { .. } => "Label".into(),
        },
        _ => format!("Group of {}", strokes.len() + shapes.len()),
    }
}

/// `s` on one line: any run of white space becomes one space.
fn flat(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Lower case, one char for one char, so a match's place carries over.
fn fold(s: &str) -> Vec<char> {
    fold_chars(&s.chars().collect::<Vec<_>>())
}

fn fold_chars(s: &[char]) -> Vec<char> {
    s.iter().map(|c| c.to_lowercase().next().unwrap_or(*c)).collect()
}

/// Where `needle` first starts in `hay`, in chars.
fn find(hay: &[char], needle: &[char]) -> Option<usize> {
    (0..=hay.len().checked_sub(needle.len())?).find(|&i| hay[i..i + needle.len()] == *needle)
}

/// At most `SNIPPET` chars of `text` around the match at `at` (`len`
/// chars), with an ellipsis where it was cut, and the match's byte range.
fn snippet(text: &[char], at: usize, len: usize) -> (String, std::ops::Range<usize>) {
    let n = text.len();
    let start = if n <= SNIPPET { 0 } else { at.saturating_sub(16).min(n - SNIPPET) };
    let end = (start + SNIPPET).min(n);
    let mut s = String::new();
    if start > 0 {
        s.push('…');
    }
    let mut mark = 0..0;
    for (i, c) in text.iter().enumerate().take(end).skip(start) {
        if i == at {
            mark.start = s.len();
        }
        s.push(*c);
        if i + 1 == at + len {
            mark.end = s.len();
        }
    }
    if mark.end < mark.start {
        // The match ran past the cut.
        mark.end = s.len();
    }
    if end < n {
        s.push('…');
    }
    (s, mark)
}

/// The find bar's state between frames.
#[derive(Default)]
pub struct FindBar {
    open: bool,
    query: String,
    /// The highlighted row.
    cursor: usize,
    /// The highlighted row is the one last gone to, so the next `Enter`
    /// moves on.
    shown: bool,
    /// Put the cursor at the first match from the shown slide on, once
    /// there is a list to look in.
    restart: bool,
    /// The text field has yet to take the keyboard; with its text selected.
    focus_pending: bool,
}

/// Most rows shown at once.
const ROWS: usize = 8;
const WIDTH: f32 = 520.0;

impl FindBar {
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Open it, with the last query selected.
    pub fn open(&mut self) {
        self.open = true;
        self.shown = false;
        self.restart = true;
        self.focus_pending = true;
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    /// The row a key moves to and goes to, if a key did: `Enter` the next
    /// (or the highlighted one, not gone to yet), `Shift+Enter` the one
    /// before, `↑` / `↓` the row above or below. Wraps around at the ends.
    fn step(&mut self, n: usize, next: bool, prev: bool, up: bool, down: bool) -> Option<usize> {
        if n == 0 {
            return None;
        }
        self.cursor = self.cursor.min(n - 1);
        let to = if next {
            if self.shown { (self.cursor + 1) % n } else { self.cursor }
        } else if prev {
            if self.shown { (self.cursor + n - 1) % n } else { self.cursor }
        } else if down {
            (self.cursor + 1) % n
        } else if up {
            (self.cursor + n - 1) % n
        } else {
            return None;
        };
        self.cursor = to;
        self.shown = true;
        Some(to)
    }

    /// Draw the bar while open; the match to go to, if one was picked.
    /// `search` finds the matches for a query; `unread` slides are not
    /// searched yet; `active` is the slide shown; `name` gives a slide's
    /// name ("" for none).
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        search: impl Fn(&str) -> Vec<Hit>,
        unread: usize,
        active: usize,
        name: impl Fn(usize) -> String,
    ) -> Option<Hit> {
        if !self.open {
            return None;
        }
        let mut hits = search(&self.query);
        let (prev, next, up, down, esc) = ctx.input_mut(|i| {
            // Shift+Enter first: a plain Enter pattern would take it too.
            let prev = i.consume_key(egui::Modifiers::SHIFT, egui::Key::Enter);
            (
                prev,
                i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
            )
        });
        if esc {
            self.close();
            return None;
        }
        let restart = |bar: &mut Self, hits: &[Hit]| {
            bar.cursor = hits.iter().position(|h| h.slide >= active).unwrap_or(0);
            bar.shown = false;
            bar.restart = false;
        };
        if self.restart {
            restart(self, &hits);
        }
        let mut picked = self.step(hits.len(), next, prev, up, down);

        let top = ctx.memory(|m| m.area_rect(egui::Id::new("osn_toolbar"))).map_or(70.0, |r| r.bottom() + 10.0);
        let x = ctx.screen_rect().center().x - WIDTH * 0.5;
        let area = egui::Area::new(egui::Id::new("osn_find_bar"))
            .fixed_pos(egui::pos2(x, top))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                theme::card_frame().inner_margin(egui::Margin::same(8.0)).show(ui, |ui| {
                    ui.set_width(WIDTH - 16.0);
                    let out = egui::TextEdit::singleline(&mut self.query)
                        .hint_text("Find labels and tags on the slides")
                        .desired_width(f32::INFINITY)
                        .show(ui);
                    if std::mem::take(&mut self.focus_pending) {
                        out.response.request_focus();
                        let mut state = out.state.clone();
                        let all = CCursorRange::two(CCursor::new(0), CCursor::new(self.query.chars().count()));
                        state.cursor.set_char_range(Some(all));
                        state.store(ui.ctx(), out.response.id);
                    } else if !out.response.has_focus() {
                        out.response.request_focus();
                    }
                    if out.response.changed() {
                        hits = search(&self.query);
                        restart(self, &hits);
                        picked = None;
                    }
                    ui.add_space(4.0);
                    let status = if self.query.trim().is_empty() {
                        "Type a label's words or a tag".to_string()
                    } else if hits.is_empty() {
                        "No label says that, and nothing is tagged so".to_string()
                    } else if self.shown {
                        format!("{} of {}", self.cursor + 1, hits.len())
                    } else if hits.len() == 1 {
                        "1 found — Enter goes there".to_string()
                    } else {
                        format!("{} found — Enter goes to each in turn", hits.len())
                    };
                    let status = match unread {
                        0 => status,
                        1 => format!("{status} · 1 slide still opening"),
                        n => format!("{status} · {n} slides still opening"),
                    };
                    ui.label(egui::RichText::new(status).small().color(theme::TEXT_MUTED));
                    let first = self.cursor.saturating_sub(ROWS - 1);
                    for (k, h) in hits.iter().enumerate().skip(first).take(ROWS) {
                        let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 24.0), egui::Sense::click());
                        let lit = k == self.cursor;
                        if lit || resp.hovered() {
                            let fill = if lit { theme::ACCENT } else { Color32::from_white_alpha(16) };
                            ui.painter().rect_filled(rect, egui::Rounding::same(5.0), fill);
                        }
                        let y = rect.center().y;
                        let slide = match name(h.slide) {
                            n if n.is_empty() => format!("Slide {}", h.slide + 1),
                            n => format!("{} · {n}", h.slide + 1),
                        };
                        let slide = match &h.found {
                            Found::Tagged { what, .. } => format!("{what} · {slide}"),
                            Found::Label(_) => slide,
                        };
                        let slide = if h.hidden { format!("{slide} · hidden layer") } else { slide };
                        let g = ui.painter().text(egui::pos2(rect.right() - 8.0, y), egui::Align2::RIGHT_CENTER, slide, FontId::proportional(11.0), theme::TEXT_MUTED);
                        let room = (g.left() - rect.left() - 24.0).max(40.0);
                        let galley = ui.painter().layout_job(row_text(h, room));
                        let at = egui::pos2(rect.left() + 8.0, y - galley.size().y * 0.5);
                        ui.painter().galley(at, galley, theme::TEXT_PRIMARY);
                        if resp.clicked() {
                            self.cursor = k;
                            self.shown = true;
                            picked = Some(k);
                        }
                    }
                    if hits.len() > first + ROWS {
                        let more = hits.len() - first - ROWS;
                        ui.label(egui::RichText::new(format!("{more} more below")).small().color(theme::TEXT_MUTED));
                    }
                });
            });
        // A click outside closes it.
        if ctx.input(|i| i.pointer.any_pressed()) && !ctx.input(|i| i.pointer.interact_pos()).is_some_and(|p| area.response.rect.contains(p)) {
            self.close();
        }
        if picked.is_some() {
            ctx.request_repaint();
        }
        picked.and_then(|k| hits.get(k).cloned())
    }
}

/// A row's text: the snippet, its match in the found colour, cut to `width`.
fn row_text(h: &Hit, width: f32) -> LayoutJob {
    let plain = TextFormat { font_id: FontId::proportional(13.5), color: theme::TEXT_PRIMARY, ..Default::default() };
    let found = TextFormat { color: FOUND, ..plain.clone() };
    let mut job = LayoutJob::default();
    let s = &h.snippet;
    job.append(&s[..h.mark.start], 0.0, plain.clone());
    job.append(&s[h.mark.clone()], 0.0, found);
    job.append(&s[h.mark.end..], 0.0, plain);
    job.wrap.max_width = width;
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(at: [f32; 2], text: &str) -> Shape {
        Shape::Text {
            pos: at,
            content: text.into(),
            font_size: 20.0,
            color: [255, 255, 255, 255],
            rotation: 0.0,
            pin: 0,
            background: None,
            group: 0,
            tags: Vec::new(),
            turn: 0.0,
        }
    }

    fn slide(labels: &[([f32; 2], &str)]) -> Canvas {
        let mut c = Canvas::default();
        c.layers[0].shapes.extend(labels.iter().map(|(at, t)| label(*at, t)));
        c
    }

    #[test]
    fn finds_labels_case_aside_across_lines_in_reading_order() {
        let a = slide(&[([0.0, 200.0], "second Apple"), ([300.0, 0.0], "an APPLE\npie"), ([0.0, 0.0], "pear")]);
        let b = slide(&[([0.0, 0.0], "Apple   pie")]);
        let hits = search("apple pie", [(0, &a), (1, &b)]);
        // "an APPLE\npie" reads as "an APPLE pie"; runs of spaces count as one.
        assert_eq!(hits.iter().map(|h| (h.slide, h.found.clone())).collect::<Vec<_>>(), vec![(0, Found::Label(1)), (1, Found::Label(0))]);
        assert_eq!(hits[0].snippet, "an APPLE pie");
        assert_eq!(&hits[0].snippet[hits[0].mark.clone()], "APPLE pie");
        // Top to bottom on a slide.
        let apples = search("apple", [(0, &a)]);
        assert_eq!(apples.iter().map(|h| h.found.clone()).collect::<Vec<_>>(), vec![Found::Label(1), Found::Label(0)]);
        assert!(search("  ", [(0, &a)]).is_empty());
        assert!(search("plum", [(0, &a)]).is_empty());
    }

    fn picture(at: [f32; 2], tags: &[&str], group: u64) -> Shape {
        let mut s = crate::canvas::shape::tests_support::rect(at, [at[0] + 10.0, at[1] + 10.0]);
        s.set_group(group);
        s.set_tags(&tags.iter().map(|t| t.to_string()).collect::<Vec<_>>());
        s
    }

    #[test]
    fn tags_find_objects_and_a_group_as_one() {
        let mut c = slide(&[([0.0, 300.0], "walk notes")]);
        let l = &mut c.layers[0];
        l.shapes.push(picture([0.0, 0.0], &["Walk cycle", "ref"], 0)); // 1
        l.shapes.push(picture([50.0, 100.0], &[], 7)); // 2: in group 7, untagged
        l.shapes.push(picture([0.0, 100.0], &["walk cycle"], 7)); // 3: tagged alone in it
        l.shapes.push(picture([0.0, 200.0], &["pose"], 0)); // 4
        let hits = search("walk", [(0, &c)]);
        let found: Vec<Found> = hits.iter().map(|h| h.found.clone()).collect();
        // In group 7 only the tagged member is found, not the whole group.
        assert_eq!(found, vec![
            Found::Tagged { strokes: vec![], shapes: vec![1], what: "Rectangle".into() },
            Found::Tagged { strokes: vec![], shapes: vec![3], what: "Rectangle".into() },
            Found::Label(0),
        ]);
        // Tagged as one (every member has it), the group is found as one.
        c.layers[0].shapes[2].set_tags(&["walk cycle".to_string()]);
        let group = search("walk", [(0, &c)]).into_iter().map(|h| h.found).find(|f| matches!(f, Found::Tagged { shapes, .. } if shapes.len() == 2));
        assert_eq!(group, Some(Found::Tagged { strokes: vec![], shapes: vec![2, 3], what: "Group of 2".into() }));
        c.layers[0].shapes[2].set_tags(&[]);
        let hits = search("walk", [(0, &c)]);
        assert_eq!(&hits[0].snippet[hits[0].mark.clone()], "Walk");
        assert_eq!(hits[0].snippet, "#Walk cycle  #ref");
        // A `#` typed finds tags only.
        assert_eq!(search("#pose", [(0, &c)]).len(), 1);
        // A label found by its text is not listed again for its tags.
        c.layers[0].shapes[0].set_tags(&["walk".to_string()]);
        assert_eq!(search("walk", [(0, &c)]).len(), 3);
    }

    #[test]
    fn tags_are_saved_and_untagged_objects_save_as_before() {
        let mut c = Canvas::default();
        c.layers[0].shapes.push(picture([0.0, 0.0], &[], 0));
        let plain = crate::persistence::serialize_canvas(&c).unwrap();
        assert!(!plain.contains("tags"), "no tags, nothing new written");
        c.layers[0].shapes[0].set_tags(&["pose".to_string()]);
        let saved = crate::persistence::serialize_canvas(&c).unwrap();
        let back = crate::persistence::parse_canvas(&saved, 1).unwrap();
        assert_eq!(back.layers[0].shapes[0].tags(), ["pose".to_string()]);
        // A file from before tags reads as untagged.
        let old = crate::persistence::parse_canvas(&plain, 1).unwrap();
        assert!(old.layers[0].shapes[0].tags().is_empty());
    }

    #[test]
    fn tags_are_read_as_typed() {
        use crate::canvas::shape::parse_tags;
        // Spaces or commas between; one word each.
        assert_eq!(parse_tags(" #pose, walk   cycle,,Pose , ref walk-cycle"), vec!["pose", "walk", "cycle", "ref", "walk-cycle"]);
        assert!(parse_tags(" , ,").is_empty());
    }

    #[test]
    fn hidden_layers_are_found_and_said() {
        let mut c = slide(&[([0.0, 0.0], "note")]);
        c.layers[0].visible = false;
        assert!(search("note", [(0, &c)])[0].hidden);
    }

    #[test]
    fn long_labels_are_cut_around_the_match() {
        let text: Vec<char> = format!("{}needle{}", "a".repeat(80), "b".repeat(80)).chars().collect();
        let at = 80;
        let (s, mark) = snippet(&text, at, 6);
        assert!(s.starts_with('…') && s.ends_with('…'));
        assert_eq!(&s[mark], "needle");
        assert!(s.chars().count() <= SNIPPET + 2);
        // A match at the very end keeps its tail.
        let text: Vec<char> = format!("{}end", "a".repeat(100)).chars().collect();
        let (s, mark) = snippet(&text, 100, 3);
        assert_eq!(&s[mark], "end");
        assert!(!s.ends_with('…'));
    }

    #[test]
    fn enter_walks_the_matches_and_arrows_move() {
        let mut bar = FindBar::default();
        // The highlighted match first, then on, wrapping round.
        assert_eq!(bar.step(3, true, false, false, false), Some(0));
        assert_eq!(bar.step(3, true, false, false, false), Some(1));
        assert_eq!(bar.step(3, true, false, false, false), Some(2));
        assert_eq!(bar.step(3, true, false, false, false), Some(0));
        assert_eq!(bar.step(3, false, true, false, false), Some(2), "Shift+Enter goes back");
        assert_eq!(bar.step(3, false, false, true, false), Some(1));
        assert_eq!(bar.step(3, false, false, false, true), Some(2));
        assert_eq!(bar.step(3, false, false, false, false), None);
        assert_eq!(bar.step(0, true, false, false, false), None);
    }
}
