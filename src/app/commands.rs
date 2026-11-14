//! Running the buttons' keys (`interaction::action_keys`) and the command palette's picks.

use super::*;

impl OnScreenNotesApp {
    /// Press the button a key is bound to (`interaction::action_keys`),
    /// as a click on it would. The arrange bar's and the pin bar's keys
    /// do something only while their bar would show.
    pub(super) fn apply_key_action(&mut self, action: interaction::action_keys::KeyAction, ctx: &egui::Context) {
        use crate::tools::arrange::Arrange;
        use interaction::action_keys::KeyAction as K;
        let say = |app: &mut Self, msg: String| app.toast = Some((msg, Instant::now()));
        let selecting = self.tool.kind == crate::tools::ToolKind::Select
            && !self.tool.select.sel.is_empty()
            && self.tool.select.gesture.is_none();
        match action {
            K::Arrange(op) => {
                if selecting && self.may_edit() {
                    self.arrange_selection(op, ctx);
                }
            }
            K::Align(a) => {
                if selecting && self.may_edit() {
                    self.arrange_selection(if self.config.arrange_apart { Arrange::Slide(a) } else { Arrange::Align(a) }, ctx);
                }
            }
            K::Match(m) => {
                if selecting && self.may_edit() {
                    self.match_selection_size(m);
                }
            }
            K::KeepApart => {
                self.config.arrange_apart = !self.config.arrange_apart;
                let msg = if self.config.arrange_apart {
                    "Keep apart on: aligning slides objects until they touch"
                } else {
                    "Keep apart off: aligning lines objects up, even on top of each other"
                };
                say(self, msg.to_string());
            }
            K::MoveToSlide => {
                if selecting {
                    self.move_picker_pending = true;
                }
            }
            K::Colour(i) => {
                if let Some(&c) = self.config.palette.get(i) {
                    let (color, size) = (self.tool.color, self.tool.size);
                    self.tool.color = c;
                    self.restyle_selection(color, size);
                }
            }
            K::Size(n) => {
                let (color, size) = (self.tool.color, self.tool.size);
                interaction::zoom_pan::nudge_size(&mut self.tool, f32::from(n));
                self.restyle_selection(color, size);
            }
            K::InsertMedia => self.open_media_dialog(),
            K::Highlight(t) => {
                let on = self.media_filter.flag(t);
                *on = !*on;
            }
            K::HighlightGroups => self.media_filter.groups = !self.media_filter.groups,
            K::HighlightTagged => self.media_filter.tagged = !self.media_filter.tagged,
            K::SelectHighlighted => {
                if self.may_edit() {
                    self.select_media_matches();
                }
            }
            K::PinToWindow => {
                if self.pinned_hwnd.is_some() {
                    self.pinned_hwnd = None;
                } else if self.pin_picking {
                    self.pin_picking = false;
                    ctx.send_viewport_cmd(egui::ViewportCommand::MousePassthrough(false));
                } else {
                    self.pin_picking = true;
                }
            }
            K::Background(n) => {
                self.config.bg_opacity = (self.config.bg_opacity + 0.1 * f32::from(n)).clamp(0.0, 1.0);
            }
            K::ResetBackground => self.config.bg_color = [0, 0, 0],
            K::NewSlide => {
                self.canvases.push_canvas(crate::canvas::canvas::Canvas::default());
                say(self, format!("Slide {} added at the end", self.canvases.count()));
            }
            K::ToggleSlidePanel => {
                self.config.slide_panel_open = !self.config.slide_panel_open;
                if self.config.slide_panel_open {
                    self.ui_visible = true;
                }
            }
            K::ToggleSlideOrder => {
                use crate::config::SlideTabOrder;
                let recent = self.config.slide_tab_order == SlideTabOrder::RecentlyUsed;
                self.config.slide_tab_order = if recent { SlideTabOrder::Sequential } else { SlideTabOrder::RecentlyUsed };
                let msg = if recent { "Ctrl+Tab goes through slides in order" } else { "Ctrl+Tab goes to recently used slides" };
                say(self, msg.to_string());
            }
            K::SlideWindow => {
                if self.slide_window.is_some() {
                    self.slide_window = None;
                } else {
                    self.slide_window = Some(ui::slide_window::SlideWindow::new(self.canvases.active_index(), &self.config));
                }
            }
            K::NewLayer => {
                if self.may_edit() {
                    self.canvases.active_mut().add_layer();
                }
            }
            K::DeleteLayer => {
                if self.may_edit() {
                    let c = self.canvases.active_mut();
                    let (li, n) = (c.active_layer, c.layers.len());
                    c.delete_layer(li);
                    // Not undoable, as the layer panel's button is not:
                    // say so, since an empty layer shows no change.
                    let msg = if n > 1 { format!("Layer {} deleted", li + 1) } else { "The last layer stays".to_string() };
                    say(self, msg);
                }
            }
            K::MoveLayer(n) => {
                if self.may_edit() {
                    let c = self.canvases.active_mut();
                    let li = c.active_layer;
                    if let Some(to) = li.checked_add_signed(isize::from(n)).filter(|&to| to < c.layers.len()) {
                        c.move_layer(li, to);
                    }
                }
            }
            K::PinGoToSlide => {
                if let Some(id) = self.pin_tool.selected {
                    let area = self.overlay_cache.canvas_rect(ctx).size();
                    self.open_pin_slide(id, area);
                }
            }
            K::PinThisSlideOnly => {
                if let Some(id) = self.pin_tool.selected {
                    if let Some(local) = self.canvases.pinned().iter().find(|p| p.pin.id == id).map(|p| p.pin.local) {
                        self.set_pin_local(id, !local);
                    }
                }
            }
            K::Nudge(px) => {
                if selecting && self.may_edit() {
                    self.nudge_selection(px, ctx.pixels_per_point());
                }
            }
            K::ZoomToFit => self.zoom_to_fit(ctx),
            K::ZoomToSelection => self.zoom_to_selection(ctx),
            K::Flip(vertical) => {
                if selecting {
                    self.flip_selection(vertical);
                }
            }
            K::Rotate(right) => {
                if selecting {
                    self.rotate_selection(right);
                }
            }
            K::Tag => {
                if selecting {
                    self.ui_visible = true;
                    self.tag_edit = Some(ui::arrange_bar::TagEdit::new(&self.selection_tags()));
                }
            }
            K::PictureColours => {
                if selecting {
                    self.toggle_picture_colours();
                }
            }
            K::TextBox => {
                self.tool.text_box = !self.tool.text_box;
                self.apply_text_box_to_selection();
                let msg = if self.tool.text_box { "Text in a box: on" } else { "Text in a box: off" };
                say(self, msg.to_string());
            }
            K::Onion => self.toggle_onion(),
            K::CommandPalette => self.commands.open(),
            K::Find => {
                self.commands.close();
                self.find.open();
            }
            K::Group(on) => {
                if selecting {
                    self.group_selection(on);
                }
            }
            K::MoveSlide(step) => {
                let from = self.canvases.active_index();
                if let Some(to) = from.checked_add_signed(isize::from(step)).filter(|&to| to < self.canvases.count()) {
                    self.move_slide(from, to);
                }
            }
        }
    }

    /// Run what was picked in the command palette, as its key or button
    /// would.
    pub(super) fn run_command(&mut self, cmd: ui::command_palette::Command, ctx: &egui::Context, canvas_rect: egui::Rect) {
        use ui::command_palette::Command as C;
        let ppp = ctx.pixels_per_point();
        let sel = |app: &mut Self, action: SelAction| {
            // Copying changes nothing; the rest want a slide that takes edits.
            if matches!(action, SelAction::Copy | SelAction::CopyToNewSlide) || app.may_edit() {
                app.apply_sel_action(action, canvas_rect, ppp);
            }
        };
        match cmd {
            C::Key(k) => self.apply_key_action(k, ctx),
            C::Tool(kind, style) => {
                self.tool.kind = kind;
                if let Some(s) = style {
                    self.tool.style = s;
                }
            }
            C::Eyedropper => self.tool.eyedropper_pending = true,
            C::Undo | C::Redo => {
                if self.may_edit() {
                    let c = self.canvases.active_mut();
                    if cmd == C::Undo { c.undo() } else { c.redo() }
                    self.restyle_run = None;
                    self.nudge_run = None;
                }
            }
            C::ClearLayer => {
                if self.may_edit() {
                    self.canvases.active_mut().clear();
                }
            }
            C::DeleteSlide => {
                if self.may_edit() {
                    self.request_slide_delete();
                }
            }
            C::LockSlide => self.toggle_lock(),
            C::RenameSlide => {
                let i = self.canvases.active_index();
                self.slide_panel.start_rename(i, self.canvases.name(i));
                self.config.slide_panel_open = true;
                self.ui_visible = true;
            }
            C::ResetView => {
                let (pan, zoom) = self.canvases.active_view_mut();
                *zoom = 1.0;
                *pan = [0.0, 0.0];
            }
            C::FreezeFrame => {
                if self.may_edit() {
                    self.queue_freeze(ppp);
                }
            }
            C::ToggleUi => self.ui_visible = !self.ui_visible,
            C::TogglePins => self.set_pins_hidden(!self.canvases.pins_hidden()),
            C::Duplicate => sel(self, SelAction::Duplicate),
            C::CopyToNewSlide => sel(self, SelAction::CopyToNewSlide),
            C::TogglePin => sel(self, SelAction::TogglePin),
            C::ToFront => sel(self, SelAction::ToFront),
            C::ToBack => sel(self, SelAction::ToBack),
            C::DeleteSelection => sel(self, SelAction::Delete),
            C::ShapeSnap => {
                self.config.shape_snap = !self.config.shape_snap;
                self.tool.shape_snap = self.config.shape_snap;
                let msg = if self.config.shape_snap { "Shape snap on" } else { "Shape snap off" };
                self.toast = Some((msg.to_string(), Instant::now()));
            }
            C::GoTo(i) => self.canvases.go_to(i),
        }
    }
}
