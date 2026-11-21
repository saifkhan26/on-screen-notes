//! Centralised visual style — colours, spacing, rounding, fonts — and the
//! small widgets every panel shares (switches, segmented pickers, the
//! settings rows, key caps, tooltips).
//!
//! We apply this once at app startup via `apply(&egui::Context)` so every
//! widget inherits the same look.
//!
//! Design intent:
//!   * Monochrome: black glass, white ink, greys between. No hue in the
//!     chrome — the only colours on screen are the user's own (palette,
//!     ink, pictures).
//!   * One strong highlight: the tool in use is a solid light chip with
//!     a dark icon. Switches that are on, and toggled buttons, are a
//!     quieter grey, so the current tool always reads first.
//!   * Flat cards with soft shadows and no borders (user-requested); thin
//!     hairlines only to split groups inside a card.
//!   * Generous corner radius so floating panels look like cards.

use crate::input::hotkeys_config::Binding;
use egui::{Color32, Margin, Rounding, Shadow, Stroke, Vec2};

/// A translucent white, premultiplied (rgb == α), usable in a `const`.
/// (`Color32::from_white_alpha` is not `const fn` in epaint 0.29, and the
/// plain `from_rgba_premultiplied(255, 255, 255, α)` is invalid premul,
/// which egui draws as opaque white.)
const fn white(a: u8) -> Color32 {
    Color32::from_rgba_premultiplied(a, a, a, a)
}

/// A grey `g` (0–255) at opacity `a`, premultiplied.
const fn grey(g: u8, a: u8) -> Color32 {
    let c = ((g as u16 * a as u16) / 255) as u8;
    Color32::from_rgba_premultiplied(c, c, c, a)
}

/// Toggled-on fill — a quiet translucent white. Used for switches that
/// are on in a bar (dashed border, text box, hidden pins, a filter in
/// use) and the active row of a list. The tool in use is
/// `SELECTED_FILL`, stronger.
pub const ACCENT: Color32 = white(40);

/// The tool in use: a solid light chip…
pub const SELECTED_FILL: Color32 = Color32::from_gray(236);
/// …with its icon or text dark on it.
pub const SELECTED_INK: Color32 = Color32::from_gray(18);

/// Under the pointer.
pub const HOVER: Color32 = white(18);
/// Pressed.
pub const PRESSED: Color32 = white(30);
/// The well a control sits in: a slider's rail, a segmented picker's
/// track, a text field.
pub const SURFACE: Color32 = white(14);
/// A thin rule between groups.
pub const HAIRLINE: Color32 = white(26);

/// Glass-panel fill — near-black, nearly opaque, so the bars read the
/// same over a white page and a dark one.
pub const GLASS_FILL: Color32 = grey(14, 238);
/// Popups, menus and tooltips: a step lighter than the card they open
/// from, so they read as on top of it.
pub const POPUP_FILL: Color32 = grey(26, 250);
/// Glass-panel border — fully transparent: cards have no outline
/// (user-requested).
#[allow(dead_code)]
pub const GLASS_STROKE: Color32 = Color32::TRANSPARENT;

/// Foreground text — neutral near-white.
pub const TEXT_PRIMARY: Color32 = Color32::from_gray(236);
/// Secondary text: descriptions, values, counts.
pub const TEXT_MUTED: Color32 = Color32::from_gray(156);
/// Least: section titles, disabled hints.
pub const TEXT_FAINT: Color32 = Color32::from_gray(112);

/// Lines drawn on the canvas for the UI — the selection's frame and
/// handles, pins' outlines — light over a dark halo (`CHROME_UNDER`), so
/// they read over a white page and a dark one alike.
pub const CHROME_LINE: Color32 = Color32::from_gray(245);
/// The halo under `CHROME_LINE`.
pub const CHROME_UNDER: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 150);

/// A card's corner radius.
pub const CARD_ROUNDING: f32 = 14.0;
/// A control's (button, field, chip) corner radius.
pub const CONTROL_ROUNDING: f32 = 8.0;

/// The semibold UI face, for titles (falls back to the regular one).
pub fn semibold(size: f32) -> egui::FontId {
    egui::FontId::new(size, egui::FontFamily::Name("semibold".into()))
}

/// A soft drop-shadow used by floating cards.
pub fn card_shadow() -> Shadow {
    Shadow {
        offset: egui::vec2(0.0, 6.0),
        blur: 24.0,
        spread: 0.0,
        color: Color32::from_black_alpha(110),
    }
}

/// A `Frame` styled like a glass card — for the floating toolbar / pills.
/// No border (user-requested minimal black-and-white aesthetic); the
/// drop shadow alone separates the card from the canvas.
pub fn card_frame() -> egui::Frame {
    egui::Frame {
        inner_margin: Margin::symmetric(6.0, 6.0),
        outer_margin: Margin::ZERO,
        rounding: Rounding::same(CARD_ROUNDING),
        shadow: card_shadow(),
        fill: GLASS_FILL,
        stroke: Stroke::NONE,
    }
}

/// A smaller card, for the slim panels (layers, slides).
pub fn panel_frame() -> egui::Frame {
    egui::Frame { inner_margin: Margin::same(5.0), rounding: Rounding::same(12.0), ..card_frame() }
}

/// Corner radius of a window, points: Windows 11's.
pub const WINDOW_ROUNDING: f32 = 8.0;

/// A window's background: `tint` (the overlay's colour at its opacity)
/// filling `rect` with Windows 11's rounded corners, square while the
/// window is maximized or full screen, as Windows draws them. A thin
/// rounded border goes round it, as Windows draws round its own
/// windows, faded with the tint so a clear overlay stays clear. The
/// windows are see-through, so Windows itself cannot round them (see
/// `platform::round_corners`).
pub fn paint_window(painter: &egui::Painter, rect: egui::Rect, tint: Color32, square: bool) {
    let rounding = if square { Rounding::ZERO } else { Rounding::same(WINDOW_ROUNDING) };
    painter.rect_filled(rect, rounding, tint);
    if !square && tint.a() > 0 {
        let a = (f32::from(tint.a()) / 255.0 * 50.0).round() as u8;
        painter.rect_stroke(rect.shrink(0.5), rounding, Stroke::new(1.0, Color32::from_white_alpha(a)));
    }
}

/// Apply the project-wide style, fonts included (the text styles name
/// the "semibold" family `install_fonts` adds). Called once on startup.
pub fn apply(ctx: &egui::Context) {
    install_fonts(ctx);
    // egui's own Ctrl+= / Ctrl+- / Ctrl+0 zoom would take those keys
    // from the hotkeys bound to them; the UI size is the settings' (and
    // `canvas.ui_scale_*`'s) instead.
    ctx.options_mut(|o| o.zoom_with_keyboard = false);

    let mut style = (*ctx.style()).clone();

    // Type: a tight, readable scale.
    {
        use egui::{FontFamily, FontId, TextStyle};
        style.text_styles = [
            (TextStyle::Small, FontId::new(11.0, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(13.0, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(13.0, FontFamily::Proportional)),
            (TextStyle::Heading, FontId::new(16.0, FontFamily::Name("semibold".into()))),
            (TextStyle::Monospace, FontId::new(12.0, FontFamily::Monospace)),
        ]
        .into();
    }

    // Visuals: dark theme baseline, then tweak.
    let v = &mut style.visuals;
    *v = egui::Visuals::dark();
    v.override_text_color = Some(TEXT_PRIMARY);
    v.window_fill = POPUP_FILL;
    v.panel_fill = Color32::TRANSPARENT;
    v.faint_bg_color = white(6);
    v.extreme_bg_color = grey(8, 230);
    v.code_bg_color = SURFACE;
    v.window_stroke = Stroke::NONE;
    v.window_rounding = Rounding::same(CARD_ROUNDING);
    v.menu_rounding = Rounding::same(10.0);
    v.window_shadow = card_shadow();
    v.popup_shadow = Shadow { offset: egui::vec2(0.0, 6.0), blur: 20.0, spread: 0.0, color: Color32::from_black_alpha(120) };
    v.clip_rect_margin = 0.0;
    v.slider_trailing_fill = true;
    v.handle_shape = egui::style::HandleShape::Circle;
    v.striped = false;
    v.indent_has_left_vline = false;
    v.text_cursor.stroke = Stroke::new(1.5, TEXT_PRIMARY);

    // Selection / active states: greys only — no hue. Text stays white
    // on the grey, so selected text in a field stays readable.
    v.selection.bg_fill = white(64);
    v.selection.stroke = Stroke::new(1.0, TEXT_PRIMARY);
    v.hyperlink_color = TEXT_PRIMARY;

    let r = Rounding::same(CONTROL_ROUNDING);
    let w = &mut v.widgets;
    // Labels, separators. `weak_bg_fill` here is also what a disabled
    // UI fades toward (`Visuals::fade_out_to_color`): never transparent,
    // which egui takes for "invisible" — a disabled panel (the layer
    // panel on a locked slide) would draw nothing at all.
    w.noninteractive.bg_fill = Color32::TRANSPARENT;
    w.noninteractive.weak_bg_fill = Color32::from_gray(22);
    w.noninteractive.bg_stroke = Stroke::new(1.0, HAIRLINE);
    w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    w.noninteractive.rounding = r;
    // Buttons and the like: `weak_bg_fill` is a button's fill,
    // `bg_fill` a checkbox's box, a slider's rail and knob.
    w.inactive.weak_bg_fill = white(14);
    w.inactive.bg_fill = white(30);
    w.inactive.bg_stroke = Stroke::NONE;
    w.inactive.fg_stroke = Stroke::new(1.5, TEXT_PRIMARY);
    w.inactive.rounding = r;
    w.hovered.weak_bg_fill = white(24);
    w.hovered.bg_fill = white(44);
    w.hovered.bg_stroke = Stroke::NONE;
    w.hovered.fg_stroke = Stroke::new(1.5, Color32::WHITE);
    w.hovered.rounding = r;
    w.hovered.expansion = 0.0;
    w.active.weak_bg_fill = white(34);
    w.active.bg_fill = white(60);
    w.active.bg_stroke = Stroke::NONE;
    w.active.fg_stroke = Stroke::new(1.5, Color32::WHITE);
    w.active.rounding = r;
    w.active.expansion = 0.0;
    w.open.weak_bg_fill = white(24);
    w.open.bg_fill = white(44);
    w.open.bg_stroke = Stroke::NONE;
    w.open.fg_stroke = Stroke::new(1.5, TEXT_PRIMARY);
    w.open.rounding = r;

    // Spacing: roomy enough for pen-tip taps, still compact.
    let s = &mut style.spacing;
    s.button_padding = egui::vec2(10.0, 4.0);
    s.item_spacing = egui::vec2(6.0, 6.0);
    s.interact_size = egui::vec2(24.0, 24.0);
    s.icon_width = 16.0;
    s.icon_width_inner = 8.0;
    s.icon_spacing = 6.0;
    s.slider_width = 120.0;
    s.menu_margin = Margin::same(8.0);
    s.window_margin = Margin::same(12.0);
    s.tooltip_width = 300.0;
    s.combo_width = 120.0;
    s.scroll = egui::style::ScrollStyle {
        bar_width: 6.0,
        floating_width: 3.0,
        floating_allocated_width: 0.0,
        bar_inner_margin: 2.0,
        bar_outer_margin: 2.0,
        ..egui::style::ScrollStyle::floating()
    };

    ctx.set_style(style);

    // Tessellation options live on the `Context`, not on `Style`. Increase
    // the feathering size so anti-aliased stroke edges read as visibly
    // smooth rather than razor-crisp. 1.0 px is egui default; 1.5 looks
    // softer on thick pen strokes without going blurry.
    ctx.tessellation_options_mut(|opts| {
        opts.feathering = true;
        opts.feathering_size_in_pixels = 1.5;
        // Tighter bezier tolerance → more segments per curved path → nicer
        // rounded caps and joins.
        opts.bezier_tolerance = 0.1;
        opts.epsilon = 1e-5;
    });
}

/// Register the fonts. The UI is set in the system's own face where
/// there is one (Segoe UI on Windows, Cascadia Mono for keys), read from
/// the fonts folder at startup, with egui's bundled fonts behind it for
/// any glyph it lacks. The "handwritten" family is the one the Text
/// tool's labels are drawn in: egui's own default chain, so labels on
/// the slides keep the look and the size they were made with.
pub fn install_fonts(ctx: &egui::Context) {
    use egui::{FontData, FontFamily};
    let mut fonts = egui::FontDefinitions::default();
    let default_prop = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let default_mono = fonts.families.get(&FontFamily::Monospace).cloned().unwrap_or_default();
    fonts.families.insert(FontFamily::Name("handwritten".into()), default_prop.clone());

    // First of `files` that the system has, as font `name`.
    let mut load = |name: &str, files: &[&str]| -> bool {
        let Some(bytes) = system_font_dirs().iter().flat_map(|d| files.iter().map(move |f| d.join(f))).find_map(|p| std::fs::read(p).ok()) else {
            return false;
        };
        fonts.font_data.insert(name.to_owned(), FontData::from_owned(bytes));
        true
    };
    let ui = load("ui", &["segoeui.ttf", "Inter-Regular.ttf", "DejaVuSans.ttf"]);
    let semi = load("ui-semibold", &["seguisb.ttf", "Inter-SemiBold.ttf", "DejaVuSans-Bold.ttf"]);
    let mono = load("ui-mono", &["CascadiaMono.ttf", "consola.ttf", "DejaVuSansMono.ttf"]);

    let chain = |first: Option<&str>, rest: &[String]| -> Vec<String> { first.map(str::to_owned).into_iter().chain(rest.iter().cloned()).collect() };
    let prop = chain(ui.then_some("ui"), &default_prop);
    fonts.families.insert(FontFamily::Proportional, prop.clone());
    fonts.families.insert(FontFamily::Name("semibold".into()), chain(semi.then_some("ui-semibold"), &prop));
    fonts.families.insert(FontFamily::Monospace, chain(mono.then_some("ui-mono"), &default_mono));
    ctx.set_fonts(fonts);
}

/// Where the system keeps its fonts.
fn system_font_dirs() -> Vec<std::path::PathBuf> {
    let mut dirs = Vec::new();
    if let Some(win) = std::env::var_os("WINDIR") {
        dirs.push(std::path::PathBuf::from(win).join("Fonts"));
    }
    for d in ["/usr/share/fonts/truetype/dejavu", "/usr/share/fonts/TTF", "/Library/Fonts", "/System/Library/Fonts"] {
        dirs.push(d.into());
    }
    dirs
}

// ---------------------------------------------------------------------------
// Buttons
// ---------------------------------------------------------------------------

/// How a bar button shows its state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Look {
    /// Nothing to show.
    Plain,
    /// Switched on (a toggle), or the row that is active.
    On,
    /// The tool in use: the one strong highlight.
    Current,
}

impl Look {
    pub fn on(on: bool) -> Self {
        if on { Look::On } else { Look::Plain }
    }
}

/// A bar button's background for `look`, hovered or pressed.
pub fn button_fill(look: Look, response: &egui::Response) -> Color32 {
    match look {
        Look::Current => SELECTED_FILL,
        Look::On if response.is_pointer_button_down_on() => white(52),
        Look::On => ACCENT,
        Look::Plain if response.is_pointer_button_down_on() => PRESSED,
        Look::Plain if response.hovered() => HOVER,
        Look::Plain => Color32::TRANSPARENT,
    }
}

/// The ink an icon on a `look` button takes.
pub fn icon_ink(look: Look) -> Color32 {
    if look == Look::Current { SELECTED_INK } else { TEXT_PRIMARY }
}

/// Paint a bar button's background over `rect`.
pub fn paint_button(ui: &egui::Ui, rect: egui::Rect, look: Look, response: &egui::Response) {
    ui.painter().rect_filled(rect, Rounding::same(CONTROL_ROUNDING), button_fill(look, response));
}

/// A square icon button, `size` points: `image` tinted for `look`.
pub fn icon_button(ui: &mut egui::Ui, image: egui::Image<'_>, size: f32, look: Look) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), egui::Sense::click());
    paint_button(ui, rect, look, &response);
    let icon = egui::Rect::from_center_size(rect.center(), Vec2::splat((size * 0.56).round()));
    image.tint(icon_ink(look)).fit_to_exact_size(icon.size()).paint_at(ui, icon);
    response
}

/// A thin upright rule between groups of a bar, `height` points tall.
pub fn divider(ui: &mut egui::Ui, height: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(9.0, height), egui::Sense::hover());
    let x = rect.center().x.round() + 0.5;
    ui.painter().line_segment([egui::pos2(x, rect.top() + 5.0), egui::pos2(x, rect.bottom() - 5.0)], Stroke::new(1.0, HAIRLINE));
}

// ---------------------------------------------------------------------------
// Controls
// ---------------------------------------------------------------------------

/// An on / off switch: a pill whose knob slides across. On is a light
/// track with a dark knob, off a grey track with a light one.
pub fn switch(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let size = egui::vec2(32.0, 18.0);
    let (rect, mut response) = ui.allocate_exact_size(size, egui::Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), *on, ""));
    let t = ui.ctx().animate_bool_with_time(response.id, *on, 0.12);
    let off_track = if response.hovered() { white(56) } else { white(40) };
    let track = lerp_colour(off_track, SELECTED_FILL, t);
    let knob = lerp_colour(Color32::from_gray(230), SELECTED_INK, t);
    let r = rect.height() * 0.5;
    ui.painter().rect_filled(rect, Rounding::same(r), track);
    let x = egui::lerp((rect.left() + r)..=(rect.right() - r), t);
    ui.painter().circle_filled(egui::pos2(x, rect.center().y), r - 3.0, knob);
    response
}

fn lerp_colour(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| egui::lerp(f32::from(x)..=f32::from(y), t).round() as u8;
    Color32::from_rgba_premultiplied(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()), l(a.a(), b.a()))
}

/// A slider: a thin rail, filled up to the value, and a round light
/// knob. `width` points wide. Snaps to `step` when it is above 0.
pub fn slider(ui: &mut egui::Ui, value: &mut f32, range: std::ops::RangeInclusive<f32>, step: f32, width: f32) -> egui::Response {
    let (rect, mut response) = ui.allocate_exact_size(egui::vec2(width, 20.0), egui::Sense::click_and_drag());
    let (lo, hi) = (*range.start(), *range.end());
    let knob_r = 6.0;
    let rail = egui::Rect::from_min_max(
        egui::pos2(rect.left() + knob_r, rect.center().y - 1.5),
        egui::pos2(rect.right() - knob_r, rect.center().y + 1.5),
    );
    if let Some(p) = response.interact_pointer_pos() {
        let f = ((p.x - rail.left()) / rail.width()).clamp(0.0, 1.0);
        let mut v = lo + f * (hi - lo);
        if step > 0.0 {
            v = (lo + ((v - lo) / step).round() * step).clamp(lo, hi);
        }
        if (v - *value).abs() > f32::EPSILON {
            *value = v;
            response.mark_changed();
        }
    }
    let f = if hi > lo { ((*value - lo) / (hi - lo)).clamp(0.0, 1.0) } else { 0.0 };
    let x = egui::lerp(rail.left()..=rail.right(), f);
    let p = ui.painter();
    p.rect_filled(rail, Rounding::same(1.5), white(40));
    p.rect_filled(egui::Rect::from_min_max(rail.min, egui::pos2(x, rail.max.y)), Rounding::same(1.5), Color32::from_gray(200));
    let hot = response.hovered() || response.dragged();
    p.circle_filled(egui::pos2(x, rail.center().y), if hot { knob_r + 1.0 } else { knob_r }, Color32::from_gray(245));
    response
}

/// A row of options in one track, the picked one a light chip with dark
/// text. `fill` stretches it across the width available. Returns
/// whether the pick changed.
pub fn segmented<T: PartialEq + Copy>(ui: &mut egui::Ui, value: &mut T, options: &[(T, &str)], fill: bool) -> bool {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let galleys: Vec<_> = options.iter().map(|(_, l)| ui.painter().layout_no_wrap((*l).to_owned(), font.clone(), TEXT_PRIMARY)).collect();
    let pad = 2.0;
    let seg_h = 24.0;
    let natural: f32 = galleys.iter().map(|g| g.size().x + 20.0).sum();
    let width = if fill { ui.available_width().max(natural) } else { natural } + pad * 2.0;
    let (rect, track) = ui.allocate_exact_size(egui::vec2(width, seg_h + pad * 2.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, Rounding::same(CONTROL_ROUNDING + pad), SURFACE);
    let extra = (width - pad * 2.0 - natural) / options.len() as f32;
    let mut x = rect.left() + pad;
    let mut changed = false;
    for (i, ((opt, _), galley)) in options.iter().zip(galleys).enumerate() {
        let w = galley.size().x + 20.0 + extra;
        let seg = egui::Rect::from_min_size(egui::pos2(x, rect.top() + pad), egui::vec2(w, seg_h));
        x += w;
        let response = ui.interact(seg, track.id.with(i), egui::Sense::click());
        let picked = *value == *opt;
        let fill = if picked {
            SELECTED_FILL
        } else if response.hovered() {
            HOVER
        } else {
            Color32::TRANSPARENT
        };
        ui.painter().rect_filled(seg, Rounding::same(CONTROL_ROUNDING), fill);
        let ink = if picked { SELECTED_INK } else if response.hovered() { TEXT_PRIMARY } else { TEXT_MUTED };
        ui.painter().galley_with_override_text_color(seg.center() - galley.size() * 0.5, galley, ink);
        if response.clicked() && !picked {
            *value = *opt;
            changed = true;
        }
    }
    changed
}

/// A small title over a group of settings, in capitals.
pub fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(4.0);
    ui.label(egui::RichText::new(title.to_uppercase()).font(semibold(10.5)).color(TEXT_FAINT).extra_letter_spacing(0.6));
    ui.add_space(-1.0);
}

/// A settings row: `title` (and `hint` under it, smaller) on the left,
/// the control `add` draws on the right.
pub fn setting_row<R>(ui: &mut egui::Ui, title: &str, hint: Option<&str>, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    // A row as tall as a control to start with: the control centres on
    // the title's line, and a hint under the title makes the row taller.
    let size = egui::vec2(ui.available_width(), 26.0);
    ui.allocate_ui_with_layout(size, egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let r = add(ui);
        ui.add_space(12.0);
        let w = ui.available_width();
        ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
            ui.set_width(w);
            ui.spacing_mut().item_spacing.y = 1.0;
            ui.label(title);
            if let Some(h) = hint {
                ui.label(egui::RichText::new(h).small().color(TEXT_MUTED));
            }
        });
        r
    })
    .inner
}

/// A settings block: `title` and `hint` above, the control the full
/// width under them (for pickers too wide for a row).
pub fn setting_block<R>(ui: &mut egui::Ui, title: &str, hint: Option<&str>, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 1.0;
        ui.label(title);
        if let Some(h) = hint {
            ui.label(egui::RichText::new(h).small().color(TEXT_MUTED));
        }
        ui.add_space(5.0);
        add(ui)
    })
    .inner
}

/// A value shown beside a slider, in a fixed width so the row does not
/// shift as it changes.
pub fn value_label(ui: &mut egui::Ui, text: impl Into<String>, width: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 18.0), egui::Sense::hover());
    ui.painter().text(rect.right_center(), egui::Align2::RIGHT_CENTER, text.into(), egui::FontId::proportional(12.0), TEXT_MUTED);
}

/// A popup under `response`, open while `id` is (`Memory::toggle_popup`):
/// its right edge under the widget's when `right` (for a button at the
/// end of a bar), else its left. Closes on Esc or a click outside both.
pub fn popup_below<R>(ui: &egui::Ui, id: egui::Id, response: &egui::Response, right: bool, add: impl FnOnce(&mut egui::Ui) -> R) -> Option<R> {
    if !ui.memory(|m| m.is_popup_open(id)) {
        return None;
    }
    let gap = egui::vec2(0.0, 6.0);
    let (pos, pivot) = if right {
        (response.rect.right_bottom() + gap, egui::Align2::RIGHT_TOP)
    } else {
        (response.rect.left_bottom() + gap, egui::Align2::LEFT_TOP)
    };
    let frame = egui::Frame::popup(ui.style()).inner_margin(Margin::same(12.0)).rounding(Rounding::same(12.0));
    let shown = egui::Area::new(id)
        .order(egui::Order::Foreground)
        .constrain(true)
        .fixed_pos(pos)
        .pivot(pivot)
        .show(ui.ctx(), |ui| frame.show(ui, |ui| ui.with_layout(egui::Layout::top_down(egui::Align::Min), add).inner).inner);
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) || (response.clicked_elsewhere() && shown.response.clicked_elsewhere()) {
        ui.memory_mut(|m| m.close_popup());
    }
    Some(shown.inner)
}

/// The pointer is over the UI — a bar, a panel, a popup or dropdown,
/// an open tooltip — not over the canvas. The canvas is a background
/// panel, and egui's own `is_pointer_over_area` counts that too.
pub fn pointer_over_ui(ctx: &egui::Context) -> bool {
    let Some(pos) = ctx.input(|i| i.pointer.interact_pos()) else { return false };
    ctx.layer_id_at(pos).is_some_and(|l| l.order != egui::Order::Background)
}

// ---------------------------------------------------------------------------
// Keys and tooltips
// ---------------------------------------------------------------------------

/// One key cap: `text` in a small rounded chip.
pub fn keycap(ui: &mut egui::Ui, text: &str) {
    let galley = ui.painter().layout_no_wrap(text.to_owned(), egui::FontId::monospace(11.0), TEXT_PRIMARY);
    let size = egui::vec2((galley.size().x + 10.0).max(18.0), 18.0);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    ui.painter().rect_filled(rect, Rounding::same(5.0), white(22));
    ui.painter().rect_filled(egui::Rect::from_min_max(egui::pos2(rect.left(), rect.bottom() - 1.5), rect.max), Rounding { sw: 5.0, se: 5.0, ..Rounding::ZERO }, Color32::from_black_alpha(60));
    ui.painter().galley(rect.center() - galley.size() * 0.5, galley, TEXT_PRIMARY);
}

/// A binding as key caps, one per key: Ctrl, Shift, Alt, the key.
pub fn binding_caps(ui: &mut egui::Ui, b: Binding) {
    ui.spacing_mut().item_spacing.x = 3.0;
    let label = b.label();
    // `label` is "Ctrl+Shift+K": the modifiers each end in "+".
    let mut rest = label.as_str();
    for m in ["Ctrl+", "Shift+", "Alt+"] {
        if let Some(r) = rest.strip_prefix(m) {
            keycap(ui, &m[..m.len() - 1]);
            rest = r;
        }
    }
    keycap(ui, rest);
}

/// How a help row says to do something, as key caps where it names
/// keys and plain text where it says what to do: "Ctrl+Shift+Z" is three
/// caps, "drag on empty space" is words, "Shift+click; Alt+click" a cap
/// and a word, twice. Alternatives (" / ", "; ") are split by a faint
/// slash.
pub fn shortcut_text(ui: &mut egui::Ui, how: &str) {
    ui.spacing_mut().item_spacing.x = 3.0;
    let word = |ui: &mut egui::Ui, t: &str| {
        ui.label(egui::RichText::new(t).color(TEXT_MUTED));
    };
    let is_cap = |t: &str| {
        !t.is_empty()
            && !t.contains(' ')
            && t.chars().count() <= 12
            && !t.chars().next().is_some_and(|c| c.is_lowercase())
    };
    for (i, alt) in how.split(['/', ';']).map(str::trim).filter(|a| !a.is_empty()).enumerate() {
        if i > 0 {
            ui.label(egui::RichText::new("/").color(TEXT_FAINT));
        }
        // "hold Z (wheel zooms)", "Shift+Space+Scroll", "drag / handles".
        let parts: Vec<&str> = if alt.contains('+') && alt.len() > 1 { alt.split('+').collect() } else { vec![alt] };
        if parts.len() == 1 && alt.contains(' ') {
            // Words, perhaps with a key in them: "hold Shift", "hold Z (wheel zooms)".
            let mut it = alt.splitn(2, ' ');
            let (first, rest) = (it.next().unwrap_or(""), it.next().unwrap_or(""));
            let key = rest.split(' ').next().unwrap_or("");
            if first == "hold" && is_cap(key) {
                word(ui, "hold");
                keycap(ui, key);
                let after = rest[key.len()..].trim();
                if !after.is_empty() {
                    word(ui, after);
                }
            } else {
                word(ui, alt);
            }
            continue;
        }
        for (j, p) in parts.iter().enumerate() {
            let p = p.trim();
            if p.is_empty() {
                // "Ctrl++": the plus key.
                if j > 0 {
                    keycap(ui, "+");
                }
                continue;
            }
            if is_cap(p) { keycap(ui, p) } else { word(ui, p) }
        }
    }
}

/// A tooltip for a bar button: its `name`, the key that presses it as
/// caps on the right, and what it does, smaller, under them.
pub fn tip(response: egui::Response, name: &str, key: Option<Binding>, about: Option<&str>) -> egui::Response {
    response.on_hover_ui(|ui| tip_body(ui, name, key, about))
}

/// What `tip` shows.
pub fn tip_body(ui: &mut egui::Ui, name: &str, key: Option<Binding>, about: Option<&str>) {
    ui.set_max_width(280.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(name).font(semibold(13.0)));
        if let Some(b) = key {
            ui.add_space(8.0);
            binding_caps(ui, b);
        }
    });
    if let Some(a) = about {
        ui.add_space(-2.0);
        ui.label(egui::RichText::new(a).small().color(TEXT_MUTED));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// egui's own Ctrl+= / Ctrl+- / Ctrl+0 zoom is off: the keys reach
    /// the app (bound to its actions) and the UI keeps its size.
    #[test]
    fn ctrl_plus_minus_and_zero_are_left_to_the_app() {
        let ctx = egui::Context::default();
        apply(&ctx);
        let ctrl = egui::Modifiers::CTRL;
        for key in [egui::Key::Equals, egui::Key::Plus, egui::Key::Minus, egui::Key::Num0] {
            let input = egui::RawInput {
                events: vec![egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: ctrl }],
                modifiers: ctrl,
                ..Default::default()
            };
            let mut seen = false;
            let _ = ctx.run(input, |ctx| {
                seen = ctx.input_mut(|i| i.consume_shortcut(&egui::KeyboardShortcut::new(ctrl, key)));
            });
            assert!(seen, "{key:?} reached the app");
            assert_eq!(ctx.zoom_factor(), 1.0, "{key:?} left the UI size alone");
        }
    }

    /// A disabled UI (the layer panel on a locked slide, the UI size's
    /// − at its smallest) still draws, faded.
    #[test]
    fn disabled_ui_still_draws() {
        let ctx = egui::Context::default();
        apply(&ctx);
        let out = ctx.run(egui::RawInput::default(), |ctx| {
            egui::Area::new(egui::Id::new("t")).show(ctx, |ui| {
                ui.add_enabled_ui(false, |ui| {
                    ui.painter().rect_filled(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(10.0, 10.0)), 0.0, TEXT_PRIMARY);
                });
            });
        });
        assert!(!out.shapes.is_empty(), "the disabled UI drew nothing");
    }

    /// What keeps the wheel over a dropdown from zooming the slide
    /// (`app::keys`): the pointer over a popup is over an area, over the
    /// canvas (a background panel) it is not.
    #[test]
    fn a_popup_is_an_area_and_the_canvas_is_not() {
        let ctx = egui::Context::default();
        apply(&ctx);
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let mut over = Vec::new();
        for p in [egui::pos2(50.0, 50.0), egui::pos2(600.0, 500.0)] {
            for _ in 0..3 {
                let input = egui::RawInput { screen_rect: Some(screen), events: vec![egui::Event::PointerMoved(p)], ..Default::default() };
                let _ = ctx.run(input, |ctx| {
                    egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| {
                        ui.allocate_rect(ui.max_rect(), egui::Sense::click_and_drag());
                    });
                    egui::Area::new(egui::Id::new("dropdown")).fixed_pos(egui::pos2(20.0, 20.0)).order(egui::Order::Foreground).show(ctx, |ui| {
                        egui::ScrollArea::vertical().max_height(100.0).show(ui, |ui| {
                            for i in 0..20 {
                                ui.label(format!("row {i}"));
                            }
                        });
                    });
                });
            }
            over.push(pointer_over_ui(&ctx));
        }
        assert_eq!(over, [true, false], "over the dropdown, then over the canvas");
    }

    #[test]
    fn segmented_and_switch_change_on_click() {
        let ctx = egui::Context::default();
        apply(&ctx);
        let mut on = false;
        let mut pick = 0;
        // One frame; where the switch and the picker were drawn.
        let frame = |events: Vec<egui::Event>, on: &mut bool, pick: &mut i32| {
            let mut rects = (egui::Rect::NOTHING, egui::Rect::NOTHING);
            let input = egui::RawInput { events, screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 300.0))), ..Default::default() };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    rects.0 = switch(ui, on).rect;
                    let top = ui.cursor().top();
                    segmented(ui, pick, &[(0, "One"), (1, "Two")], false);
                    rects.1 = egui::Rect::from_min_max(egui::pos2(ui.min_rect().left(), top), egui::pos2(ui.min_rect().right(), ui.cursor().top()));
                });
            });
            rects
        };
        let (sw, seg) = frame(vec![], &mut on, &mut pick);
        let click = |p: egui::Pos2| {
            vec![
                egui::Event::PointerMoved(p),
                egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() },
                egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() },
            ]
        };
        frame(click(sw.center()), &mut on, &mut pick);
        assert!(on, "the switch turned on");
        // The second segment: right of the picker's middle.
        frame(click(egui::pos2(seg.left() + 60.0, seg.center().y)), &mut on, &mut pick);
        assert_eq!(pick, 1, "the second segment was picked");
    }
}

/// A curated colour palette for the swatches in the toolbar. Picked for
/// high contrast on arbitrary backgrounds + decent print appearance.
///
/// Note: kept here as a reference / hard-coded fallback only; the live
/// palette now lives in `AppConfig.palette` so users can edit and persist
/// their own. See `config::default_palette()` for the runtime defaults.
#[allow(dead_code)]
pub const PALETTE: &[[u8; 4]] = &[
    [232, 71,  71,  255], // red (default accent)
    [255, 158, 35,  255], // orange
    [240, 215, 55,  255], // yellow
    [83,  201, 91,  255], // green
    [40,  165, 235, 255], // sky blue
    [120, 100, 230, 255], // violet
    [240, 240, 240, 255], // white
    [30,  30,  35,  255], // near-black
];
