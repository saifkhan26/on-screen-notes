//! A drawing pad's pixels on the GPU while it is drawn on (`canvas::pad`).
//!
//! Three textures the picture's size:
//!   * the pad as it was before the stroke in hand;
//!   * the stroke: the pen's and the eraser's stamps max-blended in, so
//!     a stroke never darkens where it crosses itself, however many
//!     stamps overlap; the pencil's piled up, each over the last, so it
//!     builds up as graphite does. Half floats, so its light layers add
//!     up without banding;
//!   * what shows: the pad with the stroke on it, rebuilt only where new
//!     stamps fell. egui draws it in the picture's place
//!     (`OverlayCache::set_pad`), so pan, zoom, turn, mirror and the slide
//!     window all work as for any picture.
//!
//! At pen-up the stroke's rectangle of the pad is copied aside for undo
//! and what shows becomes the pad. Each frame's stamps are one draw call;
//! nothing comes back to the CPU until the pad is put away
//! (`read_pixels`). Blending is in the stored (sRGB) values, as egui
//! blends; egui samples the shown texture through an sRGB view, which
//! gives back exactly those values.

use crate::canvas::pad::{self, Brush, Stamp};
use eframe::{egui_wgpu, wgpu};
use std::sync::{Arc, OnceLock};

/// What the textures hold: premultiplied RGBA, sRGB-encoded values.
const FMT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
/// How egui reads the shown one.
const SRGB: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
/// The stroke in hand: the same values, finer.
const STROKE_FMT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// GPU memory the undo steps of one pad may hold.
const UNDO_BUDGET: u64 = 256 << 20;

const STAMP_WGSL: &str = r#"
struct U { size: vec2<f32>, hard: f32, _unused: f32, color: vec4<f32> };
@group(0) @binding(0) var<uniform> u: U;

struct VOut {
  @builtin(position) pos: vec4<f32>,
  @location(0) local: vec2<f32>,
  @location(1) flow: f32,
  @location(2) r: f32,
};

@vertex
fn vs(@builtin(vertex_index) vi: u32, @location(0) c: vec2<f32>, @location(1) r: f32, @location(2) flow: f32) -> VOut {
  var corners = array<vec2<f32>, 6>(
    vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
    vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, 1.0), vec2<f32>(-1.0, 1.0));
  let k = corners[vi];
  let p = c + k * (r + 1.5);
  var o: VOut;
  o.pos = vec4<f32>(p.x / u.size.x * 2.0 - 1.0, 1.0 - p.y / u.size.y * 2.0, 0.0, 1.0);
  o.local = k * (r + 1.5);
  o.flow = flow;
  o.r = r;
  return o;
}

@fragment
fn fs(i: VOut) -> @location(0) vec4<f32> {
  let d = length(i.local);
  let inner = i.r * u.hard;
  let cov = clamp((i.r + 0.5 - d) / max(i.r + 0.5 - inner, 1.0), 0.0, 1.0);
  let a = cov * i.flow * u.color.a;
  return vec4<f32>(u.color.rgb * a, a);
}
"#;

const COMPOSE_WGSL: &str = r#"
@group(0) @binding(0) var stroke_tex: texture_2d<f32>;

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
  let x = f32((vi << 1u) & 2u) * 2.0 - 1.0;
  let y = f32(vi & 2u) * 2.0 - 1.0;
  return vec4<f32>(x, y, 0.0, 1.0);
}

@fragment
fn fs(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
  return textureLoad(stroke_tex, vec2<i32>(p.xy), 0);
}
"#;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    size: [f32; 2],
    hard: f32,
    _unused: f32,
    color: [f32; 4],
}

struct Gpu {
    rs: egui_wgpu::RenderState,
    stamp_layout: wgpu::BindGroupLayout,
    compose_layout: wgpu::BindGroupLayout,
    /// Stamps into the stroke, the strongest winning.
    stamp: wgpu::RenderPipeline,
    /// Stamps into the stroke, each over the last (`Brush::builds_up`).
    stamp_pile: wgpu::RenderPipeline,
    /// The stroke over what shows.
    over: wgpu::RenderPipeline,
    /// What shows, cleared where the stroke covers it.
    clear: wgpu::RenderPipeline,
}

static GPU: OnceLock<Gpu> = OnceLock::new();

/// Build the pipelines once, from the device eframe draws with.
pub fn init(rs: &egui_wgpu::RenderState) {
    if GPU.get().is_some() {
        return;
    }
    let device = &rs.device;
    let tex_entry = |binding| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    };
    let stamp_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("pad stamp"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    });
    let compose_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("pad compose"),
        entries: &[tex_entry(0)],
    });
    let stamp_mod = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("pad stamp"),
        source: wgpu::ShaderSource::Wgsl(STAMP_WGSL.into()),
    });
    let compose_mod = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("pad compose"),
        source: wgpu::ShaderSource::Wgsl(COMPOSE_WGSL.into()),
    });
    let blend = |src, dst, op| wgpu::BlendComponent { src_factor: src, dst_factor: dst, operation: op };
    let pipeline = |label: &str, module: &wgpu::ShaderModule, layout: &wgpu::BindGroupLayout, buffers: &[wgpu::VertexBufferLayout], format: wgpu::TextureFormat, b: wgpu::BlendComponent| {
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(label),
            bind_group_layouts: &[layout],
            push_constant_ranges: &[],
        });
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(&pl),
            vertex: wgpu::VertexState { module, entry_point: "vs", compilation_options: Default::default(), buffers },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module,
                entry_point: "fs",
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState { color: b, alpha: b }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
            cache: None,
        })
    };
    use wgpu::{BlendFactor as F, BlendOperation as Op};
    let instances = [wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<Stamp>() as u64,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32, 2 => Float32],
    }];
    let over_blend = blend(F::One, F::OneMinusSrcAlpha, Op::Add);
    let stamp = pipeline("pad stamp", &stamp_mod, &stamp_layout, &instances, STROKE_FMT, blend(F::One, F::One, Op::Max));
    let stamp_pile = pipeline("pad stamp pile", &stamp_mod, &stamp_layout, &instances, STROKE_FMT, over_blend);
    let over = pipeline("pad over", &compose_mod, &compose_layout, &[], FMT, over_blend);
    let clear = pipeline("pad clear", &compose_mod, &compose_layout, &[], FMT, blend(F::Zero, F::OneMinusSrcAlpha, Op::Add));

    let _ = GPU.set(Gpu { rs: rs.clone(), stamp_layout, compose_layout, stamp, stamp_pile, over, clear });
}

/// Pads can be drawn on: the GPU is set up.
pub fn available() -> bool {
    GPU.get().is_some()
}

/// Largest picture side this GPU takes.
pub fn max_side() -> u32 {
    GPU.get().map_or(0, |g| g.rs.device.limits().max_texture_dimension_2d.min(pad::MAX_SIDE))
}

/// Let egui forget a shown texture. Call it a frame after the texture was
/// last drawn, never in the frame that drew it.
pub fn free_texture(id: egui::TextureId) {
    if let Some(g) = GPU.get() {
        g.rs.renderer.write().free_texture(&id);
    }
}

fn corner(t: &wgpu::Texture, x: u32, y: u32) -> wgpu::ImageCopyTexture<'_> {
    wgpu::ImageCopyTexture { texture: t, mip_level: 0, origin: wgpu::Origin3d { x, y, z: 0 }, aspect: wgpu::TextureAspect::All }
}

fn extent(r: [u32; 4]) -> wgpu::Extent3d {
    wgpu::Extent3d { width: r[2] - r[0], height: r[3] - r[1], depth_or_array_layers: 1 }
}

fn texture(device: &wgpu::Device, label: &str, w: u32, h: u32, srgb_view: bool) -> wgpu::Texture {
    texture_as(device, label, w, h, FMT, srgb_view)
}

fn texture_as(device: &wgpu::Device, label: &str, w: u32, h: u32, format: wgpu::TextureFormat, srgb_view: bool) -> wgpu::Texture {
    let view_formats: &[wgpu::TextureFormat] = if srgb_view { &[SRGB] } else { &[] };
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST,
        view_formats,
    })
}

/// A rectangle of the pad as it was, for undo or redo.
struct Snap {
    rect: [u32; 4],
    tex: wgpu::Texture,
}

impl Snap {
    fn bytes(&self) -> u64 {
        (self.rect[2] - self.rect[0]) as u64 * (self.rect[3] - self.rect[1]) as u64 * 4
    }
}

/// One picture being drawn on.
pub struct Session {
    size: [u32; 2],
    pad: wgpu::Texture,
    display: wgpu::Texture,
    /// Keeps the stroke texture (only ever used through its view).
    stroke_view: wgpu::TextureView,
    display_view: wgpu::TextureView,
    tex_id: egui::TextureId,
    uniforms: wgpu::Buffer,
    instances: wgpu::Buffer,
    instance_cap: usize,
    stamp_bg: wgpu::BindGroup,
    compose_bg: wgpu::BindGroup,
    pending: Vec<Stamp>,
    /// What the stroke in hand has touched so far.
    stroke_rect: Option<[u32; 4]>,
    /// The stroke in hand clears to see-through (an eraser on a picture
    /// that has see-through parts).
    clearing: bool,
    /// The stroke in hand's stamps pile up (`Brush::builds_up`).
    pile: bool,
    /// The eraser paints white back: an opaque picture with no paper of
    /// its own. Otherwise it clears (a pad's paper shows through).
    pub erase_white: bool,
    undo: Vec<Snap>,
    redo: Vec<Snap>,
    /// Bumped by every stroke, undo and redo: whether the pixels changed.
    pub edits: u64,
}

impl Session {
    /// Open `rgba` (straight, `w`×`h`) for drawing; `has_paper` for a pad
    /// (paper behind its pixels). `None` without a GPU or for a picture
    /// too big for it.
    pub fn new(mut rgba: Vec<u8>, w: u32, h: u32, has_paper: bool) -> Option<Self> {
        let g = GPU.get()?;
        if w == 0 || h == 0 || w > max_side() || h > max_side() || rgba.len() != (w * h * 4) as usize {
            return None;
        }
        let device = &g.rs.device;
        let erase_white = !has_paper && pad::is_opaque(&rgba);
        pad::premultiply(&mut rgba);
        let pad_tex = texture(device, "pad", w, h, false);
        let stroke = texture_as(device, "pad stroke", w, h, STROKE_FMT, false);
        let display = texture(device, "pad shown", w, h, true);
        let full = [0, 0, w, h];
        for t in [&pad_tex, &display] {
            g.rs.queue.write_texture(
                corner(t, 0, 0),
                &rgba,
                wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(w * 4), rows_per_image: Some(h) },
                extent(full),
            );
        }
        let stroke_view = stroke.create_view(&Default::default());
        let display_view = display.create_view(&Default::default());
        let display_srgb = display.create_view(&wgpu::TextureViewDescriptor { format: Some(SRGB), ..Default::default() });
        let tex_id = g.rs.renderer.write().register_native_texture(device, &display_srgb, wgpu::FilterMode::Linear);
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pad uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let instance_cap = 4096;
        let instances = instance_buffer(device, instance_cap);
        let stamp_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("pad stamp"),
            layout: &g.stamp_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: uniforms.as_entire_binding() }],
        });
        let compose_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("pad compose"),
            layout: &g.compose_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&stroke_view) }],
        });
        Some(Self {
            size: [w, h],
            pad: pad_tex,
            display,
            stroke_view,
            display_view,
            tex_id,
            uniforms,
            instances,
            instance_cap,
            stamp_bg,
            compose_bg,
            pending: Vec::new(),
            stroke_rect: None,
            clearing: false,
            pile: false,
            erase_white,
            undo: Vec::new(),
            redo: Vec::new(),
            edits: 0,
        })
    }

    /// The pixels' size.
    pub fn size(&self) -> [u32; 2] {
        self.size
    }

    /// What egui draws in the picture's place.
    pub fn texture_id(&self) -> egui::TextureId {
        self.tex_id
    }

    /// A stroke is in hand.
    pub fn drawing(&self) -> bool {
        self.stroke_rect.is_some() || !self.pending.is_empty()
    }

    /// Start a stroke with `brush` in `color` (straight sRGB RGBA).
    pub fn begin(&mut self, brush: Brush, color: [u8; 4]) {
        let Some(g) = GPU.get() else { return };
        self.end();
        let look = brush.look();
        let rgba = match brush {
            // Paints white back, or clears (only its coverage counts).
            Brush::Eraser => [1.0, 1.0, 1.0, 1.0],
            _ => [color[0] as f32 / 255.0, color[1] as f32 / 255.0, color[2] as f32 / 255.0, color[3] as f32 / 255.0],
        };
        self.clearing = brush == Brush::Eraser && !self.erase_white;
        self.pile = brush.builds_up();
        let u = Uniforms {
            size: [self.size[0] as f32, self.size[1] as f32],
            hard: look.hard,
            _unused: 0.0,
            color: rgba,
        };
        g.rs.queue.write_buffer(&self.uniforms, 0, bytemuck::bytes_of(&u));
    }

    /// Stamps for the stroke in hand; drawn by the next `flush`.
    pub fn push(&mut self, stamps: &[Stamp]) {
        self.pending.extend_from_slice(stamps);
    }

    /// Draw the stamps pushed since the last flush, and show them.
    pub fn flush(&mut self) {
        let Some(g) = GPU.get() else { return };
        if self.pending.is_empty() {
            return;
        }
        let Some(rect) = pad::bounds(&self.pending, self.size) else {
            self.pending.clear();
            return;
        };
        let device = &g.rs.device;
        if self.pending.len() > self.instance_cap {
            self.instance_cap = self.pending.len().next_power_of_two();
            self.instances = instance_buffer(device, self.instance_cap);
        }
        g.rs.queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(&self.pending));
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("pad stamps") });
        {
            let mut pass = render_pass(&mut enc, &self.stroke_view, wgpu::LoadOp::Load);
            pass.set_pipeline(if self.pile { &g.stamp_pile } else { &g.stamp });
            pass.set_bind_group(0, &self.stamp_bg, &[]);
            pass.set_vertex_buffer(0, self.instances.slice(..));
            pass.draw(0..6, 0..self.pending.len() as u32);
        }
        // What shows, there: the pad with the whole stroke on it.
        enc.copy_texture_to_texture(corner(&self.pad, rect[0], rect[1]), corner(&self.display, rect[0], rect[1]), extent(rect));
        {
            let mut pass = render_pass(&mut enc, &self.display_view, wgpu::LoadOp::Load);
            pass.set_scissor_rect(rect[0], rect[1], rect[2] - rect[0], rect[3] - rect[1]);
            pass.set_pipeline(if self.clearing { &g.clear } else { &g.over });
            pass.set_bind_group(0, &self.compose_bg, &[]);
            pass.draw(0..3, 0..1);
        }
        g.rs.queue.submit(Some(enc.finish()));
        self.stroke_rect = Some(pad::union(self.stroke_rect, rect));
        self.pending.clear();
    }

    /// Finish the stroke in hand: it becomes part of the pad, one undo step.
    pub fn end(&mut self) {
        self.flush();
        let Some(g) = GPU.get() else { return };
        let Some(rect) = self.stroke_rect.take() else { return };
        let device = &g.rs.device;
        let snap = Snap { rect, tex: texture(device, "pad undo", rect[2] - rect[0], rect[3] - rect[1], false) };
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("pad commit") });
        enc.copy_texture_to_texture(corner(&self.pad, rect[0], rect[1]), corner(&snap.tex, 0, 0), extent(rect));
        enc.copy_texture_to_texture(corner(&self.display, rect[0], rect[1]), corner(&self.pad, rect[0], rect[1]), extent(rect));
        drop(render_pass(&mut enc, &self.stroke_view, wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)));
        g.rs.queue.submit(Some(enc.finish()));
        self.undo.push(snap);
        self.redo.clear();
        // Oldest steps go first once they hold too much.
        let mut held: u64 = self.undo.iter().map(Snap::bytes).sum();
        while held > UNDO_BUDGET && self.undo.len() > 1 {
            held -= self.undo.remove(0).bytes();
        }
        self.edits += 1;
    }

    /// Take back the last stroke (or redo). `false` when there is none,
    /// or a stroke is in hand.
    pub fn undo(&mut self, redo: bool) -> bool {
        let Some(g) = GPU.get() else { return false };
        if self.drawing() {
            return false;
        }
        let Some(snap) = (if redo { self.redo.pop() } else { self.undo.pop() }) else { return false };
        let r = snap.rect;
        let device = &g.rs.device;
        let now = Snap { rect: r, tex: texture(device, "pad undo", r[2] - r[0], r[3] - r[1], false) };
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("pad undo") });
        enc.copy_texture_to_texture(corner(&self.pad, r[0], r[1]), corner(&now.tex, 0, 0), extent(r));
        enc.copy_texture_to_texture(corner(&snap.tex, 0, 0), corner(&self.pad, r[0], r[1]), extent(r));
        enc.copy_texture_to_texture(corner(&snap.tex, 0, 0), corner(&self.display, r[0], r[1]), extent(r));
        g.rs.queue.submit(Some(enc.finish()));
        if redo { self.undo.push(now) } else { self.redo.push(now) }
        self.edits += 1;
        true
    }

    /// The pad's pixels, straight RGBA, `size()` big. Waits for the GPU
    /// (tens of milliseconds on a big pad). Ends a stroke in hand first.
    pub fn read_pixels(&mut self) -> Option<Vec<u8>> {
        let back = self.start_read()?;
        if !back.wait() {
            log::warn!("could not read a pad back from the GPU");
            return None;
        }
        back.into_pixels()
    }

    /// Start bringing the pad's pixels back from the GPU, as they are
    /// now, without waiting for them (`Readback`). Ends a stroke in hand
    /// first.
    pub fn start_read(&mut self) -> Option<Readback> {
        self.end();
        let g = GPU.get()?;
        let [w, h] = self.size;
        let padded = (w * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let device = &g.rs.device;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pad read back"),
            size: padded as u64 * h as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("pad read back") });
        enc.copy_texture_to_buffer(
            corner(&self.pad, 0, 0),
            wgpu::ImageCopyBuffer {
                buffer: &buffer,
                layout: wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: Some(h) },
            },
            extent([0, 0, w, h]),
        );
        g.rs.queue.submit(Some(enc.finish()));
        let mapped = Arc::new(OnceLock::new());
        let done = Arc::clone(&mapped);
        // No warning here: a readback let go of before it came (the pad
        // put away mid-snapshot) also ends up here, as an error.
        buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = done.set(r.is_ok());
        });
        Some(Readback { buffer, size: self.size, padded, mapped })
    }
}

/// A pad's pixels on their way back from the GPU (`Session::start_read`),
/// so a snapshot of them never holds up the pen: `ready` asks without
/// waiting, and `into_pixels` may run on any thread.
pub struct Readback {
    buffer: wgpu::Buffer,
    size: [u32; 2],
    /// Bytes a row takes in `buffer`: wgpu pads rows to its alignment.
    padded: u32,
    /// Set once the copy is mapped: whether it can be read.
    mapped: Arc<OnceLock<bool>>,
}

impl Readback {
    /// `None` while the pixels are still on their way (the GPU is asked,
    /// without waiting); then whether they could be read.
    pub fn ready(&self) -> Option<bool> {
        if self.mapped.get().is_none() {
            if let Some(g) = GPU.get() {
                let _ = g.rs.device.poll(wgpu::Maintain::Poll);
            }
        }
        self.mapped.get().copied()
    }

    /// Wait for the pixels; whether they could be read.
    fn wait(&self) -> bool {
        if self.mapped.get().is_none() {
            if let Some(g) = GPU.get() {
                let _ = g.rs.device.poll(wgpu::Maintain::Wait);
            }
        }
        self.mapped.get().copied().unwrap_or(false)
    }

    /// The pixels, straight RGBA, once `ready` said they are back; `None`
    /// before, or if they could not be read.
    pub fn into_pixels(self) -> Option<Vec<u8>> {
        if self.mapped.get() != Some(&true) {
            return None;
        }
        let [w, h] = self.size;
        let row = (w * 4) as usize;
        let mut out = Vec::with_capacity(row * h as usize);
        {
            let data = self.buffer.slice(..).get_mapped_range();
            for y in 0..h as usize {
                let start = y * self.padded as usize;
                out.extend_from_slice(&data[start..start + row]);
            }
        }
        self.buffer.unmap();
        pad::unpremultiply(&mut out);
        Some(out)
    }
}

fn instance_buffer(device: &wgpu::Device, cap: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("pad stamps"),
        size: (cap * std::mem::size_of::<Stamp>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn render_pass<'a>(enc: &'a mut wgpu::CommandEncoder, view: &'a wgpu::TextureView, load: wgpu::LoadOp<wgpu::Color>) -> wgpu::RenderPass<'a> {
    enc.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: None,
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            resolve_target: None,
            ops: wgpu::Operations { load, store: wgpu::StoreOp::Store },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real pipelines on this machine's GPU, without a window. `None`
    /// (the test passes without checking) where there is no GPU.
    fn gpu() -> Option<()> {
        if GPU.get().is_some() {
            return Some(());
        }
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor { backends: wgpu::Backends::PRIMARY, ..Default::default() });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default(), None)).ok()?;
        let renderer = egui_wgpu::Renderer::new(&device, wgpu::TextureFormat::Bgra8Unorm, None, 1, false);
        let rs = egui_wgpu::RenderState {
            adapter: std::sync::Arc::new(adapter),
            available_adapters: std::sync::Arc::new([]),
            device: std::sync::Arc::new(device),
            queue: std::sync::Arc::new(queue),
            target_format: wgpu::TextureFormat::Bgra8Unorm,
            renderer: std::sync::Arc::new(egui::mutex::RwLock::new(renderer)),
        };
        init(&rs);
        Some(())
    }

    fn at(px: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * w + x) * 4) as usize;
        [px[i], px[i + 1], px[i + 2], px[i + 3]]
    }

    fn line(walk: &mut pad::Walk, from: [f32; 2], to: [f32; 2], out: &mut Vec<Stamp>) {
        for k in 0..=20 {
            let f = k as f32 / 20.0;
            walk.step([from[0] + (to[0] - from[0]) * f, from[1] + (to[1] - from[1]) * f], 1.0, out);
        }
    }

    #[test]
    fn strokes_land_never_darken_themselves_and_undo() {
        if gpu().is_none() {
            eprintln!("no GPU here; skipped");
            return;
        }
        let (w, h) = (64u32, 48u32);
        let white = vec![255u8; (w * h * 4) as usize];
        let mut s = Session::new(white.clone(), w, h, false).expect("opens");
        assert!(s.erase_white);
        // A half-transparent pen stroke that crosses itself: an X.
        s.begin(Brush::Pen, [0, 0, 0, 128]);
        let mut stamps = Vec::new();
        let mut walk = pad::Walk::new(Brush::Pen, 4.0, 1.0);
        line(&mut walk, [8.0, 8.0], [56.0, 40.0], &mut stamps);
        line(&mut walk, [56.0, 40.0], [56.0, 8.0], &mut stamps);
        line(&mut walk, [56.0, 8.0], [8.0, 40.0], &mut stamps);
        s.push(&stamps);
        s.end();
        let px = s.read_pixels().expect("reads back");
        // Ink on the line, paper away from it.
        let on = at(&px, w, 20, 16);
        let crossing = at(&px, w, 32, 24);
        assert!(on[0] < 160 && on[0] > 100, "{on:?}");
        assert_eq!(at(&px, w, 32, 2), [255, 255, 255, 255]);
        // Where the stroke crosses itself it is no darker.
        assert!(crossing[0].abs_diff(on[0]) <= 2, "{crossing:?} vs {on:?}");
        assert_eq!(on[3], 255, "paper stays opaque");

        // The eraser paints paper back.
        s.begin(Brush::Eraser, [0, 0, 0, 255]);
        let mut stamps = Vec::new();
        let mut walk = pad::Walk::new(Brush::Eraser, 6.0, 1.0);
        line(&mut walk, [14.0, 12.0], [26.0, 20.0], &mut stamps);
        s.push(&stamps);
        s.end();
        let px = s.read_pixels().unwrap();
        assert_eq!(at(&px, w, 20, 16), [255, 255, 255, 255]);

        // Undo takes the eraser back, then the ink; redo brings the ink back.
        assert!(s.undo(false));
        assert_eq!(at(&s.read_pixels().unwrap(), w, 20, 16), on);
        assert!(s.undo(false));
        assert_eq!(s.read_pixels().unwrap(), white);
        assert!(!s.undo(false), "nothing left");
        assert!(s.undo(true));
        assert_eq!(at(&s.read_pixels().unwrap(), w, 20, 16), on);
        assert_eq!(s.edits, 5);
        free_texture(s.texture_id());
    }

    #[test]
    fn a_readback_holds_the_pixels_of_when_it_started_and_reads_on_any_thread() {
        if gpu().is_none() {
            return;
        }
        let (w, h) = (70u32, 20u32); // A row that wgpu pads.
        let mut s = Session::new(vec![0u8; (w * h * 4) as usize], w, h, true).expect("opens");
        s.begin(Brush::Pen, [0, 0, 0, 255]);
        s.push(&[Stamp { c: [10.0, 10.0], r: 4.0, flow: 1.0 }]);
        let back = s.start_read().expect("starts");
        let want = s.read_pixels().unwrap();
        // A stroke drawn after it started is not in it.
        s.begin(Brush::Pen, [0, 0, 0, 255]);
        s.push(&[Stamp { c: [50.0, 10.0], r: 4.0, flow: 1.0 }]);
        s.end();
        let start = std::time::Instant::now();
        while back.ready().is_none() {
            assert!(start.elapsed() < std::time::Duration::from_secs(5), "never came back");
            std::thread::yield_now();
        }
        let got = std::thread::spawn(move || back.into_pixels()).join().unwrap().expect("reads");
        assert_eq!(got, want);
        assert_eq!(at(&got, w, 10, 10)[3], 255);
        assert_eq!(at(&got, w, 50, 10)[3], 0);
        // One let go of before it came (the pad put away mid-snapshot)
        // leaves the pad as it was.
        drop(s.start_read());
        assert_eq!(at(&s.read_pixels().unwrap(), w, 50, 10)[3], 255);
        free_texture(s.texture_id());
    }

    #[test]
    fn the_pencil_builds_up_and_darkens_a_little_with_pressure() {
        if gpu().is_none() {
            return;
        }
        let (w, h) = (96u32, 48u32);
        let mut s = Session::new(vec![0u8; (w * h * 4) as usize], w, h, true).expect("opens");
        // One pencil stroke along row `y`, each pass from one x to
        // another, at pressure `p`.
        fn draw(s: &mut Session, y: f32, passes: &[(f32, f32)], p: f32) {
            s.begin(Brush::Pencil, [0, 0, 0, 255]);
            let mut walk = pad::Walk::new(Brush::Pencil, 4.0, 1.0);
            let mut stamps = Vec::new();
            for &(from, to) in passes {
                for k in 0..=40 {
                    walk.step([from + (to - from) * k as f32 / 40.0, y], p, &mut stamps);
                }
            }
            s.push(&stamps);
            s.end();
        }
        draw(&mut s, 8.5, &[(8.0, 88.0)], 1.0);
        draw(&mut s, 20.5, &[(8.0, 88.0)], 0.1);
        let once = s.read_pixels().unwrap();
        let (firm, light) = (at(&once, w, 48, 8)[3], at(&once, w, 48, 20)[3]);
        // One firm pass lays about `PENCIL_PASS` of the colour; a light
        // one less, but never faint.
        let want = |pass: f32| (pass * 255.0).round() as u8;
        assert!(firm.abs_diff(want(pad::PENCIL_PASS)) <= 15, "{firm}");
        assert!(light.abs_diff(want(Brush::pencil_pass(0.1))) <= 15, "{light}");
        assert!(light < firm && light >= firm / 2, "{firm} vs {light}");
        // Pressure set the width too: the light line is thin.
        assert!(at(&once, w, 48, 11)[3] > 0);
        assert_eq!(at(&once, w, 48, 23)[3], 0);
        // Going over a line darkens it, in another stroke or the same one.
        draw(&mut s, 8.5, &[(8.0, 88.0)], 1.0);
        draw(&mut s, 32.5, &[(8.0, 88.0), (88.0, 8.0)], 1.0);
        let twice = s.read_pixels().unwrap();        assert!(at(&twice, w, 48, 8)[3] > firm + 30, "{} vs {firm}", at(&twice, w, 48, 8)[3]);
        assert!(at(&twice, w, 48, 32)[3] > firm + 30, "{} vs {firm}", at(&twice, w, 48, 32)[3]);
        free_texture(s.texture_id());
    }

    #[test]
    fn the_eraser_clears_a_picture_with_see_through_parts() {
        if gpu().is_none() {
            return;
        }
        let (w, h) = (32u32, 32u32);
        let mut px = vec![200u8; (w * h * 4) as usize];
        px[3] = 0; // One see-through pixel: not paper.
        let mut s = Session::new(px.clone(), w, h, false).expect("opens");
        assert!(!s.erase_white);
        s.begin(Brush::Eraser, [0, 0, 0, 255]);
        s.push(&[Stamp { c: [16.0, 16.0], r: 6.0, flow: 1.0 }]);
        s.end();
        let out = s.read_pixels().unwrap();
        assert_eq!(at(&out, w, 16, 16)[3], 0);
        assert_eq!(at(&out, w, 30, 30), [200, 200, 200, 200]);
        free_texture(s.texture_id());
        // A pad's pixels may be opaque, but its paper is its own: the
        // eraser clears there too, and the paper shows.
        let mut s = Session::new(vec![255u8; (w * h * 4) as usize], w, h, true).expect("opens");
        assert!(!s.erase_white);
        s.begin(Brush::Eraser, [0, 0, 0, 255]);
        s.push(&[Stamp { c: [16.0, 16.0], r: 6.0, flow: 1.0 }]);
        s.end();
        assert_eq!(at(&s.read_pixels().unwrap(), w, 16, 16)[3], 0);
        free_texture(s.texture_id());
    }
}
