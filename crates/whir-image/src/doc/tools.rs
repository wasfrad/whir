//! The geometry of marking: drawing, moving and resizing by handles.
//! Positions are in image pixels (before any on-screen turn).

use egui::{Pos2, Rect, Vec2, pos2, vec2};
use whir_core::marks::{Ink, Kind, Mark, Pen, Text};

/// The minimum drawn box side or arrow length, in screen points.
pub const MIN_DRAW: f32 = 4.0;

/// The mark a drag between `a` and `b` draws: whole pixels, inside the
/// image. `None` if a side is under `min` pixels.
pub fn drawn(a: Pos2, b: Pos2, image: Vec2, min: f32, ink: Ink) -> Option<Mark> {
    let r = Rect::from_two_pos(a, b);
    if r.width() < min || r.height() < min {
        return None;
    }
    let x0 = r.min.x.floor().clamp(0.0, image.x) as u32;
    let y0 = r.min.y.floor().clamp(0.0, image.y) as u32;
    let x1 = r.max.x.ceil().clamp(0.0, image.x) as u32;
    let y1 = r.max.y.ceil().clamp(0.0, image.y) as u32;
    (x1 > x0 && y1 > y0).then(|| Mark::highlight(x0, y0, x1 - x0, y1 - y0, ink))
}

/// A stroke inside the image, including a single-point dot.
pub fn freehand(points: &[Pos2], image: Vec2, pen: Pen, width: u32) -> Option<Mark> {
    let points: Option<Vec<_>> = points.iter().map(|&p| clamped(p, image)).collect();
    Mark::drawing(&points?, pen, width, false)
}

/// An arrow from tail to tip, kept inside the image. Length, not either side
/// of its box, must reach `min`, so horizontal and vertical arrows are valid.
pub fn arrow(from: Pos2, to: Pos2, image: Vec2, min: f32, pen: Pen, width: u32) -> Option<Mark> {
    let (from, to) = (clamped(from, image)?, clamped(to, image)?);
    if !min.is_finite() || from.distance(to) < min {
        return None;
    }
    Mark::drawing(&[from, to], pen, width, true)
}

fn clamped(point: Pos2, image: Vec2) -> Option<Pos2> {
    if !point.is_finite() || !image.is_finite() || image.x <= 0.0 || image.y <= 0.0 {
        return None;
    }
    Some(pos2(
        point.x.clamp(0.0, image.x),
        point.y.clamp(0.0, image.y),
    ))
}

/// Where a box of `w` × `h` with its top left at `min` goes: there, moved
/// back inside the image as far as it fits.
pub fn placed(min: Pos2, w: u32, h: u32, image: Vec2) -> (u32, u32) {
    let axis =
        |at: f32, len: u32, size: f32| at.round().clamp(0.0, (size - len as f32).max(0.0)) as u32;
    (axis(min.x, w, image.x), axis(min.y, h, image.y))
}

/// `mark` moved by `delta` pixels, kept inside the image.
pub fn moved(mark: &Mark, delta: Vec2, image: Vec2) -> Mark {
    let at = pos2(mark.x as f32, mark.y as f32) + delta;
    let (x, y) = placed(at, mark.w, mark.h, image);
    Mark {
        x,
        y,
        ..mark.clone()
    }
}

/// A grip on a selected mark's box, or an arrow's tail or tip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    NW,
    N,
    NE,
    E,
    SE,
    S,
    SW,
    W,
    Start,
    End,
}

impl Handle {
    pub const ALL: [Handle; 8] = [
        Handle::NW,
        Handle::N,
        Handle::NE,
        Handle::E,
        Handle::SE,
        Handle::S,
        Handle::SW,
        Handle::W,
    ];
    pub const CORNERS: [Handle; 4] = [Handle::NW, Handle::NE, Handle::SE, Handle::SW];
    pub const ENDPOINTS: [Handle; 2] = [Handle::Start, Handle::End];

    /// Which sides it moves: -1 the left (top), 1 the right (bottom), 0 neither.
    pub fn pull(self) -> Vec2 {
        match self {
            Handle::NW => vec2(-1.0, -1.0),
            Handle::N => vec2(0.0, -1.0),
            Handle::NE => vec2(1.0, -1.0),
            Handle::E => vec2(1.0, 0.0),
            Handle::SE => vec2(1.0, 1.0),
            Handle::S => vec2(0.0, 1.0),
            Handle::SW => vec2(-1.0, 1.0),
            Handle::W => vec2(-1.0, 0.0),
            Handle::Start | Handle::End => Vec2::ZERO,
        }
    }

    /// Where it is on `mark`'s box, or its actual ordered arrow endpoint.
    pub fn on(self, mark: &Mark) -> Pos2 {
        let half = vec2(mark.w as f32, mark.h as f32) / 2.0;
        let center = pos2(mark.x as f32, mark.y as f32) + half;
        match self {
            Handle::Start => mark.drawing_points().first().copied().unwrap_or(center),
            Handle::End => mark.drawing_points().last().copied().unwrap_or(center),
            _ => center + self.pull() * half,
        }
    }
}

/// Arrows have endpoints; text and freehand scale by their corners. Boxes
/// have eight handles, or only corners when `edges` is false (too small on
/// screen to tell the handles apart).
pub fn handles(mark: &Mark, edges: bool) -> &'static [Handle] {
    match &mark.kind {
        Kind::Arrow(_) => &Handle::ENDPOINTS,
        // A dot's size is its pen width, not its one-pixel storage box. Grips
        // there would cover its entire hit region and prevent moving it.
        Kind::Freehand(d) if d.points.len() == 1 => &[],
        Kind::Highlight(_) if edges => &Handle::ALL,
        _ => &Handle::CORNERS,
    }
}

/// `mark` with `handle` dragged by `delta` pixels. A box's sides follow
/// (past the opposite side it turns over), at least `min` pixels and inside
/// the image. Freehand scales evenly from the opposite corner without
/// changing its pen width. Arrows move one endpoint, keeping tail and tip order.
/// Text also scales evenly; its box here is only an estimate, for the caller
/// to measure (see [`anchored`]). `None` if the requested geometry is invalid.
pub fn resized(mark: &Mark, handle: Handle, delta: Vec2, image: Vec2, min: f32) -> Option<Mark> {
    let pull = handle.pull();
    match &mark.kind {
        Kind::Arrow(drawing) => {
            let mut points = mark.drawing_points();
            let index = match handle {
                Handle::Start => 0,
                Handle::End => 1,
                _ => return None,
            };
            let point = points.get_mut(index)?;
            *point = clamped(*point + delta, image)?;
            if !min.is_finite() || points[0].distance(points[1]) < min {
                return None;
            }
            Mark::drawing(&points, drawing.pen, drawing.width, true)
        }
        Kind::Freehand(drawing) => {
            if !Handle::CORNERS.contains(&handle)
                || mark.w == 0
                || mark.h == 0
                || !delta.is_finite()
                || !image.is_finite()
            {
                return None;
            }
            let size = vec2(mark.w as f32, mark.h as f32);
            let fixed = handle.on(mark) - pull * size;
            let room = vec2(
                if pull.x < 0.0 {
                    fixed.x
                } else {
                    image.x - fixed.x
                },
                if pull.y < 0.0 {
                    fixed.y
                } else {
                    image.y - fixed.y
                },
            );
            if fixed.x < 0.0 || fixed.y < 0.0 || fixed.x > image.x || fixed.y > image.y {
                return None;
            }
            // A flat path's padded box axis must not prevent the path shortening.
            let first = drawing.points.first()?;
            let varies = [0, 1].map(|axis| drawing.points.iter().any(|p| p[axis] != first[axis]));
            let grown = (size + pull * delta) / size;
            let (lower, scale) = match varies {
                [true, false] => (1.0 / size.x, grown.x),
                [false, true] => (1.0 / size.y, grown.y),
                _ => ((1.0 / size.x).max(1.0 / size.y), grown.x.max(grown.y)),
            };
            // Both box axes, including padding, must still fit inside the image.
            let upper = (room.x.floor() / size.x).min(room.y.floor() / size.y);
            if upper < lower {
                return None;
            }
            let scale = scale.clamp(lower, upper);
            let (w, h) = (
                (size.x * scale).round().max(1.0) as u32,
                (size.y * scale).round().max(1.0) as u32,
            );
            let (x, y) = anchored(mark, handle, w, h);
            Some(Mark {
                x,
                y,
                w,
                h,
                ..mark.clone()
            })
        }
        Kind::Highlight(ink) => {
            let (mut x0, mut y0) = (mark.x as f32, mark.y as f32);
            let (mut x1, mut y1) = (x0 + mark.w as f32, y0 + mark.h as f32);
            // The moving side, kept `min` away from the one that stays.
            let follow = |stays: f32, moves: f32, toward: f32| {
                let d = moves - stays;
                if d.abs() >= min {
                    moves
                } else if d == 0.0 {
                    stays + toward * min
                } else {
                    stays + d.signum() * min
                }
            };
            if pull.x < 0.0 {
                x0 = follow(x1, x0 + delta.x, -1.0);
            } else if pull.x > 0.0 {
                x1 = follow(x0, x1 + delta.x, 1.0);
            }
            if pull.y < 0.0 {
                y0 = follow(y1, y0 + delta.y, -1.0);
            } else if pull.y > 0.0 {
                y1 = follow(y0, y1 + delta.y, 1.0);
            }
            // Each side to the nearest pixel (drawing rounds outward instead).
            let (a, b) = (pos2(x0, y0).round(), pos2(x1, y1).round());
            drawn(a, b, image, 0.0, *ink)
        }
        Kind::Text(t) => {
            // How much bigger the box would be, by whichever side grew more.
            let (w, h) = (mark.w.max(1) as f32, mark.h.max(1) as f32);
            let grown = vec2(w, h) + pull * delta;
            let scale = (grown.x / w).max(grown.y / h).max(0.0);
            let size =
                ((t.size as f32 * scale).round() as u32).clamp(Text::MIN_SIZE, Text::MAX_SIZE);
            let ratio = size as f32 / t.size as f32;
            let (w, h) = ((w * ratio).round() as u32, (h * ratio).round() as u32);
            let (x, y) = anchored(mark, handle, w.max(1), h.max(1));
            Some(Mark {
                x,
                y,
                w: w.max(1),
                h: h.max(1),
                kind: Kind::Text(Text { size, ..t.clone() }),
            })
        }
    }
}

/// The top left of a `w` × `h` box that keeps the corner of `mark` opposite
/// `handle` where it is (not past the image's top left).
pub fn anchored(mark: &Mark, handle: Handle, w: u32, h: u32) -> (u32, u32) {
    let pull = handle.pull();
    let x = if pull.x < 0.0 {
        (mark.x + mark.w).saturating_sub(w)
    } else {
        mark.x
    };
    let y = if pull.y < 0.0 {
        (mark.y + mark.h).saturating_sub(h)
    } else {
        mark.y
    };
    (x, y)
}

#[cfg(test)]
mod tests {
    use super::*;

    const IMAGE: Vec2 = vec2(400.0, 200.0);

    fn rect(x: u32, y: u32, w: u32, h: u32) -> Mark {
        Mark::highlight(x, y, w, h, Ink::Yellow)
    }

    fn sides(m: &Mark) -> (u32, u32, u32, u32) {
        (m.x, m.y, m.w, m.h)
    }

    fn resize(m: &Mark, handle: Handle, dx: f32, dy: f32) -> Option<(u32, u32, u32, u32)> {
        resized(m, handle, vec2(dx, dy), IMAGE, 4.0).map(|m| sides(&m))
    }

    fn text(x: u32, y: u32, w: u32, h: u32, size: u32) -> Mark {
        Mark {
            x,
            y,
            w,
            h,
            kind: Kind::Text(Text {
                body: "Hi".into(),
                size,
                pen: Pen::Red,
                turn: 0,
            }),
        }
    }

    fn points_near(mark: &Mark, expected: &[Pos2]) {
        let points = mark.drawing_points();
        assert_eq!(points.len(), expected.len());
        for (point, expected) in points.iter().zip(expected) {
            assert!(
                point.distance(*expected) < 0.01,
                "{point:?} != {expected:?}"
            );
        }
    }

    #[test]
    fn freehand_clamps_every_point_and_accepts_dots() {
        let m = freehand(
            &[pos2(-10.0, 30.0), pos2(30.0, -20.0), pos2(500.0, 250.0)],
            IMAGE,
            Pen::Black,
            8,
        )
        .unwrap();
        points_near(&m, &[pos2(0.0, 30.0), pos2(30.0, 0.0), pos2(400.0, 200.0)]);
        assert!(matches!(m.kind, Kind::Freehand(_)));
        assert_eq!(m.drawing_style().unwrap().pen, Pen::Black);
        assert_eq!(m.drawing_style().unwrap().width, 8);

        let dot = freehand(&[pos2(-10.0, 230.0)], IMAGE, Pen::Red, 4).unwrap();
        points_near(&dot, &[pos2(0.0, 200.0)]);
        assert_eq!((dot.w, dot.h), (1, 1));
        assert!(freehand(&[], IMAGE, Pen::Red, 4).is_none());
    }

    #[test]
    fn arrows_keep_endpoint_order_and_use_length_not_box_sides() {
        for (from, to) in [
            (pos2(20.0, 30.0), pos2(120.0, 30.0)),
            (pos2(120.0, 30.0), pos2(20.0, 30.0)),
            (pos2(20.0, 30.0), pos2(20.0, 130.0)),
            (pos2(20.0, 130.0), pos2(20.0, 30.0)),
            (pos2(20.0, 30.0), pos2(23.0, 33.0)),
            (pos2(24.0, 30.0), pos2(20.0, 30.0)),
        ] {
            let m = arrow(from, to, IMAGE, 4.0, Pen::Blue, 4).unwrap();
            assert!(matches!(m.kind, Kind::Arrow(_)));
            points_near(&m, &[from, to]);
            assert_eq!(Handle::Start.on(&m), m.drawing_points()[0]);
            assert_eq!(Handle::End.on(&m), m.drawing_points()[1]);
            assert!(m.w > 0 && m.h > 0);
        }
        assert!(arrow(pos2(20.0, 30.0), pos2(23.9, 30.0), IMAGE, 4.0, Pen::Red, 4).is_none());
        assert!(arrow(pos2(20.0, 30.0), pos2(23.0, 33.0), IMAGE, 5.0, Pen::Red, 4).is_none());
        assert!(arrow(pos2(20.0, 30.0), pos2(20.0, 30.0), IMAGE, 0.0, Pen::Red, 4).is_none());
    }

    #[test]
    fn arrows_check_minimum_length_after_clamping() {
        let m = arrow(
            pos2(-10.0, 300.0),
            pos2(500.0, -20.0),
            IMAGE,
            4.0,
            Pen::Red,
            4,
        )
        .unwrap();
        points_near(&m, &[pos2(0.0, 200.0), pos2(400.0, 0.0)]);
        assert!(
            arrow(
                pos2(399.0, 20.0),
                pos2(500.0, 20.0),
                IMAGE,
                4.0,
                Pen::Red,
                4
            )
            .is_none()
        );
        assert!(arrow(pos2(-10.0, 20.0), pos2(-5.0, 20.0), IMAGE, 4.0, Pen::Red, 4).is_none());
    }

    #[test]
    fn drawing_inputs_must_be_finite_and_have_valid_styles() {
        for point in [pos2(f32::NAN, 10.0), pos2(10.0, f32::INFINITY)] {
            assert!(freehand(&[point], IMAGE, Pen::Red, 4).is_none());
            assert!(arrow(point, pos2(20.0, 20.0), IMAGE, 4.0, Pen::Red, 4).is_none());
        }
        for image in [Vec2::ZERO, vec2(-1.0, 20.0), vec2(f32::NAN, 20.0)] {
            assert!(freehand(&[pos2(10.0, 10.0)], image, Pen::Red, 4).is_none());
        }
        for width in [0, 257] {
            assert!(freehand(&[pos2(10.0, 10.0)], IMAGE, Pen::Red, width).is_none());
            assert!(
                arrow(
                    pos2(10.0, 10.0),
                    pos2(20.0, 20.0),
                    IMAGE,
                    4.0,
                    Pen::Red,
                    width
                )
                .is_none()
            );
        }
    }

    #[test]
    fn arrow_handles_are_actual_endpoints_not_box_corners() {
        let m = arrow(
            pos2(100.3, 50.25),
            pos2(20.25, 90.75),
            IMAGE,
            4.0,
            Pen::Red,
            4,
        )
        .unwrap();
        assert!(Handle::Start.on(&m).distance(pos2(100.3, 50.25)) < 0.01);
        assert!(Handle::End.on(&m).distance(pos2(20.25, 90.75)) < 0.01);
        assert_eq!(Handle::Start.pull(), Vec2::ZERO);
        assert_eq!(Handle::End.pull(), Vec2::ZERO);
        for edges in [false, true] {
            assert_eq!(handles(&m, edges), Handle::ENDPOINTS);
        }
    }

    #[test]
    fn either_arrow_endpoint_can_rotate_around_the_other() {
        let m = arrow(
            pos2(100.0, 80.0),
            pos2(200.0, 100.0),
            IMAGE,
            4.0,
            Pen::Green,
            256,
        )
        .unwrap();
        for (handle, index) in [(Handle::Start, 0), (Handle::End, 1)] {
            let before = m.drawing_points();
            let fixed = before[1 - index];
            for offset in [
                vec2(-40.0, -20.0),
                vec2(40.0, -20.0),
                vec2(-40.0, 20.0),
                vec2(40.0, 20.0),
                vec2(-40.0, 0.0),
                vec2(40.0, 0.0),
                vec2(0.0, -20.0),
                vec2(0.0, 20.0),
            ] {
                let target = fixed + offset;
                let r = resized(&m, handle, target - before[index], IMAGE, 4.0).unwrap();
                let mut expected = before.clone();
                expected[index] = target;
                points_near(&r, &expected);
                assert_eq!(r.drawing_style().unwrap().pen, Pen::Green);
                assert_eq!(r.drawing_style().unwrap().width, 256);
            }
        }
    }

    #[test]
    fn arrow_endpoint_drags_clamp_and_reject_short_arrows() {
        let m = arrow(
            pos2(100.0, 100.0),
            pos2(200.0, 100.0),
            IMAGE,
            4.0,
            Pen::Red,
            4,
        )
        .unwrap();
        assert_eq!(
            resized(&m, Handle::End, Vec2::ZERO, IMAGE, 4.0),
            Some(m.clone())
        );
        assert!(resized(&m, Handle::End, vec2(-97.0, 0.0), IMAGE, 4.0).is_none());
        assert!(resized(&m, Handle::End, vec2(-100.0, 0.0), IMAGE, 0.0).is_none());
        let r = resized(&m, Handle::End, vec2(-96.0, 0.0), IMAGE, 4.0).unwrap();
        points_near(&r, &[pos2(100.0, 100.0), pos2(104.0, 100.0)]);
        let r = resized(&m, Handle::End, vec2(500.0, 500.0), IMAGE, 4.0).unwrap();
        points_near(&r, &[pos2(100.0, 100.0), pos2(400.0, 200.0)]);
        let r = resized(&m, Handle::Start, vec2(-500.0, -500.0), IMAGE, 4.0).unwrap();
        points_near(&r, &[pos2(0.0, 0.0), pos2(200.0, 100.0)]);
        assert!(resized(&m, Handle::SE, Vec2::ZERO, IMAGE, 4.0).is_none());
    }

    #[test]
    fn endpoint_drags_stay_in_image_coordinates_under_every_view_turn() {
        let m = arrow(
            pos2(200.0, 50.0),
            pos2(100.0, 120.0),
            IMAGE,
            4.0,
            Pen::Red,
            4,
        )
        .unwrap();
        for turns in 0..4 {
            let size = if turns % 2 == 0 {
                IMAGE
            } else {
                vec2(IMAGE.y, IMAGE.x)
            };
            let at = super::super::Placement {
                rect: Rect::from_min_size(pos2(30.0, 60.0), size * 2.0),
                size: IMAGE,
                turns,
            };
            let from = at.screen(Handle::End.on(&m));
            let to = from + whir_core::marks::turn_vec(vec2(20.0, -30.0), turns) * at.zoom();
            let r = resized(&m, Handle::End, at.pixel(to) - at.pixel(from), IMAGE, 4.0).unwrap();
            points_near(&r, &[pos2(200.0, 50.0), pos2(120.0, 90.0)]);
            for (handle, screen) in at.handles(&r) {
                assert!(at.pixel(screen).distance(handle.on(&r)) < 0.01);
            }
        }
    }

    #[test]
    fn moving_drawings_keeps_normalized_points_and_style() {
        for is_arrow in [false, true] {
            let m = Mark::drawing(
                &[pos2(50.0, 40.0), pos2(90.0, 60.0)],
                Pen::White,
                12,
                is_arrow,
            )
            .unwrap();
            let r = moved(&m, vec2(10.0, -10.0), IMAGE);
            assert_eq!(r.kind, m.kind);
            points_near(&r, &[pos2(60.0, 30.0), pos2(100.0, 50.0)]);
            let r = moved(&m, vec2(-500.0, 500.0), IMAGE);
            assert_eq!(r.kind, m.kind);
            points_near(&r, &[pos2(0.0, 180.0), pos2(40.0, 200.0)]);
        }
    }

    #[test]
    fn freehand_corners_scale_uniformly_without_scaling_the_pen() {
        let m = freehand(
            &[pos2(100.0, 50.0), pos2(140.0, 50.0), pos2(120.0, 70.0)],
            IMAGE,
            Pen::Black,
            8,
        )
        .unwrap();
        assert_eq!(handles(&m, true), Handle::CORNERS);
        assert_eq!(handles(&m, false), Handle::CORNERS);
        for (handle, opposite) in [
            (Handle::NW, Handle::SE),
            (Handle::NE, Handle::SW),
            (Handle::SE, Handle::NW),
            (Handle::SW, Handle::NE),
        ] {
            assert_eq!(resized(&m, handle, Vec2::ZERO, IMAGE, 4.0), Some(m.clone()));
            for scale in [0.5, 2.0] {
                let delta = handle.pull() * vec2(40.0, 20.0) * (scale - 1.0);
                let r = resized(&m, handle, delta, IMAGE, 4.0).unwrap();
                let fixed = opposite.on(&m);
                assert_eq!(opposite.on(&r), fixed);
                assert_eq!(r.kind, m.kind);
                let expected: Vec<_> = m
                    .drawing_points()
                    .iter()
                    .map(|p| fixed + (*p - fixed) * scale)
                    .collect();
                points_near(&r, &expected);
            }
        }
    }

    #[test]
    fn flat_freehand_strokes_shorten_from_each_corner() {
        for horizontal in [true, false] {
            let (points, along, across, expected_size) = if horizontal {
                (
                    [pos2(20.0, 30.5), pos2(100.0, 30.5), pos2(20.0, 30.5)],
                    vec2(80.0, 0.0),
                    vec2(0.0, 1.0),
                    (40, 1),
                )
            } else {
                (
                    [pos2(20.5, 30.0), pos2(20.5, 110.0), pos2(20.5, 30.0)],
                    vec2(0.0, 80.0),
                    vec2(1.0, 0.0),
                    (1, 40),
                )
            };
            let m = freehand(&points, IMAGE, Pen::Black, 8).unwrap();
            for (handle, opposite) in [
                (Handle::NW, Handle::SE),
                (Handle::NE, Handle::SW),
                (Handle::SE, Handle::NW),
                (Handle::SW, Handle::NE),
            ] {
                let fixed = opposite.on(&m);
                for perpendicular in [0.0, 30.0] {
                    let delta = handle.pull() * (along * -0.5 + across * perpendicular);
                    let r = resized(&m, handle, delta, IMAGE, 4.0).unwrap();
                    assert_eq!((r.w, r.h), expected_size);
                    assert_eq!(opposite.on(&r), fixed);
                    assert_eq!(r.kind, m.kind);
                    assert!(r.x + r.w <= IMAGE.x as u32 && r.y + r.h <= IMAGE.y as u32);
                    let expected: Vec<_> = m
                        .drawing_points()
                        .iter()
                        .map(|p| {
                            if horizontal {
                                pos2(fixed.x + (p.x - fixed.x) * 0.5, p.y)
                            } else {
                                pos2(p.x, fixed.y + (p.y - fixed.y) * 0.5)
                            }
                        })
                        .collect();
                    points_near(&r, &expected);
                }
            }
            // An earlier enlargement can make the padded axis wider than one pixel.
            let grown = resized(&m, Handle::SE, along, IMAGE, 4.0).unwrap();
            let r = resized(&grown, Handle::SE, along * -1.5, IMAGE, 4.0).unwrap();
            assert_eq!((r.w, r.h), expected_size);
            assert_eq!(Handle::NW.on(&r), Handle::NW.on(&m));
            assert_eq!(r.kind, m.kind);
        }
    }

    #[test]
    fn freehand_with_two_varying_axes_shortens_uniformly() {
        let m = freehand(
            &[pos2(100.0, 50.0), pos2(140.0, 50.0), pos2(120.0, 70.0)],
            IMAGE,
            Pen::Blue,
            12,
        )
        .unwrap();
        for (handle, opposite) in [
            (Handle::NW, Handle::SE),
            (Handle::NE, Handle::SW),
            (Handle::SE, Handle::NW),
            (Handle::SW, Handle::NE),
        ] {
            let delta = handle.pull() * vec2(-30.0, -10.0);
            let r = resized(&m, handle, delta, IMAGE, 4.0).unwrap();
            let fixed = opposite.on(&m);
            assert_eq!((r.w, r.h), (20, 10));
            assert_eq!(opposite.on(&r), fixed);
            assert_eq!(r.kind, m.kind);
            let expected: Vec<_> = m
                .drawing_points()
                .iter()
                .map(|p| fixed + (*p - fixed) * 0.5)
                .collect();
            points_near(&r, &expected);
        }
        // A one-pixel axis with actual variation still constrains uniform scaling.
        let thin = freehand(
            &[pos2(100.0, 50.0), pos2(140.0, 51.0)],
            IMAGE,
            Pen::Blue,
            12,
        )
        .unwrap();
        assert_eq!(
            resized(&thin, Handle::SE, vec2(-20.0, -0.5), IMAGE, 4.0),
            Some(thin)
        );
    }

    #[test]
    fn freehand_scaling_stops_at_image_edges_and_never_flips() {
        let m = freehand(&[pos2(100.0, 50.0), pos2(140.0, 70.0)], IMAGE, Pen::Red, 4).unwrap();
        for (handle, expected) in [
            (Handle::NW, (0, 0, 140, 70)),
            (Handle::NE, (100, 0, 140, 70)),
            (Handle::SE, (100, 50, 300, 150)),
            (Handle::SW, (0, 50, 140, 70)),
        ] {
            let r = resized(&m, handle, handle.pull() * 900.0, IMAGE, 4.0).unwrap();
            assert_eq!(sides(&r), expected);
            assert_eq!(r.kind, m.kind);
            let r = resized(&m, handle, handle.pull() * -900.0, IMAGE, 4.0).unwrap();
            assert_eq!((r.w, r.h), (2, 1));
            assert_eq!(r.kind, m.kind);
        }
    }

    #[test]
    fn freehand_dots_and_flat_strokes_keep_nondegenerate_boxes() {
        for points in [
            vec![pos2(20.0, 20.0)],
            vec![pos2(20.0, 20.0), pos2(60.0, 20.0)],
            vec![pos2(20.0, 20.0), pos2(20.0, 60.0)],
        ] {
            let m = freehand(&points, IMAGE, Pen::Red, 4).unwrap();
            for handle in Handle::CORNERS {
                let r = resized(&m, handle, handle.pull() * -900.0, IMAGE, 4.0).unwrap();
                assert_eq!((r.w, r.h), (1, 1));
                assert_eq!(r.kind, m.kind);
                assert_eq!(r.drawing_points().len(), points.len());
            }
            assert!(resized(&m, Handle::E, Vec2::ZERO, IMAGE, 4.0).is_none());
        }
    }

    #[test]
    fn drawn_marks_cover_whole_pixels() {
        let ink = Ink::Yellow;
        let m = drawn(pos2(10.4, 20.6), pos2(30.2, 25.1), IMAGE, 0.0, ink).unwrap();
        assert_eq!(sides(&m), (10, 20, 21, 6));
        // Too small a side.
        assert!(drawn(pos2(10.0, 20.0), pos2(30.0, 22.0), IMAGE, 4.0, ink).is_none());
        // Entirely on the edge: no pixels at all.
        assert!(drawn(pos2(400.0, 0.0), pos2(400.0, 50.0), IMAGE, 0.0, ink).is_none());
    }

    #[test]
    fn moving_keeps_a_mark_in_the_image() {
        let m = rect(50, 40, 100, 40);
        assert_eq!(
            sides(&moved(&m, vec2(30.4, -10.0), IMAGE)),
            (80, 30, 100, 40)
        );
        assert_eq!(
            sides(&moved(&m, vec2(-500.0, 500.0), IMAGE)),
            (0, 160, 100, 40)
        );
        // Bigger than the image: pinned to its top left.
        let big = rect(0, 0, 500, 40);
        assert_eq!(sides(&moved(&big, vec2(10.0, 0.0), IMAGE)), (0, 0, 500, 40));
    }

    #[test]
    fn each_handle_moves_its_own_sides() {
        let m = rect(50, 40, 100, 40);
        assert_eq!(resize(&m, Handle::SE, 50.0, 20.0), Some((50, 40, 150, 60)));
        assert_eq!(resize(&m, Handle::NW, -10.0, -5.0), Some((40, 35, 110, 45)));
        assert_eq!(resize(&m, Handle::NE, 10.0, 10.0), Some((50, 50, 110, 30)));
        assert_eq!(resize(&m, Handle::SW, 10.0, 10.0), Some((60, 40, 90, 50)));
        // A side handle moves one side only, whatever the drag does across.
        assert_eq!(resize(&m, Handle::E, 25.0, 99.0), Some((50, 40, 125, 40)));
        assert_eq!(resize(&m, Handle::W, -25.0, 99.0), Some((25, 40, 125, 40)));
        assert_eq!(resize(&m, Handle::N, 99.0, 10.0), Some((50, 50, 100, 30)));
        assert_eq!(resize(&m, Handle::S, 99.0, -10.0), Some((50, 40, 100, 30)));
    }

    #[test]
    fn dragged_past_the_other_side_it_turns_over() {
        let m = rect(50, 40, 100, 40);
        // The right side dragged 30 past the left.
        assert_eq!(resize(&m, Handle::E, -130.0, 0.0), Some((20, 40, 30, 40)));
        // The bottom dragged to just short of the top: no thinner than `min`.
        assert_eq!(resize(&m, Handle::S, 0.0, -39.0), Some((50, 40, 100, 4)));
        assert_eq!(resize(&m, Handle::S, 0.0, -40.0), Some((50, 40, 100, 4)));
        assert_eq!(resize(&m, Handle::S, 0.0, -42.0), Some((50, 36, 100, 4)));
    }

    #[test]
    fn a_resized_side_goes_to_the_nearest_pixel() {
        let m = rect(50, 40, 100, 40);
        assert_eq!(
            resize(&m, Handle::SE, 50.0001, 19.9999),
            Some((50, 40, 150, 60))
        );
        assert_eq!(resize(&m, Handle::NW, 0.4, -0.6), Some((50, 39, 100, 41)));
    }

    #[test]
    fn a_resize_stops_at_the_edge_of_the_image() {
        let m = rect(50, 40, 100, 40);
        assert_eq!(
            resize(&m, Handle::SE, 900.0, 900.0),
            Some((50, 40, 350, 160))
        );
        assert_eq!(
            resize(&m, Handle::NW, -900.0, -900.0),
            Some((0, 0, 150, 80))
        );
    }

    #[test]
    fn handles_sit_on_the_box() {
        let m = rect(10, 20, 100, 40);
        assert_eq!(Handle::NW.on(&m), pos2(10.0, 20.0));
        assert_eq!(Handle::N.on(&m), pos2(60.0, 20.0));
        assert_eq!(Handle::SE.on(&m), pos2(110.0, 60.0));
        assert_eq!(Handle::W.on(&m), pos2(10.0, 40.0));
        assert_eq!(handles(&m, true).len(), 8);
        assert_eq!(handles(&m, false), Handle::CORNERS);
        assert_eq!(handles(&text(0, 0, 10, 10, 20), true), Handle::CORNERS);
    }

    #[test]
    fn text_scales_from_the_opposite_corner() {
        let m = text(100, 50, 40, 20, 16);
        // Twice as wide: twice the size, anchored at the top left.
        let r = resized(&m, Handle::SE, vec2(40.0, 0.0), IMAGE, 4.0).unwrap();
        assert_eq!(r.text().unwrap().size, 32);
        assert_eq!(sides(&r), (100, 50, 80, 40));
        // From the top left: the bottom right stays put.
        let r = resized(&m, Handle::NW, vec2(-40.0, -20.0), IMAGE, 4.0).unwrap();
        assert_eq!(r.text().unwrap().size, 32);
        assert_eq!(sides(&r), (60, 30, 80, 40));
        // Whichever side grew more decides.
        let r = resized(&m, Handle::NE, vec2(0.0, -20.0), IMAGE, 4.0).unwrap();
        assert_eq!(r.text().unwrap().size, 32);
        assert_eq!((r.x, r.y + r.h), (100, 70));
        // Shrunk to nothing, or blown up: within the sizes text can have.
        let r = resized(&m, Handle::SE, vec2(-80.0, -80.0), IMAGE, 4.0).unwrap();
        assert_eq!(r.text().unwrap().size, Text::MIN_SIZE);
        let r = resized(&m, Handle::SE, vec2(9000.0, 0.0), IMAGE, 4.0).unwrap();
        assert_eq!(r.text().unwrap().size, Text::MAX_SIZE);
        // Its words are unchanged.
        assert_eq!(r.text().unwrap().body, "Hi");
    }

    #[test]
    fn a_measured_box_keeps_the_corner_that_stays() {
        let m = text(100, 50, 40, 20, 16);
        assert_eq!(anchored(&m, Handle::SE, 90, 45), (100, 50));
        assert_eq!(anchored(&m, Handle::NW, 90, 45), (50, 25));
        assert_eq!(anchored(&m, Handle::NE, 90, 45), (100, 25));
        assert_eq!(anchored(&m, Handle::SW, 90, 45), (50, 50));
        // Not past the image's top left.
        assert_eq!(anchored(&m, Handle::NW, 900, 450), (0, 0));
    }
}
