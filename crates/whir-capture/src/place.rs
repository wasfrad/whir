//! Where the dock sits: flush against one edge of a monitor's work area,
//! somewhere along it, with only a grip showing until it slides out; where
//! things are on its bar; and where its panel (todos, today's times, the
//! project picker) goes beside it.

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

/// What the bar holds, left to right.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    Capture,
    Delay,
    /// Play or pause the stopwatch.
    Play,
    /// Today's time on the project (only to read).
    Time,
    /// The customer over the project: opens the picker.
    Project,
    /// The ⌄ that opens today's times.
    More,
}

impl Slot {
    pub const ALL: [Slot; 6] = [
        Slot::Capture,
        Slot::Delay,
        Slot::Play,
        Slot::Time,
        Slot::Project,
        Slot::More,
    ];
}

/// The dock's measurements in pixels, at one display scale. The bar is laid
/// out left to right on every edge; on a side edge it lies along the screen
/// edge and slides out sideways.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    /// The bar, without its grip: (width, height).
    pub bar: (i32, i32),
    /// The strip on its inner side that stays out when it is closed.
    pub grip: i32,
    /// A square button's side.
    pub button: i32,
    /// Around the row of buttons.
    pub pad: i32,
    /// Between one thing and the next.
    pub space: i32,
    /// Between the capture buttons and the stopwatch (a hairline in it).
    pub gap: i32,
    pub time: i32,
    pub project: i32,
    pub more: i32,
    /// The panel's width: the bar's, so they line up.
    pub panel: i32,
    /// Between the strip and the panel.
    pub panel_gap: i32,
}

pub fn layout(dpi: u32) -> Layout {
    let px = |v: i32| (v * dpi.max(48) as i32 + 48) / 96;
    let mut l = Layout {
        bar: (0, 0),
        grip: px(8),
        button: px(28),
        pad: px(6),
        space: px(2),
        gap: px(9),
        time: px(66),
        project: px(172),
        more: px(24),
        panel: 0,
        panel_gap: px(6),
    };
    let (x, w) = l.slot_at(Slot::More);
    l.bar = (x + w + l.pad, l.button + 2 * l.pad);
    l.panel = l.bar.0;
    l
}

impl Layout {
    /// How far along the bar a slot starts, and how long it is.
    fn slot_at(&self, slot: Slot) -> (i32, i32) {
        let extent = |s: Slot| match s {
            Slot::Capture | Slot::Delay | Slot::Play => self.button,
            Slot::Time => self.time,
            Slot::Project => self.project,
            Slot::More => self.more,
        };
        let mut x = self.pad;
        for s in Slot::ALL {
            if s == slot {
                break;
            }
            x += extent(s)
                + if s == Slot::Delay {
                    self.gap
                } else {
                    self.space
                };
        }
        (x, extent(slot))
    }

    /// The whole dock, as (width, height) on `edge`.
    pub fn size(&self, edge: Edge) -> (i32, i32) {
        let (w, h) = self.bar;
        if edge.is_side() {
            (w + self.grip, h)
        } else {
            (w, h + self.grip)
        }
    }

    /// How far it reaches in from `edge`, open.
    pub fn depth(&self, edge: Edge) -> i32 {
        let (w, h) = self.size(edge);
        if edge.is_side() { w } else { h }
    }

    /// Its extent along `edge`.
    pub fn length(&self, edge: Edge) -> i32 {
        let (w, h) = self.size(edge);
        if edge.is_side() { h } else { w }
    }

    /// The grip strip: on the side away from the edge.
    pub fn grip_rect(&self, edge: Edge) -> Rect {
        let (w, h) = self.size(edge);
        match edge {
            Edge::Left => Rect::new(w - self.grip, 0, w, h),
            Edge::Right => Rect::new(0, 0, self.grip, h),
            Edge::Top => Rect::new(0, h - self.grip, w, h),
            Edge::Bottom => Rect::new(0, 0, w, self.grip),
        }
    }

    /// The bar: all of the dock but the grip.
    pub fn bar_rect(&self, edge: Edge) -> Rect {
        let (x, y) = match edge {
            Edge::Right => (self.grip, 0),
            Edge::Bottom => (0, self.grip),
            Edge::Left | Edge::Top => (0, 0),
        };
        Rect::new(x, y, x + self.bar.0, y + self.bar.1)
    }

    /// Where `slot` is in the dock.
    pub fn slot_rect(&self, edge: Edge, slot: Slot) -> Rect {
        let bar = self.bar_rect(edge);
        let (x, w) = self.slot_at(slot);
        let top = bar.top + self.pad;
        Rect::new(bar.left + x, top, bar.left + x + w, top + self.button)
    }

    /// The hairline between the capture buttons and the stopwatch.
    pub fn divider(&self, edge: Edge) -> Rect {
        let delay = self.slot_rect(edge, Slot::Delay);
        let x = delay.right + self.gap / 2;
        let inset = self.button / 4;
        Rect::new(x, delay.top + inset, x + 1, delay.bottom - inset)
    }
}

/// Where the panel beside the strip goes (the strip open at `anchor`), for a
/// panel of `size` (width, height). Below the strip, or above it on the
/// bottom edge or when there is more room above; flush with the screen edge
/// on the sides, lined up with the strip on the top and bottom. It is cut to
/// the room there is, `gap` away from the strip. Also says whether it is below.
pub fn panel_rect(
    work: Rect,
    edge: Edge,
    anchor: Rect,
    size: (i32, i32),
    gap: i32,
) -> (Rect, bool) {
    let (w, h) = size;
    let below_room = work.bottom - anchor.bottom - gap;
    let above_room = anchor.top - work.top - gap;
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
    let y = if below {
        anchor.bottom + gap
    } else {
        anchor.top - gap - h
    };
    (Rect::new(x, y, x + w, y + h), below)
}

/// Where a menu's window goes for the pointer at `at`: `size` with a `margin` of
/// shadow all round, the menu's corner at the pointer, opening away from
/// whichever sides of `work` it would otherwise run past, as Windows' do.
pub fn menu_rect(at: Point, size: (i32, i32), margin: i32, work: Rect) -> Rect {
    let (w, h) = size;
    let (menu_w, menu_h) = (w - 2 * margin, h - 2 * margin);
    let left = if at.0 + menu_w > work.right {
        at.0 - menu_w
    } else {
        at.0
    };
    let top = if at.1 + menu_h > work.bottom {
        at.1 - menu_h
    } else {
        at.1
    };
    let left = left.min(work.right - menu_w).max(work.left);
    let top = top.min(work.bottom - menu_h).max(work.top);
    Rect::new(
        left - margin,
        top - margin,
        left - margin + w,
        top - margin + h,
    )
}

/// What a shadow of `reach` covers around `shape`: all round, but for the side
/// that lies against the screen's `edge`.
pub fn shadow_rect(shape: Rect, reach: i32, edge: Option<Edge>) -> Rect {
    let side = |e: Edge| if edge == Some(e) { 0 } else { reach };
    Rect::new(
        shape.left - side(Edge::Left),
        shape.top - side(Edge::Top),
        shape.right + side(Edge::Right),
        shape.bottom + side(Edge::Bottom),
    )
}

/// The strip's shadow, `hidden` pixels of the strip still behind the screen's
/// edge: its window on screen, and which corner of the picture goes at its
/// top-left. It slides out with the strip.
pub fn shadow_viewport(edge: Edge, full: Rect, hidden: i32) -> (Rect, Point) {
    let f = full;
    match edge {
        Edge::Left => (
            Rect::new(f.left, f.top, f.right - hidden, f.bottom),
            (hidden, 0),
        ),
        Edge::Right => (Rect::new(f.left + hidden, f.top, f.right, f.bottom), (0, 0)),
        Edge::Top => (
            Rect::new(f.left, f.top, f.right, f.bottom - hidden),
            (0, hidden),
        ),
        Edge::Bottom => (Rect::new(f.left, f.top + hidden, f.right, f.bottom), (0, 0)),
    }
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
pub fn snap(at: Point, monitors: &[Monitor]) -> Option<(usize, Edge, u16)> {
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
    let len = layout(m.dpi).length(edge);
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
    fn the_layout_scales_and_slots_fit_beside_the_grip() {
        let l = layout(96);
        assert_eq!(l.bar, (375, 40));
        assert_eq!((l.depth(Edge::Top), l.length(Edge::Top)), (48, 375));
        assert_eq!((l.depth(Edge::Left), l.length(Edge::Left)), (383, 40));
        let big = layout(192);
        assert_eq!(big.bar, (750, 80));
        assert_eq!(big.depth(Edge::Top), 96);
        for edge in Edge::ALL {
            let (w, h) = l.size(edge);
            let whole = Rect::new(0, 0, w, h);
            let (grip, bar) = (l.grip_rect(edge), l.bar_rect(edge));
            assert_eq!(grip.intersect(whole), grip, "{edge:?}");
            assert_eq!(bar.intersect(whole), bar, "{edge:?}");
            assert!(grip.intersect(bar).is_empty(), "{edge:?}");
            // Left to right, inside the bar, none overlapping.
            let mut left = bar.left;
            for slot in Slot::ALL {
                let r = l.slot_rect(edge, slot);
                assert_eq!(r.intersect(bar), r, "{edge:?} {slot:?}");
                assert!(r.left >= left + l.space, "{edge:?} {slot:?}");
                assert_eq!(r.height(), l.button);
                left = r.right;
            }
            assert_eq!(left + l.pad, bar.right, "{edge:?}");
            let d = l.divider(edge);
            assert!(d.left >= l.slot_rect(edge, Slot::Delay).right);
            assert!(d.right <= l.slot_rect(edge, Slot::Play).left);
        }
    }

    #[test]
    fn the_panel_hugs_its_edge_and_stays_in_the_work_area() {
        let work = Rect::new(0, 0, 2560, 1392);
        let l = layout(96);
        assert_eq!(
            anchor(work, Edge::Left, 0, &l),
            Rect::new(0, 0, l.depth(Edge::Left), l.length(Edge::Left))
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
            let (open, src) = viewport(edge, a, l.depth(edge));
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
                let (p, below) = panel_rect(work, edge, a, size, 0);
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
        assert!(!panel_rect(work, Edge::Left, a, size, 0).1);
        // Too tall for the room: cut to it.
        let a = anchor(work, Edge::Top, 500, &l);
        let (p, _) = panel_rect(work, Edge::Top, a, (l.panel, 5000), 0);
        assert_eq!(p.bottom, work.bottom);
        // At the right end of the top edge: pulled in to stay on screen.
        let a = anchor(work, Edge::Top, 1000, &l);
        assert_eq!(panel_rect(work, Edge::Top, a, size, 0).0.right, work.right);
    }

    #[test]
    fn the_gap_keeps_the_panel_off_the_strip_and_inside_the_work_area() {
        let work = Rect::new(0, 0, 1920, 1040);
        let l = layout(96);
        let size = (l.panel, 5000);
        let a = anchor(work, Edge::Top, 500, &l);
        let (p, below) = panel_rect(work, Edge::Top, a, (l.panel, 300), 6);
        assert!(below);
        assert_eq!(p.top, a.bottom + 6);
        // Cut to the room there is, the gap counted.
        let (p, _) = panel_rect(work, Edge::Top, a, size, 6);
        assert_eq!(p.bottom, work.bottom);
        let a = anchor(work, Edge::Bottom, 500, &l);
        let (p, below) = panel_rect(work, Edge::Bottom, a, (l.panel, 300), 6);
        assert!(!below);
        assert_eq!(p.bottom, a.top - 6);
    }

    #[test]
    fn a_shadow_stays_off_the_screens_edge_and_slides_with_the_strip() {
        let shape = Rect::new(100, 0, 475, 48);
        let full = shadow_rect(shape, 22, Some(Edge::Top));
        assert_eq!(full, Rect::new(78, 0, 497, 70));
        assert_eq!(
            shadow_rect(shape, 22, None),
            Rect::new(78, -22, 497, 70),
            "a panel floats"
        );
        // Out: the whole of it. On the way: what's past the edge is cut off,
        // from the picture's side that's nearest the edge.
        assert_eq!(shadow_viewport(Edge::Top, full, 0), (full, (0, 0)));
        let (rect, from) = shadow_viewport(Edge::Top, full, 30);
        assert_eq!((rect.top, rect.bottom), (0, 40));
        assert_eq!(from, (0, 30));
        let (rect, from) = shadow_viewport(Edge::Bottom, full, 30);
        assert_eq!((rect.top, rect.bottom), (30, 70));
        assert_eq!(from, (0, 0));
        let (rect, from) = shadow_viewport(Edge::Left, full, 30);
        assert_eq!((rect.left, rect.right), (78, 467));
        assert_eq!(from, (30, 0));
        let (rect, from) = shadow_viewport(Edge::Right, full, 30);
        assert_eq!((rect.left, rect.right), (108, 497));
        assert_eq!(from, (0, 0));
    }

    #[test]
    fn a_menu_opens_at_the_pointer_and_away_from_the_screens_edges() {
        let work = Rect::new(0, 0, 1920, 1040);
        let (size, margin) = ((248, 348), 24);
        // Its own corner at the pointer: the window starts a shadow's width before.
        let r = menu_rect((500, 300), size, margin, work);
        assert_eq!((r.left, r.top), (476, 276));
        assert_eq!((r.width(), r.height()), size);
        // By the tray: up and to the left.
        let r = menu_rect((1900, 1030), size, margin, work);
        assert_eq!((r.right - margin, r.bottom - margin), (1900, 1030));
        // Too big to fit either way: it stays on the screen.
        let r = menu_rect((100, 100), (248, 1500), margin, work);
        assert_eq!(r.top + margin, 0);
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
        // Near the top of the right-hand monitor.
        let (i, edge, offset) = snap((3000, 30), &m).unwrap();
        assert_eq!((i, edge), (1, Edge::Top));
        let a = anchor(m[1].work, edge, offset, &layout(96));
        assert!(a.contains((3000, 10)), "{a:?}");
        // Far down the portrait monitor's left edge.
        let (i, edge, offset) = snap((-1430, 2000), &m).unwrap();
        assert_eq!((i, edge), (2, Edge::Left));
        assert!(anchor(m[2].work, edge, offset, &layout(96)).contains((-1440, 2000)));
        // Past the end of an edge: as far along as it goes.
        assert_eq!(snap((2550, 5000), &m).unwrap().2, 1000);
        // Off every monitor: the nearest one.
        assert_eq!(snap((9000, 700), &m).unwrap().0, 1);
        assert_eq!(snap((0, 0), &[]), None);
    }
}
