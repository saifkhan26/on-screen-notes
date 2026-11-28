//! Routes raw user input (egui events + pen events) to the active tool +
//! canvas. Holds small, focused helpers in submodules.

pub mod action_keys;
pub mod zoom_pan;

use crate::canvas::CanvasManager;
use crate::canvas::stroke::{PenSample, StrokeStyle};
use crate::config::SlideTabOrder;
use crate::input::hotkeys_config::ParsedHotkeys;
use crate::input::pen::PenEvent;
use crate::tools::{ActiveTool, ToolKind};

/// Convert window-pixel cursor coords to canvas-local coords using the
/// canvas's pan/zoom.
fn screen_to_canvas(screen: [f32; 2], canvas: &crate::canvas::canvas::Canvas) -> [f32; 2] {
    [
        (screen[0] - canvas.pan[0]) / canvas.zoom,
        (screen[1] - canvas.pan[1]) / canvas.zoom,
    ]
}

/// Mouse fallback when no pen tablet is present (or hovering over the app
/// without the pen). We synthesise a constant-pressure 1.0 sample so the
/// drawing pipeline is unchanged.
pub fn mouse_to_pen_event(
    pressed_now: bool,
    pressed_prev: &mut bool,
    cursor_screen: [f32; 2],
    canvas: &crate::canvas::canvas::Canvas,
) -> Option<PenEvent> {
    let pos = screen_to_canvas(cursor_screen, canvas);
    let sample = PenSample::from_pos(pos);
    let event = match (pressed_now, *pressed_prev) {
        (true, false) => Some(PenEvent::Down(sample)),
        (true, true)  => Some(PenEvent::Move(sample)),
        (false, true) => Some(PenEvent::Up(sample)),
        (false, false) => None,
    };
    *pressed_prev = pressed_now;
    event
}

/// React to keyboard shortcuts that select the active tool. Bindings
/// come from `HotkeysConfig` (parsed once at app start). Modifier
/// match is exact — `Pen = "p"` ignores `ctrl+p`.
pub fn react_tool_shortcut(
    key: egui::Key,
    mods: egui::Modifiers,
    tool: &mut ActiveTool,
    keys: &ParsedHotkeys,
) {
    let t = &keys.tool;
    // Pen + Pencil + Marker + Airbrush all select `ToolKind::Pen` and
    // only differ in `tool.style`. Pencil, marker and airbrush are a
    // drawing pad's brushes: with no pad open their keys only say so
    // (`pad_only_brush`). The pen's key on a pad picks its plain pen;
    // off one it keeps the pad's brush for the next pad.
    if let Some(b) = t.pen { if b.matches(key, mods) {
        tool.kind  = ToolKind::Pen;
        if tool.pad_open {
            tool.style = StrokeStyle::Default;
        }
        return;
    }}
    for (b, style) in [(t.pencil, StrokeStyle::Pencil), (t.marker, StrokeStyle::Marker), (t.airbrush, StrokeStyle::Airbrush)] {
        if b.is_some_and(|b| b.matches(key, mods)) {
            if tool.pad_open {
                tool.kind  = ToolKind::Pen;
                tool.style = style;
            } else {
                tool.pad_only_brush = Some(style);
            }
            return;
        }
    }
    if let Some(b) = t.laser          { if b.matches(key, mods) { tool.kind = ToolKind::Laser;      return; }}
    if let Some(b) = t.text           { if b.matches(key, mods) { tool.kind = ToolKind::Text;       return; }}
    if let Some(b) = t.spotlight      { if b.matches(key, mods) {
        // Press once = enter spotlight; press again = exit back to Pen.
        tool.kind = if matches!(tool.kind, ToolKind::Spotlight) {
            ToolKind::Pen
        } else {
            ToolKind::Spotlight
        };
        return;
    }}
    if let Some(b) = t.lasso          { if b.matches(key, mods) { tool.kind = ToolKind::LassoErase; return; }}
    if let Some(b) = t.lasso_select   { if b.matches(key, mods) { tool.kind = ToolKind::LassoSelect; return; }}
    if let Some(b) = t.select         { if b.matches(key, mods) { tool.kind = ToolKind::Select;     return; }}
    if let Some(b) = t.eyedropper     { if b.matches(key, mods) { tool.eyedropper_pending = true;   return; }}
    if let Some(b) = t.rect           { if b.matches(key, mods) { tool.kind = ToolKind::Rect;       return; }}
    if let Some(b) = t.ellipse        { if b.matches(key, mods) { tool.kind = ToolKind::Ellipse;    return; }}
    if let Some(b) = t.freehand_arrow { if b.matches(key, mods) { tool.kind = ToolKind::FreehandArrow; return; }}
    if let Some(b) = t.arrow          { if b.matches(key, mods) { tool.kind = ToolKind::Arrow;      return; }}
    if let Some(b) = t.line           { if b.matches(key, mods) { tool.kind = ToolKind::Line;       return; }}
    // The toolbar's filled-shape button, for the next rectangle or
    // ellipse drawn (and the selected ones: the app follows it).
    if let Some(b) = t.filled_shape   { if b.matches(key, mods) {
        tool.toggle_fill();
        return;
    }}
    // The toolbar's dashed-border button, for the next shape drawn.
    if let Some(b) = t.dashed_border  { if b.matches(key, mods) {
        tool.toggle_dashed();
        return;
    }}

    // Escape always cancels pending non-modal state regardless of
    // bindings — modal cancel is a UX primitive, not a shortcut.
    if key == egui::Key::Escape && !mods.ctrl && !mods.shift && !mods.alt {
        tool.eyedropper_pending = false;
        tool.lasso.clear();
        // First Esc drops an in-flight move / resize / rotate, the next
        // one drops the selection.
        tool.select.cancel();
    }
}

/// Keys for an open lasso polygon: Enter closes it (erasing or capturing
/// what is inside; `canvas` is the active one), a bare Backspace takes
/// back the last corner.
pub fn react_lasso_keys(
    key: egui::Key,
    mods: egui::Modifiers,
    tool: &mut ActiveTool,
    canvas: &mut crate::canvas::canvas::Canvas,
) {
    if !matches!(tool.kind, ToolKind::LassoErase | ToolKind::LassoSelect)
        || mods.ctrl || mods.shift || mods.alt
    {
        return;
    }
    match key {
        egui::Key::Enter => tool.finish_lasso(canvas),
        egui::Key::Backspace => tool.lasso.undo_corner(),
        _ => {}
    }
}

/// React to canvas-navigation key events. Bindings come from
/// `HotkeysConfig::canvas.{prev_canvas, next_canvas, cycle_*_slide,
/// first_slide, last_slide}`; `order` says what the cycle keys step
/// through.
pub fn react_canvas_nav(
    key: egui::Key,
    mods: egui::Modifiers,
    manager: &mut CanvasManager,
    keys: &ParsedHotkeys,
    order: SlideTabOrder,
) {
    let c = &keys.canvas;
    let recent = order == SlideTabOrder::RecentlyUsed;
    if let Some(b) = c.prev_canvas      { if b.matches(key, mods) { manager.prev(); return; }}
    if let Some(b) = c.next_canvas      { if b.matches(key, mods) { manager.next(); return; }}
    if let Some(b) = c.cycle_prev_slide { if b.matches(key, mods) {
        if recent { manager.walk_recent(false) } else { manager.prev() }
        return;
    }}
    if let Some(b) = c.cycle_next_slide { if b.matches(key, mods) {
        if recent { manager.walk_recent(true) } else { manager.cycle_next() }
        return;
    }}
    if let Some(b) = c.first_slide      { if b.matches(key, mods) { manager.go_to(0); return; }}
    if let Some(b) = c.last_slide       { if b.matches(key, mods) { manager.go_to(manager.count() - 1); }}
}

/// A recently-used walk lasts while the modifier its keys share stays
/// down (Ctrl for the default Ctrl+Tab); letting go ends it. Keys bound
/// without Ctrl or Alt end it at once, so each press flips between the
/// last two slides.
pub fn slide_walk_held(mods: egui::Modifiers, keys: &ParsedHotkeys) -> bool {
    let c = &keys.canvas;
    [c.cycle_next_slide, c.cycle_prev_slide]
        .into_iter()
        .flatten()
        .any(|b| (b.ctrl && (mods.ctrl || mods.command)) || (b.alt && mods.alt))
}

/// React to layer-navigation key events on the active canvas. Up
/// moves toward the top of the stack, down toward the bottom. Neither
/// wraps so holding the key does not cycle past the ends.
pub fn react_layer_nav(
    key: egui::Key,
    mods: egui::Modifiers,
    manager: &mut CanvasManager,
    keys: &ParsedHotkeys,
) {
    if let Some(b) = keys.canvas.next_layer { if b.matches(key, mods) {
        manager.active_mut().next_layer();
        return;
    }}
    if let Some(b) = keys.canvas.prev_layer { if b.matches(key, mods) {
        manager.active_mut().prev_layer();
    }}
}

/// Drain text-input events into the active text-tool buffer, if any.
/// Returns `true` when the events were consumed so the caller can
/// skip tool-shortcut + canvas-navigation handlers — that way `t`
/// typed mid-sentence becomes part of the label, not a tool switch.
///
/// Behaviour, as in any text field (`tools::text_caret`):
/// * typing and `Event::Paste` (Ctrl+V) go in at the caret, over any
///   selection; `Event::Copy` / `Event::Cut` (Ctrl+C / X) put the
///   selected text on the clipboard.
/// * arrows move the caret (Ctrl: by word), Home / End to the line's
///   ends (Ctrl: the text's), Shift with any of them selects, Ctrl+A
///   selects everything.
/// * `Backspace` / `Delete` take the selection, else a character (Ctrl:
///   a word) before / after the caret.
/// * `Shift+Enter` → a new line.
/// * `Enter` → commit buffer (`ActiveTool::commit_text`): a new label on
///   the active layer, or the edited label's new text.
/// * `Escape` → discard buffer without committing; an edited label keeps
///   its text.
///
/// Every key is the label's while it is typed: none reaches the slide's
/// shortcuts, so Ctrl+Z, say, never undoes an edit to the slide behind it.
pub fn route_text_input(
    ctx: &egui::Context,
    tool: &mut ActiveTool,
    manager: &mut CanvasManager,
) -> bool {
    if tool.in_progress_text.is_none() {
        return false;
    }
    // Another tool picked while typing: what was typed is kept.
    if tool.kind != ToolKind::Text {
        manager.edit_active(|c| tool.commit_text(c));
        return false;
    }
    let mut consumed = false;
    let mut commit = false;
    let mut cancel = false;
    let mut copied: Option<String> = None;

    ctx.input(|i| {
        let Some(t) = tool.in_progress_text.as_mut() else { return };
        for ev in &i.events {
            match ev {
                // egui sends Tab / Enter via Key events, not Event::Text
                // on Windows — so any Text event here is genuinely a
                // printable character.
                egui::Event::Text(s) => {
                    t.insert(s);
                    consumed = true;
                }
                // egui-winit hands Ctrl+V over as the clipboard's text,
                // not as a key press. The marker an in-app copy leaves
                // on the clipboard is not text to type.
                egui::Event::Paste(s) => {
                    if s != crate::app::CLIP_MARKER {
                        t.insert(&pasted_text(s));
                    }
                    consumed = true;
                }
                egui::Event::Copy => {
                    copied = t.selected_text().map(str::to_owned).or(copied.take());
                    consumed = true;
                }
                egui::Event::Cut => {
                    copied = t.cut().or(copied.take());
                    consumed = true;
                }
                egui::Event::Key { key, pressed: true, modifiers, .. } => {
                    let word = modifiers.ctrl || modifiers.command;
                    let extend = modifiers.shift;
                    match key {
                        egui::Key::Backspace => t.backspace(word),
                        egui::Key::Delete => t.delete_forward(word),
                        egui::Key::ArrowLeft => t.left(word, extend),
                        egui::Key::ArrowRight => t.right(word, extend),
                        egui::Key::ArrowUp => t.up(extend),
                        egui::Key::ArrowDown => t.down(extend),
                        egui::Key::Home => t.home(word, extend),
                        egui::Key::End => t.end(word, extend),
                        egui::Key::A if word => t.select_all(),
                        egui::Key::Enter if modifiers.shift => t.insert("\n"),
                        egui::Key::Enter => commit = true,
                        egui::Key::Escape => cancel = true,
                        _ => {}
                    }
                    consumed = true;
                }
                _ => {}
            }
        }
    });

    if let Some(text) = copied {
        ctx.copy_text(text);
    }
    if commit {
        manager.edit_active(|c| tool.commit_text(c));
    } else if cancel {
        tool.cancel_text();
    }

    consumed
}

/// Pasted text as it goes into a label: Windows line ends become the
/// plain newlines Shift+Enter types, other control characters are
/// dropped.
pub(crate) fn pasted_text(pasted: &str) -> String {
    let text = pasted.replace("\r\n", "\n").replace('\r', "\n");
    text.chars().filter(|c| matches!(c, '\n' | '\t') || !c.is_control()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A label being typed on a fresh slide, "Hi " so far.
    fn typing() -> (ActiveTool, CanvasManager) {
        let mut tool = ActiveTool::from_config(&crate::config::AppConfig::default());
        tool.kind = ToolKind::Text;
        tool.in_progress_text = Some(crate::tools::TextEdit {
            pos: [0.0, 0.0],
            buffer: "Hi ".into(),
            caret: 3,
            anchor: None,
            font_size: 18.0,
            color: [0, 0, 0, 255],
            editing: None,
        });
        (tool, CanvasManager::for_tests(vec![crate::canvas::canvas::Canvas::default()]))
    }

    /// One frame with `events`; true when the label took them.
    fn frame(events: Vec<egui::Event>, tool: &mut ActiveTool, manager: &mut CanvasManager) -> bool {
        frame_copying(events, tool, manager).0
    }

    /// `frame`, and what it put on the clipboard.
    fn frame_copying(events: Vec<egui::Event>, tool: &mut ActiveTool, manager: &mut CanvasManager) -> (bool, String) {
        let ctx = egui::Context::default();
        let mut consumed = false;
        let out = ctx.run(egui::RawInput { events, ..Default::default() }, |ctx| {
            consumed = route_text_input(ctx, tool, manager);
        });
        (consumed, out.platform_output.copied_text)
    }

    fn key(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }
    }

    #[test]
    fn keys_move_the_caret_and_copy_and_cut_take_the_selection() {
        let (mut tool, mut manager) = typing();
        let none = egui::Modifiers::NONE;
        let shift = egui::Modifiers::SHIFT;
        assert!(frame(vec![key(egui::Key::Home, none), egui::Event::Text("Oh, ".into())], &mut tool, &mut manager));
        assert_eq!(tool.in_progress_text.as_ref().unwrap().buffer, "Oh, Hi ");
        frame(vec![key(egui::Key::ArrowRight, shift), key(egui::Key::ArrowRight, shift)], &mut tool, &mut manager);
        let (_, copied) = frame_copying(vec![egui::Event::Copy], &mut tool, &mut manager);
        assert_eq!(copied, "Hi");
        let (_, cut) = frame_copying(vec![egui::Event::Cut], &mut tool, &mut manager);
        assert_eq!(cut, "Hi");
        assert_eq!(tool.in_progress_text.as_ref().unwrap().buffer, "Oh,  ");
        // Nothing selected: Ctrl+C copies nothing.
        assert_eq!(frame_copying(vec![egui::Event::Copy], &mut tool, &mut manager).1, "");
        // Every key is the label's: Ctrl+Z does not reach the slide.
        assert!(frame(vec![key(egui::Key::Z, egui::Modifiers::CTRL)], &mut tool, &mut manager));
    }

    #[test]
    fn ctrl_v_pastes_text_into_the_label() {
        let (mut tool, mut manager) = typing();
        assert!(frame(vec![egui::Event::Paste("two\r\nlines\u{0}".into())], &mut tool, &mut manager));
        let t = tool.in_progress_text.as_ref().expect("still typing");
        assert_eq!(t.buffer, "Hi two\nlines");
        assert_eq!(t.caret, t.buffer.len());
    }

    #[test]
    fn an_in_app_copy_is_not_typed_into_the_label() {
        let (mut tool, mut manager) = typing();
        assert!(frame(vec![egui::Event::Paste(crate::app::CLIP_MARKER.into())], &mut tool, &mut manager));
        assert_eq!(tool.in_progress_text.expect("still typing").buffer, "Hi ");
    }
}
