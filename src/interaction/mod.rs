//! Routes raw user input (egui events + pen events) to the active tool +
//! canvas. Holds small, focused helpers in submodules.

pub mod click_through;
pub mod zoom_pan;

use crate::canvas::CanvasManager;
use crate::canvas::shape::Shape;
use crate::canvas::stroke::{PenSample, StrokeStyle};
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
    // only differ in `tool.style` — the renderer picks the look from
    // the style. Setting both fields in one shot.
    if let Some(b) = t.pen { if b.matches(key, mods) {
        tool.kind  = ToolKind::Pen;
        tool.style = StrokeStyle::Default;
        return;
    }}
    if let Some(b) = t.pencil { if b.matches(key, mods) {
        tool.kind  = ToolKind::Pen;
        tool.style = StrokeStyle::Pencil;
        return;
    }}
    if let Some(b) = t.marker { if b.matches(key, mods) {
        tool.kind  = ToolKind::Pen;
        tool.style = StrokeStyle::Marker;
        return;
    }}
    if let Some(b) = t.airbrush { if b.matches(key, mods) {
        tool.kind  = ToolKind::Pen;
        tool.style = StrokeStyle::Airbrush;
        return;
    }}
    if let Some(b) = t.fill           { if b.matches(key, mods) { tool.kind = ToolKind::Fill;       return; }}
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
    if let Some(b) = t.eyedropper     { if b.matches(key, mods) { tool.eyedropper_pending = true;   return; }}
    if let Some(b) = t.rect           { if b.matches(key, mods) { tool.kind = ToolKind::Rect;       return; }}
    if let Some(b) = t.ellipse        { if b.matches(key, mods) { tool.kind = ToolKind::Ellipse;    return; }}
    if let Some(b) = t.freehand_arrow { if b.matches(key, mods) { tool.kind = ToolKind::FreehandArrow; return; }}
    if let Some(b) = t.arrow          { if b.matches(key, mods) { tool.kind = ToolKind::Arrow;      return; }}

    // Escape always cancels pending non-modal state regardless of
    // bindings — modal cancel is a UX primitive, not a shortcut.
    if key == egui::Key::Escape && !mods.ctrl && !mods.shift && !mods.alt {
        tool.eyedropper_pending = false;
        tool.in_progress_lasso  = None;
    }
}

/// React to canvas-navigation key events. Bindings come from
/// `HotkeysConfig::canvas.{prev_canvas, next_canvas}`.
pub fn react_canvas_nav(
    key: egui::Key,
    mods: egui::Modifiers,
    manager: &mut CanvasManager,
    keys: &ParsedHotkeys,
) {
    if let Some(b) = keys.canvas.prev_canvas { if b.matches(key, mods) { manager.prev(); return; }}
    if let Some(b) = keys.canvas.next_canvas { if b.matches(key, mods) { manager.next(); }}
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
/// Behaviour:
/// * `Event::Text(s)` → append to buffer.
/// * `Backspace` → pop the last char.
/// * `Shift+Enter` → append `\n` (multi-line label).
/// * `Enter` → commit buffer as a `Shape::Text` on the active layer.
/// * `Escape` → discard buffer without committing.
pub fn route_text_input(
    ctx: &egui::Context,
    tool: &mut ActiveTool,
    manager: &mut CanvasManager,
) -> bool {
    if tool.in_progress_text.is_none() {
        return false;
    }
    let mut consumed = false;
    let mut commit = false;
    let mut cancel = false;

    ctx.input(|i| {
        for ev in &i.events {
            match ev {
                egui::Event::Text(s) => {
                    if let Some(t) = tool.in_progress_text.as_mut() {
                        // egui sends Tab / Enter via Key events, not
                        // Event::Text on Windows — so any Text event
                        // here is genuinely a printable character.
                        t.buffer.push_str(s);
                        t.caret = t.buffer.len();
                    }
                    consumed = true;
                }
                egui::Event::Key { key: egui::Key::Backspace, pressed: true, .. } => {
                    if let Some(t) = tool.in_progress_text.as_mut() {
                        // Drop the last char (UTF-8 aware via pop()).
                        t.buffer.pop();
                        t.caret = t.buffer.len();
                    }
                    consumed = true;
                }
                egui::Event::Key { key: egui::Key::Enter, pressed: true, modifiers, .. } => {
                    if modifiers.shift {
                        if let Some(t) = tool.in_progress_text.as_mut() {
                            t.buffer.push('\n');
                            t.caret = t.buffer.len();
                        }
                    } else {
                        commit = true;
                    }
                    consumed = true;
                }
                egui::Event::Key { key: egui::Key::Escape, pressed: true, .. } => {
                    cancel = true;
                    consumed = true;
                }
                _ => {}
            }
        }
    });

    if commit {
        if let Some(t) = tool.in_progress_text.take() {
            if !t.buffer.is_empty() {
                manager.active_mut().add_shape(Shape::Text {
                    pos: t.pos,
                    content: t.buffer,
                    font_size: t.font_size,
                    color: t.color,
                });
            }
        }
    } else if cancel {
        tool.in_progress_text = None;
    }

    consumed
}
