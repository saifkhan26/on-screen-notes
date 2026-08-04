//! Layer panel — minimal floating card bottom-left.
//!
//! Each row is a tight strip of icon buttons only — no heading, no layer
//! name. Active layer's row gets a translucent-white background so the
//! user can tell which one tools target. Top of the panel = top of the
//! stack (Photoshop convention).

use crate::canvas::canvas::Canvas;
use crate::canvas::CanvasManager;
use crate::ui::theme;
use egui::{Color32, Rounding, Vec2};

const ICON: f32 = 18.0;
const ROW_H: f32 = 22.0;
const ROW_PAD: f32 = 3.0;

/// Render the layer panel and route row interactions back into the
/// active canvas. `cache` provides per-layer thumbnail textures used by
/// the hover preview popup; `canvas_idx` keys those entries.
///
/// Takes the manager rather than a `&mut Canvas` so the mutable borrow
/// can be deferred until the user actually clicks something. Drawing the
/// panel is a read-only operation, and `CanvasManager::active_mut` is
/// what flags a canvas for the autosave — taking it every frame just to
/// paint would mark the canvas dirty ~60 times a second and put the
/// autosave back into a permanent rewrite loop.
pub fn show(
    ctx: &egui::Context,
    cache: &mut crate::ui::overlay::OverlayCache,
    canvas_idx: usize,
    manager: &mut CanvasManager,
) {
    let mut action: Option<LayerAction> = None;

    egui::Area::new(egui::Id::new("osn_layer_panel"))
        .anchor(egui::Align2::LEFT_BOTTOM, Vec2::new(10.0, -10.0))
        .show(ctx, |ui| {
            // Tighter card frame than the global card — kills the
            // extra padding so the panel stays a slim icon-strip.
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
                ui.vertical(|ui| {
                    // Painting only reads the canvas; the action is
                    // collected here and applied after the panel closes.
                    let canvas = manager.active();
                    let n = canvas.layers.len();
                    for ui_row in 0..n {
                        let li = n - 1 - ui_row;
                        if let Some(a) = layer_row(ui, cache, canvas_idx, canvas, li) {
                            action = Some(a);
                        }
                    }

                    // Tiny add-layer button under the rows.
                    if add_row(ui).clicked() {
                        action = Some(LayerAction::Add);
                    }
                });
            });
        });

    // Only now take the mutable borrow — on the rare frame where the
    // user actually clicked a row.
    if let Some(a) = action {
        let canvas = manager.active_mut();
        match a {
            LayerAction::Add               => canvas.add_layer(),
            LayerAction::Select(i)         => canvas.active_layer = i,
            LayerAction::ToggleVisible(i)  => canvas.toggle_layer_visible(i),
            LayerAction::Delete(i)         => canvas.delete_layer(i),
            LayerAction::MoveUp(i)         => canvas.move_layer(i, i + 1),
            LayerAction::MoveDown(i) => {
                if i > 0 { canvas.move_layer(i, i - 1); }
            }
        }
    }
}

/// One layer row — [eye] [↑] [↓] [✕]. Background highlights the
/// currently-active layer; row body (outside the 4 icons) is clickable
/// to select.
fn layer_row(
    ui: &mut egui::Ui,
    cache: &mut crate::ui::overlay::OverlayCache,
    canvas_idx: usize,
    canvas: &Canvas,
    li: usize,
) -> Option<LayerAction> {
    let layer = &canvas.layers[li];
    let active = canvas.active_layer == li;
    let mut action: Option<LayerAction> = None;

    let total_w = ICON * 4.0 + ROW_PAD * 5.0;
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(total_w, ROW_H),
        egui::Sense::click(),
    );

    let painter = ui.painter();
    let bg = if active {
        theme::ACCENT
    } else if response.hovered() {
        Color32::from_white_alpha(14)
    } else {
        Color32::TRANSPARENT
    };
    painter.rect_filled(rect, Rounding::same(6.0), bg);

    let mid_y = rect.center().y;
    let mut x  = rect.min.x + ROW_PAD;

    // Eye toggle.
    let eye_rect = egui::Rect::from_center_size(
        egui::pos2(x + ICON * 0.5, mid_y),
        Vec2::splat(ICON),
    );
    let eye_resp = ui.interact(eye_rect, ui.id().with(("eye", li)), egui::Sense::click());
    paint_svg(
        ui,
        eye_rect.shrink(2.0),
        if layer.visible {
            egui::Image::new(egui::include_image!("../../assets/icons/eye.svg"))
        } else {
            egui::Image::new(egui::include_image!("../../assets/icons/eye-off.svg"))
        },
        if layer.visible { theme::TEXT_PRIMARY } else { theme::TEXT_MUTED },
    );
    if eye_resp.on_hover_text(if layer.visible { "Hide layer" } else { "Show layer" }).clicked() {
        action = Some(LayerAction::ToggleVisible(li));
    }
    x += ICON + ROW_PAD;

    // Move up — disabled at top of stack.
    let up_rect = egui::Rect::from_center_size(
        egui::pos2(x + ICON * 0.5, mid_y),
        Vec2::splat(ICON),
    );
    let can_up = li + 1 < canvas.layers.len();
    let up_resp = ui.interact(up_rect, ui.id().with(("up", li)), egui::Sense::click());
    paint_svg(
        ui,
        up_rect.shrink(2.0),
        egui::Image::new(egui::include_image!("../../assets/icons/arrow-up.svg")),
        if can_up { theme::TEXT_PRIMARY } else { Color32::from_white_alpha(60) },
    );
    if up_resp.on_hover_text("Move layer up").clicked() && can_up {
        action = Some(LayerAction::MoveUp(li));
    }
    x += ICON + ROW_PAD;

    // Move down — disabled at bottom of stack.
    let down_rect = egui::Rect::from_center_size(
        egui::pos2(x + ICON * 0.5, mid_y),
        Vec2::splat(ICON),
    );
    let can_down = li > 0;
    let down_resp = ui.interact(down_rect, ui.id().with(("down", li)), egui::Sense::click());
    paint_svg(
        ui,
        down_rect.shrink(2.0),
        egui::Image::new(egui::include_image!("../../assets/icons/arrow-down.svg")),
        if can_down { theme::TEXT_PRIMARY } else { Color32::from_white_alpha(60) },
    );
    if down_resp.on_hover_text("Move layer down").clicked() && can_down {
        action = Some(LayerAction::MoveDown(li));
    }
    x += ICON + ROW_PAD;

    // Delete — disabled when only one layer remains.
    let del_rect = egui::Rect::from_center_size(
        egui::pos2(x + ICON * 0.5, mid_y),
        Vec2::splat(ICON),
    );
    let can_del = canvas.layers.len() > 1;
    let del_resp = ui.interact(del_rect, ui.id().with(("del", li)), egui::Sense::click());
    paint_svg(
        ui,
        del_rect.shrink(2.0),
        egui::Image::new(egui::include_image!("../../assets/icons/trash.svg")),
        if can_del { theme::TEXT_PRIMARY } else { Color32::from_white_alpha(60) },
    );
    if del_resp.on_hover_text("Delete layer").clicked() && can_del {
        action = Some(LayerAction::Delete(li));
    }

    // Bare row click (outside icons) selects the layer.
    if response.clicked() && !active {
        action = Some(LayerAction::Select(li));
    }

    // Hover preview: render the layer's content as a small image
    // tooltip. `ensure_layer_thumb` reuses an already-uploaded
    // texture when the layer's content hasn't changed since last
    // render, so this is cheap to hover repeatedly.
    let row_id = ui.id().with(("layer_row_hover", li));
    let hovered = ui.rect_contains_pointer(rect);
    if hovered {
        let ctx = ui.ctx().clone();
        if let Some(tex_id) = cache.ensure_layer_thumb(&ctx, canvas_idx, li, layer) {
            egui::show_tooltip(&ctx, ui.layer_id(), row_id, |ui| {
                ui.label(format!("Layer {} — {} strokes, {} shapes",
                    li + 1, layer.strokes.len(), layer.shapes.len()));
                let size = Vec2::new(240.0, 180.0);
                ui.add(egui::Image::new((tex_id, size)).fit_to_exact_size(size));
            });
        } else {
            egui::show_tooltip_text(&ctx, ui.layer_id(), row_id, "Empty layer");
        }
    }

    action
}

/// "+" row below the layer list — adds a new layer above the active one.
fn add_row(ui: &mut egui::Ui) -> egui::Response {
    let total_w = ICON * 4.0 + ROW_PAD * 5.0;
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(total_w, ROW_H),
        egui::Sense::click(),
    );
    let painter = ui.painter();
    let bg = if response.hovered() {
        Color32::from_white_alpha(18)
    } else {
        Color32::TRANSPARENT
    };
    painter.rect_filled(rect, Rounding::same(6.0), bg);
    paint_svg(
        ui,
        egui::Rect::from_center_size(rect.center(), Vec2::splat(ICON)).shrink(2.0),
        egui::Image::new(egui::include_image!("../../assets/icons/plus.svg")),
        theme::TEXT_PRIMARY,
    );
    response.on_hover_text("Add a new layer")
}

fn paint_svg(ui: &mut egui::Ui, rect: egui::Rect, img: egui::Image<'_>, tint: Color32) {
    img.tint(tint).fit_to_exact_size(rect.size()).paint_at(ui, rect);
}

enum LayerAction {
    Add,
    Select(usize),
    ToggleVisible(usize),
    Delete(usize),
    MoveUp(usize),
    MoveDown(usize),
}
