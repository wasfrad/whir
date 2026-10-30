//! Where the dock sits: flush against one edge of a monitor's work area,
//! somewhere along it, with only a grip showing until it slides out; and
//! where its panel (todos, the time drawer) goes beside it.

use crate::geom::{Point, Rect};
use crate::monitors::Monitor;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

impl Edge {
    pub const ALL: [Edge; 4] = [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom];

    /// The name used in `dock.ini`.
    pub fn name(self) -> &'static str {
        match self {
            Edge::Left => "left",
            Edge::Right => "right",
            Edge::Top => "top",
            Edge::Bottom => "bottom",
        }
    }

    pub fn from_name(name: &str) -> Option<Edge> {
        Edge::ALL.into_iter().find(|e| e.name() == name)
    }

    /// On a side edge the dock runs up and down.
    pub fn is_side(self) -> bool {
        matches!(self, Edge::Left | Edge::Right)
    }
}

/// The dock's measurements in pixels, at one display scale.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    /// How far the open panel reaches in from the edge.
    pub depth: i32,
    /// Its extent along the edge.
    pub length: i32,
    /// The strip on its inner side that stays out when it is closed.
    pub grip: i32,
    pub button: i32,
    pub pad: i32,
    /// The last button (the ▾ that opens the panel) is this long.
    pub chevron: i32,
    /// The panel's width.
    pub panel: i32,
}

/// Capture, Delay, the timer, and the ▾ that opens the time drawer.
pub const BUTTONS: usize = 4;

pub fn layout(dpi: u32) -> Layout {
    let px = |v: i32| (v * dpi.max(48) as i32 + 48) / 96;
    let (button, pad, grip, chevron) = (px(34), px(4), px(8), px(20));
    Layout {
        depth: button + 2 * pad + grip,
        length: (BUTTONS as i32 - 1) * (button + pad) + chevron + 2 * pad,
        grip,
        button,
        pad,
        chevron,
        panel: px(300),
    }
}

impl Layout {
    /// The whole panel, as (width, height) on `edge`.
    pub fn size(&self, edge: Edge) -> (i32, i32) {
        if edge.is_side() {
            (self.depth, self.length)
        } else {
            (self.length, self.depth)
        }
    }

    /// The grip strip inside the panel: on the side away from the edge.
    pub fn grip_rect(&self, edge: Edge) -> Rect {
        let (w, h) = self.size(edge);
        match edge {
            Edge::Left => Rect::new(w - self.grip, 0, w, h),
            Edge::Right => Rect::new(0, 0, self.grip, h),
            Edge::Top => Rect::new(0, h - self.grip, w, h),
            Edge::Bottom => Rect::new(0, 0, w, self.grip),
        }
    }

    /// How long button `i` is, along the strip.
    fn extent(&self, i: usize) -> i32 {
        if i + 1 == BUTTONS {
            self.chevron
        } else {
            self.button
        }
    }

    /// Button `i` inside the panel.
    pub fn button_rect(&self, edge: Edge, i: usize) -> Rect {
        let along = self.pad + (0..i).map(|j| self.extent(j) + self.pad).sum::<i32>();
        // Past the grip when the grip comes first.
        let across = self.pad
            + if matches!(edge, Edge::Right | Edge::Bottom) {
                self.grip
            } else {
                0
            };
        let (x, y, w, h) = if edge.is_side() {
            (across, along, self.button, self.extent(i))
        } else {
            (along, across, self.extent(i), self.button)
        };
        Rect::new(x, y, x + w, y + h)
    }
}

/// Where the panel beside the strip goes (the strip open at `anchor`), for a
/// panel of `size` (width, height). Below the strip, or above it on the
/// bottom edge or when there is more room above; flush with the screen edge
/// on the sides, lined up with the strip on the top and bottom. It is cut to
/// the room there is. Also says whether it is below.
pub fn panel_rect(work: Rect, edge: Edge, anchor: Rect, size: (i32, i32)) -> (Rect, bool) {
    let (w, h) = size;
    let (below_room, above_room) = (work.bottom - anchor.bottom, anchor.top - work.top);
    let below = match edge {
        Edge::Top => true,
        Edge::Bottom => false,
        Edge::Left | Edge::Right => below_room >= h || below_room >= above_room,
    };
    let h = h.min(if below { below_room } else { above_room }).max(0);
    let x = match edge {
        Edge::Left => work.left,
        Edge::Right => work.right - w,
        Edge::Top | Edge::Bottom => anchor.left.min(work.right - w).max(work.left),
    };
    let y = if below { anchor.bottom } else { anchor.top - h };
    (Rect::new(x, y, x + w, y + h), below)
}

/// Where the open panel is on screen: against `edge` of `work`, `offset`
/// thousandths of the way along it.
pub fn anchor(work: Rect, edge: Edge, offset: u16, l: &Layout) -> Rect {
    let (w, h) = l.size(edge);
    let along = |from: i32, to: i32, len: i32| {
        let room = (to - from - len).max(0) as i64;
        from + (room * offset.min(1000) as i64 / 1000) as i32
    };
    match edge {
        Edge::Left | Edge::Right => {
            let y = along(work.top, work.bottom, h);
            let x = if edge == Edge::Left {
                work.left
            } else {
                work.right - w
            };
            Rect::new(x, y, x + w, y + h)
        }
        Edge::Top | Edge::Bottom => {
            let x = along(work.left, work.right, w);
            let y = if edge == Edge::Top {
                work.top
            } else {
                work.bottom - h
            };
            Rect::new(x, y, x + w, y + h)
        }
    }
}

/// With `visible` pixels of its depth out: the window's rectangle on screen,
/// and which corner of the panel's picture goes at its top-left. The panel
/// slides out of the edge, so it never hangs over onto another monitor.
pub fn viewport(edge: Edge, anchor: Rect, visible: i32) -> (Rect, Point) {
    let a = anchor;
    let depth = if edge.is_side() {
        a.width()
    } else {
        a.height()
    };
    let v = visible.clamp(0, depth);
    match edge {
        Edge::Left => (
            Rect::new(a.left, a.top, a.left + v, a.bottom),
            (depth - v, 0),
        ),
        Edge::Right => (Rect::new(a.right - v, a.top, a.right, a.bottom), (0, 0)),
        Edge::Top => (Rect::new(a.left, a.top, a.right, a.top + v), (0, depth - v)),
        Edge::Bottom => (Rect::new(a.left, a.bottom - v, a.right, a.bottom), (0, 0)),
    }
}

/// The edge of `monitors[index]` has no other monitor beyond it, so the
/// pointer stops there.
pub fn is_outer_edge(monitors: &[Monitor], index: usize, edge: Edge) -> bool {
    let me = monitors[index].rect;
    !monitors.iter().enumerate().any(|(i, m)| {
        let o = m.rect;
        let beside = o.top < me.bottom && o.bottom > me.top;
        let stacked = o.left < me.right && o.right > me.left;
        i != index
            && match edge {
                Edge::Left => o.right == me.left && beside,
                Edge::Right => o.left == me.right && beside,
                Edge::Top => o.bottom == me.top && stacked,
                Edge::Bottom => o.top == me.bottom && stacked,
            }
    })
}

/// Where a new dock goes: the primary monitor's left edge if the pointer
/// stops there, else the first of its edges where it does; halfway along.
pub fn default_spot(monitors: &[Monitor]) -> (usize, Edge, u16) {
    let primary = monitors.iter().position(|m| m.primary).unwrap_or(0);
    let edge = [Edge::Left, Edge::Top, Edge::Right, Edge::Bottom]
        .into_iter()
        .find(|&e| monitors.is_empty() || is_outer_edge(monitors, primary, e))
        .unwrap_or(Edge::Left);
    (primary, edge, 500)
}

/// Where the dock goes when dropped with the pointer at `at`: the nearest
/// edge of the monitor the pointer is on.
pub fn snap(
    at: Point,
    monitors: &[Monitor],
    length: impl Fn(u32) -> i32,
) -> Option<(usize, Edge, u16)> {
    let distance = |r: Rect| {
        let (cx, cy) = r.clamp(at);
        (cx - at.0).abs() + (cy - at.1).abs()
    };
    let (index, m) = monitors
        .iter()
        .enumerate()
        .min_by_key(|(_, m)| distance(m.rect))?;
    let w = m.work;
    let edge = Edge::ALL.into_iter().min_by_key(|e| match e {
        Edge::Left => (at.0 - w.left).abs(),
        Edge::Right => (w.right - at.0).abs(),
        Edge::Top => (at.1 - w.top).abs(),
        Edge::Bottom => (w.bottom - at.1).abs(),
    })?;
    // The pointer holds the dock by its middle.
    let len = length(m.dpi);
    let (from, to, p) = if edge.is_side() {
        (w.top, w.bottom, at.1)
    } else {
        (w.left, w.right, at.0)
    };
    let room = (to - from - len).max(1);
    let offset = ((p - len / 2 - from).clamp(0, room) as i64 * 1000 / room as i64) as u16;
    Some((index, edge, offset))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(rect: Rect, primary: bool) -> Monitor {
        Monitor {
            rect,
            // A taskbar along the bottom.
            work: Rect::new(rect.left, rect.top, rect.right, rect.bottom - 48),
            dpi: 96,
            device: String::new(),
            primary,
        }
    }

    /// A portrait monitor left of the primary, another to its right.
    fn three() -> Vec<Monitor> {
        vec![
            monitor(Rect::new(0, 0, 2560, 1440), true),
            monitor(Rect::new(2560, 0, 5120, 1440), false),
            monitor(Rect::new(-1440, 0, 0, 2560), false),
        ]
    }

    #[test]
    fn edge_names_round_trip() {
        for e in Edge::ALL {
            assert_eq!(Edge::from_name(e.name()), Some(e));
        }
        assert_eq!(Edge::from_name("middle"), None);
    }

    #[test]
    fn the_layout_scales_and_buttons_fit_beside_the_grip() {
        let l = layout(96);
        assert_eq!((l.depth, l.length, l.grip), (50, 142, 8));
        assert_eq!(layout(192).depth, 100);
        assert_eq!(layout(192).length, 284);
        for edge in Edge::ALL {
            let (w, h) = l.size(edge);
            let panel = Rect::new(0, 0, w, h);
            let grip = l.grip_rect(edge);
            assert_eq!(grip.intersect(panel), grip);
            for i in 0..BUTTONS {
                let b = l.button_rect(edge, i);
                assert_eq!(b.intersect(panel), b, "{edge:?} button {i}");
                assert!(b.intersect(grip).is_empty(), "{edge:?} button {i}");
                let along = if i + 1 == BUTTONS {
                    l.chevron
                } else {
                    l.button
                };
                let size = if edge.is_side() {
                    (l.button, along)
                } else {
                    (along, l.button)
                };
                assert_eq!((b.width(), b.height()), size, "{edge:?} button {i}");
                for j in i + 1..BUTTONS {
                    assert!(b.intersect(l.button_rect(edge, j)).is_empty());
                }
            }
            // The last one ends a pad short of the end.
            let last = l.button_rect(edge, BUTTONS - 1);
            let end = if edge.is_side() {
                last.bottom
            } else {
                last.right
            };
            assert_eq!(end + l.pad, l.length, "{edge:?}");
        }
    }

    #[test]
    fn the_panel_hugs_its_edge_and_stays_in_the_work_area() {
        let work = Rect::new(0, 0, 2560, 1392);
        let l = layout(96);
        assert_eq!(
            anchor(work, Edge::Left, 0, &l),
            Rect::new(0, 0, l.depth, l.length)
        );
        assert_eq!(anchor(work, Edge::Left, 1000, &l).bottom, 1392);
        assert_eq!(anchor(work, Edge::Right, 500, &l).right, 2560);
        assert_eq!(anchor(work, Edge::Bottom, 2000, &l).right, 2560);
        assert_eq!(anchor(work, Edge::Bottom, 500, &l).bottom, 1392);
        for edge in Edge::ALL {
            for offset in [0, 333, 1000] {
                let a = anchor(work, edge, offset, &l);
                assert_eq!(a.intersect(work), a, "{edge:?} {offset}");
            }
        }
    }

    #[test]
    fn closed_it_shows_the_grip_and_open_the_whole_panel() {
        let l = layout(96);
        let work = Rect::new(-1440, 0, 0, 2512);
        for edge in Edge::ALL {
            let a = anchor(work, edge, 500, &l);
            let (open, src) = viewport(edge, a, l.depth);
            assert_eq!((open, src), (a, (0, 0)), "{edge:?}");

            let (closed, src) = viewport(edge, a, l.grip);
            // The part of the picture on screen is exactly the grip strip.
            let shown = Rect::new(
                src.0,
                src.1,
                src.0 + closed.width(),
                src.1 + closed.height(),
            );
            assert_eq!(shown, l.grip_rect(edge), "{edge:?}");
            // And it is on the edge, inside the work area.
            assert_eq!(closed.intersect(work), closed, "{edge:?}");
            match edge {
                Edge::Left => assert_eq!(closed.left, work.left),
                Edge::Right => assert_eq!(closed.right, work.right),
                Edge::Top => assert_eq!(closed.top, work.top),
                Edge::Bottom => assert_eq!(closed.bottom, work.bottom),
            }
            assert!(viewport(edge, a, 0).0.is_empty());
        }
    }

    #[test]
    fn the_panel_sits_beside_the_strip_inside_the_work_area() {
        let work = Rect::new(0, 0, 2560, 1392);
        let l = layout(96);
        let size = (l.panel, 400);
        for edge in Edge::ALL {
            for offset in [0, 500, 1000] {
                let a = anchor(work, edge, offset, &l);
                let (p, below) = panel_rect(work, edge, a, size);
                assert_eq!(p.intersect(work), p, "{edge:?} {offset}");
                assert!(p.intersect(a).is_empty(), "{edge:?} {offset} overlaps");
                // Touching the strip.
                if below {
                    assert_eq!(p.top, a.bottom, "{edge:?} {offset}");
                } else {
                    assert_eq!(p.bottom, a.top, "{edge:?} {offset}");
                }
                assert_eq!(p.width(), l.panel);
                match edge {
                    Edge::Left => assert_eq!(p.left, work.left),
                    Edge::Right => assert_eq!(p.right, work.right),
                    Edge::Top => assert!(below),
                    Edge::Bottom => assert!(!below),
                }
            }
        }
        // Low on a side edge: above the strip.
        let a = anchor(work, Edge::Left, 1000, &l);
        assert!(!panel_rect(work, Edge::Left, a, size).1);
        // Too tall for the room: cut to it.
        let a = anchor(work, Edge::Top, 500, &l);
        let (p, _) = panel_rect(work, Edge::Top, a, (l.panel, 5000));
        assert_eq!(p.bottom, work.bottom);
        // At the right end of the top edge: pulled in to stay on screen.
        let a = anchor(work, Edge::Top, 1000, &l);
        assert_eq!(panel_rect(work, Edge::Top, a, size).0.right, work.right);
    }

    #[test]
    fn outer_edges_are_the_ones_with_nothing_beyond() {
        let m = three();
        // The primary sits between the other two.
        assert!(!is_outer_edge(&m, 0, Edge::Left));
        assert!(!is_outer_edge(&m, 0, Edge::Right));
        assert!(is_outer_edge(&m, 0, Edge::Top));
        assert!(is_outer_edge(&m, 0, Edge::Bottom));
        assert!(is_outer_edge(&m, 2, Edge::Left));
        assert!(is_outer_edge(&m, 1, Edge::Right));
        assert_eq!(default_spot(&m), (0, Edge::Top, 500));
        // One monitor: the left edge.
        assert_eq!(default_spot(&m[..1]), (0, Edge::Left, 500));
        assert_eq!(default_spot(&[]), (0, Edge::Left, 500));
    }

    #[test]
    fn dropping_snaps_to_the_nearest_edge_of_that_monitor() {
        let m = three();
        let len = |dpi| layout(dpi).length;
        // Near the top of the right-hand monitor.
        let (i, edge, offset) = snap((3000, 30), &m, len).unwrap();
        assert_eq!((i, edge), (1, Edge::Top));
        let a = anchor(m[1].work, edge, offset, &layout(96));
        assert!(a.contains((3000, 10)), "{a:?}");
        // Far down the portrait monitor's left edge.
        let (i, edge, offset) = snap((-1430, 2000), &m, len).unwrap();
        assert_eq!((i, edge), (2, Edge::Left));
        assert!(anchor(m[2].work, edge, offset, &layout(96)).contains((-1440, 2000)));
        // Past the end of an edge: as far along as it goes.
        assert_eq!(snap((2550, 5000), &m, len).unwrap().2, 1000);
        // Off every monitor: the nearest one.
        assert_eq!(snap((9000, 700), &m, len).unwrap().0, 1);
        assert_eq!(snap((0, 0), &[], len), None);
    }
}
