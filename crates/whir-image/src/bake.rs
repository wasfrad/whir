//! Marks flattened into pixels, for copying and exporting: the same
//! arithmetic the screen does (the painter's multiply blend for highlights,
//! egui's own glyphs for text), so what leaves whir is what was on screen.

use egui::epaint::{Fonts, Galley, TextOptions};
use egui::{Color32, ColorImage, FontDefinitions, FontId, Vec2};
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
        FontId::proportional(size),
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
    fn text_is_set_inside_its_box_in_the_pen_with_its_halo() {
        let fonts = FontDefinitions::default();
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
        let fonts = FontDefinitions::default();
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
        let fonts = FontDefinitions::default();
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
