//! Marks flattened into pixels, for copying and exporting: the same
//! arithmetic the screen does (the painter's multiply blend), so what leaves
//! whir is what was on screen.

use whir_core::marks::Mark;

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

/// Multiply each mark into opaque BGRA pixels, clipped to the image.
pub fn bake(d: &mut Decoded, marks: &[Mark]) {
    let (w, h) = (d.width, d.height);
    for m in marks {
        let [r, g, b] = m.ink.rgb();
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
}

/// One channel under ink, rounded as the GPU rounds it.
#[inline]
fn multiply(p: u8, ink: u8) -> u8 {
    ((p as u32 * ink as u32 + 127) / 255) as u8
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
    use whir_core::marks::Ink;

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
        Mark {
            x,
            y,
            w,
            h,
            ink: Ink::Yellow,
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

        let mut d = image(4, 4);
        d.pixels.fill(255);
        bake(&mut d, &[mark(1, 1, 2, 2)]);
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
        let mut d = image(4, 4);
        d.pixels.fill(255);
        // The second hangs off the right and bottom edges (and can't overflow).
        bake(&mut d, &[mark(0, 0, 3, 3), mark(2, 2, u32::MAX, 50)]);
        assert_eq!(at(&d, 0, 0)[1], g);
        assert_eq!(at(&d, 2, 2)[1], multiply(g, g));
        assert_eq!(at(&d, 3, 3)[1], g);
        // Entirely outside: nothing to do.
        bake(&mut d, &[mark(9, 9, 5, 5)]);
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
