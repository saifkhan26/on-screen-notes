//! Centralised visual style — colours, spacing, rounding, font scale.
//!
//! We apply this once at app startup via `apply(&egui::Context)` so every
//! widget inherits the same look.
//!
//! Design intent:
//!   * Dark, glassy. The window is transparent so we want the floating
//!     toolbar / status pills to *clearly* belong to our app — small,
//!     high-contrast, rounded.
//!   * Generous corner radius (8–12 px) so floating panels look like cards.
//!   * Subtle shadows to separate floating UI from the canvas.
//!   * Accent colour for active tools / interactive state.

use egui::{Color32, Margin, Rounding, Shadow, Stroke};

/// Active-state fill — translucent white. Used for selected tool /
/// chip / swatch ring. No saturated hue: keeps the UI monochrome.
/// `from_white_alpha` returns the *correct* premultiplied form
/// (rgb == α), so the blend reads as actual ~15 % white. The plain
/// `from_rgba_premultiplied(255,255,255,α)` form is invalid premul
/// (rgb > α) and egui renders it as fully opaque white.
// `from_white_alpha` is not `const fn` in epaint 0.29, so we expand
// the premultiplied form inline: rgb == α for translucent white.
pub const ACCENT: Color32 = Color32::from_rgba_premultiplied(38, 38, 38, 38);

/// Glass-panel fill — flat near-black at moderate alpha. Sits over
/// arbitrary desktop content without colour cast.
pub const GLASS_FILL: Color32 = Color32::from_rgba_premultiplied(10, 10, 10, 220);
/// Glass-panel border — set to fully transparent so cards have no
/// visible outline. Border removed by request.
#[allow(dead_code)]
pub const GLASS_STROKE: Color32 = Color32::TRANSPARENT;

/// Foreground text — pure neutral white at full alpha.
pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(240, 240, 240);
/// Secondary / muted text (slider readouts, tooltips).
pub const TEXT_MUTED:   Color32 = Color32::from_rgb(170, 170, 170);

/// A soft drop-shadow used by floating cards. Lifted, soft blur.
pub fn card_shadow() -> Shadow {
    Shadow {
        offset: egui::vec2(0.0, 4.0),
        blur: 22.0,
        spread: 0.0,
        color: Color32::from_black_alpha(120),
    }
}

/// A `Frame` styled like a glass card — for the floating toolbar / pills.
/// No border (user-requested minimal black-and-white aesthetic); the
/// drop shadow alone separates the card from the canvas.
pub fn card_frame() -> egui::Frame {
    egui::Frame {
        inner_margin: Margin::symmetric(8.0, 5.0),
        outer_margin: Margin::ZERO,
        rounding: Rounding::same(12.0),
        shadow: card_shadow(),
        fill: GLASS_FILL,
        stroke: Stroke::NONE,
    }
}

/// Apply the project-wide style. Called once on startup.
pub fn apply(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();

    // Visuals: dark theme baseline, then tweak.
    style.visuals = egui::Visuals::dark();
    style.visuals.override_text_color = Some(TEXT_PRIMARY);
    style.visuals.window_fill = GLASS_FILL;
    style.visuals.panel_fill = Color32::TRANSPARENT;
    style.visuals.window_stroke = Stroke::NONE;
    style.visuals.window_rounding = Rounding::same(12.0);
    style.visuals.window_shadow = card_shadow();

    // Spacing: slightly more generous so the toolbar feels modern, not
    // cramped. Targets are big enough for pen-tip taps yet stay compact.
    style.spacing.button_padding = egui::vec2(8.0, 4.0);
    style.spacing.item_spacing   = egui::vec2(6.0, 4.0);
    style.spacing.icon_width     = 18.0;
    style.spacing.icon_spacing   = 5.0;
    style.spacing.slider_width   = 100.0;

    // Selection / active states: translucent white only — no hue.
    style.visuals.selection.bg_fill = ACCENT;
    style.visuals.selection.stroke = Stroke::NONE;
    style.visuals.hyperlink_color  = TEXT_PRIMARY;

    // Slider track / knob: tighten + monochrome.
    style.visuals.widgets.inactive.bg_fill = Color32::from_white_alpha(22);
    style.visuals.widgets.hovered.bg_fill  = Color32::from_white_alpha(36);
    style.visuals.widgets.active.bg_fill   = Color32::from_white_alpha(70);
    style.visuals.widgets.inactive.bg_stroke = Stroke::NONE;
    style.visuals.widgets.hovered.bg_stroke  = Stroke::NONE;
    style.visuals.widgets.active.bg_stroke   = Stroke::NONE;
    style.visuals.widgets.inactive.rounding = Rounding::same(8.0);
    style.visuals.widgets.hovered.rounding  = Rounding::same(8.0);
    style.visuals.widgets.active.rounding   = Rounding::same(8.0);

    style.visuals.clip_rect_margin = 0.0;
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
