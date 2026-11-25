//! Arrange bar: shown under the toolbar while objects are selected.
//! Packs, groups by type, aligns (piled up or kept apart), distributes
//! and evens out the sizes of the selection (see `tools::arrange`) when
//! there are two or more; flips it and zooms to it; shows a lone
//! picture's colours (`tools::picture_colours`) in a row of its own;
//! copies it to a new slide, moves it to another slide, and pins it
//! (see `canvas::pins`), and tags it for find (Ctrl+F) to know it by. Its
//! "dim the rest" switch fades everything that is not selected.
//! A selected pin gets a bar of its own in the
//! same place, which also shows it on one slide or every slide.

use crate::canvas::CanvasManager;
use crate::input::hotkeys_config::{Binding, ParsedHotkeys};
use crate::tools::arrange::{Align, Arrange, Match, PackMode};
use crate::ui::theme;
use egui::{Color32, Rounding, Vec2};

const BTN: f32 = 30.0;

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
    /// The selection's pins show on this slide only (`true`) or on every
    /// slide.
    PinScope(bool),
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
    /// Show the selection in the slide window.
    OpenInWindow,
    /// Show the selected picture's colours, or hide them.
    PictureColours,
    /// Find pictures like the selected one.
    FindSimilar,
    /// Crop the selected picture.
    Crop,
    /// Draw with this colour.
    UseColour([u8; 4]),
    /// The picture's colours become the toolbar's palette.
    KeepColours,
    /// The palette from before `KeepColours` comes back.
    RestorePalette,
    /// Make the selection one group (`true`), or take it out of its groups.
    Group(bool),
    /// The tag row was saved: it showed tags `was` and now holds `now`
    /// (see `parse_row`). Each object's tags change by the difference.
    SetTags { was: Vec<String>, now: Vec<(String, bool)> },
    /// Find everything tagged so (a click on one of the selection's tags).
    FindTag(String),
    /// Tag the selected pictures posted by this handle with it (a click
    /// on a `FromPost` chip).
    TagFromPost(String),
    /// Whoever posted the selected pictures under this handle is not who
    /// made them: stop offering it (a right-click on the chip).
    NotFromPost(String),
    /// Find who made the selected pictures, in the browser.
    FindArtist,
}

/// A tag offered for the selection because the pictures were sent from
/// the browser off a post by this handle (`browser_link`): only offered,
/// never put on by itself.
#[derive(Clone, Debug, PartialEq)]
pub struct FromPost {
    /// The handle, spelled as a tag the app has already if one matches.
    pub tag: String,
    /// Where it was posted: "X", "Instagram".
    pub site: String,
    /// How many selected pictures were posted by it and lack the tag.
    pub count: usize,
    /// No object has the tag yet.
    pub new: bool,
}

impl FromPost {
    /// The chip's tooltip.
    fn tip(&self) -> String {
        let site = if self.site.is_empty() { String::new() } else { format!(" on {}", self.site) };
        let what = if self.count == 1 { "it".to_string() } else { format!("these {}", self.count) };
        let mut tip = format!("Posted by @{}{site} — click to tag {what} #{}. Right-click if the poster is not the artist", self.tag, self.tag);
        if self.new {
            tip.push_str("
A new tag: nothing has it yet");
        }
        tip
    }
}

/// What the tag row holds (as `shape::parse_tags` reads it): each tag
/// with whether it is marked `tag*`, on only some of the selection and
/// left that way. A tag typed both ways goes on all of it.
pub fn parse_row(text: &str) -> Vec<(String, bool)> {
    use crate::canvas::units::same_tag;
    let mut out: Vec<(String, bool)> = Vec::new();
    for t in crate::canvas::shape::parse_tags(text) {
        let bare = t.trim_end_matches('*');
        if bare.is_empty() {
            continue;
        }
        let some = bare.len() < t.len();
        match out.iter_mut().find(|(o, _)| same_tag(o, bare)) {
            Some(e) => e.1 &= some,
            None => out.push((bare.to_string(), some)),
        }
    }
    out
}

/// The tags being typed for the selection, while the tag row is open.
pub struct TagEdit {
    pub text: String,
    /// The selection's tags when the row opened: one taken out of the
    /// row comes off every object.
    pub was: Vec<String>,
    /// The field has yet to take the keyboard (and its caret goes to
    /// the end).
    pub focus: bool,
    /// Every tag in the app with how many objects carry it, most used
    /// first: what the row suggests. The app fills it in.
    pub known: Vec<(String, usize)>,
    /// Tags the selected picture probably has, and how closely it fits
    /// each (`tools::tag_suggest::fits_bars`), most likely first. The app fills
    /// it in while picture search is on.
    pub likely: Vec<(String, f32)>,
    /// The handles the selected pictures were posted by
    /// (`FromPost`). The app fills it in.
    pub from_post: Vec<FromPost>,
}

impl TagEdit {
    /// Start from the selection's tags, `current`, ready for one more.
    /// One only some of the selection carry (`.1`) is written `tag*`.
    pub fn new(current: &[(String, bool)]) -> Self {
        let mut text = current.iter().map(|(t, some)| if *some { format!("{t}*") } else { t.clone() }).collect::<Vec<_>>().join(" ");
        if !text.is_empty() {
            text.push(' ');
        }
        TagEdit { text, was: current.iter().map(|(t, _)| t.clone()).collect(), focus: true, known: Vec::new(), likely: Vec::new(), from_post: Vec::new() }
    }

    /// Where the tag being typed starts: after the last space or comma.
    fn word_start(&self) -> usize {
        self.text.rfind(|c: char| c == ',' || c.is_whitespace()).map_or(0, |i| i + 1)
    }

    /// Known tags that fit what is being typed and are not typed yet:
    /// those starting with it first, then those holding it, each group
    /// most used first. With nothing typed, the most used.
    pub fn suggestions(&self, most: usize) -> Vec<String> {
        use crate::canvas::units::same_tag;
        let start = self.word_start();
        let have = parse_row(&self.text[..start]);
        let word = self.text[start..].trim_start_matches('#').to_lowercase();
        let fresh = self.known.iter().filter(|(t, _)| !have.iter().any(|(h, _)| same_tag(h, t)));
        let (mut first, mut then): (Vec<_>, Vec<_>) = fresh.filter(|(t, _)| t.to_lowercase().contains(&word)).partition(|(t, _)| t.to_lowercase().starts_with(&word));
        first.sort_by(|a, b| b.1.cmp(&a.1));
        then.sort_by(|a, b| b.1.cmp(&a.1));
        first.into_iter().chain(then).take(most).map(|(t, _)| t.clone()).collect()
    }

    /// Put `tag` in place of the tag being typed, ready for the next.
    pub fn accept(&mut self, tag: &str) {
        let start = self.word_start();
        self.text.truncate(start);
        self.text.push_str(tag);
        self.text.push(' ');
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
/// it when clicked. `dim` is the "dim the rest" switch: on, everything
/// not selected fades back; the bar flips it too. `pack` is how Pack lays objects out, picked from the
/// arrow beside it. `keys` label the buttons' tooltips; `open_move`
/// opens "Move to slide…" as its key asks; `picture` is set when the
/// selection is one picture, for its palette button; `grouped` when some
/// of it is in a group, for the ungroup button. `tags` are the
/// selection's (`.1` set for one only some of it carries), shown beside
/// the tag button; `from_post` the tags offered for it because of who
/// posted its pictures (`FromPost`), shown after them; `tag_edit` is the
/// row for typing them while it is open. Returns what was clicked this
/// frame, if anything.
#[allow(clippy::too_many_arguments)]
pub fn show(
    ctx: &egui::Context,
    count: usize,
    pinned: bool,
    // While `pinned`: the pins show on this slide only (`Some(true)`), on
    // every slide (`Some(false)`), or some each way (`None`).
    pin_local: Option<bool>,
    slides: &CanvasManager,
    apart: &mut bool,
    dim: &mut bool,
    pack: &mut PackMode,
    keys: &ParsedHotkeys,
    open_move: bool,
    picture: Option<ColourRow<'_>>,
    grouped: bool,
    tags: &[(String, bool)],
    from_post: &[FromPost],
    // Some of the selection is a picture, GIF or video: "Find artist" shows.
    pictures: bool,
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
                        arrange_buttons(ui, count, !tags.is_empty(), apart, pack, keys, &mut picked);
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
                    let tip = if *dim {
                        "Dim the rest: on — while objects are selected, everything else fades back. Click to stop"
                    } else {
                        "Dim the rest: off — click to fade everything that is not selected, so it is plain what is"
                    };
                    if toggle_btn(ui, egui::include_image!("../../assets/icons/dim-rest.svg"), *dim, &with_key(tip, k.dim_unselected)) {
                        *dim = !*dim;
                    }
                    if let Some(row) = &picture {
                        let tip = with_key("Colours in this picture — draw with one, or keep them all as the palette", k.picture_colours);
                        if toggle_btn(ui, egui::include_image!("../../assets/icons/palette.svg"), row.shown.is_some(), &tip) {
                            picked = Some(BarAction::PictureColours);
                        }
                        let tip = with_key("Find similar — copies of this picture (resized, recoloured, mirrored or turned) and pictures like it, on every slide", k.find_similar);
                        if icon_btn(ui, egui::include_image!("../../assets/icons/find-similar.svg"), true, &tip) {
                            picked = Some(BarAction::FindSimilar);
                        }
                        let tip = with_key("Crop — drag its edges or corners; Enter crops, Esc cancels", k.crop);
                        if icon_btn(ui, egui::include_image!("../../assets/icons/crop.svg"), true, &tip) {
                            picked = Some(BarAction::Crop);
                        }
                    }
                    if pictures {
                        let tip = "Find the artist — the browser extension looks the picture up with Google Lens, and you pick who made it there";
                        if icon_btn(ui, egui::include_image!("../../assets/icons/find-artist.svg"), true, tip) {
                            picked = Some(BarAction::FindArtist);
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
                    let tip = with_key("Show in the slide window — it pops out over the selection, or the open one shows it", k.open_in_window);
                    if icon_btn(ui, egui::include_image!("../../assets/icons/open-in-window.svg"), true, &tip) {
                        picked = Some(BarAction::OpenInWindow);
                    }
                    let tip = if pinned {
                        "Unpin — stop showing these as a pin"
                    } else {
                        "Pin — the objects stay here, and the pins window shows them on every slide; or keep the pin to this slide, on the main window, from the choice that comes up"
                    };
                    let tip = with_key(tip, keys.canvas.pin_selection);
                    if toggle_btn(ui, egui::include_image!("../../assets/icons/pin.svg"), pinned, &tip) {
                        picked = Some(BarAction::TogglePin);
                    }
                    // Where a pin of these shows: this slide or every one.
                    if pinned {
                        let only = with_key("Show the pin on this slide only, here on the main window", keys.canvas.pin_this_slide_only);
                        if ui.selectable_label(pin_local == Some(true), "This slide").on_hover_text(only).clicked() {
                            picked = Some(BarAction::PinScope(true));
                        }
                        let every = with_key("Show the pin on every slide, in the pins window", keys.canvas.pin_this_slide_only);
                        if ui.selectable_label(pin_local == Some(false), "Every slide").on_hover_text(every).clicked() {
                            picked = Some(BarAction::PinScope(false));
                        }
                    }
                    let tip = with_key("Tags — words find (Ctrl+F) knows the selection by", k.tag);
                    let mut open = toggle_btn(ui, egui::include_image!("../../assets/icons/tag.svg"), tag_edit.is_some(), &tip);
                    if !tags.is_empty() && tag_edit.is_none() {
                        match tag_chips(ui, tags) {
                            Some(ChipPick::Find(tag)) => picked = Some(BarAction::FindTag(tag)),
                            Some(ChipPick::Edit) => open = true,
                            None => {}
                        }
                    }
                    if tag_edit.is_none() {
                        if let Some(a) = from_post_chips(ui, from_post) {
                            picked = Some(a);
                        }
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
                            picked = Some(BarAction::SetTags { was: std::mem::take(&mut edit.was), now: parse_row(&typed) });
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

/// Room for the tags beside the tag button, in characters.
const CHIP_ROOM: usize = 32;

/// What a click on the tags beside the tag button asks for.
enum ChipPick {
    /// Find everything tagged so.
    Find(String),
    /// Open the tags for typing, as the button does.
    Edit,
}

/// How many of `tags` (`#tag` each, a space between) fit in `room`
/// characters; the first always does, cut short if long.
fn chips_that_fit(tags: &[String], room: usize) -> usize {
    let mut used = 0;
    tags.iter()
        .take_while(|t| {
            let len = t.chars().count() + 1;
            let fits = used == 0 || used + 1 + len <= room;
            used += if used == 0 { len } else { 1 + len };
            fits
        })
        .count()
}

/// The selection's tags beside the tag button, one only some of it
/// carries marked `*`, as many as fit: a click on one finds everything
/// tagged so (as `#tag` typed in find would); on the count of the rest,
/// opens them for typing, as the button does.
fn tag_chips(ui: &mut egui::Ui, tags: &[(String, bool)]) -> Option<ChipPick> {
    use crate::ui::media_filter::tags_text;
    let names: Vec<String> = tags.iter().map(|(t, some)| if *some { format!("{t}*") } else { t.clone() }).collect();
    let fit = chips_that_fit(&names, CHIP_ROOM);
    let mut out = None;
    ui.add_space(2.0);
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.spacing_mut().button_padding = egui::vec2(6.0, 2.0);
        for ((tag, some), name) in tags.iter().zip(&names).take(fit) {
            let text = egui::RichText::new(tags_text(std::slice::from_ref(name), CHIP_ROOM)).small().color(theme::TEXT_PRIMARY);
            let mut tip = format!("Find everything tagged #{tag} — the tag button changes the tags");
            if *some {
                tip.push_str("\n* only some of the selection has it");
            }
            let r = ui.add(egui::Button::new(text).rounding(Rounding::same(9.0))).on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(tip);
            if r.clicked() {
                out = Some(ChipPick::Find(tag.clone()));
            }
        }
        if fit < tags.len() {
            let text = egui::RichText::new(format!("+{}", tags.len() - fit)).small().color(theme::TEXT_MUTED);
            let tip = format!("{} — click to change", tags_text(&names[fit..], usize::MAX));
            if ui.add(egui::Label::new(text).sense(egui::Sense::click())).on_hover_text(tip).clicked() {
                out = Some(ChipPick::Edit);
            }
        }
    });
    out
}

/// The tags offered because of who posted the selected pictures, after
/// the selection's own: `+#handle`, a click puts it on them, a
/// right-click says the poster is not the artist.
fn from_post_chips(ui: &mut egui::Ui, from_post: &[FromPost]) -> Option<BarAction> {
    if from_post.is_empty() {
        return None;
    }
    let mut out = None;
    ui.add_space(2.0);
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.spacing_mut().button_padding = egui::vec2(6.0, 2.0);
        for fp in from_post.iter().take(3) {
            let mut label = format!("+{}", crate::ui::media_filter::tags_text(std::slice::from_ref(&fp.tag), CHIP_ROOM));
            if fp.count > 1 {
                label.push_str(&format!(" ({})", fp.count));
            }
            let text = egui::RichText::new(label).small().color(theme::TEXT_MUTED);
            let r = ui
                .add(egui::Button::new(text).rounding(Rounding::same(9.0)).fill(Color32::TRANSPARENT))
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text(fp.tip());
            if r.clicked() {
                out = Some(BarAction::TagFromPost(fp.tag.clone()));
            } else if r.secondary_clicked() {
                out = Some(BarAction::NotFromPost(fp.tag.clone()));
            }
        }
    });
    out
}

/// The row for typing the selection's tags, with the tags already used
/// in the app offered under it: a click, or `Tab` for the first, puts
/// one in. `Some(Some(text))` once `Enter` saves them, `Some(None)` when
/// `Esc` or a click elsewhere leaves them as they were.
fn tag_row(ui: &mut egui::Ui, edit: &mut TagEdit) -> Option<Option<String>> {
    const SUGGESTED: usize = 8;
    let id = egui::Id::new("osn_tag_field");
    ui.add_space(2.0);
    // Tab takes the first suggestion, before the field could take it to
    // move the keyboard on.
    let first = edit.suggestions(1).into_iter().next();
    if let Some(tag) = first.filter(|_| ui.memory(|m| m.has_focus(id))) {
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Tab)) {
            edit.accept(&tag);
            edit.focus = true;
        }
    }
    let mut picked = None;
    let row = ui.vertical(|ui| {
        let r = ui
            .horizontal(|ui| {
                ui.label(egui::RichText::new("Tags").small().color(theme::TEXT_MUTED));
                let field = egui::TextEdit::singleline(&mut edit.text)
                    .id(id)
                    .hint_text("pose ref walk-cycle — spaces or commas between, Enter saves")
                    .desired_width(300.0);
                ui.add(field)
            })
            .inner;
        if parse_row(&edit.text).iter().any(|(_, some)| *some) {
            let note = "tag* — stays only on the objects that have it; take the * off to put it on all";
            ui.label(egui::RichText::new(note).small().color(theme::TEXT_MUTED));
        }
        let typed = parse_row(&edit.text);
        let likely: Vec<(String, f32)> =
            edit.likely.iter().filter(|(t, _)| !typed.iter().any(|(h, _)| crate::canvas::units::same_tag(h, t))).cloned().collect();
        if !likely.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(egui::RichText::new("Looks like").small().color(theme::TEXT_MUTED));
                for (tag, close) in &likely {
                    let text = egui::RichText::new(format!("#{tag} {:.0}%", close * 100.0)).small().color(theme::TEXT_PRIMARY);
                    let tip = format!("It looks like the pictures tagged #{tag}. Click to add it");
                    if ui.add(egui::Label::new(text).sense(egui::Sense::click())).on_hover_text(tip).clicked() {
                        picked = Some(tag.clone());
                    }
                }
            });
        }
        let posted: Vec<&FromPost> = edit.from_post.iter().filter(|f| !typed.iter().any(|(h, _)| crate::canvas::units::same_tag(h, &f.tag))).collect();
        if !posted.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(egui::RichText::new("From post").small().color(theme::TEXT_MUTED));
                for fp in posted {
                    let site = if fp.site.is_empty() { String::new() } else { format!(" · {}", fp.site) };
                    let text = egui::RichText::new(format!("#{}{site}", fp.tag)).small().color(theme::TEXT_PRIMARY);
                    if ui.add(egui::Label::new(text).sense(egui::Sense::click())).on_hover_text(fp.tip()).clicked() {
                        picked = Some(fp.tag.clone());
                    }
                }
            });
        }
        let offered = edit.suggestions(SUGGESTED);
        if !offered.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(egui::RichText::new("Used").small().color(theme::TEXT_MUTED));
                for (k, tag) in offered.iter().enumerate() {
                    let text = egui::RichText::new(format!("#{tag}")).small().color(theme::TEXT_PRIMARY);
                    let tip = if k == 0 { "Click, or Tab, to add it" } else { "Click to add it" };
                    if ui.add(egui::Label::new(text).sense(egui::Sense::click())).on_hover_text(tip).clicked() {
                        picked = Some(tag.clone());
                    }
                }
            });
        }
        r
    });
    let r = row.inner;
    if let Some(tag) = picked {
        edit.accept(&tag);
        edit.focus = true;
        return None;
    }
    if std::mem::take(&mut edit.focus) {
        r.request_focus();
        // The caret goes to the end, after what is there.
        if let Some(mut st) = egui::text_edit::TextEditState::load(ui.ctx(), id) {
            let end = egui::text::CCursor::new(edit.text.chars().count());
            st.cursor.set_char_range(Some(egui::text::CCursorRange::one(end)));
            st.store(ui.ctx(), id);
        }
        return None;
    }
    if r.lost_focus() {
        if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            return Some(Some(edit.text.clone()));
        }
        // A press on the row itself (a suggestion) keeps it open: the
        // click lands when the button comes up.
        let on_row = ui.input(|i| i.pointer.interact_pos()).is_some_and(|p| row.response.rect.contains(p));
        if on_row && !ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            edit.focus = true;
            return None;
        }
        return Some(None);
    }
    None
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
        theme::on_fill()
    } else if others && response.hovered() {
        theme::HOVER
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, Rounding::same(theme::CONTROL_ROUNDING), bg);
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
        theme::on_fill()
    } else if response.hovered() {
        theme::HOVER
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
/// objects; `tagged` when some of them carry a tag, for group by tags.
fn arrange_buttons(ui: &mut egui::Ui, count: usize, tagged: bool, apart: &mut bool, pack: &mut PackMode, keys: &ParsedHotkeys, picked: &mut Option<BarAction>) {
    let k = &keys.arrange;
    let mut btn = |ui: &mut egui::Ui, op: Arrange, img: egui::ImageSource<'static>, tip: &str, key: Option<Binding>| {
        let can = count >= op.min_items() && (op != Arrange::GroupByTags || tagged);
        if icon_btn(ui, img, can, &with_key(tip, key)) {
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
    let tip = if tagged {
        "Group by tags — the objects under each tag packed into their own cluster, A to Z, untagged ones last. One with several tags goes with the tag most of the selection shares"
    } else {
        "Group by tags — none of the selection is tagged yet (the tag button tags it)"
    };
    btn(ui, Arrange::GroupByTags, egui::include_image!("../../assets/icons/group-by-tags.svg"), tip, k.group_by_tags);
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
        theme::on_fill()
    } else if resp.hovered() {
        theme::HOVER
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, Rounding::same(theme::CONTROL_ROUNDING), bg);
    let icon = rect.shrink(rect.width() * 0.24);
    egui::Image::new(img).tint(theme::TEXT_PRIMARY).fit_to_exact_size(icon.size()).paint_at(ui, icon);
    resp.on_hover_text(tooltip).clicked()
}

fn icon_btn(ui: &mut egui::Ui, img: egui::ImageSource<'static>, enabled: bool, tooltip: &str) -> bool {
    let sense = if enabled { egui::Sense::click() } else { egui::Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(BTN), sense);
    let bg = if enabled && resp.hovered() { theme::HOVER } else { Color32::TRANSPARENT };
    ui.painter().rect_filled(rect, Rounding::same(theme::CONTROL_ROUNDING), bg);
    let tint = if enabled { theme::TEXT_PRIMARY } else { Color32::from_white_alpha(70) };
    let icon = rect.shrink(rect.width() * 0.24);
    egui::Image::new(img).tint(tint).fit_to_exact_size(icon.size()).paint_at(ui, icon);
    resp.on_hover_text(tooltip).clicked()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_many_tag_chips_as_fit_the_first_always() {
        let tags = |t: &[&str]| t.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // "#pose #ref" is 10 characters.
        assert_eq!(chips_that_fit(&tags(&["pose", "ref"]), 10), 2);
        assert_eq!(chips_that_fit(&tags(&["pose", "ref"]), 9), 1);
        assert_eq!(chips_that_fit(&tags(&["a-very-long-tag-indeed"]), 5), 1, "one alone is cut, not left out");
        assert_eq!(chips_that_fit(&tags(&["a", "bb", "c"]), 7), 2, "#a #bb");
        assert_eq!(chips_that_fit(&[], 10), 0);
    }

    fn edit(text: &str) -> TagEdit {
        let known = [("pose", 9), ("ref", 4), ("portrait", 6), ("prop", 1), ("hands", 2)];
        TagEdit { text: text.into(), was: Vec::new(), focus: false, known: known.iter().map(|(t, n)| (t.to_string(), *n)).collect(), likely: Vec::new(), from_post: Vec::new() }
    }

    #[test]
    fn suggestions_fit_the_word_being_typed_and_skip_typed_tags() {
        // Nothing typed: the most used.
        assert_eq!(edit("").suggestions(3), vec!["pose", "portrait", "ref"]);
        // Starting with it first, then holding it; most used within each.
        assert_eq!(edit("po").suggestions(8), vec!["pose", "portrait"]);
        assert_eq!(edit("r").suggestions(8), vec!["ref", "portrait", "prop"]);
        // Tags typed already are not offered again, case aside.
        assert_eq!(edit("POSE ref ").suggestions(2), vec!["portrait", "hands"]);
        assert_eq!(edit("ref,#po").suggestions(8), vec!["pose", "portrait"]);
    }

    #[test]
    fn accepting_replaces_the_word_being_typed() {
        let mut e = edit("ref po");
        e.accept("pose");
        assert_eq!(e.text, "ref pose ");
        let mut e = edit("");
        e.accept("hands");
        assert_eq!(e.text, "hands ");
        assert_eq!(TagEdit::new(&[("a".into(), false), ("b".into(), false)]).text, "a b ");
    }

    #[test]
    fn tags_only_some_of_the_selection_carry_are_marked() {
        let e = TagEdit::new(&[("cat".into(), true), ("ref".into(), false)]);
        assert_eq!(e.text, "cat* ref ");
        assert_eq!(e.was, vec!["cat".to_string(), "ref".to_string()]);
        let row = |v: &[(&str, bool)]| v.iter().map(|(t, s)| (t.to_string(), *s)).collect::<Vec<_>>();
        assert_eq!(parse_row("cat* ref dog"), row(&[("cat", true), ("ref", false), ("dog", false)]));
        // Typed both ways it goes on all; a lone * is no tag.
        assert_eq!(parse_row("cat* CAT * #x**"), row(&[("cat", false), ("x", true)]));
        // A marked tag is not offered again.
        assert_eq!(edit("pose* ").suggestions(1), vec!["portrait"]);
    }
}
