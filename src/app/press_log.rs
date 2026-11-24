//! Diagnostics for a fault seen now and then: with the Select tool, a
//! press on the slide starts nothing (no box, no pick) on every slide,
//! until something resets it (locking and unlocking the slide, moving
//! the window to another monitor). Each such press appends one line to
//! `input-debug.log` in the data folder with everything that decides
//! where a press goes, so the next occurrence names the gate that held
//! it. Remove once the cause is found and fixed.

use super::*;
use std::fmt::Write as _;
use std::io::Write as _;

/// Past this size the log starts over.
const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;

/// Areas the app shows, by name, to say which one a press landed on.
const AREAS: &[&str] = &[
    "osn_toolbar",
    "osn_slide_panel",
    "osn_layer_panel",
    "osn_toast",
    "osn_dup_bar",
    "osn_slide_status",
    "osn_arrange_bar",
    "osn_pin_bar",
    "osn_command_palette",
    "osn_find_bar",
    "osn_crop_bar",
    "osn_pad_paper",
    "osn_rec_indicator",
    "osn_drop_hint",
    "osn_pureref_progress",
    "osn_layer_preview_popup",
    "help",
];

/// What this frame's dispatch was handed (see `app::pointer`).
pub(super) struct Gates {
    pub drag_pans: bool,
    pub space_held: bool,
    pub editable: bool,
    pub pen_owns_pointer: bool,
    pub toolbar_clicked: bool,
    pub canvas_rect: egui::Rect,
}

/// A press this frame and the state it met, taken before dispatch.
pub(super) struct Press {
    line: String,
}

impl OnScreenNotesApp {
    /// Before this frame's pointer dispatch: when a press starts with the
    /// Select tool, write down everything that decides where it goes.
    pub(super) fn press_log_before(&self, ctx: &egui::Context, pen_events: &[crate::input::pen::PenEvent], g: &Gates) -> Option<Press> {
        use crate::input::pen::PenEvent;
        if self.ws.tool.kind != crate::tools::ToolKind::Select {
            return None;
        }
        let pen_down = pen_events.iter().find_map(|e| match e {
            PenEvent::Down(s) => Some(egui::pos2(s.pos[0], s.pos[1])),
            _ => None,
        });
        let mouse_down = ctx.input(|i| i.pointer.primary_pressed().then(|| i.pointer.press_origin().or(i.pointer.interact_pos())).flatten());
        let (source, pos) = match (pen_down, ctx.input(|i| i.pointer.primary_pressed())) {
            (Some(p), _) => ("pen", Some(p)),
            (None, true) => ("mouse", mouse_down),
            (None, false) => return None,
        };

        let mut s = String::new();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        let _ = write!(s, "t={}.{:03} src={source} pos={pos:?}", now.as_secs(), now.subsec_millis());
        let c = &self.canvases;
        let _ = write!(s, " | slide={} editable={} locked={} opening={}", c.active_index(), g.editable, c.is_locked(c.active_index()), c.opening().is_some());
        let _ = write!(s, " | drag_pans={} space_held={} toolbar_clicked={} canvas_rect={:?}", g.drag_pans, g.space_held, g.toolbar_clicked, g.canvas_rect);
        let kinds: Vec<&str> = pen_events.iter().map(|e| match e { PenEvent::Down(_) => "D", PenEvent::Move(_) => "M", PenEvent::Up(_) => "U" }).collect();
        let _ = write!(
            s,
            " | pen_owns={} pen_events={} pen.is_down={} pen.used250={} pen.seen250={} pen.seen1s={}",
            g.pen_owns_pointer,
            kinds.concat(),
            self.pen.is_down(),
            self.pen.used_within(PEN_OWNS_POINTER),
            self.pen.seen_within(PEN_OWNS_POINTER),
            self.pen.seen_within(Duration::from_secs(1)),
        );
        let _ = write!(
            s,
            " | mouse_gesture={} mouse_was_down={} pen_blocked={} pen_picking={} pen_using_eraser={} eyedropper={}",
            self.ws.mouse_gesture, self.ws.mouse_was_down, self.ws.pen_blocked, self.ws.pen_picking, self.ws.pen_using_eraser, self.ws.tool.eyedropper_pending,
        );
        let _ = write!(
            s,
            " | region_select={} loupe={} crop=[{}] pad=[{}] pin_sel={:?} pin_busy={} sel_gesture={} sel_n={}",
            self.region_select.is_some(),
            self.loupe.is_some(),
            self.ws.crop.debug(),
            self.ws.pad.debug(),
            self.ws.pin_tool.selected,
            self.ws.pin_tool.busy(),
            self.ws.tool.select.gesture.is_some(),
            self.ws.tool.select.sel.strokes.len() + self.ws.tool.select.sel.shapes.len(),
        );
        let _ = write!(
            s,
            " | slide_window={} pinned_hwnd={:?} pin_picking={} overlay_visible={} ui_visible={}",
            self.slide_window.is_some(), self.pinned_hwnd, self.pin_picking, self.overlay_visible, self.ws.ui_visible,
        );
        ctx.input(|i| {
            let _ = write!(
                s,
                " | egui: mods={:?} keys_down={:?} primary_down={} any_down={} hover={:?} interact={:?} latest={:?} focused={} ppp={} screen={:?} inner={:?} outer={:?}",
                i.modifiers,
                i.keys_down,
                i.pointer.primary_down(),
                i.pointer.any_down(),
                i.pointer.hover_pos(),
                i.pointer.interact_pos(),
                i.pointer.latest_pos(),
                i.focused,
                i.pixels_per_point,
                i.screen_rect,
                i.viewport().inner_rect,
                i.viewport().outer_rect,
            );
        });
        let _ = write!(s, " wants_kb={} wants_ptr={}", ctx.wants_keyboard_input(), ctx.wants_pointer_input());
        if let Some(p) = pos {
            match ctx.layer_id_at(p) {
                Some(layer) => {
                    let name = AREAS.iter().find(|n| egui::Id::new(**n) == layer.id).copied().unwrap_or("?");
                    let rect = ctx.memory(|m| m.area_rect(layer.id));
                    let _ = write!(s, " | layer_at_pos={name} order={:?} id={:?} rect={rect:?}", layer.order, layer.id);
                }
                None => {
                    let _ = write!(s, " | layer_at_pos=none");
                }
            }
        }
        Some(Press { line: s })
    }

    /// After this frame's pointer dispatch: log the press if it started
    /// nothing with the Select tool (no gesture, no pin taken).
    pub(super) fn press_log_after(&self, press: Option<Press>) {
        let Some(press) = press else { return };
        if self.ws.tool.kind != crate::tools::ToolKind::Select || self.ws.tool.select.gesture.is_some() || self.ws.pin_tool.busy() {
            return;
        }
        let line = format!(
            "{} | after: mouse_gesture={} pen_blocked={} pen_using_eraser={}\n",
            press.line, self.ws.mouse_gesture, self.ws.pen_blocked, self.ws.pen_using_eraser,
        );
        if let Ok(dir) = crate::persistence::paths::data_dir() {
            append_capped(&dir.join("input-debug.log"), &line, MAX_LOG_BYTES);
        }
    }
}

/// Add `line` to the file at `path`, first emptying it if it has grown
/// past `max` bytes.
fn append_capped(path: &std::path::Path, line: &str, max: u64) {
    let start_over = std::fs::metadata(path).is_ok_and(|m| m.len() > max);
    let file = std::fs::OpenOptions::new().create(true).write(true).append(!start_over).truncate(start_over).open(path);
    if let Ok(mut f) = file {
        let _ = f.write_all(line.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_grows_line_by_line_and_starts_over_past_its_cap() {
        let dir = std::env::temp_dir().join(format!("osn-press-log-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("input-debug.log");
        let _ = std::fs::remove_file(&path);
        append_capped(&path, "one\n", 8);
        append_capped(&path, "two\n", 8);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\ntwo\n");
        append_capped(&path, "three\n", 4);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "three\n", "past the cap it starts over");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
