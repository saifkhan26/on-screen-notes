//! Editing the text of a label as it is typed (`TextEdit`): the caret, a
//! selection, and the keys and clicks that move them. Offsets are bytes
//! into the buffer, always on a character boundary.

use super::TextEdit;
use crate::canvas::shape::{line_caret_x, line_char_at, text_box_pad, text_extent};

/// A label's line height per unit of font size, as it is painted.
const LINE_HEIGHT: f32 = 1.25;

/// Characters a Ctrl+arrow or a double-click treats as one word.
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The byte offset of character `chars` of `line` (its end when past it).
fn byte_of(line: &str, chars: usize) -> usize {
    line.char_indices().nth(chars).map_or(line.len(), |(i, _)| i)
}

impl TextEdit {
    /// The selected text's byte range, when any text is selected.
    pub fn selection(&self) -> Option<(usize, usize)> {
        let a = self.anchor?;
        (a != self.caret).then(|| (a.min(self.caret), a.max(self.caret)))
    }

    pub fn selected_text(&self) -> Option<&str> {
        self.selection().map(|(a, b)| &self.buffer[a..b])
    }

    /// Take the selected text out. False when none was selected.
    fn delete_selection(&mut self) -> bool {
        let Some((a, b)) = self.selection() else { return false };
        self.buffer.replace_range(a..b, "");
        self.caret = a;
        self.anchor = None;
        true
    }

    /// Type `s` at the caret, in place of the selection.
    pub fn insert(&mut self, s: &str) {
        self.delete_selection();
        self.buffer.insert_str(self.caret, s);
        self.caret += s.len();
        self.anchor = None;
    }

    /// Backspace: the selection, else the character before the caret, or
    /// with Ctrl (`word`) the word before it.
    pub fn backspace(&mut self, word: bool) {
        if self.delete_selection() {
            return;
        }
        let from = if word { self.word_left(self.caret) } else { self.char_left(self.caret) };
        self.buffer.replace_range(from..self.caret, "");
        self.caret = from;
    }

    /// Delete: the selection, else the character after the caret, or with
    /// Ctrl (`word`) the word after it.
    pub fn delete_forward(&mut self, word: bool) {
        if self.delete_selection() {
            return;
        }
        let to = if word { self.word_right(self.caret) } else { self.char_right(self.caret) };
        self.buffer.replace_range(self.caret..to, "");
    }

    /// Cut: take the selected text out and hand it back.
    pub fn cut(&mut self) -> Option<String> {
        let s = self.selected_text()?.to_owned();
        self.delete_selection();
        Some(s)
    }

    pub fn select_all(&mut self) {
        self.anchor = Some(0);
        self.caret = self.buffer.len();
    }

    /// The word around byte `at` selected, for a double-click. Off a word,
    /// the caret just goes there.
    pub fn select_word(&mut self, at: usize) {
        let at = at.min(self.buffer.len());
        let starts_word = self.buffer[at..].chars().next().is_some_and(is_word);
        let ends_word = self.buffer[..at].chars().next_back().is_some_and(is_word);
        if !starts_word && !ends_word {
            self.move_caret(at, false);
            return;
        }
        let start = self.buffer[..at]
            .char_indices()
            .rev()
            .take_while(|&(_, c)| is_word(c))
            .last()
            .map_or(at, |(i, _)| i);
        let end = at + self.buffer[at..].chars().take_while(|&c| is_word(c)).map(char::len_utf8).sum::<usize>();
        self.anchor = Some(start);
        self.caret = end;
    }

    /// Put the caret at byte `to`. `extend` (Shift held) keeps a selection
    /// from where it was, or starts one; otherwise any selection goes.
    pub fn move_caret(&mut self, to: usize, extend: bool) {
        if extend {
            self.anchor.get_or_insert(self.caret);
        } else {
            self.anchor = None;
        }
        let mut to = to.min(self.buffer.len());
        while !self.buffer.is_char_boundary(to) {
            to -= 1;
        }
        self.caret = to;
    }

    /// Left arrow: a character, or with Ctrl (`word`) a word. Without
    /// Shift a selection closes up at its start instead.
    pub fn left(&mut self, word: bool, extend: bool) {
        if let (false, Some((a, _))) = (extend, self.selection()) {
            return self.move_caret(a, false);
        }
        let to = if word { self.word_left(self.caret) } else { self.char_left(self.caret) };
        self.move_caret(to, extend);
    }

    /// Right arrow, as `left` the other way.
    pub fn right(&mut self, word: bool, extend: bool) {
        if let (false, Some((_, b))) = (extend, self.selection()) {
            return self.move_caret(b, false);
        }
        let to = if word { self.word_right(self.caret) } else { self.char_right(self.caret) };
        self.move_caret(to, extend);
    }

    /// Home: the start of the caret's line, or with Ctrl (`all`) of the
    /// whole text.
    pub fn home(&mut self, all: bool, extend: bool) {
        let to = if all { 0 } else { self.line_start(self.caret) };
        self.move_caret(to, extend);
    }

    /// End: the end of the caret's line, or with Ctrl (`all`) of the
    /// whole text.
    pub fn end(&mut self, all: bool, extend: bool) {
        let to = if all { self.buffer.len() } else { self.line_end(self.caret) };
        self.move_caret(to, extend);
    }

    /// Up arrow: the line above, as near the same place along it as it
    /// reaches; from the first line, the start.
    pub fn up(&mut self, extend: bool) {
        self.line_step(-1, extend);
    }

    /// Down arrow, as `up` the other way.
    pub fn down(&mut self, extend: bool) {
        self.line_step(1, extend);
    }

    fn line_step(&mut self, step: isize, extend: bool) {
        let (row, before) = self.caret_row();
        let x = line_caret_x(self.line(row), before.chars().count(), self.font_size);
        let rows = self.buffer.split('\n').count();
        let to = match row.checked_add_signed(step) {
            None => 0,
            Some(r) if r >= rows => self.buffer.len(),
            Some(r) => {
                let line = self.line(r);
                self.row_start(r) + byte_of(line, line_char_at(line, x, self.font_size))
            }
        };
        self.move_caret(to, extend);
    }

    /// Where in the text a press at `p` (a canvas point) puts the caret,
    /// or `None` when it is off the label (its text, and its box's
    /// padding round it).
    pub fn offset_at(&self, p: [f32; 2]) -> Option<usize> {
        let [x, y] = self.local(p);
        let [w, h] = text_extent(&self.buffer, self.font_size);
        let pad = text_box_pad(self.font_size);
        if x < -pad[0] || y < -pad[1] || x > w + pad[0] || y > h + pad[1] {
            return None;
        }
        Some(self.offset_near(p))
    }

    /// Where in the text the caret goes for the pointer at `p`, on the
    /// label or off it (the nearest line, the nearest place along it):
    /// for a drag that selects.
    pub fn offset_near(&self, p: [f32; 2]) -> usize {
        let [x, y] = self.local(p);
        let rows = self.buffer.split('\n').count();
        let row = ((y / (self.font_size * LINE_HEIGHT)).floor().max(0.0) as usize).min(rows - 1);
        let line = self.line(row);
        self.row_start(row) + byte_of(line, line_char_at(line, x, self.font_size))
    }

    /// Canvas point `p` in the label's own frame: x along its lines from
    /// its top-left corner, y down them.
    fn local(&self, p: [f32; 2]) -> [f32; 2] {
        let (s, c) = self.turn().sin_cos();
        let d = [p[0] - self.pos[0], p[1] - self.pos[1]];
        [c * d[0] + s * d[1], -s * d[0] + c * d[1]]
    }

    /// The label's turn: an edited label keeps its own; a new one is upright.
    pub fn turn(&self) -> f32 {
        self.editing.as_ref().map_or(0.0, |e| e.rotation)
    }

    /// The caret's line (from 0) and that line's text before the caret.
    pub fn caret_row(&self) -> (usize, &str) {
        let start = self.line_start(self.caret);
        (self.buffer[..start].matches('\n').count(), &self.buffer[start..self.caret])
    }

    /// The selection line by line: each line's number, its text before
    /// the selection, and the part of it selected.
    pub fn selected_rows(&self) -> Vec<(usize, &str, &str)> {
        let Some((a, b)) = self.selection() else { return Vec::new() };
        let mut out = Vec::new();
        let mut start = 0;
        for (row, line) in self.buffer.split('\n').enumerate() {
            let end = start + line.len();
            let (from, to) = (a.max(start), b.min(end));
            if from < to || (from == to && a <= start && end < b) {
                out.push((row, &line[..from - start], &line[from - start..to - start]));
            }
            start = end + 1;
        }
        out
    }

    fn line(&self, row: usize) -> &str {
        self.buffer.split('\n').nth(row).unwrap_or("")
    }

    fn row_start(&self, row: usize) -> usize {
        self.buffer.split('\n').take(row).map(|l| l.len() + 1).sum()
    }

    fn line_start(&self, at: usize) -> usize {
        self.buffer[..at].rfind('\n').map_or(0, |i| i + 1)
    }

    fn line_end(&self, at: usize) -> usize {
        self.buffer[at..].find('\n').map_or(self.buffer.len(), |i| at + i)
    }

    fn char_left(&self, at: usize) -> usize {
        self.buffer[..at].char_indices().next_back().map_or(0, |(i, _)| i)
    }

    fn char_right(&self, at: usize) -> usize {
        self.buffer[at..].chars().next().map_or(at, |c| at + c.len_utf8())
    }

    /// The start of the word before `at`, past any spaces or marks first.
    fn word_left(&self, at: usize) -> usize {
        let mut chars = self.buffer[..at].char_indices().rev().peekable();
        let mut to = at;
        while let Some((i, _)) = chars.next_if(|&(_, c)| !is_word(c)) {
            to = i;
        }
        while let Some((i, _)) = chars.next_if(|&(_, c)| is_word(c)) {
            to = i;
        }
        to
    }

    /// The end of the word after `at`, past any spaces or marks first.
    fn word_right(&self, at: usize) -> usize {
        let mut chars = self.buffer[at..].char_indices().peekable();
        let mut to = at;
        while let Some((i, c)) = chars.next_if(|&(_, c)| !is_word(c)) {
            to = at + i + c.len_utf8();
        }
        while let Some((i, c)) = chars.next_if(|&(_, c)| is_word(c)) {
            to = at + i + c.len_utf8();
        }
        to
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(text: &str, caret: usize) -> TextEdit {
        TextEdit { pos: [0.0, 0.0], buffer: text.into(), caret, anchor: None, font_size: 20.0, color: [255; 4], editing: None }
    }

    #[test]
    fn typing_goes_in_at_the_caret_and_over_a_selection() {
        let mut t = edit("hello world", 5);
        t.insert(",");
        assert_eq!((t.buffer.as_str(), t.caret), ("hello, world", 6));
        t.move_caret(7, false);
        t.move_caret(12, true);
        assert_eq!(t.selected_text(), Some("world"));
        t.insert("there");
        assert_eq!((t.buffer.as_str(), t.caret, t.selection()), ("hello, there", 12, None));
    }

    #[test]
    fn backspace_and_delete_work_at_the_caret_by_letter_or_word() {
        let mut t = edit("one two three", 7);
        t.backspace(false);
        assert_eq!((t.buffer.as_str(), t.caret), ("one tw three", 6));
        t.delete_forward(false);
        assert_eq!((t.buffer.as_str(), t.caret), ("one twthree", 6));
        t.backspace(true);
        assert_eq!((t.buffer.as_str(), t.caret), ("one three", 4));
        t.delete_forward(true);
        assert_eq!((t.buffer.as_str(), t.caret), ("one ", 4));
        // Ends of the text: nothing happens.
        let mut t = edit("ab", 0);
        t.backspace(false);
        assert_eq!(t.buffer, "ab");
        t.move_caret(2, false);
        t.delete_forward(true);
        assert_eq!(t.buffer, "ab");
    }

    #[test]
    fn arrows_move_by_letter_or_word_and_shift_selects() {
        let mut t = edit("héllo wörld", 0);
        t.right(false, false);
        t.right(false, false);
        assert_eq!(t.caret, "hé".len(), "a two-byte letter is one step");
        t.right(true, false);
        assert_eq!(t.caret, "héllo".len());
        t.right(true, true);
        assert_eq!(t.selected_text(), Some(" wörld"));
        // Without Shift a selection closes up at its near end.
        t.left(false, false);
        assert_eq!((t.caret, t.selection()), ("héllo".len(), None));
        t.left(true, false);
        assert_eq!(t.caret, 0);
        t.select_all();
        assert_eq!(t.selected_text(), Some("héllo wörld"));
        t.right(false, false);
        assert_eq!((t.caret, t.selection()), (t.buffer.len(), None));
    }

    #[test]
    fn home_end_and_up_down_follow_the_lines() {
        let mut t = edit("first line\nsecond\nthird line here", 3);
        t.end(false, false);
        assert_eq!(t.caret, "first line".len());
        t.down(false);
        assert_eq!(t.caret_row(), (1, "second"), "the shorter line's end");
        t.home(false, true);
        assert_eq!(t.selected_text(), Some("second"));
        t.down(false);
        assert_eq!(t.caret_row(), (2, ""));
        t.up(false);
        t.up(false);
        assert_eq!(t.caret_row(), (0, ""));
        t.up(false);
        assert_eq!(t.caret, 0);
        t.end(true, false);
        assert_eq!(t.caret, t.buffer.len());
        t.down(false);
        assert_eq!(t.caret, t.buffer.len());
        // Up keeps the place along the line.
        let mut t = edit("abcdefgh\nabcdefgh", "abcdefgh\nabcd".len());
        t.up(false);
        assert_eq!(t.caret_row(), (0, "abcd"));
    }

    #[test]
    fn a_double_click_takes_the_word_and_cut_takes_the_selection() {
        let mut t = edit("see filmstories.co.uk now", 0);
        t.select_word(7);
        assert_eq!(t.selected_text(), Some("filmstories"));
        assert_eq!(t.cut().as_deref(), Some("filmstories"));
        assert_eq!(t.buffer, "see .co.uk now");
        assert_eq!(t.cut(), None);
        t.select_word(3);
        assert_eq!(t.selected_text(), Some("see"), "right after a word takes it");
    }

    #[test]
    fn a_selection_over_lines_shows_on_each() {
        let mut t = edit("ab\ncd\nef", 1);
        t.move_caret(7, true);
        assert_eq!(t.selected_rows(), vec![(0, "a", "b"), (1, "", "cd"), (2, "", "e")]);
    }

    /// A press on the label puts the caret at the nearest letter boundary;
    /// off it, nothing.
    #[test]
    fn a_press_on_the_label_finds_its_place_in_the_text() {
        let t = edit("hello\nworld", 0);
        let x = line_caret_x("hello", 2, 20.0);
        assert_eq!(t.offset_at([x + 1.0, 10.0]), Some(2));
        assert_eq!(t.offset_at([-2.0, 30.0]), Some("hello\n".len()));
        assert_eq!(t.offset_at([1000.0, 10.0]), None);
        // Turned a quarter, its lines run down the slide.
        let mut turned = edit("hello", 0);
        turned.editing = Some(super::super::LabelEdit {
            layer: 0,
            index: 0,
            before: "hello".into(),
            rotation: std::f32::consts::FRAC_PI_2,
            background: None,
            return_to: None,
        });
        assert_eq!(turned.offset_at([-10.0, x + 1.0]), Some(2));
    }
}
