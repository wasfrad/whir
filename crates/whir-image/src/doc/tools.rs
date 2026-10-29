//! The geometry of marking: drawing a box, moving one, and resizing one by
//! its handles. Positions are in image pixels (before any on-screen turn).

use egui::{Pos2, Rect, Vec2, pos2, vec2};
use whir_core::marks::{Ink, Kind, Mark, Text};

/// A drawn mark is at least this many points a side (smaller is a slip).
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

/// A grip on a selected mark's box: a corner, or the middle of a side.
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
        }
    }

    /// Where it is on `mark`'s box.
    pub fn on(self, mark: &Mark) -> Pos2 {
        let half = vec2(mark.w as f32, mark.h as f32) / 2.0;
        pos2(mark.x as f32, mark.y as f32) + half + self.pull() * half
    }
}

/// The handles `mark` is resized by: the corners of text (which scales), all
/// eight around a box, or only its corners when `edges` is false (it is too
/// small on screen to tell them apart).
pub fn handles(mark: &Mark, edges: bool) -> &'static [Handle] {
    match mark.kind {
        Kind::Highlight(_) if edges => &Handle::ALL,
        _ => &Handle::CORNERS,
    }
}

/// `mark` with `handle` dragged by `delta` pixels. A box's sides follow
/// (past the opposite side it turns over), at least `min` pixels and inside
/// the image. Text scales evenly from the opposite corner; its box here is
/// only an estimate, for the caller to measure (see [`anchored`]). `None` if
/// nothing of it would be left in the image.
pub fn resized(mark: &Mark, handle: Handle, delta: Vec2, image: Vec2, min: f32) -> Option<Mark> {
    let pull = handle.pull();
    match &mark.kind {
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
    use whir_core::marks::Pen;

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
