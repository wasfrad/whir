//! Find in the document: the find bar's keys and status, where the first
//! match is looked for, and scrolling a match into view.

use std::ops::Range;
use std::time::Duration;

use egui::{Key, Modifiers, Rect};
use whir_core::find_bar::{FindBar, FindStatus, fmt_int};

use super::{BAR, PdfDoc, REVEAL_WAIT, Reveal};
use crate::geom::{self, NO_BOX};
use crate::layout::{self, Layout};
use crate::search::Match;
use crate::select::{Gesture, Selection, TextPos, Unit};
use crate::text;
use crate::view::View;

/// Search work per frame; more is spread over the following frames.
const SEARCH_SLICE: Duration = Duration::from_millis(4);
/// Longest selection (UTF-16 units) that pre-fills the find field.
const PREFILL_MAX: u32 = 256;

impl PdfDoc {
    /// Ctrl+F, F3 / Shift+F3 and Esc.
    pub(super) fn find_keys(&mut self, ui: &mut egui::Ui, find_focus: bool, now: f64) {
        let keys = FindBar::consume_keys(ui);
        if keys.open {
            let prefill = self.prefill();
            self.find.open_with(prefill);
        }
        if keys.next || keys.prev {
            if self.find.open {
                self.step_match(keys.next, now);
            } else {
                self.find.open_with(None);
            }
        }
        let busy = self.find.open || self.sel.is_some() || self.gesture != Gesture::None;
        if !find_focus && busy && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            if self.find.open {
                self.close_find();
            } else {
                self.set_sel(None);
                self.gesture = Gesture::None;
            }
        }
    }

    /// The selection as a find query, if it's a short run within one line.
    fn prefill(&self) -> Option<String> {
        let s = self
            .sel
            .filter(|s| !s.is_empty() && s.start.page == s.end.page)?;
        let t = self.text.page(s.start.page)?;
        let r = s.on_page(s.start.page, t.chars.len() as u32)?;
        let units = &t.chars[r.start as usize..r.end as usize];
        if r.end - r.start > PREFILL_MAX || units.iter().any(|&u| text::is_break(u)) {
            return None;
        }
        let mut out = String::new();
        text::push_copy(&mut out, &t.chars, &t.hyphens, r, usize::MAX);
        let q = out.trim();
        (!q.is_empty()).then(|| q.to_owned())
    }

    /// Close the bar; the current match becomes the selection.
    pub(super) fn close_find(&mut self) {
        if let Some((p, m)) = self.search.current() {
            let (a, b) = (TextPos::new(p, m.start), TextPos::new(p, m.end()));
            self.set_sel(Some(Selection::new(a, b, Unit::Char)));
        }
        self.find.close();
        self.pending_reveal = None;
    }

    pub(super) fn start_search(&mut self, q: &str, anchor: TextPos, now: f64, ctx: &egui::Context) {
        self.search.restart(q, anchor, self.text.pages());
        self.pending_reveal = None;
        // Extract the pages the results will come from first.
        if self.search.is_active() && self.sweep_on {
            self.handle.sweep(Some(anchor.page));
        }
        self.run_search(ctx, now);
        ctx.request_repaint();
    }

    /// Where the first match is looked for: the current match if it's on
    /// screen (typing more letters keeps the view still), else a visible
    /// selection, else the first line on screen.
    pub(super) fn search_anchor(
        &mut self,
        l: &Layout,
        v: View,
        visible: Range<usize>,
        frame: u64,
    ) -> TextPos {
        if let Some((p, m)) = self.search.current()
            && visible.contains(&(p as usize))
        {
            let prect = v.page_rect(l, p as usize);
            let on_screen = match self.text.geom(p, frame) {
                Some(g) => geom::range_rects(g, m.start..m.end())
                    .iter()
                    .any(|r| v.to_screen(prect, *r).intersects(v.vp)),
                None => true,
            };
            if on_screen {
                return TextPos::new(p, m.start);
            }
        }
        if let Some(s) = self.sel
            && !s.is_empty()
            && (s.start.page as usize) < visible.end
            && (s.end.page as usize) >= visible.start
        {
            return s.start;
        }
        let y = v.sy / v.zoom;
        let mut page = l.page_at(y);
        if y > l.bottom(page) && page + 1 < l.len() {
            page += 1; // the viewport starts in the gap below `page`
        }
        let local_top = (y - l.top(page)) as f32;
        let index = match self.text.geom(page as u32, frame) {
            // Past every line: the anchor page's matches come last.
            Some(g) => g
                .lines
                .iter()
                .find(|ln| !geom::is_empty(&ln.rect) && ln.rect[3] > local_top)
                .map_or(u32::MAX, |ln| ln.start),
            None => 0,
        };
        TextPos::new(page as u32, index)
    }

    pub(super) fn run_search(&mut self, ctx: &egui::Context, now: f64) {
        if !self.search.is_active() {
            return;
        }
        self.search.set_partial(self.text.full());
        let store = &self.text;
        let more = self.search.run(|p| store.search_text(p), SEARCH_SLICE);
        if let Some((p, m)) = self.search.settle() {
            self.reveal(p, m, now);
        }
        if more {
            ctx.request_repaint();
        }
    }

    pub(super) fn step_match(&mut self, forward: bool, now: f64) {
        if let Some((p, m)) = self.search.step(forward) {
            self.reveal(p, m, now);
        }
    }

    pub(super) fn find_status(&self) -> FindStatus {
        if self.find.searched().is_empty() {
            return FindStatus::Idle;
        }
        if !self.text.available {
            return FindStatus::Message("Search needs a newer pdfium.dll".into());
        }
        if self.text.all_known() && !self.text.any_text() {
            return FindStatus::Message("No text in this PDF".into());
        }
        if !self.search.is_active() {
            return FindStatus::Idle;
        }
        let pages = self.text.pages();
        let known = self.text.known_count();
        let incomplete = !self.text.all_known() || self.search.pending();
        let total = self.search.total();
        if total > 0 {
            return FindStatus::Results {
                current: self.search.ordinal(),
                total,
                more: incomplete || self.search.truncated(),
            };
        }
        if self.text.full() && !self.search.pending() {
            FindStatus::Message(format!(
                "Searched {} of {} pages",
                fmt_int(known as u64),
                fmt_int(pages as u64)
            ))
        } else if incomplete {
            FindStatus::Searching {
                progress: Some(known as f32 / pages.max(1) as f32),
            }
        } else {
            FindStatus::NoResults
        }
    }

    // --- scrolling to a match ------------------------------------------------------

    fn reveal(&mut self, page: u32, m: Match, now: f64) {
        self.pending_reveal = Some(Reveal {
            page,
            m,
            since: now,
            page_shown: false,
        });
    }

    /// Scroll a pending match into view once its page's geometry is here.
    pub(super) fn apply_reveal(
        &mut self,
        l: &Layout,
        vp: Rect,
        zoom: f64,
        x_off: f64,
        now: f64,
        frame: u64,
    ) {
        let Some(mut r) = self.pending_reveal else {
            return;
        };
        let p = r.page as usize;
        if p >= l.len() {
            self.pending_reveal = None;
            return;
        }
        if let Some(g) = self.text.geom(r.page, frame) {
            let u = geom::range_rects(g, r.m.start..r.m.end())
                .into_iter()
                .fold(NO_BOX, geom::union);
            if geom::is_empty(&u) {
                self.show_page(l, p, zoom, vp.height() as f64);
            } else {
                self.scroll_into_view(l, p, u, vp, zoom, x_off);
            }
            self.pending_reveal = None;
        } else if !self.text.needs_geom(r.page) {
            // No geometry is coming (no boxes on that page): the page will do.
            self.show_page(l, p, zoom, vp.height() as f64);
            self.pending_reveal = None;
        } else if !r.page_shown && now - r.since >= REVEAL_WAIT {
            self.show_page(l, p, zoom, vp.height() as f64);
            r.page_shown = true;
            self.pending_reveal = Some(r);
        }
    }

    /// Scroll page `p` on screen if none of it is.
    fn show_page(&mut self, l: &Layout, p: usize, zoom: f64, vh: f64) {
        let (top, bottom) = (l.top(p) * zoom, l.bottom(p) * zoom);
        if bottom < self.sy.offset || top > self.sy.offset + vh {
            self.sy.scroll_to(top - layout::GAP * zoom / 2.0);
        }
    }

    /// Scroll so page-local box `u` of page `p` is visible: vertically to 30%
    /// of the viewport if it's hidden (or under the find bar / scrollbar),
    /// horizontally just enough.
    fn scroll_into_view(
        &mut self,
        l: &Layout,
        p: usize,
        u: [f32; 4],
        vp: Rect,
        zoom: f64,
        x_off: f64,
    ) {
        let [pw, _] = l.sizes[p];
        let doc_x = layout::MARGIN + (l.max_width - pw as f64) / 2.0;
        let (x0, x1) = (
            x_off + (doc_x + u[0] as f64) * zoom,
            x_off + (doc_x + u[2] as f64) * zoom,
        );
        let (y0, y1) = (
            (l.top(p) + u[1] as f64) * zoom,
            (l.top(p) + u[3] as f64) * zoom,
        );
        let vh = vp.height() as f64;
        let top_inset = if self.find.open {
            (FindBar::rect(vp, 0.0).bottom() - vp.top()) as f64
        } else {
            0.0
        };
        let (band_top, band_bottom) =
            (self.sy.offset + top_inset, self.sy.offset + vh - BAR as f64);
        if y0 < band_top || y1 > band_bottom {
            self.sy.scroll_to(y0 - 0.3 * vh);
        }
        self.sx.reveal(x0 - 20.0, x1 + 20.0 + BAR as f64);
    }
}
