//! The slide window's Pins view: the global pins moved, resized and
//! turned with the Select tool, as on the canvas, and the pen's gestures
//! over the window taken from the canvas's for it. On a slide the window
//! runs the app's own frame instead (`app::view_pass`).
//!
//! The window keeps its own copy of the tool (`WindowEdit::tool`): the
//! toolbar's kind, colour, size, style and smoothing, followed every
//! frame, but its own stroke in progress, shape being dragged and
//! selection. So a gesture in one window never shows up or lands in the
//! other, and the canvas's input code is left as it is.
//!
//! Everything runs through `CanvasManager::edit_slide_in_view`: the slide
//! is seen through the window's pan and zoom while a tool works on it,
//! so pens and the eraser work at their size on screen here as they do
//! on the canvas.
//!
//! The pen reaches the app through one Wintab queue, in screen pixels.
//! A gesture that starts over this window is taken out of the canvas's
//! events (`claim_pen`) and replayed here in the window's own frame.
//!
//! Tools that need the overlay itself — fill, text, the lassos, the
//! spotlight — are not here yet; with one of them picked a drag pans.

use crate::canvas::stroke::PenSample;
use crate::canvas::CanvasManager;
use crate::input::hotkeys_config::ParsedHotkeys;
use crate::input::pen::PenEvent;
use crate::tools::{ActiveTool, ToolKind};

/// What a gesture in flight does.
#[derive(Clone, Copy, PartialEq)]
enum Gesture {
    /// The toolbar's tool.
    Tool,
    /// Pans the view; the last point, window points.
    Pan([f32; 2]),
    /// Landed on the card: the card's widgets take it (as a click).
    Ui,
}

/// The window's tool and the gestures in flight.
pub struct WindowEdit {
    tool: ActiveTool,
    /// Pen events taken for this window this frame, in screen pixels.
    pen_queue: Vec<PenEvent>,
    /// A pen gesture started over this window: its moves and its lift
    /// come here too, wherever the pen goes.
    pen_claimed: bool,
    pen: Option<Gesture>,
    mouse: Option<Gesture>,
    mouse_was_down: bool,
    /// Something to tell the user (a drop done or refused), for the app's
    /// toast.
    notice: Option<String>,
    /// Pins mode: which global pin is selected here, and the move /
    /// resize / turn in flight. The canvas has its own.
    pin_tool: crate::canvas::pins::PinTool,
    /// A pin was double-clicked: the app goes to its slide.
    open_pin: Option<u64>,
}

/// What the app tells the window about input this frame.
pub struct Input<'a> {
    /// The pen drove the pointer a moment ago, so mouse presses are the
    /// tablet driver's copies of the pen's and must not draw twice.
    pub pen_owns_pointer: bool,
    pub keys: &'a ParsedHotkeys,
    /// The canvas area's size, points: a pin moved in the window is kept
    /// on it, so it can be reached there again.
    pub main_area: [f32; 2],
}

fn sample_of(ev: &PenEvent) -> PenSample {
    match ev {
        PenEvent::Down(s) | PenEvent::Move(s) | PenEvent::Up(s) => *s,
    }
}

fn with_sample(ev: PenEvent, s: PenSample) -> PenEvent {
    match ev {
        PenEvent::Down(_) => PenEvent::Down(s),
        PenEvent::Move(_) => PenEvent::Move(s),
        PenEvent::Up(_) => PenEvent::Up(s),
    }
}

/// Where dropped objects go: the slide point `start` (where the drag
/// began, under the pointer then) lands on the target slide's point
/// `target` (under the pointer at the release), and everything keeps its
/// size on screen (`k`: the source's screen pixels per slide unit over
/// the target's).
pub fn drop_mapping(start: [f32; 2], target: [f32; 2], k: f32) -> crate::canvas::affine::Affine {
    use crate::canvas::affine::Affine;
    Affine::translate([-start[0], -start[1]]).then(Affine::scale_about([0.0, 0.0], k, k)).then(Affine::translate(target))
}

impl WindowEdit {
    pub fn new(cfg: &crate::config::AppConfig) -> Self {
        Self {
            tool: ActiveTool::from_config(cfg),
            pen_queue: Vec::new(),
            pen_claimed: false,
            pen: None,
            mouse: None,
            mouse_was_down: false,
            notice: None,
            pin_tool: crate::canvas::pins::PinTool::default(),
            open_pin: None,
        }
    }

    /// A pin double-clicked in the window, for the app to go to its slide.
    pub fn take_open_pin(&mut self) -> Option<u64> {
        self.open_pin.take()
    }

    /// The pin selected in the window.
    pub fn selected_pin(&self) -> Option<u64> {
        self.pin_tool.selected
    }

    /// Let go of the selected pin (Pins mode turned off).
    pub fn drop_pin_selection(&mut self) {
        self.pin_tool.clear();
    }

    /// The pen events taken for this window, in its own points (`hwnd` is
    /// the window's), each with its screen position in pixels.
    fn drain_pen(&mut self, ctx: &egui::Context, hwnd: isize) -> Vec<(PenEvent, [f32; 2])> {
        let queue = std::mem::take(&mut self.pen_queue);
        let Some((l, t, _, _)) = crate::platform::client_rect(hwnd) else { return Vec::new() };
        let ppp = ctx.pixels_per_point();
        queue
            .into_iter()
            .map(|ev| {
                let s = sample_of(&ev);
                let local = PenSample { pos: [(s.pos[0] - l as f32) / ppp, (s.pos[1] - t as f32) / ppp], ..s };
                (with_sample(ev, local), s.pos)
            })
            .collect()
    }

    /// Pins mode: with the Select tool a press on a pin selects it, and a
    /// drag moves, resizes or turns it, as on the canvas; a double-click
    /// goes to its slide. Any other press pans. `origin` is where a pin's
    /// screen point 0 shows (canvas area corner plus pan), `zoom` the
    /// window's. The pins keep their places as the canvas has them.
    #[allow(clippy::too_many_arguments)]
    pub fn pins_pointer(
        &mut self,
        ctx: &egui::Context,
        response: &egui::Response,
        manager: &mut CanvasManager,
        origin: egui::Pos2,
        zoom: f32,
        pan: &mut [f32; 2],
        input: &Input,
        hwnd: isize,
        editable: bool,
    ) {
        // Locked, any press pans.
        let select = editable && self.tool.kind == ToolKind::Select;
        let ids: Vec<u64> = manager.global_pins().iter().map(|p| p.pin.id).collect();
        if !select || self.pin_tool.selected.is_some_and(|id| !ids.contains(&id)) {
            self.pin_tool.clear();
        }
        let shift = ctx.input(|i| i.modifiers.shift);
        let o = [origin.x, origin.y];

        // The pen first; the mouse only while the pen is not the one
        // moving the pointer (the tablet driver copies its presses).
        for (ev, _) in self.drain_pen(ctx, hwnd) {
            let g = self.pen;
            self.pen = self.pin_event(ev, g, ctx, manager, o, zoom, pan, select, shift, input.main_area);
        }
        let down = ctx.input(|i| i.pointer.primary_down());
        if !input.pen_owns_pointer && self.pen.is_none() {
            if let Some(p) = ctx.input(|i| i.pointer.interact_pos()) {
                let s = PenSample::from_pos([p.x, p.y]);
                let ev = match (down, self.mouse_was_down) {
                    (true, false) if response.is_pointer_button_down_on() => Some(PenEvent::Down(s)),
                    (true, true) if self.mouse.is_some() => Some(PenEvent::Move(s)),
                    (false, true) if self.mouse.is_some() => Some(PenEvent::Up(s)),
                    _ => None,
                };
                if let Some(ev) = ev {
                    let g = self.mouse;
                    self.mouse = self.pin_event(ev, g, ctx, manager, o, zoom, pan, select, shift, input.main_area);
                }
            }
        }
        self.mouse_was_down = down;

        // The cursor says what a press would do.
        if let Some(p) = response.hover_pos() {
            let w = [p.x, p.y];
            let cursor = if select {
                let pins = manager.global_pins();
                let shown: Vec<crate::canvas::pins::Pin> = pins.iter().map(|p| p.pin.viewed(o, zoom)).collect();
                let view: Vec<crate::canvas::pins::Pinned> = pins.iter().zip(&shown).map(|(p, s)| crate::canvas::pins::Pinned { pin: *s, ..*p }).collect();
                crate::app::pin_cursor(&self.pin_tool, w, &view)
            } else {
                None
            };
            let panning = matches!(self.pen, Some(Gesture::Pan(_))) || matches!(self.mouse, Some(Gesture::Pan(_)));
            ctx.set_cursor_icon(cursor.unwrap_or(if panning { egui::CursorIcon::Grabbing } else { egui::CursorIcon::Grab }));
        }
    }

    /// One pen or mouse event in Pins mode, given the gesture it belongs
    /// to; returns that gesture as it goes on (`None` once it ended).
    #[allow(clippy::too_many_arguments)]
    fn pin_event(
        &mut self,
        ev: PenEvent,
        g: Option<Gesture>,
        ctx: &egui::Context,
        manager: &mut CanvasManager,
        o: [f32; 2],
        zoom: f32,
        pan: &mut [f32; 2],
        select: bool,
        snap: bool,
        area: [f32; 2],
    ) -> Option<Gesture> {
        let w = sample_of(&ev).pos;
        match ev {
            PenEvent::Down(_) => {
                let on_ui = ctx.layer_id_at(egui::pos2(w[0], w[1])).is_some_and(|l| l.order > egui::Order::Background);
                Some(if on_ui {
                    Gesture::Ui
                } else if select {
                    match self.press_pin(w, manager, o, zoom) {
                        crate::canvas::pins::Press::Took => Gesture::Tool,
                        crate::canvas::pins::Press::Open(id) => {
                            self.open_pin = Some(id);
                            Gesture::Ui
                        }
                        crate::canvas::pins::Press::Missed => Gesture::Pan(w),
                    }
                } else {
                    Gesture::Pan(w)
                })
            }
            PenEvent::Move(_) | PenEvent::Up(_) => {
                let up = matches!(ev, PenEvent::Up(_));
                let next = match g {
                    Some(Gesture::Tool) => {
                        self.move_pin(w, snap, up, manager, o, zoom, area);
                        g
                    }
                    Some(Gesture::Pan(last)) => {
                        pan[0] += w[0] - last[0];
                        pan[1] += w[1] - last[1];
                        Some(Gesture::Pan(w))
                    }
                    g => g,
                };
                if up { None } else { next }
            }
        }
    }

    /// A press at window point `w` on the global pins.
    fn press_pin(&mut self, w: [f32; 2], manager: &mut CanvasManager, o: [f32; 2], zoom: f32) -> crate::canvas::pins::Press {
        let press = {
            let pins = manager.global_pins();
            let shown: Vec<crate::canvas::pins::Pin> = pins.iter().map(|p| p.pin.viewed(o, zoom)).collect();
            let view: Vec<crate::canvas::pins::Pinned> = pins.iter().zip(&shown).map(|(p, s)| crate::canvas::pins::Pinned { pin: *s, ..*p }).collect();
            self.pin_tool.press(w, &view)
        };
        if press == crate::canvas::pins::Press::Took {
            if let Some(id) = self.pin_tool.selected {
                manager.pin_board().raise(id);
            }
        }
        press
    }

    /// Carry the pin gesture to window point `w` (`up`: let go), working
    /// on the pins as the window shows them and writing the result back
    /// as the canvas places them, kept on the canvas area (`area`).
    #[allow(clippy::too_many_arguments)]
    fn move_pin(&mut self, w: [f32; 2], snap: bool, up: bool, manager: &mut CanvasManager, o: [f32; 2], zoom: f32, area: [f32; 2]) {
        let shown: Vec<crate::canvas::pins::Pin> = manager.global_pins().iter().map(|p| p.pin.viewed(o, zoom)).collect();
        let mut scratch = crate::canvas::pins::PinBoard::scratch(shown);
        if up {
            self.pin_tool.release(w, snap, &mut scratch);
        } else {
            self.pin_tool.drag(w, snap, &mut scratch);
        }
        let Some(id) = self.pin_tool.selected else { return };
        let Some(moved) = scratch.get_mut(id).map(|p| p.unviewed(o, zoom)) else { return };
        if let Some(pin) = manager.pin_board().get_mut(id) {
            pin.center = moved.center;
            pin.scale = moved.scale;
            pin.angle = moved.angle;
            if up {
                pin.center = [pin.center[0].clamp(0.0, area[0].max(0.0)), pin.center[1].clamp(0.0, area[1].max(0.0))];
            }
        }
    }

    /// Pins mode keys: Delete unpins the selected pin (its objects stay
    /// on their slide), Esc lets go of it, the tool keys pick the tool.
    pub fn pins_keys(&mut self, ctx: &egui::Context, manager: &mut CanvasManager, main: &mut ActiveTool, keys: &ParsedHotkeys, editable: bool) {
        let events: Vec<(egui::Key, egui::Modifiers)> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Key { key, pressed: true, modifiers, .. } => Some((*key, *modifiers)),
                    _ => None,
                })
                .collect()
        });
        for (key, mods) in events {
            if matches!(key, egui::Key::Delete | egui::Key::Backspace) {
                if let Some(id) = self.pin_tool.selected.filter(|_| editable) {
                    let slide = manager.pin_board().members(id).map(|(s, _)| s);
                    self.pin_tool.clear();
                    if manager.unpin(id) {
                        self.notice = Some(match slide {
                            Some(s) => format!("Unpinned — the objects stay on slide {}", s + 1),
                            None => "Unpinned".to_string(),
                        });
                    }
                }
            } else if key == egui::Key::Escape {
                self.pin_tool.clear();
            } else if !mods.ctrl && !mods.command && !mods.alt {
                crate::interaction::react_tool_shortcut(key, mods, main, keys);
            }
        }
    }

    /// What the window has to tell the user, once.
    pub fn take_notice(&mut self) -> Option<String> {
        self.notice.take()
    }

    /// Take the toolbar's choices: kind, colour, size, style, smoothing,
    /// shape snap. Switching tools drops what was in progress, as on the
    /// canvas.
    pub fn follow(&mut self, main: &ActiveTool) {
        if self.tool.kind != main.kind {
            self.tool.cancel();
            self.tool.select.sel.clear();
        }
        self.tool.kind = main.kind;
        self.tool.color = main.color;
        self.tool.size = main.size;
        self.tool.style = main.style;
        self.tool.smoothing = main.smoothing;
        self.tool.position_passes = main.position_passes;
        self.tool.border_style = main.border_style;
        self.tool.shape_snap = main.shape_snap;
    }

    /// The pen events taken for the window (`claim_pen`), in its client
    /// points, and its client area's top-left on screen: what the app's
    /// frame for the window reads, as the overlay's reads its own.
    pub fn take_pen_events(&mut self, hwnd: isize, ppp: f32) -> (Vec<PenEvent>, [i32; 2]) {
        let queue = std::mem::take(&mut self.pen_queue);
        let Some((l, t, _, _)) = crate::platform::client_rect(hwnd) else { return (Vec::new(), [0, 0]) };
        let ppp = ppp.max(1e-3);
        let events = queue
            .into_iter()
            .map(|ev| {
                let s = sample_of(&ev);
                with_sample(ev, PenSample { pos: [(s.pos[0] - l as f32) / ppp, (s.pos[1] - t as f32) / ppp], ..s })
            })
            .collect();
        (events, [l, t])
    }

    /// Take out of `events` (the canvas's pen events, in its window-client
    /// points from `client_origin`) every one of a gesture that started
    /// over the window `hwnd`, for the window's frame.
    pub fn claim_pen(&mut self, events: &mut Vec<PenEvent>, client_origin: [i32; 2], hwnd: isize) {
        if hwnd == 0 {
            return;
        }
        let mut queue = std::mem::take(&mut self.pen_queue);
        let mut claimed = self.pen_claimed;
        events.retain(|ev| {
            let s = sample_of(ev);
            let screen = [client_origin[0] as f32 + s.pos[0], client_origin[1] as f32 + s.pos[1]];
            let mine = match ev {
                PenEvent::Down(_) => {
                    claimed = crate::platform::top_level_window_at(screen[0] as i32, screen[1] as i32) == hwnd;
                    claimed
                }
                PenEvent::Move(_) => claimed,
                PenEvent::Up(_) => std::mem::replace(&mut claimed, false),
            };
            if mine {
                queue.push(with_sample(*ev, PenSample { pos: screen, ..s }));
            }
            !mine
        });
        self.pen_claimed = claimed;
        self.pen_queue = queue;
    }

    pub fn tool(&self) -> &ActiveTool {
        &self.tool
    }

    /// A gesture is drawing or moving something right now.
    pub fn busy(&self) -> bool {
        matches!(self.pen, Some(Gesture::Tool)) || matches!(self.mouse, Some(Gesture::Tool))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::canvas::Canvas;
    use crate::config::AppConfig;

    #[test]
    fn a_drop_lands_under_the_pointer_at_the_same_size_on_screen() {
        // Grabbed at slide point (10, 20), let go over target point
        // (100, 50); the target shows things twice as big per unit.
        let m = drop_mapping([10.0, 20.0], [100.0, 50.0], 0.5);
        assert_eq!(m.apply([10.0, 20.0]), [100.0, 50.0], "the grabbed point is under the pointer");
        let p = m.apply([30.0, 20.0]);
        assert!((p[0] - 110.0).abs() < 1e-4 && (p[1] - 50.0).abs() < 1e-4, "20 units away become 10: {p:?}");
    }

    #[test]
    fn a_pin_moves_in_the_window_and_keeps_its_canvas_place() {
        let mut slide = Canvas { dirty: false, ..Canvas::default() };
        slide.layers[0].shapes.push(crate::canvas::shape::Shape::Line {
            a: [0.0, 0.0],
            b: [100.0, 0.0],
            color: [255; 4],
            stroke_width: 4.0,
            border: Default::default(),
            pin: 0,
            group: 0,
            tags: Vec::new(),
            turn: 0.0,
        });
        let mut m = CanvasManager::for_tests(vec![slide]);
        let id = m.pin_objects(0, &[], &[0], [0.0, 0.0], false).expect("pinned");
        let before = m.global_pins()[0].pin.center;
        assert_eq!(before, [50.0, 0.0], "over the line, as the canvas shows it");

        // The window shows the pins at half size, 100 / 50 points in.
        let (o, zoom) = ([100.0, 50.0], 0.5);
        let mut e = WindowEdit::new(&AppConfig::default());
        e.tool.kind = ToolKind::Select;
        let at = [o[0] + before[0] * zoom, o[1] + before[1] * zoom];
        let stamp = m.slide(0).unwrap().edit_stamp();
        assert_eq!(e.press_pin(at, &mut m, o, zoom), crate::canvas::pins::Press::Took);
        assert_eq!(e.selected_pin(), Some(id));
        e.move_pin([at[0] + 20.0, at[1] + 10.0], false, true, &mut m, o, zoom, [2000.0, 2000.0]);
        let after = m.global_pins()[0].pin.center;
        assert!((after[0] - 90.0).abs() < 1e-3 && (after[1] - 20.0).abs() < 1e-3, "20 / 10 window points are 40 / 20 on the canvas: {after:?}");
        assert_eq!(m.slide(0).unwrap().edit_stamp(), stamp, "moving a pin edits no slide");
    }

    #[test]
    fn a_pin_seen_in_the_window_maps_back() {
        let pin = crate::canvas::pins::Pin { id: 1, center: [300.0, 200.0], scale: 2.0, angle: 0.5, local: false };
        let v = pin.viewed([40.0, -10.0], 0.25);
        assert_eq!((v.center, v.scale, v.angle), ([115.0, 40.0], 0.5, 0.5));
        let back = v.unviewed([40.0, -10.0], 0.25);
        assert!((back.center[0] - 300.0).abs() < 1e-3 && (back.center[1] - 200.0).abs() < 1e-3 && (back.scale - 2.0).abs() < 1e-5);
    }
}
