//! Playback bar for videos and GIFs, the file dialogs that bring them
//! in, and the window that sets up FFmpeg.
//!
//! The bar sits just *below* the object (above it when there is no room)
//! rather than over it, so drawing on a video is never blocked by its
//! controls. It shows while the pointer is over the object or the bar,
//! for a moment after it leaves, and whenever the object is selected.

use crate::canvas::canvas::Canvas;
use crate::media::ffmpeg::{self, Installer};
use crate::media::{Cmd, MediaHub, PlayerStatus};
use crate::ui::theme;
use egui::{Color32, Pos2, Rect, Rounding, Vec2};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

/// How long the bar stays after the pointer leaves the object.
const LINGER: Duration = Duration::from_millis(1200);
const BTN: f32 = 26.0;
const BAR_H: f32 = 34.0;

/// A media object on the active slide, in screen space.
#[derive(Debug, Clone, Copy)]
pub struct OnScreen {
    pub id: u64,
    /// Axis-aligned screen box around the (possibly rotated) object.
    pub rect: Rect,
}

/// Media objects on `canvas`'s visible layers, bottom to top, placed on
/// screen the way the renderer places them.
pub fn media_on_screen(canvas: &Canvas, canvas_rect: Rect) -> Vec<OnScreen> {
    let to_screen = |p: [f32; 2]| {
        canvas_rect.min + Vec2::new(canvas.pan[0] + p[0] * canvas.zoom, canvas.pan[1] + p[1] * canvas.zoom)
    };
    let mut out = Vec::new();
    for layer in canvas.layers.iter().filter(|l| l.visible && l.opacity > 0.0) {
        for shape in &layer.shapes {
            let Some(m) = shape.media() else { continue };
            let bb = shape.obb().aabb();
            out.push(OnScreen { id: m.id, rect: Rect::from_two_pos(to_screen([bb[0], bb[1]]), to_screen([bb[2], bb[3]])) });
        }
    }
    out
}

/// The media object selected on its own in the Select tool, if any.
pub fn selected_media(canvas: &Canvas, sel: &crate::canvas::hit::Selection) -> Option<u64> {
    if sel.shapes.len() != 1 || !sel.strokes.is_empty() { return None; }
    let layer = canvas.layers.get(canvas.active_layer)?;
    layer.shapes.get(sel.shapes[0])?.media().map(|m| m.id)
}

/// Which object a play / step hotkey acts on: the selected one, else
/// the topmost under the pointer, else the only one on the slide.
pub fn hotkey_target(objs: &[OnScreen], selected: Option<u64>, pointer: Option<Pos2>) -> Option<u64> {
    selected
        .or_else(|| pointer.and_then(|p| objs.iter().rev().find(|o| o.rect.contains(p)).map(|o| o.id)))
        .or_else(|| if objs.len() == 1 { Some(objs[0].id) } else { None })
}

/// Control-bar state kept across frames.
#[derive(Default)]
pub struct MediaUi {
    /// When the pointer was last over each object or its bar.
    hovered_at: HashMap<u64, Instant>,
    /// Where each bar was drawn last frame.
    bar_rects: HashMap<u64, Rect>,
    /// The FFmpeg setup window is open.
    pub ffmpeg_window: bool,
    installer: Option<Installer>,
    locate: Option<Receiver<Vec<PathBuf>>>,
    /// Last outcome to show in the FFmpeg window.
    ffmpeg_msg: Option<String>,
}

/// What the app should do after the controls ran.
#[derive(Default)]
pub struct MediaUiOut {
    pub commands: Vec<(u64, Cmd)>,
    /// A bar was clicked: the overlay should take keyboard focus back so
    /// the play / step hotkeys keep working.
    pub clicked: bool,
    /// FFmpeg became usable (installed, or located).
    pub ffmpeg_ready: bool,
    /// Picked with "Locate ffmpeg…"; to be saved in config.
    pub located: Option<PathBuf>,
    pub toast: Option<String>,
}

impl MediaUi {
    pub fn new() -> Self {
        Self::default()
    }

    /// Draw the bars. `show_bars` is false while drawing, capturing the
    /// screen, recording or with the UI hidden.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        hub: &MediaHub,
        objs: &[OnScreen],
        selected: Option<u64>,
        canvas_rect: Rect,
        show_bars: bool,
    ) -> MediaUiOut {
        let mut out = MediaUiOut::default();
        let now = Instant::now();
        let pointer = ctx.input(|i| i.pointer.hover_pos());
        if let Some(p) = pointer {
            // Only the topmost object under the pointer counts.
            let over = objs.iter().rev().find(|o| o.rect.contains(p)).map(|o| o.id);
            for id in over.into_iter().chain(self.bar_rects.iter().filter(|(_, r)| r.contains(p)).map(|(id, _)| *id)) {
                self.hovered_at.insert(id, now);
            }
        }
        self.hovered_at.retain(|_, t| now.duration_since(*t) < LINGER * 4);

        let mut drawn = HashMap::new();
        if show_bars {
            for o in objs {
                let lingering = self.hovered_at.get(&o.id).is_some_and(|t| now.duration_since(*t) < LINGER);
                if !(lingering || selected == Some(o.id)) { continue; }
                let Some(status) = hub.status(o.id) else { continue };
                let rect = self.bar(ctx, o, &status, canvas_rect, &mut out);
                drawn.insert(o.id, rect);
            }
        }
        self.bar_rects = drawn;
        out
    }

    fn bar(&mut self, ctx: &egui::Context, o: &OnScreen, st: &PlayerStatus, canvas_rect: Rect, out: &mut MediaUiOut) -> Rect {
        let width = o.rect.width().clamp(300.0, 560.0);
        let gap = 6.0;
        let x = (o.rect.center().x - width * 0.5).clamp(canvas_rect.left() + 4.0, (canvas_rect.right() - width - 4.0).max(canvas_rect.left()));
        let below = o.rect.bottom() + gap;
        let y = if below + BAR_H <= canvas_rect.bottom() - 4.0 {
            below
        } else if o.rect.top() - gap - BAR_H >= canvas_rect.top() + 4.0 {
            o.rect.top() - gap - BAR_H
        } else {
            // No room either side: inside, along the bottom edge.
            (o.rect.bottom() - BAR_H - gap).min(canvas_rect.bottom() - BAR_H - 4.0)
        };
        let id = o.id;
        let area = egui::Area::new(egui::Id::new(("osn_media_bar", id)))
            .fixed_pos(Pos2::new(x, y))
            .order(egui::Order::Middle)
            .show(ctx, |ui| {
                theme::card_frame().inner_margin(egui::Margin::symmetric(6.0, 4.0)).show(ui, |ui| {
                    ui.set_width(width - 12.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        if st.needs_ffmpeg {
                            ui.label(egui::RichText::new("Video needs FFmpeg").color(theme::TEXT_MUTED));
                            if ui.button("Set up…").clicked() {
                                self.ffmpeg_window = true;
                                out.clicked = true;
                            }
                            return;
                        }
                        if let Some(err) = &st.error {
                            ui.label(egui::RichText::new(err).color(theme::TEXT_MUTED).small());
                            return;
                        }
                        let cmd = |c: Cmd, out: &mut MediaUiOut| {
                            out.commands.push((id, c));
                            out.clicked = true;
                        };
                        if icon_btn(ui, Icon::StepBack, false, "Previous frame — ,") { cmd(Cmd::Step(-1), out); }
                        let (icon, tip) = if st.playing { (Icon::Pause, "Pause — K") } else { (Icon::Play, "Play — K") };
                        if icon_btn(ui, icon, false, tip) { cmd(Cmd::TogglePlay, out); }
                        if icon_btn(ui, Icon::StepForward, false, "Next frame — .") { cmd(Cmd::Step(1), out); }

                        let label = time_label(st);
                        let galley = ui.painter().layout_no_wrap(label, egui::FontId::proportional(11.0), theme::TEXT_MUTED);
                        let reserved = galley.size().x + BTN + 14.0 + if st.loading { 16.0 } else { 0.0 };
                        let scrub_w = (ui.available_width() - reserved).max(40.0);
                        if let Some(c) = scrubber(ui, scrub_w, st) { cmd(c, out); }
                        if st.loading {
                            ui.add(egui::Spinner::new().size(12.0));
                        }
                        let (r, _) = ui.allocate_exact_size(Vec2::new(galley.size().x + 6.0, BTN), egui::Sense::hover());
                        ui.painter().galley(Pos2::new(r.left() + 3.0, r.center().y - galley.size().y * 0.5), galley, theme::TEXT_MUTED);
                        if icon_btn(ui, Icon::Repeat, st.looping, if st.looping { "Loop: on" } else { "Loop: off" }) {
                            cmd(Cmd::ToggleLoop, out);
                        }
                    });
                });
            });
        area.response.rect
    }

    /// The "Video needs FFmpeg" window, while open. Also polls a running
    /// install or Locate dialog.
    pub fn ffmpeg_window(&mut self, ctx: &egui::Context, owner: isize, out: &mut MediaUiOut) {
        if let Some(inst) = &self.installer {
            if let Some(r) = inst.take_result() {
                self.installer = None;
                match r {
                    Ok(true) => {
                        out.ffmpeg_ready = true;
                        out.toast = Some("FFmpeg installed — videos can play now".into());
                        self.ffmpeg_window = false;
                        self.ffmpeg_msg = None;
                    }
                    Ok(false) => self.ffmpeg_msg = Some("winget finished, but FFmpeg still can't be found. Try \"Locate ffmpeg…\".".into()),
                    Err(e) => self.ffmpeg_msg = Some(format!("{e}. Install FFmpeg yourself, then use \"Locate ffmpeg…\".")),
                }
            }
        }
        if let Some(rx) = &self.locate {
            if let Ok(paths) = rx.try_recv() {
                self.locate = None;
                if let Some(p) = paths.into_iter().next() {
                    ffmpeg::set_configured(Some(p.clone()));
                    if ffmpeg::tools().is_some() {
                        out.located = Some(p);
                        out.ffmpeg_ready = true;
                        out.toast = Some("FFmpeg found — videos can play now".into());
                        self.ffmpeg_window = false;
                        self.ffmpeg_msg = None;
                    } else {
                        ffmpeg::set_configured(None);
                        self.ffmpeg_msg = Some("That file didn't run as ffmpeg (ffprobe must sit next to it).".into());
                    }
                }
            }
        }
        if !self.ffmpeg_window { return; }

        let mut open = true;
        egui::Window::new("Video needs FFmpeg")
            .id(egui::Id::new("osn_ffmpeg_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_max_width(360.0);
                ui.label("Videos are played with FFmpeg, a free tool that isn't installed yet. GIFs work without it.");
                ui.add_space(6.0);
                if self.installer.is_some() {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new());
                        ui.label("Installing… follow the console window.");
                    });
                } else {
                    ui.horizontal(|ui| {
                        if ffmpeg::can_install() && ui.button("Install FFmpeg").on_hover_text("Runs: winget install Gyan.FFmpeg.Essentials").clicked() {
                            self.ffmpeg_msg = None;
                            self.installer = Some(Installer::start());
                        }
                        if self.locate.is_none() && ui.button("Locate ffmpeg…").clicked() {
                            self.locate = Some(pick_files(owner, "Locate ffmpeg", &[("FFmpeg", &[if cfg!(windows) { "exe" } else { "*" }])], false));
                        }
                        if ui.button("Check again").clicked() {
                            if ffmpeg::redetect().is_some() {
                                out.ffmpeg_ready = true;
                                self.ffmpeg_window = false;
                            } else {
                                self.ffmpeg_msg = Some("Still not found.".into());
                            }
                        }
                    });
                }
                ui.add_space(4.0);
                let hint = if cfg!(target_os = "windows") {
                    "Or put ffmpeg.exe and ffprobe.exe next to on-screen-notes.exe."
                } else if cfg!(target_os = "macos") {
                    "Install it with: brew install ffmpeg"
                } else {
                    "Install it with your package manager, e.g. sudo apt install ffmpeg"
                };
                ui.label(egui::RichText::new(hint).small().color(theme::TEXT_MUTED));
                if let Some(m) = &self.ffmpeg_msg {
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(m).small());
                }
            });
        if !open {
            self.ffmpeg_window = false;
        }
    }
}

/// `m:ss.s / m:ss.s · frame/frames`, trimmed to what is known.
fn time_label(st: &PlayerStatus) -> String {
    let t = |s: f64| {
        let s = s.max(0.0);
        format!("{}:{:04.1}", (s / 60.0).floor() as u64, s % 60.0)
    };
    let time = match st.duration {
        Some(d) => format!("{} / {}", t(st.pos), t(d)),
        None => t(st.pos),
    };
    match st.frames {
        Some(n) => format!("{time} · {}/{n}", st.frame + 1),
        None => format!("{time} · {}", st.frame + 1),
    }
}

/// Seek bar. Dragging seeks continuously (previews); letting go or a
/// click seeks for real.
fn scrubber(ui: &mut egui::Ui, width: f32, st: &PlayerStatus) -> Option<Cmd> {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, BTN), egui::Sense::click_and_drag());
    let track = Rect::from_center_size(rect.center(), Vec2::new(rect.width() - 12.0, 4.0));
    let duration = st.duration.filter(|d| *d > 0.0);
    let frac = duration.map_or(0.0, |d| (st.pos / d).clamp(0.0, 1.0)) as f32;
    let painter = ui.painter();
    painter.rect_filled(track, Rounding::same(2.0), Color32::from_white_alpha(45));
    let fill = Rect::from_min_max(track.min, Pos2::new(track.left() + track.width() * frac, track.bottom()));
    painter.rect_filled(fill, Rounding::same(2.0), theme::TEXT_PRIMARY);
    let knob_r = if resp.hovered() || resp.dragged() { 6.0 } else { 4.5 };
    painter.circle_filled(Pos2::new(fill.right(), track.center().y), knob_r, theme::TEXT_PRIMARY);

    let d = duration?;
    let at = |p: Pos2| (((p.x - track.left()) / track.width().max(1.0)).clamp(0.0, 1.0) as f64) * d;
    let p = resp.interact_pointer_pos();
    if resp.drag_stopped() || resp.clicked() {
        return p.map(|p| Cmd::Seek { secs: at(p), preview: false });
    }
    if resp.dragged() {
        return p.map(|p| Cmd::Seek { secs: at(p), preview: true });
    }
    None
}

#[derive(Clone, Copy)]
enum Icon {
    Play,
    Pause,
    StepBack,
    StepForward,
    Repeat,
}

fn icon_btn(ui: &mut egui::Ui, icon: Icon, active: bool, tooltip: &str) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(BTN), egui::Sense::click());
    let bg = if active {
        theme::ACCENT
    } else if resp.hovered() {
        Color32::from_white_alpha(22)
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, Rounding::same(7.0), bg);
    let img = match icon {
        Icon::Play => egui::Image::new(egui::include_image!("../../assets/icons/play.svg")),
        Icon::Pause => egui::Image::new(egui::include_image!("../../assets/icons/pause.svg")),
        Icon::StepBack => egui::Image::new(egui::include_image!("../../assets/icons/step-back.svg")),
        Icon::StepForward => egui::Image::new(egui::include_image!("../../assets/icons/step-forward.svg")),
        Icon::Repeat => egui::Image::new(egui::include_image!("../../assets/icons/repeat.svg")),
    };
    let icon_rect = rect.shrink(rect.width() * 0.24);
    let tint = if active || resp.hovered() { theme::TEXT_PRIMARY } else { Color32::from_gray(205) };
    img.tint(tint).fit_to_exact_size(icon_rect.size()).paint_at(ui, icon_rect);
    resp.on_hover_text(tooltip).clicked()
}

/// The overlay window as a dialog owner, so the Open dialog stays above
/// it. Only a window handle is needed; it is rebuilt on the dialog's
/// thread because raw handles are not `Send`.
struct Owner(isize);

impl raw_window_handle::HasWindowHandle for Owner {
    fn window_handle(&self) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError> {
        #[cfg(target_os = "windows")]
        {
            let hwnd = std::num::NonZeroIsize::new(self.0).ok_or(raw_window_handle::HandleError::Unavailable)?;
            let raw = raw_window_handle::RawWindowHandle::Win32(raw_window_handle::Win32WindowHandle::new(hwnd));
            // SAFETY: the overlay window outlives the modal dialog it owns.
            Ok(unsafe { raw_window_handle::WindowHandle::borrow_raw(raw) })
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = self.0;
            Err(raw_window_handle::HandleError::Unavailable)
        }
    }
}

/// Show an Open dialog without freezing the overlay: it runs on its own
/// thread (on macOS, where dialogs must run on the main thread, it
/// blocks instead) and the chosen paths arrive on the returned channel —
/// empty when cancelled.
pub fn pick_files(owner: isize, title: &str, filters: &[(&str, &[&str])], multiple: bool) -> Receiver<Vec<PathBuf>> {
    let (tx, rx) = mpsc::channel();
    let title = title.to_string();
    let filters: Vec<(String, Vec<String>)> =
        filters.iter().map(|(n, e)| (n.to_string(), e.iter().map(|s| s.to_string()).collect())).collect();
    let run = move || {
        let mut d = rfd::FileDialog::new().set_title(&title);
        for (name, exts) in &filters {
            d = d.add_filter(name, exts);
        }
        if owner != 0 {
            d = d.set_parent(&Owner(owner));
        }
        let picked = if multiple { d.pick_files().unwrap_or_default() } else { d.pick_file().into_iter().collect() };
        let _ = tx.send(picked);
    };
    if cfg!(target_os = "macos") {
        run();
    } else if let Err(e) = std::thread::Builder::new().name("osn-open-dialog".into()).spawn(run) {
        log::warn!("could not open the file dialog: {e}");
    }
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(id: u64, x0: f32, x1: f32) -> OnScreen {
        OnScreen { id, rect: Rect::from_min_max(Pos2::new(x0, 0.0), Pos2::new(x1, 100.0)) }
    }

    #[test]
    fn hotkeys_prefer_selection_then_pointer_then_the_only_one() {
        let objs = [obj(1, 0.0, 100.0), obj(2, 50.0, 150.0)];
        assert_eq!(hotkey_target(&objs, Some(1), Some(Pos2::new(120.0, 10.0))), Some(1));
        // Overlap: the topmost (last) wins.
        assert_eq!(hotkey_target(&objs, None, Some(Pos2::new(75.0, 10.0))), Some(2));
        assert_eq!(hotkey_target(&objs, None, Some(Pos2::new(500.0, 10.0))), None);
        assert_eq!(hotkey_target(&objs[..1], None, None), Some(1));
    }

    #[test]
    fn time_label_shows_what_is_known() {
        let st = PlayerStatus { pos: 72.25, duration: Some(130.0), frame: 9, frames: Some(48), ..Default::default() };
        assert_eq!(time_label(&st), "1:12.2 / 2:10.0 · 10/48");
        let st = PlayerStatus { pos: 3.0, ..Default::default() };
        assert_eq!(time_label(&st), "0:03.0 · 1");
    }
}
