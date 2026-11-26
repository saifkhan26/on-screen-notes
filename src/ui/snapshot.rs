//! Test-only: draw egui UI off screen, on this machine's GPU, to a PNG.
//!
//! The overlay is a full-screen window over the user's desktop, so a
//! test build run to look at the UI sits over their work and catches
//! their pen. These tests draw the same widgets into a texture instead,
//! with clicks and hovers fed in as egui input, so a popup or a hover
//! state can be looked at too. They are `#[ignore]`d and write only
//! when `OSN_SNAP_DIR` names a folder:
//!
//! ```text
//! $env:OSN_SNAP_DIR = "<scratch folder>"; cargo test snapshot -- --ignored
//! ```

use eframe::{egui_wgpu, wgpu};

/// One frame's input: where the pointer is, and whether it presses or
/// lets go there.
#[derive(Clone, Copy)]
pub enum Step {
    /// The pointer rests at this point (hover).
    Move(egui::Pos2),
    /// A click at this point: press this frame, release the next. No
    /// snapshot clicks today (popups are opened through egui's memory).
    #[allow(dead_code)]
    Click(egui::Pos2),
    /// Nothing happens; a frame for layout to settle.
    Idle,
}

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
}

fn gpu() -> Option<Gpu> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor { backends: wgpu::Backends::PRIMARY, ..Default::default() });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default(), None)).ok()?;
    Some(Gpu { device, queue })
}

/// The folder snapshots go to, or `None` (the test does nothing).
pub fn out_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("OSN_SNAP_DIR").filter(|d| !d.is_empty()).map(std::path::PathBuf::from)
}

/// A context styled as the app's.
pub fn context() -> egui::Context {
    let ctx = egui::Context::default();
    crate::ui::theme::apply(&ctx);
    egui_extras::install_image_loaders(&ctx);
    ctx
}

/// A stand-in desktop behind the overlay: a light page on the left, a
/// dark one on the right, so the bars can be judged over both.
pub fn backdrop(ctx: &egui::Context) {
    let painter = ctx.layer_painter(egui::LayerId::background());
    let r = ctx.screen_rect();
    let mid = r.center().x;
    painter.rect_filled(egui::Rect::from_min_max(r.min, egui::pos2(mid, r.max.y)), 0.0, egui::Color32::from_rgb(236, 236, 232));
    painter.rect_filled(egui::Rect::from_min_max(egui::pos2(mid, r.min.y), r.max), 0.0, egui::Color32::from_rgb(38, 40, 46));
    for i in 0..12 {
        let y = r.min.y + 120.0 + i as f32 * 26.0;
        painter.rect_filled(egui::Rect::from_min_size(egui::pos2(r.min.x + 60.0, y), egui::vec2(mid - 140.0 - (i % 3) as f32 * 60.0, 9.0)), 3.0, egui::Color32::from_gray(190));
        painter.rect_filled(egui::Rect::from_min_size(egui::pos2(mid + 60.0, y), egui::vec2(mid - 160.0 - (i % 4) as f32 * 40.0, 9.0)), 3.0, egui::Color32::from_gray(78));
    }
}

/// Run `ui` for `steps` (one frame each, then two to settle) on a
/// `size`-point screen at `ppp` pixels a point, and write the last frame
/// to `name`.png in `out_dir()`, in a fresh `context()`. Returns that
/// context, to read where things were drawn, or `None` without a GPU or
/// `OSN_SNAP_DIR`.
pub fn shoot(name: &str, size: [f32; 2], ppp: f32, steps: &[Step], mut ui: impl FnMut(&egui::Context)) -> Option<egui::Context> {
    let dir = out_dir()?;
    let Some(g) = gpu() else {
        eprintln!("no GPU here; skipped");
        return None;
    };
    let ctx = context();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut renderer = egui_wgpu::Renderer::new(&g.device, format, None, 1, false);
    let px = [(size[0] * ppp).round() as u32, (size[1] * ppp).round() as u32];

    let mut pointer = egui::Pos2::new(-100.0, -100.0);
    let mut release = false;
    let mut last = None;
    // Two frames first: an area's first frame only measures it, and takes no clicks.
    let frames = [Step::Idle, Step::Idle].into_iter().chain(steps.iter().copied()).chain([Step::Idle, Step::Idle, Step::Idle]);
    for (n, step) in frames.enumerate() {
        let mut events = Vec::new();
        if release {
            events.push(egui::Event::PointerButton { pos: pointer, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
            release = false;
        }
        match step {
            Step::Move(p) => {
                pointer = p;
                events.push(egui::Event::PointerMoved(p));
            }
            Step::Click(p) => {
                pointer = p;
                events.push(egui::Event::PointerMoved(p));
                events.push(egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
                release = true;
            }
            Step::Idle => {}
        }
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(size[0], size[1]))),
            events,
            // Tooltips wait for the pointer to rest; a frame is 1/60 s.
            time: Some(n as f64 * 0.6),
            predicted_dt: 1.0 / 60.0,
            focused: true,
            ..Default::default()
        };
        input.viewports.entry(egui::ViewportId::ROOT).or_default().native_pixels_per_point = Some(ppp);
        let out = ctx.run(input, |ctx| {
            backdrop(ctx);
            ui(ctx);
        });
        for (id, delta) in &out.textures_delta.set {
            renderer.update_texture(&g.device, &g.queue, *id, delta);
        }
        last = Some(out);
    }
    let out = last.expect("ran a frame");
    let jobs = ctx.tessellate(out.shapes, out.pixels_per_point);
    let screen = egui_wgpu::ScreenDescriptor { size_in_pixels: px, pixels_per_point: out.pixels_per_point };

    let texture = g.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("snapshot"),
        size: wgpu::Extent3d { width: px[0], height: px[1], depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&Default::default());
    let mut enc = g.device.create_command_encoder(&Default::default());
    let cmds = renderer.update_buffers(&g.device, &g.queue, &mut enc, &jobs, &screen);
    {
        let pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        renderer.render(&mut pass.forget_lifetime(), &jobs, &screen);
    }
    let row = (px[0] * 4).div_ceil(256) * 256;
    let buf = g.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(row * px[1]),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    enc.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::ImageCopyBuffer { buffer: &buf, layout: wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: None } },
        wgpu::Extent3d { width: px[0], height: px[1], depth_or_array_layers: 1 },
    );
    g.queue.submit(cmds.into_iter().chain([enc.finish()]));
    buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    g.device.poll(wgpu::Maintain::Wait);
    let data = buf.slice(..).get_mapped_range();
    let mut img = image::RgbaImage::new(px[0], px[1]);
    for y in 0..px[1] {
        let src = &data[(y * row) as usize..(y * row + px[0] * 4) as usize];
        img.as_mut()[(y * px[0] * 4) as usize..((y + 1) * px[0] * 4) as usize].copy_from_slice(src);
    }
    for p in img.pixels_mut() {
        p.0[3] = 255;
    }
    let path = dir.join(format!("{name}.png"));
    img.save(&path).expect("writes the snapshot");
    eprintln!("wrote {}", path.display());
    Some(ctx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::canvas::Canvas;
    use crate::canvas::CanvasManager;
    use crate::input::hotkeys_config::HotkeysConfig;

    /// The panels that show over a slide at once: layers, slides, the
    /// arrange bar under the toolbar, the command palette.
    #[test]
    #[ignore = "writes PNGs to OSN_SNAP_DIR"]
    fn snapshot_panels() {
        let mut slide = Canvas::default();
        slide.add_layer();
        slide.add_layer();
        let mut m = CanvasManager::for_tests(vec![slide, Canvas::default(), Canvas::default(), Canvas::default()]);
        let mut cache = crate::ui::overlay::OverlayCache::new();
        let mut cfg = crate::config::AppConfig::default();
        let mut panel = crate::ui::slide_panel::SlidePanel::default();
        let keys = HotkeysConfig::default().parsed();
        let mut tool = crate::tools::ActiveTool::from_config(&cfg);
        tool.kind = crate::tools::ToolKind::Select;
        let (mut apart, mut dim, mut pack) = (true, false, crate::tools::arrange::PackMode::default());
        let mut commands = crate::ui::command_palette::Palette::default();
        let palette = cfg.palette.clone();
        let cx = crate::ui::command_palette::Context {
            keys: &keys,
            selected: 3,
            picture: false,
            grouped: false,
            inside_group: false,
            pin: false,
            palette: &palette,
            slides: vec![String::new(); 4],
            active_slide: 0,
            deleted_slides: 0,
            pad: false,
        };
        let entries = crate::ui::command_palette::entries(&cx);
        let mut draw = |ctx: &egui::Context, with_palette: bool| {
            let (mut pin, mut picking, mut insert, mut quit, mut select) = (None, false, false, false, false);
            let mut filter = crate::ui::media_filter::MediaFilter::default();
            let counts = crate::ui::media_filter::Counts::default();
            let mut pal = cfg.palette.clone();
            crate::ui::toolbar::show(
                ctx, &mut tool, &mut cfg.bg_opacity, &mut cfg.bg_color, &mut pal, &mut cfg.smoothing_level, &mut cfg.smoothing,
                &mut cfg.pressure_curve, &mut cfg.ui_scale, &mut cfg.accent,
                (&mut cfg.shape_snap, &mut cfg.smart_guides, &mut cfg.keep_proportions),
                (&mut cfg.beside_fit, &mut cfg.beside_side), (&mut cfg.picture_search, "", &mut cfg.browser_link, "", &mut false),
                &mut pin, &mut picking, &mut insert, &mut quit, &mut filter, &counts, &mut select, None, false, false, &keys,
            );
            crate::ui::arrange_bar::show(ctx, 3, false, None, &m, &mut apart, &mut dim, &mut pack, &keys, false, None, false, &[], &[], false, &mut None);
            crate::ui::slide_panel::show(ctx, &mut panel, &mut cfg, 10.0, &m);
            crate::ui::layer_panel::show(ctx, &mut cache, 0, &mut m);
            if with_palette {
                if !commands.is_open() {
                    commands.open();
                }
                commands.show(ctx, &entries);
            }
        };
        shoot("panels", [1500.0, 860.0], 1.0, &[Step::Idle], |ctx| draw(ctx, false));
        shoot("palette", [1500.0, 860.0], 1.0, &[Step::Idle], |ctx| draw(ctx, true));
        let mut find = crate::ui::find_bar::FindBar::default();
        find.open();
        let tags = || {
            ["character", "pose", "study", "hands"]
                .iter()
                .enumerate()
                .map(|(i, t)| crate::canvas::TagCount { tag: t.to_string(), slides: i + 1, objects: 3 * i + 2 })
                .collect()
        };
        shoot("find", [1500.0, 860.0], 1.0, &[Step::Idle], |ctx| {
            draw(ctx, false);
            find.show(ctx, |_| Vec::new(), 0, 0, |_| String::new(), None, tags);
        });
    }

    /// The highlight menu with duplicates just added: their row, and
    /// "Unhighlight all".
    #[test]
    #[ignore = "writes PNGs to OSN_SNAP_DIR"]
    fn snapshot_highlight_menu() {
        let keys = HotkeysConfig::default().parsed();
        let mut cfg = crate::config::AppConfig::default();
        let mut tool = crate::tools::ActiveTool::from_config(&cfg);
        let mut filter = crate::ui::media_filter::MediaFilter { duplicates: true, ..Default::default() };
        let counts = crate::ui::media_filter::Counts { media: [12, 1, 0], groups: 2, duplicates: Some(2), ..Default::default() };
        shoot("highlight-menu", [1500.0, 760.0], 1.0, &[Step::Idle], |ctx| {
            ctx.memory_mut(|m| m.open_popup(egui::Id::new("osn_media_filter_popup")));
            let (mut pin, mut picking, mut insert, mut quit, mut select) = (None, false, false, false, false);
            let mut pal = cfg.palette.clone();
            crate::ui::toolbar::show(
                ctx, &mut tool, &mut cfg.bg_opacity, &mut cfg.bg_color, &mut pal, &mut cfg.smoothing_level, &mut cfg.smoothing,
                &mut cfg.pressure_curve, &mut cfg.ui_scale, &mut cfg.accent,
                (&mut cfg.shape_snap, &mut cfg.smart_guides, &mut cfg.keep_proportions),
                (&mut cfg.beside_fit, &mut cfg.beside_side), (&mut cfg.picture_search, "", &mut cfg.browser_link, "", &mut false),
                &mut pin, &mut picking, &mut insert, &mut quit, &mut filter, &counts, &mut select, None, false, false, &keys,
            );
        });
        shoot("duplicate-bar", [1100.0, 260.0], 1.0, &[Step::Idle], crate::app::snapshot_dup_bar);
    }

    /// The arrange bar over one tagged picture: each tag a chip that
    /// finds it, the rest as a count. `OSN_SNAP_CLICK=x,y` clicks there in
    /// a second shot and prints what the bar asked for.
    #[test]
    #[ignore = "writes PNGs to OSN_SNAP_DIR"]
    fn snapshot_tag_chips() {
        let m = CanvasManager::for_tests(vec![Canvas::default()]);
        let keys = HotkeysConfig::default().parsed();
        let (mut apart, mut dim, mut pack) = (true, false, crate::tools::arrange::PackMode::default());
        let tags: Vec<(String, bool)> = [("pose", false), ("character", false), ("ref", true), ("hands", false), ("study", false)].iter().map(|(t, s)| (t.to_string(), *s)).collect();
        let mut asked = None;
        let mut draw = |ctx: &egui::Context| {
            if let Some(a) = crate::ui::arrange_bar::show(ctx, 1, false, None, &m, &mut apart, &mut dim, &mut pack, &keys, false, None, false, &tags, &[], false, &mut None) {
                asked = Some(a);
            }
        };
        shoot("tag-chips", [1100.0, 300.0], 1.0, &[Step::Idle], &mut draw);
        let click = std::env::var("OSN_SNAP_CLICK").ok().and_then(|s| {
            let (x, y) = s.split_once(',')?;
            Some(egui::pos2(x.trim().parse().ok()?, y.trim().parse().ok()?))
        });
        if let Some(at) = click {
            shoot("tag-chips-hover", [1100.0, 300.0], 1.0, &[Step::Move(at), Step::Idle, Step::Idle], &mut draw);
            shoot("tag-chips-clicked", [1100.0, 300.0], 1.0, &[Step::Click(at)], &mut draw);
            println!("asked for: {}", match asked {
                Some(crate::ui::arrange_bar::BarAction::FindTag(t)) => format!("find #{t}"),
                Some(_) => "something else".into(),
                None => "nothing".into(),
            });
        }
    }
}
