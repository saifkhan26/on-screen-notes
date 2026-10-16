// GPU ink: see `ink_gpu.rs`. Colour handling, blending inputs and
// dithering are egui-wgpu 0.29's `egui.wgsl`, unchanged; the geometry is
// worked out here from canvas-space nodes and discs. Placeholders in
// CAPITALS are filled in by `shader_source`.

struct VertexOutput {
    @location(0) tex_coord: vec2<f32>,
    @location(1) color: vec4<f32>, // gamma 0-1
    // Polygon discs only: position from the centre in points.
    @location(2) local: vec2<f32>,
    // Polygon discs only: (radius in points, number of sides); sides is
    // 0 for everything else.
    @location(3) @interpolate(flat) poly: vec2<f32>,
    @builtin(position) position: vec4<f32>,
};

struct Locals {
    screen_size: vec2<f32>,
    // A canvas point p lands at pan + (scale * p + off) * zoom.
    pan: vec2<f32>,
    off: vec2<f32>,
    zoom: f32,
    scale: f32,
    pixels_per_point: f32,
    dithering: u32, // 1 if dithering is enabled, 0 otherwise
    // egui's feathering width (TessellationOptions), in points.
    feathering: f32,
    _padding: u32,
};
@group(0) @binding(0) var<uniform> r_locals: Locals;

@group(1) @binding(0) var r_tex_color: texture_2d<f32>;
@group(1) @binding(1) var r_tex_sampler: sampler;

struct Node {
    c: vec2<f32>,
    n: vec2<f32>,
    h: f32,
    color: u32,
};
struct Disc {
    c: vec2<f32>,
    rc: f32,
    color: u32,
    rule: u32,
    _pad: u32,
};
@group(2) @binding(0) var<storage, read> nodes: array<Node>;
@group(2) @binding(1) var<storage, read> discs: array<Disc>;
@group(2) @binding(2) var<storage, read> items: array<u32>;

const FEATHER: f32 = RIBBON_FEATHER_VALUE;
const QUARTER_ROOT_2: f32 = QUARTER_ROOT_2_VALUE;
const WHITE_UV: vec2<f32> = vec2<f32>(0.0, 0.0);
const TAU: f32 = 6.2831853;
const PI: f32 = 3.14159265;

// -----------------------------------------------
// Adapted from
// https://www.shadertoy.com/view/llVGzG
// Originally presented in:
// Jimenez 2014, "Next Generation Post-Processing in Call of Duty"
//
// A good overview can be found in
// https://blog.demofox.org/2022/01/01/interleaved-gradient-noise-a-different-kind-of-low-discrepancy-sequence/
// via https://github.com/rerun-io/rerun/
fn interleaved_gradient_noise(n: vec2<f32>) -> f32 {
    let f = 0.06711056 * n.x + 0.00583715 * n.y;
    return fract(52.9829189 * fract(f));
}

fn dither_interleaved(rgb: vec3<f32>, levels: f32, frag_coord: vec4<f32>) -> vec3<f32> {
    var noise = interleaved_gradient_noise(frag_coord.xy);
    // scale down the noise slightly to ensure flat colors aren't getting dithered
    noise = (noise - 0.5) * 0.95;
    return rgb + noise / (levels - 1.0);
}

// 0-1 linear  from  0-1 sRGB gamma
fn linear_from_gamma_rgb(srgb: vec3<f32>) -> vec3<f32> {
    let cutoff = srgb < vec3<f32>(0.04045);
    let lower = srgb / vec3<f32>(12.92);
    let higher = pow((srgb + vec3<f32>(0.055)) / vec3<f32>(1.055), vec3<f32>(2.4));
    return select(higher, lower, cutoff);
}

// 0-1 sRGB gamma  from  0-1 linear
fn gamma_from_linear_rgb(rgb: vec3<f32>) -> vec3<f32> {
    let cutoff = rgb < vec3<f32>(0.0031308);
    let lower = rgb * vec3<f32>(12.92);
    let higher = vec3<f32>(1.055) * pow(rgb, vec3<f32>(1.0 / 2.4)) - vec3<f32>(0.055);
    return select(higher, lower, cutoff);
}

// 0-1 sRGBA gamma  from  0-1 linear
fn gamma_from_linear_rgba(linear_rgba: vec4<f32>) -> vec4<f32> {
    return vec4<f32>(gamma_from_linear_rgb(linear_rgba.rgb), linear_rgba.a);
}

// [u8; 4] SRGB as u32 -> [r, g, b, a] in 0.-1
fn unpack_color(color: u32) -> vec4<f32> {
    return vec4<f32>(
        f32(color & 255u),
        f32((color >> 8u) & 255u),
        f32((color >> 16u) & 255u),
        f32((color >> 24u) & 255u),
    ) / 255.0;
}

fn position_from_screen(screen_pos: vec2<f32>) -> vec4<f32> {
    return vec4<f32>(
        2.0 * screen_pos.x / r_locals.screen_size.x - 1.0,
        1.0 - 2.0 * screen_pos.y / r_locals.screen_size.y,
        0.0,
        1.0,
    );
}

// Canvas point -> screen point, as the egui path's `to_screen`.
fn to_screen(p: vec2<f32>) -> vec2<f32> {
    return r_locals.pan + (r_locals.scale * p + r_locals.off) * r_locals.zoom;
}

// Zoom that widths scale by (the egui path's layer `zoom`).
fn width_zoom() -> f32 {
    return r_locals.zoom * r_locals.scale;
}

fn hidden() -> VertexOutput {
    var out: VertexOutput;
    out.tex_coord = WHITE_UV;
    out.color = vec4<f32>(0.0);
    out.local = vec2<f32>(0.0);
    out.poly = vec2<f32>(0.0);
    out.position = vec4<f32>(2.0, 2.0, 0.0, 1.0);
    return out;
}

// Band `band` (0 left feather, 1 interior, 2 right feather) of the
// ribbon segment from node `i` to node `i + 1`: two triangles, in the
// order `PainterSink::ribbon` adds them. Each node has four vertices
// across the ribbon: outer left, inner left, inner right, outer right.
fn ribbon_vertex(i: u32, band: u32, corner: u32) -> VertexOutput {
    // Per triangle corner: which node, and whether it is the band's
    // second vertex across. Triangles (0,A) (0,B) (1,A) and
    // (0,B) (1,B) (1,A), with A = band and B = band + 1.
    var node_of = array<u32, 6>(0u, 0u, 1u, 0u, 1u, 1u);
    var second = array<u32, 6>(0u, 1u, 0u, 1u, 1u, 0u);
    let node = nodes[i + node_of[corner]];
    let across = band + second[corner];
    let half = max(node.h * width_zoom(), 0.4);
    var out: VertexOutput;
    out.tex_coord = WHITE_UV;
    out.local = vec2<f32>(0.0);
    out.poly = vec2<f32>(0.0);
    out.color = unpack_color(node.color);
    var reach: f32;
    switch across {
        case 0u: {
            reach = half + FEATHER;
            out.color = vec4<f32>(0.0);
        }
        case 1u: {
            reach = half;
        }
        case 2u: {
            reach = -half;
        }
        default: {
            reach = -(half + FEATHER);
            out.color = vec4<f32>(0.0);
        }
    }
    let p = to_screen(node.c);
    out.position = position_from_screen(vec2<f32>(p.x + node.n.x * reach, p.y + node.n.y * reach));
    return out;
}

// `DiscRule::radius` (render.rs); <= 0 means not drawn.
fn disc_radius(rc: f32, rule: u32) -> f32 {
    let r = rc * width_zoom();
    if rule == 0u {
        return max(r - FEATHER * 0.5, 0.4);
    }
    if rule == 1u {
        return select(-1.0, r, r >= 0.4);
    }
    return r;
}

// One of egui's pre-rasterised discs: radius and width in texels, and
// where it sits in the atlas. r == 0 when none is used.
struct PreDisc {
    r: f32,
    w: f32,
    uv: vec4<f32>,
};

// The first disc whose radius is at least `cutoff`, as
// `Tessellator::tessellate_circle` picks it.
fn prerasterized(cutoff: f32) -> PreDisc {
//DISC_TABLE
    return PreDisc(0.0, 0.0, vec4<f32>(0.0));
}

// A disc as `Tessellator::tessellate_circle` draws it.
fn disc_vertex(d: Disc, corner: u32) -> VertexOutput {
    let radius = disc_radius(d.rc, d.rule);
    if radius <= 0.0 {
        return hidden();
    }
    var qx = array<f32, 6>(0.0, 1.0, 0.0, 0.0, 1.0, 1.0);
    var qy = array<f32, 6>(0.0, 0.0, 1.0, 1.0, 0.0, 1.0);
    let far = vec2<bool>(qx[corner] > 0.5, qy[corner] > 0.5);
    let center = to_screen(d.c);
    var out: VertexOutput;
    out.color = unpack_color(d.color);
    let ppp = r_locals.pixels_per_point;
    let radius_px = radius * ppp;
    let pre = prerasterized(radius_px * QUARTER_ROOT_2);
    var pos: vec2<f32>;
    if pre.r > 0.0 {
        // A quad textured with the disc, `Rect::from_center_size`.
        let side = radius_px * pre.w / (ppp * pre.r);
        let lo = center - vec2<f32>(side, side) * 0.5;
        let hi = center + vec2<f32>(side, side) * 0.5;
        pos = select(lo, hi, far);
        out.tex_coord = select(pre.uv.xy, pre.uv.zw, far);
        out.local = vec2<f32>(0.0);
        out.poly = vec2<f32>(0.0);
    } else {
        // `Path::add_circle` + feathered `fill_closed_path`: a regular
        // polygon whose corners lie on the circle. The quad just covers
        // it; the fragment shader works out the coverage.
        var sides = 128.0;
        if radius <= 2.0 {
            sides = 8.0;
        } else if radius <= 5.0 {
            sides = 16.0;
        } else if radius < 18.0 {
            sides = 32.0;
        } else if radius < 50.0 {
            sides = 64.0;
        }
        let reach = radius + r_locals.feathering + 1.0 / ppp;
        let corner_dir = select(vec2<f32>(-1.0), vec2<f32>(1.0), far);
        pos = center + corner_dir * reach;
        out.tex_coord = WHITE_UV;
        out.local = corner_dir * reach;
        out.poly = vec2<f32>(radius, sides);
    }
    out.position = position_from_screen(pos);
    return out;
}

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    let item = items[vertex_index / 6u];
    let corner = vertex_index % 6u;
    let kind = item >> 30u;
    let index = item & 0x3FFFFFFFu;
    if kind == 3u {
        return disc_vertex(discs[index], corner);
    }
    return ribbon_vertex(index, kind, corner);
}

// Coverage of a feathered regular polygon at `local` from its centre:
// egui insets each corner by half the feather along its radius for the
// opaque inner polygon and pushes it out as much for the transparent
// outer one, and the colour ramps linearly between their edges.
fn polygon_coverage(local: vec2<f32>, radius: f32, sides: f32) -> f32 {
    // With feathering off egui draws the bare polygon; a hair of
    // feather gives the same hard edge without dividing by zero.
    let feather = max(r_locals.feathering, 1e-4);
    let step = TAU / sides;
    let mid = (floor(atan2(local.y, local.x) / step) + 0.5) * step;
    let d = dot(local, vec2<f32>(cos(mid), sin(mid)));
    let c = cos(PI / sides);
    return clamp(((radius + 0.5 * feather) * c - d) / (feather * c), 0.0, 1.0);
}

fn shade(in: VertexOutput) -> vec4<f32> {
    // We always have an sRGB aware texture at the moment.
    let tex_linear = textureSample(r_tex_color, r_tex_sampler, in.tex_coord);
    let tex_gamma = gamma_from_linear_rgba(tex_linear);
    var out_color_gamma = in.color * tex_gamma;
    if in.poly.y > 0.0 {
        out_color_gamma = out_color_gamma * polygon_coverage(in.local, in.poly.x, in.poly.y);
    }
    // Dither the float color down to eight bits to reduce banding.
    // This step is optional for egui backends.
    if r_locals.dithering == 1u {
        let out_color_gamma_rgb = dither_interleaved(out_color_gamma.rgb, 256.0, in.position);
        out_color_gamma = vec4<f32>(out_color_gamma_rgb, out_color_gamma.a);
    }
    return out_color_gamma;
}

@fragment
fn fs_main_linear_framebuffer(in: VertexOutput) -> @location(0) vec4<f32> {
    let out_color_gamma = shade(in);
    let out_color_linear = linear_from_gamma_rgb(out_color_gamma.rgb);
    return vec4<f32>(out_color_linear, out_color_gamma.a);
}

@fragment
fn fs_main_gamma_framebuffer(in: VertexOutput) -> @location(0) vec4<f32> {
    return shade(in);
}
