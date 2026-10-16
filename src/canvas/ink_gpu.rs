//! GPU-resident ink.
//!
//! Committed strokes are uploaded once, in canvas coordinates, as the
//! discs and ribbons `render::emit_stroke` describes, and a small wgpu
//! pipeline draws them each frame with the current pan and zoom applied
//! in the vertex shader. Before, every stroke was rebuilt into an egui
//! mesh every frame, so frame time grew with the stroke count and a big
//! enough slide overflowed egui's GPU buffers (a fatal wgpu error). Now
//! a frame costs a few draw calls however much ink the slide holds.
//!
//! The look is the egui path's, pixel for pixel. The shader is egui's
//! own (`egui-wgpu`'s `egui.wgsl`: the same vertex colours, texture
//! sampling, blending and dithering), the disc texture is a copy of the
//! pre-rasterised discs egui keeps in its font atlas, and the geometry
//! is worked out the way the egui path works it out:
//!   * ribbon vertices as `PainterSink::ribbon` places them;
//!   * discs as `epaint::Tessellator::tessellate_circle` does: a small
//!     disc is a quad textured from the disc table, a bigger one a
//!     feathered polygon of 8–128 sides whose coverage the fragment
//!     shader computes exactly.
//!
//! Strokes are grouped into chunks of about 64 consecutive strokes. A
//! chunk ends after a stroke whose own hash says so, so boundaries move
//! with the strokes and adding, erasing or moving one stroke rebuilds
//! only its chunk. Chunks are keyed by a hash of everything they draw,
//! kept while in use and dropped after a while unused.
//!
//! What this path cannot draw — filled strokes, strokes without a cache,
//! strokes the Select tool is dragging, chunks too big for one GPU
//! buffer or past the memory budget — goes through the egui path, in
//! paint order. `OSN_CPU_INK=1` uses the egui path for everything, for
//! comparing the two.
//!
//! The pipeline is built for the render pass eframe sets up by default:
//! one sample per pixel and no depth buffer. Turning on
//! `NativeOptions::multisampling` or `depth_buffer` needs the same here.

use crate::canvas::render::{self, DiscRule, InkSink};
use crate::canvas::stroke::{Stroke, StrokeStyle};
use eframe::egui_wgpu;
use eframe::wgpu;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

/// One ribbon node: centre, unit normal and half-width in canvas units,
/// colour as egui vertex colour bits.
#[repr(C)]
#[derive(Clone, Copy)]
struct Node {
    c: [f32; 2],
    n: [f32; 2],
    h: f32,
    color: u32,
}

/// One disc: centre and radius in canvas units, colour, `DiscRule`.
#[repr(C)]
#[derive(Clone, Copy)]
struct Disc {
    c: [f32; 2],
    rc: f32,
    color: u32,
    rule: u32,
    _pad: u32,
}

/// Uniforms for one draw: where the canvas sits on screen and how the
/// shader should finish colours. Layout matches `Locals` in the shader.
#[repr(C)]
#[derive(Clone, Copy)]
struct Locals {
    screen_size: [f32; 2],
    pan: [f32; 2],
    off: [f32; 2],
    zoom: f32,
    scale: f32,
    ppp: f32,
    dithering: u32,
    /// egui's feathering width, in points.
    feathering: f32,
    _pad: u32,
}

// SAFETY: plain `repr(C)` structs of 4-byte fields with no padding
// bytes, so every bit pattern is valid and they may be viewed as bytes.
unsafe impl bytemuck::Zeroable for Node {}
unsafe impl bytemuck::Pod for Node {}
unsafe impl bytemuck::Zeroable for Disc {}
unsafe impl bytemuck::Pod for Disc {}
unsafe impl bytemuck::Zeroable for Locals {}
unsafe impl bytemuck::Pod for Locals {}

/// An item is one quad (two triangles, six vertices): its kind in the
/// top two bits, an index below. Kinds 0–2 are the three bands of the
/// ribbon segment starting at node `index` (left feather, interior,
/// right feather); kind 3 is disc `index`.
const DISC_ITEM: u32 = 3 << 30;

/// A chunk ends after a stroke whose hash has these bits clear, so
/// chunks hold about 64 strokes; never more than `CHUNK_MAX`.
const BOUNDARY_MASK: u64 = 63;
const CHUNK_MAX: usize = 256;
/// Largest chunk the GPU path takes. Each buffer stays well inside
/// wgpu's default 128 MB storage binding limit.
const MAX_CHUNK_NODES: usize = 2_000_000;
const MAX_CHUNK_ITEMS: usize = 8_000_000;
/// GPU memory all chunks together may use. Past it, the least recently
/// used chunks are dropped, and if that is not enough the rest of the
/// ink goes through the egui path, which has its own budget.
const MAX_GPU_BYTES: u64 = 384 << 20;
/// Frames a chunk is kept after it was last drawn.
const KEEP_FRAMES: u64 = 600;
/// Frames before a chunk that did not fit is tried again.
const RETRY_FRAMES: u64 = 120;

/// Mirrors eframe's `NativeOptions::dithering` (on by default), which
/// egui's renderer was created with.
const DITHERING: bool = true;

/// Everything one chunk draws, ready to upload.
#[derive(Default)]
struct ChunkData {
    nodes: Vec<Node>,
    discs: Vec<Disc>,
    items: Vec<u32>,
}

fn color_bits(c: egui::Color32) -> u32 {
    u32::from_le_bytes(c.to_array())
}

impl InkSink for ChunkData {
    fn disc(&mut self, c: [f32; 2], rc: f32, rule: DiscRule, color: egui::Color32) {
        self.items.push(DISC_ITEM | self.discs.len() as u32);
        self.discs.push(Disc { c, rc, color: color_bits(color), rule: rule as u32, _pad: 0 });
    }

    fn ribbon(&mut self, points: &[[f32; 2]], widths: &[f32], style: StrokeStyle, color: egui::Color32) {
        let base = self.nodes.len() as u32;
        let color = color_bits(color);
        for i in 0..points.len() {
            self.nodes.push(Node {
                c: points[i],
                n: render::ribbon_normal(points, i),
                h: render::ribbon_half(widths, style, i),
                color,
            });
        }
        for i in 0..points.len() as u32 - 1 {
            for band in 0..3 {
                self.items.push(band << 30 | (base + i));
            }
        }
    }

    fn fill(&mut self, _points: &[[f32; 2]], _color: egui::Color32) {
        // Filled strokes never reach this path (`drawable`).
        debug_assert!(false, "filled stroke in the GPU ink cache");
    }
}

impl ChunkData {
    fn bytes(&self) -> u64 {
        (self.nodes.len() * std::mem::size_of::<Node>()
            + self.discs.len() * std::mem::size_of::<Disc>()
            + self.items.len() * 4) as u64
    }

    fn fits(&self) -> bool {
        self.nodes.len() <= MAX_CHUNK_NODES
            && self.discs.len() <= MAX_CHUNK_NODES
            && self.items.len() <= MAX_CHUNK_ITEMS
    }
}

/// Whether the GPU path can draw `stroke`.
pub(crate) fn drawable(stroke: &Stroke) -> bool {
    stroke.cache.is_some() && !stroke.filled && !stroke.samples.is_empty()
}

fn mix(h: u64, v: u64) -> u64 {
    (h.rotate_left(5) ^ v).wrapping_mul(0x517C_C1B7_2722_0A95)
}

/// Final avalanche (splitmix64), so every bit depends on every input.
fn finish(mut h: u64) -> u64 {
    h ^= h >> 30;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94D0_49BB_1331_11EB);
    h ^ (h >> 31)
}

/// Hash of everything about `stroke` that its ink depends on. The
/// cache's `rev` changes whenever its geometry does; the rest are the
/// fields painting reads directly.
fn stroke_key(s: &Stroke) -> u64 {
    let mut h = mix(0x9E37_79B9_7F4A_7C15, s.cache.as_ref().map_or(u64::MAX, |c| c.rev));
    h = mix(h, u32::from_le_bytes(s.color) as u64);
    h = mix(h, s.style as u64);
    h = mix(h, s.filled as u64);
    h = mix(h, s.base_width.to_bits() as u64);
    h = mix(h, s.samples.len() as u64);
    if let Some(p) = s.samples.first() {
        h = mix(h, p.pos[0].to_bits() as u64 | (p.pos[1].to_bits() as u64) << 32);
        h = mix(h, p.pressure.to_bits() as u64);
    }
    h
}

struct GpuChunk {
    bind_group: wgpu::BindGroup,
}

struct Chunk {
    /// Item offset of each stroke's ink, plus the end.
    starts: Arc<[u32]>,
    /// Canvas-space bounds of the strokes drawn here.
    bbox: Option<[f32; 4]>,
    /// Every stroke in the chunk is drawn here.
    all: bool,
    /// Waiting to be uploaded by the next draw that uses the chunk.
    data: Option<ChunkData>,
    gpu: Option<GpuChunk>,
    bytes: u64,
    last_used: u64,
    /// Did not fit; the frame it last failed to.
    rejected: Option<u64>,
}

#[derive(Default)]
struct Store {
    chunks: HashMap<u64, Chunk>,
    bytes: u64,
    frame: u64,
}

fn store() -> MutexGuard<'static, Store> {
    static STORE: OnceLock<Mutex<Store>> = OnceLock::new();
    STORE.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

impl Store {
    /// Drop chunks unused for `KEEP_FRAMES`, once per frame.
    fn begin_frame(&mut self, frame: u64) {
        if frame == self.frame {
            return;
        }
        self.frame = frame;
        let mut freed = 0;
        self.chunks.retain(|_, c| {
            let keep = c.last_used + KEEP_FRAMES >= frame;
            if !keep {
                freed += c.bytes;
            }
            keep
        });
        self.bytes -= freed;
    }

    /// Free GPU memory for `bytes` more, dropping chunks not used this
    /// frame, least recently used first.
    fn make_room(&mut self, bytes: u64, frame: u64) -> bool {
        while self.bytes + bytes > MAX_GPU_BYTES {
            let victim = self
                .chunks
                .iter()
                .filter(|(_, c)| c.last_used < frame && c.bytes > 0)
                .min_by_key(|(_, c)| c.last_used)
                .map(|(k, _)| *k);
            let Some(k) = victim else { return false };
            if let Some(c) = self.chunks.remove(&k) {
                self.bytes -= c.bytes;
            }
        }
        true
    }
}

/// What the renderer needs to know about one chunk of a layer.
pub(crate) struct ChunkPlan {
    pub key: u64,
    /// Index of the chunk's first stroke in the layer, and how many.
    pub first: usize,
    pub len: usize,
    pub starts: Arc<[u32]>,
    pub bbox: Option<[f32; 4]>,
    /// The chunk is on (or on its way to) the GPU; when false, its
    /// strokes go through the egui path.
    pub on_gpu: bool,
    /// Every stroke in the chunk is drawn by the GPU path.
    pub all: bool,
}

/// Whether the GPU path is set up.
pub(crate) fn enabled() -> bool {
    GPU.get().is_some()
}

/// Split `strokes` into chunks and make sure each is stored, building
/// the ones that are not. `opacity` is the layer's.
pub(crate) fn plan_layer(ctx: &egui::Context, strokes: &[Stroke], opacity: f32) -> Vec<ChunkPlan> {
    let frame = ctx.cumulative_pass_nr();
    let (fan_on, fan_step) = render::fan_corners();
    let seed = mix(mix(mix(1, opacity.to_bits() as u64), fan_on as u64), fan_step.to_bits() as u64);
    let mut st = store();
    st.begin_frame(frame);
    let mut plans = Vec::new();
    let mut first = 0;
    let mut h = seed;
    for (i, s) in strokes.iter().enumerate() {
        let k = stroke_key(s);
        h = mix(h, k);
        let len = i + 1 - first;
        if finish(k) & BOUNDARY_MASK == 0 || len == CHUNK_MAX || i + 1 == strokes.len() {
            let key = finish(mix(h, len as u64));
            plans.push(ensure(&mut st, key, &strokes[first..=i], first, opacity, frame));
            first = i + 1;
            h = seed;
        }
    }
    plans
}

fn ensure(st: &mut Store, key: u64, strokes: &[Stroke], first: usize, opacity: f32, frame: u64) -> ChunkPlan {
    let plan = |c: &Chunk, on_gpu: bool| ChunkPlan {
        key,
        first,
        len: strokes.len(),
        starts: c.starts.clone(),
        bbox: c.bbox,
        on_gpu,
        all: c.all,
    };
    if let Some(c) = st.chunks.get_mut(&key) {
        c.last_used = frame;
        match c.rejected {
            None => return plan(c, true),
            Some(at) if at + RETRY_FRAMES > frame => return plan(c, false),
            Some(_) => {
                st.chunks.remove(&key);
            }
        }
    }

    let mut data = ChunkData::default();
    let mut starts = Vec::with_capacity(strokes.len() + 1);
    let mut bbox: Option<[f32; 4]> = None;
    let mut all = true;
    for s in strokes {
        starts.push(data.items.len() as u32);
        if !drawable(s) {
            all = false;
            continue;
        }
        render::emit_stroke(&mut data, s, 1.0, 0, opacity);
        if let Some(b) = s.bounds() {
            bbox = Some(match bbox {
                None => b,
                Some(a) => [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])],
            });
        }
    }
    starts.push(data.items.len() as u32);
    let bytes = data.bytes();
    let ok = data.fits() && st.make_room(bytes, frame);
    let chunk = Chunk {
        starts: starts.into(),
        bbox,
        all,
        data: ok.then_some(data),
        gpu: None,
        bytes: if ok { bytes } else { 0 },
        last_used: frame,
        rejected: (!ok).then_some(frame),
    };
    if !ok {
        log::debug!("GPU ink: a chunk of {} strokes ({bytes} bytes) goes through egui", strokes.len());
    }
    let p = plan(&chunk, ok);
    st.bytes += chunk.bytes;
    st.chunks.insert(key, chunk);
    p
}

/// Where a layer's canvas sits on screen: a canvas point `p` lands at
/// `pan + (scale · p + off) · zoom`, and widths scale by `zoom · scale`.
/// `scale` and `off` are an in-progress layer gesture (1 and 0 outside
/// one); this is the egui path's `to_screen` term for term.
#[derive(Clone, Copy)]
pub(crate) struct Xform {
    pub pan: [f32; 2],
    pub zoom: f32,
    pub scale: f32,
    pub off: [f32; 2],
}

/// Consecutive GPU-drawn ink, gathered until something drawn the egui
/// way has to come between.
pub(crate) struct Run {
    xf: Xform,
    ranges: Vec<(u64, u32, u32)>,
}

impl Run {
    pub fn new(xf: Xform) -> Self {
        Self { xf, ranges: Vec::new() }
    }

    /// Add items `a..b` of chunk `key`.
    pub fn push(&mut self, key: u64, a: u32, b: u32) {
        if a >= b {
            return;
        }
        match self.ranges.last_mut() {
            Some((k, _, end)) if *k == key && *end == a => *end = b,
            _ => self.ranges.push((key, a, b)),
        }
    }

    /// Paint what has been gathered, in place in egui's paint order.
    pub fn flush(&mut self, painter: &egui::Painter) {
        if self.ranges.is_empty() {
            return;
        }
        // egui's own feathering, which its discs are drawn with (the
        // app's theme widens it).
        let feathering_px = painter
            .ctx()
            .tessellation_options(|o| if o.feathering { o.feathering_size_in_pixels } else { 0.0 });
        let cb = InkCallback {
            ranges: std::mem::take(&mut self.ranges),
            xf: self.xf,
            feathering_px,
            locals: Mutex::new(None),
        };
        painter.add(egui::Shape::Callback(egui_wgpu::Callback::new_paint_callback(painter.clip_rect(), cb)));
    }
}

struct InkCallback {
    ranges: Vec<(u64, u32, u32)>,
    xf: Xform,
    /// `TessellationOptions::feathering_size_in_pixels`, or 0 with
    /// feathering off.
    feathering_px: f32,
    /// This draw's uniforms, made in `prepare`.
    locals: Mutex<Option<wgpu::BindGroup>>,
}

impl egui_wgpu::CallbackTrait for InkCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        _queue: &wgpu::Queue,
        screen: &egui_wgpu::ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        _resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        use wgpu::util::DeviceExt;
        let Some(gpu) = GPU.get() else { return Vec::new() };
        let ppp = screen.pixels_per_point;
        let locals = Locals {
            screen_size: [screen.size_in_pixels[0] as f32 / ppp, screen.size_in_pixels[1] as f32 / ppp],
            pan: self.xf.pan,
            off: self.xf.off,
            zoom: self.xf.zoom,
            scale: self.xf.scale,
            ppp,
            dithering: DITHERING as u32,
            feathering: self.feathering_px / ppp,
            _pad: 0,
        };
        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("ink locals"),
            contents: bytemuck::bytes_of(&locals),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ink locals"),
            layout: &gpu.locals_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: buffer.as_entire_binding() }],
        });
        *self.locals.lock().unwrap_or_else(|e| e.into_inner()) = Some(bind_group);

        let mut st = store();
        for &(key, _, _) in &self.ranges {
            if let Some(c) = st.chunks.get_mut(&key) {
                if let Some(data) = c.data.take() {
                    c.gpu = Some(upload(device, gpu, &data));
                }
            }
        }
        Vec::new()
    }

    fn paint(
        &self,
        info: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        _resources: &egui_wgpu::CallbackResources,
    ) {
        let Some(gpu) = GPU.get() else { return };
        let locals = self.locals.lock().unwrap_or_else(|e| e.into_inner());
        let Some(locals) = locals.as_ref() else { return };
        // egui sets the viewport to the callback's rect; the shader
        // works in whole-screen points like egui's own.
        let [w, h] = info.screen_size_px;
        pass.set_viewport(0.0, 0.0, w as f32, h as f32, 0.0, 1.0);
        pass.set_pipeline(&gpu.pipeline);
        pass.set_bind_group(0, locals, &[]);
        pass.set_bind_group(1, &gpu.atlas, &[]);
        let st = store();
        for &(key, a, b) in &self.ranges {
            if let Some(chunk) = st.chunks.get(&key).and_then(|c| c.gpu.as_ref()) {
                pass.set_bind_group(2, &chunk.bind_group, &[]);
                pass.draw(a * 6..b * 6, 0..1);
            }
        }
    }
}

fn upload(device: &wgpu::Device, gpu: &Gpu, data: &ChunkData) -> GpuChunk {
    use wgpu::util::DeviceExt;
    // A storage binding cannot be empty; nothing reads the filler.
    const FILLER: [u8; 32] = [0; 32];
    let buffer = |label: &str, bytes: &[u8]| {
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents: if bytes.is_empty() { &FILLER } else { bytes },
            usage: wgpu::BufferUsages::STORAGE,
        })
    };
    let nodes = buffer("ink nodes", bytemuck::cast_slice(&data.nodes));
    let discs = buffer("ink discs", bytemuck::cast_slice(&data.discs));
    let items = buffer("ink items", bytemuck::cast_slice(&data.items));
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("ink chunk"),
        layout: &gpu.chunk_layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: nodes.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: discs.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 2, resource: items.as_entire_binding() },
        ],
    });
    GpuChunk { bind_group }
}

struct Gpu {
    pipeline: wgpu::RenderPipeline,
    locals_layout: wgpu::BindGroupLayout,
    chunk_layout: wgpu::BindGroupLayout,
    atlas: wgpu::BindGroup,
}

static GPU: OnceLock<Gpu> = OnceLock::new();

/// One pre-rasterised disc in the atlas: its radius and width in
/// texels, and its texture rectangle as `[min_u, min_v, max_u, max_v]`.
struct AtlasDisc {
    r: f32,
    w: f32,
    uv: [f32; 4],
}

const ATLAS_SIZE: [usize; 2] = [128, 32];

/// The discs egui's font atlas holds (`epaint::TextureAtlas::new`),
/// drawn the same way, plus the white texel at (0, 0) that `WHITE_UV`
/// samples. Returns the texture's RGBA bytes and the disc table.
fn disc_atlas() -> (Vec<u8>, Vec<AtlasDisc>) {
    const LARGEST_CIRCLE_RADIUS: f32 = 8.0;
    let mut image = egui::epaint::FontImage::new(ATLAS_SIZE);
    image[(0, 0)] = 1.0;
    let inv_w = 1.0 / ATLAS_SIZE[0] as f32;
    let inv_h = 1.0 / ATLAS_SIZE[1] as f32;
    let mut discs = Vec::new();
    // Discs sit in a row with empty texels around each, as the font
    // atlas pads its entries, so sampling at a disc's edge reads zeros.
    let (mut x, y) = (2usize, 2usize);
    for i in 0.. {
        let r = 2.0_f32.powf(i as f32 / 2.0 - 1.0);
        if r > LARGEST_CIRCLE_RADIUS {
            break;
        }
        let hw = (r + 0.5).ceil() as i32;
        let w = (2 * hw + 1) as usize;
        for dx in -hw..=hw {
            for dy in -hw..=hw {
                let distance_to_center = ((dx * dx + dy * dy) as f32).sqrt();
                let coverage = egui::emath::remap_clamp(distance_to_center, (r - 0.5)..=(r + 0.5), 1.0..=0.0);
                image[((x as i32 + hw + dx) as usize, (y as i32 + hw + dy) as usize)] = coverage;
            }
        }
        discs.push(AtlasDisc {
            r,
            w: w as f32,
            uv: [x as f32 * inv_w, y as f32 * inv_h, (x + w) as f32 * inv_w, (y + w) as f32 * inv_h],
        });
        x += w + 1;
    }
    assert!(x <= ATLAS_SIZE[0], "disc atlas too narrow");
    let rgba = image.srgba_pixels(None).flat_map(|c| c.to_array()).collect();
    (rgba, discs)
}

/// The shader, with the disc table and constants filled in. `{:?}`
/// prints an `f32` so that it reads back as the same value.
fn shader_source(discs: &[AtlasDisc]) -> String {
    let mut table = String::new();
    for d in discs {
        table += &format!(
            "    if cutoff <= {r:?} {{ return PreDisc({r:?}, {w:?}, vec4<f32>({:?}, {:?}, {:?}, {:?})); }}\n",
            d.uv[0],
            d.uv[1],
            d.uv[2],
            d.uv[3],
            r = d.r,
            w = d.w,
        );
    }
    include_str!("ink_gpu.wgsl")
        .replace("//DISC_TABLE", &table)
        .replace("RIBBON_FEATHER_VALUE", &format!("{:?}", render::RIBBON_FEATHER))
        .replace("QUARTER_ROOT_2_VALUE", &format!("{:?}", 2.0_f32.powf(0.25)))
}

/// Build the pipeline. Leaves the GPU path off (and ink on the egui
/// path) if the device cannot run it, if `OSN_CPU_INK` asks for that,
/// or if anything fails to validate.
pub fn init(rs: &egui_wgpu::RenderState) {
    if std::env::var_os("OSN_CPU_INK").is_some_and(|v| !v.is_empty() && v != "0") {
        log::info!("GPU ink off: OSN_CPU_INK is set");
        return;
    }
    let caps = rs.adapter.get_downlevel_capabilities();
    if !caps.flags.contains(wgpu::DownlevelFlags::VERTEX_STORAGE)
        || rs.device.limits().max_storage_buffers_per_shader_stage < 3
    {
        log::info!("GPU ink off: no storage buffers in vertex shaders");
        return;
    }
    let device = &rs.device;
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    let gpu = build(rs);
    if let Some(err) = block_on(device.pop_error_scope()) {
        log::warn!("GPU ink off: {err}");
        return;
    }
    let _ = GPU.set(gpu);
}

fn build(rs: &egui_wgpu::RenderState) -> Gpu {
    use wgpu::util::DeviceExt;
    let device = &rs.device;
    let (rgba, discs) = disc_atlas();
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("ink"),
        source: wgpu::ShaderSource::Wgsl(shader_source(&discs).into()),
    });

    let locals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("ink locals"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: std::num::NonZeroU64::new(std::mem::size_of::<Locals>() as u64),
            },
            count: None,
        }],
    });
    let atlas_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("ink atlas"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });
    let storage = |binding| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::VERTEX,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    };
    let chunk_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("ink chunk"),
        entries: &[storage(0), storage(1), storage(2)],
    });

    // The disc atlas, uploaded and sampled exactly as egui-wgpu treats
    // its font texture (sRGB texels, linear filtering, clamped).
    let texture = device.create_texture_with_data(
        &rs.queue,
        &wgpu::TextureDescriptor {
            label: Some("ink discs"),
            size: wgpu::Extent3d {
                width: ATLAS_SIZE[0] as u32,
                height: ATLAS_SIZE[1] as u32,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[wgpu::TextureFormat::Rgba8UnormSrgb],
        },
        wgpu::util::TextureDataOrder::LayerMajor,
        &rgba,
    );
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("ink discs"),
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        ..Default::default()
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let atlas = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("ink atlas"),
        layout: &atlas_layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) },
        ],
    });

    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("ink"),
        bind_group_layouts: &[&locals_layout, &atlas_layout, &chunk_layout],
        push_constant_ranges: &[],
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("ink"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: "vs_main",
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState { count: 1, mask: !0, alpha_to_coverage_enabled: false },
        fragment: Some(wgpu::FragmentState {
            module: &module,
            // Same choice egui-wgpu makes for the same target.
            entry_point: if rs.target_format.is_srgb() {
                "fs_main_linear_framebuffer"
            } else {
                "fs_main_gamma_framebuffer"
            },
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: rs.target_format,
                // egui's blend: premultiplied colour over what is there.
                blend: Some(wgpu::BlendState {
                    color: wgpu::BlendComponent {
                        src_factor: wgpu::BlendFactor::One,
                        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                        operation: wgpu::BlendOperation::Add,
                    },
                    alpha: wgpu::BlendComponent {
                        src_factor: wgpu::BlendFactor::OneMinusDstAlpha,
                        dst_factor: wgpu::BlendFactor::One,
                        operation: wgpu::BlendOperation::Add,
                    },
                }),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview: None,
        cache: None,
    });
    Gpu { pipeline, locals_layout, chunk_layout, atlas }
}

/// Wait for a future that native wgpu completes straight away (an error
/// scope's result).
fn block_on<F: std::future::Future>(f: F) -> F::Output {
    use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
    fn noop(_: *const ()) {}
    fn clone(p: *const ()) -> RawWaker {
        RawWaker::new(p, &VTABLE)
    }
    static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);
    // SAFETY: every vtable function ignores the data pointer.
    let waker = unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) };
    let mut cx = Context::from_waker(&waker);
    let mut f = std::pin::pin!(f);
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::yield_now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::stroke::{PenSample, Stroke};

    fn stroke(n: usize, style: StrokeStyle) -> Stroke {
        let mut s = Stroke::with_style([200, 40, 40, 255], 6.0, style);
        for i in 0..n {
            let t = i as f32;
            s.samples.push(PenSample { pos: [t * 4.0, (t * 0.3).sin() * 20.0], pressure: 0.6, tilt: [0.0; 2] });
        }
        s.build_cache();
        s
    }

    #[test]
    fn shader_validates() {
        use wgpu::naga;
        let (_, discs) = disc_atlas();
        let src = shader_source(&discs);
        let module = naga::front::wgsl::parse_str(&src).unwrap_or_else(|e| panic!("{}", e.emit_to_string(&src)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::empty())
            .validate(&module)
            .unwrap_or_else(|e| panic!("{e:?}"));
    }

    #[test]
    fn disc_table_matches_egui() {
        // The font atlas's discs, read through the same `PreparedDisc`
        // fields the tessellator uses.
        let fonts = egui::epaint::Fonts::new(1.0, 2048, Default::default());
        let theirs = fonts.texture_atlas().lock().prepared_discs();
        let (_, ours) = disc_atlas();
        assert_eq!(theirs.len(), ours.len());
        for (t, o) in theirs.iter().zip(&ours) {
            assert_eq!(t.r, o.r);
            assert_eq!(t.w, o.w);
        }
    }

    #[test]
    fn ribbon_items_cover_every_segment_and_disc() {
        let s = stroke(30, StrokeStyle::Default);
        let n = s.cache.as_ref().unwrap().points.len() as u32;
        let mut d = ChunkData::default();
        render::emit_stroke(&mut d, &s, 1.0, 0, 1.0);
        assert_eq!(d.nodes.len() as u32, n);
        // Two end caps, then three bands per segment.
        assert_eq!(d.discs.len(), 2);
        assert_eq!(d.items.len() as u32, 2 + 3 * (n - 1));
        assert_eq!(d.items[0], DISC_ITEM);
        assert_eq!(d.items[2] >> 30, 0);
        assert_eq!(d.items[2 + 3 * (n - 1) as usize - 1], 2 << 30 | (n - 2));
        assert!(d.items.iter().all(|i| i & ((1 << 30) - 1) < n.max(2)));
    }

    #[test]
    fn chunks_follow_their_strokes() {
        let strokes: Vec<Stroke> = (0..500).map(|i| stroke(5 + i % 7, StrokeStyle::Default)).collect();
        let keys = |s: &[Stroke]| -> Vec<u64> {
            let ctx = egui::Context::default();
            plan_layer(&ctx, s, 1.0).iter().map(|p| p.key).collect()
        };
        let before = keys(&strokes);
        assert!(before.len() > 2, "500 strokes should make several chunks");
        let mut fewer = strokes.clone();
        fewer.remove(3);
        let after = keys(&fewer);
        // Only the chunk that held the removed stroke changes (two if
        // that stroke ended a chunk, or a few if a forced cut at
        // `CHUNK_MAX` moved).
        let changed = before.iter().filter(|k| !after.contains(k)).count();
        assert!((1..=3).contains(&changed), "{before:?} vs {after:?}");
        assert!(before.len() - changed >= before.len() / 2);
    }

    #[test]
    fn filled_and_uncached_strokes_stay_on_egui() {
        let mut filled = stroke(10, StrokeStyle::Default);
        filled.filled = true;
        let mut live = stroke(10, StrokeStyle::Default);
        live.cache = None;
        assert!(!drawable(&filled));
        assert!(!drawable(&live));
        let ctx = egui::Context::default();
        let plans = plan_layer(&ctx, &[stroke(10, StrokeStyle::Marker), filled, live], 0.5);
        let p = &plans[0];
        assert!(!p.all);
        assert_eq!(p.starts[2], p.starts[1]);
        assert_eq!(p.starts[3], p.starts[2]);
    }
}
