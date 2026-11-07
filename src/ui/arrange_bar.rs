//! Arrange bar: shown under the toolbar while objects are selected.
//! Packs, groups by type, aligns and distributes the selection (see
//! `tools::arrange`) when there are two or more, copies it to a new
//! slide, moves it to another slide, and pins it (see `canvas::pins`). A selected pin gets a bar
//! of its own in the same place, which also shows it on one slide or
//! every slide.

use crate::canvas::CanvasManager;
use crate::tools::arrange::{Align, Arrange};
use crate::ui::theme;
use egui::{Color32, Rounding, Vec2};

const BTN: f32 = 28.0;

/// What a click on the bar asks for.
pub enum BarAction {
    Arrange(Arrange),
    CopyToNewSlide,
    /// Move the selection to this slide (an index).
    MoveToSlide(usize),
    /// Pin the selection to every slide, or unpin it if all of it is.
    TogglePin,
}

/// What a click on a selected pin's bar asks for.
pub enum PinBarAction {
    /// Go to the slide the pin's objects are on.
    Open,
    /// Show the pin on its own slide only (true), or on every slide.
    SetLocal(bool),
    Unpin,
}

/// Just under the toolbar, centred on it.
fn bar_pos(ctx: &egui::Context) -> egui::Pos2 {
    let toolbar = ctx.memory(|m| m.area_rect(egui::Id::new("osn_toolbar")));
    toolbar.map_or(egui::pos2(ctx.screen_rect().center().x, 62.0), |r| egui::pos2(r.center().x, r.bottom() + 6.0))
}

/// Draw the bar for a selection of `count` objects; `pinned` when every
/// one of them is in a pin; `slides` for the list "Move to slide" opens.
/// Returns what was clicked this frame, if anything.
pub fn show(ctx: &egui::Context, count: usize, pinned: bool, slides: &CanvasManager) -> Option<BarAction> {
    let pos = bar_pos(ctx);
    let mut picked = None;
    egui::Area::new(egui::Id::new("osn_arrange_bar"))
        .fixed_pos(pos)
        .pivot(egui::Align2::CENTER_TOP)
        .order(egui::Order::Middle)
        .show(ctx, |ui| {
            theme::card_frame().inner_margin(egui::Margin::symmetric(8.0, 4.0)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    ui.label(egui::RichText::new(format!("{count} selected")).small().color(theme::TEXT_MUTED));
                    ui.add_space(6.0);
                    if count >= 2 {
                        arrange_buttons(ui, count, &mut picked);
                        divider(ui);
                    }
                    if icon_btn(ui, egui::include_image!("../../assets/icons/copy-to-slide.svg"), true,
                        "Copy to a new slide — Ctrl+Shift+D  (added after the last slide)")
                    {
                        picked = Some(BarAction::CopyToNewSlide);
                    }
                    if let Some(to) = move_btn(ui, slides) {
                        picked = Some(BarAction::MoveToSlide(to));
                    }
                    let tip = if pinned {
                        "Unpin — stop showing these as a pin — Ctrl+P"
                    } else {
                        "Pin to every slide — the objects stay here, and a pin you can move and resize shows them on every slide; its bar can keep it to this slide — Ctrl+P"
                    };
                    if toggle_btn(ui, egui::include_image!("../../assets/icons/pin.svg"), pinned, tip) {
                        picked = Some(BarAction::TogglePin);
                    }
                });
            });
        });
    picked
}

/// The bar for a selected pin whose objects are on slide `slide` (an
/// index); `here` when that is the slide showing, `local` when the pin
/// shows on that slide only.
pub fn show_pin(ctx: &egui::Context, slide: usize, here: bool, local: bool) -> Option<PinBarAction> {
    let mut picked = None;
    egui::Area::new(egui::Id::new("osn_pin_bar"))
        .fixed_pos(bar_pos(ctx))
        .pivot(egui::Align2::CENTER_TOP)
        .order(egui::Order::Middle)
        .show(ctx, |ui| {
            theme::card_frame().inner_margin(egui::Margin::symmetric(8.0, 4.0)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let what = if here { "Pinned from this slide".to_string() } else { format!("Pinned from slide {}", slide + 1) };
                    ui.label(egui::RichText::new(what).small().color(theme::TEXT_MUTED));
                    if !here && ui.button(format!("Go to slide {}", slide + 1)).on_hover_text("Or double-click the pin").clicked() {
                        picked = Some(PinBarAction::Open);
                    }
                    divider(ui);
                    let only = if here { "This slide".to_string() } else { format!("Slide {} only", slide + 1) };
                    if ui.selectable_label(local, only).on_hover_text("Show the pin only on the slide its objects are on").clicked() {
                        picked = Some(PinBarAction::SetLocal(true));
                    }
                    if ui.selectable_label(!local, "Every slide").on_hover_text("Show the pin on every slide, at the same place").clicked() {
                        picked = Some(PinBarAction::SetLocal(false));
                    }
                    divider(ui);
                    if ui.button("Unpin").on_hover_text("Stop showing these as a pin — they stay on their slide. Delete does the same").clicked() {
                        picked = Some(PinBarAction::Unpin);
                    }
                });
            });
        });
    picked
}

/// "Move to slide": a button opening the list of the other slides, drawn
/// like the slide window's picker. The slide picked, if one was.
fn move_btn(ui: &mut egui::Ui, slides: &CanvasManager) -> Option<usize> {
    let popup = ui.make_persistent_id("osn_move_to_slide");
    let open = ui.memory(|m| m.is_popup_open(popup));
    let others = slides.count() > 1;
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(BTN), egui::Sense::click());
    let bg = if open {
        theme::ACCENT
    } else if others && response.hovered() {
        Color32::from_white_alpha(22)
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, Rounding::same(7.0), bg);
    let tint = if others { theme::TEXT_PRIMARY } else { Color32::from_white_alpha(60) };
    let icon = rect.shrink(rect.width() * 0.22);
    egui::Image::new(egui::include_image!("../../assets/icons/move-to-slide.svg"))
        .tint(tint)
        .fit_to_exact_size(icon.size())
        .paint_at(ui, icon);
    let tip = if others {
        "Move to slide… — the objects leave this slide and land on the one you pick, where they show now"
    } else {
        "Move to slide… — there is no other slide yet"
    };
    let response = response.on_hover_text(tip);
    if others && response.clicked() {
        ui.memory_mut(|m| m.toggle_popup(popup));
    }

    let mut picked = None;
    let active = slides.active_index();
    let room = (ui.ctx().screen_rect().max.y - rect.max.y - 24.0).max(72.0);
    egui::popup_below_widget(ui, popup, &response, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
        ui.set_min_width(crate::ui::slide_window::PICKER_W);
        ui.spacing_mut().item_spacing = Vec2::new(0.0, 2.0);
        ui.label(egui::RichText::new("Move to slide").small().color(theme::TEXT_MUTED));
        egui::ScrollArea::vertical().max_height(room.min(360.0)).show(ui, |ui| {
            for i in (0..slides.count()).filter(|&i| i != active) {
                if crate::ui::slide_window::slide_row(ui, slides, i, false) {
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

/// Pack, group, align and distribute, for two or more objects.
fn arrange_buttons(ui: &mut egui::Ui, count: usize, picked: &mut Option<BarAction>) {
    let mut btn = |ui: &mut egui::Ui, op: Arrange, img: egui::ImageSource<'static>, tip: &str| {
        if icon_btn(ui, img, count >= op.min_items(), tip) {
            *picked = Some(BarAction::Arrange(op));
        }
    };
    btn(ui, Arrange::Pack, egui::include_image!("../../assets/icons/pack.svg"),
        "Pack — fit the selection into a tight block, sizes kept");
    btn(ui, Arrange::GroupByType, egui::include_image!("../../assets/icons/group-by-type.svg"),
        "Group by type — pictures, GIFs, videos and other objects each packed into their own cluster");
    divider(ui);
    btn(ui, Arrange::Align(Align::Left), egui::include_image!("../../assets/icons/align-left.svg"), "Align left edges");
    btn(ui, Arrange::Align(Align::CenterX), egui::include_image!("../../assets/icons/align-center-x.svg"), "Align centres horizontally");
    btn(ui, Arrange::Align(Align::Right), egui::include_image!("../../assets/icons/align-right.svg"), "Align right edges");
    btn(ui, Arrange::Align(Align::Top), egui::include_image!("../../assets/icons/align-top.svg"), "Align top edges");
    btn(ui, Arrange::Align(Align::CenterY), egui::include_image!("../../assets/icons/align-center-y.svg"), "Align centres vertically");
    btn(ui, Arrange::Align(Align::Bottom), egui::include_image!("../../assets/icons/align-bottom.svg"), "Align bottom edges");
    divider(ui);
    btn(ui, Arrange::DistributeX, egui::include_image!("../../assets/icons/distribute-x.svg"),
        "Distribute horizontally — equal gaps (3 or more objects)");
    btn(ui, Arrange::DistributeY, egui::include_image!("../../assets/icons/distribute-y.svg"),
        "Distribute vertically — equal gaps (3 or more objects)");
}

fn divider(ui: &mut egui::Ui) {
    ui.add_space(3.0);
    let (r, _) = ui.allocate_exact_size(Vec2::new(1.0, BTN - 8.0), egui::Sense::hover());
    ui.painter().rect_filled(r, Rounding::ZERO, Color32::from_white_alpha(30));
    ui.add_space(3.0);
}

/// An icon button that shows whether it is on.
fn toggle_btn(ui: &mut egui::Ui, img: egui::ImageSource<'static>, on: bool, tooltip: &str) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(BTN), egui::Sense::click());
    let bg = if on {
        Color32::from_rgba_unmultiplied(86, 160, 255, 70)
    } else if resp.hovered() {
        Color32::from_white_alpha(22)
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, Rounding::same(7.0), bg);
    let icon = rect.shrink(rect.width() * 0.24);
    egui::Image::new(img).tint(theme::TEXT_PRIMARY).fit_to_exact_size(icon.size()).paint_at(ui, icon);
    resp.on_hover_text(tooltip).clicked()
}

fn icon_btn(ui: &mut egui::Ui, img: egui::ImageSource<'static>, enabled: bool, tooltip: &str) -> bool {
    let sense = if enabled { egui::Sense::click() } else { egui::Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(BTN), sense);
    let bg = if enabled && resp.hovered() { Color32::from_white_alpha(22) } else { Color32::TRANSPARENT };
    ui.painter().rect_filled(rect, Rounding::same(7.0), bg);
    let tint = if enabled { theme::TEXT_PRIMARY } else { Color32::from_white_alpha(70) };
    let icon = rect.shrink(rect.width() * 0.24);
    egui::Image::new(img).tint(tint).fit_to_exact_size(icon.size()).paint_at(ui, icon);
    resp.on_hover_text(tooltip).clicked()
}
