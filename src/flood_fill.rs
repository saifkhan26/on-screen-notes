//! Flood-fill action for the Fill tool.
//!
//! Behaviour: at click time we rasterise every visible layer of the
//! active canvas into a tiny-skia pixmap sized to the OSN viewport,
//! treat any alpha > threshold as "ink barrier", then scanline-flood
//! from the click point. Strict gap policy — if the flood reaches a
//! canvas-rect edge, we abort and let the caller toast the user.
//! Otherwise we crop the filled pixel set to its bbox, paint it with
//! the user's fill colour, and hand back a ready-to-store
//! `Shape::Raster`.
//!
//! Cross-layer bounding: ink on *any* visible layer counts as a
//! barrier; the new sticker still lands on the active layer (the
//! caller is responsible for calling `add_shape` on the right
//! canvas). Hidden layers do not contribute to the mask — they are
//! invisible, so the user would be fighting against barriers they
//! cannot see.

use crate::canvas::canvas::Canvas;
use crate::canvas::render;
use crate::canvas::shape::Shape;
use image::RgbaImage;

/// Outcome of a flood-fill attempt. `Filled` carries the new shape
/// the caller should add to the active layer. `OpenRegion` means
/// the flood spilled to a viewport edge (no enclosing barrier) and
/// the caller should toast the user.
pub enum FillResult {
    Filled(Shape),
    OpenRegion,
    /// Click landed outside the viewport pixmap, or on an ink pixel
    /// — nothing meaningful to fill. Silent no-op.
    Empty,
}

/// Run the flood. `viewport_logical` is the size of the canvas
/// viewport in logical px (`canvas_rect` width / height). `click`
/// is the pen-down position in canvas-local coords. `color` is the
/// fill RGBA.
pub fn flood_fill_active(
    canvas: &Canvas,
    click: [f32; 2],
    color: [u8; 4],
    viewport_logical: [f32; 2],
    pixels_per_point: f32,
) -> FillResult {
    let ppp = pixels_per_point.max(0.01);
    let w_px = (viewport_logical[0] * ppp).round().max(2.0) as u32;
    let h_px = (viewport_logical[1] * ppp).round().max(2.0) as u32;

    let mut pixmap = match tiny_skia::Pixmap::new(w_px, h_px) {
        Some(p) => p,
        None => return FillResult::Empty,
    };
    {
        let mut view = pixmap.as_mut();
        render::render_canvas(&mut view, canvas, None, None, ppp);
    }

    // Convert click (canvas-local) → pixmap pixel coords.
    let zoom = canvas.zoom;
    let cx = (click[0] * zoom + canvas.pan[0]) * ppp;
    let cy = (click[1] * zoom + canvas.pan[1]) * ppp;
    let sx = cx.round() as i32;
    let sy = cy.round() as i32;
    let (iw, ih) = (w_px as i32, h_px as i32);
    if sx < 0 || sy < 0 || sx >= iw || sy >= ih {
        return FillResult::Empty;
    }

    // Build the binary barrier mask. tiny-skia premultiplied RGBA;
    // alpha > 24 (~10 %) reads as "ink" for flood purposes.
    let raw = pixmap.data();
    let stride = w_px as usize * 4;
    let is_barrier = |px: i32, py: i32| -> bool {
        if px < 0 || py < 0 || px >= iw || py >= ih { return true; }
        let i = py as usize * stride + px as usize * 4;
        raw[i + 3] > 24
    };
    if is_barrier(sx, sy) {
        // Click sat on existing ink — nothing to fill.
        return FillResult::Empty;
    }

    let mut filled = vec![false; (w_px as usize) * (h_px as usize)];
    let idx = |x: i32, y: i32| (y as usize) * (w_px as usize) + (x as usize);
    let mut edge_touched = false;
    let mut min_x = sx;
    let mut max_x = sx;
    let mut min_y = sy;
    let mut max_y = sy;

    // Scanline flood. Stack holds seed pixels — left/right scan
    // happens inside the loop body.
    let mut stack: Vec<(i32, i32)> = vec![(sx, sy)];
    while let Some((x, y)) = stack.pop() {
        if y < 0 || y >= ih { continue; }
        if filled[idx(x, y)] || is_barrier(x, y) { continue; }
        // Walk left.
        let mut lx = x;
        while lx > 0 && !filled[idx(lx - 1, y)] && !is_barrier(lx - 1, y) {
            lx -= 1;
        }
        // Walk right.
        let mut rx = x;
        while rx + 1 < iw && !filled[idx(rx + 1, y)] && !is_barrier(rx + 1, y) {
            rx += 1;
        }
        if lx == 0 || rx == iw - 1 || y == 0 || y == ih - 1 {
            edge_touched = true;
        }
        for fx in lx..=rx {
            filled[idx(fx, y)] = true;
        }
        if lx < min_x { min_x = lx; }
        if rx > max_x { max_x = rx; }
        if y < min_y { min_y = y; }
        if y > max_y { max_y = y; }
        if y > 0 {
            for fx in lx..=rx {
                if !filled[idx(fx, y - 1)] && !is_barrier(fx, y - 1) {
                    stack.push((fx, y - 1));
                }
            }
        }
        if y + 1 < ih {
            for fx in lx..=rx {
                if !filled[idx(fx, y + 1)] && !is_barrier(fx, y + 1) {
                    stack.push((fx, y + 1));
                }
            }
        }
    }

    if edge_touched {
        return FillResult::OpenRegion;
    }
    // Dilate the fill by 2 px so it pushes into the surrounding
    // stroke's anti-aliased fringe — without this, the AA edge
    // pixels register as barriers and the fill stops ~2 px short
    // of the visible stroke centerline, leaving a hair-line gap.
    // Strokes are typically ≥4 px wide so the dilation stays under
    // the ink and never escapes the closed region.
    const DILATE: usize = 2;
    for _ in 0..DILATE {
        let prev = filled.clone();
        for y in 0..ih {
            for x in 0..iw {
                let i = idx(x, y);
                if prev[i] { continue; }
                let neighbour =
                    (y > 0          && prev[idx(x, y - 1)]) ||
                    (y + 1 < ih     && prev[idx(x, y + 1)]) ||
                    (x > 0          && prev[idx(x - 1, y)]) ||
                    (x + 1 < iw     && prev[idx(x + 1, y)]);
                if neighbour {
                    filled[i] = true;
                    if x < min_x { min_x = x; }
                    if x > max_x { max_x = x; }
                    if y < min_y { min_y = y; }
                    if y > max_y { max_y = y; }
                }
            }
        }
    }
    let bbox_w = (max_x - min_x + 1) as u32;
    let bbox_h = (max_y - min_y + 1) as u32;
    if bbox_w == 0 || bbox_h == 0 {
        return FillResult::Empty;
    }

    // Materialise the cropped fill region as an RgbaImage.
    let mut out = RgbaImage::from_pixel(bbox_w, bbox_h, image::Rgba([0, 0, 0, 0]));
    for y in 0..bbox_h {
        for x in 0..bbox_w {
            let sx = x as i32 + min_x;
            let sy = y as i32 + min_y;
            if filled[idx(sx, sy)] {
                out.put_pixel(x, y, image::Rgba(color));
            }
        }
    }

    // Encode PNG.
    let mut png_bytes: Vec<u8> = Vec::new();
    {
        let mut cursor = std::io::Cursor::new(&mut png_bytes);
        if out
            .write_to(&mut cursor, image::ImageFormat::Png)
            .is_err()
        {
            return FillResult::Empty;
        }
    }

    // Convert bbox back to canvas-local coords:
    //   canvas_local = pixmap_px / (zoom * ppp) - pan
    let scale_inv = 1.0 / (zoom * ppp);
    let pos = [
        min_x as f32 * scale_inv - canvas.pan[0] / zoom,
        min_y as f32 * scale_inv - canvas.pan[1] / zoom,
    ];
    let size = [bbox_w as f32 * scale_inv, bbox_h as f32 * scale_inv];

    FillResult::Filled(Shape::Raster {
        pos,
        size,
        png: png_bytes,
        px_size: [bbox_w, bbox_h],
        color,
    })
}
