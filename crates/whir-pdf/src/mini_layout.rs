//! The minimap's own layout: the document scaled down to a narrow column
//! (gaps and margins included), in minimap points from the top of the column.
//! Scaling everything alike keeps the minimap's scroll and the viewport
//! slider moving smoothly in step with the document. Only extreme pages
//! (slivers, receipt-like strips) get a scale of their own; the mapping stays
//! piecewise linear through them.

use std::ops::Range;

use crate::layout::{GAP, Layout, MARGIN};

/// No thumbnail is shorter than this (a sliver can't be clicked or seen).
const MIN_THUMB_H: f64 = 16.0;
/// Nor taller than this many column widths (a receipt-like page would
/// otherwise take over the minimap).
const MAX_THUMB_ASPECT: f64 = 3.0;

pub struct MiniLayout {
    /// Column width the widest page fills.
    width: f64,
    /// Minimap points per PDF point for everything but extreme pages.
    base: f64,
    /// Minimap points per PDF point, per page.
    scales: Vec<f64>,
    tops: Vec<f64>,
    bottoms: Vec<f64>,
    /// Content height.
    pub height: f64,
    /// (document y, minimap y) break points, both increasing.
    knots: Vec<(f64, f64)>,
}

impl MiniLayout {
    pub fn new(l: &Layout, width: f64) -> Self {
        let base = if l.max_width > 0.0 {
            width / l.max_width
        } else {
            1.0
        };
        let n = l.len();
        let (mut scales, mut tops, mut bottoms) = (
            Vec::with_capacity(n),
            Vec::with_capacity(n),
            Vec::with_capacity(n),
        );
        let mut knots = Vec::with_capacity(2 * n + 2);
        knots.push((0.0, 0.0));
        let mut y = MARGIN * base;
        for (i, &[w, h]) in l.sizes.iter().enumerate() {
            let (w, h) = (w as f64, h as f64);
            let scale = if h * base > MAX_THUMB_ASPECT * width {
                MAX_THUMB_ASPECT * width / h
            } else if h * base < MIN_THUMB_H {
                // Enlarged, but never wider than the column.
                (MIN_THUMB_H / h).min(width / w)
            } else {
                base
            };
            scales.push(scale);
            tops.push(y);
            bottoms.push(y + h * scale);
            knots.push((l.top(i), y));
            knots.push((l.bottom(i), y + h * scale));
            y += h * scale + GAP * base;
        }
        let height = if n == 0 {
            0.0
        } else {
            y - GAP * base + MARGIN * base
        };
        knots.push((l.total_height, height));
        Self {
            width,
            base,
            scales,
            tops,
            bottoms,
            height,
            knots,
        }
    }

    pub fn len(&self) -> usize {
        self.tops.len()
    }

    pub fn base(&self) -> f64 {
        self.base
    }

    pub fn scale(&self, page: usize) -> f64 {
        self.scales[page]
    }

    #[cfg(test)]
    pub fn top(&self, page: usize) -> f64 {
        self.tops[page]
    }

    #[cfg(test)]
    pub fn bottom(&self, page: usize) -> f64 {
        self.bottoms[page]
    }

    /// Page `i`'s thumbnail: x (centred in the column), y, width, height.
    pub fn thumb(&self, l: &Layout, page: usize) -> [f64; 4] {
        let s = self.scales[page];
        let w = l.sizes[page][0] as f64 * s;
        [
            (self.width - w) / 2.0,
            self.tops[page],
            w,
            self.bottoms[page] - self.tops[page],
        ]
    }

    /// Document y (PDF points) → minimap y.
    pub fn doc_to_map(&self, y: f64) -> f64 {
        interpolate(&self.knots, y, |k| k.0, |k| k.1)
    }

    /// Minimap y → document y (PDF points).
    #[cfg(test)]
    pub fn map_to_doc(&self, m: f64) -> f64 {
        interpolate(&self.knots, m, |k| k.1, |k| k.0)
    }

    /// Thumbnails overlapping [m0, m1).
    pub fn visible(&self, m0: f64, m1: f64) -> Range<usize> {
        let first = self.bottoms.partition_point(|&b| b <= m0);
        let last = self.tops.partition_point(|&t| t < m1);
        first..last.max(first)
    }

    /// The thumbnail at minimap y `m` (in a gap, the closer one).
    pub fn page_at(&self, m: f64) -> usize {
        let n = self.len();
        let mut i = self.tops.partition_point(|&t| t <= m).saturating_sub(1);
        if i + 1 < n && m > self.bottoms[i] && m - self.bottoms[i] > self.tops[i + 1] - m {
            i += 1;
        }
        i.min(n.saturating_sub(1))
    }

    /// How far the minimap is scrolled, VS Code style: not at all while it
    /// fits, else in proportion to the document (`t` = scroll fraction), so
    /// the slider can never leave it.
    pub fn offset(&self, view_h: f64, t: f64) -> f64 {
        if self.height <= view_h {
            0.0
        } else {
            t.clamp(0.0, 1.0) * (self.height - view_h)
        }
    }

    /// Where the viewport slider's top sits, in minimap points below the top
    /// of a `view_h`-tall minimap, when the document is scrolled to `off` UI
    /// points (of `max_off`) at `zoom`.
    pub fn slider_top(&self, view_h: f64, max_off: f64, zoom: f64, off: f64) -> f64 {
        let t = if max_off > 0.0 { off / max_off } else { 0.0 };
        self.doc_to_map(off / zoom) - self.offset(view_h, t)
    }

    /// The scroll offset that puts the slider's top at `target` (bisection:
    /// the slider moves steadily down as the document scrolls).
    pub fn offset_for_slider(&self, view_h: f64, max_off: f64, zoom: f64, target: f64) -> f64 {
        let top = |off: f64| self.slider_top(view_h, max_off, zoom, off);
        let (mut lo, mut hi) = (0.0, max_off.max(0.0));
        if target <= top(lo) {
            return lo;
        }
        if target >= top(hi) {
            return hi;
        }
        for _ in 0..64 {
            let mid = (lo + hi) / 2.0;
            if top(mid) < target {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        (lo + hi) / 2.0
    }
}

/// Piecewise-linear lookup over increasing knots (clamped to the ends).
fn interpolate(
    knots: &[(f64, f64)],
    x: f64,
    from: impl Fn(&(f64, f64)) -> f64,
    to: impl Fn(&(f64, f64)) -> f64,
) -> f64 {
    let (Some(first), Some(last)) = (knots.first(), knots.last()) else {
        return 0.0;
    };
    if x <= from(first) {
        return to(first);
    }
    if x >= from(last) {
        return to(last);
    }
    let i = knots
        .partition_point(|k| from(k) <= x)
        .clamp(1, knots.len() - 1);
    let (a, b) = (&knots[i - 1], &knots[i]);
    let span = from(b) - from(a);
    if span <= 0.0 {
        return to(b);
    }
    to(a) + (x - from(a)) / span * (to(b) - to(a))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    #[test]
    fn maps_pages_onto_thumbnails() {
        let l = Layout::new(vec![[612.0, 792.0], [792.0, 612.0], [612.0, 792.0]]);
        let m = MiniLayout::new(&l, 100.0);
        assert_eq!(m.len(), 3);
        // The widest page fills the column; the others keep their proportions.
        let t1 = m.thumb(&l, 1);
        assert!(close(t1[0], 0.0) && close(t1[2], 100.0));
        let base = 100.0 / 792.0;
        let t0 = m.thumb(&l, 0);
        assert!(close(t0[2], 612.0 * base) && close(t0[0], (100.0 - t0[2]) / 2.0));
        assert!(close(m.top(0), MARGIN * base));
        assert!(close(m.top(1), m.bottom(0) + GAP * base));
        assert!(close(m.height, m.bottom(2) + MARGIN * base));
        // Nothing clamped: a plain scale-down.
        assert!(close(m.height, l.total_height * base));
        assert!(close(m.doc_to_map(1234.5), 1234.5 * base));
        // Page edges land on thumbnail edges; the ends on the ends.
        for i in 0..3 {
            assert!(close(m.doc_to_map(l.top(i)), m.top(i)));
            assert!(close(m.doc_to_map(l.bottom(i)), m.bottom(i)));
        }
        assert!(close(m.doc_to_map(0.0), 0.0));
        assert!(close(m.doc_to_map(l.total_height), m.height));
        assert!(close(m.doc_to_map(-5.0), 0.0) && close(m.doc_to_map(1e9), m.height));
        // Monotonic, and the inverse round-trips.
        let mut prev = -1.0;
        for k in 0..=400 {
            let y = l.total_height * k as f64 / 400.0;
            let v = m.doc_to_map(y);
            assert!(v >= prev);
            prev = v;
            assert!(close(m.map_to_doc(v), y), "{y}");
        }
    }

    #[test]
    fn extreme_pages_are_clamped() {
        let l = Layout::new(vec![[612.0, 792.0], [200.0, 20000.0], [612.0, 4.0]]);
        let m = MiniLayout::new(&l, 100.0);
        let tall = m.thumb(&l, 1);
        assert!(close(tall[3], 300.0) && tall[2] < 100.0);
        let sliver = m.thumb(&l, 2);
        assert!(sliver[3] >= 16.0 - 1e-9 || close(sliver[2], 100.0));
        assert!(sliver[2] <= 100.0 + 1e-9);
    }

    #[test]
    fn lookup_and_scroll() {
        let l = Layout::new(vec![[612.0, 792.0]; 10]);
        let m = MiniLayout::new(&l, 100.0);
        assert_eq!(m.visible(0.0, 1.0), 0..0, "padding only");
        assert_eq!(m.visible(0.0, m.top(0) + 1.0), 0..1);
        assert_eq!(m.visible(m.bottom(0) + 1.0, m.top(3) + 1.0), 1..4);
        assert_eq!(m.visible(0.0, 1e9), 0..10);
        assert_eq!(m.page_at(0.0), 0);
        let gap = m.top(4) - m.bottom(3);
        assert_eq!(m.page_at(m.bottom(3) + gap * 0.25), 3);
        assert_eq!(m.page_at(m.top(4) - gap * 0.25), 4);
        assert_eq!(m.page_at(1e9), 9);
        // Fits: never scrolls. Else in proportion, end to end.
        assert_eq!(m.offset(m.height + 10.0, 0.7), 0.0);
        assert_eq!(m.offset(300.0, 0.0), 0.0);
        assert!(close(m.offset(300.0, 1.0), m.height - 300.0));
        assert!(close(m.offset(300.0, 2.0), m.height - 300.0));
        // The slider: steadily down the minimap, top to bottom, and dragging
        // it back to a position finds the same scroll offset.
        let (zoom, vh, view_h) = (1.3, 900.0, 300.0);
        let max_off = l.total_height * zoom - vh;
        let slider_h = |off: f64| m.doc_to_map((off + vh) / zoom) - m.doc_to_map(off / zoom);
        assert!(close(m.slider_top(view_h, max_off, zoom, 0.0), 0.0));
        assert!(close(
            m.slider_top(view_h, max_off, zoom, max_off),
            view_h - slider_h(max_off)
        ));
        let mut prev = -1.0;
        for k in 0..=200 {
            let off = max_off * k as f64 / 200.0;
            let top = m.slider_top(view_h, max_off, zoom, off);
            assert!(top >= prev - 1e-9, "{off}");
            prev = top;
            let back = m.offset_for_slider(view_h, max_off, zoom, top);
            assert!((back - off).abs() < 1e-3, "{off} → {top} → {back}");
        }
        assert_eq!(m.offset_for_slider(view_h, max_off, zoom, -50.0), 0.0);
        assert_eq!(m.offset_for_slider(view_h, max_off, zoom, 1e6), max_off);
        // A short document: the minimap doesn't scroll, the slider maps directly.
        let short = Layout::new(vec![[612.0, 792.0]; 2]);
        let ms = MiniLayout::new(&short, 100.0);
        let max_s = short.total_height * zoom - vh;
        assert!(close(
            ms.slider_top(view_h, max_s, zoom, max_s),
            ms.doc_to_map(max_s / zoom)
        ));

        let empty = MiniLayout::new(&Layout::new(Vec::new()), 100.0);
        assert_eq!(empty.height, 0.0);
        assert_eq!(empty.visible(0.0, 100.0), 0..0);
        assert_eq!(empty.doc_to_map(5.0), 0.0);
    }
}
