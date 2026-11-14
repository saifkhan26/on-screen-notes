//! Slide panel — floating card in the bottom-left corner listing every
//! slide.
//!
//! One row per slide: its number and name. A click goes to the slide; a
//! double-click (or F2, for the active slide) names it. Dragging a row
//! moves the slide (a line shows where it lands), and so do the row's
//! right-click menu and the move-slide keys. The header
//! switches what Ctrl+Tab steps through (slide order or recently used),
//! jumps to the last slide, adds a slide after it, and folds the panel
//! down to a small "n/N" tab.
//!
//! Its tag button opens a list of every tag in the app (a modal in the
//! middle of the screen; also from the command palette), with how many slides and
//! objects carry each. Picking tags shows only the slides with any of
//! them (going to one lights those tags up there); each tag can be
//! renamed or removed everywhere from the list.

use crate::canvas::shape::parse_tags;
use crate::canvas::units::same_tag;
use crate::canvas::{CanvasManager, TagCount};
use crate::ui::media_filter::TAG_COLOUR;
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
    /// The slide whose row is being dragged to a new place.
    dragging: Option<usize>,
    /// Only slides with any of these tags show (none: every slide).
    tag_filter: Vec<String>,
    /// The tag being renamed in the tag list, and the name typed so far.
    tag_renaming: Option<(String, String)>,
    /// The rename field has yet to take the keyboard.
    tag_focus: bool,
    /// The tag whose removal waits for a yes.
    tag_removing: Option<String>,
    /// The list of every tag is open.
    tags_open: bool,
}

/// A tag changed everywhere from the tag list, for the app to do.
pub enum TagAction {
    /// Put these tags in place of this one.
    Rename(String, Vec<String>),
    Remove(String),
}

pub enum SlideAction {
    GoTo(usize),
    Rename(usize, String),
    /// A new slide after the last one.
    Add,
    /// Show the slide in the slide window (`ui::slide_window`).
    OpenWindow(usize),
    /// Move slide `.0` to place `.1`, the slides between shifting along.
    Move(usize, usize),
}

#[derive(Default)]
pub struct Shown {
    pub action: Option<SlideAction>,
    /// The pointer is over the panel: the wheel scrolls the list, not
    /// the canvas.
    pub hovered: bool,
    /// A name was being typed this frame: keys are text, not shortcuts.
    pub typing: bool,
    /// A tag renamed or removed in the tag list.
    pub tag_action: Option<TagAction>,
    /// A slide was picked from the tag-filtered list: light these tags
    /// up on it.
    pub highlight_tags: Option<Vec<String>>,
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

    /// Open the list of every tag.
    pub fn open_tags(&mut self) {
        self.tags_open = true;
    }

    /// The list of every tag is open: it takes the keys and the pointer.
    pub fn tags_open(&self) -> bool {
        self.tags_open
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
    let typing = state.renaming.is_some() || state.tag_renaming.is_some();
    let mut tag_action = None;
    let mut highlight_tags = None;
    if state.dragging.is_some_and(|i| i >= count) {
        state.dragging = None;
    }
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
                // Slides a tag filter leaves showing.
                let filtering = !state.tag_filter.is_empty();
                let visible: Vec<usize> = (0..count).filter(|&i| !filtering || manager.slide_has_any_tag(i, &state.tag_filter)).collect();
                ui.horizontal(|ui| {
                    ui.add_space(6.0);
                    let heading = if filtering { format!("Slides · {} of {count}", visible.len()) } else { format!("Slides · {count}") };
                    ui.label(egui::RichText::new(heading).small().color(theme::TEXT_MUTED));
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
                        let tags = egui::Image::new(egui::include_image!("../../assets/icons/tag.svg"));
                        let tip = "Every tag: show only the slides with some, rename or remove them everywhere";
                        if icon_btn(ui, tags, tip, true, filtering || state.tags_open) {
                            state.tags_open = !state.tags_open;
                        }
                    });
                });
                if filtering {
                    tag_chips(ui, state);
                }
                // The list's height comes from its rows, not the space
                // left: an Area only offers last frame's size, so a list
                // sized by that could never grow (more slides, unfolding).
                let rows_h = (visible.len().max(1)) as f32 * (ROW_H + 2.0) - 2.0;
                let list_h = rows_h.min(room.max(ROW_H * 3.0));
                egui::ScrollArea::vertical()
                    .max_height(list_h)
                    .min_scrolled_height(list_h)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        if visible.is_empty() {
                            ui.label(egui::RichText::new("  No slide has these tags").small().color(theme::TEXT_MUTED));
                        }
                        let mut rows = Vec::with_capacity(visible.len());
                        for &i in &visible {
                            let (a, rect, clicked) = slide_row(ui, state, manager, i, active, filtering);
                            if let Some(a) = a {
                                action = Some(a);
                            }
                            // Picked from the filtered list: its tags light up there.
                            if clicked && filtering {
                                highlight_tags = Some(state.tag_filter.clone());
                            }
                            rows.push(rect);
                        }
                        // Moving needs every slide in the list.
                        if !filtering {
                            if let Some(m) = drag_rows(ui, state, &rows) {
                                action = Some(m);
                            }
                        }
                    });
            });
        });
    if state.tags_open {
        tag_action = tag_modal(ctx, state, manager);
    } else {
        // Closed: nothing left half typed or half asked.
        state.tag_renaming = None;
        state.tag_removing = None;
    }
    // Folded, nothing was shown: unfolding scrolls to the active slide.
    state.shown_active = (*open).then_some(active);

    // Over the panel, or anywhere while the tag list is up: the wheel is
    // not the canvas's.
    let hovered = state.tags_open || ctx.input(|i| i.pointer.hover_pos()).is_some_and(|p| area.response.rect.contains(p));
    Shown { action, hovered, typing, tag_action, highlight_tags }
}

/// A row being dragged: the line where it would land, the list scrolling
/// when the pointer nears its top or bottom, and on letting go the move
/// (none when it lands where it was). Esc puts it back.
fn drag_rows(ui: &mut egui::Ui, state: &mut SlidePanel, rows: &[egui::Rect]) -> Option<SlideAction> {
    let from = state.dragging?;
    if rows.is_empty() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        state.dragging = None;
        return None;
    }
    let pointer = ui.ctx().pointer_latest_pos()?;
    ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    // The gap the pointer is nearest: before row `gap`.
    let gap = rows.iter().filter(|r| r.center().y < pointer.y).count();
    let lands = if gap > from { gap - 1 } else { gap };
    if lands != from {
        let y = match gap {
            0 => rows[0].top(),
            g if g == rows.len() => rows[g - 1].bottom(),
            g => (rows[g - 1].bottom() + rows[g].top()) * 0.5,
        };
        let x = rows[0].x_range();
        ui.painter().line_segment([egui::pos2(x.min + 4.0, y), egui::pos2(x.max - 4.0, y)], egui::Stroke::new(2.0, theme::ACCENT));
        ui.painter().circle_filled(egui::pos2(x.min + 4.0, y), 3.0, theme::ACCENT);
    }
    let clip = ui.clip_rect();
    let step = if pointer.y < clip.top() + 18.0 { 6.0 } else if pointer.y > clip.bottom() - 18.0 { -6.0 } else { 0.0 };
    if step != 0.0 {
        ui.scroll_with_delta(Vec2::new(0.0, step));
        ui.ctx().request_repaint();
    }
    if ui.input(|i| !i.pointer.any_down()) {
        state.dragging = None;
        return (lands != from).then_some(SlideAction::Move(from, lands));
    }
    None
}

/// One row: the slide's number, then its name — or the name field while
/// it is being named. Also returns where the row is, for dragging, and
/// whether it was clicked. While `filtering`, rows do not drag.
fn slide_row(
    ui: &mut egui::Ui,
    state: &mut SlidePanel,
    manager: &CanvasManager,
    i: usize,
    active: usize,
    filtering: bool,
) -> (Option<SlideAction>, egui::Rect, bool) {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(WIDTH, ROW_H), egui::Sense::click_and_drag());
    if response.drag_started() && state.renaming.is_none() && !filtering {
        state.dragging = Some(i);
    }
    let is_active = i == active;
    let bg = if state.dragging == Some(i) {
        Color32::from_white_alpha(34)
    } else if is_active {
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
        return (action, rect, false);
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
    tip.push_str(if filtering { "\nRight-click to open in a window or move it" } else { "\nDrag to move it\nRight-click to open in a window or move it" });
    if locked {
        tip.push_str("\nLocked: pans and zooms, takes no edits");
    }
    let last = manager.count().saturating_sub(1);
    response.context_menu(|ui| {
        if ui.button("Open in a window").clicked() {
            action = Some(SlideAction::OpenWindow(i));
            ui.close_menu();
        }
        ui.separator();
        for (label, to, enabled) in [
            ("Move to the top", 0, i > 0),
            ("Move up", i.saturating_sub(1), i > 0),
            ("Move down", (i + 1).min(last), i < last),
            ("Move to the bottom", last, i < last),
        ] {
            if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
                action = Some(SlideAction::Move(i, to));
                ui.close_menu();
            }
        }
    });
    let response = response.on_hover_text(tip);
    if response.double_clicked() {
        state.start_rename(i, name);
    } else if response.clicked() && !is_active {
        action = Some(SlideAction::GoTo(i));
    }
    (action, rect, response.clicked())
}

/// The tags the list is filtered by, under the header: a click on one
/// stops filtering by it.
fn tag_chips(ui: &mut egui::Ui, state: &mut SlidePanel) {
    let mut drop = None;
    ui.horizontal_wrapped(|ui| {
        ui.add_space(6.0);
        ui.spacing_mut().item_spacing.x = 6.0;
        for (k, t) in state.tag_filter.iter().enumerate() {
            let chip = egui::Label::new(egui::RichText::new(format!("#{t} ×")).small().color(TAG_COLOUR)).sense(egui::Sense::click());
            if ui.add(chip).on_hover_text("Stop filtering by this tag").clicked() {
                drop = Some(k);
            }
        }
    });
    if let Some(k) = drop {
        state.tag_filter.remove(k);
    }
}

/// The list of every tag as a modal in the middle of the screen, over a
/// dimmed slide: Close, Esc (when not leaving a rename) or a click on the
/// dimmed part closes it.
fn tag_modal(ctx: &egui::Context, state: &mut SlidePanel, manager: &CanvasManager) -> Option<TagAction> {
    const W: f32 = 400.0;
    if state.tag_renaming.is_none() && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
        state.tags_open = false;
        return None;
    }
    let screen = ctx.screen_rect();
    let dimmed = egui::Area::new(egui::Id::new("osn_tag_modal_dim"))
        .order(egui::Order::Foreground)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            let r = ui.allocate_rect(screen, egui::Sense::click());
            ui.painter().rect_filled(screen, Rounding::ZERO, Color32::from_black_alpha(140));
            r.clicked()
        })
        .inner;
    let (index, unread) = manager.tag_index();
    let card = egui::Id::new("osn_tag_modal");
    let mut close = dimmed;
    let out = egui::Area::new(card)
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .show(ctx, |ui| {
            theme::card_frame()
                .inner_margin(egui::Margin::same(14.0))
                .show(ui, |ui| {
                    ui.set_width(W);
                    ui.horizontal(|ui| {
                        let title = egui::RichText::new(format!("Every tag · {}", index.len())).size(15.0).color(theme::TEXT_PRIMARY);
                        ui.label(title);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Close").on_hover_text("Esc").clicked() {
                                close = true;
                            }
                        });
                    });
                    let help = "Pick tags to see only the slides with any of them; going to one lights them up there. \
                                The pen renames a tag everywhere, the bin removes it.";
                    ui.label(egui::RichText::new(help).small().color(theme::TEXT_MUTED));
                    ui.add_space(6.0);
                    tag_list(ui, state, &index, unread, (screen.height() * 0.6).min(480.0))
                })
                .inner
        })
        .inner;
    // The card above its dimmed backdrop.
    let card_layer = egui::LayerId::new(egui::Order::Foreground, card);
    ctx.move_to_top(card_layer);
    // The keys stay in here: a widget behind the backdrop that took the
    // keyboard (Tab moves it before any of this runs) lets go at once,
    // so Space or Enter has nothing back there to press.
    if let Some(id) = ctx.memory(|m| m.focused()) {
        if ctx.read_response(id).map_or(true, |r| r.layer_id != card_layer) {
            ctx.memory_mut(|m| m.surrender_focus(id));
        }
    }
    if close {
        state.tags_open = false;
    }
    out
}

/// The tag list: every tag in `index`, a click showing only the slides
/// with it (or any other picked), and beside each its counts, a rename
/// and a remove (which asks first). `unread` slides are still opening;
/// the list scrolls past `height`.
fn tag_list(ui: &mut egui::Ui, state: &mut SlidePanel, index: &[TagCount], unread: usize, height: f32) -> Option<TagAction> {
    let mut out = None;
    if index.is_empty() {
        ui.label(egui::RichText::new("None yet: select something, then the arrange bar's tag button").small().color(theme::TEXT_MUTED));
    }
    if unread > 0 {
        let s = if unread == 1 { "slide is" } else { "slides are" };
        ui.label(egui::RichText::new(format!("{unread} {s} still opening; their tags come in when they do")).small().color(theme::TEXT_MUTED));
    }
    egui::ScrollArea::vertical().max_height(height).auto_shrink([false, true]).show(ui, |ui| {
        for t in index {
            ui.horizontal(|ui| {
                let renaming = state.tag_renaming.as_ref().is_some_and(|(from, _)| same_tag(from, &t.tag));
                if renaming {
                    let Some((from, text)) = state.tag_renaming.as_mut() else { return };
                    let r = ui.add(egui::TextEdit::singleline(text).desired_width(240.0).hint_text("new name (spaces: several)"));
                    if std::mem::take(&mut state.tag_focus) {
                        r.request_focus();
                        if let Some(mut st) = egui::text_edit::TextEditState::load(ui.ctx(), r.id) {
                            let end = egui::text::CCursor::new(text.chars().count());
                            st.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), end)));
                            st.store(ui.ctx(), r.id);
                        }
                    } else if r.lost_focus() {
                        // Enter renames; Esc or a click elsewhere leaves it.
                        let to = parse_tags(text);
                        if ui.input(|i| i.key_pressed(egui::Key::Enter)) && !to.is_empty() && to != [from.clone()] {
                            let from = from.clone();
                            // The filter follows the new name.
                            if state.tag_filter.iter().any(|f| same_tag(f, &from)) {
                                state.tag_filter.retain(|f| !same_tag(f, &from));
                                for n in &to {
                                    if !state.tag_filter.iter().any(|f| same_tag(f, n)) {
                                        state.tag_filter.push(n.clone());
                                    }
                                }
                            }
                            out = Some(TagAction::Rename(from, to));
                        }
                        state.tag_renaming = None;
                    }
                    return;
                }
                if state.tag_removing.as_deref().is_some_and(|r| same_tag(r, &t.tag)) {
                    let things = if t.objects == 1 { "object" } else { "objects" };
                    ui.label(egui::RichText::new(format!("Take #{} off {} {things}?", t.tag, t.objects)).small());
                    if ui.small_button("Remove").clicked() {
                        state.tag_filter.retain(|f| !same_tag(f, &t.tag));
                        out = Some(TagAction::Remove(t.tag.clone()));
                        state.tag_removing = None;
                    }
                    if ui.small_button("Keep").clicked() {
                        state.tag_removing = None;
                    }
                    return;
                }
                let mut on = state.tag_filter.iter().any(|f| same_tag(f, &t.tag));
                let label = egui::RichText::new(format!("#{}", t.tag)).color(TAG_COLOUR);
                if ui.toggle_value(&mut on, label).on_hover_text("Show only the slides with this tag (or another picked)").clicked() {
                    if on {
                        state.tag_filter.push(t.tag.clone());
                    } else {
                        state.tag_filter.retain(|f| !same_tag(f, &t.tag));
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    let trash = egui::Image::new(egui::include_image!("../../assets/icons/trash.svg"));
                    if icon_btn(ui, trash, "Remove this tag from every object (on unlocked slides)", true, false) {
                        state.tag_removing = Some(t.tag.clone());
                        state.tag_renaming = None;
                    }
                    let pen = egui::Image::new(egui::include_image!("../../assets/icons/pen.svg"));
                    if icon_btn(ui, pen, "Rename this tag everywhere; to a tag that exists, they merge", true, false) {
                        state.tag_renaming = Some((t.tag.clone(), t.tag.clone()));
                        state.tag_focus = true;
                        state.tag_removing = None;
                    }
                    let slides = if t.slides == 1 { "slide" } else { "slides" };
                    ui.label(egui::RichText::new(format!("{} {slides} · {}", t.slides, t.objects)).small().color(theme::TEXT_MUTED))
                        .on_hover_text(format!("On {} {slides}, {} objects", t.slides, t.objects));
                });
            });
        }
    });
    if !state.tag_filter.is_empty() && ui.button("Show every slide").clicked() {
        state.tag_filter.clear();
    }
    out
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
    icon_btn_response(ui, img, tooltip, enabled, selected).clicked() && enabled
}

fn icon_btn_response(ui: &mut egui::Ui, img: egui::Image<'_>, tooltip: &str, enabled: bool, selected: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(ICON + 4.0), egui::Sense::click());
    if selected {
        ui.painter().rect_filled(rect, Rounding::same(5.0), theme::ACCENT);
    } else if enabled && response.hovered() {
        ui.painter().rect_filled(rect, Rounding::same(5.0), Color32::from_white_alpha(18));
    }
    let tint = if enabled { theme::TEXT_PRIMARY } else { Color32::from_white_alpha(60) };
    paint_svg(ui, rect.shrink(4.0), img, tint);
    response.on_hover_text(tooltip)
}

fn paint_svg(ui: &mut egui::Ui, rect: egui::Rect, img: egui::Image<'_>, tint: Color32) {
    img.tint(tint).fit_to_exact_size(rect.size()).paint_at(ui, rect);
}
