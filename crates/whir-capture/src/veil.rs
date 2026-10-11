//! The dimming over a frozen screen, done in the frame's own pixels so that
//! selecting never needs a second copy of the screen.
//!
//! Pixels are `0x00RRGGBB`. Dimming halves each channel and keeps the three
//! bits that drops in the spare top byte, so it can be undone exactly.

use crate::geom::{Rect, subtract};

const CHANNELS: u32 = 0x007f_7f7f;

#[inline]
pub fn dim(p: u32) -> u32 {
    let lost = ((p >> 16) & 1) << 2 | ((p >> 8) & 1) << 1 | (p & 1);
    lost << 24 | (p >> 1) & CHANNELS
}

/// The pixel [`dim`] was given (its colour; the spare byte comes back zero).
#[inline]
pub fn undim(p: u32) -> u32 {
    let lost = p >> 24;
    (p & CHANNELS) << 1 | ((lost >> 2) & 1) << 16 | ((lost >> 1) & 1) << 8 | (lost & 1)
}

pub fn dim_all(px: &mut [u32]) {
    for p in px {
        *p = dim(*p);
    }
}

fn each(px: &mut [u32], stride: usize, r: Rect, f: impl Fn(u32) -> u32) {
    for y in r.top..r.bottom {
        let row = y as usize * stride;
        for p in &mut px[row + r.left as usize..row + r.right as usize] {
            *p = f(*p);
        }
    }
}

/// Move the clear (undimmed) rectangle from `old` to `new`, touching only the
/// pixels that change. Both must lie inside the frame.
pub fn move_clear(px: &mut [u32], stride: usize, old: Option<Rect>, new: Option<Rect>) {
    let (old, new) = (old.unwrap_or_default(), new.unwrap_or_default());
    let (leaving, n) = subtract(old, new);
    for &r in &leaving[..n] {
        each(px, stride, r, dim);
    }
    let (entering, n) = subtract(new, old);
    for &r in &entering[..n] {
        each(px, stride, r, undim);
    }
}

/// The pixels under something drawn over the frame, to put back later.
pub struct Saved {
    rect: Rect,
    pixels: Vec<u32>,
}

impl Saved {
    /// Remember what is in `rect` (the part of it inside `bounds`).
    pub fn take(px: &[u32], stride: usize, bounds: Rect, rect: Rect) -> Saved {
        let rect = rect.intersect(bounds);
        let mut pixels = Vec::with_capacity((rect.width() * rect.height()) as usize);
        for y in rect.top..rect.bottom {
            let row = y as usize * stride;
            pixels.extend_from_slice(&px[row + rect.left as usize..row + rect.right as usize]);
        }
        Saved { rect, pixels }
    }

    pub fn rect(&self) -> Rect {
        self.rect
    }

    pub fn restore(self, px: &mut [u32], stride: usize) {
        let w = self.rect.width() as usize;
        if w == 0 {
            return;
        }
        for (y, saved) in (self.rect.top..self.rect.bottom).zip(self.pixels.chunks_exact(w)) {
            let at = y as usize * stride + self.rect.left as usize;
            px[at..at + w].copy_from_slice(saved);
        }
    }
}

/// A frame drawn just inside a rectangle's edge.
pub struct Outline {
    strips: Vec<Saved>,
}

impl Outline {
    pub fn draw(
        px: &mut [u32],
        stride: usize,
        bounds: Rect,
        around: Rect,
        thickness: i32,
        color: u32,
    ) -> Outline {
        let (r, t) = (around, thickness);
        let strips = [
            Rect::new(r.left, r.top, r.right, r.top + t),
            Rect::new(r.left, r.bottom - t, r.right, r.bottom),
            Rect::new(r.left, r.top + t, r.left + t, r.bottom - t),
            Rect::new(r.right - t, r.top + t, r.right, r.bottom - t),
        ];
        let strips = strips
            .into_iter()
            .map(|s| {
                // A rectangle thinner than two outlines: don't draw past it.
                let saved = Saved::take(px, stride, bounds, s.intersect(around));
                each(px, stride, saved.rect, |_| color);
                saved
            })
            .collect();
        Outline { strips }
    }

    pub fn restore(self, px: &mut [u32], stride: usize) {
        // Last drawn, first restored: strips can overlap on a tiny rectangle.
        for s in self.strips.into_iter().rev() {
            s.restore(px, stride);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dimming_is_undone_exactly_for_every_colour() {
        for p in 0..=0x00ff_ffffu32 {
            let d = dim(p);
            assert_eq!(undim(d), p, "{p:06x}");
        }
        // Half as bright, and whatever was in the spare byte doesn't matter.
        assert_eq!(dim(0x00ff_8001) & 0x00ff_ffff, 0x007f_4000);
        assert_eq!(undim(dim(0xab12_3456)), 0x0012_3456);
    }

    const W: usize = 16;
    const H: usize = 12;

    fn frame() -> Vec<u32> {
        // Every pixel distinct, with odd and even channels.
        (0..W * H)
            .map(|i| ((i * 7919) as u32) & 0x00ff_ffff)
            .collect()
    }

    /// What the frame should look like with only `clear` undimmed.
    fn expected(original: &[u32], clear: Option<Rect>) -> Vec<u32> {
        original
            .iter()
            .enumerate()
            .map(|(i, &p)| {
                let at = ((i % W) as i32, (i / W) as i32);
                if clear.is_some_and(|r| r.contains(at)) {
                    p
                } else {
                    dim(p)
                }
            })
            .collect()
    }

    #[test]
    fn moving_the_clear_rectangle_matches_dimming_from_scratch() {
        let original = frame();
        let mut px = original.clone();
        dim_all(&mut px);
        let steps = [
            Some(Rect::new(2, 2, 8, 6)),
            Some(Rect::new(4, 3, 12, 9)),  // overlapping
            Some(Rect::new(0, 0, 16, 12)), // everything
            Some(Rect::new(5, 5, 6, 6)),   // inside the last
            None,
            Some(Rect::new(10, 1, 15, 4)),
            Some(Rect::new(0, 8, 3, 12)), // disjoint
            None,
        ];
        let mut clear = None;
        for next in steps {
            move_clear(&mut px, W, clear, next);
            clear = next;
            assert_eq!(px, expected(&original, clear), "after moving to {next:?}");
        }
    }

    #[test]
    fn an_outline_and_a_patch_come_off_without_a_trace() {
        let bounds = Rect::new(0, 0, W as i32, H as i32);
        let original = frame();
        for around in [
            Rect::new(2, 2, 12, 9),
            Rect::new(0, 0, 16, 12), // on the edges
            Rect::new(5, 5, 7, 6),   // thinner than two outlines
            Rect::new(3, 3, 4, 4),
        ] {
            let mut px = original.clone();
            let outline = Outline::draw(&mut px, W, bounds, around, 2, 0x00ff_ffff);
            assert_ne!(px, original);
            assert_eq!(
                px[around.top as usize * W + around.left as usize],
                0x00ff_ffff
            );
            // Nothing outside the rectangle was touched.
            for (i, (&a, &b)) in px.iter().zip(&original).enumerate() {
                if !around.contains(((i % W) as i32, (i / W) as i32)) {
                    assert_eq!(a, b);
                }
            }
            outline.restore(&mut px, W);
            assert_eq!(px, original, "{around:?}");
        }

        // A patch hanging off the frame saves only what is inside it.
        let mut px = original.clone();
        let saved = Saved::take(&px, W, bounds, Rect::new(12, 8, 30, 30));
        assert_eq!(saved.rect(), Rect::new(12, 8, 16, 12));
        px.fill(0);
        saved.restore(&mut px, W);
        assert_eq!(px[8 * W + 12], original[8 * W + 12]);
        assert_eq!(px[0], 0);
    }
}
