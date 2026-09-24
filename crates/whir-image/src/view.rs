//! Zoom/pan state for an image, independent of egui widgets (unit-testable).
//!
//! Units: `zoom` is egui points per full-resolution image pixel; `center` is the
//! image pixel shown at the middle of the viewport.

use egui::{Pos2, Rect, Vec2, pos2, vec2};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode {
    /// Fit the whole image in the viewport (never upscaling beyond 100%).
    Fit,
    Manual {
        zoom: f32,
        center: Pos2,
    },
}

#[derive(Clone, Copy, Debug)]
pub struct View {
    pub mode: Mode,
}

impl Default for View {
    fn default() -> Self {
        Self { mode: Mode::Fit }
    }
}

/// 100% = one image pixel per physical screen pixel.
pub fn actual_size_zoom(ppp: f32) -> f32 {
    1.0 / ppp
}

pub fn fit_zoom(image: Vec2, viewport: Vec2, ppp: f32) -> f32 {
    if image.x <= 0.0 || image.y <= 0.0 {
        return 1.0;
    }
    (viewport.x / image.x)
        .min(viewport.y / image.y)
        .min(actual_size_zoom(ppp))
}

/// Size of `image` on screen after `turns` quarter turns.
pub fn turned(image: Vec2, turns: u8) -> Vec2 {
    if turns % 2 == 1 {
        vec2(image.y, image.x)
    } else {
        image
    }
}

/// Where a point of the unturned image lands after `turns` quarter turns
/// clockwise, both as fractions of the image's width and height.
pub fn turn_point(p: Pos2, turns: u8) -> Pos2 {
    match turns % 4 {
        1 => pos2(1.0 - p.y, p.x),
        2 => pos2(1.0 - p.x, 1.0 - p.y),
        3 => pos2(p.y, 1.0 - p.x),
        _ => p,
    }
}

impl View {
    /// The image was turned a quarter (clockwise if `clockwise`); `image` is its
    /// size before. A zoomed-in view keeps its zoom and the point in the middle.
    pub fn rotate(&mut self, clockwise: bool, image: Vec2, viewport: Vec2, ppp: f32) {
        let Mode::Manual { zoom, center } = self.mode else {
            return;
        };
        let center = if clockwise {
            pos2(image.y - center.y, center.x)
        } else {
            pos2(center.y, image.x - center.x)
        };
        self.mode = Mode::Manual { zoom, center };
        // Re-clamps, and falls back to Fit if the zoom is now below it.
        self.set_zoom(zoom, Vec2::ZERO, turned(image, 1), viewport, ppp);
    }

    pub fn zoom(&self, image: Vec2, viewport: Vec2, ppp: f32) -> f32 {
        match self.mode {
            Mode::Fit => fit_zoom(image, viewport, ppp),
            Mode::Manual { zoom, .. } => zoom,
        }
    }

    pub fn center(&self, image: Vec2) -> Pos2 {
        match self.mode {
            Mode::Fit => (image / 2.0).to_pos2(),
            Mode::Manual { center, .. } => center,
        }
    }

    /// Multiply the zoom by `factor`, keeping the image point under `anchor`
    /// (relative to the viewport center, in points) fixed on screen.
    pub fn zoom_by(&mut self, factor: f32, anchor: Vec2, image: Vec2, viewport: Vec2, ppp: f32) {
        let old = self.zoom(image, viewport, ppp);
        self.set_zoom(old * factor, anchor, image, viewport, ppp);
    }

    /// Zoom never goes below fit; reaching it switches to [`Mode::Fit`], which
    /// then follows window resizes.
    pub fn set_zoom(&mut self, zoom: f32, anchor: Vec2, image: Vec2, viewport: Vec2, ppp: f32) {
        let old = self.zoom(image, viewport, ppp);
        let fit = fit_zoom(image, viewport, ppp);
        let max = (64.0 * actual_size_zoom(ppp)).max(fit);
        let new = zoom.clamp(fit, max);
        if new <= fit * 1.001 {
            self.mode = Mode::Fit;
            return;
        }
        let under = self.center(image) + anchor / old;
        let center = under - anchor / new;
        self.mode = Mode::Manual { zoom: new, center };
        self.clamp(image, viewport, ppp);
    }

    pub fn pan(&mut self, delta_points: Vec2, image: Vec2, viewport: Vec2, ppp: f32) {
        // Nothing to pan: stay in Fit so the image keeps following window resizes.
        if matches!(self.mode, Mode::Fit) && !self.pannable(image, viewport, ppp) {
            return;
        }
        let zoom = self.zoom(image, viewport, ppp);
        let center = self.center(image) - delta_points / zoom;
        self.mode = Mode::Manual { zoom, center };
        self.clamp(image, viewport, ppp);
    }

    /// Keep the image on screen: centered along axes where it fits, edges
    /// never pulled inside the viewport along axes where it doesn't.
    pub fn clamp(&mut self, image: Vec2, viewport: Vec2, ppp: f32) {
        let Mode::Manual { zoom, center } = self.mode else {
            return;
        };
        let half = viewport / (2.0 * zoom);
        let axis = |c: f32, img: f32, half: f32| {
            if img <= 2.0 * half {
                img / 2.0
            } else {
                c.clamp(half, img - half)
            }
        };
        let center = pos2(
            axis(center.x, image.x, half.x),
            axis(center.y, image.y, half.y),
        );
        self.mode = Mode::Manual { zoom, center };
        let _ = ppp;
    }

    /// Screen rect of the whole image, snapped to physical pixels.
    pub fn image_rect(&self, viewport: Rect, image: Vec2, ppp: f32) -> Rect {
        let zoom = self.zoom(image, viewport.size(), ppp);
        let size = image * zoom;
        let min = viewport.center() - self.center(image).to_vec2() * zoom;
        let snap = |v: f32| (v * ppp).round() / ppp;
        let min = pos2(snap(min.x), snap(min.y));
        Rect::from_min_size(
            min,
            vec2(snap(size.x).max(1.0 / ppp), snap(size.y).max(1.0 / ppp)),
        )
    }

    /// Whether the image is larger than the viewport (so it can be panned).
    pub fn pannable(&self, image: Vec2, viewport: Vec2, ppp: f32) -> bool {
        let size = image * self.zoom(image, viewport, ppp);
        size.x > viewport.x + 0.5 || size.y > viewport.y + 0.5
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IMG: Vec2 = vec2(4000.0, 3000.0);
    const VP: Vec2 = vec2(1000.0, 800.0);

    #[test]
    fn fit_never_upscales() {
        assert_eq!(fit_zoom(vec2(100.0, 50.0), VP, 1.0), 1.0);
        assert_eq!(fit_zoom(vec2(100.0, 50.0), VP, 2.0), 0.5);
        assert!((fit_zoom(IMG, VP, 1.0) - 0.25).abs() < 1e-6);
    }

    #[test]
    fn zoom_keeps_anchor_fixed() {
        let mut v = View::default();
        let anchor = vec2(200.0, -100.0);
        let before = v.center(IMG) + anchor / v.zoom(IMG, VP, 1.0);
        v.zoom_by(2.0, anchor, IMG, VP, 1.0);
        let after = v.center(IMG) + anchor / v.zoom(IMG, VP, 1.0);
        assert!((before - after).length() < 1e-3, "{before:?} vs {after:?}");
    }

    #[test]
    fn zooming_out_past_fit_snaps_back_to_fit() {
        let mut v = View::default();
        v.zoom_by(3.0, Vec2::ZERO, IMG, VP, 1.0);
        assert!(matches!(v.mode, Mode::Manual { .. }));
        v.zoom_by(0.1, Vec2::ZERO, IMG, VP, 1.0);
        assert_eq!(v.mode, Mode::Fit);
        v.zoom_by(0.5, Vec2::ZERO, IMG, VP, 1.0);
        assert_eq!(v.mode, Mode::Fit);
    }

    #[test]
    fn pan_is_clamped_to_image() {
        let mut v = View::default();
        v.set_zoom(1.0, Vec2::ZERO, IMG, VP, 1.0);
        v.pan(vec2(1e6, 1e6), IMG, VP, 1.0);
        let c = v.center(IMG);
        assert_eq!(c, pos2(500.0, 400.0));
    }

    #[test]
    fn dragging_a_fitted_image_keeps_fit() {
        let mut v = View::default();
        v.pan(vec2(30.0, 10.0), IMG, VP, 1.0);
        assert_eq!(v.mode, Mode::Fit);
    }

    #[test]
    fn quarter_turns_move_corners() {
        let tl = pos2(0.0, 0.0);
        assert_eq!(turn_point(tl, 1), pos2(1.0, 0.0)); // clockwise: top-left → top-right
        assert_eq!(turn_point(tl, 2), pos2(1.0, 1.0));
        assert_eq!(turn_point(tl, 3), pos2(0.0, 1.0)); // counter-clockwise: → bottom-left
        let p = pos2(0.25, 0.75);
        let mut q = p;
        for _ in 0..4 {
            q = turn_point(q, 1);
        }
        assert_eq!(q, p);
        assert_eq!(turned(IMG, 1), vec2(3000.0, 4000.0));
        assert_eq!(turned(IMG, 2), IMG);
    }

    #[test]
    fn rotating_keeps_the_middle_point() {
        let mut v = View::default();
        v.set_zoom(1.0, Vec2::ZERO, IMG, VP, 1.0);
        v.mode = Mode::Manual {
            zoom: 1.0,
            center: pos2(1000.0, 500.0),
        };
        v.rotate(true, IMG, VP, 1.0);
        let t = turned(IMG, 1);
        let expected = turn_point(pos2(1000.0 / IMG.x, 500.0 / IMG.y), 1);
        assert_eq!(v.center(t), pos2(expected.x * t.x, expected.y * t.y));
        assert_eq!(v.zoom(t, VP, 1.0), 1.0);
        v.rotate(false, t, VP, 1.0);
        assert_eq!(v.center(IMG), pos2(1000.0, 500.0));
    }

    #[test]
    fn rotating_below_the_new_fit_goes_to_fit() {
        // Portrait in a landscape window: fit 0.2 before the turn, 0.25 after.
        let img = vec2(3000.0, 4000.0);
        let mut v = View::default();
        v.set_zoom(0.22, Vec2::ZERO, img, VP, 1.0);
        assert!(matches!(v.mode, Mode::Manual { .. }));
        v.rotate(true, img, VP, 1.0);
        assert_eq!(v.mode, Mode::Fit);
        v.rotate(true, turned(img, 1), VP, 1.0);
        assert_eq!(v.mode, Mode::Fit);
    }

    #[test]
    fn small_image_stays_centered() {
        let img = vec2(200.0, 100.0);
        let mut v = View::default();
        v.set_zoom(2.0, Vec2::ZERO, img, VP, 1.0);
        v.pan(vec2(300.0, 0.0), img, VP, 1.0);
        assert_eq!(v.center(img), pos2(100.0, 50.0));
    }
}
