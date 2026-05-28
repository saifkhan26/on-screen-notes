//! Bottom-right status pill: canvas index only.
//!
//! Compact glass pill with the Lucide "frame" glyph + "n/N" indicator.
//! Pen-active and click-through indicators removed by request.

use crate::ui::theme;
use egui::{Color32, Vec2};

pub fn show(
    ctx: &egui::Context,
    canvas_index: usize,
    canvas_count: usize,
    _click_through: bool,
    _pen_active: bool,
) {
    egui::Area::new(egui::Id::new("osn_status"))
        .anchor(egui::Align2::RIGHT_BOTTOM, Vec2::new(-12.0, -12.0))
        .show(ctx, |ui| {
            theme::card_frame().show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    // Frame icon — represents the canvas surface.
                    let (rect, _) = ui.allocate_exact_size(
                        Vec2::splat(14.0),
                        egui::Sense::hover(),
                    );
                    egui::Image::new(egui::include_image!("../../assets/icons/frame.svg"))
                        .tint(Color32::from_rgb(240, 240, 240))
                        .fit_to_exact_size(rect.size())
                        .paint_at(ui, rect);
                    ui.label(
                        egui::RichText::new(format!("{}/{}", canvas_index + 1, canvas_count))
                            .small()
                            .color(Color32::from_rgb(220, 222, 230)),
                    );
                });
            });
        });
}
