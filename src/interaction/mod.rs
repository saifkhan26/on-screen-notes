//! Routes raw user input (egui events + pen events) to the active tool +
//! canvas. Holds small, focused helpers in submodules.

pub mod click_through;
pub mod zoom_pan;

use crate::canvas::CanvasManager;
use crate::canvas::shape::Shape;
use crate::canvas::stroke::{PenSample, StrokeStyle};
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

/// React to keyboard shortcuts that select the active tool. The `egui::Key`
/// values come from the per-frame input state.
pub fn react_tool_shortcut(
    key: egui::Key,
    shift_held: bool,
    tool: &mut ActiveTool,
) {
    use egui::Key;
    match (key, shift_held) {
        // Pen + Pencil both select `ToolKind::Pen` and only differ in
        // `tool.style` — the renderer picks the look from the style.
        // Pressing P or B forces both fields in one shot.
        (Key::P, false) => {
            tool.kind  = ToolKind::Pen;
            tool.style = StrokeStyle::Default;
        }
        (Key::B, false) => {
            tool.kind  = ToolKind::Pen;
            tool.style = StrokeStyle::Pencil;
        }
        (Key::M, false) => {
            tool.kind  = ToolKind::Pen;
            tool.style = StrokeStyle::Marker;
        }
        (Key::G, false) => {
            tool.kind  = ToolKind::Pen;
            tool.style = StrokeStyle::Airbrush;
        }
        (Key::F, false) => tool.kind = ToolKind::Fill,
        // Eraser is reachable only via Ctrl-hold now (see
        // `app::eraser_override`); pressing E used to switch the active
        // tool, but the toolbar entry was removed for the laser-pointer
        // rework. Keep no mapping so the key doesn't silently flip the
        // user away from their pen mid-session.
        (Key::L, false) => tool.kind = ToolKind::Laser,
        (Key::T, false) => tool.kind = ToolKind::Text,
        // S toggles the spotlight: press once to dim-everything-but-
        // cursor, press again to exit back to the previous tool. We
        // remember the prior kind so Esc / S un-toggles cleanly.
        (Key::S, false) => {
            if matches!(tool.kind, ToolKind::Spotlight) {
                tool.kind = ToolKind::Pen;
            } else {
                tool.kind = ToolKind::Spotlight;
            }
        }
        // I = one-shot eyedropper. The next click samples a pixel
        // anywhere on screen and assigns it to `tool.color`; the
        // click does NOT begin a stroke. Esc cancels the pending
        // state without picking.
        (Key::I, false) => tool.eyedropper_pending = true,
        // X = lasso erase. Press once to enter; Esc / tool switch
        // returns to the previous tool. Sits next to the standard
        // Pen flow because it's a "clean up" gesture, not a new
        // primitive.
        (Key::X, false) => tool.kind = ToolKind::LassoErase,
        (Key::Escape, false) => {
            tool.eyedropper_pending = false;
            tool.in_progress_lasso  = None;
        }
        (Key::R, false) => tool.kind = ToolKind::Rect,
        (Key::O, false) => tool.kind = ToolKind::Ellipse,
        (Key::A, false) => tool.kind = ToolKind::Arrow,
        (Key::A, true)  => tool.kind = ToolKind::FreehandArrow,
        _ => {}
    }
}

/// React to canvas-navigation arrow keys.
pub fn react_canvas_nav(key: egui::Key, manager: &mut CanvasManager) {
    use egui::Key;
    match key {
        Key::ArrowLeft  => manager.prev(),
        Key::ArrowRight => manager.next(),
        _ => {}
    }
}

/// React to layer-navigation arrow keys (Up / Down) on the active
/// canvas. Up moves the active layer toward the top of the stack;
/// Down toward the bottom. Neither key wraps so holding the key does
/// not cycle the user past the ends of the stack.
pub fn react_layer_nav(key: egui::Key, manager: &mut CanvasManager) {
    use egui::Key;
    match key {
        Key::ArrowUp   => manager.active_mut().next_layer(),
        Key::ArrowDown => manager.active_mut().prev_layer(),
        _ => {}
    }
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
