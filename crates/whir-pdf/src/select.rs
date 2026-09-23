//! Text selection state: positions are (page, char index) in PDFium's content
//! order, so a selection is a plain index range and can span pages without
//! any geometry. Geometry only turns pointer positions into indices.

use std::ops::Range;

use egui::Pos2;

/// A caret position: before char `index` of `page`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct TextPos {
    pub page: u32,
    pub index: u32,
}

impl TextPos {
    pub fn new(page: u32, index: u32) -> Self {
        Self { page, index }
    }
}

/// What a drag extends by: set by the click count that started it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unit {
    Char,
    Word,
    Line,
}

/// `[start, end)`, with the unit the gesture began on kept as the anchor so
/// dragging back past it flips direction without losing the first word/line.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Selection {
    pub anchor_lo: TextPos,
    pub anchor_hi: TextPos,
    pub unit: Unit,
    pub start: TextPos,
    pub end: TextPos,
}

impl Selection {
    pub fn new(lo: TextPos, hi: TextPos, unit: Unit) -> Self {
        Self {
            anchor_lo: lo,
            anchor_hi: hi,
            unit,
            start: lo,
            end: hi,
        }
    }

    /// Everything, from the first page to the end of the last.
    pub fn all(pages: u32) -> Self {
        let start = TextPos::new(0, 0);
        Self {
            anchor_lo: start,
            anchor_hi: start,
            unit: Unit::Char,
            start,
            end: TextPos::new(pages.saturating_sub(1), u32::MAX),
        }
    }

    /// Extend to the unit `[f_lo, f_hi)` under the pointer.
    pub fn extend(&mut self, f_lo: TextPos, f_hi: TextPos) {
        if f_lo < self.anchor_lo {
            (self.start, self.end) = (f_lo, self.anchor_hi);
        } else {
            (self.start, self.end) = (self.anchor_lo, f_hi.max(self.anchor_hi));
        }
    }

    pub fn is_empty(&self) -> bool {
        self.start >= self.end
    }

    /// The end that moves when extending: where the pointer (or keyboard) is.
    pub fn focus(&self) -> TextPos {
        if self.start < self.anchor_lo {
            self.start
        } else {
            self.end
        }
    }

    /// The fixed end, opposite [`Self::focus`].
    pub fn fixed(&self) -> TextPos {
        if self.start < self.anchor_lo {
            self.end
        } else {
            self.start
        }
    }

    /// The selected indices on `page` (which has `n` chars), if any.
    pub fn on_page(&self, page: u32, n: u32) -> Option<Range<u32>> {
        if page < self.start.page || page > self.end.page {
            return None;
        }
        let s = if page == self.start.page {
            self.start.index.min(n)
        } else {
            0
        };
        let e = if page == self.end.page {
            self.end.index.min(n)
        } else {
            n
        };
        (s < e).then_some(s..e)
    }
}

/// Counts presses at press time (egui reports double/triple clicks on
/// release), so word/line selection starts on mouse-down and a drag that
/// follows extends by that unit.
#[derive(Default)]
pub struct ClickCounter {
    last: Option<(f64, Pos2)>,
    count: u8,
}

impl ClickCounter {
    /// Register a press at time `t`; returns 1, 2 or 3 (and stays at 3).
    pub fn press(&mut self, t: f64, p: Pos2, max_delay: f64, max_dist: f32) -> u8 {
        let chained = self
            .last
            .is_some_and(|(lt, lp)| t >= lt && t - lt <= max_delay && lp.distance(p) <= max_dist);
        self.count = if chained { (self.count + 1).min(3) } else { 1 };
        self.last = Some((t, p));
        self.count
    }
}

/// A primary press that landed before the page's geometry arrived; replayed
/// when it does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PendingPress {
    pub page: u32,
    /// Page-local display points.
    pub at: [f32; 2],
    pub clicks: u8,
    pub shift: bool,
}

/// What the primary button is doing on the page area.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Gesture {
    #[default]
    None,
    /// Dragging the canvas (or a page without text).
    Pan,
    Select {
        /// Screen position of the press.
        origin: Pos2,
        /// Moved far enough to start extending (a plain click never selects a char).
        moved: bool,
        pending: Option<PendingPress>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::pos2;

    fn p(page: u32, index: u32) -> TextPos {
        TextPos::new(page, index)
    }

    #[test]
    fn extend_by_char() {
        let mut s = Selection::new(p(0, 5), p(0, 5), Unit::Char);
        s.extend(p(0, 9), p(0, 9));
        assert_eq!((s.start, s.end), (p(0, 5), p(0, 9)));
        assert_eq!(s.focus(), p(0, 9));
        s.extend(p(0, 2), p(0, 2));
        assert_eq!((s.start, s.end), (p(0, 2), p(0, 5)));
        assert_eq!((s.focus(), s.fixed()), (p(0, 2), p(0, 5)));
        s.extend(p(0, 5), p(0, 5));
        assert!(s.is_empty());
        // Across pages.
        s.extend(p(3, 1), p(3, 1));
        assert_eq!((s.start, s.end), (p(0, 5), p(3, 1)));
    }

    #[test]
    fn extend_by_word_keeps_the_anchor_word() {
        // Double-clicked word [10, 15).
        let mut s = Selection::new(p(0, 10), p(0, 15), Unit::Word);
        s.extend(p(0, 20), p(0, 24));
        assert_eq!((s.start, s.end), (p(0, 10), p(0, 24)));
        s.extend(p(0, 2), p(0, 6));
        assert_eq!((s.start, s.end), (p(0, 2), p(0, 15)));
        // Back inside the anchor word: just the word.
        s.extend(p(0, 10), p(0, 15));
        assert_eq!((s.start, s.end), (p(0, 10), p(0, 15)));
        s.extend(p(0, 12), p(0, 13));
        assert_eq!((s.start, s.end), (p(0, 10), p(0, 15)));
    }

    #[test]
    fn extend_by_line() {
        let mut s = Selection::new(p(1, 40), p(1, 80), Unit::Line);
        s.extend(p(2, 0), p(2, 30));
        assert_eq!((s.start, s.end), (p(1, 40), p(2, 30)));
        s.extend(p(0, 100), p(0, 150));
        assert_eq!((s.start, s.end), (p(0, 100), p(1, 80)));
    }

    #[test]
    fn per_page_ranges() {
        let s = Selection::all(3);
        assert_eq!(s.on_page(0, 10), Some(0..10));
        assert_eq!(s.on_page(2, 7), Some(0..7));
        assert_eq!(s.on_page(3, 7), None);
        let s = Selection::new(p(1, 4), p(3, 2), Unit::Char);
        assert_eq!(s.on_page(0, 10), None);
        assert_eq!(s.on_page(1, 10), Some(4..10));
        assert_eq!(s.on_page(1, 3), None);
        assert_eq!(s.on_page(2, 5), Some(0..5));
        assert_eq!(s.on_page(3, 10), Some(0..2));
        assert_eq!(s.on_page(2, 0), None);
    }

    #[test]
    fn click_counting() {
        let mut c = ClickCounter::default();
        let at = pos2(100.0, 100.0);
        assert_eq!(c.press(1.0, at, 0.5, 6.0), 1);
        assert_eq!(c.press(1.2, at, 0.5, 6.0), 2);
        assert_eq!(c.press(1.4, pos2(103.0, 101.0), 0.5, 6.0), 3);
        // Saturates.
        assert_eq!(c.press(1.6, at, 0.5, 6.0), 3);
        // Too slow → starts over.
        assert_eq!(c.press(2.3, at, 0.5, 6.0), 1);
        // Too far → starts over.
        assert_eq!(c.press(2.4, pos2(120.0, 100.0), 0.5, 6.0), 1);
        assert_eq!(c.press(2.5, pos2(120.0, 100.0), 0.5, 6.0), 2);
    }
}
