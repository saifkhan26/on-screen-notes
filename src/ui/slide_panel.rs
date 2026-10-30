//! Slide panel — floating card in the bottom-left corner listing every
//! slide.
//!
//! One row per slide: its number and name. A click goes to the slide; a
//! double-click (or F2, for the active slide) names it. The header
//! switches what Ctrl+Tab steps through (slide order or recently used),
//! jumps to the last slide, adds a slide after it, and folds the panel
//! down to a small "n/N" tab.

use crate::canvas::CanvasManager;
use crate::config::{AppConfig, SlideTabOrder};
use crate::ui::theme;
use egui::{Color32, Rounding, Vec2};

const WIDTH: f32 = 180.0;
const ROW_H: f32 = 24.0;
const ICON: f32 = 18.0;
/// Width of the slide-number column at the start of each row.
const NUM_W: f32 = 26.0;
const NAME_LIMIT: usize = 60;

/// What the panel keeps between frames.
#[derive(Default)]
pub struct SlidePanel {
    /// The slide being named, and the text typed so far.
    renaming: Option<(usize, String)>,
    /// The name field has yet to take the keyboard (its first frame).
    focus_pending: bool,
    /// The active slide when the panel last drew, so a slide reached
    /// another way (a key) is scrolled into view once.
    shown_active: Option<usize>,
}

pub enum SlideAction {
    GoTo(usize),
    Rename(usize, String),
    /// A new slide after the last one.
    Add,
}

#[derive(Default)]
pub struct Shown {
    pub action: Option<SlideAction>,
    /// The pointer is over the panel: the wheel scrolls the list, not
    /// the canvas.
    pub hovered: bool,
    /// A name was being typed this frame: keys are text, not shortcuts.
    pub typing: bool,
}

impl SlidePanel {
    /// Open the name field on slide `idx`, holding `name`.
    pub fn start_rename(&mut self, idx: usize, name: &str) {
        self.renaming = Some((idx, name.to_string()));
        self.focus_pending = true;
    }

    pub fn is_renaming(&self) -> bool {
        self.renaming.is_some()
    }
}

/// Draw the panel, growing up from the bottom-left corner no higher than
/// `top` (nor the toolbar, when the screen is narrow enough for the two
/// to meet). `cfg` holds whether it is folded and what Ctrl+Tab steps
/// through.
pub fn show(ctx: &egui::Context, state: &mut SlidePanel, cfg: &mut AppConfig, top: f32, manager: &CanvasManager) -> Shown {
    let count = manager.count();
    let active = manager.active_index();
    if state.renaming.as_ref().is_some_and(|(i, _)| *i >= count) {
        state.renaming = None;
    }
    let typing = state.renaming.is_some();
    let mut action = None;
    let open = &mut cfg.slide_panel_open;
    let tab_order = &mut cfg.slide_tab_order;

    // The toolbar along the top, as it was drawn last frame.
    let toolbar = ctx.memory(|m| m.area_rect(egui::Id::new("osn_toolbar")));
    let (left, bottom) = (10.0, 10.0);
    let top = match toolbar {
        Some(t) if t.min.x < left + WIDTH + 20.0 => top.max(t.max.y + 8.0),
        _ => top,
    };
    // Room for the list: up to `top`, less the header.
    let room = ctx.screen_rect().max.y - bottom - top - 40.0;

    let area = egui::Area::new(egui::Id::new("osn_slide_panel"))
        .anchor(egui::Align2::LEFT_BOTTOM, Vec2::new(left, -bottom))
        .show(ctx, |ui| {
            egui::Frame {
                inner_margin: egui::Margin::symmetric(4.0, 4.0),
                outer_margin: egui::Margin::ZERO,
                rounding: Rounding::same(10.0),
                shadow: theme::card_shadow(),
                fill: theme::GLASS_FILL,
                stroke: egui::Stroke::NONE,
            }
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(0.0, 2.0);
                if !*open {
                    if folded_tab(ui, active, count).clicked() {
                        *open = true;
                    }
                    return;
                }
                ui.set_width(WIDTH);
                ui.horizontal(|ui| {
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new(format!("Slides · {count}")).small().color(theme::TEXT_MUTED));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        let fold = egui::Image::new(egui::include_image!("../../assets/icons/chevron-left.svg"));
                        if icon_btn(ui, fold, "Fold the slide panel", true, false) {
                            *open = false;
                        }
                        let add = egui::Image::new(egui::include_image!("../../assets/icons/plus.svg"));
                        if icon_btn(ui, add, "New slide after the last one", true, false) {
                            action = Some(SlideAction::Add);
                        }
                        let last = egui::Image::new(egui::include_image!("../../assets/icons/arrow-down-to-line.svg"));
                        if icon_btn(ui, last, "Last slide — End", active + 1 < count, false) {
                            action = Some(SlideAction::GoTo(count - 1));
                        }
                        let recent = *tab_order == SlideTabOrder::RecentlyUsed;
                        let tip = if recent {
                            "Ctrl+Tab goes to recently used slides: a tap flips back to the last slide, \
                             hold Ctrl and tap again to go further back. Click for slide order"
                        } else {
                            "Ctrl+Tab goes to the next slide, Ctrl+Shift+Tab the previous one. \
                             Click to go to recently used slides instead (like Alt+Tab)"
                        };
                        let history = egui::Image::new(egui::include_image!("../../assets/icons/history.svg"));
                        if icon_btn(ui, history, tip, true, recent) {
                            *tab_order = if recent { SlideTabOrder::Sequential } else { SlideTabOrder::RecentlyUsed };
                        }
                    });
                });
                // The list's height comes from its rows, not the space
                // left: an Area only offers last frame's size, so a list
                // sized by that could never grow (more slides, unfolding).
                let rows_h = count as f32 * (ROW_H + 2.0) - 2.0;
                let list_h = rows_h.min(room.max(ROW_H * 3.0));
                egui::ScrollArea::vertical()
                    .max_height(list_h)
                    .min_scrolled_height(list_h)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for i in 0..count {
                            if let Some(a) = slide_row(ui, state, manager, i, active) {
                                action = Some(a);
                            }
                        }
                    });
            });
        });
    // Folded, nothing was shown: unfolding scrolls to the active slide.
    state.shown_active = (*open).then_some(active);

    let hovered = ctx.input(|i| i.pointer.hover_pos()).is_some_and(|p| area.response.rect.contains(p));
    Shown { action, hovered, typing }
}

/// One row: the slide's number, then its name — or the name field while
/// it is being named.
fn slide_row(ui: &mut egui::Ui, state: &mut SlidePanel, manager: &CanvasManager, i: usize, active: usize) -> Option<SlideAction> {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(WIDTH, ROW_H), egui::Sense::click());
    let is_active = i == active;
    let bg = if is_active {
        theme::ACCENT
    } else if response.hovered() {
        Color32::from_white_alpha(14)
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, Rounding::same(6.0), bg);
    ui.painter().text(
        egui::pos2(rect.min.x + NUM_W - 6.0, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        (i + 1).to_string(),
        egui::FontId::proportional(12.0),
        theme::TEXT_MUTED,
    );
    let mut name_rect = egui::Rect::from_min_max(egui::pos2(rect.min.x + NUM_W, rect.min.y), egui::pos2(rect.max.x - 4.0, rect.max.y));
    let locked = manager.is_locked(i);
    if locked {
        let r = egui::Rect::from_center_size(egui::pos2(rect.max.x - 4.0 - ICON * 0.5, rect.center().y), Vec2::splat(ICON)).shrink(3.0);
        paint_svg(ui, r, egui::Image::new(egui::include_image!("../../assets/icons/lock.svg")), theme::TEXT_MUTED);
        name_rect.max.x -= ICON + 2.0;
    }

    if is_active && state.shown_active != Some(active) {
        response.scroll_to_me(None);
    }

    let mut action = None;
    if let Some((idx, buf)) = state.renaming.as_mut().filter(|(idx, _)| *idx == i) {
        // A light outlined box over the row instead of egui's dark field,
        // which would hide the row's highlight.
        let field = name_rect.shrink2(Vec2::new(0.0, 2.0));
        ui.painter().rect(field, Rounding::same(5.0), Color32::from_white_alpha(20), egui::Stroke::new(1.0, Color32::from_white_alpha(70)));
        let edit = egui::TextEdit::singleline(buf)
            .char_limit(NAME_LIMIT)
            .hint_text("Slide name")
            .frame(false)
            .margin(egui::Margin::symmetric(4.0, 0.0))
            .vertical_align(egui::Align::Center);
        let r = ui.put(field, edit);
        if std::mem::take(&mut state.focus_pending) {
            r.request_focus();
            r.scroll_to_me(None);
            // Start with the whole name selected, so typing replaces it.
            if let Some(mut st) = egui::text_edit::TextEditState::load(ui.ctx(), r.id) {
                let end = egui::text::CCursor::new(buf.chars().count());
                st.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), end)));
                st.store(ui.ctx(), r.id);
            }
        } else if r.lost_focus() {
            // Enter or a click elsewhere keeps the name; Esc drops it.
            if !ui.input(|inp| inp.key_pressed(egui::Key::Escape)) {
                action = Some(SlideAction::Rename(*idx, buf.clone()));
            }
            state.renaming = None;
        }
        return action;
    }

    let name = manager.name(i);
    let (text, colour) = if name.is_empty() {
        ("Untitled", theme::TEXT_MUTED)
    } else {
        (name, if is_active { theme::TEXT_PRIMARY } else { Color32::from_rgb(215, 215, 215) })
    };
    let galley = egui::WidgetText::from(egui::RichText::new(text).color(colour)).into_galley(ui, Some(egui::TextWrapMode::Truncate), name_rect.width() - 4.0, egui::TextStyle::Body);
    let pos = egui::pos2(name_rect.min.x + 4.0, name_rect.center().y - galley.size().y * 0.5);
    ui.painter().galley(pos, galley, colour);

    let mut tip = if name.is_empty() { "Double-click to name".to_string() } else { format!("{name}\nDouble-click to rename") };
    if locked {
        tip.push_str("\nLocked: pans and zooms, takes no edits");
    }
    let response = response.on_hover_text(tip);
    if response.double_clicked() {
        state.start_rename(i, name);
    } else if response.clicked() && !is_active {
        action = Some(SlideAction::GoTo(i));
    }
    action
}

/// The folded panel: "n/N" and an unfold arrow.
fn folded_tab(ui: &mut egui::Ui, active: usize, count: usize) -> egui::Response {
    let label = egui::WidgetText::from(egui::RichText::new(format!("{}/{}", active + 1, count)).small())
        .into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Small);
    let w = 6.0 + ICON + 4.0 + label.size().x + 2.0 + ICON + 2.0;
    let (rect, response) = ui.allocate_exact_size(Vec2::new(w, ROW_H), egui::Sense::click());
    let bg = if response.hovered() { Color32::from_white_alpha(18) } else { Color32::TRANSPARENT };
    ui.painter().rect_filled(rect, Rounding::same(6.0), bg);
    let mut x = rect.min.x + 6.0;
    let icon_rect = |x: f32| egui::Rect::from_min_size(egui::pos2(x, rect.center().y - ICON * 0.5), Vec2::splat(ICON)).shrink(2.0);
    paint_svg(ui, icon_rect(x), egui::Image::new(egui::include_image!("../../assets/icons/frame.svg")), theme::TEXT_PRIMARY);
    x += ICON + 4.0;
    let label_w = label.size().x;
    ui.painter().galley(egui::pos2(x, rect.center().y - label.size().y * 0.5), label, theme::TEXT_PRIMARY);
    x += label_w + 2.0;
    paint_svg(ui, icon_rect(x), egui::Image::new(egui::include_image!("../../assets/icons/chevron-right.svg")), theme::TEXT_MUTED);
    response.on_hover_text("Show the slide panel")
}

/// A bare icon button, lit while `selected`; true when clicked while
/// enabled.
fn icon_btn(ui: &mut egui::Ui, img: egui::Image<'_>, tooltip: &str, enabled: bool, selected: bool) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(ICON + 4.0), egui::Sense::click());
    if selected {
        ui.painter().rect_filled(rect, Rounding::same(5.0), theme::ACCENT);
    } else if enabled && response.hovered() {
        ui.painter().rect_filled(rect, Rounding::same(5.0), Color32::from_white_alpha(18));
    }
    let tint = if enabled { theme::TEXT_PRIMARY } else { Color32::from_white_alpha(60) };
    paint_svg(ui, rect.shrink(4.0), img, tint);
    response.on_hover_text(tooltip).clicked() && enabled
}

fn paint_svg(ui: &mut egui::Ui, rect: egui::Rect, img: egui::Image<'_>, tint: Color32) {
    img.tint(tint).fit_to_exact_size(rect.size()).paint_at(ui, rect);
}
