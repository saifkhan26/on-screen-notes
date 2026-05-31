//! Floating toolbar: tool buttons + colour palette + size slider with
//! live preview. Anchored top-centre of the overlay.
//!
//! Icons are bundled SVGs (Lucide set) loaded through `egui_extras` and
//! tinted at render time. SVGs scale crisply at any DPI, are easy to swap,
//! and ship inside the binary via `include_image!`.

use crate::canvas::shape::BorderStyle;
use crate::canvas::smoothing::{SmoothingOptions, SmoothingType};
use crate::canvas::stroke::StrokeStyle;
use crate::config::SmoothingLevel;
use crate::tools::{ActiveTool, ToolKind};
use crate::ui::theme;
use egui::{Color32, Rounding, Stroke, Vec2};

/// Render the toolbar. Returns `true` if the user interacted with any
/// element this frame.
pub fn show(
    ctx: &egui::Context,
    tool: &mut ActiveTool,
    bg_opacity: &mut f32,
    bg_color: &mut [u8; 3],
    palette: &mut Vec<[u8; 4]>,
    smoothing: &mut SmoothingLevel,
    smoothing_options: &mut SmoothingOptions,
    pressure_curve: &mut f32,
    pinned_hwnd: &mut Option<isize>,
    pin_picking: &mut bool,
) -> bool {
    let mut interacted = false;

    egui::Area::new(egui::Id::new("osn_toolbar"))
        .anchor(egui::Align2::CENTER_TOP, Vec2::new(0.0, 10.0))
        .show(ctx, |ui| {
            theme::card_frame().show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 3.0;

                    // Group 1: freehand tools — Pen + Pencil share the
                    // same `ToolKind::Pen`, distinguished by `tool.style`.
                    //
                    // The standalone Eraser button intentionally is not in
                    // this group: erasing is now a Ctrl-held modifier (see
                    // `app::eraser_override`), which feels lighter than
                    // round-tripping through a tool change.
                    interacted |= pen_style_btn(ui, tool, StrokeStyle::Default, "Pen — P");
                    interacted |= pen_style_btn(ui, tool, StrokeStyle::Pencil,  "Pencil — B");
                    interacted |= pen_style_btn(ui, tool, StrokeStyle::Marker,  "Marker — M  (translucent highlighter; overlaps darken)");
                    interacted |= pen_style_btn(ui, tool, StrokeStyle::Airbrush, "Airbrush — G  (soft spray; hold still to build density)");
                    interacted |= tool_btn(ui, tool, ToolKind::Fill,          "Fill — F");
                    interacted |= tool_btn(ui, tool, ToolKind::Laser,         "Laser pointer — L  (fades after pen-up)");
                    interacted |= tool_btn(ui, tool, ToolKind::Text,          "Text — T  (click to place, Enter to commit, Shift+Enter newline)");
                    interacted |= tool_btn(ui, tool, ToolKind::Spotlight,     "Spotlight — S  (dim everything except a square around the cursor; wheel resizes)");
                    interacted |= tool_btn(ui, tool, ToolKind::LassoErase,    "Lasso erase — X  (draw a closed loop; ink whose centroid lands inside is removed)");
                    spacer(ui);

                    // Group 2: shape tools
                    interacted |= tool_btn(ui, tool, ToolKind::Rect,          "Rectangle — R");
                    interacted |= tool_btn(ui, tool, ToolKind::Ellipse,       "Ellipse — O");
                    interacted |= tool_btn(ui, tool, ToolKind::Line,          "Line — straight line (toolbar only)");
                    interacted |= tool_btn(ui, tool, ToolKind::Arrow,         "Arrow — A");
                    interacted |= tool_btn(ui, tool, ToolKind::FreehandArrow, "Freehand arrow — Shift+A");
                    // Contextual dashed-border toggle — visible only
                    // while the Rect / Ellipse / Line tool is active,
                    // since the dashed variant is only wired into
                    // those shapes. Sits inside the same group so it
                    // reads as a modifier on the adjacent buttons.
                    if matches!(tool.kind, ToolKind::Rect | ToolKind::Ellipse | ToolKind::Line | ToolKind::Arrow) {
                        interacted |= dash_toggle_btn(ui, tool);
                    }
                    spacer(ui);

                    // Group 3: colours
                    for i in 0..palette.len() {
                        let slot = palette[i];
                        let (new_slot, hit) = colour_swatch(ui, tool, slot, i);
                        palette[i] = new_slot;
                        interacted |= hit;
                    }
                    spacer(ui);

                    // Group 4: size
                    interacted |= size_widget(ui, tool);
                    spacer(ui);

                    // Group 5: utilities (pin + settings + help)
                    interacted |= pin_btn(ui, pinned_hwnd, pin_picking);
                    interacted |= settings_btn(ui, bg_opacity, bg_color, smoothing, smoothing_options, pressure_curve);
                    interacted |= help_btn(ui);
                });
            });
        });

    interacted
}

const BTN_SIZE: f32 = 28.0;
const SWATCH_SIZE: f32 = 16.0;
const BTN_ROUND: f32 = 7.0;

/// Pen / Pencil button. Both select `ToolKind::Pen`; they differ only in
/// `tool.style`, which the renderer uses to switch between the smooth
/// opaque ribbon and the translucent grainy pencil look. Active when the
/// pen tool is selected *and* its style matches `style`.
fn pen_style_btn(
    ui: &mut egui::Ui,
    tool: &mut ActiveTool,
    style: StrokeStyle,
    tooltip: &str,
) -> bool {
    let selected = tool.kind == ToolKind::Pen && tool.style == style;
    let size = Vec2::splat(BTN_SIZE);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());

    let painter = ui.painter();
    let bg = if selected {
        theme::ACCENT
    } else if response.hovered() {
        Color32::from_white_alpha(22)
    } else {
        Color32::TRANSPARENT
    };
    painter.rect_filled(rect, Rounding::same(BTN_ROUND), bg);

    let icon_color = theme::TEXT_PRIMARY;
    let icon_rect = rect.shrink(rect.width() * 0.22);
    let img = match style {
        StrokeStyle::Default => egui::Image::new(egui::include_image!("../../assets/icons/pen.svg")),
        StrokeStyle::Pencil  => egui::Image::new(egui::include_image!("../../assets/icons/pencil.svg")),
        StrokeStyle::Marker  => egui::Image::new(egui::include_image!("../../assets/icons/marker.svg")),
        StrokeStyle::Airbrush => egui::Image::new(egui::include_image!("../../assets/icons/spray-can.svg")),
    };
    img.tint(icon_color)
        .fit_to_exact_size(icon_rect.size())
        .paint_at(ui, icon_rect);

    let response = response.on_hover_text(tooltip);
    if response.clicked() {
        tool.kind  = ToolKind::Pen;
        tool.style = style;
        return true;
    }
    response.hovered() || response.is_pointer_button_down_on()
}

fn tool_btn(ui: &mut egui::Ui, tool: &mut ActiveTool, kind: ToolKind, tooltip: &str) -> bool {
    let selected = tool.kind == kind;
    let size = Vec2::splat(BTN_SIZE);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());

    let painter = ui.painter();
    let bg = if selected {
        theme::ACCENT
    } else if response.hovered() {
        Color32::from_white_alpha(22)
    } else {
        Color32::TRANSPARENT
    };
    painter.rect_filled(rect, Rounding::same(BTN_ROUND), bg);

    // Icon stays white regardless of state — selection is signalled by
    // the translucent white fill behind, not by a colour change.
    let icon_color = theme::TEXT_PRIMARY;

    // Centre the icon, leaving ~22 % padding on every side.
    let icon_rect = rect.shrink(rect.width() * 0.22);
    paint_icon(ui, icon_rect, kind, icon_color);

    let response = response.on_hover_text(tooltip);
    if response.clicked() {
        tool.kind = kind;
        return true;
    }
    response.hovered() || response.is_pointer_button_down_on()
}

/// Render the SVG icon for a tool inside `rect`, tinted to `color`.
///
/// SVGs are baked into the binary by `egui::include_image!`. The Lucide
/// icons use `stroke="currentColor"`, which `egui_extras`'s SVG loader
/// honours via `Image::tint(...)` — so the same SVG renders white when the
/// button is selected, near-white otherwise.
fn paint_icon(ui: &mut egui::Ui, rect: egui::Rect, kind: ToolKind, color: Color32) {
    let img = match kind {
        ToolKind::Pen           => egui::Image::new(egui::include_image!("../../assets/icons/pen.svg")),
        ToolKind::Eraser        => egui::Image::new(egui::include_image!("../../assets/icons/eraser.svg")),
        ToolKind::Rect          => egui::Image::new(egui::include_image!("../../assets/icons/rect.svg")),
        ToolKind::Ellipse       => egui::Image::new(egui::include_image!("../../assets/icons/ellipse.svg")),
        ToolKind::Line          => egui::Image::new(egui::include_image!("../../assets/icons/line.svg")),
        ToolKind::Arrow         => egui::Image::new(egui::include_image!("../../assets/icons/arrow.svg")),
        ToolKind::FreehandArrow => egui::Image::new(egui::include_image!("../../assets/icons/freehand_arrow.svg")),
        ToolKind::Fill          => egui::Image::new(egui::include_image!("../../assets/icons/fill.svg")),
        ToolKind::Laser         => egui::Image::new(egui::include_image!("../../assets/icons/laser.svg")),
        ToolKind::Text          => egui::Image::new(egui::include_image!("../../assets/icons/text.svg")),
        ToolKind::Spotlight     => egui::Image::new(egui::include_image!("../../assets/icons/spotlight.svg")),
        ToolKind::LassoErase    => egui::Image::new(egui::include_image!("../../assets/icons/lasso.svg")),
    };
    img.tint(color)
        .fit_to_exact_size(rect.size())
        .paint_at(ui, rect);
}

/// Palette swatch widget. Returns the (possibly edited) colour + whether
/// the user interacted with this swatch this frame.
///
/// Interaction model:
///   * **Left click** — pick this colour as the active pen colour.
///   * **Right click** — open an egui colour picker popup anchored to the
///     swatch so the user can edit the pinned colour in place.
///
/// The popup uses a stable `egui::Id` derived from the swatch index so it
/// survives across frames and only closes when the user clicks outside.
fn colour_swatch(
    ui: &mut egui::Ui,
    tool: &mut ActiveTool,
    color: [u8; 4],
    index: usize,
) -> ([u8; 4], bool) {
    let size = Vec2::splat(SWATCH_SIZE);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
    let mut current = color;
    let c = Color32::from_rgba_unmultiplied(color[0], color[1], color[2], color[3]);
    let selected = tool.color == color;

    let painter = ui.painter();
    // Swatch body — flat rounded square at the chosen colour. No
    // outline (user-requested minimal aesthetic). Selected state is
    // signalled by a slight inset gap + translucent ring, not by a
    // saturated halo.
    let body = if selected {
        rect.shrink(3.5)
    } else {
        rect.shrink(2.5)
    };
    painter.rect_filled(body, Rounding::same(5.0), c);
    if selected {
        // Translucent white ring around the selected swatch.
        painter.rect_stroke(
            rect.shrink(1.0),
            Rounding::same(6.0),
            Stroke::new(1.4, Color32::from_white_alpha(200)),
        );
    } else if response.hovered() {
        painter.rect_stroke(
            rect.shrink(1.5),
            Rounding::same(5.5),
            Stroke::new(1.0, Color32::from_white_alpha(90)),
        );
    }

    let popup_id = ui.make_persistent_id(("osn_palette_popup", index));
    let response = response.on_hover_text(format!(
        "Colour #{:02X}{:02X}{:02X}  •  L-click: select  •  R-click: edit",
        color[0], color[1], color[2]
    ));

    let mut interacted = false;
    if response.clicked() {
        tool.color = color;
        interacted = true;
    }
    if response.secondary_clicked() {
        ui.memory_mut(|m| m.toggle_popup(popup_id));
        interacted = true;
    }

    // Popup with the egui colour picker, anchored under the swatch. The
    // user mutates `c32`; we mirror the change back into the [u8;4] slot.
    egui::popup::popup_below_widget(
        ui,
        popup_id,
        &response,
        egui::popup::PopupCloseBehavior::CloseOnClickOutside,
        |ui: &mut egui::Ui| {
        ui.set_min_width(220.0);
        let mut c32 = c;
        let changed = egui::color_picker::color_picker_color32(
            ui,
            &mut c32,
            egui::color_picker::Alpha::Opaque,
        );
        if changed {
            current = c32.to_array();
            // Live-update the active pen colour too if this slot is the
            // currently-selected one — feels more responsive than waiting
            // for the user to re-click after editing.
            if selected {
                tool.color = current;
            }
            interacted = true;
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.button("Reset").clicked() {
                let defaults = crate::config::default_palette();
                if let Some(&d) = defaults.get(index) {
                    current = d;
                    if selected {
                        tool.color = current;
                    }
                    interacted = true;
                }
            }
            if ui.button("Close").clicked() {
                ui.memory_mut(|m| m.close_popup());
            }
        });
    },
    );

    let hovering = response.hovered() || response.is_pointer_button_down_on();
    (current, interacted || hovering)
}

fn size_widget(ui: &mut egui::Ui, tool: &mut ActiveTool) -> bool {
    let mut interacted = false;
    ui.scope(|ui| {
        // Live preview dot — scales with `tool.size`, capped to fit the row.
        let dot_size = Vec2::splat(SWATCH_SIZE);
        let (rect, _) = ui.allocate_exact_size(dot_size, egui::Sense::hover());
        let preview_radius = (tool.size * 0.5).clamp(1.5, 7.0);
        ui.painter().circle_filled(
            rect.center(),
            preview_radius,
            Color32::from_rgba_unmultiplied(
                tool.color[0], tool.color[1], tool.color[2], tool.color[3],
            ),
        );

        let slider = ui.add_sized(
            Vec2::new(100.0, 16.0),
            egui::Slider::new(&mut tool.size, 1.0..=80.0)
                .integer()
                .show_value(false),
        );
        if slider.changed() || slider.dragged() {
            interacted = true;
        }
    });
    interacted
}

/// Help "?" button — purely hover-driven. On hover, egui pops a tooltip
/// listing every keyboard shortcut + scroll-wheel + Alt behaviour. Lives
/// in the toolbar so it is always discoverable but never in the way.
fn help_btn(ui: &mut egui::Ui) -> bool {
    let size = Vec2::splat(BTN_SIZE);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());

    let painter = ui.painter();
    let bg = if response.hovered() {
        Color32::from_white_alpha(32)
    } else {
        Color32::TRANSPARENT
    };
    painter.rect_filled(rect, Rounding::same(BTN_ROUND), bg);

    // Outline circle + "?" glyph drawn via the painter, so we don't depend
    // on a font containing a stylised help glyph.
    let r = rect.width() * 0.30;
    let stroke = Stroke::new(1.6, theme::TEXT_PRIMARY);
    painter.circle_stroke(rect.center(), r, stroke);
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "?",
        egui::FontId::proportional(BTN_SIZE * 0.50),
        theme::TEXT_PRIMARY,
    );

    // Rich tooltip on hover — three-column grid of action / shortcut. Using
    // `on_hover_ui` (not `on_hover_text`) gives us proper layout control.
    response.on_hover_ui(|ui| {
        ui.set_max_width(360.0);
        ui.vertical(|ui| {
            ui.label(
                egui::RichText::new("Keyboard & input shortcuts")
                    .strong()
                    .size(13.0),
            );
            ui.add_space(4.0);

            egui::Grid::new("osn_help_grid")
                .num_columns(2)
                .spacing([18.0, 4.0])
                .show(ui, |ui| {
                    let row = |ui: &mut egui::Ui, action: &str, keys: &str| {
                        ui.label(action);
                        ui.label(
                            egui::RichText::new(keys)
                                .monospace()
                                .color(Color32::from_rgb(232, 200, 200)),
                        );
                        ui.end_row();
                    };
                    row(ui, "Pen",                "P");
                    row(ui, "Pencil (brush)",     "B");
                    row(ui, "Marker (highlighter)","M");
                    row(ui, "Airbrush (spray)",   "G");
                    row(ui, "Fill",               "F");
                    row(ui, "Laser pointer",      "L");
                    row(ui, "Spotlight",          "S");
                    row(ui, "Lasso erase",        "X");
                    row(ui, "Eyedropper (1 shot)","I");
                    row(ui, "Magnifier loupe",    "Hold Z  (wheel resizes zoom)");
                    row(ui, "Text",               "T  (Enter = commit, Shift+Enter = newline)");
                    row(ui, "Rectangle",          "R");
                    row(ui, "Ellipse",            "O");
                    row(ui, "Straight arrow",     "A");
                    row(ui, "Freehand arrow",     "Shift + A");
                    row(ui, "Undo / Redo",        "Ctrl+Z / Ctrl+Shift+Z");
                    row(ui, "Clear canvas",       "Ctrl + Delete");
                    row(ui, "Delete this canvas", "Ctrl + Shift + Backspace");
                    row(ui, "Prev / next canvas", "← / →");
                    row(ui, "Prev / next layer",  "↓ / ↑");
                    row(ui, "Hide / show UI",     "Tab");
                    row(ui, "Background opacity", "Ctrl + Scroll");
                    row(ui, "Layer opacity",      "Shift + Scroll");
                    row(ui, "Click-through",      "Hold Alt");
                    row(ui, "Temporary eraser",   "Hold Ctrl + draw");
                    row(ui, "Pick colour",        "Hold Shift + tap");
                    row(ui, "Pan canvas",         "Hold Space + drag");
                    row(ui, "Pen size (pen tool)","Scroll");
                    row(ui, "Zoom (other tools)", "Scroll");
                    row(ui, "Reset zoom + pan",   "Ctrl + 0");
                    row(ui, "Screenshot (bg+ink)","Ctrl + Shift + S");
                    row(ui, "Freeze frame → layer","Ctrl + Shift + F");
                    row(ui, "Pin to window",      "toolbar pin button");
                    row(ui, "Pause / resume record","Ctrl + Shift + P");
                    row(ui, "Toggle overlay",     "Ctrl + Shift + N");
                });
        });
    });

    // Hover is *not* a click — never report it as a user interaction so
    // pen events still flow to the canvas.
    false
}

/// Smoothing-level picker: Low / Medium / High as inline
/// `selectable_value` pills. Matches the Krita mode row visually so
/// both selectors in the settings popup read as the same widget family.
fn smoothing_widget(ui: &mut egui::Ui, level: &mut SmoothingLevel) -> bool {
    let mut interacted = false;
    ui.horizontal_wrapped(|ui| {
        for opt in [SmoothingLevel::Low, SmoothingLevel::Medium, SmoothingLevel::High] {
            let resp = ui.selectable_value(level, opt, opt.label());
            let tip = match opt {
                SmoothingLevel::Low    => "Low — least lag, raw feel",
                SmoothingLevel::Medium => "Medium — balanced default",
                SmoothingLevel::High   => "High — cleanest lines, slight lag",
            };
            let resp = resp.on_hover_text(tip);
            if resp.clicked() || resp.hovered() {
                interacted = true;
            }
        }
    });
    interacted
}

/// Invisible group divider — small gap so groups read as separate
/// without a visible rule. Minimal aesthetic — no painted line.
fn spacer(ui: &mut egui::Ui) {
    ui.add_space(8.0);
}

/// Contextual "dashed border" toggle. Drawn as a small rectangle with
/// either a solid or dashed outline, painted directly via the egui
/// painter so we don't need an extra SVG. Clicking flips
/// `tool.border_style` between `Solid` and `DashedAnimated`; the
/// renderer reads this flag when committing the next Rect or Ellipse.
fn dash_toggle_btn(ui: &mut egui::Ui, tool: &mut ActiveTool) -> bool {
    let active = matches!(tool.border_style, BorderStyle::DashedAnimated);
    let size = Vec2::splat(BTN_SIZE);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());

    let painter = ui.painter();
    let bg = if active {
        theme::ACCENT
    } else if response.hovered() {
        Color32::from_white_alpha(22)
    } else {
        Color32::TRANSPARENT
    };
    painter.rect_filled(rect, Rounding::same(BTN_ROUND), bg);

    // Icon: a small box, dashed when `active` so the user sees what
    // they will get. The four sides are emitted as short dashed
    // segments — no font glyph needed.
    let icon = rect.shrink(rect.width() * 0.26);
    let stroke = Stroke::new(1.6, theme::TEXT_PRIMARY);
    if active {
        // Eight dashes per side feels lively without overcrowding the
        // 28-px button. Emit by hand so we don't pull a polyline
        // helper just for the icon.
        let segs = 6;
        let on  = 1.0_f32;
        let off = 1.0_f32;
        let span = on + off;
        let _ = span;
        let edges = [
            (icon.left_top(),    icon.right_top()),
            (icon.right_top(),   icon.right_bottom()),
            (icon.right_bottom(),icon.left_bottom()),
            (icon.left_bottom(), icon.left_top()),
        ];
        for (a, b) in edges {
            let dx = b.x - a.x;
            let dy = b.y - a.y;
            for i in 0..segs {
                let t0 = i as f32 / segs as f32;
                let t1 = (i as f32 + 0.55) / segs as f32;
                painter.line_segment(
                    [egui::pos2(a.x + dx * t0, a.y + dy * t0),
                     egui::pos2(a.x + dx * t1, a.y + dy * t1)],
                    stroke,
                );
            }
        }
    } else {
        painter.rect_stroke(icon, Rounding::same(2.0), stroke);
    }

    let response = response.on_hover_text(
        if active {
            "Dashed border ON — next shape will use animated dashes (click to switch back)"
        } else {
            "Dashed border OFF — click to switch the next shape to animated dashes"
        }
    );
    if response.clicked() {
        tool.border_style = match tool.border_style {
            BorderStyle::Solid          => BorderStyle::DashedAnimated,
            BorderStyle::DashedAnimated => BorderStyle::Solid,
        };
        return true;
    }
    response.hovered() || response.is_pointer_button_down_on()
}

/// Pin-to-window toolbar button. Three visual states:
///   * No pin set, no picking → idle outline.
///   * Pin picking armed → accent fill + crosshair tooltip.
///   * Pinned to a window → accent fill + "unpin" tooltip.
/// Click semantics: idle → arm picking; picking → cancel; pinned →
/// unpin. The actual pick-and-follow logic lives in `app.rs`.
fn pin_btn(
    ui: &mut egui::Ui,
    pinned_hwnd: &mut Option<isize>,
    pin_picking: &mut bool,
) -> bool {
    let size = Vec2::splat(BTN_SIZE);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let active = pinned_hwnd.is_some() || *pin_picking;

    let painter = ui.painter();
    let bg = if active {
        theme::ACCENT
    } else if response.hovered() {
        Color32::from_white_alpha(22)
    } else {
        Color32::TRANSPARENT
    };
    painter.rect_filled(rect, Rounding::same(BTN_ROUND), bg);

    let icon_color = theme::TEXT_PRIMARY;
    let icon_rect = rect.shrink(rect.width() * 0.22);
    egui::Image::new(egui::include_image!("../../assets/icons/pin.svg"))
        .tint(icon_color)
        .fit_to_exact_size(icon_rect.size())
        .paint_at(ui, icon_rect);

    let tooltip = if *pin_picking {
        "Click any window to pin — Esc cancels"
    } else if pinned_hwnd.is_some() {
        "Pinned. Click to unpin (OSN stops following the target window)"
    } else {
        "Pin to a window — click, then click any window to follow it"
    };
    let response = response.on_hover_text(tooltip);
    if response.clicked() {
        if pinned_hwnd.is_some() {
            *pinned_hwnd = None;
        } else {
            *pin_picking = !*pin_picking;
        }
        return true;
    }
    response.hovered() || response.is_pointer_button_down_on()
}

/// Combined settings button — opens a popup with background-opacity and
/// smoothing-level controls. Folding these secondary controls behind a
/// single button keeps the always-visible toolbar tight.
fn settings_btn(
    ui: &mut egui::Ui,
    bg_opacity: &mut f32,
    bg_color: &mut [u8; 3],
    smoothing: &mut SmoothingLevel,
    smoothing_options: &mut SmoothingOptions,
    pressure_curve: &mut f32,
) -> bool {
    let size = Vec2::splat(BTN_SIZE);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());

    let painter = ui.painter();
    let bg = if response.hovered() {
        Color32::from_white_alpha(32)
    } else {
        Color32::TRANSPARENT
    };
    painter.rect_filled(rect, Rounding::same(BTN_ROUND), bg);

    // Sliders glyph — three short horizontal lines with a knob.
    let cx = rect.center().x;
    let cy = rect.center().y;
    let line_color = theme::TEXT_PRIMARY;
    let line_w = 1.6;
    let half_w = rect.width() * 0.30;
    for (i, ratio) in [-1.0_f32, 0.0, 1.0].into_iter().enumerate() {
        let y = cy + ratio * 5.0;
        painter.line_segment(
            [egui::pos2(cx - half_w, y), egui::pos2(cx + half_w, y)],
            Stroke::new(line_w, line_color),
        );
        // Per-row "slider knob" position — staggered so the icon reads as
        // a multi-slider settings panel rather than a stack of identical
        // lines.
        let knob_x = match i {
            0 => cx + half_w * 0.35,
            1 => cx - half_w * 0.45,
            _ => cx + half_w * 0.10,
        };
        painter.circle_filled(egui::pos2(knob_x, y), 1.6, line_color);
    }

    let response = response.on_hover_text("Settings");
    let popup_id = ui.make_persistent_id("osn_settings_popup");
    let mut interacted = false;
    if response.clicked() {
        ui.memory_mut(|m| m.toggle_popup(popup_id));
        interacted = true;
    }

    egui::popup::popup_below_widget(
        ui,
        popup_id,
        &response,
        egui::popup::PopupCloseBehavior::CloseOnClickOutside,
        |ui: &mut egui::Ui| {
            ui.set_min_width(240.0);
            // Background opacity row.
            ui.vertical(|ui| {
                ui.label(
                    egui::RichText::new("Background opacity")
                        .small()
                        .color(theme::TEXT_MUTED),
                );
                ui.add(
                    egui::Slider::new(bg_opacity, 0.0..=1.0)
                        .show_value(false),
                );
            });
            ui.add_space(6.0);
            // Background colour row — paired with the opacity slider so
            // the user can tint the dim layer (paper-cream, navy, etc.).
            //
            // We render the full HSV picker *inline* inside this popup
            // rather than using `color_edit_button_srgba`, because that
            // helper opens its own child popup. Egui treats the click on
            // the child popup as "outside" the parent popup, so the
            // parent (this settings popup) closes the instant the user
            // tries to interact with the picker. Inline rendering keeps
            // everything in one popup — same model the palette swatch
            // editor already uses.
            ui.vertical(|ui| {
                ui.label(
                    egui::RichText::new("Background colour")
                        .small()
                        .color(theme::TEXT_MUTED),
                );
                let mut c32 = Color32::from_rgb(bg_color[0], bg_color[1], bg_color[2]);
                let changed = egui::color_picker::color_picker_color32(
                    ui,
                    &mut c32,
                    egui::color_picker::Alpha::Opaque,
                );
                if changed {
                    *bg_color = [c32.r(), c32.g(), c32.b()];
                }
                ui.add_space(4.0);
                if ui
                    .button("Reset background")
                    .on_hover_text("Restore black background")
                    .clicked()
                {
                    *bg_color = [0, 0, 0];
                }
            });
            ui.add_space(6.0);
            // Smoothing level row.
            ui.vertical(|ui| {
                ui.label(
                    egui::RichText::new("Stroke smoothing")
                        .small()
                        .color(theme::TEXT_MUTED),
                );
                ui.horizontal(|ui| {
                    let _ = smoothing_widget(ui, smoothing);
                });
            });
            ui.add_space(6.0);
            // Krita-style smoothing mode selector + per-mode knobs.
            ui.vertical(|ui| {
                ui.label(
                    egui::RichText::new("Smoothing mode (Krita)")
                        .small()
                        .color(theme::TEXT_MUTED),
                );
                // Inline pill row — NOT a ComboBox. ComboBox opens a
                // child popup, and egui treats that child popup as
                // "outside" the settings popup → settings closes the
                // instant the user clicks the dropdown. Same gotcha
                // already documented above for the bg colour picker.
                ui.horizontal_wrapped(|ui| {
                    for opt in [
                        SmoothingType::Adaptive,
                        SmoothingType::None,
                        SmoothingType::Simple,
                        SmoothingType::Weighted,
                        SmoothingType::Stabilizer,
                    ] {
                        let resp = ui.selectable_value(
                            &mut smoothing_options.kind,
                            opt,
                            opt.label(),
                        );
                        resp.on_hover_text(opt.tooltip());
                    }
                });
                // Per-mode controls. Only the relevant rows render so
                // the popup stays compact.
                match smoothing_options.kind {
                    SmoothingType::Adaptive | SmoothingType::None | SmoothingType::Simple => {
                        // Nothing to tune — Adaptive is driven by the
                        // SmoothingLevel chips above; None/Simple have
                        // no parameters in Krita either.
                    }
                    SmoothingType::Weighted => {
                        ui.add(
                            egui::Slider::new(&mut smoothing_options.smoothness_distance, 3.0..=80.0)
                                .text("Distance px"),
                        ).on_hover_text("Gaussian sigma · 3 — bigger = smoother + laggier");
                        ui.add(
                            egui::Slider::new(&mut smoothing_options.tail_aggressiveness, 0.0..=1.0)
                                .text("Tail aggr."),
                        ).on_hover_text("Penalises rising pressure to tighten lift-off");
                        ui.checkbox(&mut smoothing_options.smooth_pressure, "Smooth pressure");
                    }
                    SmoothingType::Stabilizer => {
                        ui.add(
                            egui::Slider::new(&mut smoothing_options.smoothness_distance, 3.0..=80.0)
                                .text("Sample size"),
                        ).on_hover_text("Queue depth — bigger = more rope-pull lag");
                        ui.checkbox(&mut smoothing_options.use_delay_distance, "Use delay distance");
                        if smoothing_options.use_delay_distance {
                            ui.add(
                                egui::Slider::new(&mut smoothing_options.delay_distance, 1.0..=80.0)
                                    .text("Delay px"),
                            ).on_hover_text("Cursor must move farther than this before ink commits");
                        }
                        ui.checkbox(&mut smoothing_options.finish_stabilized_curve,
                                    "Finish stabilized curve");
                        ui.checkbox(&mut smoothing_options.stabilize_sensors,
                                    "Stabilize sensors (pressure, tilt)");
                    }
                }
                ui.add_space(4.0);
                ui.checkbox(&mut smoothing_options.fan_corners, "Fan corners (render)");
                if smoothing_options.fan_corners {
                    ui.add(
                        egui::Slider::new(&mut smoothing_options.fan_corners_step, 0.05..=0.80)
                            .text("Corner step (rad)"),
                    );
                }
            });
            ui.add_space(6.0);
            // Pressure curve row.
            //
            // Slider maps raw pressure → effective width via
            // `raw.powf(curve)`. 0.5 = lighter touch (heavy users
            // squash less); 2.0 = heavier touch (light users get a
            // sharper taper). 1.0 leaves pressure linear (default).
            ui.vertical(|ui| {
                ui.label(
                    egui::RichText::new("Pressure curve")
                        .small()
                        .color(theme::TEXT_MUTED),
                );
                let slider = egui::Slider::new(pressure_curve, 0.5..=2.0)
                    .step_by(0.05)
                    .show_value(true)
                    .text("");
                let resp = ui.add(slider);
                if resp.hovered() {
                    resp.on_hover_text(
                        "0.5 = lighter touch (low pressure thicker)\n\
                         1.0 = linear\n\
                         2.0 = heavier touch (low pressure thinner)",
                    );
                }
            });
        },
    );

    if response.hovered() {
        interacted = true;
    }
    interacted
}
