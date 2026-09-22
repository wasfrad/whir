//! Zoom/pan state for an image, independent of egui widgets (unit-testable).
//!
//! Units: `zoom` is egui points per full-resolution image pixel; `center` is the
//! image pixel shown at the middle of the viewport.

use egui::{Pos2, Rect, Vec2, pos2, vec2};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode {
    /// Fit the whole image in the viewport (never upscaling beyond 100%).
    Fit,
    Manual { zoom: f32, center: Pos2 },
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

impl View {
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
        let center = pos2(axis(center.x, image.x, half.x), axis(center.y, image.y, half.y));
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
        Rect::from_min_size(min, vec2(snap(size.x).max(1.0 / ppp), snap(size.y).max(1.0 / ppp)))
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
    fn small_image_stays_centered() {
        let img = vec2(200.0, 100.0);
        let mut v = View::default();
        v.set_zoom(2.0, Vec2::ZERO, img, VP, 1.0);
        v.pan(vec2(300.0, 0.0), img, VP, 1.0);
        assert_eq!(v.center(img), pos2(100.0, 50.0));
    }
}
