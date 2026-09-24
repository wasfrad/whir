//! Continuous vertical page layout, in PDF points (1/72 in). Everything scales
//! linearly with zoom (UI points per PDF point), gaps included, so anchoring
//! the zoom at the cursor is exact.

pub const GAP: f64 = 10.0;
pub const MARGIN: f64 = 10.0;

pub struct Layout {
    pub sizes: Vec<[f32; 2]>,
    /// Top of each page, in PDF points from the top of the document.
    tops: Vec<f64>,
    pub total_height: f64,
    pub max_width: f64,
}

impl Layout {
    pub fn new(sizes: Vec<[f32; 2]>) -> Self {
        let mut tops = Vec::with_capacity(sizes.len());
        let mut y = MARGIN;
        let mut max_width: f64 = 0.0;
        for s in &sizes {
            tops.push(y);
            y += s[1] as f64 + GAP;
            max_width = max_width.max(s[0] as f64);
        }
        let total_height = if sizes.is_empty() {
            0.0
        } else {
            y - GAP + MARGIN
        };
        Self {
            sizes,
            tops,
            total_height,
            max_width,
        }
    }

    pub fn len(&self) -> usize {
        self.sizes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sizes.is_empty()
    }

    pub fn top(&self, page: usize) -> f64 {
        self.tops[page]
    }

    /// Bottom edge of a page, in PDF points from the top of the document.
    pub fn bottom(&self, page: usize) -> f64 {
        self.tops[page] + self.sizes[page][1] as f64
    }

    pub fn content_width(&self) -> f64 {
        self.max_width + 2.0 * MARGIN
    }

    /// Page containing (or nearest to) document y (PDF points).
    pub fn page_at(&self, y: f64) -> usize {
        match self.tops.partition_point(|&t| t <= y) {
            0 => 0,
            i => (i - 1).min(self.len().saturating_sub(1)),
        }
    }

    /// Pages overlapping [y0, y1) in PDF points.
    pub fn visible(&self, y0: f64, y1: f64) -> std::ops::Range<usize> {
        if self.is_empty() {
            return 0..0;
        }
        let first = self.page_at(y0);
        let last = self.page_at(y1);
        first..(last + 1).min(self.len())
    }

    /// Where a jump to `page` puts the top of the viewport: just above the
    /// page, half a gap of breathing room (the first page: the very top).
    pub fn landing(&self, page: usize) -> f64 {
        if page == 0 {
            0.0
        } else {
            self.tops[page] - GAP / 2.0
        }
    }

    /// The page "next page" / "previous page" goes to from viewport top `y`:
    /// the first page landing below it, or the last landing above it (from
    /// partway down a page, that page's own top). Stays put at either end.
    pub fn page_step(&self, y: f64, forward: bool) -> usize {
        const EPS: f64 = 0.5;
        let n = self.len();
        if n == 0 {
            return 0;
        }
        if forward {
            (0..n).find(|&p| self.landing(p) > y + EPS).unwrap_or(n - 1)
        } else {
            (0..n)
                .rev()
                .find(|&p| self.landing(p) < y - EPS)
                .unwrap_or(0)
        }
    }

    /// The page that best fills [y0, y1): the most of what could fit of it is
    /// visible (all of a short page, or the whole viewport for a tall one).
    /// Ties go to the earliest, so right after a page jump it's that page.
    pub fn most_visible(&self, y0: f64, y1: f64) -> usize {
        let mut best = (0, -1.0);
        for p in self.visible(y0, y1) {
            let h = self.sizes[p][1] as f64;
            let seen = self.bottom(p).min(y1) - self.top(p).max(y0);
            let score = seen / h.min(y1 - y0).max(1e-9);
            if score > best.1 + 1e-9 {
                best = (p, score);
            }
        }
        best.0
    }

    /// Zoom that fits the widest page to `viewport_width` UI points.
    pub fn fit_width_zoom(&self, viewport_width: f64) -> f64 {
        if self.max_width <= 0.0 {
            return 1.0;
        }
        (viewport_width / self.content_width()).max(0.02)
    }
}

/// Split a page raster (`page_px`) into tiles. Small pages are one tile.
pub fn tiles(page_px: [u32; 2], max_side: u32) -> Vec<[u32; 4]> {
    const WHOLE_PAGE_MAX_PIXELS: u64 = 8 << 20;
    const TILE: u32 = 1024;
    let [w, h] = page_px;
    if (w as u64) * (h as u64) <= WHOLE_PAGE_MAX_PIXELS && w <= max_side && h <= max_side {
        return vec![[0, 0, w, h]];
    }
    let mut out = Vec::new();
    let mut y = 0;
    while y < h {
        let mut x = 0;
        while x < w {
            out.push([x, y, TILE.min(w - x), TILE.min(h - y)]);
            x += TILE;
        }
        y += TILE;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn letter(n: usize) -> Layout {
        Layout::new(vec![[612.0, 792.0]; n])
    }

    #[test]
    fn page_lookup() {
        let l = letter(3);
        assert_eq!(l.page_at(0.0), 0);
        assert_eq!(l.page_at(MARGIN + 791.0), 0);
        assert_eq!(l.page_at(MARGIN + 792.0 + GAP + 1.0), 1);
        assert_eq!(l.page_at(1e9), 2);
        assert_eq!(l.visible(0.0, 100.0), 0..1);
        assert_eq!(l.visible(700.0, 900.0), 0..2);
        assert_eq!(l.total_height, 3.0 * 792.0 + 2.0 * GAP + 2.0 * MARGIN);
    }

    #[test]
    fn page_steps() {
        let l = letter(3);
        let land = |p| l.landing(p);
        assert_eq!(land(0), 0.0);
        assert_eq!(land(1), l.top(1) - GAP / 2.0);
        // From the top: next is page 1, previous stays on page 0.
        assert_eq!(l.page_step(0.0, true), 1);
        assert_eq!(l.page_step(0.0, false), 0);
        // A little way into page 0: back to the top.
        assert_eq!(l.page_step(MARGIN + 50.0, false), 0);
        // Exactly on a page: its neighbours.
        assert_eq!(l.page_step(land(1), true), 2);
        assert_eq!(l.page_step(land(1), false), 0);
        // Partway down page 1: back goes to its own top.
        assert_eq!(l.page_step(land(1) + 300.0, false), 1);
        assert_eq!(l.page_step(land(1) + 300.0, true), 2);
        // The last page: forward stays.
        assert_eq!(l.page_step(land(2), true), 2);
        assert_eq!(Layout::new(Vec::new()).page_step(0.0, true), 0);
    }

    #[test]
    fn most_visible_page() {
        let l = letter(3);
        // A viewport shorter than a page: the one filling more of it.
        assert_eq!(l.most_visible(0.0, 500.0), 0);
        assert_eq!(l.most_visible(l.landing(1), l.landing(1) + 500.0), 1);
        assert_eq!(l.most_visible(l.top(1) - 300.0, l.top(1) + 200.0), 0);
        assert_eq!(l.most_visible(l.top(1) - 200.0, l.top(1) + 300.0), 1);
        // Several whole pages: the first.
        assert_eq!(l.most_visible(l.landing(1), l.total_height), 1);
        // A small page wholly visible beats most of a big one.
        let mixed = Layout::new(vec![[612.0, 792.0], [300.0, 200.0], [612.0, 792.0]]);
        assert_eq!(
            mixed.most_visible(mixed.landing(1), mixed.landing(1) + 600.0),
            1
        );
    }

    #[test]
    fn tiling() {
        assert_eq!(tiles([816, 1056], 16384), vec![[0, 0, 816, 1056]]);
        let t = tiles([5000, 6000], 16384);
        assert_eq!(t.len(), 5 * 6);
        assert_eq!(t.last().unwrap(), &[4096, 5120, 904, 880]);
        let area: u64 = t.iter().map(|r| r[2] as u64 * r[3] as u64).sum();
        assert_eq!(area, 5000 * 6000);
    }
}
