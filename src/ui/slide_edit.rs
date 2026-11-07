//! Editing in the slide window: the pen, the mouse and the keys working
//! on the slide it shows, with the tool the toolbar has picked.
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

use crate::canvas::canvas::Canvas;
use crate::canvas::stroke::{PenSample, Stroke};
use crate::canvas::CanvasManager;
use crate::input::hotkeys_config::ParsedHotkeys;
use crate::input::pen::PenEvent;
use crate::tools::{ActiveTool, ToolKind};

/// What a gesture in flight does.
#[derive(Clone, Copy, PartialEq)]
enum Gesture {
    /// The toolbar's tool.
    Tool,
    /// The eraser, held with Ctrl at the press.
    Eraser,
    /// Pans the view; the last point, window points.
    Pan([f32; 2]),
    /// Landed on the card: the card's widgets take it (as a click).
    Ui,
}

/// The window's tool and the gestures in flight.
pub struct WindowEdit {
    tool: ActiveTool,
    eraser: ActiveTool,
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
}

/// Where the window's slide sits: the canvas area, and the window's pan
/// and zoom (slide point `p` shows at `rect.min + pan + p · zoom`).
#[derive(Clone, Copy)]
pub struct View {
    pub slide: usize,
    pub rect: egui::Rect,
    pub pan: [f32; 2],
    pub zoom: f32,
}

impl View {
    fn to_canvas(self, s: PenSample) -> PenSample {
        PenSample {
            pos: [(s.pos[0] - self.rect.min.x - self.pan[0]) / self.zoom, (s.pos[1] - self.rect.min.y - self.pan[1]) / self.zoom],
            ..s
        }
    }
}

/// What the app tells the window about input this frame.
pub struct Input<'a> {
    /// Space is held over the canvas: drags pan.
    pub space: bool,
    /// The pen drove the pointer a moment ago, so mouse presses are the
    /// tablet driver's copies of the pen's and must not draw twice.
    pub pen_owns_pointer: bool,
    pub keys: &'a ParsedHotkeys,
    /// Where the canvas is, for a selection dragged out of the window and
    /// let go over it.
    pub main: Option<MainCanvas>,
    /// A selection is being dragged on the canvas and the pointer is over
    /// this window: letting go drops it here.
    pub drop_hover: bool,
}

/// The canvas (main window), as a drop target.
#[derive(Clone, Copy)]
pub struct MainCanvas {
    pub hwnd: isize,
    /// The canvas area's top-left on screen, physical pixels.
    pub origin_px: [f32; 2],
    /// Physical pixels per point there.
    pub ppp: f32,
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

/// The tools the window can use; with any other a drag pans.
pub fn supported(kind: ToolKind) -> bool {
    matches!(
        kind,
        ToolKind::Pen
            | ToolKind::FreehandArrow
            | ToolKind::Eraser
            | ToolKind::Laser
            | ToolKind::Rect
            | ToolKind::Ellipse
            | ToolKind::Line
            | ToolKind::Arrow
            | ToolKind::Select
    )
}

impl WindowEdit {
    pub fn new(cfg: &crate::config::AppConfig) -> Self {
        let mut eraser = ActiveTool::from_config(cfg);
        eraser.kind = ToolKind::Eraser;
        Self {
            tool: ActiveTool::from_config(cfg),
            eraser,
            pen_queue: Vec::new(),
            pen_claimed: false,
            pen: None,
            mouse: None,
            mouse_was_down: false,
            notice: None,
        }
    }

    /// What the window has to tell the user, once.
    pub fn take_notice(&mut self) -> Option<String> {
        self.notice.take()
    }

    /// Objects dropped here from the canvas: select them, on slide `idx`.
    pub fn select_dropped(&mut self, placed: crate::canvas::hit::Selection, idx: usize, canvas: &Canvas) {
        self.tool.select.sel = placed;
        self.tool.select.gesture = None;
        self.tool.select.rebind(idx, canvas);
    }

    /// A selection is being moved here right now (the Select tool's move,
    /// not a resize, turn or rubber band); where its drag began, slide
    /// units.
    fn moving_from(&self) -> Option<[f32; 2]> {
        match (self.tool.kind, self.tool.select.gesture) {
            (ToolKind::Select, Some(crate::tools::select_tool::Gesture::Move { start, .. })) => Some(start),
            _ => None,
        }
    }

    /// A selection being moved here was let go at `screen` (physical
    /// pixels): over the canvas, it moves to the canvas's slide, landing
    /// under the pointer at the same size on screen, selected there.
    /// True when it was, so the release is not the Select tool's.
    #[allow(clippy::too_many_arguments)]
    fn drop_on_canvas(&mut self, screen: [f32; 2], manager: &mut CanvasManager, view: View, ppp: f32, input: &Input, main_tool: &mut ActiveTool) -> bool {
        let (Some(start), Some(main)) = (self.moving_from(), input.main) else { return false };
        if self.tool.select.sel.is_empty() || crate::platform::top_level_window_at(screen[0] as i32, screen[1] as i32) != main.hwnd {
            return false;
        }
        let to = manager.active_index();
        let Some(dst) = manager.slide(to) else { return false };
        if manager.is_locked(to) {
            self.notice = Some(format!("Slide {} is locked — unlock it to move objects there", to + 1));
            self.tool.select.gesture = None;
            return true;
        }
        let local = [(screen[0] - main.origin_px[0]) / main.ppp, (screen[1] - main.origin_px[1]) / main.ppp];
        let target = [(local[0] - dst.pan[0]) / dst.zoom, (local[1] - dst.pan[1]) / dst.zoom];
        let k = (view.zoom * ppp) / (dst.zoom * main.ppp).max(1e-6);
        let m = drop_mapping(start, target, k);
        let sel = std::mem::take(&mut self.tool.select.sel);
        self.tool.select.gesture = None;
        let li = manager.slide(view.slide).map_or(0, |c| c.active_layer);
        match manager.drop_objects(view.slide, to, li, &sel.strokes, &sel.shapes, &m) {
            Some(placed) => {
                let n = placed.strokes.len() + placed.shapes.len();
                main_tool.select.sel = placed;
                main_tool.select.gesture = None;
                main_tool.select.rebind(to, manager.active());
                if view.slide != to {
                    let what = if n == 1 { "1 object".to_string() } else { format!("{n} objects") };
                    self.notice = Some(format!("Moved {what} to slide {}", to + 1));
                }
            }
            None => self.tool.select.sel = sel,
        }
        true
    }

    /// Take the toolbar's choices: kind, colour, size, style, smoothing.
    /// Switching tools drops what was in progress, as on the canvas.
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
        // The held eraser is as big as the pen, as on the canvas.
        self.eraser.size = main.size;
        self.eraser.color = main.color;
        self.eraser.smoothing = main.smoothing;
        self.eraser.position_passes = main.position_passes;
    }

    /// Take out of `events` (the canvas's pen events, in its window-client
    /// points from `client_origin`) every one of a gesture that started
    /// over the window `hwnd`, to replay in the window.
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

    /// A drag on the canvas area should pan rather than use the tool.
    pub fn drag_pans(&self, editable: bool, input: &Input, ctx: &egui::Context) -> bool {
        !editable || !supported(self.tool.kind) || input.space || ctx.input(|i| i.key_down(egui::Key::Space))
    }

    /// Replay this frame's pen events on the slide, and the mouse when the
    /// pen is not the one moving the pointer. `pan` / `zoom` are the
    /// window's, moved by a pen drag that pans. `hwnd` is the window's.
    #[allow(clippy::too_many_arguments)]
    pub fn pointer(
        &mut self,
        ctx: &egui::Context,
        response: &egui::Response,
        manager: &mut CanvasManager,
        view: View,
        pan: &mut [f32; 2],
        editable: bool,
        input: &Input,
        hwnd: isize,
        main_tool: &mut ActiveTool,
    ) {
        let pans = self.drag_pans(editable, input, ctx);
        let (ctrl, shift) = ctx.input(|i| (i.modifiers.ctrl || i.modifiers.command, i.modifiers.shift));
        self.tool.select.shift = shift;
        if let Some(c) = manager.slide(view.slide) {
            self.tool.select.sync(self.tool.kind == ToolKind::Select, view.slide, c);
        }

        // ---- pen -------------------------------------------------------
        let origin = crate::platform::client_rect(hwnd).map(|(l, t, _, _)| [l as f32, t as f32]);
        let ppp = ctx.pixels_per_point();
        for ev in std::mem::take(&mut self.pen_queue) {
            let Some(o) = origin else { break };
            let screen = sample_of(&ev).pos;
            let s = sample_of(&ev);
            let s = PenSample { pos: [(s.pos[0] - o[0]) / ppp, (s.pos[1] - o[1]) / ppp], ..s };
            let ev = with_sample(ev, s);
            match ev {
                PenEvent::Down(_) => {
                    let on_ui = ctx.layer_id_at(egui::pos2(s.pos[0], s.pos[1])).is_some_and(|l| l.order > egui::Order::Background);
                    self.pen = Some(self.press(on_ui, pans, ctrl, s.pos));
                    self.apply(self.pen, ev, manager, view);
                }
                PenEvent::Move(_) => self.pen = self.drag(self.pen, ev, manager, view, pan),
                PenEvent::Up(_) => {
                    let dropped = self.pen == Some(Gesture::Tool) && self.drop_on_canvas(screen, manager, view, ppp, input, main_tool);
                    if !dropped {
                        self.apply(self.pen, ev, manager, view);
                    }
                    self.pen = None;
                }
            }
        }

        // ---- mouse -----------------------------------------------------
        // Only for the tool: a drag that pans is the window's own
        // navigation (`slide_window::navigate`).
        let down = ctx.input(|i| i.pointer.primary_down());
        let at = ctx.input(|i| i.pointer.interact_pos()).map(|p| [p.x, p.y]);
        if input.pen_owns_pointer || self.pen.is_some() {
            self.mouse_was_down = down;
            return;
        }
        if let Some(pos) = at {
            let sample = PenSample::from_pos(pos);
            match (down, self.mouse_was_down) {
                (true, false) if response.is_pointer_button_down_on() && !pans => {
                    self.mouse = Some(if ctrl && self.tool.kind != ToolKind::Select { Gesture::Eraser } else { Gesture::Tool });
                    self.apply(self.mouse, PenEvent::Down(sample), manager, view);
                }
                (true, true) => self.apply(self.mouse, PenEvent::Move(sample), manager, view),
                (false, true) => {
                    // Let go outside the window, maybe over the canvas.
                    let screen = origin.map(|o| [o[0] + pos[0] * ppp, o[1] + pos[1] * ppp]);
                    let dropped = self.mouse == Some(Gesture::Tool)
                        && screen.is_some_and(|s| self.drop_on_canvas(s, manager, view, ppp, input, main_tool));
                    if !dropped {
                        self.apply(self.mouse, PenEvent::Up(sample), manager, view);
                    }
                    self.mouse = None;
                }
                _ => {}
            }
        }
        self.mouse_was_down = down;
    }

    /// What a press starts.
    fn press(&self, on_ui: bool, pans: bool, ctrl: bool, at: [f32; 2]) -> Gesture {
        if on_ui {
            Gesture::Ui
        } else if pans {
            Gesture::Pan(at)
        } else if ctrl && self.tool.kind != ToolKind::Select {
            Gesture::Eraser
        } else {
            Gesture::Tool
        }
    }

    /// A pen move: the tool's, or a pan.
    fn drag(&mut self, g: Option<Gesture>, ev: PenEvent, manager: &mut CanvasManager, view: View, pan: &mut [f32; 2]) -> Option<Gesture> {
        match g {
            Some(Gesture::Pan(last)) => {
                let p = sample_of(&ev).pos;
                pan[0] += p[0] - last[0];
                pan[1] += p[1] - last[1];
                Some(Gesture::Pan(p))
            }
            g => {
                self.apply(g, ev, manager, view);
                g
            }
        }
    }

    /// Hand `ev` (window points) to the tool or the eraser, on the slide
    /// seen through the window's view.
    fn apply(&mut self, g: Option<Gesture>, ev: PenEvent, manager: &mut CanvasManager, view: View) {
        let tool = match g {
            Some(Gesture::Tool) => &mut self.tool,
            Some(Gesture::Eraser) => &mut self.eraser,
            _ => return,
        };
        let ev = with_sample(ev, view.to_canvas(sample_of(&ev)));
        manager.edit_slide_in_view(view.slide, view.pan, view.zoom, |c| crate::input::pen::dispatch(ev, tool, c));
    }

    /// Keys pressed while the window has the keyboard: undo and redo on
    /// its slide, Delete for a selection, Esc, and the tool keys, which
    /// pick the toolbar's tool (`main`).
    pub fn keys(&mut self, ctx: &egui::Context, manager: &mut CanvasManager, view: View, editable: bool, main: &mut ActiveTool, keys: &ParsedHotkeys) {
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
            let undo = keys.canvas.undo.as_ref().is_some_and(|b| b.matches(key, mods));
            let redo = keys.canvas.redo.as_ref().is_some_and(|b| b.matches(key, mods));
            if (undo || redo) && editable {
                self.tool.cancel();
                self.tool.select.sel.clear();
                manager.edit_slide_in_view(view.slide, view.pan, view.zoom, |c| if undo { c.undo() } else { c.redo() });
            } else if matches!(key, egui::Key::Delete | egui::Key::Backspace) && editable && !self.tool.select.sel.is_empty() {
                let sel = std::mem::take(&mut self.tool.select.sel);
                self.tool.select.gesture = None;
                manager.edit_slide_in_view(view.slide, view.pan, view.zoom, |c| {
                    let li = c.active_layer;
                    c.delete_objects(li, &sel.strokes, &sel.shapes);
                });
            } else if key == egui::Key::Escape {
                self.tool.cancel();
                if self.tool.kind == ToolKind::Select {
                    self.tool.select.cancel();
                }
            } else if !mods.ctrl && !mods.command && !mods.alt {
                crate::interaction::react_tool_shortcut(key, mods, main, keys);
            }
        }
    }

    /// The stroke being drawn (or rubbed out) now, with the look-ahead the
    /// canvas gives it, for drawing over the slide.
    pub fn live_stroke(&self) -> Option<Stroke> {
        self.eraser
            .in_progress_stroke
            .as_ref()
            .or(self.tool.in_progress_stroke.as_ref())
            .map(crate::ui::overlay::predict_ahead)
    }

    pub fn tool(&self) -> &ActiveTool {
        &self.tool
    }

    /// Laser trails still fading, pruned of the faded ones.
    pub fn laser(&mut self) -> &[(Stroke, std::time::Instant)] {
        let fade = crate::canvas::render::LASER_FADE_SECS;
        self.tool.laser_strokes.retain(|(_, up)| up.elapsed().as_secs_f32() < fade);
        &self.tool.laser_strokes
    }

    /// The Select tool's chrome for the slide seen through the view.
    pub fn chrome(&self, manager: &mut CanvasManager, view: View) -> Option<crate::tools::select_tool::Chrome> {
        if self.tool.kind != ToolKind::Select {
            return None;
        }
        manager.edit_slide_in_view(view.slide, view.pan, view.zoom, |c| self.tool.select.chrome(c))
    }

    /// The selection's in-flight move / resize / turn, for the renderer.
    pub fn sel_preview(&self, canvas: &Canvas, zoom: f32) -> Option<(usize, &crate::canvas::hit::Selection, crate::canvas::affine::Affine)> {
        if self.tool.kind != ToolKind::Select {
            return None;
        }
        self.tool.select.preview_affine(zoom).map(|m| (canvas.active_layer, &self.tool.select.sel, m))
    }

    /// A gesture is drawing or moving something right now.
    pub fn busy(&self) -> bool {
        matches!(self.pen, Some(Gesture::Tool | Gesture::Eraser)) || matches!(self.mouse, Some(Gesture::Tool | Gesture::Eraser))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;

    fn manager() -> CanvasManager {
        let blank = || Canvas { dirty: false, ..Canvas::default() };
        CanvasManager::for_tests(vec![blank(), blank()])
    }

    /// Slide 2 shown at twice its size, its origin 100 / 50 points in.
    fn view() -> View {
        View { slide: 1, rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0)), pan: [100.0, 50.0], zoom: 2.0 }
    }

    fn at(x: f32, y: f32) -> PenSample {
        PenSample { pos: [x, y], pressure: 0.8, tilt: [0.0, 0.0] }
    }

    fn draw(e: &mut WindowEdit, m: &mut CanvasManager, g: Gesture, from: [f32; 2], to: [f32; 2]) {
        e.apply(Some(g), PenEvent::Down(at(from[0], from[1])), m, view());
        for k in 1..8 {
            let t = k as f32 / 8.0;
            e.apply(Some(g), PenEvent::Move(at(from[0] + (to[0] - from[0]) * t, from[1] + (to[1] - from[1]) * t)), m, view());
        }
        e.apply(Some(g), PenEvent::Up(at(to[0], to[1])), m, view());
    }

    #[test]
    fn the_pen_draws_on_the_window_slide_through_its_view() {
        let mut m = manager();
        let mut e = WindowEdit::new(&AppConfig::default());
        e.tool.kind = ToolKind::Pen;
        draw(&mut e, &mut m, Gesture::Tool, [300.0, 250.0], [380.0, 270.0]);

        let b = m.slide(1).unwrap();
        let stroke = &b.layers[0].strokes[0];
        let start = stroke.samples[0].pos;
        assert!((start[0] - 100.0).abs() < 2.0 && (start[1] - 100.0).abs() < 2.0, "window point → slide point: {start:?}");
        assert!((stroke.px - 0.5).abs() < 1e-6, "drawn at its size on screen, at the window's zoom");
        assert_eq!((b.pan, b.zoom), ([0.0, 0.0], 1.0), "the slide's own view stays");
        assert!(b.dirty, "saved");
        assert!(m.slide(0).unwrap().layers[0].strokes.is_empty(), "the canvas's slide is untouched");
    }

    #[test]
    fn the_held_eraser_and_undo_work_on_the_window_slide() {
        let mut m = manager();
        let mut e = WindowEdit::new(&AppConfig::default());
        e.tool.kind = ToolKind::Pen;
        draw(&mut e, &mut m, Gesture::Tool, [300.0, 250.0], [380.0, 270.0]);
        draw(&mut e, &mut m, Gesture::Eraser, [290.0, 250.0], [390.0, 270.0]);
        assert!(m.slide(1).unwrap().layers[0].strokes.is_empty(), "rubbed out");

        // Ctrl+Z with the window focused brings it back.
        let ctx = egui::Context::default();
        let keys = crate::input::hotkeys_config::HotkeysConfig::default().parsed();
        let mut main = ActiveTool::from_config(&AppConfig::default());
        let mods = egui::Modifiers { ctrl: true, command: true, ..Default::default() };
        let input = egui::RawInput {
            events: vec![egui::Event::Key { key: egui::Key::Z, physical_key: None, pressed: true, repeat: false, modifiers: mods }],
            modifiers: mods,
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| e.keys(ctx, &mut m, view(), true, &mut main, &keys));
        assert_eq!(m.slide(1).unwrap().layers[0].strokes.len(), 1, "undone on the window's slide");
    }

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
    fn a_drop_on_the_same_slide_moves_in_place() {
        let mut m = manager();
        let mut e = WindowEdit::new(&AppConfig::default());
        e.tool.kind = ToolKind::Pen;
        draw(&mut e, &mut m, Gesture::Tool, [300.0, 250.0], [380.0, 270.0]);
        let undo = m.slide(1).unwrap().edit_stamp().0;
        let shift = crate::canvas::affine::Affine::translate([40.0, 0.0]);
        let placed = m.drop_objects(1, 1, 0, &[0], &[], &shift).expect("moved");
        assert_eq!(placed.strokes, [0], "still the same object");
        let b = m.slide(1).unwrap();
        assert_eq!(b.layers[0].strokes.len(), 1);
        assert!((b.layers[0].strokes[0].samples[0].pos[0] - 140.0).abs() < 2.0, "moved 40 units");
        assert_eq!(b.edit_stamp().0, undo + 1, "one undo step");
    }

    #[test]
    fn tools_that_need_the_overlay_pan_instead() {
        assert!(supported(ToolKind::Pen) && supported(ToolKind::Select) && supported(ToolKind::Eraser));
        assert!(!supported(ToolKind::Fill) && !supported(ToolKind::Text) && !supported(ToolKind::LassoErase));
    }
}
