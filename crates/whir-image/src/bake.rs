//! Marks flattened into pixels, for copying and exporting: the same
//! arithmetic the screen does (the painter's multiply blend for highlights,
//! shared drawing paths for strokes, egui's own glyphs for text), so what
//! leaves whir is what was on screen.

use egui::epaint::{Fonts, Galley, TextOptions};
use egui::{Color32, ColorImage, FontDefinitions, Pos2, Vec2};
use whir_core::marks::{Kind, Mark, Text};

use crate::decode::Decoded;

/// Put premultiplied BGRA on white paper and make it opaque.
pub fn flatten_on_white(d: &mut Decoded) {
    if !d.has_alpha {
        return;
    }
    for p in d.pixels.chunks_exact_mut(4) {
        let paper = 255 - p[3];
        for c in &mut p[..3] {
            *c = c.saturating_add(paper);
        }
        p[3] = 255;
    }
    d.has_alpha = false;
}

/// Draw each mark into opaque BGRA pixels, bottom to top as the screen does,
/// clipped to the image. Text is set in `fonts` (the ones on screen); without
/// them it is left out.
pub fn bake(d: &mut Decoded, marks: &[Mark], fonts: Option<&FontDefinitions>) {
    let mut typesetter = None;
    for m in marks {
        match &m.kind {
            Kind::Highlight(ink) => highlight(d, m, ink.rgb()),
            Kind::Freehand(ink) | Kind::Arrow(ink) => {
                stroke(d, &m.drawing_paths(), ink.width, ink.pen.rgb());
            }
            Kind::Text(t) => {
                let Some(fonts) = fonts else { continue };
                let fonts = typesetter.get_or_insert_with(|| typesetter_for(fonts));
                let (w, h) = if t.turn % 2 == 1 {
                    (m.h, m.w)
                } else {
                    (m.w, m.h)
                };
                // Typed upright on an image turned `turn` quarters: on the
                // image itself, it runs that many quarters the other way.
                let tile = turn(typeset(t, w, h, fonts), (4 - t.turn % 4) % 4);
                composite(d, &tile, m.x, m.y);
            }
        }
    }
}

/// Multiply a highlight's ink into its box.
fn highlight(d: &mut Decoded, m: &Mark, [r, g, b]: [u8; 3]) {
    let (w, h) = (d.width, d.height);
    let (x0, x1) = (m.x.min(w) as usize, m.x.saturating_add(m.w).min(w) as usize);
    let (y0, y1) = (m.y.min(h) as usize, m.y.saturating_add(m.h).min(h) as usize);
    for y in y0..y1 {
        let row = &mut d.pixels[y * d.stride + x0 * 4..y * d.stride + x1 * 4];
        for p in row.chunks_exact_mut(4) {
            p[0] = multiply(p[0], b);
            p[1] = multiply(p[1], g);
            p[2] = multiply(p[2], r);
        }
    }
}

/// One channel under ink, rounded as the GPU rounds it.
#[inline]
fn multiply(p: u8, ink: u8) -> u8 {
    ((p as u32 * ink as u32 + 127) / 255) as u8
}

/// Round strokes with a one-pixel antialias fringe. Union coverage before
/// compositing: retracing a line or joining the arrowhead must not darken it.
fn stroke(d: &mut Decoded, paths: &[Vec<Pos2>], width: u32, [r, g, b]: [u8; 3]) {
    let reach = width.clamp(1, 256) as f64 * 0.5 + 0.5;
    let mut lo = [f64::INFINITY; 2];
    let mut hi = [f64::NEG_INFINITY; 2];
    for p in paths.iter().flatten() {
        for (axis, value) in [p.x as f64, p.y as f64].into_iter().enumerate() {
            lo[axis] = lo[axis].min(value);
            hi[axis] = hi[axis].max(value);
        }
    }
    let xs = pixel_span(lo[0] - reach, hi[0] + reach, 0..d.width as usize);
    let ys = pixel_span(lo[1] - reach, hi[1] + reach, 0..d.height as usize);
    if xs.is_empty() || ys.is_empty() {
        return;
    }
    let bounds = [xs.start, ys.start, xs.end, ys.end];
    let stride = xs.len();
    // Only the image-clipped bounds, never a tile sized by off-image points.
    let mut coverage = vec![0u8; stride * ys.len()];
    for path in paths {
        if let [p] = path.as_slice() {
            stroke_segment(&mut coverage, bounds, *p, *p, reach);
        }
        for pair in path.windows(2) {
            stroke_segment(&mut coverage, bounds, pair[0], pair[1], reach);
        }
    }
    for (row, y) in ys.enumerate() {
        for (col, x) in xs.clone().enumerate() {
            let a = coverage[row * stride + col];
            if a == 0 {
                continue;
            }
            let at = y * d.stride + x * 4;
            over(
                &mut d.pixels[at..at + 4],
                [multiply(b, a), multiply(g, a), multiply(r, a), a],
            );
        }
    }
}

fn pixel_span(lo: f64, hi: f64, clip: std::ops::Range<usize>) -> std::ops::Range<usize> {
    let start = clip.start as f64;
    let end = clip.end as f64;
    lo.floor().clamp(start, end) as usize..hi.ceil().clamp(start, end) as usize
}

/// A capsule, scanned in narrow row spans rather than its whole bounding box.
/// Each row considers only the centreline within `reach` of that row, expanded
/// horizontally by `reach`. This bounds work by visible length × stroke width,
/// even for long diagonals and segments with endpoints far outside the image.
fn stroke_segment(coverage: &mut [u8], bounds: [usize; 4], a: Pos2, b: Pos2, reach: f64) {
    // A fixed endpoint order also makes reversed paths rasterize identically.
    let (a, b) = if a.y < b.y || (a.y == b.y && a.x <= b.x) {
        (a, b)
    } else {
        (b, a)
    };
    let (ax, ay) = (a.x as f64, a.y as f64);
    let (bx, by) = (b.x as f64, b.y as f64);
    let (dx, dy) = (bx - ax, by - ay);
    let length_sq = dx * dx + dy * dy;
    let [x0, y0, x1, y1] = bounds;
    for y in pixel_span(ay - reach, by + reach, y0..y1) {
        let cy = y as f64 + 0.5;
        let (left, right) = if dy == 0.0 {
            (ax.min(bx), ax.max(bx))
        } else {
            let t0 = ((cy - reach - ay) / dy).clamp(0.0, 1.0);
            let t1 = ((cy + reach - ay) / dy).clamp(0.0, 1.0);
            let u = ax + t0 * dx;
            let v = ax + t1 * dx;
            (u.min(v), u.max(v))
        };
        let row = (y - y0) * (x1 - x0);
        for x in pixel_span(left - reach, right + reach, x0..x1) {
            let cell = &mut coverage[row + x - x0];
            if *cell == 255 {
                continue;
            }
            let (px, py) = (x as f64 + 0.5 - ax, cy - ay);
            let t = if length_sq == 0.0 {
                0.0
            } else {
                ((px * dx + py * dy) / length_sq).clamp(0.0, 1.0)
            };
            let (ox, oy) = (px - t * dx, py - t * dy);
            let distance = (ox * ox + oy * oy).sqrt();
            let a = ((reach - distance).clamp(0.0, 1.0) * 255.0).round() as u8;
            *cell = (*cell).max(a);
        }
    }
}

/// Fonts for laying text out at one point per image pixel. Text is at most
/// `Text::MAX_SIZE` pixels, so its glyphs always fit the atlas.
fn typesetter_for(definitions: &FontDefinitions) -> Fonts {
    let options = TextOptions {
        max_texture_side: 4096,
        // Whole-pixel glyph positions: each glyph is stamped once, unscaled.
        subpixel_binning: false,
        ..TextOptions::default()
    };
    Fonts::new(options, definitions.clone())
}

/// A text mark drawn upright on a transparent `w` × `h` tile (premultiplied
/// BGRA): the halo, then the letters, inset by the halo.
fn typeset(t: &Text, w: u32, h: u32, fonts: &mut Fonts) -> Decoded {
    let size = t.size.clamp(Text::MIN_SIZE, Text::MAX_SIZE) as f32;
    let galley = fonts.with_pixels_per_point(1.0).layout_no_wrap(
        t.body.clone(),
        whir_core::marks::font(size),
        Color32::WHITE,
    );
    let atlas = fonts.texture_atlas().image();
    let mut tile = Decoded {
        width: w,
        height: h,
        stride: w as usize * 4,
        pixels: vec![0; w as usize * h as usize * 4],
        full_size: [w, h],
        has_alpha: true,
    };
    let inset = Vec2::splat(t.halo());
    for offset in t.halo_offsets() {
        stamp(&mut tile, &galley, atlas, inset + offset, t.pen.halo());
    }
    stamp(&mut tile, &galley, atlas, inset, t.pen.color());
    tile
}

/// Each glyph of `galley`, from the font atlas, at `at` in `color`: placed
/// as epaint places it on screen (`tessellate_glyphs`).
fn stamp(tile: &mut Decoded, galley: &Galley, atlas: &ColorImage, at: Vec2, color: Color32) {
    let (tw, th) = (tile.width as i64, tile.height as i64);
    for placed in &galley.rows {
        for g in &placed.row.glyphs {
            let uv = g.uv_rect;
            if uv.is_nothing() {
                continue;
            }
            let left = (g.pos.x + uv.offset.x).round() + placed.pos.x + at.x;
            let top = (g.pos.y + uv.offset.y).round() + placed.pos.y + at.y;
            let (left, top) = (left.round() as i64, top.round() as i64);
            for v in uv.min[1]..uv.max[1] {
                let y = top + (v - uv.min[1]) as i64;
                if !(0..th).contains(&y) {
                    continue;
                }
                for u in uv.min[0]..uv.max[0] {
                    let x = left + (u - uv.min[0]) as i64;
                    if !(0..tw).contains(&x) {
                        continue;
                    }
                    let texel = atlas.pixels[v as usize * atlas.size[0] + u as usize];
                    if texel.a() == 0 {
                        continue;
                    }
                    // The vertex colour times the texel, as the GPU has it.
                    let src = color * texel;
                    let i = y as usize * tile.stride + x as usize * 4;
                    over(
                        &mut tile.pixels[i..i + 4],
                        [src.b(), src.g(), src.r(), src.a()],
                    );
                }
            }
        }
    }
}

/// Premultiplied BGRA `src` over `dst`.
#[inline]
fn over(dst: &mut [u8], src: [u8; 4]) {
    let keep = 255 - src[3] as u32;
    for (d, s) in dst.iter_mut().zip(src) {
        *d = (s as u32 + (*d as u32 * keep + 127) / 255).min(255) as u8;
    }
}

/// `tile` over `d` with its top left at (`x0`, `y0`), clipped to `d`.
fn composite(d: &mut Decoded, tile: &Decoded, x0: u32, y0: u32) {
    for ty in 0..tile.height {
        let y = y0 as u64 + ty as u64;
        if y >= d.height as u64 {
            break;
        }
        for tx in 0..tile.width {
            let x = x0 as u64 + tx as u64;
            if x >= d.width as u64 {
                break;
            }
            let s = ty as usize * tile.stride + tx as usize * 4;
            let src: [u8; 4] = tile.pixels[s..s + 4].try_into().expect("four bytes");
            if src[3] == 0 {
                continue;
            }
            let at = y as usize * d.stride + x as usize * 4;
            over(&mut d.pixels[at..at + 4], src);
        }
    }
}

/// `d` turned `turns` quarters clockwise (as rotated on screen).
pub fn turn(d: Decoded, turns: u8) -> Decoded {
    let turns = turns % 4;
    if turns == 0 {
        return d;
    }
    let (w, h) = (d.width as usize, d.height as usize);
    let (tw, th) = if turns == 2 { (w, h) } else { (h, w) };
    let mut pixels = vec![0u8; tw * th * 4];
    for ty in 0..th {
        for tx in 0..tw {
            // The source pixel that lands here (see `view::turn_point`).
            let (sx, sy) = match turns {
                1 => (ty, h - 1 - tx),
                2 => (w - 1 - tx, h - 1 - ty),
                _ => (w - 1 - ty, tx),
            };
            let from = sy * d.stride + sx * 4;
            let to = (ty * tw + tx) * 4;
            pixels[to..to + 4].copy_from_slice(&d.pixels[from..from + 4]);
        }
    }
    Decoded {
        width: tw as u32,
        height: th as u32,
        stride: tw * 4,
        pixels,
        full_size: [tw as u32, th as u32],
        has_alpha: d.has_alpha,
    }
}

/// Opaque pixels as a clipboard `CF_DIB`: a BITMAPINFOHEADER, then 32-bit
/// rows from the bottom up.
pub fn pack_dib(d: &Decoded) -> Vec<u8> {
    let (w, h) = (d.width as usize, d.height as usize);
    let mut out = Vec::with_capacity(40 + w * h * 4);
    out.extend_from_slice(&40u32.to_le_bytes()); // biSize
    out.extend_from_slice(&(d.width as i32).to_le_bytes());
    out.extend_from_slice(&(d.height as i32).to_le_bytes()); // positive: bottom-up
    out.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    out.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
    out.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    out.extend_from_slice(&((w * h * 4) as u32).to_le_bytes());
    out.extend_from_slice(&[0u8; 16]); // resolution, palette
    for y in (0..h).rev() {
        out.extend_from_slice(&d.pixels[y * d.stride..y * d.stride + w * 4]);
    }
    out
}

#[cfg(test)]
mod tests {
    use whir_core::marks::{Ink, Pen};

    use super::*;

    /// A `w` × `h` image whose pixel (x, y) is BGRA (x, y, 200, 255).
    fn image(w: u32, h: u32) -> Decoded {
        let mut pixels = Vec::new();
        for y in 0..h {
            for x in 0..w {
                pixels.extend_from_slice(&[x as u8, y as u8, 200, 255]);
            }
        }
        Decoded {
            width: w,
            height: h,
            stride: w as usize * 4,
            pixels,
            full_size: [w, h],
            has_alpha: false,
        }
    }

    fn white(w: u32, h: u32) -> Decoded {
        let mut d = image(w, h);
        d.pixels.fill(255);
        d
    }

    const GREY: [u8; 4] = [128, 128, 128, 255];

    /// Grey paper, so a white halo shows on it.
    fn grey(w: u32, h: u32) -> Decoded {
        let mut d = image(w, h);
        d.pixels
            .chunks_exact_mut(4)
            .for_each(|p| p.copy_from_slice(&GREY));
        d
    }

    fn at(d: &Decoded, x: usize, y: usize) -> [u8; 4] {
        let i = y * d.stride + x * 4;
        [
            d.pixels[i],
            d.pixels[i + 1],
            d.pixels[i + 2],
            d.pixels[i + 3],
        ]
    }

    fn mark(x: u32, y: u32, w: u32, h: u32) -> Mark {
        Mark::highlight(x, y, w, h, Ink::Yellow)
    }

    fn text(x: u32, y: u32, w: u32, h: u32, body: &str, turn: u8) -> Mark {
        Mark {
            x,
            y,
            w,
            h,
            kind: Kind::Text(Text {
                body: body.to_owned(),
                size: 40,
                pen: Pen::Black,
                turn,
            }),
        }
    }

    fn drawing(points: &[[f32; 2]], pen: Pen, width: u32, arrow: bool) -> Mark {
        let points: Vec<_> = points.iter().map(|&[x, y]| egui::pos2(x, y)).collect();
        Mark::drawing(&points, pen, width, arrow).expect("a drawing")
    }

    fn pen_bgra(pen: Pen) -> [u8; 4] {
        let [r, g, b] = pen.rgb();
        [b, g, r, 255]
    }

    fn padded_image(w: u32, h: u32) -> Decoded {
        let mut d = image(w, h);
        let stride = d.stride + 12;
        let mut pixels = vec![0xa5; stride * h as usize];
        for y in 0..h as usize {
            pixels[y * stride..y * stride + d.stride]
                .copy_from_slice(&d.pixels[y * d.stride..(y + 1) * d.stride]);
        }
        d.stride = stride;
        d.pixels = pixels;
        d
    }

    /// Deliberately slow oracle: every pixel against every segment, independent
    /// of the production rasterizer's bounding boxes and scanline spans.
    fn reference_stroke(d: &mut Decoded, paths: &[Vec<Pos2>], width: u32, pen: Pen) {
        let reach = width.clamp(1, 256) as f64 * 0.5 + 0.5;
        let [r, g, b] = pen.rgb();
        for y in 0..d.height as usize {
            for x in 0..d.width as usize {
                let mut distance = f64::INFINITY;
                for path in paths {
                    for (i, a) in path.iter().enumerate() {
                        // The last point also covers a one-point path.
                        let b = path.get(i + 1).unwrap_or(a);
                        let vx = b.x as f64 - a.x as f64;
                        let vy = b.y as f64 - a.y as f64;
                        let px = x as f64 + 0.5 - a.x as f64;
                        let py = y as f64 + 0.5 - a.y as f64;
                        let length_sq = vx * vx + vy * vy;
                        let t = if length_sq == 0.0 {
                            0.0
                        } else {
                            ((px * vx + py * vy) / length_sq).clamp(0.0, 1.0)
                        };
                        let dx = px - t * vx;
                        let dy = py - t * vy;
                        distance = distance.min((dx * dx + dy * dy).sqrt());
                    }
                }
                let a = ((reach - distance).clamp(0.0, 1.0) * 255.0).round() as u8;
                let i = y * d.stride + x * 4;
                over(
                    &mut d.pixels[i..i + 4],
                    [multiply(b, a), multiply(g, a), multiply(r, a), a],
                );
            }
        }
    }

    /// Whether any pixel is in the pen's colour, and any in the halo's.
    fn inked(d: &Decoded) -> (bool, bool) {
        let [r, g, b] = Pen::Black.rgb();
        let mut pixels = d.pixels.chunks_exact(4);
        let pen = pixels.clone().any(|p| p == [b, g, r, 255]);
        (pen, pixels.any(|p| p == [255; 4]))
    }

    /// Every pixel of `d` outside `x` × `y` is grey.
    fn grey_outside(d: &Decoded, x: std::ops::Range<usize>, y: std::ops::Range<usize>) {
        for py in 0..d.height as usize {
            for px in 0..d.width as usize {
                if !x.contains(&px) || !y.contains(&py) {
                    assert_eq!(at(d, px, py), GREY, "outside the box at ({px}, {py})");
                }
            }
        }
    }

    #[test]
    fn white_takes_the_ink_and_black_stays_black() {
        let [r, g, b] = Ink::Yellow.rgb();
        assert_eq!(multiply(255, g), g);
        assert_eq!(multiply(0, g), 0);
        // Rounded to nearest, not down.
        assert_eq!(multiply(100, 240), 94);
        assert_eq!(multiply(50, 80), 16);

        let mut d = white(4, 4);
        bake(&mut d, &[mark(1, 1, 2, 2)], None);
        assert_eq!(at(&d, 1, 1), [b, g, r, 255]);
        assert_eq!(at(&d, 2, 2), [b, g, r, 255]);
        // Outside the mark, nothing changed.
        assert_eq!(at(&d, 0, 1), [255; 4]);
        assert_eq!(at(&d, 3, 2), [255; 4]);
        assert_eq!(at(&d, 1, 3), [255; 4]);
    }

    #[test]
    fn overlapping_marks_multiply_twice_and_marks_are_clipped() {
        let [_, g, _] = Ink::Yellow.rgb();
        let mut d = white(4, 4);
        // The second hangs off the right and bottom edges (and can't overflow).
        bake(&mut d, &[mark(0, 0, 3, 3), mark(2, 2, u32::MAX, 50)], None);
        assert_eq!(at(&d, 0, 0)[1], g);
        assert_eq!(at(&d, 2, 2)[1], multiply(g, g));
        assert_eq!(at(&d, 3, 3)[1], g);
        // Entirely outside: nothing to do.
        bake(&mut d, &[mark(9, 9, 5, 5)], None);
    }

    #[test]
    fn pen_is_opaque_bgra_with_antialiased_round_caps() {
        for pen in Pen::ALL {
            let mut d = image(40, 24);
            let before = image(40, 24);
            let m = drawing(&[[8.0, 12.0], [24.0, 12.0]], pen, 6, false);
            bake(&mut d, &[m], None);
            let ink = pen_bgra(pen);
            assert_eq!(at(&d, 16, 12), ink, "solid ink, not multiply");
            assert_eq!(at(&d, 6, 11), ink, "round cap before the start");
            assert_eq!(at(&d, 25, 11), ink, "round cap after the end");
            let fringe = at(&d, 5, 10);
            assert_ne!(fringe, at(&before, 5, 10), "partial edge coverage");
            assert_ne!(fringe, ink, "antialiased, not a hard edge");
            assert_eq!(at(&d, 5, 9), at(&before, 5, 9), "not a square cap");
            assert_eq!(at(&d, 16, 8), at(&before, 16, 8), "no halo");
            assert!(d.pixels.chunks_exact(4).all(|p| p[3] == 255));
            assert!(!d.has_alpha);
        }
    }

    #[test]
    fn one_point_and_repeated_points_make_a_round_dot() {
        let mut dot = white(20, 20);
        bake(
            &mut dot,
            &[drawing(&[[10.0, 10.0]], Pen::Black, 8, false)],
            None,
        );
        assert_eq!(at(&dot, 10, 10), pen_bgra(Pen::Black));
        assert_eq!(at(&dot, 6, 6), [255; 4], "outside the circle");
        assert_ne!(at(&dot, 6, 8), [255; 4]);
        assert_ne!(at(&dot, 6, 8), pen_bgra(Pen::Black));
        for y in 0..20 {
            for x in 0..20 {
                assert_eq!(at(&dot, x, y), at(&dot, 19 - x, y));
                assert_eq!(at(&dot, x, y), at(&dot, x, 19 - y));
            }
        }
        let mut repeated = white(20, 20);
        bake(
            &mut repeated,
            &[drawing(&[[10.0, 10.0]; 3], Pen::Black, 8, false)],
            None,
        );
        assert_eq!(repeated.pixels, dot.pixels);
    }

    #[test]
    fn pen_width_is_in_image_pixels_and_invalid_styles_are_not_drawn() {
        for width in [0, 1, 2, 5, 12, 256, u32::MAX] {
            let mut m = drawing(&[[8.0, 140.0], [24.0, 140.0]], Pen::Black, 1, false);
            let Kind::Freehand(style) = &mut m.kind else {
                unreachable!()
            };
            style.width = width;
            let mut d = white(32, 280);
            bake(&mut d, &[m], None);
            if !(1..=256).contains(&width) {
                assert!(
                    d.pixels.iter().all(|&p| p == 255),
                    "invalid drawing is ignored"
                );
                continue;
            }
            let rows = (0..280).filter(|&y| at(&d, 16, y) != [255; 4]).count();
            // Odd widths straddle two half-covered boundary rows.
            assert_eq!(rows, ((width + 1) & !1) as usize);
            let ink: u32 = (0..280).map(|y| 255 - at(&d, 16, y)[0] as u32).sum();
            let expected = width * (255 - Pen::Black.rgb()[2] as u32);
            assert!(
                ink.abs_diff(expected) <= 1,
                "width {width}: {ink} vs {expected}"
            );
        }
    }

    #[test]
    fn joins_and_retracing_union_coverage_but_separate_marks_blend_again() {
        let mut horizontal = white(32, 32);
        let a = drawing(&[[8.0, 10.0], [20.0, 10.0]], Pen::Black, 7, false);
        bake(&mut horizontal, std::slice::from_ref(&a), None);
        let mut vertical = white(32, 32);
        let b = drawing(&[[20.0, 10.0], [20.0, 24.0]], Pen::Black, 7, false);
        bake(&mut vertical, &[b], None);
        let mut joined = white(32, 32);
        let m = drawing(
            &[
                [8.0, 10.0],
                [20.0, 10.0],
                [20.0, 24.0],
                [20.0, 10.0],
                [8.0, 10.0],
            ],
            Pen::Black,
            7,
            false,
        );
        bake(&mut joined, &[m], None);
        for ((&a, &b), &c) in horizontal
            .pixels
            .iter()
            .zip(&vertical.pixels)
            .zip(&joined.pixels)
        {
            assert_eq!(c, a.min(b), "max coverage, not repeated source-over");
        }
        assert_eq!(
            at(&joined, 23, 6),
            [255; 4],
            "no miter outside the round join"
        );
        assert_ne!(at(&joined, 22, 7), [255; 4], "round join fringe");
        assert_ne!(at(&joined, 22, 7), pen_bgra(Pen::Black));
        let edge = at(&horizontal, 14, 6);
        bake(&mut horizontal, &[a], None);
        assert!(
            at(&horizontal, 14, 6)[0] < edge[0],
            "a second mark blends separately"
        );
    }

    #[test]
    fn clipped_segment_spans_match_all_pixel_reference_and_preserve_row_padding() {
        use egui::pos2 as p;
        let cases = [
            vec![],
            vec![vec![]],
            vec![vec![p(0.0, 0.0)]],
            vec![vec![p(-2.0, 6.0)], vec![p(19.0, 6.0)]],
            vec![vec![p(-1.0e9, 6.0), p(1.0e9, 6.0)]],
            vec![vec![p(8.0, -1.0e9), p(8.0, 1.0e9)]],
            vec![vec![p(-1.0e9, -1.0e9), p(1.0e9, 1.0e9)]],
            vec![vec![p(1.0, -50.0), p(12.0, 50.0)]],
            vec![vec![p(-50.0, 2.0), p(70.0, 11.0)]],
            vec![vec![
                p(-4.0, 14.0),
                p(20.0, -3.0),
                p(20.0, -3.0),
                p(3.0, 6.0),
            ]],
            vec![vec![p(4.0e9, 4.0e9), p(4.1e9, 4.1e9)]],
        ];
        for (case, paths) in cases.iter().enumerate() {
            for width in [1, 6, 256] {
                let mut actual = padded_image(17, 13);
                stroke(&mut actual, paths, width, Pen::Red.rgb());
                let mut expected = padded_image(17, 13);
                reference_stroke(&mut expected, paths, width, Pen::Red);
                assert_eq!(actual.pixels, expected.pixels, "case {case}, width {width}");
                let mut reversed = paths.clone();
                for path in &mut reversed {
                    path.reverse();
                }
                let mut backwards = padded_image(17, 13);
                stroke(&mut backwards, &reversed, width, Pen::Red.rgb());
                assert_eq!(
                    actual.pixels, backwards.pixels,
                    "reversed case {case}, width {width}"
                );
            }
        }
    }

    #[test]
    fn drawing_vertices_can_be_outside_the_image_and_empty_images_are_safe() {
        let mut d = white(16, 12);
        bake(
            &mut d,
            &[drawing(&[[1.0e9, 6.0], [0.0, 6.0]], Pen::Blue, 4, false)],
            None,
        );
        for x in 0..16 {
            assert_eq!(at(&d, x, 5), pen_bgra(Pen::Blue));
            assert_eq!(at(&d, x, 0), [255; 4]);
        }
        let offscreen = drawing(&[[4.0e9, 4.0e9]], Pen::Red, 256, false);
        let before = d.pixels.clone();
        bake(&mut d, &[offscreen], None);
        assert_eq!(d.pixels, before);
        let dot = drawing(&[[18.0, 6.0]], Pen::Red, 8, false);
        bake(&mut d, std::slice::from_ref(&dot), None);
        assert_eq!(
            at(&d, 15, 6),
            pen_bgra(Pen::Red),
            "offscreen centre, visible cap"
        );
        for (w, h) in [(0, 0), (0, 12), (16, 0)] {
            let mut empty = white(w, h);
            bake(&mut empty, std::slice::from_ref(&dot), None);
            assert!(empty.pixels.is_empty());
        }
    }

    #[test]
    fn arrows_use_shared_open_heads_at_the_dragged_end_in_every_direction() {
        let pairs = [
            [[28.0, 64.0], [100.0, 64.0]],
            [[100.0, 64.0], [28.0, 64.0]],
            [[64.0, 28.0], [64.0, 100.0]],
            [[64.0, 100.0], [64.0, 28.0]],
            [[28.0, 28.0], [100.0, 100.0]],
            [[100.0, 100.0], [28.0, 28.0]],
            [[100.0, 28.0], [28.0, 100.0]],
            [[28.0, 100.0], [100.0, 28.0]],
        ];
        for points in pairs {
            let m = drawing(&points, Pen::Blue, 4, true);
            let paths = m.drawing_paths();
            let tail = egui::pos2(points[0][0], points[0][1]);
            let tip = egui::pos2(points[1][0], points[1][1]);
            assert_eq!(paths[0], [tail, tip]);
            assert_eq!(paths.len(), 2, "shaft and open V, not a closed triangle");
            assert_eq!(paths[1].len(), 3);
            assert_eq!(paths[1][1], tip);
            let mut actual = white(128, 128);
            bake(&mut actual, &[m], None);
            let mut expected = white(128, 128);
            reference_stroke(&mut expected, &paths, 4, Pen::Blue);
            assert_eq!(actual.pixels, expected.pixels, "arrow {points:?}");
            for wing in [paths[1][0], paths[1][2]] {
                assert!(
                    wing.distance(tip) < wing.distance(tail),
                    "head is by the tip"
                );
                assert!((tip - wing).dot(tip - tail) > 0.0, "wings trail the tip");
                let on_head = wing.lerp(tip, 0.5);
                assert_eq!(
                    at(&actual, on_head.x as usize, on_head.y as usize),
                    pen_bgra(Pen::Blue)
                );
                let at_tail = on_head - (tip - tail);
                assert_eq!(
                    at(&actual, at_tail.x as usize, at_tail.y as usize),
                    [255; 4],
                    "no head at the tail"
                );
            }
        }
    }

    #[test]
    fn arrow_heads_crossing_each_image_edge_are_clipped_not_moved() {
        let pairs = [
            [[28.0, 0.0], [2.0, 0.0]],
            [[0.0, 28.0], [0.0, 2.0]],
            [[32.0, 2.0], [32.0, 28.0]],
            [[2.0, 32.0], [28.0, 32.0]],
        ];
        for points in pairs {
            let m = drawing(&points, Pen::Red, 6, true);
            let paths = m.drawing_paths();
            assert!(
                paths
                    .iter()
                    .flatten()
                    .any(|p| p.x < 0.0 || p.y < 0.0 || p.x > 32.0 || p.y > 32.0)
            );
            let mut actual = padded_image(32, 32);
            bake(&mut actual, &[m], None);
            let mut expected = padded_image(32, 32);
            reference_stroke(&mut expected, &paths, 6, Pen::Red);
            assert_eq!(actual.pixels, expected.pixels, "edge arrow {points:?}");
        }
    }

    #[test]
    fn drawings_stack_with_highlights_and_each_other() {
        let line = drawing(&[[4.0, 8.0], [28.0, 8.0]], Pen::Blue, 6, false);
        let highlight = mark(0, 0, 32, 32);
        let mut d = grey(32, 32);
        bake(&mut d, &[highlight.clone(), line.clone()], None);
        assert_eq!(at(&d, 16, 8), pen_bgra(Pen::Blue));
        let mut d = grey(32, 32);
        bake(&mut d, &[line.clone(), highlight], None);
        let [r, g, b] = Ink::Yellow.rgb();
        let [pb, pg, pr, _] = pen_bgra(Pen::Blue);
        assert_eq!(
            at(&d, 16, 8),
            [multiply(pb, b), multiply(pg, g), multiply(pr, r), 255]
        );
        let arrow = drawing(&[[16.0, 2.0], [16.0, 28.0]], Pen::Red, 6, true);
        let mut d = grey(32, 32);
        bake(&mut d, &[line.clone(), arrow.clone()], None);
        assert_eq!(at(&d, 16, 8), pen_bgra(Pen::Red));
        let mut d = grey(32, 32);
        bake(&mut d, &[arrow, line], None);
        assert_eq!(at(&d, 16, 8), pen_bgra(Pen::Blue));
    }

    #[test]
    fn baking_then_turning_matches_turning_drawing_geometry() {
        let drawings = [
            (
                vec![[8.0, 9.0], [42.0, 17.0], [64.0, 38.0]],
                Pen::Blue,
                5,
                false,
            ),
            (vec![[12.0, 45.0], [60.0, 45.0]], Pen::Red, 4, true),
            (vec![[68.0, 12.0]], Pen::Green, 7, false),
        ];
        for quarters in 1..=3 {
            let marks: Vec<_> = drawings
                .iter()
                .map(|(points, pen, width, arrow)| drawing(points, *pen, *width, *arrow))
                .collect();
            let mut upright = white(80, 60);
            bake(&mut upright, &marks, None);
            let expected = turn(upright, quarters);
            let rotated: Vec<_> = drawings
                .iter()
                .map(|(points, pen, width, arrow)| {
                    let points: Vec<_> = points
                        .iter()
                        .map(|&[x, y]| match quarters {
                            1 => [60.0 - y, x],
                            2 => [80.0 - x, 60.0 - y],
                            _ => [y, 80.0 - x],
                        })
                        .collect();
                    drawing(&points, *pen, *width, *arrow)
                })
                .collect();
            let mut actual = white(expected.width, expected.height);
            bake(&mut actual, &rotated, None);
            for (i, (&a, &b)) in actual.pixels.iter().zip(&expected.pixels).enumerate() {
                // Shared arrowhead coordinates are f32; rotation can change
                // the last bit of antialias coverage, not the geometry.
                assert!(a.abs_diff(b) <= 1, "turn {quarters}, byte {i}: {a} vs {b}");
            }
        }
    }

    #[test]
    fn text_is_set_inside_its_box_in_the_pen_with_its_halo() {
        let fonts = whir_core::theme::test_fonts();
        let mut d = grey(200, 100);
        bake(&mut d, &[text(20, 10, 120, 60, "Hi", 0)], Some(&fonts));
        assert_eq!(inked(&d), (true, true), "letters and halo");
        grey_outside(&d, 20..140, 10..70);
        // Without fonts, there is nothing to set it in.
        let mut d = grey(200, 100);
        bake(&mut d, &[text(20, 10, 120, 60, "Hi", 0)], None);
        grey_outside(&d, 0..0, 0..0);
    }

    #[test]
    fn text_typed_on_a_turned_image_runs_the_other_way_on_the_image() {
        let fonts = whir_core::theme::test_fonts();
        // Upright: wider than tall.
        let mut upright = grey(200, 200);
        bake(
            &mut upright,
            &[text(10, 10, 150, 60, "Hello", 0)],
            Some(&fonts),
        );
        // Typed with the image turned a quarter: its box is tall, and the
        // letters are the upright ones turned back a quarter.
        let mut turned = grey(200, 200);
        bake(
            &mut turned,
            &[text(10, 10, 60, 150, "Hello", 1)],
            Some(&fonts),
        );
        grey_outside(&turned, 10..70, 10..160);
        let back = turn(turned, 1);
        // Turned clockwise, the box (10..70, 10..160) lands at (40..190, 10..70).
        for y in 0..60 {
            for x in 0..150 {
                assert_eq!(
                    at(&back, 40 + x, 10 + y),
                    at(&upright, 10 + x, 10 + y),
                    "at ({x}, {y}) of the box"
                );
            }
        }
    }

    #[test]
    fn text_off_the_edge_is_clipped() {
        let fonts = whir_core::theme::test_fonts();
        let mut d = grey(50, 30);
        bake(&mut d, &[text(30, 0, 150, 60, "Hello", 0)], Some(&fonts));
        assert!(inked(&d).0);
        grey_outside(&d, 30..50, 0..30);
    }

    #[test]
    fn transparency_becomes_white_paper() {
        let mut d = image(2, 1);
        // Half-transparent red (premultiplied), then fully transparent.
        d.pixels = vec![0, 0, 128, 128, 0, 0, 0, 0];
        d.has_alpha = true;
        flatten_on_white(&mut d);
        assert_eq!(at(&d, 0, 0), [127, 127, 255, 255]);
        assert_eq!(at(&d, 1, 0), [255, 255, 255, 255]);
        assert!(!d.has_alpha);
    }

    #[test]
    fn turning_moves_the_corners_clockwise() {
        // 3 wide, 2 tall; the blue channel is x, the green is y.
        let top_left = [0, 0, 200, 255];
        let top_right = [2, 0, 200, 255];
        let bottom_left = [0, 1, 200, 255];

        let t = turn(image(3, 2), 1);
        assert_eq!((t.width, t.height), (2, 3));
        assert_eq!(at(&t, 1, 0), top_left);
        assert_eq!(at(&t, 1, 2), top_right);
        assert_eq!(at(&t, 0, 0), bottom_left);

        let t = turn(image(3, 2), 2);
        assert_eq!((t.width, t.height), (3, 2));
        assert_eq!(at(&t, 2, 1), top_left);
        assert_eq!(at(&t, 0, 1), top_right);

        let t = turn(image(3, 2), 3);
        assert_eq!((t.width, t.height), (2, 3));
        assert_eq!(at(&t, 0, 2), top_left);
        assert_eq!(at(&t, 0, 0), top_right);
        assert_eq!(at(&t, 1, 2), bottom_left);

        // Four quarter turns are none.
        let mut d = image(3, 2);
        for _ in 0..4 {
            d = turn(d, 1);
        }
        assert_eq!(d.pixels, image(3, 2).pixels);
        assert_eq!(turn(image(3, 2), 4).pixels, image(3, 2).pixels);
    }

    #[test]
    fn a_dib_has_its_header_and_its_rows_bottom_up() {
        let dib = pack_dib(&image(3, 2));
        assert_eq!(dib.len(), 40 + 3 * 2 * 4);
        let u32_at = |i: usize| u32::from_le_bytes(dib[i..i + 4].try_into().unwrap());
        assert_eq!(u32_at(0), 40);
        assert_eq!(u32_at(4), 3);
        assert_eq!(u32_at(8), 2);
        assert_eq!(&dib[12..16], [1, 0, 32, 0]);
        assert_eq!(u32_at(16), 0);
        assert_eq!(u32_at(20), 24);
        // The first row stored is the image's last.
        assert_eq!(&dib[40..44], [0, 1, 200, 255]);
        assert_eq!(&dib[40 + 12..40 + 16], [0, 0, 200, 255]);
    }
}
