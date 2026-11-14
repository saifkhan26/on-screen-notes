//! Floating toolbar: tool buttons + colour palette + size slider with
//! live preview. Anchored top-centre of the overlay.
//!
//! Icons are bundled SVGs (Lucide set) loaded through `egui_extras` and
//! tinted at render time. SVGs scale crisply at any DPI, are easy to swap,
//! and ship inside the binary via `include_image!`.

use crate::canvas::shape::{BorderStyle, MediaType};
use crate::canvas::smoothing::{SmoothingOptions, SmoothingType};
use crate::canvas::stroke::StrokeStyle;
use crate::config::SmoothingLevel;
use crate::input::hotkeys_config::{Binding, ParsedHotkeys};
use crate::tools::beside::{BesideFit, BesideSide};
use crate::tools::{ActiveTool, ToolKind};
use crate::ui::media_filter::{self, MediaFilter};
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
    // Shape snap on / off, in the settings popup.
    shape_snap: &mut bool,
    // New media beside the selected picture: what it matches and which
    // side, in the settings popup.
    beside: (&mut BesideFit, &mut BesideSide),
    pinned_hwnd: &mut Option<isize>,
    pin_picking: &mut bool,
    // Set when the "Insert video / GIF" button is clicked.
    insert_media: &mut bool,
    media_filter: &mut MediaFilter,
    // Pictures / GIFs / videos on the slide, for the filter popup.
    media_counts: [usize; 3],
    // Set when the filter's "Select these" is clicked.
    select_filtered: &mut bool,
    // Whether pins are hidden; `None` when there are no pins, which
    // hides the button.
    pins_hidden: Option<&mut bool>,
    // Text labels are selected: the text-box switch shows for them.
    text_selected: bool,
    // The hotkeys as bound, for the help panel.
    keys: &ParsedHotkeys,
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
                    // Contextual, like the dashed-border switch: with the
                    // Text tool, or with labels selected.
                    if tool.kind == ToolKind::Text || text_selected {
                        interacted |= text_box_btn(ui, tool, keys.toolbar.text_box);
                    }
                    interacted |= tool_btn(ui, tool, ToolKind::Spotlight,     "Spotlight — S  (dim everything except a square around the cursor; wheel resizes)");
                    interacted |= tool_btn(ui, tool, ToolKind::LassoErase,    "Lasso erase — X  (draw a closed loop, or click corner by corner for a polygon and close it on the first corner, with a double click or Enter; ink whose centroid lands inside is removed)");
                    interacted |= tool_btn(ui, tool, ToolKind::LassoSelect,   "Lasso capture — Shift+X  (draw a closed loop, or click corner by corner for a polygon and close it on the first corner, with a double click or Enter; the screen pixels inside become an image on the active layer)");
                    interacted |= tool_btn(ui, tool, ToolKind::Select,        "Select — V  (click an object, Shift+click for more, drag empty space to box-select; handles move / resize / rotate; Delete removes)");
                    spacer(ui);

                    // Group 2: shape tools
                    interacted |= tool_btn(ui, tool, ToolKind::Rect,          "Rectangle — R");
                    interacted |= tool_btn(ui, tool, ToolKind::Ellipse,       "Ellipse — O");
                    let line_tip = keys.tool.line.map_or_else(|| "Line — straight line".to_string(), |b| format!("Line — {}", b.label()));
                    interacted |= tool_btn(ui, tool, ToolKind::Line,          &line_tip);
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

                    // Group 5: utilities (insert media + pin + settings + help)
                    interacted |= insert_media_btn(ui, insert_media);
                    interacted |= filter_btn(ui, media_filter, media_counts, select_filtered);
                    if let Some(hidden) = pins_hidden {
                        interacted |= pins_btn(ui, hidden);
                    }
                    interacted |= pin_btn(ui, pinned_hwnd, pin_picking);
                    interacted |= settings_btn(ui, bg_opacity, bg_color, smoothing, smoothing_options, pressure_curve, shape_snap, (&mut *beside.0, &mut *beside.1));
                    interacted |= help_btn(ui, keys);
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

/// "Insert picture / video / GIF" — opens the Open dialog (the app runs it).
fn insert_media_btn(ui: &mut egui::Ui, clicked: &mut bool) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(BTN_SIZE), egui::Sense::click());
    let bg = if response.hovered() { Color32::from_white_alpha(22) } else { Color32::TRANSPARENT };
    ui.painter().rect_filled(rect, Rounding::same(BTN_ROUND), bg);
    let icon_rect = rect.shrink(rect.width() * 0.22);
    egui::Image::new(egui::include_image!("../../assets/icons/film.svg"))
        .tint(theme::TEXT_PRIMARY)
        .fit_to_exact_size(icon_rect.size())
        .paint_at(ui, icon_rect);
    let response = response.on_hover_text("Insert picture / video / GIF  (or drop a file onto the canvas)");
    if response.clicked() {
        *clicked = true;
        return true;
    }
    response.hovered() || response.is_pointer_button_down_on()
}

/// Show / hide every pin (objects kept on screen). Lit while they
/// are hidden, so a hidden state is never forgotten.
fn pins_btn(ui: &mut egui::Ui, hidden: &mut bool) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(BTN_SIZE), egui::Sense::click());
    let bg = if *hidden {
        theme::ACCENT
    } else if response.hovered() {
        Color32::from_white_alpha(22)
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, Rounding::same(BTN_ROUND), bg);
    let icon = if *hidden {
        egui::include_image!("../../assets/icons/eye-off.svg")
    } else {
        egui::include_image!("../../assets/icons/eye.svg")
    };
    let icon_rect = rect.shrink(rect.width() * 0.22);
    egui::Image::new(icon).tint(theme::TEXT_PRIMARY).fit_to_exact_size(icon_rect.size()).paint_at(ui, icon_rect);
    let tooltip = if *hidden {
        "Pinned objects are hidden — click to show them again (Shift+P)"
    } else {
        "Hide all pins (Shift+P). Their originals stay on their own slides"
    };
    let response = response.on_hover_text(tooltip);
    if response.clicked() {
        *hidden = !*hidden;
        return true;
    }
    response.hovered() || response.is_pointer_button_down_on()
}

/// Media filter: highlights pictures, GIFs and / or videos on the slide.
/// Lit while any type is on; the popup picks the types.
fn filter_btn(ui: &mut egui::Ui, filter: &mut MediaFilter, counts: [usize; 3], select: &mut bool) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(BTN_SIZE), egui::Sense::click());
    let bg = if filter.any() {
        theme::ACCENT
    } else if response.hovered() {
        Color32::from_white_alpha(22)
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, Rounding::same(BTN_ROUND), bg);
    let icon_rect = rect.shrink(rect.width() * 0.24);
    egui::Image::new(egui::include_image!("../../assets/icons/filter.svg"))
        .tint(theme::TEXT_PRIMARY)
        .fit_to_exact_size(icon_rect.size())
        .paint_at(ui, icon_rect);
    let response = response.on_hover_text("Highlight pictures, GIFs or videos");
    let popup_id = ui.make_persistent_id("osn_media_filter_popup");
    let mut interacted = response.hovered() || response.is_pointer_button_down_on();
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
            ui.set_min_width(200.0);
            ui.label(egui::RichText::new("Highlight media").small().color(theme::TEXT_MUTED));
            ui.add_space(2.0);
            for (t, n) in MediaType::ALL.into_iter().zip(counts) {
                ui.horizontal(|ui| {
                    let (dot, _) = ui.allocate_exact_size(Vec2::splat(10.0), egui::Sense::hover());
                    ui.painter().circle_filled(dot.center(), 4.5, media_filter::color(t));
                    ui.toggle_value(filter.flag(t), format!("{}  ({n})", media_filter::label(t)));
                });
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let matching: usize = MediaType::ALL.into_iter().zip(counts).filter(|(t, _)| filter.shows(*t)).map(|(_, n)| n).sum();
                if ui
                    .add_enabled(matching > 0, egui::Button::new("Select these"))
                    .on_hover_text("Select the highlighted objects, ready to arrange")
                    .clicked()
                {
                    *select = true;
                    ui.memory_mut(|m| m.close_popup());
                }
                if ui.add_enabled(filter.any(), egui::Button::new("Clear")).clicked() {
                    *filter = MediaFilter::default();
                }
            });
        },
    );
    interacted
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
        // Reuses the lasso glyph for now; a dedicated lasso-select icon
        // can be dropped into assets/icons/ later.
        ToolKind::LassoSelect   => egui::Image::new(egui::include_image!("../../assets/icons/lasso.svg")),
        ToolKind::Select        => egui::Image::new(egui::include_image!("../../assets/icons/select.svg")),
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
/// listing every hotkey as bound in `hotkeys.toml`, plus the mouse,
/// wheel and held-modifier gestures. Lives in the toolbar so it is
/// always discoverable but never in the way.
fn help_btn(ui: &mut egui::Ui, keys: &ParsedHotkeys) -> bool {
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

    // Rich tooltip on hover. Using `on_hover_ui` (not `on_hover_text`)
    // gives us proper layout control.
    response.on_hover_ui(|ui| help_panel(ui, keys));

    // Hover is *not* a click — never report it as a user interaction so
    // pen events still flow to the canvas.
    false
}

/// One titled group of help rows, each an action and how to do it.
type HelpSection = (&'static str, Vec<(&'static str, String)>);

/// The help tooltip: titled sections of action / keys grids, flowed
/// into as many columns as the screen fits so the list never runs off
/// the bottom of it.
fn help_panel(ui: &mut egui::Ui, keys: &ParsedHotkeys) {
    // Room a column takes, with the gap after it.
    const COL_W: f32 = 400.0;
    let sections = help_sections(keys);
    let screen_w = ui.ctx().screen_rect().width() - 40.0;
    let cols = (screen_w / COL_W).floor().clamp(1.0, 3.0) as usize;
    // A tooltip stops at `spacing.tooltip_width` (500), which would
    // wrap every column past the first.
    ui.set_max_width(screen_w);

    // Fill columns in order, a heading counting as a row. A section
    // starts the next column when more than half of it would spill
    // past an even share.
    let total: usize = sections.iter().map(|(_, rows)| rows.len() + 1).sum();
    let share = total.div_ceil(cols);
    let mut columns: Vec<Vec<&HelpSection>> = vec![Vec::new()];
    let mut filled = 0;
    for s in &sections {
        let n = s.1.len() + 1;
        if filled > 0 && filled + n / 2 > share && columns.len() < cols {
            columns.push(Vec::new());
            filled = 0;
        }
        columns.last_mut().unwrap().push(s);
        filled += n;
    }

    ui.label(
        egui::RichText::new("Keyboard & input shortcuts")
            .strong()
            .size(13.0),
    );
    ui.add_space(4.0);
    // Taller than the screen allows, the columns scroll: a scroll bar
    // makes the tooltip one the pointer can move onto, so it stays open
    // to be scrolled.
    let room = (ui.ctx().screen_rect().height() - 150.0).max(200.0);
    egui::ScrollArea::vertical().max_height(room).auto_shrink([true, true]).show(ui, |ui| {
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 24.0;
        for (ci, column) in columns.iter().enumerate() {
            ui.vertical(|ui| {
                for (si, (title, rows)) in column.iter().enumerate() {
                    if si > 0 {
                        ui.add_space(6.0);
                    }
                    ui.label(egui::RichText::new(*title).strong().color(theme::TEXT_MUTED));
                    egui::Grid::new(("osn_help_grid", ci, si))
                        .num_columns(2)
                        .spacing([18.0, 2.0])
                        .show(ui, |ui| {
                            for (action, how) in rows {
                                ui.label(*action);
                                ui.label(
                                    egui::RichText::new(how)
                                        .monospace()
                                        .color(Color32::from_rgb(232, 200, 200)),
                                );
                                ui.end_row();
                            }
                        });
                }
            });
        }
    });
    });
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new(match keys.canvas.command_palette {
            Some(b) => format!("{}: find any command by name. Keys can be changed in hotkeys.toml, where every button can be given one too.", b.label()),
            None => "Keys can be changed in hotkeys.toml, where every button can be given one too.".to_string(),
        })
            .small()
            .color(theme::TEXT_MUTED),
    );
}

/// Every hotkey as bound (a rebound key shows its new combo, an unbound
/// or unreadable one a dash) and the fixed mouse / modifier gestures.
/// The buttons' own keys, unbound until set, are listed once they are.
fn help_sections(keys: &ParsedHotkeys) -> Vec<HelpSection> {
    let mut sections = fixed_help_sections(keys);
    let (t, tb, a, c) = (&keys.tool, &keys.toolbar, &keys.arrange, &keys.canvas);
    let bound = |rows: Vec<(&'static str, Option<Binding>)>| -> Vec<(&'static str, String)> {
        rows.into_iter().filter_map(|(name, b)| b.map(|b| (name, b.label()))).collect()
    };
    let mut more = [
        ("Toolbar buttons", bound(vec![
            ("Dashed / solid border",    t.dashed_border),
            ("Colour 1",                 tb.colour[0]),
            ("Colour 2",                 tb.colour[1]),
            ("Colour 3",                 tb.colour[2]),
            ("Colour 4",                 tb.colour[3]),
            ("Colour 5",                 tb.colour[4]),
            ("Colour 6",                 tb.colour[5]),
            ("Colour 7",                 tb.colour[6]),
            ("Colour 8",                 tb.colour[7]),
            ("Size up",                  tb.size_up),
            ("Size down",                tb.size_down),
            ("Insert picture / video / GIF", tb.insert_media),
            ("Highlight pictures",       tb.highlight[0]),
            ("Highlight GIFs",           tb.highlight[1]),
            ("Highlight videos",         tb.highlight[2]),
            ("Select the highlighted",   tb.select_highlighted),
            ("Pin to a window / unpin",  tb.pin_to_window),
            ("Background opacity up",    tb.background_up),
            ("Background opacity down",  tb.background_down),
            ("Black background",         tb.reset_background),
            ("Text in a box on / off",   tb.text_box),
        ])),
        ("Arrange (2+ selected)", bound(vec![
            ("Pack",                     a.pack),
            ("Group by type",            a.group_by_type),
            ("Keep apart on / off",      a.keep_apart),
            ("Align left",               a.align_left),
            ("Align centres across",     a.align_center_x),
            ("Align right",              a.align_right),
            ("Align top",                a.align_top),
            ("Align centres down",       a.align_center_y),
            ("Align bottom",             a.align_bottom),
            ("Distribute across",        a.distribute_x),
            ("Distribute down",          a.distribute_y),
            ("Width of the narrowest",   a.match_narrowest_width),
            ("Width of the widest",      a.match_widest_width),
            ("Height of the shortest",   a.match_shortest_height),
            ("Height of the tallest",    a.match_tallest_height),
            ("Move to slide…",           a.move_to_slide),
            ("Flip left to right",       a.flip_horizontal),
            ("Flip top to bottom",       a.flip_vertical),
            ("A picture's colours",      a.picture_colours),
        ])),
        ("Panels & pins", bound(vec![
            ("New slide",                c.new_slide),
            ("Move the slide up",        c.move_slide_up),
            ("Move the slide down",      c.move_slide_down),
            ("Fold / unfold slide panel", c.toggle_slide_panel),
            ("Ctrl+Tab: in order / recent", c.toggle_slide_order),
            ("Slide window open / close", c.slide_window),
            ("New layer",                c.new_layer),
            ("Delete layer (no undo)",   c.delete_layer),
            ("Pin: go to its slide",     c.pin_go_to_slide),
            ("Pin: this slide only / every slide", c.pin_this_slide_only),
            ("Loop video / GIF",         c.media_loop),
            ("Onion skin on / off",      c.media_onion),
        ])),
    ];
    // Both layer moves on one row, when either is bound.
    if c.move_layer_up.is_some() || c.move_layer_down.is_some() {
        let label = |b: Option<Binding>| b.map_or_else(|| "—".to_owned(), |b| b.label());
        more[2].1.push(("Move layer up / down", format!("{} / {}", label(c.move_layer_up), label(c.move_layer_down))));
    }
    sections.extend(more.into_iter().filter(|(_, rows)| !rows.is_empty()));
    sections
}

/// `help_sections`' rows that always show.
fn fixed_help_sections(keys: &ParsedHotkeys) -> Vec<HelpSection> {
    let k = |b: Option<Binding>| b.map_or_else(|| "—".to_owned(), |b| b.label());
    let k2 = |a: Option<Binding>, b: Option<Binding>| format!("{} / {}", k(a), k(b));
    let s = |text: &str| text.to_owned();
    // One key per direction, a modifier they share said once:
    // "Shift+← → ↑ ↓".
    let k4 = |bs: [Option<Binding>; 4]| {
        let mods = |b: Binding| (b.ctrl, b.shift, b.alt);
        let bare = |b: Binding| Binding { ctrl: false, shift: false, alt: false, ..b }.label();
        match bs {
            [Some(first), ..] if bs.iter().all(|b| b.is_some_and(|b| mods(b) == mods(first))) => {
                let full = first.label();
                let prefix = &full[..full.len() - bare(first).len()];
                format!("{prefix}{}", bs.map(|b| b.map_or_else(String::new, bare)).join(" "))
            }
            _ => bs.map(|b| k(b)).join(" "),
        }
    };
    let (t, c, g) = (&keys.tool, &keys.canvas, &keys.global);
    vec![
        ("Tools", vec![
            ("Pen",                         k(t.pen)),
            ("Pencil",                      k(t.pencil)),
            ("Marker (highlighter)",        k(t.marker)),
            ("Airbrush (spray)",            k(t.airbrush)),
            ("Fill",                        k(t.fill)),
            ("Laser pointer",               k(t.laser)),
            ("Spotlight (again: pen)",      k(t.spotlight)),
            ("Text",                        k(t.text)),
            ("Rectangle",                   k(t.rect)),
            ("Ellipse",                     k(t.ellipse)),
            ("Line",                        t.line.map_or_else(|| s("toolbar (or bind tool.line)"), |b| b.label())),
            ("Straight arrow",              k(t.arrow)),
            ("Freehand arrow",              k(t.freehand_arrow)),
            ("Lasso erase",                 k(t.lasso)),
            ("Lasso capture (screen to picture)", k(t.lasso_select)),
            ("Select",                      k(t.select)),
            ("Eyedropper (one pick)",       k(t.eyedropper)),
            ("Magnifier loupe",             s("hold Z (wheel zooms)")),
        ]),
        ("Text & lasso", vec![
            ("Text: commit / new line",     s("Enter / Shift+Enter")),
            ("Lasso polygon",               s("click corner by corner")),
            ("  close it",                  s("Enter / double click")),
            ("  take back a corner",        s("Backspace")),
            ("Cancel what is in progress",  s("Esc")),
        ]),
        ("Selection (Select tool)", vec![
            ("Add / remove; one of a group", s("Shift+click; Alt+click")),
            ("Group / ungroup",             k2(c.group, c.ungroup)),
            ("Box select",                  s("drag on empty space")),
            ("Move / resize / rotate",      s("drag / handles / knob")),
            ("Nudge 1 px",                  k4(c.nudge_1px)),
            ("Nudge 10 px",                 k4(c.nudge_10px)),
            ("Rotate in 15° steps",         s("hold Shift")),
            ("Delete",                      format!("{} / Backspace", k(c.delete_selection))),
            ("Duplicate",                   k(c.duplicate)),
            ("Copy / cut / paste (pictures too)", s("Ctrl+C / X / V")),
            ("To front / to back",          k2(c.bring_front, c.send_back)),
            ("Copy to a new slide",         k(c.copy_to_new_slide)),
            ("Pin / unpin; hide all pins",  format!("{}; {}", k(c.pin_selection), k(c.toggle_pins))),
            ("Arrange, flip, a picture's colours", s("bar under the toolbar")),
            ("Recolour / resize",           s("palette / size slider")),
        ]),
        ("Pictures, videos & GIFs", vec![
            ("Insert",                      s("film button / drop a file")),
            ("Play / pause",                k(c.media_play_pause)),
            ("Previous / next frame",       k2(c.media_prev_frame, c.media_next_frame)),
            ("Highlight by type",           s("funnel button")),
        ]),
        ("Slides", vec![
            ("Previous / next (adds one at the end)", k2(c.prev_canvas, c.next_canvas)),
            ("Previous / next, wrapping",   k2(c.cycle_prev_slide, c.cycle_next_slide)),
            ("First / last",                k2(c.first_slide, c.last_slide)),
            ("Name the slide",              k(c.rename_slide)),
            ("Lock / unlock",               k(c.lock_slide)),
            ("Undo / redo",                 k2(c.undo, c.redo)),
            ("Delete the slide (asks first)", k(c.delete_canvas)),
            ("Zoom to fit / selection / while held", format!("{} / hold {}", k2(c.zoom_to_fit, c.zoom_to_selection), k(c.hold_zoom_to_selection))),
        ]),
        ("Layers (the active one)", vec![
            ("Previous / next layer",       k2(c.prev_layer, c.next_layer)),
            ("Hide / show",                 k(c.toggle_layer_visible)),
            ("Preview",                     k(c.preview_layer)),
            ("Clear",                       k(c.clear_layer)),
            ("Opacity 0 / 10 … 90 %",       format!("{} / {} … {}", k(c.opacity[0]), k(c.opacity[1]), k(c.opacity[9]))),
            ("Opacity −10 / +10 %",         k2(c.opacity_dec, c.opacity_inc)),
            ("Opacity, finely",             s("Shift+Scroll")),
            ("Pan",                         s("Shift+Space+drag")),
            ("Zoom",                        s("Shift+Space+Scroll")),
            ("Reset position",              k(c.reset_layer_pos)),
            ("Reset zoom",                  k(c.reset_layer_zoom)),
            ("Freeze screen to a new layer", k(c.freeze_frame)),
        ]),
        ("View & input", vec![
            ("Hide / show UI",              k(c.toggle_ui)),
            ("Pan",                         s("Space+drag / middle drag")),
            ("Zoom",                        s("Scroll / Space+Scroll")),
            ("  pen tools, fill, spotlight: size", s("Scroll")),
            ("Reset zoom + pan",            k(c.reset_view)),
            ("Background opacity",          s("Ctrl+Scroll")),
            ("Erase while drawing",         s("hold Ctrl")),
            ("Pick a colour",               s("hold Shift + tap")),
            ("Recolour a swatch",           s("right-click it")),
            ("Pin to a window",             s("pin button")),
        ]),
        ("Global (from any app)", vec![
            ("Show / hide overlay",         k(g.toggle_overlay)),
            ("Screenshot (bg + ink)",       k(g.screenshot)),
            ("Start / stop recording",      k(g.start_record)),
            ("Pause / resume recording",    k(g.pause_record)),
        ]),
    ]
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

/// Text-in-a-box switch: flips `tool.text_box`, which the Text tool
/// reads when it places a label (and the app applies to selected
/// labels).
fn text_box_btn(ui: &mut egui::Ui, tool: &mut ActiveTool, key: Option<Binding>) -> bool {
    let on = tool.text_box;
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(BTN_SIZE), egui::Sense::click());
    let bg = if on {
        theme::ACCENT
    } else if response.hovered() {
        Color32::from_white_alpha(22)
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, Rounding::same(BTN_ROUND), bg);
    let icon = rect.shrink(rect.width() * 0.22);
    egui::Image::new(egui::include_image!("../../assets/icons/text-box.svg"))
        .tint(theme::TEXT_PRIMARY)
        .fit_to_exact_size(icon.size())
        .paint_at(ui, icon);
    let tip = if on {
        "Text in a box ON — labels get a box that keeps them readable (click for plain text; applies to selected labels too)"
    } else {
        "Text in a box OFF — click to put labels in a box that keeps them readable (applies to selected labels too)"
    };
    let tip = match key {
        Some(b) => format!("{tip} — {}", b.label()),
        None => tip.to_string(),
    };
    let response = response.on_hover_text(tip);
    if response.clicked() {
        tool.text_box = !tool.text_box;
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
#[allow(clippy::too_many_arguments)]
fn settings_btn(
    ui: &mut egui::Ui,
    bg_opacity: &mut f32,
    bg_color: &mut [u8; 3],
    smoothing: &mut SmoothingLevel,
    smoothing_options: &mut SmoothingOptions,
    pressure_curve: &mut f32,
    shape_snap: &mut bool,
    (beside_fit, beside_side): (&mut BesideFit, &mut BesideSide),
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
            ui.add_space(6.0);
            ui.checkbox(shape_snap, "Shape snap").on_hover_text(
                "Hold the pen still at the end of a stroke and it snaps to the line, \
                 triangle, rectangle, polygon, circle or ellipse it was meant to be",
            );
            ui.add_space(6.0);
            // Pills, not a ComboBox: see the smoothing mode row.
            ui.vertical(|ui| {
                ui.label(
                    egui::RichText::new("New picture beside the selected one")
                        .small()
                        .color(theme::TEXT_MUTED),
                );
                let about = "With a picture, GIF or video selected, one you drop, paste or insert \
                             comes in at its height (or width) and lands right beside it, past any \
                             already there. With nothing like that selected, or Off, new ones come \
                             in at their own size where they fall";
                ui.horizontal(|ui| {
                    for f in BesideFit::ALL {
                        ui.selectable_value(beside_fit, f, f.label()).on_hover_text(about);
                    }
                });
                if *beside_fit != BesideFit::Off {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("on the").small().color(theme::TEXT_MUTED));
                        for s in BesideSide::ALL {
                            ui.selectable_value(beside_side, s, s.label());
                        }
                    });
                }
            });
        },
    );

    if response.hovered() {
        interacted = true;
    }
    interacted
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::hotkeys_config::HotkeysConfig;

    /// Size the help panel lays out to on a screen of `w` × `h` points.
    fn help_panel_size(w: f32, h: f32) -> Vec2 {
        let keys = HotkeysConfig::default().parsed();
        let ctx = egui::Context::default();
        crate::ui::theme::apply(&ctx);
        let mut size = Vec2::ZERO;
        // A second frame, so the grids lay out with last frame's widths.
        for _ in 0..2 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(w, h))),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                egui::Area::new(egui::Id::new("help")).show(ctx, |ui| {
                    // As `on_hover_ui` hands it over.
                    ui.set_max_width(ui.spacing().tooltip_width);
                    size = ui.scope(|ui| help_panel(ui, &keys)).response.rect.size();
                });
            });
        }
        size
    }

    #[test]
    fn help_panel_fits_the_screen() {
        // 1080p at 100 % and 150 % scaling, a 768-tall laptop, and a
        // short window, where the columns scroll.
        for (w, h) in [(1920.0, 1080.0), (1280.0, 720.0), (1366.0, 768.0), (1280.0, 560.0)] {
            let size = help_panel_size(w, h);
            assert!(size.x < w && size.y < h - 60.0, "{size:?} on {w}x{h}");
        }
    }

    #[test]
    fn help_panel_text_has_glyphs() {
        // The default proportional font has no arrows, so "→" in an
        // action would show as a box.
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |_| {});
        let body = egui::FontId::proportional(14.0);
        let mono = egui::FontId::monospace(14.0);
        for (title, rows) in help_sections(&HotkeysConfig::default().parsed()) {
            let texts = std::iter::once((title.to_owned(), &body))
                .chain(rows.into_iter().flat_map(|(a, k)| [(a.to_owned(), &body), (k, &mono)]));
            for (text, font) in texts {
                assert!(ctx.fonts(|f| f.has_glyphs(font, &text)), "no glyph for part of {text:?}");
            }
        }
    }
}
