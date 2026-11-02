//! Integer-backed paths shared by the editor, thumbnails and raster baking.

use std::f32::consts::FRAC_PI_6;
use std::fmt::Write as _;

use egui::{Pos2, Stroke, pos2, vec2};

use super::{Kind, Mark, Pen};

/// A stroke in its mark's box. Normalized coordinates keep marks exactly
/// comparable and let moving/resizing change the box without rewriting points.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Drawing {
    /// Ordered coordinates in `0..=Self::COORD_MAX`, inclusive on both axes.
    /// `[0, 0]` resolves to `(mark.x, mark.y)` and `[COORD_MAX, COORD_MAX]`
    /// to `(mark.x + mark.w, mark.y + mark.h)`, in original image pixels.
    pub points: Vec<[u32; 2]>,
    pub pen: Pen,
    /// Stroke diameter in image pixels, independent of the box's dimensions.
    pub width: u32,
}

impl Drawing {
    /// The fixed coordinate denominator in `.marks` files (not an image size).
    pub const COORD_MAX: u32 = 65_535;
    pub const MIN_WIDTH: u32 = 1;
    pub const MAX_WIDTH: u32 = 256;
}

impl Mark {
    /// Make a freehand stroke (one or more points) or arrow (exactly two,
    /// tail then tip). Reject invalid widths, negative/nonfinite coordinates,
    /// overflowing boxes and arrows whose endpoints coincide after quantization.
    /// The box rounds outward, with at least one pixel on each axis; it bounds
    /// the centerline points, not the stroke thickness or arrowhead.
    pub fn drawing(points: &[Pos2], pen: Pen, width: u32, arrow: bool) -> Option<Mark> {
        if points.is_empty()
            || (arrow && points.len() != 2)
            || !(Drawing::MIN_WIDTH..=Drawing::MAX_WIDTH).contains(&width)
            || points
                .iter()
                .any(|p| !p.is_finite() || p.x < 0.0 || p.y < 0.0)
        {
            return None;
        }
        let (x, w) = bounds(points.iter().map(|p| p.x))?;
        let (y, h) = bounds(points.iter().map(|p| p.y))?;
        let normalize = |value: f32, origin: u32, extent: u32| {
            ((f64::from(value) - f64::from(origin)) / f64::from(extent)
                * f64::from(Drawing::COORD_MAX))
            .round() as u32
        };
        let mut points: Vec<_> = points
            .iter()
            .map(|p| [normalize(p.x, x, w), normalize(p.y, y, h)])
            .collect();
        // Quantization can also collapse neighboring samples. Keep later returns
        // to a previous position: they are part of the stroke, not duplicates.
        points.dedup();
        let drawing = Drawing { points, pen, width };
        let mark = Mark {
            x,
            y,
            w,
            h,
            kind: if arrow {
                Kind::Arrow(drawing)
            } else {
                Kind::Freehand(drawing)
            },
        };
        mark.valid_drawing().then_some(mark)
    }

    /// The stored stroke style, or `None` for highlights and text.
    pub fn drawing_style(&self) -> Option<&Drawing> {
        match &self.kind {
            Kind::Freehand(d) | Kind::Arrow(d) => Some(d),
            Kind::Highlight(_) | Kind::Text(_) => None,
        }
    }

    /// Resolve normalized points in the current box, in original image pixels.
    /// Non-drawings and invalid drawings have no points.
    pub fn drawing_points(&self) -> Vec<Pos2> {
        if !self.valid_drawing() {
            return Vec::new();
        }
        self.drawing_style()
            .unwrap()
            .points
            .iter()
            .map(|&p| self.resolve_drawing_point(p))
            .collect()
    }

    /// Rendered centerlines in original image pixels, shared with raster baking
    /// and hit testing. Freehand is one path (possibly a dot); an arrow is its
    /// tail-to-tip shaft followed by an open `[wing, tip, wing]` V, never a fill.
    pub fn drawing_paths(&self) -> Vec<Vec<Pos2>> {
        let points = self.drawing_points();
        match &self.kind {
            Kind::Freehand(_) if !points.is_empty() => vec![points],
            Kind::Arrow(d) => {
                let [tail, tip] = points.as_slice() else {
                    return Vec::new();
                };
                let delta = *tip - *tail;
                let length = delta.length();
                let along = delta / length;
                let across = vec2(-along.y, along.x);
                // A deterministic 60-degree V. Thicker pens get longer wings,
                // but short arrows never have a head longer than their shaft.
                let head = (8.0 + 4.0 * d.width as f32).min(length * 0.35);
                let (sin, cos) = FRAC_PI_6.sin_cos();
                let back = *tip - along * (head * cos);
                let side = across * (head * sin);
                let head = vec![back + side, *tip, back - side];
                vec![points, head]
            }
            _ => Vec::new(),
        }
    }

    /// Hit the actual rounded drawing stroke, including arrowhead wings.
    /// `tolerance` is added to its half-width, in image pixels. Other kinds
    /// retain their rectangular `contains` behavior and ignore tolerance.
    pub fn hit(&self, x: f32, y: f32, tolerance: f32) -> bool {
        let Some(drawing) = self.drawing_style() else {
            return self.contains(x, y);
        };
        let point = pos2(x, y);
        if !point.is_finite() || !tolerance.is_finite() {
            return false;
        }
        let radius = f64::from(drawing.width) * 0.5 + f64::from(tolerance);
        if radius < 0.0 {
            return false;
        }
        let radius_sq = radius * radius;
        self.drawing_paths()
            .iter()
            .any(|path| match path.as_slice() {
                [p] => distance_sq(point, *p, *p) <= radius_sq,
                _ => path
                    .windows(2)
                    .any(|pair| distance_sq(point, pair[0], pair[1]) <= radius_sq),
            })
    }

    fn resolve_drawing_point(&self, point: [u32; 2]) -> Pos2 {
        // Use doubles until the final screen-compatible value: neither integer
        // multiplication nor adding the box's far edge should overflow.
        let axis = |origin: u32, extent: u32, n: u32| {
            (f64::from(origin) + f64::from(extent) * f64::from(n) / f64::from(Drawing::COORD_MAX))
                as f32
        };
        pos2(
            axis(self.x, self.w, point[0]),
            axis(self.y, self.h, point[1]),
        )
    }

    fn valid_drawing(&self) -> bool {
        let Some(d) = self.drawing_style() else {
            return false;
        };
        if self.w == 0
            || self.h == 0
            || self.x.checked_add(self.w).is_none()
            || self.y.checked_add(self.h).is_none()
            || !(Drawing::MIN_WIDTH..=Drawing::MAX_WIDTH).contains(&d.width)
            || d.points.is_empty()
            || d.points
                .iter()
                .any(|p| p[0] > Drawing::COORD_MAX || p[1] > Drawing::COORD_MAX)
        {
            return false;
        }
        !matches!(self.kind, Kind::Arrow(_))
            || (d.points.len() == 2
                && d.points[0] != d.points[1]
                && self.resolve_drawing_point(d.points[0])
                    != self.resolve_drawing_point(d.points[1]))
    }
}

fn bounds(values: impl Iterator<Item = f32>) -> Option<(u32, u32)> {
    let (min, max) = values.fold((f32::INFINITY, f32::NEG_INFINITY), |(min, max), v| {
        (min.min(v), max.max(v))
    });
    let start = f64::from(min).floor();
    let end = f64::from(max).ceil().max(start + 1.0);
    // u32::MAX rounds UP when cast to f32; compare in f64 before casting.
    (end <= f64::from(u32::MAX)).then_some((start as u32, (end - start) as u32))
}

fn distance_sq(point: Pos2, a: Pos2, b: Pos2) -> f64 {
    let dx = f64::from(b.x) - f64::from(a.x);
    let dy = f64::from(b.y) - f64::from(a.y);
    let px = f64::from(point.x) - f64::from(a.x);
    let py = f64::from(point.y) - f64::from(a.y);
    let length_sq = dx * dx + dy * dy;
    let t = if length_sq == 0.0 {
        0.0
    } else {
        ((px * dx + py * dy) / length_sq).clamp(0.0, 1.0)
    };
    (px - t * dx).powi(2) + (py - t * dy).powi(2)
}

/// Draw shared image-space paths using `to_screen` (including any rotation),
/// with `scale` screen points per image pixel. Segment bodies plus vertex disks
/// give round caps/joins without the sharp miters of a stroked polyline.
pub fn paint_drawing(
    painter: &egui::Painter,
    mark: &Mark,
    to_screen: impl Fn(Pos2) -> Pos2,
    scale: f32,
) {
    let Some(drawing) = mark.drawing_style() else {
        return;
    };
    let width = drawing.width as f32 * scale;
    if !width.is_finite() || width <= 0.0 {
        return;
    }
    let color = drawing.pen.color();
    let stroke = Stroke::new(width, color);
    for path in mark.drawing_paths() {
        let points: Vec<_> = path.into_iter().map(&to_screen).collect();
        if points.iter().any(|p| !p.is_finite()) {
            continue;
        }
        for pair in points.windows(2) {
            painter.line_segment([pair[0], pair[1]], stroke);
        }
        for point in points {
            painter.circle_filled(point, width * 0.5, color);
        }
    }
}

/// `freehand|arrow <x> <y> <w> <h> <width> <pen> <nx> <ny> [<nx> <ny> ...]`.
/// Invalid lines are left to the caller to preserve verbatim as `other`.
pub(super) fn parse(words: &[&str]) -> Option<Mark> {
    let arrow = match *words.first()? {
        "freehand" => false,
        "arrow" => true,
        _ => return None,
    };
    if words.len() < 9 || !(words.len() - 7).is_multiple_of(2) {
        return None;
    }
    if arrow && words.len() != 11 {
        return None;
    }
    let mut points = words[7..]
        .chunks_exact(2)
        .map(|pair| Some([pair[0].parse().ok()?, pair[1].parse().ok()?]))
        .collect::<Option<Vec<[u32; 2]>>>()?;
    points.dedup();
    let drawing = Drawing {
        points,
        pen: Pen::from_name(words[6])?,
        width: words[5].parse().ok()?,
    };
    let mark = Mark {
        x: words[1].parse().ok()?,
        y: words[2].parse().ok()?,
        w: words[3].parse().ok()?,
        h: words[4].parse().ok()?,
        kind: if arrow {
            Kind::Arrow(drawing)
        } else {
            Kind::Freehand(drawing)
        },
    };
    mark.valid_drawing().then_some(mark)
}

pub(super) fn serialize(mark: &Mark, drawing: &Drawing) -> String {
    let tag = if matches!(mark.kind, Kind::Arrow(_)) {
        "arrow"
    } else {
        "freehand"
    };
    let mut line = format!(
        "{tag} {} {} {} {} {} {}",
        mark.x,
        mark.y,
        mark.w,
        mark.h,
        drawing.width,
        drawing.pen.name()
    );
    for [x, y] in &drawing.points {
        write!(line, " {x} {y}").unwrap();
    }
    line.push('\n');
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::marks::{self, Ink, MarkSet, Text};

    fn near(a: Pos2, b: Pos2, tolerance: f32) {
        assert!(a.distance(b) <= tolerance, "{a:?} != {b:?}");
    }

    #[test]
    fn boxes_round_outward_and_points_keep_their_order() {
        let a = pos2(10.25, 30.75);
        let b = pos2(40.5, 20.25);
        let mark = Mark::drawing(&[a, a, b, b, a], Pen::Blue, 3, false).unwrap();
        assert_eq!((mark.x, mark.y, mark.w, mark.h), (10, 20, 31, 11));
        let points = mark.drawing_points();
        assert_eq!(points.len(), 3);
        for (actual, expected) in points.iter().zip([a, b, a]) {
            near(*actual, expected, 0.001);
        }
        assert_eq!(mark.drawing_paths(), vec![points]);
        assert_eq!(mark.drawing_style().unwrap().pen, Pen::Blue);
        assert_eq!(mark.text(), None);
    }

    #[test]
    fn horizontal_vertical_and_dot_boxes_have_nonzero_sides() {
        for points in [
            vec![pos2(10.0, 20.0), pos2(40.0, 20.0)],
            vec![pos2(10.0, 20.0), pos2(10.0, 50.0)],
            vec![pos2(10.0, 20.0)],
        ] {
            let mark = Mark::drawing(&points, Pen::Red, 1, false).unwrap();
            assert!(mark.w >= 1 && mark.h >= 1);
            assert_eq!(mark.drawing_points(), points);
        }
        let dot = Mark::drawing(&[pos2(10.0, 20.0); 3], Pen::Red, 1, false).unwrap();
        assert_eq!((dot.w, dot.h), (1, 1));
        assert_eq!(dot.drawing_style().unwrap().points, vec![[0, 0]]);
        assert_eq!(dot.drawing_paths(), vec![vec![pos2(10.0, 20.0)]]);
    }

    #[test]
    fn normalization_uses_the_entire_closed_box_and_survives_resize() {
        let mut mark =
            Mark::drawing(&[pos2(10.0, 40.0), pos2(30.0, 20.0)], Pen::Green, 5, true).unwrap();
        assert_eq!(
            mark.drawing_style().unwrap().points,
            vec![[0, Drawing::COORD_MAX], [Drawing::COORD_MAX, 0]]
        );
        let style = mark.drawing_style().unwrap().clone();
        mark.x = 100;
        mark.y = 200;
        mark.w = 40;
        mark.h = 10;
        assert_eq!(
            mark.drawing_points(),
            [pos2(100.0, 210.0), pos2(140.0, 200.0)]
        );
        assert_eq!(mark.drawing_style(), Some(&style));
    }

    #[test]
    fn invalid_input_and_collapsed_arrows_are_rejected() {
        let a = pos2(10.0, 20.0);
        let b = pos2(30.0, 40.0);
        assert!(Mark::drawing(&[], Pen::Red, 1, false).is_none());
        for points in [vec![], vec![a], vec![a, a], vec![a, b, b], vec![a, b, a]] {
            assert!(Mark::drawing(&points, Pen::Red, 1, true).is_none());
        }
        for width in [0, Drawing::MAX_WIDTH + 1, u32::MAX] {
            assert!(Mark::drawing(&[a, b], Pen::Red, width, false).is_none());
            assert!(Mark::drawing(&[a, b], Pen::Red, width, true).is_none());
        }
        for invalid in [
            -1.0,
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::MAX,
            u32::MAX as f32,
        ] {
            for p in [pos2(invalid, 1.0), pos2(1.0, invalid)] {
                assert!(Mark::drawing(&[p], Pen::Red, 1, false).is_none());
                assert!(Mark::drawing(&[a, p], Pen::Red, 1, true).is_none());
            }
        }
        let points = [pos2(0.0, 0.0), pos2(0.000001, 0.0)];
        assert!(Mark::drawing(&points, Pen::Red, 1, true).is_none());
        let dot = Mark::drawing(&points, Pen::Red, 1, false).unwrap();
        assert_eq!(dot.drawing_points(), [pos2(0.0, 0.0)]);
    }

    #[test]
    fn drawings_round_trip_with_old_marks_and_all_pen_width_limits() {
        let mut set = MarkSet {
            size: [800, 600],
            marks: vec![
                Mark::highlight(1, 2, 3, 4, Ink::Yellow),
                Mark {
                    x: 2,
                    y: 3,
                    w: 40,
                    h: 20,
                    kind: Kind::Text(Text {
                        body: "unchanged".into(),
                        size: 12,
                        pen: Pen::Black,
                        turn: 0,
                    }),
                },
            ],
            other: vec!["future-kind 1 2 3".into()],
        };
        for pen in Pen::ALL {
            for width in [Drawing::MIN_WIDTH, 7, Drawing::MAX_WIDTH] {
                for arrow in [false, true] {
                    set.marks.push(
                        Mark::drawing(&[pos2(20.5, 80.25), pos2(3.25, 7.5)], pen, width, arrow)
                            .unwrap(),
                    );
                }
                set.marks
                    .push(Mark::drawing(&[pos2(30.0, 40.0)], pen, width, false).unwrap());
            }
        }
        let text = marks::serialize(&set);
        assert_eq!(marks::parse(&text).unwrap(), set);
        assert_eq!(marks::serialize(&marks::parse(&text).unwrap()), text);
        assert!(text.contains("freehand 30 40 1 1 256 white 0 0\n"));
        assert!(text.contains("arrow 3 7 18 74 1 red "));
    }

    #[test]
    fn explicit_format_preserves_direction_and_full_coordinate_range() {
        let text = "whir-marks 1\nsize 100 100\n\
                    freehand 1 2 3 4 1 red 0 65535 32768 32768 65535 0\n\
                    arrow 10 20 30 40 256 blue 65535 0 0 65535\n";
        let set = marks::parse(text).unwrap();
        assert_eq!(set.marks.len(), 2);
        assert!(set.other.is_empty());
        assert_eq!(marks::serialize(&set), text);
        assert_eq!(
            set.marks[1].drawing_points(),
            [pos2(40.0, 20.0), pos2(10.0, 60.0)]
        );
    }

    #[test]
    fn parsed_freehand_deduplicates_only_neighboring_points() {
        let set = marks::parse(
            "whir-marks 1\nfreehand 0 0 10 10 1 red 0 0 0 0 65535 65535 65535 65535 0 0\n",
        )
        .unwrap();
        assert_eq!(
            set.marks[0].drawing_style().unwrap().points,
            [[0, 0], [65535, 65535], [0, 0]]
        );
        assert_eq!(marks::parse(&marks::serialize(&set)).unwrap(), set);
    }

    #[test]
    fn malformed_drawings_survive_as_other_without_partial_acceptance() {
        let bad = [
            "freehand",
            "freehand 1 2 3 4 1 red",
            "freehand 1 2 3 4 1 red 0",
            "freehand 1 2 3 4 1 red 0 0 1",
            "freehand 1 2 3 4 1 red 0 0 extra",
            "freehand 1 2 3 4 0 red 0 0",
            "freehand 1 2 3 4 257 red 0 0",
            "freehand 1 2 3 4 4294967296 red 0 0",
            "freehand 1 2 3 4 1 mauve 0 0",
            "freehand 1 2 3 4 1 red -1 0",
            "freehand 1 2 3 4 1 red 0 65536",
            "freehand 1 2 3 4 1 red 65536 0",
            "freehand 1 2 3 4 1 red 4294967296 0",
            "freehand 1 2 3 4 1 red 0.5 0",
            "freehand 1 2 3 4 1 red NaN 0",
            "freehand -1 2 3 4 1 red 0 0",
            "freehand 1 2 0 4 1 red 0 0",
            "freehand 1 2 3 0 1 red 0 0",
            "freehand 4294967295 0 1 1 1 red 0 0",
            "freehand 0 4294967295 1 1 1 red 0 0",
            "freehand 1 0 4294967295 1 1 red 0 0",
            "freehand 0 1 1 4294967295 1 red 0 0",
            "freehand 4294967296 0 1 1 1 red 0 0",
            "arrow 0 0 10 10 1 red",
            "arrow 0 0 10 10 1 red 0 0",
            "arrow 0 0 10 10 1 red 0 0 0 0",
            "arrow 0 0 10 10 1 red 0 0 65535 65535 65535 65535",
            "arrow 0 0 10 10 1 red 0 0 65536 0",
            "arrow 0 0 10 10 0 red 0 0 65535 65535",
            "arrow 0 0 0 10 1 red 0 0 65535 65535",
            "arrow 0 4294967295 10 10 1 red 0 0 65535 65535",
            // Distinct normalized endpoints can still coincide as image-space f32s.
            "arrow 1000000000 1000000000 1 1 1 red 0 0 65535 65535",
        ];
        let text = format!("whir-marks 1\nsize 10 10\n{}\n", bad.join("\n"));
        let set = marks::parse(&text).unwrap();
        assert!(set.marks.is_empty());
        assert_eq!(set.other, bad);
        assert_eq!(marks::serialize(&set), text);
        assert_eq!(marks::parse(&marks::serialize(&set)).unwrap(), set);
    }

    #[test]
    fn large_valid_boxes_resolve_without_integer_overflow() {
        let text = "whir-marks 1\n\
                    arrow 0 0 4294967295 4294967295 256 red 0 0 65535 65535\n";
        let set = marks::parse(text).unwrap();
        assert_eq!(set.marks.len(), 1);
        let mark = &set.marks[0];
        let points = mark.drawing_points();
        assert_eq!(points, [Pos2::ZERO, pos2(u32::MAX as f32, u32::MAX as f32)]);
        assert!(mark.drawing_paths().iter().flatten().all(|p| p.is_finite()));
        assert!(mark.hit(1000.0, 1000.0, 0.0));
        assert!(!mark.hit(1000.0, 3000.0, 0.0));
        assert_eq!(marks::parse(&marks::serialize(&set)).unwrap(), set);
        // Unlike u32::MAX itself, this is a representable f32 below the limit.
        assert!(Mark::drawing(&[pos2(4_294_967_040.0, 0.0)], Pen::Red, 1, false).is_some());
    }

    #[test]
    fn arrowheads_follow_the_tip_in_all_directions_and_remain_symmetric() {
        for dx in [-1.0, 0.0, 1.0] {
            for dy in [-1.0, 0.0, 1.0] {
                if dx == 0.0 && dy == 0.0 {
                    continue;
                }
                for length in [1.0, 100.0, 1000.0] {
                    for width in [Drawing::MIN_WIDTH, Drawing::MAX_WIDTH] {
                        let tail = pos2(2000.0, 2000.0);
                        let tip = tail + vec2(dx, dy) * length;
                        let mark = Mark::drawing(&[tail, tip], Pen::Black, width, true).unwrap();
                        let paths = mark.drawing_paths();
                        assert_eq!(paths.len(), 2);
                        assert_eq!(paths[0], [tail, tip]);
                        assert_eq!(paths[1].len(), 3);
                        assert_eq!(paths[1][1], tip);
                        assert_eq!(mark.drawing_paths(), paths, "no jitter");
                        let along = (tip - tail).normalized();
                        let across = vec2(-along.y, along.x);
                        let left = paths[1][0] - tip;
                        let right = paths[1][2] - tip;
                        assert!(left.dot(along) < 0.0 && right.dot(along) < 0.0);
                        assert!(left.dot(across) > 0.0 && right.dot(across) < 0.0);
                        assert!((left.dot(along) - right.dot(along)).abs() < 0.001);
                        assert!((left.dot(across) + right.dot(across)).abs() < 0.001);
                        assert!((left.length() - right.length()).abs() < 0.001);
                        assert!(left.length() <= tail.distance(tip) * 0.35 + 0.001);
                        assert!(mark.hit(tip.x, tip.y, 0.0));
                    }
                }
            }
        }
    }

    #[test]
    fn reversing_an_arrow_changes_which_end_has_the_head() {
        let a = pos2(10.0, 20.0);
        let b = pos2(210.0, 20.0);
        let forward = Mark::drawing(&[a, b], Pen::Red, 4, true).unwrap();
        let reverse = Mark::drawing(&[b, a], Pen::Red, 4, true).unwrap();
        assert_eq!(
            (forward.x, forward.y, forward.w, forward.h),
            (reverse.x, reverse.y, reverse.w, reverse.h)
        );
        assert_ne!(forward, reverse);
        let head = forward.drawing_paths()[1][0];
        assert!(forward.hit(head.x, head.y, 0.0));
        assert!(!reverse.hit(head.x, head.y, 0.0));
        assert_eq!(reverse.drawing_paths()[1][1], a);
    }

    #[test]
    fn arrowhead_size_grows_with_width_and_is_capped_by_length() {
        let wing_length = |width, length| {
            let mark =
                Mark::drawing(&[pos2(0.0, 0.0), pos2(length, 0.0)], Pen::Red, width, true).unwrap();
            let head = &mark.drawing_paths()[1];
            head[0].distance(head[1])
        };
        assert!(wing_length(10, 1000.0) > wing_length(1, 1000.0));
        assert!((wing_length(256, 1.0) - 0.35).abs() < 0.0001);
    }

    #[test]
    fn hitting_a_drawing_uses_rounded_strokes_not_its_empty_box() {
        let mark = Mark::drawing(
            &[pos2(10.0, 20.0), pos2(110.0, 20.0), pos2(110.0, 120.0)],
            Pen::Blue,
            4,
            false,
        )
        .unwrap();
        assert!(mark.contains(60.0, 20.0));
        assert!(mark.hit(60.0, 22.0, 0.0));
        assert!(!mark.hit(60.0, 22.1, 0.0));
        assert!(mark.hit(60.0, 24.0, 2.0));
        assert!(!mark.hit(60.0, 24.1, 2.0));
        assert!(mark.hit(8.0, 20.0, 0.0), "round end cap outside box");
        assert!(!mark.hit(7.9, 20.0, 0.0));
        assert!(mark.hit(111.0, 19.0, 0.0), "round join");
        assert!(!mark.hit(111.9, 18.1, 0.0), "not a square or miter join");
        assert!(!mark.contains(30.0, 100.0), "empty part of bounding box");
        assert!(!mark.hit(30.0, 100.0, 2.0));
        assert!(!mark.hit(f32::NAN, 20.0, 0.0));
        assert!(!mark.hit(60.0, 20.0, f32::INFINITY));
        assert!(!mark.hit(f32::MAX, f32::MAX, 0.0));
    }

    #[test]
    fn arrowhead_wings_are_hittable_outside_the_shaft_box() {
        let mark =
            Mark::drawing(&[pos2(10.0, 50.0), pos2(210.0, 50.0)], Pen::Red, 4, true).unwrap();
        let paths = mark.drawing_paths();
        for wing in [paths[1][0], paths[1][2]] {
            assert!((wing.y - 50.0).abs() > mark.h as f32);
            assert!(mark.contains(wing.x, wing.y));
            let midpoint = wing.lerp(paths[1][1], 0.5);
            assert!(mark.hit(midpoint.x, midpoint.y, 0.0));
        }
        assert!(!mark.hit(190.0, 70.0, 0.0));
        assert!(!mark.hit(214.1, 50.0, 2.0));
    }

    #[test]
    fn a_freehand_dot_hits_a_disk_and_respects_tolerance() {
        let mark = Mark::drawing(&[pos2(12.0, 34.0)], Pen::White, 6, false).unwrap();
        assert!(mark.contains(12.0, 34.0));
        assert!(mark.hit(15.0, 34.0, 0.0));
        assert!(!mark.hit(15.01, 34.0, 0.0));
        assert!(!mark.hit(15.0, 37.0, 0.0));
        assert!(mark.hit(17.0, 34.0, 2.0));
        assert!(!mark.hit(17.01, 34.0, 2.0));
    }

    #[test]
    fn old_kinds_still_hit_only_their_rectangles() {
        let highlight = Mark::highlight(10, 20, 5, 5, Ink::Blue);
        let mut text = highlight.clone();
        text.kind = Kind::Text(Text {
            body: "hello".into(),
            size: 12,
            pen: Pen::Red,
            turn: 0,
        });
        for mark in [highlight, text] {
            assert!(mark.hit(10.0, 20.0, 0.0));
            assert!(mark.hit(14.9, 24.9, f32::NAN));
            assert!(!mark.hit(15.0, 22.0, 100.0));
            assert!(!mark.hit(9.9, 22.0, 100.0));
            assert!(mark.drawing_points().is_empty());
            assert!(mark.drawing_paths().is_empty());
            assert!(mark.drawing_style().is_none());
        }
    }

    #[test]
    fn painting_maps_the_shared_paths_with_scaled_round_caps_and_joins() {
        let mark =
            Mark::drawing(&[pos2(10.0, 20.0), pos2(50.0, 60.0)], Pen::Green, 3, true).unwrap();
        let to_screen = |p: Pos2| pos2(200.0 - p.y * 2.0, 100.0 + p.x * 2.0);
        let ctx = egui::Context::default();
        let output = ctx.run_ui(Default::default(), |ui| {
            paint_drawing(&ui.ctx().debug_painter(), &mark, to_screen, 2.0);
        });
        let paths = mark.drawing_paths();
        let mut shapes = output.shapes.iter();
        for path in paths {
            for pair in path.windows(2) {
                let egui::Shape::LineSegment { points, stroke } = &shapes.next().unwrap().shape
                else {
                    panic!("expected a segment body");
                };
                assert_eq!(*points, [to_screen(pair[0]), to_screen(pair[1])]);
                assert_eq!(stroke.width, 6.0);
                assert_eq!(stroke.color, Pen::Green.color());
            }
            for point in path {
                let egui::Shape::Circle(circle) = &shapes.next().unwrap().shape else {
                    panic!("expected a rounded cap/join");
                };
                assert_eq!(circle.center, to_screen(point));
                assert_eq!(circle.radius, 3.0);
                assert_eq!(circle.fill, Pen::Green.color());
            }
        }
        assert!(shapes.next().is_none());
        output.drop_without_applying_deltas();
    }

    #[test]
    fn painting_a_dot_emits_one_circle_and_invalid_scales_emit_nothing() {
        let mark = Mark::drawing(&[pos2(12.0, 34.0)], Pen::White, 6, false).unwrap();
        let ctx = egui::Context::default();
        let output = ctx.run_ui(Default::default(), |ui| {
            paint_drawing(&ui.ctx().debug_painter(), &mark, |p| p, 0.5);
        });
        assert_eq!(output.shapes.len(), 1);
        let egui::Shape::Circle(circle) = &output.shapes[0].shape else {
            panic!("expected a dot");
        };
        assert_eq!(circle.center, pos2(12.0, 34.0));
        assert_eq!(circle.radius, 1.5);
        output.drop_without_applying_deltas();
        for scale in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            let output = ctx.run_ui(Default::default(), |ui| {
                paint_drawing(&ui.ctx().debug_painter(), &mark, |p| p, scale);
            });
            assert!(output.shapes.is_empty());
            output.drop_without_applying_deltas();
        }
    }
}
