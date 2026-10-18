//! Arrange bar: shown under the toolbar while two or more objects are
//! selected. Packs, groups by type, aligns and distributes the
//! selection (see `tools::arrange`), and copies it to a new slide.

use crate::tools::arrange::{Align, Arrange};
use crate::ui::theme;
use egui::{Color32, Rounding, Vec2};

const BTN: f32 = 28.0;

/// What a click on the bar asks for.
pub enum BarAction {
    Arrange(Arrange),
    CopyToNewSlide,
}

/// Draw the bar for a selection of `count` objects. Returns what was
/// clicked this frame, if anything.
pub fn show(ctx: &egui::Context, count: usize) -> Option<BarAction> {
    // Just under the toolbar, centred on it.
    let toolbar = ctx.memory(|m| m.area_rect(egui::Id::new("osn_toolbar")));
    let pos = toolbar.map_or(egui::pos2(ctx.screen_rect().center().x, 62.0), |r| egui::pos2(r.center().x, r.bottom() + 6.0));
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
                    let mut btn = |ui: &mut egui::Ui, op: Arrange, img: egui::ImageSource<'static>, tip: &str| {
                        if icon_btn(ui, img, count >= op.min_items(), tip) {
                            picked = Some(BarAction::Arrange(op));
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
                    divider(ui);
                    if icon_btn(ui, egui::include_image!("../../assets/icons/copy-to-slide.svg"), true,
                        "Copy to a new slide — Ctrl+Shift+D  (added after the last slide)")
                    {
                        picked = Some(BarAction::CopyToNewSlide);
                    }
                });
            });
        });
    picked
}

fn divider(ui: &mut egui::Ui) {
    ui.add_space(3.0);
    let (r, _) = ui.allocate_exact_size(Vec2::new(1.0, BTN - 8.0), egui::Sense::hover());
    ui.painter().rect_filled(r, Rounding::ZERO, Color32::from_white_alpha(30));
    ui.add_space(3.0);
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
