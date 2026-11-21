//! Slide window — a second window showing one slide, to look at while
//! working on another: a reference board, the slide before.
//!
//! It looks like the main window: no title bar, see-through, tinted with
//! the overlay's background colour at the overlay's opacity (both follow
//! the overlay live), with its controls on a floating glass card like the
//! toolbar. Tab hides the card with the rest of the app's UI. Moving and
//! resizing are the card's grip and the window's edges, since there is no
//! frame to drag.
//!
//! The window opens locked, for looking only: a drag pans and nothing
//! can be changed here by accident. The lock on its card unlocks it.
//! Unlocked, the toolbar's tool works here too, on the slide shown (see
//! `slide_edit`): draw, erase (Ctrl), shapes, laser, select. Wheel zooms;
//! middle-drag or Space+drag pans, and a plain drag pans on a locked
//! slide or with a tool the window cannot use yet. In Pins mode the
//! Select tool moves, resizes and turns the pins (Delete unpins,
//! double-click goes to a pin's slide) and any other drag pans. The
//! window has its own pan and zoom, so looking around here never moves
//! that slide's view on the canvas.
//!
//! It is an egui *immediate* viewport: drawn inside the main window's
//! frame, which lets it read the slides and the texture cache as they
//! are, with no copy or lock. The price is that it redraws whenever the
//! main window does. Pictures are shared with the overlay, one texture
//! for both windows; only `OverlayCache::begin_frame` evicts from that
//! cache (see `OverlayCache::slide_textures`).
//!
//! On Windows the overlay owns the window, so it always sits above the
//! overlay without being always-on-top. A borderless, see-through,
//! always-on-top window can end up never taking focus (see `main.rs`), so
//! "On top" is the user's choice, off at first.

use crate::canvas::canvas::Canvas;
use crate::canvas::CanvasManager;
use crate::interaction::zoom_pan::MIN_ZOOM;
use crate::ui::theme;
use egui::{Color32, Rounding, Vec2};

/// Space kept around a slide fitted to the window, in points.
const FIT_MARGIN: f32 = 24.0;
/// Most a fitted slide is blown up: a lone small sketch fills the window
/// without its lines turning into bars.
const FIT_MAX_ZOOM: f32 = 4.0;
/// Toolbar button metrics, so the card matches the main toolbar.
const BTN: f32 = 30.0;
const BTN_ROUND: f32 = theme::CONTROL_ROUNDING;
/// The pins window's title: not shown, Alt+Tab and the handle search use
/// it. Not starting "Slide ", which the slide window's search looks for.
const PINS_TITLE: &str = "Pins window";
/// Width of the band along each edge that resizes the window.
const EDGE: f32 = 6.0;
/// Frames to look for the window's handle after it opens.
const FIND_FRAMES: u32 = 120;
/// The window's size when it first opens on a slide, and the least it
/// can be made, in points.
const START_SIZE: Vec2 = Vec2::new(1040.0, 680.0);
pub const MIN_SIZE: Vec2 = Vec2::new(580.0, 240.0);

/// The open slide window: which slide it shows and how.
pub struct SlideWindow {
    slide: usize,
    /// Where the slide's `(0, 0)` sits from the canvas area's top-left,
    /// and its scale — like `Canvas::pan` / `zoom`, but this window's.
    pan: [f32; 2],
    zoom: f32,
    /// Fit the slide to the window on the next frame: its region if it
    /// has one, else all of it.
    fit_pending: bool,
    /// The next fit shows all of the slide even though it has a region.
    fit_whole: bool,
    /// The part of the slide the window was opened on (a box dragged on
    /// the canvas, or a selection), in slide units: what Fit goes back
    /// to. Only for this slide; showing another one drops it.
    region: Option<[f32; 4]>,
    /// Where the window opened on screen and its size, in points, when it
    /// popped out over a region; `None` for the usual size, wherever the
    /// system puts it. Asked for on opening only: moves and resizes stick.
    place: Option<egui::Rect>,
    /// Stay above every other window, not only the overlay.
    on_top: bool,
    /// Looking only: no edits here until the user unlocks it. Every
    /// window opens locked; showing another slide or region keeps it as
    /// it is.
    locked: bool,
    /// The user was told once that a drag pans because it is locked.
    lock_told: bool,
    /// Something to tell the user, for the app's toast.
    notice: Option<String>,
    /// The pins window: it shows the pins and the pins board, never a
    /// slide.
    pins: bool,
    /// The window's native handle once found (0 before; Windows only).
    hwnd: isize,
    /// Frames spent looking for it.
    find_tries: u32,
    /// The window's own copy of the tool and its gestures in flight.
    pen: crate::ui::slide_edit::WindowPen,
    /// The canvas area last frame, window points, and the window's points
    /// per pixel: where a drop from the canvas lands (`canvas_at`).
    last_rect: egui::Rect,
    last_ppp: f32,
    /// Where the window's canvas was drawn last frame, window points: the
    /// overlay cache's canvas rect while the window's frame runs.
    canvas_rect: Option<egui::Rect>,
}

/// How the main window looks this frame, for the slide window to match.
pub struct Look {
    /// The overlay's tint: its background colour at its opacity.
    pub background: Color32,
}

impl Look {
    /// The overlay's background as the window clears to it
    /// (`OnScreenNotesApp::clear_color`): each channel times the opacity,
    /// as stored. Not `from_rgba_unmultiplied`, which multiplies in
    /// linear light and comes out brighter than the overlay.
    pub fn background(color: [u8; 3], opacity: f32) -> Color32 {
        let a = opacity.clamp(0.0, 1.0);
        let ch = |c: u8| (f32::from(c) * a).round() as u8;
        Color32::from_rgba_premultiplied(ch(color[0]), ch(color[1]), ch(color[2]), (a * 255.0).round() as u8)
    }
}

impl SlideWindow {
    /// Scale the window's view by `f`, as `CanvasManager::scale_views`
    /// does the slides': the UI size changed by `1 / f`.
    pub fn scale_view(&mut self, f: f32) {
        use crate::interaction::zoom_pan::{MAX_ZOOM, MIN_ZOOM};
        self.pan = [self.pan[0] * f, self.pan[1] * f];
        self.zoom = (self.zoom * f).clamp(MIN_ZOOM, MAX_ZOOM);
    }

    /// A window on slide `slide`, fitted to it.
    pub fn new(slide: usize, _cfg: &crate::config::AppConfig) -> Self {
        Self {
            slide,
            pan: [0.0; 2],
            zoom: 1.0,
            fit_pending: true,
            fit_whole: false,
            region: None,
            place: None,
            on_top: false,
            locked: true,
            lock_told: false,
            notice: None,
            pins: false,
            hwnd: 0,
            find_tries: 0,
            pen: Default::default(),
            last_rect: egui::Rect::NOTHING,
            last_ppp: 1.0,
            canvas_rect: None,
        }
    }

    /// The pins window: the pins and the pins board, fitted to them.
    pub fn pins_window(cfg: &crate::config::AppConfig) -> Self {
        Self { pins: true, ..Self::new(0, cfg) }
    }

    /// The viewport this window is drawn in.
    pub fn viewport(&self) -> egui::ViewportId {
        if self.pins {
            Self::pins_viewport_id()
        } else {
            Self::viewport_id()
        }
    }

    pub fn pins_viewport_id() -> egui::ViewportId {
        egui::ViewportId::from_hash_of("osn_pins_window")
    }

    /// A window popped out over `region` of slide `slide` (slide units):
    /// it opens at `place` (screen points, see `pop_out_rect`) seen through
    /// `pan` / `zoom`, which keep the region where it was on the canvas.
    pub fn popped_out(slide: usize, region: [f32; 4], place: egui::Rect, pan: [f32; 2], zoom: f32, cfg: &crate::config::AppConfig) -> Self {
        Self { pan, zoom, fit_pending: false, region: Some(region), place: Some(place), ..Self::new(slide, cfg) }
    }

    /// Show `region` of slide `slide` (slide units), fitted to the window
    /// where it is now; the global pins give way to the slide.
    pub fn aim(&mut self, slide: usize, region: [f32; 4]) {
        if self.pins {
            self.pins = false;
        }
        self.slide = slide;
        self.region = Some(region);
        self.fit_pending = true;
        self.fit_whole = false;
    }

    /// The slide shown, and its point under screen point `screen`
    /// (physical pixels), with the screen pixels per slide unit there:
    /// where something dropped from the canvas lands. `None` in Pins mode
    /// or before the window is placed.
    pub fn canvas_at(&self, screen: [f32; 2]) -> Option<(usize, [f32; 2], f32)> {
        if self.pins || !self.last_rect.is_positive() {
            return None;
        }
        let (l, t, _, _) = crate::platform::client_rect(self.hwnd)?;
        let local = [(screen[0] - l as f32) / self.last_ppp, (screen[1] - t as f32) / self.last_ppp];
        let p = [
            (local[0] - self.last_rect.min.x - self.pan[0]) / self.zoom,
            (local[1] - self.last_rect.min.y - self.pan[1]) / self.zoom,
        ];
        Some((self.slide, p, self.zoom * self.last_ppp))
    }

    /// What the window has to tell the user, once.
    pub fn take_notice(&mut self) -> Option<String> {
        self.notice.take()
    }

    /// Looking only: nothing on its slide, or its pins, can change here.
    pub fn locked(&self) -> bool {
        self.locked
    }

    /// Take the pen gestures that start over this window out of the
    /// canvas's `events`, to replay here (see `WindowEdit::claim_pen`).
    pub fn claim_pen(&mut self, events: &mut Vec<crate::input::pen::PenEvent>, client_origin: [i32; 2]) {
        self.pen.claim(events, client_origin, self.hwnd);
    }

    /// A pen or mouse gesture is drawing or moving something here.
    pub fn busy(&self) -> bool {
        self.pen.busy()
    }

    pub fn viewport_id() -> egui::ViewportId {
        egui::ViewportId::from_hash_of("osn_slide_window")
    }

    /// The window's native handle, 0 until it is known.
    pub fn hwnd(&self) -> isize {
        self.hwnd
    }

    /// The pins window.
    pub fn shows_pins(&self) -> bool {
        self.pins
    }

    /// Show slide `idx` instead, fitted to the window. The region was the
    /// other slide's, so it goes.
    pub fn set_slide(&mut self, idx: usize) {
        if idx != self.slide {
            self.slide = idx;
            self.region = None;
            self.fit_pending = true;
        }
    }

    /// Slides were moved: keep showing the same slide, `map` giving its
    /// new number.
    pub fn renumber(&mut self, map: impl Fn(usize) -> usize) {
        self.slide = map(self.slide);
    }

    /// Slide `idx` was deleted: keep showing the same slide under its new
    /// number. False when it was the one shown, so the window should go.
    pub fn slide_removed(&mut self, idx: usize) -> bool {
        match self.slide.cmp(&idx) {
            std::cmp::Ordering::Less => true,
            std::cmp::Ordering::Equal => false,
            std::cmp::Ordering::Greater => {
                self.slide -= 1;
                true
            }
        }
    }
}

impl SlideWindow {
    /// The window's viewport: no frame, see-through, owned by the overlay
    /// (see `find_handle`). The same every frame, so the user's moves
    /// and resizes stick; size and place count on opening only.
    pub fn builder(&self, manager: &CanvasManager) -> egui::ViewportBuilder {
        // Not shown on the window; Alt+Tab and the handle search use it.
        let title = if self.pins { PINS_TITLE.to_string() } else { format!("Slide {}", slide_label(manager, self.slide)) };
        let mut builder = egui::ViewportBuilder::default()
            .with_title(title)
            .with_decorations(false)
            .with_transparent(true)
            .with_resizable(true)
            .with_taskbar(false)
            .with_inner_size(self.place.map_or(START_SIZE, |r| r.size()))
            .with_min_inner_size(MIN_SIZE)
            .with_window_level(if self.on_top { egui::WindowLevel::AlwaysOnTop } else { egui::WindowLevel::Normal });
        if let Some(r) = self.place {
            builder = builder.with_position(r.min);
        }
        builder
    }

    /// Keep showing a slide that exists, and have it read soon.
    pub fn keep_in_range(&mut self, manager: &mut CanvasManager) {
        self.slide = self.slide.min(manager.count().saturating_sub(1));
        manager.prefetch(self.slide);
    }

    /// The slide shown and the window's view of it.
    pub fn view(&self) -> (usize, [f32; 2], f32) {
        (self.slide, self.pan, self.zoom)
    }

    /// For the video players: the slide shown, the view, and where the
    /// window's canvas is (window points). The Pins view gives `canvas::BOARD` for the pins board.
    pub fn media_view(&self) -> Option<(usize, [f32; 2], f32, egui::Rect)> {
        let slide = if self.pins { crate::canvas::BOARD } else { self.slide };
        self.canvas_rect.map(|r| (slide, self.pan, self.zoom, r))
    }

    /// Before the window's frame runs on its slide: fit it if that was
    /// asked for — its region, else all of it — below `top`, the room the
    /// toolbar takes. Waits for the slide to be read.
    pub fn before_pass(&mut self, ctx: &egui::Context, manager: &CanvasManager, top: f32) {
        if self.pins {
            // The pins window: its pins and the board's ink, no bigger
            // than on the canvas, where the pins were sized.
            if std::mem::take(&mut self.fit_pending) {
                let size = self.canvas_rect.map_or(ctx.screen_rect().size(), |r| r.size());
                let bounds = manager
                    .window_pins()
                    .iter()
                    .map(|p| p.frame().aabb())
                    .chain(content_bounds(manager.board()))
                    .reduce(|a, b| [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]);
                match bounds {
                    Some(b) => fit_bounds(self, b, size, top, 1.0),
                    None => {
                        self.pan = [0.0, top];
                        self.zoom = 1.0;
                    }
                }
            }
            return;
        }
        if !self.fit_pending || manager.slide(self.slide).is_none() {
            return;
        }
        self.fit_pending = false;
        let size = self.canvas_rect.map_or(ctx.screen_rect().size(), |r| r.size());
        let whole = std::mem::take(&mut self.fit_whole);
        match self.region {
            // A small region blows up as far as the canvas goes: the
            // user picked it.
            Some(r) if !whole => fit_bounds(self, r, size, top, crate::interaction::zoom_pan::MAX_ZOOM),
            _ => {
                if let Some(slide) = manager.slide(self.slide) {
                    fit(self, slide, size, top);
                }
            }
        }
    }

    /// Where the window's canvas is, for the overlay cache to use while
    /// its frame runs (`OverlayCache::enter_child`).
    pub fn canvas_rect_mut(&mut self) -> &mut Option<egui::Rect> {
        &mut self.canvas_rect
    }

    /// After the window's frame: the slide it ended on (its slide panel
    /// or keys may have gone to another, whose region this is not) and
    /// its view there; `ppp` for drops from the canvas (`canvas_at`).
    pub fn after_pass(&mut self, slide: usize, pan: [f32; 2], zoom: f32, ppp: f32) {
        if slide != crate::canvas::BOARD && slide != self.slide {
            self.slide = slide;
            self.region = None;
        }
        self.pan = pan;
        self.zoom = zoom;
        self.last_rect = self.canvas_rect.unwrap_or(egui::Rect::NOTHING);
        self.last_ppp = ppp;
    }

    /// This frame's pen events over the window (see `claim_pen`), in its
    /// client points, and its client area's top-left on screen.
    pub fn take_pen_events(&mut self, ppp: f32) -> (Vec<crate::input::pen::PenEvent>, [i32; 2]) {
        self.pen.take(self.hwnd, ppp)
    }

    /// Locked: the first press with a tool that would draw says how to
    /// unlock, once.
    pub fn lock_hint(&mut self, ctx: &egui::Context, kind: crate::tools::ToolKind) {
        use crate::tools::ToolKind as K;
        let draws = !matches!(kind, K::Select | K::Laser | K::Spotlight);
        if self.locked && !self.lock_told && draws && ctx.input(|i| i.pointer.primary_pressed()) {
            self.lock_told = true;
            self.notice = Some("The slide window is locked, so a drag pans. Its lock button unlocks it for editing".to_string());
        }
    }

    /// The window exists once its viewport was shown: find its handle and
    /// hand it to the overlay (`owner`), which keeps it above itself.
    pub fn find_handle(&mut self, owner: isize) {
        if self.hwnd == 0 && self.find_tries < FIND_FRAMES && owner != 0 {
            self.find_tries += 1;
            self.hwnd = crate::platform::own_window_titled(if self.pins { PINS_TITLE } else { "Slide " }, owner);
            if self.hwnd != 0 {
                crate::platform::set_owner(self.hwnd, owner);
                // Rounded corners, like the overlay and any Windows 11 window.
                crate::platform::round_corners(self.hwnd);
            }
        }
    }
}

/// The overlay's tint over the whole window, under everything drawn in
/// it, with Windows 11's rounded corners.
pub fn paint_background(ctx: &egui::Context, tint: Color32) {
    theme::paint_window(&ctx.layer_painter(egui::LayerId::background()), ctx.screen_rect(), tint, square_corners(ctx));
}

/// The slide is still being read.
pub fn opening(ctx: &egui::Context) {
    egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| {
        let rect = ui.max_rect();
        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "Opening slide…", egui::FontId::proportional(14.0), theme::TEXT_MUTED);
    });
}

/// The window's own controls over what its frame drew: the edges that
/// resize it and, while its UI shows, the card. False when the card's
/// close was clicked.
///
/// The card shows the tool in use and the palette (`palette`, as the
/// toolbar has it; right-click edits a slot): the window has no toolbar.
/// Returns whether it stays open and the colour clicked, if one was.
pub fn chrome(
    ctx: &egui::Context,
    win: &mut SlideWindow,
    manager: &CanvasManager,
    card_shown: bool,
    tool: &crate::tools::ActiveTool,
    palette: &mut [[u8; 4]],
    parts: &mut crate::config::SlideWindowUi,
) -> (bool, Option<[u8; 4]>) {
    resize_edges(ctx);
    if !card_shown || (parts.bar_autohide && !bar_wanted(ctx)) {
        return (true, None);
    }
    card(ctx, win, manager, tool, palette, parts)
}

/// An auto-hiding bar shows: the pointer is near the window's top (where
/// the bar was), or one of its menus is open.
fn bar_wanted(ctx: &egui::Context) -> bool {
    let reach = bar_bottom(ctx).unwrap_or(56.0) + 24.0;
    ctx.input(|i| i.pointer.hover_pos()).is_some_and(|p| p.y <= reach) || ctx.memory(|m| m.any_popup_open())
}

/// The bar's area id: the toolbar's, so the arrange bar, find and the
/// command palette sit under it as they sit under the toolbar.
pub const BAR_ID: &str = "osn_toolbar";

/// How far down the slide window's bar reaches, last frame; room to keep
/// clear when fitting.
pub fn bar_bottom(ctx: &egui::Context) -> Option<f32> {
    ctx.memory(|m| m.area_rect(egui::Id::new(BAR_ID))).map(|r| r.bottom())
}

/// "3 · Name", or "3" for a slide without a name.
fn slide_label(manager: &CanvasManager, idx: usize) -> String {
    match manager.name(idx) {
        "" => format!("{}", idx + 1),
        name => format!("{} · {name}", idx + 1),
    }
}

/// Width of the slide picker's button and list.
pub(crate) const PICKER_W: f32 = 170.0;
const ROW_H: f32 = 24.0;
/// Width of the slide-number column, as in the slide panel.
const NUM_W: f32 = 26.0;

/// One slide in the picker's list: number, then name; lit when shown.
pub(crate) fn slide_row(ui: &mut egui::Ui, manager: &CanvasManager, i: usize, current: bool) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(PICKER_W, ROW_H), egui::Sense::click());
    let bg = if current {
        theme::on_fill()
    } else if response.hovered() {
        Color32::from_white_alpha(14)
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, Rounding::same(6.0), bg);
    if current {
        response.scroll_to_me(None);
    }
    slide_text(ui, rect, manager, i, if current { theme::TEXT_PRIMARY } else { Color32::from_rgb(215, 215, 215) });
    response.clicked()
}

/// A slide's number (muted, right-aligned in its column) and name, in
/// `rect`, the name cut short to fit.
fn slide_text(ui: &mut egui::Ui, rect: egui::Rect, manager: &CanvasManager, i: usize, colour: Color32) {
    ui.painter().text(
        egui::pos2(rect.min.x + NUM_W - 6.0, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        (i + 1).to_string(),
        egui::FontId::proportional(12.0),
        theme::TEXT_MUTED,
    );
    let name = manager.name(i);
    let (text, colour) = if name.is_empty() { ("Untitled", theme::TEXT_MUTED) } else { (name, colour) };
    let width = rect.width() - NUM_W - 4.0;
    let galley = egui::WidgetText::from(egui::RichText::new(text).color(colour))
        .into_galley(ui, Some(egui::TextWrapMode::Truncate), width, egui::TextStyle::Body);
    let pos = egui::pos2(rect.min.x + NUM_W + 4.0, rect.center().y - galley.size().y * 0.5);
    ui.painter().galley(pos, galley, colour);
}

/// Which slide: a toolbar-style button with the slide's number and name,
/// opening a list of slides drawn like the slide panel's rows. The slide
/// picked, if one was.
fn slide_picker(ui: &mut egui::Ui, current: usize, manager: &CanvasManager) -> Option<usize> {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(PICKER_W, BTN), egui::Sense::click());
    let popup = ui.make_persistent_id("osn_slide_window_picker");
    let open = ui.memory(|m| m.is_popup_open(popup));
    let bg = if open {
        theme::on_fill()
    } else if response.hovered() {
        theme::HOVER
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, Rounding::same(BTN_ROUND), bg);
    let text_rect = egui::Rect::from_min_max(egui::pos2(rect.min.x + 8.0, rect.min.y), egui::pos2(rect.max.x - 22.0, rect.max.y));
    slide_text(ui, text_rect, manager, current, theme::TEXT_PRIMARY);
    chevron_down(ui, egui::Rect::from_center_size(egui::pos2(rect.max.x - 12.0, rect.center().y), Vec2::splat(8.0)));
    let response = response.on_hover_text("Choose the slide to show");
    if response.clicked() {
        ui.memory_mut(|m| m.toggle_popup(popup));
    }

    let mut picked = None;
    let room = (ui.ctx().screen_rect().max.y - rect.max.y - 24.0).max(ROW_H * 3.0);
    egui::popup_below_widget(ui, popup, &response, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
        ui.spacing_mut().item_spacing = Vec2::new(0.0, 2.0);
        egui::ScrollArea::vertical().max_height(room.min(360.0)).show(ui, |ui| {
            for i in 0..manager.count() {
                if slide_row(ui, manager, i, i == current) {
                    picked = Some(i);
                }
            }
        });
    });
    if picked.is_some() {
        ui.memory_mut(|m| m.close_popup());
    }
    picked
}

/// A small "v", drawn: the icon set has no down chevron.
fn chevron_down(ui: &mut egui::Ui, r: egui::Rect) {
    let stroke = egui::Stroke::new(1.5, theme::TEXT_MUTED);
    let pts = [egui::pos2(r.min.x, r.min.y + r.height() * 0.3), egui::pos2(r.center().x, r.min.y + r.height() * 0.75), egui::pos2(r.max.x, r.min.y + r.height() * 0.3)];
    ui.painter().line_segment([pts[0], pts[1]], stroke);
    ui.painter().line_segment([pts[1], pts[2]], stroke);
}

/// The window's bar along the top, styled like the main toolbar: a grip
/// to move the window, the tool in use and the colours, which slide, the
/// lock, Pins, how it is fitted, "On top", close. Whether it stays open
/// (false once its close was clicked), and a colour clicked.
fn card(
    ctx: &egui::Context,
    win: &mut SlideWindow,
    manager: &CanvasManager,
    tool: &crate::tools::ActiveTool,
    palette: &mut [[u8; 4]],
    parts: &mut crate::config::SlideWindowUi,
) -> (bool, Option<[u8; 4]>) {
    let mut open = true;
    let mut picked = None;
    egui::Area::new(egui::Id::new(BAR_ID))
        .anchor(egui::Align2::CENTER_TOP, Vec2::new(0.0, 10.0))
        .show(ctx, |ui| {
            theme::card_frame().show(ui, |ui| {
                // One row when there is room; a narrow window wraps it.
                ui.set_max_width((ctx.screen_rect().width() - 40.0).max(200.0));
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 3.0;
                    grip(ui);
                    // The tool in use and the colours, as the toolbar
                    // shows them.
                    crate::ui::toolbar::active_tool(ui, tool);
                    ui.add_space(4.0);
                    let mut colour = tool.color;
                    for (i, slot) in palette.iter_mut().enumerate() {
                        let (edited, _) = crate::ui::toolbar::colour_swatch(ui, &mut colour, *slot, i, None);
                        *slot = edited;
                    }
                    if colour != tool.color {
                        picked = Some(colour);
                    }
                    ui.add_space(8.0);
                    if win.pins {
                        // No slide to step through while the pins show.
                        // As wide as the arrows and the picker, so the
                        // card keeps its size.
                        let (rect, _) = ui.allocate_exact_size(Vec2::new(2.0 * BTN + 6.0 + PICKER_W, BTN), egui::Sense::hover());
                        let text = egui::Pos2::new(rect.min.x + 8.0, rect.center().y);
                        ui.painter().text(text, egui::Align2::LEFT_CENTER, "Pins", egui::FontId::proportional(14.0), theme::TEXT_PRIMARY);
                    } else {
                        // The window has no slide panel: this picks.
                        let count = manager.count();
                        let prev = egui::Image::new(egui::include_image!("../../assets/icons/chevron-left.svg"));
                        if icon_btn(ui, prev, "Previous slide", win.slide > 0) {
                            win.set_slide(win.slide - 1);
                        }
                        let next = egui::Image::new(egui::include_image!("../../assets/icons/chevron-right.svg"));
                        if icon_btn(ui, next, "Next slide", win.slide + 1 < count) {
                            win.set_slide(win.slide + 1);
                        }
                        if let Some(i) = slide_picker(ui, win.slide, manager) {
                            win.set_slide(i);
                        }
                    }
                    let lock = egui::Image::new(egui::include_image!("../../assets/icons/lock.svg"));
                    let tip = if win.locked {
                        "Locked — this window is for looking: a drag pans, nothing changes here. Click to edit in it"
                    } else {
                        "Unlocked — the toolbar's tool works here. Click to lock it again"
                    };
                    if toggle_btn(ui, lock, win.locked, tip) {
                        win.locked = !win.locked;
                    }
                    ui.add_space(8.0);
                    let fit_tip = if win.pins {
                        "Fit the pins and the board's ink to the window"
                    } else if win.region.is_some() {
                        "Back to the region this window was opened on"
                    } else {
                        "Fit the slide to the window"
                    };
                    if text_btn(ui, "Fit", fit_tip, false) {
                        win.fit_pending = true;
                    }
                    if win.region.is_some() && !win.pins && text_btn(ui, "All", "Fit the whole slide. Fit goes back to the region", false) {
                        win.fit_pending = true;
                        win.fit_whole = true;
                    }
                    ui.label(egui::RichText::new(zoom_label(win.zoom)).color(theme::TEXT_MUTED));
                    ui.add_space(8.0);
                    if text_btn(ui, "On top", "Keep this window above every other window, not only the overlay", win.on_top) {
                        win.on_top = !win.on_top;
                    }
                    view_menu(ui, parts);
                    if close_btn(ui) {
                        open = false;
                    }
                });
            });
        });
    (open, picked)
}

/// The View menu: which parts of its UI the window shows. Kept in the
/// config, so a window opened later looks the same.
fn view_menu(ui: &mut egui::Ui, parts: &mut crate::config::SlideWindowUi) {
    let popup = ui.make_persistent_id("osn_slide_window_view");
    let open = ui.memory(|m| m.is_popup_open(popup));
    let eye = egui::Image::new(egui::include_image!("../../assets/icons/eye.svg"));
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(BTN), egui::Sense::click());
    let bg = if open {
        theme::on_fill()
    } else if response.hovered() {
        theme::HOVER
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, Rounding::same(BTN_ROUND), bg);
    let icon = rect.shrink(rect.width() * 0.22);
    eye.tint(theme::TEXT_PRIMARY).fit_to_exact_size(icon.size()).paint_at(ui, icon);
    let response = response.on_hover_text("What this window shows — Tab hides all of it");
    if response.clicked() {
        ui.memory_mut(|m| m.toggle_popup(popup));
    }
    egui::popup_below_widget(ui, popup, &response, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
        ui.set_min_width(250.0);
        ui.label(egui::RichText::new("Show in this window").small().color(theme::TEXT_MUTED));
        let mut layers = !parts.hide_layers;
        if ui.checkbox(&mut layers, "Layer panel").changed() {
            parts.hide_layers = !layers;
        }
        let mut media = !parts.hide_media_bars;
        if ui.checkbox(&mut media, "Video and GIF controls").changed() {
            parts.hide_media_bars = !media;
        }
        ui.checkbox(&mut parts.bar_autohide, "Hide this bar until the pointer nears the top");
    });
}

/// Six dots at the card's start; dragging them moves the window.
fn grip(ui: &mut egui::Ui) {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(14.0, BTN), egui::Sense::drag());
    let tint = if response.hovered() || response.dragged() { theme::TEXT_PRIMARY } else { theme::TEXT_MUTED };
    for col in 0..2 {
        for row in 0..3 {
            let c = egui::pos2(rect.center().x + (col as f32 - 0.5) * 5.0, rect.center().y + (row as f32 - 1.0) * 5.0);
            ui.painter().circle_filled(c, 1.3, tint);
        }
    }
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Move);
    }
    if response.drag_started_by(egui::PointerButton::Primary) {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    response.on_hover_text("Drag to move the window");
}

/// A toolbar-style icon button; true when clicked while enabled.
fn icon_btn(ui: &mut egui::Ui, img: egui::Image<'_>, tooltip: &str, enabled: bool) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(BTN), egui::Sense::click());
    if enabled && response.hovered() {
        ui.painter().rect_filled(rect, Rounding::same(BTN_ROUND), theme::HOVER);
    }
    let tint = if enabled { theme::TEXT_PRIMARY } else { Color32::from_white_alpha(60) };
    let icon = rect.shrink(rect.width() * 0.22);
    img.tint(tint).fit_to_exact_size(icon.size()).paint_at(ui, icon);
    response.on_hover_text(tooltip).clicked() && enabled
}

/// A toolbar-style icon button lit while `on`; true when clicked.
fn toggle_btn(ui: &mut egui::Ui, img: egui::Image<'_>, on: bool, tooltip: &str) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(BTN), egui::Sense::click());
    let bg = if on {
        theme::on_fill()
    } else if response.hovered() {
        theme::HOVER
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, Rounding::same(BTN_ROUND), bg);
    let tint = if on { theme::TEXT_PRIMARY } else { theme::TEXT_MUTED };
    let icon = rect.shrink(rect.width() * 0.22);
    img.tint(tint).fit_to_exact_size(icon.size()).paint_at(ui, icon);
    response.on_hover_text(tooltip).clicked()
}

/// A toolbar-style button with a word on it, lit while `selected`.
fn text_btn(ui: &mut egui::Ui, text: &str, tooltip: &str, selected: bool) -> bool {
    let galley = egui::WidgetText::from(egui::RichText::new(text).color(theme::TEXT_PRIMARY))
        .into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Body);
    let (rect, response) = ui.allocate_exact_size(Vec2::new(galley.size().x + 16.0, BTN), egui::Sense::click());
    let bg = if selected {
        theme::on_fill()
    } else if response.hovered() {
        theme::HOVER
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, Rounding::same(BTN_ROUND), bg);
    let pos = rect.center() - galley.size() * 0.5;
    ui.painter().galley(pos, galley, theme::TEXT_PRIMARY);
    response.on_hover_text(tooltip).clicked()
}

/// Close: a drawn ×, the icon set has none.
fn close_btn(ui: &mut egui::Ui) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(BTN), egui::Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(rect, Rounding::same(BTN_ROUND), theme::HOVER);
    }
    let r = rect.shrink(rect.width() * 0.34);
    let stroke = egui::Stroke::new(1.6, theme::TEXT_PRIMARY);
    ui.painter().line_segment([r.left_top(), r.right_bottom()], stroke);
    ui.painter().line_segment([r.right_top(), r.left_bottom()], stroke);
    response.on_hover_text("Close this window").clicked()
}

/// The window is maximized or full screen, where Windows draws windows
/// with square corners.
fn square_corners(ctx: &egui::Context) -> bool {
    ctx.input(|i| i.viewport().maximized == Some(true) || i.viewport().fullscreen == Some(true))
}

fn zoom_label(zoom: f32) -> String {
    let pct = zoom * 100.0;
    if pct < 10.0 { format!("{pct:.1}%") } else { format!("{pct:.0}%") }
}

/// Bands along the window's edges and corners that resize it, as a frame
/// would: each a small area of its own over everything else, so presses
/// anywhere else still reach the canvas.
fn resize_edges(ctx: &egui::Context) {
    use egui::{CursorIcon as C, ResizeDirection as D};
    let rect = ctx.screen_rect();
    let (l, r, t, b) = (rect.min.x, rect.max.x, rect.min.y, rect.max.y);
    // Corners take twice the band, so they are easy to catch.
    let c = EDGE * 2.0;
    let band = |x0: f32, y0: f32, x1: f32, y1: f32| egui::Rect::from_min_max(egui::pos2(x0, y0), egui::pos2(x1, y1));
    let zones = [
        (band(l, t, l + c, t + c), D::NorthWest, C::ResizeNorthWest),
        (band(r - c, t, r, t + c), D::NorthEast, C::ResizeNorthEast),
        (band(l, b - c, l + c, b), D::SouthWest, C::ResizeSouthWest),
        (band(r - c, b - c, r, b), D::SouthEast, C::ResizeSouthEast),
        (band(l + c, t, r - c, t + EDGE), D::North, C::ResizeNorth),
        (band(l + c, b - EDGE, r - c, b), D::South, C::ResizeSouth),
        (band(l, t + c, l + EDGE, b - c), D::West, C::ResizeWest),
        (band(r - EDGE, t + c, r, b - c), D::East, C::ResizeEast),
    ];
    for (i, (zone, dir, cursor)) in zones.into_iter().enumerate() {
        egui::Area::new(egui::Id::new(("osn_slide_window_edge", i)))
            .order(egui::Order::Foreground)
            .fixed_pos(zone.min)
            .constrain(false)
            .show(ctx, |ui| {
                let (_, response) = ui.allocate_exact_size(zone.size(), egui::Sense::drag());
                if response.hovered() || response.dragged() {
                    ui.ctx().set_cursor_icon(cursor);
                }
                if response.drag_started_by(egui::PointerButton::Primary) {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::BeginResize(dir));
                }
            });
    }
}

/// Where a window popped out over `region` (points, like `screen`)
/// goes: over it, at least `min` in size and grown about its middle if
/// smaller, no bigger than `screen`, and moved inside it.
pub fn pop_out_rect(region: egui::Rect, min: Vec2, screen: egui::Rect) -> egui::Rect {
    let r = egui::Rect::from_center_size(region.center(), region.size().max(min).min(screen.size()));
    let shift = |lo: f32, hi: f32, a: f32, b: f32| {
        if a < lo || b - a >= hi - lo {
            lo - a
        } else if b > hi {
            hi - b
        } else {
            0.0
        }
    };
    r.translate(Vec2::new(shift(screen.min.x, screen.max.x, r.min.x, r.max.x), shift(screen.min.y, screen.max.y, r.min.y, r.max.y)))
}

/// Everything the slide's visible layers paint, in slide coordinates.
fn content_bounds(slide: &Canvas) -> Option<[f32; 4]> {
    slide
        .layers
        .iter()
        .filter(|l| l.visible)
        .filter_map(|l| l.content_bounds())
        .reduce(|a, b| [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])])
}

/// Fit the slide's content to a canvas area of `size`, centred in what
/// is left below `top`. An empty slide gets the view it has on the canvas.
fn fit(win: &mut SlideWindow, slide: &Canvas, size: Vec2, top: f32) {
    match content_bounds(slide) {
        Some(b) => fit_bounds(win, b, size, top, FIT_MAX_ZOOM),
        None => {
            win.pan = slide.pan;
            win.zoom = slide.zoom;
        }
    }
}

/// Fit `[x0, y0, x1, y1]` to a canvas area of `size`, centred in what is
/// left below `top`, at most `max_zoom`.
fn fit_bounds(win: &mut SlideWindow, [x0, y0, x1, y1]: [f32; 4], size: Vec2, top: f32, max_zoom: f32) {
    let (w, h) = ((x1 - x0).max(1.0), (y1 - y0).max(1.0));
    let room = (size - Vec2::new(2.0 * FIT_MARGIN, 2.0 * FIT_MARGIN + top)).max(Vec2::splat(1.0));
    let zoom = (room.x / w).min(room.y / h).clamp(MIN_ZOOM, max_zoom);
    win.zoom = zoom;
    let centre_y = top + (size.y - top) * 0.5;
    win.pan = [size.x * 0.5 - (x0 + x1) * 0.5 * zoom, centre_y - (y0 + y1) * 0.5 * zoom];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deleting_a_slide_keeps_the_window_on_the_same_one() {
        let mut w = SlideWindow::new(3, &crate::config::AppConfig::default());
        assert!(w.slide_removed(5), "a later slide: same number");
        assert_eq!(w.slide, 3);
        assert!(w.slide_removed(1), "an earlier slide: one down");
        assert_eq!(w.slide, 2);
        assert!(!w.slide_removed(2), "the slide shown: the window goes");
    }

    #[test]
    fn fitting_centres_the_content_inside_the_margin() {
        let mut w = SlideWindow::new(0, &crate::config::AppConfig::default());
        let mut slide = Canvas::default();
        slide.layers[0].shapes.push(crate::canvas::shape::Shape::Rect {
            a: [100.0, 100.0],
            b: [500.0, 300.0],
            color: [255; 4],
            stroke_width: 0.0,
            border: Default::default(),
            rotation: 0.0,
            pin: 0,
            group: 0,
            tags: Vec::new(),
            turn: 0.0,
        });
        fit(&mut w, &slide, Vec2::new(448.0, 448.0), 0.0);
        let to_screen = |p: [f32; 2]| [w.pan[0] + p[0] * w.zoom, w.pan[1] + p[1] * w.zoom];
        let [a, b] = [to_screen([100.0, 100.0]), to_screen([500.0, 300.0])];
        assert!((a[0] - FIT_MARGIN).abs() < 1.0 && (b[0] - (448.0 - FIT_MARGIN)).abs() < 1.0, "wide content spans the width");
        assert!(((a[1] + b[1]) * 0.5 - 224.0).abs() < 1.0, "and is centred down");

        // With the toolbar showing, the slide sits below it.
        fit(&mut w, &slide, Vec2::new(448.0, 448.0), 56.0);
        let a = [w.pan[0] + 100.0 * w.zoom, w.pan[1] + 100.0 * w.zoom];
        assert!(a[1] >= 56.0 + FIT_MARGIN - 1.0, "top edge clears the toolbar");
    }

    #[test]
    fn a_pop_out_sits_over_its_region_and_stays_on_screen() {
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(1920.0, 1080.0));
        let big = egui::Rect::from_min_size(egui::pos2(300.0, 200.0), Vec2::new(800.0, 500.0));
        assert_eq!(pop_out_rect(big, MIN_SIZE, screen), big, "a region past the least size: exactly over it");

        let small = egui::Rect::from_center_size(egui::pos2(900.0, 500.0), Vec2::new(100.0, 60.0));
        let r = pop_out_rect(small, MIN_SIZE, screen);
        assert_eq!(r.size(), MIN_SIZE);
        assert_eq!(r.center(), small.center(), "grown about its middle");

        let corner = egui::Rect::from_min_size(egui::pos2(1880.0, 1050.0), Vec2::new(30.0, 20.0));
        let r = pop_out_rect(corner, MIN_SIZE, screen);
        assert!(screen.contains_rect(r), "pushed back on screen: {r:?}");
        assert!(r.contains_rect(corner), "still over the region");

        let huge = egui::Rect::from_min_size(egui::pos2(-50.0, -20.0), Vec2::new(2400.0, 1400.0));
        assert_eq!(pop_out_rect(huge, MIN_SIZE, screen), screen, "too big: the whole screen");
        let wide = egui::Rect::from_min_size(egui::pos2(0.0, 200.0), Vec2::new(1600.0, 340.0));
        let r = pop_out_rect(wide, MIN_SIZE, screen);
        assert!(screen.contains_rect(r) && r.width() == 1600.0 && r.min.y == 200.0, "wide: kept in place, on screen: {r:?}");
        let wider = egui::Rect::from_min_size(egui::pos2(400.0, 200.0), Vec2::new(2000.0, 340.0));
        assert_eq!(pop_out_rect(wider, MIN_SIZE, screen).x_range(), screen.x_range(), "wider than the screen: as wide as it");
    }

    #[test]
    fn fit_goes_back_to_the_region_until_another_slide_shows() {
        let mut w = SlideWindow::new(2, &crate::config::AppConfig::default());
        w.aim(2, [100.0, 100.0, 300.0, 200.0]);
        assert!(w.fit_pending && w.region.is_some());
        w.set_slide(2);
        assert!(w.region.is_some(), "same slide: kept");
        w.set_slide(3);
        assert!(w.region.is_none(), "another slide: the region was not its");

        w.pins = true;
        w.aim(0, [0.0, 0.0, 10.0, 10.0]);
        assert!(!w.pins && w.slide == 0, "a region shows its slide, not the pins");
    }

    #[test]
    fn every_window_opens_locked_and_keeps_its_lock_when_aimed() {
        let cfg = crate::config::AppConfig::default();
        let mut w = SlideWindow::new(0, &cfg);
        assert!(w.locked());
        let p = SlideWindow::popped_out(0, [0.0; 4], egui::Rect::from_min_size(egui::Pos2::ZERO, MIN_SIZE), [0.0; 2], 1.0, &cfg);
        assert!(p.locked(), "a pop-out too");
        w.locked = false;
        w.aim(1, [0.0, 0.0, 50.0, 50.0]);
        w.set_slide(2);
        assert!(!w.locked(), "aiming the open window keeps the user's unlock");
    }

    #[test]
    fn the_tint_is_the_overlays_colour_at_its_opacity() {
        let c = Look::background([20, 40, 90], 0.8);
        assert_eq!(c.to_array(), [16, 32, 72, 204], "the overlay clears to these, premultiplied");
        assert_eq!(Look::background([20, 40, 60], 0.0), Color32::TRANSPARENT);
        assert_eq!(Look::background([20, 40, 60], 1.0), Color32::from_rgb(20, 40, 60));
    }
}
