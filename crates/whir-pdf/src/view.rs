//! This frame's mapping between the screen and the pages: where each page is
//! drawn, and which page (and where on it) a screen point falls.
//!
//! Page-local coordinates are display points from the page's top-left, the
//! same frame as [`crate::geom`], so `to_screen` and `locate` invert each other.

use egui::{Pos2, Rect, pos2, vec2};

use crate::layout::{self, Layout};

#[derive(Clone, Copy, Debug)]
pub struct View {
    /// The viewport on screen.
    pub vp: Rect,
    /// UI points per PDF point.
    pub zoom: f64,
    /// Horizontal centring offset when the content is narrower than the viewport.
    pub x_off: f64,
    pub sx: f64,
    pub sy: f64,
    /// Physical pixels per UI point (page corners snap to whole pixels).
    pub ppp: f64,
}

impl View {
    /// Where page `i` is drawn, its top-left snapped to the pixel grid so tiles
    /// land on exact device pixels.
    pub fn page_rect(&self, l: &Layout, i: usize) -> Rect {
        let [pw, ph] = l.sizes[i];
        let doc_x = layout::MARGIN + (l.max_width - pw as f64) / 2.0;
        let x = self.vp.left() as f64 + self.x_off + doc_x * self.zoom - self.sx;
        let y = self.vp.top() as f64 + l.top(i) * self.zoom - self.sy;
        let snap = |v: f64| (v * self.ppp).round() / self.ppp;
        Rect::from_min_size(
            pos2(snap(x) as f32, snap(y) as f32),
            vec2(
                (pw as f64 * self.zoom) as f32,
                (ph as f64 * self.zoom) as f32,
            ),
        )
    }

    /// The page nearest to `p` vertically (in a gap, the closer of the two),
    /// and `p` in that page's local display points (possibly off the page).
    pub fn locate(&self, l: &Layout, p: Pos2) -> (usize, [f32; 2]) {
        let y = ((p.y - self.vp.top()) as f64 + self.sy) / self.zoom;
        let mut i = l.page_at(y);
        if y > l.bottom(i) && i + 1 < l.len() && y - l.bottom(i) > l.top(i + 1) - y {
            i += 1;
        }
        let r = self.page_rect(l, i);
        let local = [
            ((p.x - r.left()) as f64 / self.zoom) as f32,
            ((p.y - r.top()) as f64 / self.zoom) as f32,
        ];
        (i, local)
    }

    /// A page-local box on screen, given the page's screen rect.
    pub fn to_screen(self, page: Rect, r: [f32; 4]) -> Rect {
        let z = self.zoom as f32;
        Rect::from_min_max(
            page.min + vec2(r[0] * z, r[1] * z),
            page.min + vec2(r[2] * z, r[3] * z),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(sy: f64) -> View {
        View {
            vp: Rect::from_min_size(pos2(0.0, 40.0), vec2(800.0, 600.0)),
            zoom: 1.25,
            x_off: 7.0,
            sx: 0.0,
            sy,
            ppp: 1.5,
        }
    }

    #[test]
    fn locate_inverts_page_rect() {
        let l = Layout::new(vec![[612.0, 792.0], [400.0, 300.0], [612.0, 792.0]]);
        let v = view(900.0);
        for i in 0..3 {
            let r = v.page_rect(&l, i);
            for local in [[0.0f32, 0.0], [100.0, 50.0], [399.0, 299.0]] {
                let screen = v.to_screen(r, [local[0], local[1], local[0], local[1]]).min;
                let (page, back) = v.locate(&l, screen);
                assert_eq!(page, i);
                assert!((back[0] - local[0]).abs() < 1e-3 && (back[1] - local[1]).abs() < 1e-3);
            }
        }
    }

    #[test]
    fn gap_picks_the_closer_page() {
        let l = Layout::new(vec![[612.0, 792.0]; 2]);
        let v = view(0.0);
        // The gap is [MARGIN + 792, MARGIN + 792 + GAP) in document points.
        let doc_y = |y: f64| pos2(100.0, (40.0 + y * 1.25) as f32);
        let gap0 = layout::MARGIN + 792.0;
        assert_eq!(v.locate(&l, doc_y(gap0 + 2.0)).0, 0);
        assert_eq!(v.locate(&l, doc_y(gap0 + layout::GAP - 2.0)).0, 1);
        // Above the first page and below the last: the end pages.
        assert_eq!(v.locate(&l, doc_y(1.0)).0, 0);
        assert_eq!(v.locate(&l, doc_y(5000.0)).0, 1);
        let (_, local) = v.locate(&l, doc_y(gap0 + 2.0));
        assert!(local[1] > 792.0);
    }
}
