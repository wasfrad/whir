//! Integer rectangles, in physical pixels.

pub type Point = (i32, i32);

/// Half-open: `right` and `bottom` are just past the last pixel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    /// The rectangle with `a` and `b` as opposite corners, in any order.
    pub fn from_points(a: Point, b: Point) -> Self {
        Self::new(a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1))
    }

    pub fn width(&self) -> i32 {
        (self.right - self.left).max(0)
    }

    pub fn height(&self) -> i32 {
        (self.bottom - self.top).max(0)
    }

    pub fn is_empty(&self) -> bool {
        self.right <= self.left || self.bottom <= self.top
    }

    pub fn contains(&self, p: Point) -> bool {
        p.0 >= self.left && p.0 < self.right && p.1 >= self.top && p.1 < self.bottom
    }

    /// The part in both (empty if they don't overlap).
    pub fn intersect(&self, o: Rect) -> Rect {
        let r = Rect::new(
            self.left.max(o.left),
            self.top.max(o.top),
            self.right.min(o.right),
            self.bottom.min(o.bottom),
        );
        if r.is_empty() { Rect::default() } else { r }
    }

    /// The smallest rectangle holding both; an empty one doesn't count.
    pub fn union(&self, o: Rect) -> Rect {
        match (self.is_empty(), o.is_empty()) {
            (true, _) => o,
            (_, true) => *self,
            _ => Rect::new(
                self.left.min(o.left),
                self.top.min(o.top),
                self.right.max(o.right),
                self.bottom.max(o.bottom),
            ),
        }
    }

    pub fn translate(&self, dx: i32, dy: i32) -> Rect {
        Rect::new(
            self.left + dx,
            self.top + dy,
            self.right + dx,
            self.bottom + dy,
        )
    }

    /// `p` moved to the nearest point inside (or on the far edges of) the rectangle.
    pub fn clamp(&self, p: Point) -> Point {
        (
            p.0.clamp(self.left, self.right.max(self.left)),
            p.1.clamp(self.top, self.bottom.max(self.top)),
        )
    }
}

/// `a` with `b` cut out of it: up to four rectangles that don't overlap.
pub fn subtract(a: Rect, b: Rect) -> ([Rect; 4], usize) {
    let mut out = [Rect::default(); 4];
    let mut n = 0;
    let mut push = |r: Rect| {
        if !r.is_empty() {
            out[n] = r;
            n += 1;
        }
    };
    let i = a.intersect(b);
    if i.is_empty() {
        push(a);
    } else {
        push(Rect::new(a.left, a.top, a.right, i.top));
        push(Rect::new(a.left, i.bottom, a.right, a.bottom));
        push(Rect::new(a.left, i.top, i.left, i.bottom));
        push(Rect::new(i.right, i.top, a.right, i.bottom));
    }
    (out, n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(r: Rect) -> i64 {
        r.width() as i64 * r.height() as i64
    }

    #[test]
    fn corners_in_any_order() {
        let r = Rect::from_points((30, 5), (10, 20));
        assert_eq!(r, Rect::new(10, 5, 30, 20));
        assert_eq!((r.width(), r.height()), (20, 15));
        assert!(r.contains((10, 5)));
        assert!(r.contains((29, 19)));
        assert!(!r.contains((30, 19)));
        assert!(!r.contains((29, 20)));
    }

    #[test]
    fn intersect_union_and_clamp() {
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(5, 5, 20, 8);
        assert_eq!(a.intersect(b), Rect::new(5, 5, 10, 8));
        assert!(a.intersect(Rect::new(10, 0, 20, 10)).is_empty());
        assert_eq!(a.union(b), Rect::new(0, 0, 20, 10));
        assert_eq!(a.union(Rect::default()), a);
        assert_eq!(Rect::default().union(b), b);
        assert_eq!(a.clamp((-5, 4)), (0, 4));
        assert_eq!(a.clamp((50, 50)), (10, 10));
    }

    #[test]
    fn subtracting_leaves_pieces_that_tile_the_rest() {
        let a = Rect::new(0, 0, 100, 80);
        let cuts = [
            Rect::new(20, 10, 50, 40),     // inside
            Rect::new(-10, -10, 30, 30),   // a corner
            Rect::new(40, -5, 60, 200),    // a band right through
            Rect::new(-1, -1, 101, 81),    // everything
            Rect::new(200, 200, 300, 300), // nothing
        ];
        for b in cuts {
            let (pieces, n) = subtract(a, b);
            let pieces = &pieces[..n];
            let total: i64 = pieces.iter().map(|&r| area(r)).sum();
            assert_eq!(total, area(a) - area(a.intersect(b)), "{b:?}");
            for (i, p) in pieces.iter().enumerate() {
                assert!(p.intersect(b).is_empty(), "{p:?} overlaps the cut {b:?}");
                assert_eq!(p.intersect(a), *p, "{p:?} leaves {a:?}");
                for q in &pieces[i + 1..] {
                    assert!(p.intersect(*q).is_empty(), "{p:?} overlaps {q:?}");
                }
            }
        }
    }
}
