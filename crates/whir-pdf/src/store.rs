//! The UI side's text cache. Text (~2 bytes a character) is kept for every
//! page that arrives so search and copy never re-ask; geometry (~25× more)
//! only for pages near the viewport, in a small LRU.

use crate::geom::PageGeom;
use crate::text::{self, PageText};

/// Geometry for this many bytes of pages stays cached.
const GEOM_BUDGET: usize = 8 << 20;
/// Beyond this much text, stop collecting (and searching) more pages.
const TEXT_BUDGET: usize = 64 << 20;

/// A page's characters, ready for search and copy.
pub struct PageChars {
    pub chars: Vec<u16>,
    pub hyphens: Vec<u32>,
    /// [`text::soft_ranges`], precomputed for search.
    pub soft: Vec<(u32, u32)>,
}

impl PageChars {
    fn bytes(&self) -> usize {
        self.chars.capacity() * 2 + self.hyphens.capacity() * 4 + self.soft.capacity() * 8
    }
}

#[derive(Default)]
struct Slot {
    text: Option<PageChars>,
    failed: bool,
    geom: Option<PageGeom>,
    /// Arrived with text but no usable geometry (degenerate page transform):
    /// asking again won't help.
    no_geom: bool,
    geom_used: u64,
}

pub struct TextStore {
    /// The loaded pdfium.dll has the text API.
    pub available: bool,
    slots: Vec<Slot>,
    geom_bytes: usize,
    text_bytes: usize,
    known: usize,
    any_text: bool,
    /// The text budget ran out: some pages will never be known.
    full: bool,
}

impl TextStore {
    pub fn new(pages: usize, available: bool) -> Self {
        Self {
            available,
            slots: (0..pages).map(|_| Slot::default()).collect(),
            geom_bytes: 0,
            text_bytes: 0,
            known: 0,
            any_text: false,
            full: false,
        }
    }

    pub fn pages(&self) -> usize {
        self.slots.len()
    }

    /// Take a page from the PDF thread. Geometry is kept only if `keep_geom`
    /// (the page is on or near the screen). Returns true if the page's text
    /// just became known (search should look at it).
    pub fn insert(&mut self, page: u32, t: PageText, keep_geom: bool, frame: u64) -> bool {
        let Some(slot) = self.slots.get_mut(page as usize) else {
            return false;
        };
        let known_before = slot.text.is_some() || slot.failed;
        if keep_geom {
            match t.geom {
                Some(g) => {
                    if let Some(old) = slot.geom.take() {
                        self.geom_bytes -= old.bytes();
                    }
                    self.geom_bytes += g.bytes();
                    slot.geom = Some(g);
                    slot.geom_used = frame;
                }
                None => slot.no_geom = !t.failed,
            }
        }
        if known_before {
            return false;
        }
        if t.failed {
            slot.failed = true;
        } else {
            let bytes = t.chars.len() * 2;
            // Over budget, only on-screen pages still get text (selection must work).
            if self.text_bytes + bytes > TEXT_BUDGET && !keep_geom {
                self.full = true;
                return false;
            }
            let soft = text::soft_ranges(&t.chars, &t.hyphens);
            let mut chars = t.chars;
            chars.shrink_to_fit();
            let pc = PageChars {
                chars,
                hyphens: t.hyphens,
                soft,
            };
            self.text_bytes += pc.bytes();
            self.any_text |= pc
                .chars
                .iter()
                .any(|&u| !text::is_space(u) && !text::is_ignorable(u));
            slot.text = Some(pc);
        }
        self.known += 1;
        true
    }

    /// The page's text; empty for pages PDFium couldn't read.
    pub fn page(&self, page: u32) -> Option<&PageChars> {
        const EMPTY: &PageChars = &PageChars {
            chars: Vec::new(),
            hyphens: Vec::new(),
            soft: Vec::new(),
        };
        let slot = self.slots.get(page as usize)?;
        match &slot.text {
            Some(t) => Some(t),
            None if slot.failed => Some(EMPTY),
            None => None,
        }
    }

    /// Units and soft ranges, as search wants them.
    pub fn search_text(&self, page: u32) -> Option<(&[u16], &[(u32, u32)])> {
        self.page(page).map(|t| (&t.chars[..], &t.soft[..]))
    }

    /// The page's geometry, marking it used this frame.
    pub fn geom(&mut self, page: u32, frame: u64) -> Option<&PageGeom> {
        let slot = self.slots.get_mut(page as usize)?;
        slot.geom_used = frame;
        slot.geom.as_ref()
    }

    /// Worth asking the PDF thread for this page's geometry.
    pub fn needs_geom(&self, page: u32) -> bool {
        self.available
            && self.slots.get(page as usize).is_some_and(|s| {
                s.geom.is_none()
                    && !s.failed
                    && !s.no_geom
                    && s.text.as_ref().is_none_or(|t| !t.chars.is_empty())
            })
    }

    /// `None`: not known yet; `Some(false)`: nothing selectable (scanned page,
    /// unreadable page, or no text API).
    pub fn has_text(&self, page: u32) -> Option<bool> {
        if !self.available {
            return Some(false);
        }
        let slot = self.slots.get(page as usize)?;
        if slot.failed {
            return Some(false);
        }
        slot.text.as_ref().map(|t| !t.chars.is_empty())
    }

    pub fn all_known(&self) -> bool {
        self.known == self.slots.len()
    }

    pub fn known_count(&self) -> usize {
        self.known
    }

    /// Some known page has visible characters.
    pub fn any_text(&self) -> bool {
        self.any_text
    }

    /// The text budget ran out before every page arrived.
    pub fn full(&self) -> bool {
        self.full
    }

    /// Keep geometry within budget, dropping the least recently used first;
    /// never what was used in `keep_frame`.
    pub fn evict_geom(&mut self, keep_frame: u64) {
        if self.geom_bytes <= GEOM_BUDGET {
            return;
        }
        let mut by_age: Vec<(u64, usize)> = self
            .slots
            .iter()
            .enumerate()
            .filter(|(_, s)| s.geom.is_some() && s.geom_used < keep_frame)
            .map(|(i, s)| (s.geom_used, i))
            .collect();
        by_age.sort_unstable();
        for (_, i) in by_age {
            if self.geom_bytes <= GEOM_BUDGET {
                break;
            }
            if let Some(g) = self.slots[i].geom.take() {
                self.geom_bytes -= g.bytes();
            }
        }
    }

    /// Drop all geometry not used in `frame` (the tab was hidden).
    pub fn drop_geom_except(&mut self, frame: u64) {
        for s in &mut self.slots {
            if s.geom_used < frame
                && let Some(g) = s.geom.take()
            {
                self.geom_bytes -= g.bytes();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::{Line, NO_BOX};

    fn page_text(s: &str, geom_boxes: usize) -> PageText {
        PageText {
            chars: s.encode_utf16().collect(),
            hyphens: Vec::new(),
            geom: (geom_boxes > 0).then(|| PageGeom {
                boxes: vec![NO_BOX; geom_boxes],
                lines: vec![Line {
                    start: 0,
                    end: 0,
                    rect: NO_BOX,
                    vertical: false,
                    dir: 1,
                }],
            }),
            failed: false,
        }
    }

    #[test]
    fn tracks_known_pages_and_geometry() {
        let mut s = TextStore::new(3, true);
        assert_eq!(s.has_text(0), None);
        assert!(s.needs_geom(0));
        assert!(s.insert(0, page_text("hello", 5), true, 1));
        assert_eq!(s.has_text(0), Some(true));
        assert!(!s.needs_geom(0));
        assert!(s.geom(0, 2).is_some());
        // Re-sent with geometry: not new text.
        assert!(!s.insert(0, page_text("hello", 5), true, 3));
        // Arrived without geometry wanted: text only.
        assert!(s.insert(1, page_text("x", 1), false, 3));
        assert!(s.geom(1, 3).is_none());
        assert!(s.needs_geom(1));
        // A failed page is known and empty.
        assert!(s.insert(2, PageText::failed(), true, 3));
        assert_eq!(s.has_text(2), Some(false));
        assert!(!s.needs_geom(2));
        assert_eq!(s.search_text(2), Some((&[][..], &[][..])));
        assert!(s.all_known());
        assert!(s.any_text());
        // Scanned page: known, empty, nothing to ask for.
        let mut s = TextStore::new(1, true);
        assert!(s.insert(0, page_text("", 0), true, 1));
        assert_eq!(s.has_text(0), Some(false));
        assert!(!s.needs_geom(0));
        assert!(!s.any_text());
        // Without the text API nothing is selectable.
        let s = TextStore::new(1, false);
        assert_eq!(s.has_text(0), Some(false));
        assert!(!s.needs_geom(0));
    }

    #[test]
    fn geometry_lru() {
        let per_page = PageGeom {
            boxes: vec![NO_BOX; 100_000],
            lines: Vec::new(),
        }
        .bytes();
        let n = GEOM_BUDGET / per_page + 3;
        let mut s = TextStore::new(n, true);
        for p in 0..n {
            s.insert(p as u32, page_text("x", 100_000), true, p as u64);
        }
        // Page 0 used recently: survives.
        s.geom(0, n as u64);
        s.evict_geom(n as u64);
        assert!(s.geom_bytes <= GEOM_BUDGET);
        assert!(s.geom(0, n as u64).is_some());
        assert!(s.geom(1, n as u64).is_none());
        assert!(s.geom(n as u32 - 1, n as u64).is_some());
        s.drop_geom_except(n as u64 + 1);
        assert_eq!(s.geom_bytes, 0);
        // Text survives geometry eviction.
        assert!(s.page(1).is_some());
    }
}
