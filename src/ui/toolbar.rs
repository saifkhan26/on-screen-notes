//! Floating toolbar: tool buttons + colour palette + size slider with
//! live preview, then the utilities and the settings. Anchored
//! top-centre of the overlay.
//!
//! Icons are bundled SVGs (Lucide set) loaded through `egui_extras` and
//! tinted at render time. SVGs scale crisply at any DPI, are easy to swap,
//! and ship inside the binary via `include_image!`.
//!
//! Buttons are grouped by what they do — drawing, marking, selecting,
//! shapes, colour, size, media, settings — with a thin rule between
//! groups. The tool in use is the one solid light chip; switches that
//! are on are a quieter grey. Every tooltip names the key that presses
//! the button, as bound in `hotkeys.toml`.

use crate::canvas::shape::{BorderStyle, MediaType};
use crate::canvas::smoothing::{SmoothingOptions, SmoothingType};
use crate::canvas::stroke::StrokeStyle;
use crate::config::SmoothingLevel;
use crate::input::hotkeys_config::{Binding, ParsedHotkeys};
use crate::tools::beside::{BesideFit, BesideSide};
use crate::tools::{ActiveTool, ToolKind};
use crate::ui::media_filter::{self, MediaFilter};
use crate::ui::theme::{self, Look};
use egui::{Color32, Rounding, Stroke, Vec2};

/// The settings popup's id: global, so a test can open it.
pub const SETTINGS_POPUP: &str = "osn_settings_popup";

/// Render the toolbar. Returns `true` if the user interacted with any
/// element this frame.
#[allow(clippy::too_many_arguments)]
pub fn show(
    ctx: &egui::Context,
    tool: &mut ActiveTool,
    bg_opacity: &mut f32,
    bg_color: &mut [u8; 3],
    palette: &mut [[u8; 4]],
    smoothing: &mut SmoothingLevel,
    smoothing_options: &mut SmoothingOptions,
    pressure_curve: &mut f32,
    // How big the UI is drawn (`config.ui_scale`), and its accent
    // colour (`config.accent`), in the settings popup.
    ui_scale: &mut f32,
    accent: &mut [u8; 3],
    // Shape snap, smart guides and keep proportions on / off, in the
    // settings popup.
    snaps: (&mut bool, &mut bool, &mut bool),
    // New media beside the selected picture: what it matches and which
    // side, in the settings popup.
    beside: (&mut BesideFit, &mut BesideSide),
    // Picture search on / off and where it is, then the browser link on /
    // off, where it is, and "Disconnect all" (set when clicked), in the
    // settings popup.
    services: (&mut bool, &str, &mut bool, &str, &mut bool),
    pinned_hwnd: &mut Option<isize>,
    pin_picking: &mut bool,
    // Set when the "Insert video / GIF" button is clicked.
    insert_media: &mut bool,
    // Set when the close button (the bar's last) is clicked.
    quit: &mut bool,
    media_filter: &mut MediaFilter,
    // Pictures / GIFs / videos on the slide, for the filter popup.
    media_counts: &media_filter::Counts,
    // Set when the filter's "Select these" is clicked.
    select_filtered: &mut bool,
    // Whether pins are hidden; `None` when there are no pins, which
    // hides the button.
    pins_hidden: Option<&mut bool>,
    // Text labels are selected: the text-box switch shows for them.
    text_selected: bool,
    // The hotkeys as bound, for the tooltips and the help panel.
    keys: &ParsedHotkeys,
) -> bool {
    let mut interacted = false;

    egui::Area::new(egui::Id::new("osn_toolbar"))
        .anchor(egui::Align2::CENTER_TOP, Vec2::new(0.0, 10.0))
        .show(ctx, |ui| {
            theme::card_frame().show(ui, |ui| {
                // One row on the overlay; in a narrower window the
                // buttons wrap onto more rows.
                ui.set_max_width((ctx.screen_rect().width() - 40.0).max(200.0));
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(2.0, 4.0);
                    let t = &keys.tool;

                    // Drawing. Pen, pencil, marker and airbrush are one
                    // `ToolKind::Pen`, told apart by `tool.style`. The
                    // eraser has no button: hold Ctrl while drawing.
                    interacted |= pen_style_btn(ui, tool, StrokeStyle::Default, t.pen, "Smooth ink that follows the pen's pressure. Hold Ctrl to erase");
                    interacted |= pen_style_btn(ui, tool, StrokeStyle::Pencil, t.pencil, "Grainy, see-through pencil");
                    interacted |= pen_style_btn(ui, tool, StrokeStyle::Marker, t.marker, "See-through highlighter: where strokes overlap, they darken");
                    interacted |= pen_style_btn(ui, tool, StrokeStyle::Airbrush, t.airbrush, "Soft spray: hold still to build it up");
                    interacted |= tool_btn(ui, tool, ToolKind::Fill, t.fill, "Fill a closed area with the colour");
                    theme::divider(ui, BTN_SIZE);

                    // Marking up: text, and the pointers that leave nothing.
                    interacted |= tool_btn(ui, tool, ToolKind::Text, t.text, "Click to place a label. Enter keeps it, Shift+Enter starts a new line");
                    // Contextual: with the Text tool, or with labels selected.
                    if tool.kind == ToolKind::Text || text_selected {
                        interacted |= text_box_btn(ui, tool, keys.toolbar.text_box);
                    }
                    interacted |= tool_btn(ui, tool, ToolKind::Laser, t.laser, "A pointer trail that fades once the pen lifts");
                    interacted |= tool_btn(ui, tool, ToolKind::Spotlight, t.spotlight, "Dims all but a square round the pointer; the wheel resizes it. Its key again goes back to the pen");
                    theme::divider(ui, BTN_SIZE);

                    // Selecting.
                    interacted |= tool_btn(ui, tool, ToolKind::Select, t.select, "Click an object, Shift+click for more, drag on empty space for a box. The handles move, resize and turn it; Delete removes it");
                    interacted |= tool_btn(ui, tool, ToolKind::LassoErase, t.lasso, "Draw a loop — or click corner by corner, closing it with a double click or Enter — and the ink inside goes");
                    interacted |= tool_btn(ui, tool, ToolKind::LassoSelect, t.lasso_select, "Draw a loop round part of the screen: what is inside becomes a picture on the active layer");
                    theme::divider(ui, BTN_SIZE);

                    // Shapes.
                    interacted |= tool_btn(ui, tool, ToolKind::Rect, t.rect, "Drag out a rectangle");
                    interacted |= tool_btn(ui, tool, ToolKind::Ellipse, t.ellipse, "Drag out an ellipse");
                    interacted |= tool_btn(ui, tool, ToolKind::Line, t.line, "A straight line");
                    interacted |= tool_btn(ui, tool, ToolKind::Arrow, t.arrow, "A straight arrow");
                    interacted |= tool_btn(ui, tool, ToolKind::FreehandArrow, t.freehand_arrow, "Draw a path; an arrowhead goes on its end");
                    // Contextual: the shapes that can have a dashed border.
                    if matches!(tool.kind, ToolKind::Rect | ToolKind::Ellipse | ToolKind::Line | ToolKind::Arrow) {
                        interacted |= dash_toggle_btn(ui, tool, t.dashed_border);
                    }
                    theme::divider(ui, BTN_SIZE);

                    // Colours.
                    ui.spacing_mut().item_spacing.x = 0.0;
                    for (i, slot) in palette.iter_mut().enumerate() {
                        let (new_slot, hit) = colour_swatch(ui, &mut tool.color, *slot, i, keys.toolbar.colour.get(i).copied().flatten());
                        *slot = new_slot;
                        interacted |= hit;
                    }
                    ui.spacing_mut().item_spacing.x = 2.0;
                    theme::divider(ui, BTN_SIZE);

                    // Size.
                    interacted |= size_widget(ui, tool, keys);
                    theme::divider(ui, BTN_SIZE);

                    // Media, pins, the window pin.
                    interacted |= insert_media_btn(ui, insert_media, keys.toolbar.insert_media);
                    interacted |= filter_btn(ui, media_filter, media_counts, select_filtered);
                    if let Some(hidden) = pins_hidden {
                        interacted |= pins_btn(ui, hidden);
                    }
                    interacted |= pin_btn(ui, pinned_hwnd, pin_picking, keys.toolbar.pin_to_window);
                    theme::divider(ui, BTN_SIZE);

                    // Settings and the keys.
                    let settings = Settings {
                        bg_opacity,
                        bg_color,
                        smoothing,
                        smoothing_options,
                        pressure_curve,
                        ui_scale,
                        accent,
                        shape_snap: snaps.0,
                        smart_guides: snaps.1,
                        keep_proportions: snaps.2,
                        beside_fit: beside.0,
                        beside_side: beside.1,
                        picture_search: services.0,
                        picture_status: services.1,
                        browser_link: services.2,
                        browser_status: services.3,
                        forget_browsers: services.4,
                        keys,
                    };
                    interacted |= settings_btn(ui, settings);
                    interacted |= help_btn(ui, keys);
                    theme::divider(ui, BTN_SIZE);

                    // Closing the app, apart at the end.
                    interacted |= close_btn(ui, quit);
                });
            });
        });

    interacted
}

/// A bar button's side, points.
pub(crate) const BTN_SIZE: f32 = 30.0;
/// A colour swatch's cell: narrower than a button, as tall.
const SWATCH_W: f32 = 22.0;

/// A tooltip naming `name` and its `key`, then what it does.
fn tip(response: egui::Response, name: &str, key: Option<Binding>, about: &str) -> egui::Response {
    theme::tip(response, name, key, (!about.is_empty()).then_some(about))
}

/// `response` as a bar button's: clicked, or just under the pointer (a
/// press there is the UI's, not ink).
fn touched(response: &egui::Response) -> bool {
    response.hovered() || response.is_pointer_button_down_on()
}

/// The icon a pen style shows.
fn pen_icon(style: StrokeStyle) -> egui::Image<'static> {
    match style {
        StrokeStyle::Default => egui::Image::new(egui::include_image!("../../assets/icons/pen.svg")),
        StrokeStyle::Pencil => egui::Image::new(egui::include_image!("../../assets/icons/pencil.svg")),
        StrokeStyle::Marker => egui::Image::new(egui::include_image!("../../assets/icons/marker.svg")),
        StrokeStyle::Airbrush => egui::Image::new(egui::include_image!("../../assets/icons/spray-can.svg")),
    }
}

/// Pen / Pencil / Marker / Airbrush button. All select `ToolKind::Pen`;
/// they differ only in `tool.style`, which the renderer uses to switch
/// between the smooth opaque ribbon, the grainy pencil, the highlighter
/// and the spray. Current when the pen tool is selected *and* its style
/// matches `style`.
fn pen_style_btn(ui: &mut egui::Ui, tool: &mut ActiveTool, style: StrokeStyle, key: Option<Binding>, about: &str) -> bool {
    let current = tool.kind == ToolKind::Pen && tool.style == style;
    let look = if current { Look::Current } else { Look::Plain };
    let response = theme::icon_button(ui, pen_icon(style), BTN_SIZE, look);
    let name = tool_name(ToolKind::Pen, style);
    let response = tip(response, name, key, about);
    if response.clicked() {
        tool.kind = ToolKind::Pen;
        tool.style = style;
        return true;
    }
    touched(&response)
}

fn tool_btn(ui: &mut egui::Ui, tool: &mut ActiveTool, kind: ToolKind, key: Option<Binding>, about: &str) -> bool {
    let look = if tool.kind == kind { Look::Current } else { Look::Plain };
    let response = theme::icon_button(ui, tool_icon(kind), BTN_SIZE, look);
    let name = tool_name(kind, tool.style);
    let response = tip(response, name, key, about);
    if response.clicked() {
        tool.kind = kind;
        return true;
    }
    touched(&response)
}

/// "Insert picture / video / GIF" — opens the Open dialog (the app runs it).
fn insert_media_btn(ui: &mut egui::Ui, clicked: &mut bool, key: Option<Binding>) -> bool {
    let response = theme::icon_button(ui, egui::Image::new(egui::include_image!("../../assets/icons/film.svg")), BTN_SIZE, Look::Plain);
    let response = tip(response, "Insert", key, "A picture, video, GIF or PureRef board — or drop a file onto the canvas");
    if response.clicked() {
        *clicked = true;
        return true;
    }
    touched(&response)
}

/// Show / hide every pin (objects kept on screen). Lit while they
/// are hidden, so a hidden state is never forgotten.
fn pins_btn(ui: &mut egui::Ui, hidden: &mut bool) -> bool {
    let icon = if *hidden {
        egui::Image::new(egui::include_image!("../../assets/icons/eye-off.svg"))
    } else {
        egui::Image::new(egui::include_image!("../../assets/icons/eye.svg"))
    };
    let response = theme::icon_button(ui, icon, BTN_SIZE, Look::on(*hidden));
    let response = if *hidden {
        tip(response, "Pins hidden", None, "Click to show them again")
    } else {
        tip(response, "Hide pins", None, "Their originals stay on their own slides")
    };
    if response.clicked() {
        *hidden = !*hidden;
        return true;
    }
    touched(&response)
}

/// Media filter: highlights pictures, GIFs and / or videos on the slide,
/// its groups, and what is tagged. Lit while any is on; the popup picks.
fn filter_btn(ui: &mut egui::Ui, filter: &mut MediaFilter, counts: &media_filter::Counts, select: &mut bool) -> bool {
    let response = theme::icon_button(ui, egui::Image::new(egui::include_image!("../../assets/icons/filter.svg")), BTN_SIZE, Look::on(filter.lit()));
    let response = tip(response, "Highlight", None, "Pictures, GIFs, videos, groups or tags on this slide, and duplicates just added");
    let popup_id = egui::Id::new("osn_media_filter_popup");
    let mut interacted = touched(&response);
    if response.clicked() {
        ui.memory_mut(|m| m.toggle_popup(popup_id));
        interacted = true;
    }
    theme::popup_below(ui, popup_id, &response, true, |ui| {
        ui.set_width(260.0);
        ui.spacing_mut().item_spacing.y = 4.0;
        // A row: the highlight's colour, what it is, how many, a switch.
        let row = |ui: &mut egui::Ui, colour: Color32, on: &mut bool, label: &str, n: usize| -> bool {
            ui.horizontal(|ui| {
                let (dot, _) = ui.allocate_exact_size(Vec2::splat(10.0), egui::Sense::hover());
                ui.painter().circle_filled(dot.center(), 4.0, colour);
                ui.label(label);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let changed = theme::switch(ui, on).changed();
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(n.to_string()).color(theme::TEXT_MUTED));
                    changed
                })
                .inner
            })
            .inner
        };
        // Pictures just pasted or dropped that are copies: their rings
        // stay until switched off here (or the bar's ×).
        if let Some(n) = counts.duplicates {
            theme::section(ui, "Just added");
            row(ui, crate::ui::find_bar::FOUND, &mut filter.duplicates, "Duplicates", n);
        }
        theme::section(ui, "Media");
        for (t, n) in MediaType::ALL.into_iter().zip(counts.media) {
            row(ui, media_filter::color(t), filter.flag(t), media_filter::label(t), n);
        }
        theme::section(ui, "Groups and tags");
        row(ui, media_filter::GROUP_COLOUR, &mut filter.groups, "Groups", counts.groups);
        row(ui, media_filter::TAG_COLOUR, &mut filter.tagged, "All tagged", counts.tagged);
        // Each tag on this slide, and any picked that is not on it, so
        // it can be put back.
        let mut tags = counts.tags.clone();
        for t in &filter.tags {
            if !tags.iter().any(|(have, _)| crate::canvas::units::same_tag(have, t)) {
                tags.push((t.clone(), 0));
            }
        }
        if tags.is_empty() {
            ui.label(egui::RichText::new("Nothing here is tagged — select something, then the arrange bar's tag button").small().color(theme::TEXT_MUTED));
        } else {
            egui::ScrollArea::vertical().max_height(180.0).auto_shrink([false, true]).show(ui, |ui| {
                for (tag, n) in &tags {
                    let mut on = filter.has_tag(tag);
                    if row(ui, media_filter::TAG_COLOUR, &mut on, &format!("#{tag}"), *n) {
                        filter.toggle_tag(tag);
                    }
                }
            });
        }
        // A slide opened from a PureRef board: what the board has
        // dropped since (a resync keeps it on the slide).
        if let Some(gone) = counts.gone {
            theme::section(ui, "PureRef board");
            row(ui, media_filter::GONE_COLOUR, &mut filter.gone, "Gone from the board", gone);
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let media: usize = MediaType::ALL.into_iter().zip(counts.media).filter(|(t, _)| filter.shows(*t)).map(|(_, n)| n).sum();
            let picked: usize = tags.iter().filter(|(t, _)| filter.has_tag(t)).map(|(_, n)| n).sum();
            let matching = media
                + if filter.groups { counts.groups } else { 0 }
                + if filter.tagged { counts.tagged } else { picked }
                + if filter.gone { counts.gone.unwrap_or(0) } else { 0 };
            if ui
                .add_enabled(matching > 0, egui::Button::new(format!("Select {matching}")))
                .on_hover_text("Select the highlighted objects, ready to arrange")
                .clicked()
            {
                *select = true;
                ui.memory_mut(|m| m.close_popup());
            }
            if ui
                .add_enabled(filter.lit(), egui::Button::new("Unhighlight all"))
                .on_hover_text("Turn every highlight off, the duplicates' rings too")
                .clicked()
            {
                *filter = MediaFilter::default();
            }
        });
    });
    interacted
}

/// The tool in use, lit like a picked toolbar button, for a bar that
/// shows only it (the slide window's): the pen's style shows as its own
/// icon. Shows only; the tool keys pick tools.
pub(crate) fn active_tool(ui: &mut egui::Ui, tool: &ActiveTool) {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(BTN_SIZE), egui::Sense::hover());
    ui.painter().rect_filled(rect, Rounding::same(theme::CONTROL_ROUNDING), theme::accent());
    let icon = egui::Rect::from_center_size(rect.center(), Vec2::splat((BTN_SIZE * 0.56).round()));
    let img = if tool.kind == ToolKind::Pen { pen_icon(tool.style) } else { tool_icon(tool.kind) };
    img.tint(theme::accent_ink()).fit_to_exact_size(icon.size()).paint_at(ui, icon);
    response.on_hover_text(format!("{} — the tool keys pick another", tool_name(tool.kind, tool.style)));
}

/// A tool's name, as its toolbar button calls it (`style` names the pen).
fn tool_name(kind: ToolKind, style: StrokeStyle) -> &'static str {
    match kind {
        ToolKind::Pen => match style {
            StrokeStyle::Default => "Pen",
            StrokeStyle::Pencil => "Pencil",
            StrokeStyle::Marker => "Marker",
            StrokeStyle::Airbrush => "Airbrush",
        },
        ToolKind::Eraser => "Eraser",
        ToolKind::Rect => "Rectangle",
        ToolKind::Ellipse => "Ellipse",
        ToolKind::Line => "Line",
        ToolKind::Arrow => "Arrow",
        ToolKind::FreehandArrow => "Freehand arrow",
        ToolKind::Fill => "Fill",
        ToolKind::Laser => "Laser",
        ToolKind::Text => "Text",
        ToolKind::Spotlight => "Spotlight",
        ToolKind::LassoErase => "Lasso erase",
        ToolKind::LassoSelect => "Lasso capture",
        ToolKind::Select => "Select",
    }
}

/// A tool's icon. The SVGs are baked into the binary by
/// `egui::include_image!`; their strokes are white, and `Image::tint`
/// darkens them on the light chip of the tool in use.
fn tool_icon(kind: ToolKind) -> egui::Image<'static> {
    match kind {
        ToolKind::Pen => egui::Image::new(egui::include_image!("../../assets/icons/pen.svg")),
        ToolKind::Eraser => egui::Image::new(egui::include_image!("../../assets/icons/eraser.svg")),
        ToolKind::Rect => egui::Image::new(egui::include_image!("../../assets/icons/rect.svg")),
        ToolKind::Ellipse => egui::Image::new(egui::include_image!("../../assets/icons/ellipse.svg")),
        ToolKind::Line => egui::Image::new(egui::include_image!("../../assets/icons/line.svg")),
        ToolKind::Arrow => egui::Image::new(egui::include_image!("../../assets/icons/arrow.svg")),
        ToolKind::FreehandArrow => egui::Image::new(egui::include_image!("../../assets/icons/freehand_arrow.svg")),
        ToolKind::Fill => egui::Image::new(egui::include_image!("../../assets/icons/fill.svg")),
        ToolKind::Laser => egui::Image::new(egui::include_image!("../../assets/icons/laser.svg")),
        ToolKind::Text => egui::Image::new(egui::include_image!("../../assets/icons/text.svg")),
        ToolKind::Spotlight => egui::Image::new(egui::include_image!("../../assets/icons/spotlight.svg")),
        ToolKind::LassoErase => egui::Image::new(egui::include_image!("../../assets/icons/lasso.svg")),
        // Reuses the lasso glyph for now; a dedicated lasso-select icon
        // can be dropped into assets/icons/ later.
        ToolKind::LassoSelect => egui::Image::new(egui::include_image!("../../assets/icons/lasso.svg")),
        ToolKind::Select => egui::Image::new(egui::include_image!("../../assets/icons/select.svg")),
    }
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
/// `active` is the drawing colour: a click sets it, and editing the
/// slot that is the drawing colour changes it too.
pub(crate) fn colour_swatch(
    ui: &mut egui::Ui,
    active: &mut [u8; 4],
    color: [u8; 4],
    index: usize,
    key: Option<Binding>,
) -> ([u8; 4], bool) {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(SWATCH_W, BTN_SIZE), egui::Sense::click_and_drag());
    let mut current = color;
    let c = Color32::from_rgba_unmultiplied(color[0], color[1], color[2], color[3]);
    let selected = *active == color;

    // A round dot at the colour; the picked one gets a white ring with
    // a gap, a hovered one a faint ring.
    let painter = ui.painter();
    let centre = rect.center();
    painter.circle_filled(centre, 7.0, c);
    // A dark dot on the dark card: a hairline keeps it visible.
    if c.r().max(c.g()).max(c.b()) < 60 {
        painter.circle_stroke(centre, 7.0, Stroke::new(1.0, theme::HAIRLINE));
    }
    if selected {
        painter.circle_stroke(centre, 9.75, Stroke::new(1.5, theme::TEXT_PRIMARY));
    } else if response.hovered() {
        painter.circle_stroke(centre, 9.75, Stroke::new(1.0, Color32::from_white_alpha(90)));
    }

    let popup_id = egui::Id::new(("osn_palette_popup", index));
    let response = response.on_hover_ui(|ui| {
        theme::tip_body(ui, &format!("Colour {}", index + 1), key, Some(&format!("#{:02X}{:02X}{:02X} · right-click to change it", color[0], color[1], color[2])));
    });

    let mut interacted = false;
    if response.clicked() {
        *active = color;
        interacted = true;
    }
    if response.secondary_clicked() {
        ui.memory_mut(|m| m.toggle_popup(popup_id));
        interacted = true;
    }

    // Popup with the egui colour picker, anchored under the swatch. The
    // user mutates `c32`; we mirror the change back into the [u8;4] slot.
    egui::popup::popup_below_widget(ui, popup_id, &response, egui::popup::PopupCloseBehavior::CloseOnClickOutside, |ui: &mut egui::Ui| {
        ui.set_min_width(220.0);
        theme::section(ui, &format!("Colour {}", index + 1));
        let mut c32 = c;
        let changed = egui::color_picker::color_picker_color32(ui, &mut c32, egui::color_picker::Alpha::Opaque);
        if changed {
            current = c32.to_array();
            // Live-update the active pen colour too if this slot is the
            // currently-selected one — feels more responsive than waiting
            // for the user to re-click after editing.
            if selected {
                *active = current;
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
                        *active = current;
                    }
                    interacted = true;
                }
            }
            if ui.button("Done").clicked() {
                ui.memory_mut(|m| m.close_popup());
            }
        });
    });

    (current, interacted || touched(&response))
}

/// The size: a dot as big as the line (in the drawing colour), a
/// slider, and the size in points.
fn size_widget(ui: &mut egui::Ui, tool: &mut ActiveTool, keys: &ParsedHotkeys) -> bool {
    let (rect, dot) = ui.allocate_exact_size(Vec2::new(24.0, BTN_SIZE), egui::Sense::hover());
    let c = Color32::from_rgba_unmultiplied(tool.color[0], tool.color[1], tool.color[2], tool.color[3]);
    ui.painter().circle_filled(rect.center(), (tool.size * 0.5).clamp(1.5, 10.0), c);
    if c.r().max(c.g()).max(c.b()) < 60 {
        ui.painter().circle_stroke(rect.center(), (tool.size * 0.5).clamp(1.5, 10.0), Stroke::new(1.0, theme::HAIRLINE));
    }
    let about = match (keys.toolbar.size_down, keys.toolbar.size_up) {
        (Some(d), Some(u)) => format!("Pen size; also the wheel over the canvas, or {} / {}", d.label(), u.label()),
        _ => "Pen size; also the wheel over the canvas".to_string(),
    };
    dot.on_hover_ui(|ui| theme::tip_body(ui, "Size", None, Some(&about)));
    let slider = theme::slider(ui, &mut tool.size, 1.0..=80.0, 1.0, 92.0);
    theme::value_label(ui, format!("{:.0}", tool.size), 22.0);
    ui.add_space(4.0);
    slider.changed() || slider.dragged() || slider.hovered()
}

/// Help button — hover-driven. On hover, a panel lists every hotkey as
/// bound in `hotkeys.toml`, plus the mouse, wheel and held-modifier
/// gestures. Lives in the toolbar so it is always discoverable but
/// never in the way.
fn help_btn(ui: &mut egui::Ui, keys: &ParsedHotkeys) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(BTN_SIZE), egui::Sense::hover());
    theme::paint_button(ui, rect, Look::Plain, &response);
    let icon = egui::Rect::from_center_size(rect.center(), Vec2::splat((BTN_SIZE * 0.56).round()));
    egui::Image::new(egui::include_image!("../../assets/icons/keyboard.svg"))
        .tint(theme::TEXT_PRIMARY)
        .fit_to_exact_size(icon.size())
        .paint_at(ui, icon);
    response.on_hover_ui(|ui| help_panel(ui, keys));
    // Hover is *not* a click — never report it as a user interaction so
    // pen events still flow to the canvas.
    false
}

/// Close the app: a drawn × (the icon set has none). Everything is saved
/// first (`on_exit`); the app asks when slides deleted this run would be
/// gone for good (`app::confirm`).
fn close_btn(ui: &mut egui::Ui, quit: &mut bool) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(BTN_SIZE), egui::Sense::click());
    theme::paint_button(ui, rect, Look::Plain, &response);
    let r = rect.shrink(rect.width() * 0.34);
    let stroke = Stroke::new(1.6, theme::TEXT_PRIMARY);
    ui.painter().line_segment([r.left_top(), r.right_bottom()], stroke);
    ui.painter().line_segment([r.right_top(), r.left_bottom()], stroke);
    let response = tip(response, "Close", None, "Close On-Screen Notes. Everything is saved first");
    if response.clicked() {
        *quit = true;
    }
    touched(&response)
}

/// One titled group of help rows, each an action and how to do it.
type HelpSection = (&'static str, Vec<(&'static str, String)>);

/// The help tooltip: titled sections of action / keys grids, flowed
/// into as many columns as the screen fits so the list never runs off
/// the bottom of it.
fn help_panel(ui: &mut egui::Ui, keys: &ParsedHotkeys) {
    // Room a column takes, with the gap after it.
    const COL_W: f32 = 430.0;
    let sections = help_sections(keys);
    let screen_w = ui.ctx().screen_rect().width() - 40.0;
    let cols = (screen_w / COL_W).floor().clamp(1.0, 3.0) as usize;
    // A tooltip stops at `spacing.tooltip_width`, which would wrap every
    // column past the first.
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

    ui.add_space(2.0);
    ui.label(egui::RichText::new("Shortcuts").font(theme::semibold(15.0)));
    ui.add_space(2.0);
    // Taller than the screen allows, the columns scroll: a scroll bar
    // makes the tooltip one the pointer can move onto, so it stays open
    // to be scrolled.
    let room = (ui.ctx().screen_rect().height() - 150.0).max(200.0);
    egui::ScrollArea::vertical().max_height(room).auto_shrink([true, true]).show(ui, |ui| {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 28.0;
            for (ci, column) in columns.iter().enumerate() {
                ui.vertical(|ui| {
                    for (si, (title, rows)) in column.iter().enumerate() {
                        if si > 0 {
                            ui.add_space(6.0);
                        }
                        theme::section(ui, title);
                        egui::Grid::new(("osn_help_grid", ci, si)).num_columns(2).spacing([16.0, 3.0]).min_row_height(20.0).show(ui, |ui| {
                            for (action, how) in rows {
                                ui.label(*action);
                                ui.horizontal(|ui| theme::shortcut_text(ui, how));
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
            Some(b) => format!(
                "{}: find any command by name.{} Keys can be changed in hotkeys.toml, where every button can be given one too.",
                b.label(),
                keys.canvas.find.map_or(String::new(), |f| format!(" {}: find text on the slides.", f.label())),
            ),
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
            ("Group by tags",            a.group_by_tags),
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
            ("Dim the rest on / off",    a.dim_unselected),
            ("Selection in slide window", a.open_in_window),
        ])),
        ("Panels & pins", bound(vec![
            ("New slide",                c.new_slide),
            ("Move the slide up",        c.move_slide_up),
            ("Move the slide down",      c.move_slide_down),
            ("Fold / unfold slide panel", c.toggle_slide_panel),
            ("Ctrl+Tab: in order / recent", c.toggle_slide_order),
            ("Slide window open / close", c.slide_window),
            ("Slide window on a box",    c.slide_window_region),
            ("New layer",                c.new_layer),
            ("Delete layer",             c.delete_layer),
            ("Pin: go to its slide",     c.pin_go_to_slide),
            ("Pin: this slide only / every slide", c.pin_this_slide_only),
            ("Loop video / GIF",         c.media_loop),
            ("Onion skin on / off",      c.media_onion),
            ("UI bigger",                c.ui_scale_up),
            ("UI smaller",               c.ui_scale_down),
            ("UI size 100 %",            c.ui_scale_reset),
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
            _ => bs.map(&k).join(" "),
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
            ("Select all (active layer)",   k(c.select_all)),
            ("Move / resize / rotate",      s("drag / handles / knob")),
            ("Nudge 1 px",                  k4(c.nudge_1px)),
            ("Nudge 10 px",                 k4(c.nudge_10px)),
            ("Next object left / right / up / down", k4(c.hop)),
            ("Select mid-view; into a group", k(c.step_in)),
            ("Out of a group",              k(c.step_out)),
            ("Rotate in 15° steps",         s("hold Shift")),
            ("Delete",                      format!("{} / Backspace", k(c.delete_selection))),
            ("Duplicate",                   k(c.duplicate)),
            ("Copy / cut / paste (pictures too)", s("Ctrl+C / X / V")),
            ("To front / to back",          k2(c.bring_front, c.send_back)),
            ("Copy to a new slide",         k(c.copy_to_new_slide)),
            ("Pin / unpin; pins window",  format!("{}; {}", k(c.pin_selection), k(c.toggle_pins))),
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
            ("Resync from its PureRef board", c.resync_pureref.map_or_else(|| s("slide right-click"), |b| b.label())),
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

/// Contextual "dashed border" toggle. Drawn as a small rectangle with
/// either a solid or dashed outline, painted directly via the egui
/// painter so we don't need an extra SVG. Clicking flips
/// `tool.border_style` between `Solid` and `DashedAnimated`; the
/// renderer reads this flag when committing the next shape.
fn dash_toggle_btn(ui: &mut egui::Ui, tool: &mut ActiveTool, key: Option<Binding>) -> bool {
    let on = matches!(tool.border_style, BorderStyle::DashedAnimated);
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(BTN_SIZE), egui::Sense::click());
    theme::paint_button(ui, rect, Look::on(on), &response);

    // Icon: a small box, dashed when on so the user sees what they will
    // get. The four sides are emitted as short segments — no glyph needed.
    let painter = ui.painter();
    let icon = egui::Rect::from_center_size(rect.center(), Vec2::splat((BTN_SIZE * 0.48).round()));
    let stroke = Stroke::new(1.6, theme::TEXT_PRIMARY);
    if on {
        let segs = 4;
        let edges = [
            (icon.left_top(), icon.right_top()),
            (icon.right_top(), icon.right_bottom()),
            (icon.right_bottom(), icon.left_bottom()),
            (icon.left_bottom(), icon.left_top()),
        ];
        for (a, b) in edges {
            let d = b - a;
            for i in 0..segs {
                let t0 = i as f32 / segs as f32;
                let t1 = (i as f32 + 0.55) / segs as f32;
                painter.line_segment([a + d * t0, a + d * t1], stroke);
            }
        }
    } else {
        painter.rect_stroke(icon, Rounding::same(2.0), stroke);
    }

    let response = if on {
        tip(response, "Dashed border", key, "New shapes get moving dashes. Click for a solid line")
    } else {
        tip(response, "Solid border", key, "Click to give new shapes moving dashes")
    };
    if response.clicked() {
        tool.border_style = match tool.border_style {
            BorderStyle::Solid => BorderStyle::DashedAnimated,
            BorderStyle::DashedAnimated => BorderStyle::Solid,
        };
        return true;
    }
    touched(&response)
}

/// Text-in-a-box switch: flips `tool.text_box`, which the Text tool
/// reads when it places a label (and the app applies to selected
/// labels).
fn text_box_btn(ui: &mut egui::Ui, tool: &mut ActiveTool, key: Option<Binding>) -> bool {
    let on = tool.text_box;
    let response = theme::icon_button(ui, egui::Image::new(egui::include_image!("../../assets/icons/text-box.svg")), BTN_SIZE, Look::on(on));
    let response = if on {
        tip(response, "Text in a box", key, "On: labels sit in a box that keeps them readable. Applies to the selected labels too")
    } else {
        tip(response, "Text in a box", key, "Off: plain labels. Click to put them in a box that keeps them readable (the selected ones too)")
    };
    if response.clicked() {
        tool.text_box = !tool.text_box;
        return true;
    }
    touched(&response)
}

/// Pin-to-window toolbar button. Three states:
///   * No pin set, no picking → plain.
///   * Pin picking armed → on.
///   * Pinned to a window → on.
///
/// Click semantics: idle → arm picking; picking → cancel; pinned →
/// unpin. The actual pick-and-follow logic lives in `app.rs`.
fn pin_btn(ui: &mut egui::Ui, pinned_hwnd: &mut Option<isize>, pin_picking: &mut bool, key: Option<Binding>) -> bool {
    let on = pinned_hwnd.is_some() || *pin_picking;
    let response = theme::icon_button(ui, egui::Image::new(egui::include_image!("../../assets/icons/pin.svg")), BTN_SIZE, Look::on(on));
    let response = if *pin_picking {
        tip(response, "Pick a window", key, "Click any window to follow it — Esc cancels")
    } else if pinned_hwnd.is_some() {
        tip(response, "Pinned to a window", key, "The overlay follows that window. Click to unpin")
    } else {
        tip(response, "Pin to a window", key, "Click, then click any window: the overlay follows it")
    };
    if response.clicked() {
        if pinned_hwnd.is_some() {
            *pinned_hwnd = None;
        } else {
            *pin_picking = !*pin_picking;
        }
        return true;
    }
    touched(&response)
}

/// What the settings popup changes.
struct Settings<'a> {
    bg_opacity: &'a mut f32,
    bg_color: &'a mut [u8; 3],
    smoothing: &'a mut SmoothingLevel,
    smoothing_options: &'a mut SmoothingOptions,
    pressure_curve: &'a mut f32,
    ui_scale: &'a mut f32,
    accent: &'a mut [u8; 3],
    shape_snap: &'a mut bool,
    smart_guides: &'a mut bool,
    keep_proportions: &'a mut bool,
    beside_fit: &'a mut BesideFit,
    beside_side: &'a mut BesideSide,
    picture_search: &'a mut bool,
    picture_status: &'a str,
    browser_link: &'a mut bool,
    browser_status: &'a str,
    forget_browsers: &'a mut bool,
    keys: &'a ParsedHotkeys,
}

/// The settings popup's pages.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum SettingsTab {
    #[default]
    General,
    Pen,
    Editing,
}

/// Where the settings popup keeps its page: global, so a test can pick one.
pub(crate) const SETTINGS_TAB: &str = "osn_settings_tab";

/// The settings button and its popup: the UI size and the background,
/// the pen, and editing, each a page. Everything in the popup is drawn
/// inline — a combo box or a colour button would open a popup of its
/// own, which egui counts as a click outside this one, closing it.
fn settings_btn(ui: &mut egui::Ui, s: Settings) -> bool {
    let popup_id = egui::Id::new(SETTINGS_POPUP);
    let open = ui.memory(|m| m.is_popup_open(popup_id));
    let response = theme::icon_button(ui, egui::Image::new(egui::include_image!("../../assets/icons/settings.svg")), BTN_SIZE, Look::on(open));
    let response = tip(response, "Settings", None, "UI size, background, pen, selection and pictures");
    let mut interacted = touched(&response);
    if response.clicked() {
        ui.memory_mut(|m| m.toggle_popup(popup_id));
        interacted = true;
    }
    theme::popup_below(ui, popup_id, &response, true, |ui| {
        ui.set_width(320.0);
        settings_panel(ui, s);
    });
    interacted
}

fn settings_panel(ui: &mut egui::Ui, s: Settings) {
    let tab_id = egui::Id::new(SETTINGS_TAB);
    let mut tab = ui.data(|d| d.get_temp::<SettingsTab>(tab_id)).unwrap_or_default();
    ui.spacing_mut().item_spacing.y = 8.0;
    ui.add_space(2.0);
    ui.label(egui::RichText::new("Settings").font(theme::semibold(15.0)));
    theme::segmented(ui, &mut tab, &[(SettingsTab::General, "General"), (SettingsTab::Pen, "Pen"), (SettingsTab::Editing, "Editing")], true);
    ui.data_mut(|d| d.insert_temp(tab_id, tab));
    ui.add_space(2.0);
    match tab {
        SettingsTab::General => general_page(ui, s),
        SettingsTab::Pen => pen_page(ui, s),
        SettingsTab::Editing => editing_page(ui, s),
    }
    ui.add_space(2.0);
}

/// The background tints on offer, before "custom".
const BACKGROUNDS: [(&str, [u8; 3]); 5] = [
    ("Black", [0, 0, 0]),
    ("Graphite", [36, 36, 38]),
    ("Grey", [128, 128, 128]),
    ("Paper", [244, 242, 236]),
    ("White", [255, 255, 255]),
];

fn general_page(ui: &mut egui::Ui, s: Settings) {
    theme::section(ui, "Interface");
    let c = &s.keys.canvas;
    let hint = match (c.ui_scale_down, c.ui_scale_up) {
        (Some(d), Some(u)) => format!("Bars and panels; slides keep their size. {} / {}", d.label(), u.label()),
        _ => "Bars and panels; slides keep their size".to_string(),
    };
    theme::setting_row(ui, "UI size", Some(&hint), |ui| ui_scale_stepper(ui, s.ui_scale, c));
    colour_row(ui, "Accent", Some("The tool in use, switches, picks and the selection"), "osn_settings_custom_accent", &ACCENTS, s.accent);

    theme::section(ui, "Background");
    theme::setting_row(ui, "Opacity", Some("How much the canvas covers what is behind it"), |ui| {
        theme::value_label(ui, format!("{:.0} %", *s.bg_opacity * 100.0), 40.0);
        theme::slider(ui, s.bg_opacity, 0.0..=1.0, 0.01, 120.0);
    });
    colour_row(ui, "Colour", None, "osn_settings_custom_bg", &BACKGROUNDS, s.bg_color);
}

/// The accents on offer, before "custom". The first is the monochrome
/// default.
const ACCENTS: [(&str, [u8; 3]); 6] = [
    ("Mono", theme::MONO),
    ("Blue", [64, 156, 255]),
    ("Violet", [150, 122, 255]),
    ("Pink", [255, 92, 160]),
    ("Orange", [255, 153, 51]),
    ("Green", [60, 200, 112]),
];

/// A settings row of colour swatches — `presets`, then a custom one —
/// for `colour`. The custom one opens a picker under the row, inline
/// (see `settings_btn`); `custom_key` keeps whether it is open.
fn colour_row(ui: &mut egui::Ui, title: &str, hint: Option<&str>, custom_key: &str, presets: &[(&str, [u8; 3])], colour: &mut [u8; 3]) {
    let custom_id = egui::Id::new(custom_key);
    let mut custom = ui.data(|d| d.get_temp::<bool>(custom_id)).unwrap_or(false);
    theme::setting_row(ui, title, hint, |ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let preset = presets.iter().any(|(_, c)| c == colour);
        // Custom: the colour itself when it is none of the presets.
        let shown = if preset { None } else { Some(*colour) };
        if bg_swatch(ui, shown, custom || !preset, "Custom colour…").clicked() {
            custom = !custom;
        }
        for (name, c) in presets.iter().rev() {
            if bg_swatch(ui, Some(*c), !custom && colour == c, name).clicked() {
                *colour = *c;
                custom = false;
            }
        }
    });
    if custom {
        let mut c32 = Color32::from_rgb(colour[0], colour[1], colour[2]);
        if egui::color_picker::color_picker_color32(ui, &mut c32, egui::color_picker::Alpha::Opaque) {
            *colour = [c32.r(), c32.g(), c32.b()];
        }
    }
    ui.data_mut(|d| d.insert_temp(custom_id, custom));
}

/// A round background swatch: `colour`, or a "+" for a custom one not
/// picked yet. Ringed when `picked`.
fn bg_swatch(ui: &mut egui::Ui, colour: Option<[u8; 3]>, picked: bool, name: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(24.0), egui::Sense::click());
    let p = ui.painter();
    let c = rect.center();
    match colour {
        Some([r, g, b]) => {
            p.circle_filled(c, 8.0, Color32::from_rgb(r, g, b));
            p.circle_stroke(c, 8.0, Stroke::new(1.0, theme::HAIRLINE));
        }
        None => {
            p.circle_stroke(c, 7.5, Stroke::new(1.0, Color32::from_white_alpha(70)));
            let s = Stroke::new(1.4, theme::TEXT_PRIMARY);
            p.line_segment([c - egui::vec2(3.5, 0.0), c + egui::vec2(3.5, 0.0)], s);
            p.line_segment([c - egui::vec2(0.0, 3.5), c + egui::vec2(0.0, 3.5)], s);
        }
    }
    if picked {
        p.circle_stroke(c, 11.0, Stroke::new(1.5, theme::TEXT_PRIMARY));
    } else if response.hovered() {
        p.circle_stroke(c, 11.0, Stroke::new(1.0, Color32::from_white_alpha(90)));
    }
    response.on_hover_text(name)
}

/// The UI size: − / the size / +, the size a button back to 100 %.
/// Drawn right to left (a `setting_row` control).
fn ui_scale_stepper(ui: &mut egui::Ui, scale: &mut f32, s_keys: &crate::input::hotkeys_config::CanvasBindings) {
    let reset_key = s_keys.ui_scale_reset;
    use crate::config::{ui_scale_step, UI_SCALES};
    ui.spacing_mut().item_spacing.x = 2.0;
    let top = *scale >= UI_SCALES[UI_SCALES.len() - 1] - 1e-3;
    let bottom = *scale <= UI_SCALES[0] + 1e-3;
    let plus = ui.add_enabled_ui(!top, |ui| theme::icon_button(ui, egui::Image::new(egui::include_image!("../../assets/icons/plus.svg")), 26.0, Look::Plain)).inner;
    let unbound = "Keys can be bound in hotkeys.toml: canvas.ui_scale_up, _down and _reset";
    if theme::tip(plus, "Bigger", s_keys.ui_scale_up, s_keys.ui_scale_up.is_none().then_some(unbound)).clicked() {
        *scale = ui_scale_step(*scale, 1);
    }
    let (rect, value) = ui.allocate_exact_size(egui::vec2(52.0, 26.0), egui::Sense::click());
    theme::paint_button(ui, rect, Look::Plain, &value);
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, format!("{:.0} %", *scale * 100.0), egui::FontId::proportional(13.0), theme::TEXT_PRIMARY);
    if theme::tip(value, "100 %", reset_key, Some("Click to go back to the system's size")).clicked() {
        *scale = 1.0;
    }
    let minus = ui.add_enabled_ui(!bottom, |ui| theme::icon_button(ui, egui::Image::new(egui::include_image!("../../assets/icons/minus.svg")), 26.0, Look::Plain)).inner;
    if theme::tip(minus, "Smaller", s_keys.ui_scale_down, s_keys.ui_scale_down.is_none().then_some(unbound)).clicked() {
        *scale = ui_scale_step(*scale, -1);
    }
}

fn pen_page(ui: &mut egui::Ui, s: Settings) {
    let o = s.smoothing_options;
    theme::section(ui, "Smoothing");
    theme::setting_row(ui, "Strength", Some("More is cleaner, with a little lag"), |ui| {
        let levels = [SmoothingLevel::Low, SmoothingLevel::Medium, SmoothingLevel::High];
        let options: Vec<_> = levels.iter().map(|l| (*l, l.label())).collect();
        theme::segmented(ui, s.smoothing, &options, false);
    });
    theme::setting_block(ui, "Method", Some(o.kind.tooltip()), |ui| {
        let kinds = [SmoothingType::Adaptive, SmoothingType::None, SmoothingType::Simple, SmoothingType::Weighted, SmoothingType::Stabilizer];
        let options: Vec<_> = kinds.iter().map(|k| (*k, k.label())).collect();
        theme::segmented(ui, &mut o.kind, &options, true);
    });
    // The method's own knobs; Adaptive is tuned by the strength above,
    // None and Simple have none.
    let slider_row = |ui: &mut egui::Ui, title: &str, hint: &str, v: &mut f32, range: std::ops::RangeInclusive<f32>, step: f32, fmt: &dyn Fn(f32) -> String| {
        theme::setting_row(ui, title, Some(hint), |ui| {
            theme::value_label(ui, fmt(*v), 36.0);
            theme::slider(ui, v, range, step, 110.0);
        });
    };
    let px = |v: f32| format!("{v:.0}");
    match o.kind {
        SmoothingType::Adaptive | SmoothingType::None | SmoothingType::Simple => {}
        SmoothingType::Weighted => {
            slider_row(ui, "Distance", "Bigger is smoother and lags more", &mut o.smoothness_distance, 3.0..=80.0, 1.0, &px);
            slider_row(ui, "Tail", "Tightens the line as the pen lifts", &mut o.tail_aggressiveness, 0.0..=1.0, 0.05, &|v| format!("{v:.2}"));
            theme::setting_row(ui, "Smooth pressure", None, |ui| theme::switch(ui, &mut o.smooth_pressure));
        }
        SmoothingType::Stabilizer => {
            slider_row(ui, "Sample size", "Bigger pulls the line along like a rope", &mut o.smoothness_distance, 3.0..=80.0, 1.0, &px);
            theme::setting_row(ui, "Delay distance", Some("Ink waits until the pen is this far ahead"), |ui| theme::switch(ui, &mut o.use_delay_distance));
            if o.use_delay_distance {
                slider_row(ui, "Delay", "Points", &mut o.delay_distance, 1.0..=80.0, 1.0, &px);
            }
            theme::setting_row(ui, "Finish the curve", Some("The line catches up with the pen when it lifts"), |ui| theme::switch(ui, &mut o.finish_stabilized_curve));
            theme::setting_row(ui, "Steady pressure and tilt", None, |ui| theme::switch(ui, &mut o.stabilize_sensors));
        }
    }
    theme::setting_row(ui, "Round the corners", Some("Sharp turns are drawn as fans"), |ui| theme::switch(ui, &mut o.fan_corners));
    if o.fan_corners {
        slider_row(ui, "Corner step", "Radians a fan blade turns", &mut o.fan_corners_step, 0.05..=0.80, 0.01, &|v| format!("{v:.2}"));
    }

    theme::section(ui, "Pressure");
    // Raw pressure → width as `raw.powf(curve)`: 0.5 a lighter touch,
    // 2.0 a heavier one, 1.0 linear (the default).
    slider_row(ui, "Curve", "Below 1 a light touch draws thicker, above 1 thinner", s.pressure_curve, 0.5..=2.0, 0.05, &|v| format!("{v:.2}"));

    theme::section(ui, "Shapes");
    theme::setting_row(ui, "Shape snap", Some("Hold the pen still at the end of a stroke and it becomes the line, rectangle or circle it was meant to be"), |ui| theme::switch(ui, s.shape_snap));
}

fn editing_page(ui: &mut egui::Ui, s: Settings) {
    theme::section(ui, "Selection");
    theme::setting_row(ui, "Smart guides", Some("A selection being moved lines up with the objects round it. Hold Alt to move it freely"), |ui| theme::switch(ui, s.smart_guides));
    theme::setting_row(ui, "Keep proportions", Some("Resizing keeps the shape. Off, the sides stretch it — text, and objects turned differently, always keep theirs"), |ui| theme::switch(ui, s.keep_proportions));

    theme::section(ui, "New pictures");
    theme::setting_block(
        ui,
        "Beside the selected one",
        Some("With a picture, GIF or video selected, one dropped, pasted or inserted takes its height or width and lands beside it — or, stacked, on top of it"),
        |ui| {
            let options: Vec<_> = BesideFit::ALL.iter().map(|f| (*f, f.label())).collect();
            theme::segmented(ui, s.beside_fit, &options, true);
        },
    );
    if *s.beside_fit != BesideFit::Off {
        let hint = match *s.beside_side {
            BesideSide::Stack => "Centred on top of it",
            BesideSide::Left => "Left of it, past any already there",
            BesideSide::Right => "Right of it, past any already there",
        };
        theme::setting_row(ui, "Where", Some(hint), |ui| {
            let options: Vec<_> = BesideSide::ALL.iter().map(|f| (*f, f.label())).collect();
            theme::segmented(ui, s.beside_side, &options, false);
        });
    }

    theme::section(ui, "Picture search");
    theme::setting_row(
        ui,
        "Find by subject",
        Some("Pictures like this one find the same character, pose or kind of study, and words find what pictures show. A vision model reads each picture once, on this computer: about 215 MB to download, 570 MB more for words"),
        |ui| theme::switch(ui, s.picture_search),
    );
    if *s.picture_search && !s.picture_status.is_empty() {
        ui.label(egui::RichText::new(s.picture_status).small().color(theme::TEXT_MUTED));
    }

    theme::section(ui, "Browser extension");
    theme::setting_row(
        ui,
        "Receive from the browser",
        Some("The On-Screen Notes extension for Firefox and Chrome sends pictures, GIFs and videos straight to the slide: right-click one, or pick it. Listens on this computer only, and asks before an extension may send"),
        |ui| theme::switch(ui, s.browser_link),
    );
    if *s.browser_link && !s.browser_status.is_empty() {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(s.browser_status).small().color(theme::TEXT_MUTED));
            if s.browser_status.contains("connected:") {
                let tip = "Let every connected browser go: each asks again (Connect) the next time it sends";
                if ui.small_button("Disconnect all").on_hover_text(tip).clicked() {
                    *s.forget_browsers = true;
                }
            }
        });
    }
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

    /// The toolbar as the app shows it, with everything at its default.
    fn show_default(ctx: &egui::Context, cfg: &mut crate::config::AppConfig, tool: &mut ActiveTool) {
        // As the app does each frame.
        crate::ui::theme::set_accent(ctx, cfg.accent);
        let keys = HotkeysConfig::default().parsed();
        let mut palette = cfg.palette.clone();
        let (mut pin, mut picking, mut insert, mut quit, mut select) = (None, false, false, false, false);
        let mut filter = MediaFilter::default();
        let counts = media_filter::Counts::default();
        show(
            ctx,
            tool,
            &mut cfg.bg_opacity,
            &mut cfg.bg_color,
            &mut palette,
            &mut cfg.smoothing_level,
            &mut cfg.smoothing,
            &mut cfg.pressure_curve,
            &mut cfg.ui_scale,
            &mut cfg.accent,
            (&mut cfg.shape_snap, &mut cfg.smart_guides, &mut cfg.keep_proportions),
            (&mut cfg.beside_fit, &mut cfg.beside_side),
            (&mut cfg.picture_search, "Reading pictures: 12 of 340", &mut cfg.browser_link, "Listening on 127.0.0.1:47823 · connected: Firefox", &mut false),
            &mut pin,
            &mut picking,
            &mut insert,
            &mut quit,
            &mut filter,
            &counts,
            &mut select,
            None,
            false,
            &keys,
        );
    }

    #[test]
    #[ignore = "writes PNGs to OSN_SNAP_DIR"]
    fn snapshot_toolbar() {
        use crate::ui::snapshot::{shoot, Step};
        let mut cfg = crate::config::AppConfig::default();
        let mut tool = ActiveTool::from_config(&cfg);
        let size = [1500.0, 860.0];
        let Some(ctx) = shoot("toolbar", size, 1.0, &[Step::Idle], |ctx| show_default(ctx, &mut cfg, &mut tool)) else { return };
        // The close button is the toolbar's last; before it, past a
        // divider (9 points, 2 of spacing each side), the help.
        let bar = ctx.memory(|m| m.area_rect(egui::Id::new("osn_toolbar"))).expect("toolbar drawn");
        let y = bar.center().y;
        let close = egui::pos2(bar.right() - 6.0 - BTN_SIZE * 0.5, y);
        let help = egui::pos2(close.x - BTN_SIZE - 13.0, y);
        let first = egui::pos2(bar.left() + 6.0 + BTN_SIZE * 1.5 + 2.0, y);
        let rest = |p| [Step::Move(p), Step::Move(p), Step::Move(p), Step::Move(p)];
        // A stack picked, to see the three-way picker.
        cfg.beside_side = BesideSide::Stack;
        for (tab, name) in [(SettingsTab::General, "general"), (SettingsTab::Pen, "pen"), (SettingsTab::Editing, "editing")] {
            shoot(&format!("settings-{name}"), size, 1.0, &[Step::Idle], |ctx| {
                ctx.memory_mut(|m| m.open_popup(egui::Id::new(SETTINGS_POPUP)));
                ctx.data_mut(|d| d.insert_temp(egui::Id::new(SETTINGS_TAB), tab));
                show_default(ctx, &mut cfg, &mut tool);
            });
        }
        shoot("toolbar-hover", size, 1.0, &rest(first), |ctx| show_default(ctx, &mut cfg, &mut tool));
        shoot("toolbar-help", size, 1.0, &rest(help), |ctx| show_default(ctx, &mut cfg, &mut tool));
        shoot("toolbar-close", size, 1.0, &rest(close), |ctx| show_default(ctx, &mut cfg, &mut tool));
        cfg.ui_scale = 1.5;
        shoot("toolbar-150", size, 1.5, &[Step::Idle], |ctx| show_default(ctx, &mut cfg, &mut tool));
        // A colour accent: the tool in use, switches, picks. Run the
        // snapshots one at a time (`--test-threads=1`): the accent is the
        // whole app's.
        cfg.ui_scale = 1.0;
        cfg.accent = [64, 156, 255];
        for (tab, name) in [(SettingsTab::General, "accent-general"), (SettingsTab::Editing, "accent-editing")] {
            shoot(name, size, 1.0, &[Step::Idle], |ctx| {
                ctx.memory_mut(|m| m.open_popup(egui::Id::new(SETTINGS_POPUP)));
                ctx.data_mut(|d| d.insert_temp(egui::Id::new(SETTINGS_TAB), tab));
                show_default(ctx, &mut cfg, &mut tool);
            });
        }
        crate::ui::theme::set_accent(&egui::Context::default(), crate::ui::theme::MONO);
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
