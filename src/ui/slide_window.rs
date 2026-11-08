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
//! The toolbar's tool works here too, on the slide shown (see
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
use crate::canvas::render;
use crate::canvas::CanvasManager;
use crate::interaction::zoom_pan::MIN_ZOOM;
use crate::ui::overlay::OverlayCache;
use crate::ui::theme;
use egui::{Color32, Rounding, Vec2};

/// Space kept around a slide fitted to the window, in points.
const FIT_MARGIN: f32 = 24.0;
/// Most a fitted slide is blown up: a lone small sketch fills the window
/// without its lines turning into bars.
const FIT_MAX_ZOOM: f32 = 4.0;
/// Toolbar button metrics, so the card matches the main toolbar.
const BTN: f32 = 28.0;
const BTN_ROUND: f32 = 7.0;
/// Room the card takes at the top, kept clear when fitting the slide.
const CARD_INSET: f32 = 50.0;
/// Width of the band along each edge that resizes the window.
const EDGE: f32 = 6.0;
/// Frames to look for the window's handle after it opens.
const FIND_FRAMES: u32 = 120;

/// The open slide window: which slide it shows and how.
pub struct SlideWindow {
    slide: usize,
    /// Where the slide's `(0, 0)` sits from the canvas area's top-left,
    /// and its scale — like `Canvas::pan` / `zoom`, but this window's.
    pan: [f32; 2],
    zoom: f32,
    /// Fit the slide to the window on the next frame.
    fit_pending: bool,
    /// Stay above every other window, not only the overlay.
    on_top: bool,
    /// Show the global pins, and no slide, instead of the slide; the
    /// canvas then leaves them out (`CanvasManager::set_globals_away`).
    pins: bool,
    /// The window's native handle once found (0 before; Windows only).
    hwnd: isize,
    /// Frames spent looking for it.
    find_tries: u32,
    /// The window's own copy of the tool and its gestures in flight.
    edit: crate::ui::slide_edit::WindowEdit,
    /// The canvas area last frame, window points, and the window's points
    /// per pixel: where a drop from the canvas lands (`canvas_at`).
    last_rect: egui::Rect,
    last_ppp: f32,
}

/// How the main window looks this frame, for the slide window to match.
pub struct Look {
    /// The overlay's tint: its background colour at its opacity.
    pub background: Color32,
    /// The app's UI shows (Tab hides it, and captures do).
    pub chrome: bool,
    /// The overlay's native handle, to own the slide window (0 if none).
    pub owner: isize,
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
    /// A window on slide `slide`, fitted to it.
    pub fn new(slide: usize, cfg: &crate::config::AppConfig) -> Self {
        Self {
            slide,
            pan: [0.0; 2],
            zoom: 1.0,
            fit_pending: true,
            on_top: false,
            pins: false,
            hwnd: 0,
            find_tries: 0,
            edit: crate::ui::slide_edit::WindowEdit::new(cfg),
            last_rect: egui::Rect::NOTHING,
            last_ppp: 1.0,
        }
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

    /// Objects dropped here from the canvas: select them.
    pub fn select_dropped(&mut self, placed: crate::canvas::hit::Selection, manager: &CanvasManager) {
        if let Some(c) = manager.slide(self.slide) {
            self.edit.select_dropped(placed, self.slide, c);
        }
    }

    /// What the window has to tell the user, once.
    pub fn take_notice(&mut self) -> Option<String> {
        self.edit.take_notice()
    }

    /// Take the pen gestures that start over this window out of the
    /// canvas's `events`, to replay here (see `WindowEdit::claim_pen`).
    pub fn claim_pen(&mut self, events: &mut Vec<crate::input::pen::PenEvent>, client_origin: [i32; 2]) {
        self.edit.claim_pen(events, client_origin, self.hwnd);
    }

    /// A pin double-clicked here, for the app to go to its slide.
    pub fn take_open_pin(&mut self) -> Option<u64> {
        self.edit.take_open_pin()
    }

    /// A pen or mouse gesture is drawing or moving something here.
    pub fn busy(&self) -> bool {
        self.edit.busy()
    }

    pub fn viewport_id() -> egui::ViewportId {
        egui::ViewportId::from_hash_of("osn_slide_window")
    }

    /// The window's native handle, 0 until it is known.
    pub fn hwnd(&self) -> isize {
        self.hwnd
    }

    /// The window shows the global pins instead of a slide, so the canvas
    /// should not.
    pub fn shows_pins(&self) -> bool {
        self.pins
    }

    /// Show slide `idx` instead, fitted to the window.
    pub fn set_slide(&mut self, idx: usize) {
        if idx != self.slide {
            self.slide = idx;
            self.fit_pending = true;
        }
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

/// Draw the window for this frame. Must run every frame it stays open:
/// a frame without it closes the window. False once the user closed it.
#[allow(clippy::too_many_arguments)]
pub fn show(
    ctx: &egui::Context,
    win: &mut SlideWindow,
    cache: &mut OverlayCache,
    manager: &mut CanvasManager,
    position_passes: usize,
    media: &crate::media::MediaHub,
    look: &Look,
    main_tool: &mut crate::tools::ActiveTool,
    input: &crate::ui::slide_edit::Input,
) -> bool {
    let count = manager.count();
    if count == 0 {
        return false;
    }
    win.slide = win.slide.min(count - 1);
    manager.prefetch(win.slide);
    win.edit.follow(main_tool);

    // Not shown on the window; Alt+Tab and the handle search use it.
    let title = if win.pins { "Slide window · global pins".to_string() } else { format!("Slide {}", slide_label(manager, win.slide)) };
    let builder = egui::ViewportBuilder::default()
        .with_title(title)
        .with_decorations(false)
        .with_transparent(true)
        .with_resizable(true)
        .with_taskbar(false)
        // Size on first opening only; the user's resizes stick after.
        .with_inner_size([720.0, 480.0])
        .with_min_inner_size([520.0, 240.0])
        .with_window_level(if win.on_top { egui::WindowLevel::AlwaysOnTop } else { egui::WindowLevel::Normal });
    let open = ctx.show_viewport_immediate(SlideWindow::viewport_id(), builder, |ctx, class| {
        // A backend with one window would draw this over the canvas.
        // Desktop eframe always makes a real window.
        if class == egui::ViewportClass::Embedded {
            return false;
        }
        let mut open = !ctx.input(|i| i.viewport().close_requested());
        let top = if look.chrome { CARD_INSET } else { 0.0 };
        if win.pins {
            pins_area(ctx, win, cache, manager, position_passes, media, look, top, main_tool, input);
        } else {
            canvas_area(ctx, win, cache, manager, position_passes, media, look.background, top, main_tool, input);
        }
        if look.chrome {
            open &= card(ctx, win, manager);
        }
        open
    });

    // The window exists once the viewport has been shown: find its handle
    // and hand it to the overlay, which keeps it above itself.
    if win.hwnd == 0 && win.find_tries < FIND_FRAMES && look.owner != 0 {
        win.find_tries += 1;
        win.hwnd = crate::platform::own_window_titled("Slide ", look.owner);
        if win.hwnd != 0 {
            crate::platform::set_owner(win.hwnd, look.owner);
            // Rounded corners, like the overlay and any Windows 11 window.
            crate::platform::round_corners(win.hwnd);
        }
    }
    open
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

/// Which slide: a toolbar-style button with the slide's number and name,
/// opening a list of slides drawn like the slide panel's rows. The slide
/// picked, if one was.
fn slide_picker(ui: &mut egui::Ui, current: usize, manager: &CanvasManager) -> Option<usize> {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(PICKER_W, BTN), egui::Sense::click());
    let popup = ui.make_persistent_id("osn_slide_window_picker");
    let open = ui.memory(|m| m.is_popup_open(popup));
    let bg = if open {
        theme::ACCENT
    } else if response.hovered() {
        Color32::from_white_alpha(22)
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

/// One slide in the picker's list: number, then name; lit when shown.
pub(crate) fn slide_row(ui: &mut egui::Ui, manager: &CanvasManager, i: usize, current: bool) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(PICKER_W, ROW_H), egui::Sense::click());
    let bg = if current {
        theme::ACCENT
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

/// A small "v", drawn: the icon set has no down chevron.
fn chevron_down(ui: &mut egui::Ui, r: egui::Rect) {
    let stroke = egui::Stroke::new(1.5, theme::TEXT_MUTED);
    let pts = [egui::pos2(r.min.x, r.min.y + r.height() * 0.3), egui::pos2(r.center().x, r.min.y + r.height() * 0.75), egui::pos2(r.max.x, r.min.y + r.height() * 0.3)];
    ui.painter().line_segment([pts[0], pts[1]], stroke);
    ui.painter().line_segment([pts[1], pts[2]], stroke);
}

/// The floating card along the top, styled like the main toolbar: a grip
/// to move the window, which slide, how it is fitted, "On top", close.
/// False when its close button was clicked.
fn card(ctx: &egui::Context, win: &mut SlideWindow, manager: &CanvasManager) -> bool {
    let count = manager.count();
    let mut open = true;
    egui::Area::new(egui::Id::new("osn_slide_window_card"))
        .anchor(egui::Align2::CENTER_TOP, Vec2::new(0.0, 10.0))
        .show(ctx, |ui| {
            theme::card_frame().show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 3.0;
                    grip(ui);
                    if win.pins {
                        // No slide to step through while the pins show.
                        // As wide as the arrows and the picker, so the
                        // card keeps its size.
                        let (rect, _) = ui.allocate_exact_size(Vec2::new(2.0 * BTN + 6.0 + PICKER_W, BTN), egui::Sense::hover());
                        let text = egui::Pos2::new(rect.min.x + 8.0, rect.center().y);
                        ui.painter().text(text, egui::Align2::LEFT_CENTER, "Global pins", egui::FontId::proportional(14.0), theme::TEXT_PRIMARY);
                    } else {
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
                    let tip = if win.pins {
                        "Showing the pins on every slide here. Click to show the slide again and give the pins back to the main window"
                    } else {
                        "Show the pins on every slide here, without a slide. They leave the main window while this is on"
                    };
                    if text_btn(ui, "Pins", tip, win.pins) {
                        win.pins = !win.pins;
                        win.edit.drop_pin_selection();
                        win.fit_pending = true;
                    }
                    ui.add_space(8.0);
                    let fit_tip = if win.pins { "Fit the pins to the window — or double-click" } else { "Fit the slide to the window — or double-click it" };
                    if text_btn(ui, "Fit", fit_tip, false) {
                        win.fit_pending = true;
                    }
                    ui.label(egui::RichText::new(zoom_label(win.zoom)).color(theme::TEXT_MUTED));
                    ui.add_space(8.0);
                    if text_btn(ui, "On top", "Keep this window above every other window, not only the overlay", win.on_top) {
                        win.on_top = !win.on_top;
                    }
                    if close_btn(ui) {
                        open = false;
                    }
                });
            });
        });
    open
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
        ui.painter().rect_filled(rect, Rounding::same(BTN_ROUND), Color32::from_white_alpha(22));
    }
    let tint = if enabled { theme::TEXT_PRIMARY } else { Color32::from_white_alpha(60) };
    let icon = rect.shrink(rect.width() * 0.22);
    img.tint(tint).fit_to_exact_size(icon.size()).paint_at(ui, icon);
    response.on_hover_text(tooltip).clicked() && enabled
}

/// A toolbar-style button with a word on it, lit while `selected`.
fn text_btn(ui: &mut egui::Ui, text: &str, tooltip: &str, selected: bool) -> bool {
    let galley = egui::WidgetText::from(egui::RichText::new(text).color(theme::TEXT_PRIMARY))
        .into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Body);
    let (rect, response) = ui.allocate_exact_size(Vec2::new(galley.size().x + 16.0, BTN), egui::Sense::click());
    let bg = if selected {
        theme::ACCENT
    } else if response.hovered() {
        Color32::from_white_alpha(22)
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
        ui.painter().rect_filled(rect, Rounding::same(BTN_ROUND), Color32::from_white_alpha(22));
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

/// The slide itself on the overlay's tint: the toolbar's tool working on
/// it, wheel zoom, panning, and the resize bands along the edges. `top`
/// is the room the card takes, which a fitted slide keeps clear of.
#[allow(clippy::too_many_arguments)]
fn canvas_area(
    ctx: &egui::Context,
    win: &mut SlideWindow,
    cache: &mut OverlayCache,
    manager: &mut CanvasManager,
    position_passes: usize,
    media: &crate::media::MediaHub,
    background: Color32,
    top: f32,
    main_tool: &mut crate::tools::ActiveTool,
    input: &crate::ui::slide_edit::Input,
) {
    egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| {
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
        theme::paint_window(ui.painter(), ctx.screen_rect(), background, square_corners(ctx));
        if manager.slide(win.slide).is_none() {
            // Still being read; the main window keeps frames coming
            // while slides load.
            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "Opening slide…", egui::FontId::proportional(14.0), theme::TEXT_MUTED);
            resize_edges(ui, rect);
            return;
        }
        win.last_rect = rect;
        win.last_ppp = ctx.pixels_per_point();
        let editable = !manager.is_locked(win.slide);
        let view = crate::ui::slide_edit::View { slide: win.slide, rect, pan: win.pan, zoom: win.zoom };
        win.edit.keys(ctx, manager, view, editable, main_tool, input.keys);
        win.edit.pointer(ctx, &response, manager, view, &mut win.pan, editable, input, win.hwnd, main_tool);
        let drag_pans = win.edit.drag_pans(editable, input, ctx);
        if navigate(ctx, win, rect, &response, drag_pans) {
            if let Some(slide) = manager.slide(win.slide) {
                fit(win, slide, rect.size(), top);
            }
        }
        let view = crate::ui::slide_edit::View { pan: win.pan, zoom: win.zoom, ..view };
        let chrome = win.edit.chrome(manager, view);
        let live = win.edit.live_stroke();
        let Some(slide) = manager.slide(win.slide) else { return };

        let painter = ui.painter_at(rect);
        let (layer_textures, shape_textures) =
            cache.slide_textures(ctx, win.slide, slide, rect, win.pan, win.zoom, media, false);
        let laser: Vec<(crate::canvas::stroke::Stroke, std::time::Instant)> = win.edit.laser().to_vec();
        if !laser.is_empty() {
            // The trails fade on their own.
            ctx.request_repaint();
        }
        let preview_shape = win.edit.tool().preview_shape();
        let sel_preview = win.edit.sel_preview(slide, win.zoom);
        render::paint_canvas_egui(
            &painter,
            rect.min,
            slide,
            preview_shape.as_ref(),
            live.as_ref(),
            position_passes,
            &laser,
            &layer_textures,
            &shape_textures,
            Some((win.pan, win.zoom)),
            None,
            sel_preview,
        );
        let origin = rect.min + Vec2::new(win.pan[0], win.pan[1]);
        if let Some(chrome) = chrome {
            crate::ui::overlay::paint_select_chrome(&painter, origin, win.zoom, &chrome);
        }
        // Counts, not `content_bounds`: that walks every pen sample, too
        // slow to do each frame on a big slide.
        let empty = !slide.layers.iter().any(|l| l.visible && (!l.strokes.is_empty() || !l.shapes.is_empty() || l.image.is_some()));
        if empty {
            painter.text(rect.center(), egui::Align2::CENTER_CENTER, "This slide is empty", egui::FontId::proportional(14.0), theme::TEXT_MUTED);
        }
        // The pen's or eraser's reach at the pointer, as on the canvas.
        let tool = win.edit.tool();
        let ringed = matches!(tool.kind, crate::tools::ToolKind::Pen | crate::tools::ToolKind::Eraser | crate::tools::ToolKind::FreehandArrow);
        if editable && ringed && !drag_pans && live.is_none() {
            if let Some(p) = response.hover_pos() {
                let r = tool.cursor_radius(win.zoom);
                let c = tool.color;
                let colour = if tool.kind == crate::tools::ToolKind::Eraser {
                    Color32::from_rgb(220, 222, 230)
                } else {
                    Color32::from_rgba_unmultiplied(c[0], c[1], c[2], 220)
                };
                painter.circle_stroke(p, r, egui::Stroke::new(1.2, colour));
            }
        }
        // A selection dragged on the canvas is over this window: say
        // letting go drops it here.
        if input.drop_hover {
            let accent = Color32::from_rgb(86, 160, 255);
            painter.rect_stroke(rect.shrink(3.0), Rounding::same(10.0), egui::Stroke::new(3.0, accent));
            let text = if editable { format!("Drop to move to slide {}", win.slide + 1) } else { format!("Slide {} is locked", win.slide + 1) };
            let at = egui::pos2(rect.center().x, rect.max.y - 28.0);
            let galley = painter.layout_no_wrap(text, egui::FontId::proportional(14.0), theme::TEXT_PRIMARY);
            let pill = egui::Rect::from_center_size(at, galley.size() + Vec2::new(20.0, 10.0));
            painter.rect_filled(pill, Rounding::same(8.0), theme::GLASS_FILL);
            painter.galley(pill.center() - galley.size() * 0.5, galley, theme::TEXT_PRIMARY);
        }
        resize_edges(ui, rect);
    });
}

/// The global pins alone, laid out as they are on the canvas, under the
/// window's own pan and zoom (`fit` gathers them into view). With the
/// Select tool they move, resize and turn here as on the canvas.
#[allow(clippy::too_many_arguments)]
fn pins_area(
    ctx: &egui::Context,
    win: &mut SlideWindow,
    cache: &mut OverlayCache,
    manager: &mut CanvasManager,
    position_passes: usize,
    media: &crate::media::MediaHub,
    look: &Look,
    top: f32,
    main_tool: &mut crate::tools::ActiveTool,
    input: &crate::ui::slide_edit::Input,
) {
    egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| {
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
        theme::paint_window(ui.painter(), ctx.screen_rect(), look.background, square_corners(ctx));
        win.last_rect = rect;
        win.last_ppp = ctx.pixels_per_point();
        // With the Select tool the pins move, resize and turn here as on
        // the canvas (see `WindowEdit::pins_pointer`); any other press
        // pans, so a plain drag is not `navigate`'s.
        win.edit.pins_keys(ctx, manager, main_tool, input.keys);
        let origin = rect.min + Vec2::new(win.pan[0], win.pan[1]);
        win.edit.pins_pointer(ctx, &response, manager, origin, win.zoom, &mut win.pan, input, win.hwnd);
        if navigate(ctx, win, rect, &response, false) {
            let bounds = manager
                .global_pins()
                .iter()
                .map(|p| p.frame().aabb())
                .reduce(|a, b| [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]);
            if let Some(b) = bounds {
                // No bigger than on the canvas: they were sized there.
                fit_bounds(win, b, rect.size(), top, 1.0);
            }
        }

        let pins = manager.global_pins();
        if pins.is_empty() {
            let text = if manager.pins_hidden() { "Pins are hidden — Shift+P shows them" } else { "No pins on every slide yet — Ctrl+P pins a selection" };
            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(14.0), theme::TEXT_MUTED);
        } else {
            let origin = rect.min + Vec2::new(win.pan[0], win.pan[1]);
            let textures = cache.pin_textures(ctx, &pins, rect, origin, win.zoom, media, false);
            let painter = ui.painter_at(rect);
            for (p, textures) in pins.iter().zip(&textures) {
                // The pin's own scale times the window's zoom; its turn
                // and place (`rigid`) are the canvas's, so the layout
                // is kept.
                render::paint_pinned(&painter, origin, p.canvas, p.members, p.pin.rigid(p.anchor()), p.pin.scale * win.zoom, position_passes, textures);
            }
            // Outlines, badges and the selected pin's handles, as on the
            // canvas with the Select tool, drawn on the pins as the window
            // shows them so the handles keep their size on screen.
            if look.chrome && win.edit.tool().kind == crate::tools::ToolKind::Select {
                let o = [origin.x, origin.y];
                let shown: Vec<crate::canvas::pins::Pin> = pins.iter().map(|p| p.pin.viewed(o, win.zoom)).collect();
                let view: Vec<crate::canvas::pins::Pinned> = pins.iter().zip(&shown).map(|(p, s)| crate::canvas::pins::Pinned { pin: s, ..*p }).collect();
                let marks = crate::ui::overlay::PinMarks { selected: win.edit.selected_pin(), outlines: true };
                crate::ui::overlay::paint_pin_marks(ui, &painter, egui::Pos2::ZERO, &view, marks);
            }
        }
        resize_edges(ui, rect);
    });
}

/// Pan with a drag, zoom with the wheel about the pointer; true when the
/// view should be fitted (asked for, or a double-click).
/// Pan with a middle-button drag, or with a plain drag when `drag_pans`
/// (the tool does not draw here); zoom with the wheel about the
/// pointer. True when the view should be fitted: asked for, or a
/// double-click that is not the tool's.
fn navigate(ctx: &egui::Context, win: &mut SlideWindow, rect: egui::Rect, response: &egui::Response, drag_pans: bool) -> bool {
    let fit = std::mem::take(&mut win.fit_pending) || (drag_pans && response.double_clicked());
    let panning = response.dragged_by(egui::PointerButton::Middle) || (drag_pans && response.dragged_by(egui::PointerButton::Primary));
    if panning {
        let d = response.drag_delta();
        win.pan[0] += d.x;
        win.pan[1] += d.y;
    }
    if let Some(p) = response.hover_pos() {
        let steps = ctx.input(wheel_steps);
        if steps.abs() > 1e-4 {
            zoom_about(win, 1.10_f32.powf(steps), p - rect.min);
        }
        if panning {
            ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
        } else if drag_pans {
            ctx.set_cursor_icon(egui::CursorIcon::Grab);
        }
    }
    fit
}

/// Bands along the window's edges and corners that resize it, as a frame
/// would. Over the slide, so they win where they overlap it.
fn resize_edges(ui: &mut egui::Ui, rect: egui::Rect) {
    use egui::{CursorIcon as C, ResizeDirection as D};
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
        let response = ui.interact(zone, egui::Id::new(("osn_slide_window_edge", i)), egui::Sense::drag());
        if response.hovered() || response.dragged() {
            ui.ctx().set_cursor_icon(cursor);
        }
        if response.drag_started_by(egui::PointerButton::Primary) {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::BeginResize(dir));
        }
    }
}

/// Wheel input this frame as zoom steps of 1.10×, the canvas's own rule
/// (`interaction::zoom_pan`): one per wheel notch, and a trackpad pinch
/// in proportion. Ctrl + wheel arrives both as a wheel event and as
/// egui's zoom, so the zoom is only counted without Ctrl.
fn wheel_steps(i: &egui::InputState) -> f32 {
    let mut steps: f32 = i
        .events
        .iter()
        .filter_map(|e| match e {
            egui::Event::MouseWheel { delta, .. } => Some(delta.y),
            _ => None,
        })
        .sum();
    let pinch = i.zoom_delta();
    if !i.modifiers.ctrl && (pinch - 1.0).abs() > 1e-4 {
        steps += pinch.ln() / 1.10_f32.ln();
    }
    steps
}

/// Scale the view by `factor` about `at` (from the canvas area's
/// top-left), keeping the slide point under it in place.
fn zoom_about(win: &mut SlideWindow, factor: f32, at: Vec2) {
    use crate::interaction::zoom_pan::MAX_ZOOM;
    let z_old = win.zoom;
    let z_new = (z_old * factor).clamp(MIN_ZOOM, MAX_ZOOM);
    let cx = (at.x - win.pan[0]) / z_old;
    let cy = (at.y - win.pan[1]) / z_old;
    win.zoom = z_new;
    win.pan = [at.x - cx * z_new, at.y - cy * z_new];
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
    fn zooming_keeps_the_point_under_the_pointer() {
        let mut w = SlideWindow::new(0, &crate::config::AppConfig::default());
        w.pan = [30.0, -20.0];
        w.zoom = 0.5;
        let at = Vec2::new(200.0, 120.0);
        let before = [(at.x - w.pan[0]) / w.zoom, (at.y - w.pan[1]) / w.zoom];
        zoom_about(&mut w, 1.1_f32.powi(3), at);
        let after = [(at.x - w.pan[0]) / w.zoom, (at.y - w.pan[1]) / w.zoom];
        assert!((before[0] - after[0]).abs() < 1e-3 && (before[1] - after[1]).abs() < 1e-3);
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
        });
        fit(&mut w, &slide, Vec2::new(448.0, 448.0), 0.0);
        let to_screen = |p: [f32; 2]| [w.pan[0] + p[0] * w.zoom, w.pan[1] + p[1] * w.zoom];
        let [a, b] = [to_screen([100.0, 100.0]), to_screen([500.0, 300.0])];
        assert!((a[0] - FIT_MARGIN).abs() < 1.0 && (b[0] - (448.0 - FIT_MARGIN)).abs() < 1.0, "wide content spans the width");
        assert!(((a[1] + b[1]) * 0.5 - 224.0).abs() < 1.0, "and is centred down");

        // With the card showing, the slide sits below it.
        fit(&mut w, &slide, Vec2::new(448.0, 448.0), CARD_INSET);
        let a = [w.pan[0] + 100.0 * w.zoom, w.pan[1] + 100.0 * w.zoom];
        assert!(a[1] >= CARD_INSET + FIT_MARGIN - 1.0, "top edge clears the card");
    }

    #[test]
    fn the_tint_is_the_overlays_colour_at_its_opacity() {
        let c = Look::background([20, 40, 90], 0.8);
        assert_eq!(c.to_array(), [16, 32, 72, 204], "the overlay clears to these, premultiplied");
        assert_eq!(Look::background([20, 40, 60], 0.0), Color32::TRANSPARENT);
        assert_eq!(Look::background([20, 40, 60], 1.0), Color32::from_rgb(20, 40, 60));
    }
}
