//! Selecting text with the mouse and keyboard, painting highlights, copying.

use std::ops::Range;
use std::time::Duration;

use egui::{CursorIcon, Key, Modifiers, PointerButton, Stroke, StrokeKind, pos2};
use whir_core::find_bar::fmt_int;
use whir_core::theme::Theme;

use super::{PdfDoc, ZOOM_100, Zoom, minimap};
use crate::geom;
use crate::layout::Layout;
use crate::select::{Gesture, PendingPress, Selection, TextPos, Unit};
use crate::store::TextStore;
use crate::text::{self, Class};
use crate::view::View;

/// Pointer travel (points) before a press starts extending the selection,
/// so a plain click never selects a character.
const DRAG_SLOP: f32 = 3.0;
/// Clipboard text at most (UTF-8 bytes).
const MAX_COPY_BYTES: usize = 64 << 20;

/// Shift+arrow selection moves: by character, by word, to the line's edge
/// (`true` = forwards / the end).
#[derive(Clone, Copy)]
pub(super) enum SelKey {
    Char(bool),
    Word(bool),
    LineEdge(bool),
}

impl PdfDoc {
    /// Every selection change goes through here: it cancels a pending copy.
    pub(super) fn set_sel(&mut self, s: Option<Selection>) {
        if self.sel != s {
            self.sel = s;
            if self.pending_copy {
                self.pending_copy = false;
                self.toast = None;
            }
        }
    }

    /// Primary-button presses and drags on the page area.
    pub(super) fn pointer(
        &mut self,
        ui: &egui::Ui,
        resp: &egui::Response,
        l: &Layout,
        mut v: View,
        frame: u64,
    ) {
        let (pressed, down, origin, pos, shift, time, dt) = ui.input(|i| {
            (
                i.pointer.primary_pressed(),
                i.pointer.primary_down(),
                i.pointer.press_origin(),
                i.pointer.interact_pos(),
                i.modifiers.shift,
                i.time,
                i.stable_dt,
            )
        });

        // Middle-drag pans anywhere.
        if resp.dragged_by(PointerButton::Middle) {
            let d = resp.drag_delta();
            self.sy.scroll_by(-d.y as f64);
            self.sx.scroll_by(-d.x as f64);
        }

        // `hovered` is false over the scrollbars and the find bar (they're on top).
        if pressed
            && resp.hovered()
            && let Some(p) = origin.or(pos)
        {
            let (page, at) = v.locate(l, p);
            let on_page = v.page_rect(l, page).contains(p);
            let page = page as u32;
            // Text pages (or not known yet) select; the canvas and scanned pages pan.
            if on_page && self.text.available && self.text.has_text(page) != Some(false) {
                let opts = ui.ctx().options(|o| o.input_options);
                let clicks =
                    self.clicks
                        .press(time, p, opts.max_double_click_delay, opts.max_click_dist);
                self.gesture = Gesture::Select {
                    origin: p,
                    moved: false,
                    pending: Some(PendingPress {
                        page,
                        at,
                        clicks,
                        shift,
                    }),
                };
            } else {
                self.gesture = Gesture::Pan;
            }
        }

        // A press takes effect once its page's geometry is here (normally at once).
        if let Gesture::Select {
            origin,
            moved,
            pending: Some(press),
        } = self.gesture
        {
            if self.text.has_text(press.page) == Some(false) {
                self.gesture = Gesture::Pan; // no text there after all
            } else if self.text.page_geom(press.page, frame).is_some() {
                self.gesture = if self.begin_select(press, frame) {
                    Gesture::Select {
                        origin,
                        moved,
                        pending: None,
                    }
                } else {
                    Gesture::Pan
                };
            }
        }

        if !down {
            // Released (a press still waiting for geometry is dropped).
            self.gesture = Gesture::None;
            return;
        }
        match self.gesture {
            Gesture::Select {
                origin,
                moved,
                pending,
            } => {
                let Some(p) = pos else { return };
                let moved = moved || p.distance(origin) > DRAG_SLOP;
                self.gesture = Gesture::Select {
                    origin,
                    moved,
                    pending,
                };
                if !moved || pending.is_some() {
                    return;
                }
                // Past an edge: scroll towards the pointer, faster the further out.
                let vp = v.vp;
                let over = |x: f32, lo: f32, hi: f32| {
                    if x < lo {
                        x - lo
                    } else if x > hi {
                        x - hi
                    } else {
                        0.0
                    }
                };
                let (dx, dy) = (
                    over(p.x, vp.left(), vp.right()),
                    over(p.y, vp.top(), vp.bottom()),
                );
                if dx != 0.0 || dy != 0.0 {
                    let k = 30.0 * (dt as f64).min(0.05);
                    self.sx.scroll_by(dx as f64 * k);
                    self.sy.scroll_by(dy as f64 * k);
                    (v.sx, v.sy) = (self.sx.offset, self.sy.offset);
                    ui.ctx().request_repaint();
                }
                let inner = vp.shrink(1.0);
                let c = pos2(
                    p.x.clamp(inner.left(), inner.right()),
                    p.y.clamp(inner.top(), inner.bottom()),
                );
                let (page, at) = v.locate(l, c);
                self.extend_to(page as u32, at, frame);
            }
            Gesture::Pan => {
                if resp.dragged_by(PointerButton::Primary) {
                    let d = resp.drag_delta();
                    self.sy.scroll_by(-d.y as f64);
                    self.sx.scroll_by(-d.x as f64);
                }
            }
            Gesture::None => {}
        }
    }

    /// Apply a press: caret (1 click), word (2), line (3); Shift extends the
    /// current selection. False if the page has no lines to hit.
    fn begin_select(&mut self, press: PendingPress, frame: u64) -> bool {
        let Some((g, chars)) = self.text.page_geom(press.page, frame) else {
            return false;
        };
        let Some(hit) = geom::hit_test(g, chars, press.at) else {
            return false;
        };
        let at = |i: u32| TextPos::new(press.page, i);
        let new = match self.sel {
            Some(mut s) if press.shift => {
                s.unit = Unit::Char;
                s.extend(at(hit.caret), at(hit.caret));
                s
            }
            _ => {
                // Far from any text, a double click is just a click.
                let clicks = if hit.near { press.clicks } else { 1 };
                let (lo, hi, unit) = match clicks {
                    1 => (hit.caret, hit.caret, Unit::Char),
                    2 => {
                        let w = text::word_at(chars, hit.char.unwrap_or(hit.caret));
                        (w.start, w.end, Unit::Word)
                    }
                    _ => {
                        let line = g.lines[hit.line as usize];
                        (line.start, line.end, Unit::Line)
                    }
                };
                Selection::new(at(lo), at(hi), unit)
            }
        };
        self.set_sel(Some(new));
        true
    }

    /// Extend the selection to the unit under page-local point `at`.
    fn extend_to(&mut self, page: u32, at: [f32; 2], frame: u64) {
        let Some(mut s) = self.sel else { return };
        // Pages without geometry yet (or without text) leave it as it is.
        let Some((g, chars)) = self.text.page_geom(page, frame) else {
            return;
        };
        let Some(hit) = geom::hit_test(g, chars, at) else {
            return;
        };
        let (lo, hi) = match s.unit {
            Unit::Char => (hit.caret, hit.caret),
            Unit::Word => {
                let w = text::word_at(chars, hit.char.unwrap_or(hit.caret));
                (w.start, w.end)
            }
            Unit::Line => {
                let line = g.lines[hit.line as usize];
                (line.start, line.end)
            }
        };
        s.extend(TextPos::new(page, lo), TextPos::new(page, hi));
        self.set_sel(Some(s));
    }

    /// Shift+arrow keys: move the selection's moving end.
    pub(super) fn sel_key(&mut self, k: SelKey, frame: u64) {
        let Some(sel) = self.sel else { return };
        let pages = self.text.pages() as u32;
        let f = sel.focus();
        // Without that page's text there's nothing to step over (e.g. the far
        // end of Ctrl+A on a long document): leave the selection alone.
        let Some(n) = self.text.page(f.page).map(|t| t.chars.len() as u32) else {
            return;
        };
        let f = TextPos::new(f.page, f.index.min(n));
        let to = match k {
            SelKey::Char(fwd) => step_char(&self.text, f, fwd, pages),
            SelKey::Word(fwd) => step_word(&self.text, f, fwd, pages),
            SelKey::LineEdge(end) => {
                // Lines need geometry (pages near the screen have it).
                let Some(line) = self
                    .text
                    .geom(f.page, frame)
                    .and_then(|g| Some(g.lines[g.line_of(f.index)?]))
                else {
                    return;
                };
                TextPos::new(f.page, if end { line.end } else { line.start })
            }
        };
        let fixed = sel.fixed();
        self.set_sel(Some(Selection {
            anchor_lo: fixed,
            anchor_hi: fixed,
            unit: Unit::Char,
            start: fixed.min(to),
            end: fixed.max(to),
        }));
    }

    /// I-beam over text and while selecting; hand over the canvas and pages
    /// without text (where dragging pans); arrow elsewhere on text pages.
    pub(super) fn hover_cursor(
        &mut self,
        ui: &egui::Ui,
        resp: &egui::Response,
        l: &Layout,
        v: View,
        frame: u64,
    ) {
        let ctx = ui.ctx();
        let panning = self.gesture == Gesture::Pan && resp.dragged_by(PointerButton::Primary);
        if panning || resp.dragged_by(PointerButton::Middle) {
            ctx.set_cursor_icon(CursorIcon::Grabbing);
            return;
        }
        if matches!(self.gesture, Gesture::Select { .. }) {
            ctx.set_cursor_icon(CursorIcon::Text);
            return;
        }
        if !resp.hovered() {
            return;
        }
        let Some(p) = ui.input(|i| i.pointer.hover_pos()) else {
            return;
        };
        let (page, at) = v.locate(l, p);
        if v.page_rect(l, page).contains(p) {
            match self.text.has_text(page as u32) {
                Some(true) => {
                    let over_text = self
                        .text
                        .page_geom(page as u32, frame)
                        .and_then(|(g, chars)| geom::hit_test(g, chars, at))
                        .is_some_and(|h| h.inside);
                    if over_text {
                        ctx.set_cursor_icon(CursorIcon::Text);
                    }
                    return;
                }
                None => return,
                Some(false) => {}
            }
        }
        if self.sy.is_scrollable() || self.sx.is_scrollable() {
            ctx.set_cursor_icon(CursorIcon::Grab);
        }
    }

    /// Find matches, the current match and the selection, over the tiles.
    pub(super) fn paint_highlights(
        &mut self,
        painter: &egui::Painter,
        l: &Layout,
        v: View,
        visible: Range<usize>,
        frame: u64,
        theme: &Theme,
    ) {
        let show_matches = self.find.open && self.search.is_active();
        let current = self.search.current().filter(|_| show_matches);
        for i in visible {
            let page = i as u32;
            let n = self.text.page(page).map_or(0, |t| t.chars.len() as u32);
            let sel = self.sel.and_then(|s| s.on_page(page, n));
            let matches = if show_matches {
                self.search.matches_on(page)
            } else {
                &[]
            };
            if sel.is_none() && matches.is_empty() {
                continue;
            }
            let Some(g) = self.text.geom(page, frame) else {
                continue;
            };
            let prect = v.page_rect(l, i);
            let p = painter.with_clip_rect(prect.intersect(v.vp));
            let cur = current.filter(|c| c.0 == page).map(|c| c.1);
            for m in matches.iter().filter(|m| Some(**m) != cur) {
                for r in geom::range_rects(g, m.start..m.end()) {
                    p.rect_filled(v.to_screen(prect, r), 0.0, theme.page_match);
                }
            }
            if let Some(m) = cur {
                for r in geom::range_rects(g, m.start..m.end()) {
                    let r = v.to_screen(prect, r).expand(1.0);
                    p.rect_filled(r, 1.0, theme.page_match_current);
                    p.rect_stroke(
                        r,
                        1.0,
                        Stroke::new(1.5, theme.page_match_outline),
                        StrokeKind::Outside,
                    );
                }
            }
            if let Some(range) = sel {
                for r in geom::range_rects(g, range) {
                    p.rect_filled(v.to_screen(prect, r), 0.0, theme.page_selection);
                }
            }
        }
    }

    /// Copy the selection, or wait for pages whose text hasn't arrived.
    pub(super) fn copy(&mut self, ctx: &egui::Context, now: f64) {
        let Some(sel) = self.sel.filter(|s| !s.is_empty()) else {
            self.pending_copy = false;
            return;
        };
        let last = sel.end.page.min(self.text.pages().saturating_sub(1) as u32);
        let pages = sel.start.page..=last;
        let total = pages.clone().count();
        let known = pages
            .clone()
            .filter(|&p| self.text.page(p).is_some())
            .count();
        if known < total && !self.text.full() {
            if !self.pending_copy {
                self.pending_copy = true;
                // Extract the selected pages first.
                self.handle.sweep(Some(sel.start.page));
                self.sweep_on = true;
            }
            self.toast = Some((
                format!(
                    "Copying… {} / {} pages",
                    fmt_int(known as u64),
                    fmt_int(total as u64)
                ),
                now + 2.0,
            ));
            ctx.request_repaint_after(Duration::from_millis(250));
            return;
        }
        let waited = std::mem::take(&mut self.pending_copy);
        let mut out = String::new();
        let mut complete = true;
        for p in pages {
            if p > sel.start.page {
                text::push_page_break(&mut out);
            }
            // Missing only when the text budget ran out: copy what precedes it.
            let Some(t) = self.text.page(p) else {
                complete = false;
                break;
            };
            if let Some(r) = sel.on_page(p, t.chars.len() as u32)
                && !text::push_copy(&mut out, &t.chars, &t.hyphens, r, MAX_COPY_BYTES)
            {
                complete = false;
                break;
            }
        }
        if out.trim().is_empty() {
            self.toast = waited.then(|| ("Nothing to copy".to_owned(), now + 1.5));
            return;
        }
        let msg = if !complete {
            Some(format!(
                "Copied the first {} characters",
                fmt_int(out.chars().count() as u64)
            ))
        } else {
            waited.then(|| "Copied".to_owned())
        };
        ctx.copy_text(out);
        if let Some(msg) = msg {
            self.toast = Some((msg, now + 2.0));
        }
    }

    pub(super) fn context_menu(
        &mut self,
        resp: &egui::Response,
        ctx: &egui::Context,
        pages: usize,
        now: f64,
    ) {
        let has_text = self.text.available;
        let has_sel = self.sel.is_some_and(|s| !s.is_empty());
        let (mut copy, mut all) = (false, false);
        let current = self.zoom;
        let mut pick = None;
        resp.context_menu(|ui| {
            if has_text {
                let c = egui::Button::new("Copy").shortcut_text("Ctrl+C");
                if ui.add_enabled(has_sel, c).clicked() {
                    copy = true;
                    ui.close();
                }
                let a = egui::Button::new("Select all").shortcut_text("Ctrl+A");
                if ui.add(a).clicked() {
                    all = true;
                    ui.close();
                }
                ui.separator();
            }
            for (zoom, label, key) in [
                (Zoom::FitPage, "Fit page", "0"),
                (Zoom::FitWidth, "Fit width", "2"),
                (Zoom::Manual(ZOOM_100), "Actual size", "1"),
            ] {
                let b = egui::Button::new(label)
                    .shortcut_text(key)
                    .selected(current == zoom);
                if ui.add(b).clicked() {
                    pick = Some(zoom);
                    ui.close();
                }
            }
            ui.separator();
            let label = if minimap::enabled() {
                "Hide minimap"
            } else {
                "Show minimap"
            };
            if ui
                .add(egui::Button::new(label).shortcut_text("Ctrl+M"))
                .clicked()
            {
                minimap::toggle();
                ui.close();
            }
        });
        if all {
            self.set_sel(Some(Selection::all(pages as u32)));
        }
        if pick.is_some() {
            // The next frame zooms, as if its key was pressed.
            self.menu_zoom = pick;
            ctx.request_repaint();
        }
        if copy {
            self.copy(ctx, now);
        }
    }
}

/// Shift(+Ctrl)+arrows and Shift+Home/End, most specific first.
pub(super) fn consume_sel_key(i: &mut egui::InputState) -> Option<SelKey> {
    let cs = Modifiers::CTRL | Modifiers::SHIFT;
    let s = Modifiers::SHIFT;
    [
        (cs, Key::ArrowRight, SelKey::Word(true)),
        (cs, Key::ArrowLeft, SelKey::Word(false)),
        (s, Key::ArrowRight, SelKey::Char(true)),
        (s, Key::ArrowLeft, SelKey::Char(false)),
        (s, Key::End, SelKey::LineEdge(true)),
        (s, Key::Home, SelKey::LineEdge(false)),
    ]
    .into_iter()
    .find(|&(m, k, _)| i.consume_key(m, k))
    .map(|(_, _, a)| a)
}

/// The end of `page`; `u32::MAX` (still its end, see `Selection::on_page`)
/// while its text is unknown.
fn page_end(store: &TextStore, page: u32) -> u32 {
    store.page(page).map_or(u32::MAX, |t| t.chars.len() as u32)
}

/// One character from `f` (a line break counts as one), across pages.
fn step_char(store: &TextStore, f: TextPos, fwd: bool, pages: u32) -> TextPos {
    let chars = store.page(f.page).map_or(&[][..], |t| &t.chars[..]);
    let n = chars.len() as u32;
    let i = f.index.min(n);
    if fwd {
        if i >= n {
            return if f.page + 1 < pages {
                TextPos::new(f.page + 1, 0)
            } else {
                TextPos::new(f.page, n)
            };
        }
        let mut k = i + 1;
        if chars[i as usize] == text::CR && chars.get(k as usize) == Some(&text::LF) {
            k += 1;
        }
        TextPos::new(f.page, geom::snap_caret(chars, k, true))
    } else {
        if i == 0 {
            return match f.page.checked_sub(1) {
                Some(p) => TextPos::new(p, page_end(store, p)),
                None => f,
            };
        }
        let mut k = i - 1;
        if k > 0 && chars[k as usize] == text::LF && chars[k as usize - 1] == text::CR {
            k -= 1;
        }
        TextPos::new(f.page, geom::snap_caret(chars, k, false))
    }
}

/// To the end of the next word (or the start of the previous one), across pages.
fn step_word(store: &TextStore, f: TextPos, fwd: bool, pages: u32) -> TextPos {
    let chars = store.page(f.page).map_or(&[][..], |t| &t.chars[..]);
    let n = chars.len() as u32;
    let i = f.index.min(n);
    let blank = |u: u16| {
        matches!(
            text::class(u),
            Class::Space | Class::LineBreak | Class::Ignorable
        )
    };
    let mut k = i;
    if fwd {
        while k < n && blank(chars[k as usize]) {
            k += 1;
        }
        if k < n {
            k = text::word_at(chars, k).end;
        }
        if k == i && f.page + 1 < pages {
            return TextPos::new(f.page + 1, 0);
        }
    } else {
        while k > 0 && blank(chars[k as usize - 1]) {
            k -= 1;
        }
        if k > 0 {
            k = text::word_at(chars, k - 1).start;
        }
        if k == i && f.page > 0 {
            let p = f.page - 1;
            return TextPos::new(p, page_end(store, p));
        }
    }
    TextPos::new(f.page, k)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::PageText;

    /// Pages 0 and 2 known ("ab\r\ncd", "xy"), page 1 not extracted yet.
    fn store() -> TextStore {
        let mut s = TextStore::new(3, true);
        for (p, t) in [(0, "ab\r\ncd"), (2, "xy")] {
            let t = PageText {
                chars: t.encode_utf16().collect(),
                ..PageText::default()
            };
            s.insert(p, t, false, 1);
        }
        s
    }

    fn p(page: u32, index: u32) -> TextPos {
        TextPos::new(page, index)
    }

    #[test]
    fn char_steps() {
        let s = store();
        // A line break is one step.
        assert_eq!(step_char(&s, p(0, 2), true, 3), p(0, 4));
        assert_eq!(step_char(&s, p(0, 4), false, 3), p(0, 2));
        // Across pages; an unknown page's end stays "the end".
        assert_eq!(step_char(&s, p(0, 6), true, 3), p(1, 0));
        assert_eq!(step_char(&s, p(2, 0), false, 3), p(1, u32::MAX));
        assert_eq!(step_char(&s, p(1, 0), false, 3), p(0, 6));
        // The document's ends don't move.
        assert_eq!(step_char(&s, p(0, 0), false, 3), p(0, 0));
        assert_eq!(step_char(&s, p(2, 2), true, 3), p(2, 2));
    }

    #[test]
    fn word_steps() {
        let s = store();
        assert_eq!(step_word(&s, p(0, 0), true, 3), p(0, 2));
        // Skips the break, then to the next word's end.
        assert_eq!(step_word(&s, p(0, 2), true, 3), p(0, 6));
        assert_eq!(step_word(&s, p(0, 6), false, 3), p(0, 4));
        assert_eq!(step_word(&s, p(0, 4), false, 3), p(0, 0));
        assert_eq!(step_word(&s, p(0, 6), true, 3), p(1, 0));
        assert_eq!(step_word(&s, p(2, 0), false, 3), p(1, u32::MAX));
    }
}
