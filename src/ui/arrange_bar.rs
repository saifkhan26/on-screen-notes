//! Arrange bar: shown under the toolbar while objects are selected.
//! Packs, groups by type, aligns (piled up or kept apart), distributes
//! and evens out the sizes of the selection (see `tools::arrange`) when
//! there are two or more; flips it and zooms to it; shows a lone
//! picture's colours (`tools::picture_colours`) in a row of its own;
//! copies it to a new slide, moves it to another slide, and pins it
//! (see `canvas::pins`), and tags it for find (Ctrl+F) to know it by.
//! A selected pin gets a bar of its own in the
//! same place, which also shows it on one slide or every slide.

use crate::canvas::CanvasManager;
use crate::input::hotkeys_config::{Binding, ParsedHotkeys};
use crate::tools::arrange::{Align, Arrange, Match, PackMode};
use crate::ui::theme;
use egui::{Color32, Rounding, Vec2};

const BTN: f32 = 28.0;

/// `tip`, followed by the keys that press the button when it has any.
fn with_key(tip: &str, key: Option<Binding>) -> String {
    match key {
        Some(b) => format!("{tip} — {}", b.label()),
        None => tip.to_string(),
    }
}

/// What a click on the bar asks for.
pub enum BarAction {
    Arrange(Arrange),
    /// Give every selected object this width or height.
    Match(Match),
    CopyToNewSlide,
    /// Move the selection to this slide (an index).
    MoveToSlide(usize),
    /// Pin the selection to every slide, or unpin it if all of it is.
    TogglePin,
    /// Mirror the selection left to right (`false`) or top to bottom.
    Flip(bool),
    /// Turn the selection a quarter turn right (`true`, clockwise) or left.
    Rotate(bool),
    ZoomToSelection,
    /// Show the selected picture's colours, or hide them.
    PictureColours,
    /// Draw with this colour.
    UseColour([u8; 4]),
    /// The picture's colours become the toolbar's palette.
    KeepColours,
    /// The palette from before `KeepColours` comes back.
    RestorePalette,
    /// Make the selection one group (`true`), or take it out of its groups.
    Group(bool),
    /// Give every selected object these tags in place of its own.
    SetTags(Vec<String>),
}

/// The tags being typed for the selection, while the tag row is open.
pub struct TagEdit {
    pub text: String,
    /// The field has yet to take the keyboard.
    pub focus: bool,
}

impl TagEdit {
    /// Start from the selection's tags, `current`.
    pub fn new(current: &[String]) -> Self {
        TagEdit { text: current.join(", "), focus: true }
    }
}

/// The palette button's part, for a selection that is one picture.
pub struct ColourRow<'a> {
    /// Its colours, when they are showing.
    pub shown: Option<&'a [[u8; 4]]>,
    /// A palette replaced by `KeepColours` can come back.
    pub can_restore: bool,
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
/// `apart` is the "keep apart" switch: on, the align buttons slide
/// objects until they touch rather than piling them up; the bar flips
/// it when clicked. `pack` is how Pack lays objects out, picked from the
/// arrow beside it. `keys` label the buttons' tooltips; `open_move`
/// opens "Move to slide…" as its key asks; `picture` is set when the
/// selection is one picture, for its palette button; `grouped` when some
/// of it is in a group, for the ungroup button. `tags` are the
/// selection's, shown beside the tag button; `tag_edit` is the row
/// for typing them while it is open. Returns what was clicked this
/// frame, if anything.
#[allow(clippy::too_many_arguments)]
pub fn show(
    ctx: &egui::Context,
    count: usize,
    pinned: bool,
    slides: &CanvasManager,
    apart: &mut bool,
    pack: &mut PackMode,
    keys: &ParsedHotkeys,
    open_move: bool,
    picture: Option<ColourRow<'_>>,
    grouped: bool,
    tags: &[String],
    tag_edit: &mut Option<TagEdit>,
) -> Option<BarAction> {
    let pos = bar_pos(ctx);
    let mut picked = None;
    egui::Area::new(egui::Id::new("osn_arrange_bar"))
        .fixed_pos(pos)
        .pivot(egui::Align2::CENTER_TOP)
        .order(egui::Order::Middle)
        .show(ctx, |ui| {
            theme::card_frame().inner_margin(egui::Margin::symmetric(8.0, 4.0)).show(ui, |ui| {
                ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    ui.label(egui::RichText::new(format!("{count} selected")).small().color(theme::TEXT_MUTED));
                    ui.add_space(6.0);
                    if count >= 2 {
                        arrange_buttons(ui, count, apart, pack, keys, &mut picked);
                        divider(ui);
                    }
                    let k = &keys.arrange;
                    if count >= 2 {
                        let tip = with_key("Group — the selection selects, moves and arranges as one (Alt+click takes a single object)", keys.canvas.group);
                        if icon_btn(ui, egui::include_image!("../../assets/icons/group.svg"), true, &tip) {
                            picked = Some(BarAction::Group(true));
                        }
                    }
                    if grouped {
                        let tip = with_key("Ungroup — the objects select and move on their own again", keys.canvas.ungroup);
                        if icon_btn(ui, egui::include_image!("../../assets/icons/ungroup.svg"), true, &tip) {
                            picked = Some(BarAction::Group(false));
                        }
                    }
                    if count >= 2 || grouped {
                        divider(ui);
                    }
                    let tip = with_key("Flip left to right — about the selection's middle", k.flip_horizontal);
                    if icon_btn(ui, egui::include_image!("../../assets/icons/flip-horizontal.svg"), true, &tip) {
                        picked = Some(BarAction::Flip(false));
                    }
                    let tip = with_key("Flip top to bottom — about the selection's middle", k.flip_vertical);
                    if icon_btn(ui, egui::include_image!("../../assets/icons/flip-vertical.svg"), true, &tip) {
                        picked = Some(BarAction::Flip(true));
                    }
                    let tip = with_key("Turn left 90° — about the selection's middle", k.rotate_left);
                    if icon_btn(ui, egui::include_image!("../../assets/icons/rotate-ccw.svg"), true, &tip) {
                        picked = Some(BarAction::Rotate(false));
                    }
                    let tip = with_key("Turn right 90° — about the selection's middle", k.rotate_right);
                    if icon_btn(ui, egui::include_image!("../../assets/icons/rotate-cw.svg"), true, &tip) {
                        picked = Some(BarAction::Rotate(true));
                    }
                    let tip = with_key("Zoom to the selection", keys.canvas.zoom_to_selection);
                    if icon_btn(ui, egui::include_image!("../../assets/icons/zoom-selection.svg"), true, &tip) {
                        picked = Some(BarAction::ZoomToSelection);
                    }
                    if let Some(row) = &picture {
                        let tip = with_key("Colours in this picture — draw with one, or keep them all as the palette", k.picture_colours);
                        if toggle_btn(ui, egui::include_image!("../../assets/icons/palette.svg"), row.shown.is_some(), &tip) {
                            picked = Some(BarAction::PictureColours);
                        }
                    }
                    divider(ui);
                    let tip = with_key("Copy to a new slide (added after the last slide)", keys.canvas.copy_to_new_slide);
                    if icon_btn(ui, egui::include_image!("../../assets/icons/copy-to-slide.svg"), true, &tip) {
                        picked = Some(BarAction::CopyToNewSlide);
                    }
                    if let Some(to) = move_btn(ui, slides, keys.arrange.move_to_slide, open_move) {
                        picked = Some(BarAction::MoveToSlide(to));
                    }
                    let tip = if pinned {
                        "Unpin — stop showing these as a pin"
                    } else {
                        "Pin to every slide — the objects stay here, and a pin you can move and resize shows them on every slide; its bar can keep it to this slide"
                    };
                    let tip = with_key(tip, keys.canvas.pin_selection);
                    if toggle_btn(ui, egui::include_image!("../../assets/icons/pin.svg"), pinned, &tip) {
                        picked = Some(BarAction::TogglePin);
                    }
                    let tip = with_key("Tags — words find (Ctrl+F) knows the selection by", k.tag);
                    let mut open = toggle_btn(ui, egui::include_image!("../../assets/icons/tag.svg"), tag_edit.is_some(), &tip);
                    if !tags.is_empty() && tag_edit.is_none() {
                        open |= tag_chips(ui, tags);
                    }
                    if open {
                        *tag_edit = match tag_edit {
                            Some(_) => None,
                            None => Some(TagEdit::new(tags)),
                        };
                    }
                });
                if let Some(edit) = tag_edit.as_mut() {
                    match tag_row(ui, edit) {
                        Some(Some(typed)) => {
                            picked = Some(BarAction::SetTags(crate::canvas::shape::parse_tags(&typed)));
                            *tag_edit = None;
                        }
                        Some(None) => *tag_edit = None,
                        None => {}
                    }
                }
                if let Some(row) = &picture {
                    if let Some(colours) = row.shown {
                        colour_row(ui, colours, row.can_restore, &mut picked);
                    }
                }
                });
            });
        });
    picked
}

/// The selection's tags beside the tag button, cut short if long; a
/// click opens them for typing, as the button does.
fn tag_chips(ui: &mut egui::Ui, tags: &[String]) -> bool {
    let all = crate::ui::media_filter::tags_text(tags, usize::MAX);
    let text = egui::RichText::new(crate::ui::media_filter::tags_text(tags, 32)).small().color(crate::ui::media_filter::TAG_COLOUR);
    ui.add(egui::Label::new(text).sense(egui::Sense::click())).on_hover_text(format!("{all} — click to change")).clicked()
}

/// The row for typing the selection's tags. `Some(Some(text))` once
/// `Enter` saves them, `Some(None)` when `Esc` or a click elsewhere
/// leaves them as they were.
fn tag_row(ui: &mut egui::Ui, edit: &mut TagEdit) -> Option<Option<String>> {
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Tags").small().color(theme::TEXT_MUTED));
        let field = egui::TextEdit::singleline(&mut edit.text)
            .hint_text("pose, walk cycle — commas between, Enter saves")
            .desired_width(300.0);
        let r = ui.add(field);
        if std::mem::take(&mut edit.focus) {
            r.request_focus();
            return None;
        }
        if r.lost_focus() {
            let saved = ui.input(|i| i.key_pressed(egui::Key::Enter));
            return Some(saved.then(|| edit.text.clone()));
        }
        None
    })
    .inner
}

/// The picture's colours under the bar's buttons: click one to draw
/// with it; keep them all as the palette, or bring the old one back.
fn colour_row(ui: &mut egui::Ui, colours: &[[u8; 4]], can_restore: bool, picked: &mut Option<BarAction>) {
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if colours.is_empty() {
            ui.label(egui::RichText::new("No colours: the picture is see-through").small().color(theme::TEXT_MUTED));
            return;
        }
        ui.label(egui::RichText::new("Its colours").small().color(theme::TEXT_MUTED));
        for &c in colours {
            let (rect, resp) = ui.allocate_exact_size(Vec2::splat(22.0), egui::Sense::click());
            let fill = Color32::from_rgba_unmultiplied(c[0], c[1], c[2], 255);
            let body = if resp.hovered() { rect.shrink(1.0) } else { rect.shrink(2.5) };
            ui.painter().rect_filled(body, Rounding::same(5.0), fill);
            ui.painter().rect_stroke(body, Rounding::same(5.0), egui::Stroke::new(1.0, Color32::from_white_alpha(40)));
            let tip = format!("#{:02X}{:02X}{:02X} — click to draw with it", c[0], c[1], c[2]);
            if resp.on_hover_text(tip).clicked() {
                *picked = Some(BarAction::UseColour(c));
            }
        }
        divider(ui);
        if ui.button("Use as palette").on_hover_text("These colours replace the toolbar's swatches").clicked() {
            *picked = Some(BarAction::KeepColours);
        }
        if can_restore && ui.button("Restore palette").on_hover_text("Bring back the swatches from before").clicked() {
            *picked = Some(BarAction::RestorePalette);
        }
    });
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
/// like the slide window's picker; `key` presses it, and `open_now` is
/// it pressed. The slide picked, if one was.
fn move_btn(ui: &mut egui::Ui, slides: &CanvasManager, key: Option<Binding>, open_now: bool) -> Option<usize> {
    let popup = ui.make_persistent_id("osn_move_to_slide");
    let others = slides.count() > 1;
    if open_now && others {
        ui.memory_mut(|m| m.open_popup(popup));
    }
    let open = ui.memory(|m| m.is_popup_open(popup));
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
        with_key("Move to slide… — the objects leave this slide and land on the one you pick, where they show now", key)
    } else {
        "Move to slide… — there is no other slide yet".to_string()
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

/// Pack's icon for each way it packs.
fn pack_icon(mode: PackMode) -> egui::ImageSource<'static> {
    match mode {
        PackMode::Optimal => egui::include_image!("../../assets/icons/pack.svg"),
        PackMode::Horizontal => egui::include_image!("../../assets/icons/pack-rows.svg"),
        PackMode::Vertical => egui::include_image!("../../assets/icons/pack-columns.svg"),
    }
}

/// The arrow beside Pack: a list of the ways it can pack, the one in
/// use marked. The pick stays until another is made (it is saved).
fn pack_mode_menu(ui: &mut egui::Ui, mode: &mut PackMode) {
    let popup = ui.make_persistent_id("osn_pack_mode");
    let open = ui.memory(|m| m.is_popup_open(popup));
    let (rect, response) = ui.allocate_exact_size(Vec2::new(14.0, BTN), egui::Sense::click());
    let bg = if open {
        theme::ACCENT
    } else if response.hovered() {
        Color32::from_white_alpha(22)
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, Rounding::same(5.0), bg);
    let c = rect.center();
    ui.painter().add(egui::Shape::line(
        vec![c + Vec2::new(-3.5, -1.5), c + Vec2::new(0.0, 2.0), c + Vec2::new(3.5, -1.5)],
        egui::Stroke::new(1.5, theme::TEXT_PRIMARY),
    ));
    let response = response.on_hover_text(format!("How Pack lays objects out — now {}", mode.label().to_lowercase()));
    if response.clicked() {
        ui.memory_mut(|m| m.toggle_popup(popup));
    }
    let mut chosen = false;
    egui::popup_below_widget(ui, popup, &response, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
        ui.set_min_width(250.0);
        ui.spacing_mut().item_spacing = Vec2::new(0.0, 2.0);
        ui.label(egui::RichText::new("Pack").small().color(theme::TEXT_MUTED));
        for m in PackMode::ALL {
            let img = egui::Image::new(pack_icon(m)).tint(theme::TEXT_PRIMARY).fit_to_exact_size(Vec2::splat(16.0));
            let tip = format!("Pack {} — {}; sizes kept, on screen (zooming out if it must)", m.label().to_lowercase(), m.about());
            if ui.add(egui::Button::image_and_text(img, m.label()).selected(*mode == m).frame(false)).on_hover_text(tip).clicked() {
                *mode = m;
                chosen = true;
            }
        }
    });
    if chosen {
        ui.memory_mut(|m| m.close_popup());
    }
}

/// Pack, group, align, distribute and match sizes, for two or more
/// objects.
fn arrange_buttons(ui: &mut egui::Ui, count: usize, apart: &mut bool, pack: &mut PackMode, keys: &ParsedHotkeys, picked: &mut Option<BarAction>) {
    let k = &keys.arrange;
    let mut btn = |ui: &mut egui::Ui, op: Arrange, img: egui::ImageSource<'static>, tip: &str, key: Option<Binding>| {
        if icon_btn(ui, img, count >= op.min_items(), &with_key(tip, key)) {
            *picked = Some(BarAction::Arrange(op));
        }
    };
    let tip = format!(
        "Pack {} — {}; sizes kept, kept on screen (zooming out if it must). The arrow picks another way",
        pack.label().to_lowercase(),
        pack.about()
    );
    btn(ui, Arrange::Pack, pack_icon(*pack), &tip, k.pack);
    pack_mode_menu(ui, pack);
    btn(ui, Arrange::GroupByType, egui::include_image!("../../assets/icons/group-by-type.svg"),
        "Group by type — pictures, GIFs, videos and other objects each packed into their own cluster", k.group_by_type);
    divider(ui);
    let tip = if *apart {
        "Keep apart: on — aligning slides each object until it reaches the edge or bumps into another, so nothing ends up on top of anything. Click to let aligning pile objects up"
    } else {
        "Keep apart: off — aligning lines objects up exactly, even on top of each other. Click to have them slide until they touch instead"
    };
    if toggle_btn(ui, egui::include_image!("../../assets/icons/keep-apart.svg"), *apart, &with_key(tip, k.keep_apart)) {
        *apart = !*apart;
    }
    let align = |a: Align| if *apart { Arrange::Slide(a) } else { Arrange::Align(a) };
    let (edge, middle) = if *apart {
        ("slide until touching", "slide to the middle until touching")
    } else {
        ("line up edges", "line up centres")
    };
    btn(ui, align(Align::Left), egui::include_image!("../../assets/icons/align-left.svg"), &format!("Align left — {edge}"), k.align_left);
    btn(ui, align(Align::CenterX), egui::include_image!("../../assets/icons/align-center-x.svg"), &format!("Align centres horizontally — {middle}"), k.align_center_x);
    btn(ui, align(Align::Right), egui::include_image!("../../assets/icons/align-right.svg"), &format!("Align right — {edge}"), k.align_right);
    btn(ui, align(Align::Top), egui::include_image!("../../assets/icons/align-top.svg"), &format!("Align top — {edge}"), k.align_top);
    btn(ui, align(Align::CenterY), egui::include_image!("../../assets/icons/align-center-y.svg"), &format!("Align centres vertically — {middle}"), k.align_center_y);
    btn(ui, align(Align::Bottom), egui::include_image!("../../assets/icons/align-bottom.svg"), &format!("Align bottom — {edge}"), k.align_bottom);
    divider(ui);
    btn(ui, Arrange::DistributeX, egui::include_image!("../../assets/icons/distribute-x.svg"),
        "Distribute horizontally — equal gaps (3 or more objects)", k.distribute_x);
    btn(ui, Arrange::DistributeY, egui::include_image!("../../assets/icons/distribute-y.svg"),
        "Distribute vertically — equal gaps (3 or more objects)", k.distribute_y);
    divider(ui);
    let mut size = |ui: &mut egui::Ui, m: Match, img: egui::ImageSource<'static>, tip: &str, key: Option<Binding>| {
        if icon_btn(ui, img, true, &with_key(tip, key)) {
            *picked = Some(BarAction::Match(m));
        }
    };
    size(ui, Match::NarrowestWidth, egui::include_image!("../../assets/icons/match-width-min.svg"),
        "Same width as the narrowest — each object scales, keeping its proportions and its top-left corner", k.match_narrowest_width);
    size(ui, Match::WidestWidth, egui::include_image!("../../assets/icons/match-width-max.svg"),
        "Same width as the widest — each object scales, keeping its proportions and its top-left corner", k.match_widest_width);
    size(ui, Match::ShortestHeight, egui::include_image!("../../assets/icons/match-height-min.svg"),
        "Same height as the shortest — each object scales, keeping its proportions and its top-left corner", k.match_shortest_height);
    size(ui, Match::TallestHeight, egui::include_image!("../../assets/icons/match-height-max.svg"),
        "Same height as the tallest — each object scales, keeping its proportions and its top-left corner", k.match_tallest_height);
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
