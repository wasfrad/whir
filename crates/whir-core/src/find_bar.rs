//! The find bar shared by the CSV and PDF viewers: a small overlay at the top
//! right with a search field, a match counter and ↑ ↓ ✕ (tables add a
//! whole-cell toggle). The shell's Find in all tabs panel docks the same bar.
//!
//! The owner keeps the results; the bar only reports what the user asked for
//! ([`FindBarOut`]) and shows a [`FindStatus`].

use std::sync::atomic::{AtomicU64, Ordering};

use egui::text::{CCursor, CCursorRange};
use egui::{Align, EventFilter, Frame, Key, Layout, Modifiers, Rect, RichText, Sense, UiBuilder};

use crate::icons::{self, Icon};
use crate::theme::Theme;

const SIZE: egui::Vec2 = egui::vec2(390.0, 36.0);
/// The whole-cell toggle's button.
const TOGGLE: egui::Vec2 = egui::vec2(26.0, 22.0);
/// Longest query accepted.
const MAX_QUERY: usize = 1000;

/// What to look for.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct FindQuery {
    pub text: String,
    /// Only whole cells match, ignoring case and surrounding spaces; numbers
    /// match by value (`2.45` finds `2.4500`). Tables only.
    pub whole_cell: bool,
}

impl FindQuery {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            whole_cell: false,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
}

impl From<&str> for FindQuery {
    fn from(text: &str) -> Self {
        Self::new(text)
    }
}

/// What the bar should say next to the field.
#[derive(Clone, Debug, PartialEq)]
pub enum FindStatus {
    Idle,
    Searching {
        /// 0..=1 when known.
        progress: Option<f32>,
    },
    Results {
        /// 0-based index of the current match.
        current: Option<usize>,
        total: usize,
        /// More results may still arrive (or the count was capped).
        more: bool,
    },
    NoResults,
    Message(String),
}

/// Find shortcuts that work whether or not the bar has focus.
#[derive(Default, Debug)]
pub struct FindKeys {
    /// Ctrl+F.
    pub open: bool,
    /// F3.
    pub next: bool,
    /// Shift+F3.
    pub prev: bool,
}

#[derive(Default, Debug)]
pub struct FindBarOut {
    /// Run a new search for this query (typing settled, Enter on a changed
    /// query, the whole-cell toggle, or the bar was just opened with text).
    pub search: Option<FindQuery>,
    pub next: bool,
    pub prev: bool,
    /// The user closed the bar (Esc in the field, or ✕).
    pub closed: bool,
}

pub struct FindBar {
    id: egui::Id,
    pub open: bool,
    pub query: String,
    hint: &'static str,
    /// Seconds of typing quiet before searching.
    debounce: f64,
    /// The whole-cell toggle's state; `None`: the bar has no toggle.
    whole_cell: Option<bool>,
    /// The query the owner's results belong to (last emitted `search`).
    searched: FindQuery,
    focus_pending: bool,
    select_all_pending: bool,
    submit_pending: bool,
    edited_at: Option<f64>,
}

impl FindBar {
    pub fn new(hint: &'static str, debounce: f64) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self {
            id: egui::Id::new(("whir-find", NEXT.fetch_add(1, Ordering::Relaxed))),
            open: false,
            query: String::new(),
            hint,
            debounce,
            whole_cell: None,
            searched: FindQuery::default(),
            focus_pending: false,
            select_all_pending: false,
            submit_pending: false,
            edited_at: None,
        }
    }

    /// With the whole-cell toggle (Alt+W), for tables.
    pub fn with_whole_cell(mut self) -> Self {
        self.whole_cell = Some(false);
        self
    }

    fn edit_id(&self) -> egui::Id {
        self.id.with("edit")
    }

    /// The search field has keyboard focus (so the document must leave keys alone).
    pub fn has_focus(&self, ctx: &egui::Context) -> bool {
        self.open && ctx.memory(|m| m.has_focus(self.edit_id()))
    }

    /// The query the current results belong to.
    pub fn searched(&self) -> &FindQuery {
        &self.searched
    }

    /// What the bar says now (the field and the toggle).
    pub fn current(&self) -> FindQuery {
        FindQuery {
            text: self.query.clone(),
            whole_cell: self.whole_cell.unwrap_or(false),
        }
    }

    /// The owner searched for `q` on its own (Find in all tabs): show it as
    /// if the user had typed it here.
    pub fn set_searched(&mut self, q: &FindQuery) {
        self.query = q.text.chars().take(MAX_QUERY).collect();
        if let Some(w) = &mut self.whole_cell {
            *w = q.whole_cell;
        }
        self.searched = self.current();
        self.edited_at = None;
        self.submit_pending = false;
    }

    /// Ctrl+F, F3, Shift+F3. Always consumed (they mean nothing else anywhere).
    pub fn consume_keys(ui: &mut egui::Ui) -> FindKeys {
        ui.input_mut(|i| FindKeys {
            open: i.consume_key(Modifiers::CTRL, Key::F),
            // Shift+F3 first: a plain F3 pattern also matches with Shift held.
            prev: i.consume_key(Modifiers::SHIFT, Key::F3),
            next: i.consume_key(Modifiers::NONE, Key::F3),
        })
    }

    /// Open (or re-focus) the bar, optionally replacing the query. The text is
    /// selected so typing replaces it.
    pub fn open_with(&mut self, prefill: Option<String>) {
        if let Some(q) = prefill {
            let q: String = q.chars().take(MAX_QUERY).collect();
            if q != self.query {
                self.query = q;
            }
        }
        self.open = true;
        self.focus_pending = true;
        self.select_all_pending = true;
        if !self.query.is_empty() && self.current() != self.searched {
            self.submit_pending = true;
        }
    }

    pub fn close(&mut self) {
        self.open = false;
        self.edited_at = None;
    }

    /// Where a bar without the toggle sits inside `area`: top right, below `top_inset`.
    pub fn rect(area: Rect, top_inset: f32) -> Rect {
        Self::rect_sized(area, top_inset, SIZE)
    }

    fn rect_sized(area: Rect, top_inset: f32, size: egui::Vec2) -> Rect {
        Rect::from_min_size(
            egui::pos2(area.right() - size.x - 20.0, area.top() + top_inset + 8.0),
            size,
        )
    }

    fn size(&self) -> egui::Vec2 {
        match self.whole_cell {
            Some(_) => SIZE + egui::vec2(TOGGLE.x + 4.0, 0.0),
            None => SIZE,
        }
    }

    /// Draw the bar (if open) and report what the user did. Call after the
    /// document content so the bar is on top.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        area: Rect,
        top_inset: f32,
        status: &FindStatus,
    ) -> FindBarOut {
        if !self.open {
            return FindBarOut::default();
        }
        let rect = Self::rect_sized(area, top_inset, self.size());
        // Swallow clicks on the bar's background so they don't reach the document.
        ui.interact(rect, self.id.with("bg"), Sense::CLICK | Sense::DRAG);
        let theme = Theme::current();
        let frame = crate::kit::floating(&theme).inner_margin(egui::Margin::symmetric(8, 4));
        self.show_in(ui, rect, frame, status, 190.0)
    }

    /// The bar's contents in `rect`, without the overlay's frame: a bar docked
    /// in a panel. It counts as open while shown.
    pub fn show_docked(
        &mut self,
        ui: &mut egui::Ui,
        rect: Rect,
        field_width: f32,
        status: &FindStatus,
    ) -> FindBarOut {
        self.open = true;
        let frame = Frame::NONE.inner_margin(egui::Margin::symmetric(0, 4));
        self.show_in(ui, rect, frame, status, field_width)
    }

    fn show_in(
        &mut self,
        ui: &mut egui::Ui,
        rect: Rect,
        frame: Frame,
        status: &FindStatus,
        field_width: f32,
    ) -> FindBarOut {
        let mut out = FindBarOut::default();
        let theme = Theme::current();
        let now = ui.input(|i| i.time);
        let edit_id = self.edit_id();

        // Focus before the field runs: egui collapses the selection of an
        // unfocused text field, which would undo the select-all below.
        if self.focus_pending {
            self.focus_pending = false;
            ui.memory_mut(|m| m.request_focus(edit_id));
        }
        if self.select_all_pending {
            self.select_all_pending = false;
            let mut state =
                egui::text_edit::TextEditState::load(ui.ctx(), edit_id).unwrap_or_default();
            let n = self.query.chars().count();
            state
                .cursor
                .set_char_range(Some(CCursorRange::two(CCursor::new(0), CCursor::new(n))));
            state.store(ui.ctx(), edit_id);
        }

        let mut next = false;
        let mut prev = false;
        let mut close = false;
        let mut toggled = false;
        ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
            frame.show(ui, |ui| {
                ui.set_min_size(rect.size() - frame.inner_margin.sum());
                ui.horizontal_centered(|ui| {
                    let edit = egui::TextEdit::singleline(&mut self.query)
                        .id(edit_id)
                        .hint_text(self.hint)
                        // Enter steps through matches instead of dropping focus.
                        .return_key(None)
                        .char_limit(MAX_QUERY)
                        // Keep focus on Esc (egui would drop it before we see
                        // the key): Esc closes the bar instead.
                        .event_filter(EventFilter {
                            tab: false,
                            horizontal_arrows: true,
                            vertical_arrows: true,
                            escape: true,
                        })
                        .desired_width(field_width);
                    let resp = ui.add(edit);
                    if resp.changed() {
                        self.edited_at = Some(now);
                    }
                    if resp.has_focus() {
                        let (shift_enter, enter, esc, alt_w) = ui.input_mut(|i| {
                            (
                                i.consume_key(Modifiers::SHIFT, Key::Enter),
                                i.consume_key(Modifiers::NONE, Key::Enter),
                                i.consume_key(Modifiers::NONE, Key::Escape),
                                i.consume_key(Modifiers::ALT, Key::W),
                            )
                        });
                        next |= enter;
                        prev |= shift_enter;
                        close |= esc;
                        toggled |= alt_w && self.whole_cell.is_some();
                    }
                    if let Some(on) = self.whole_cell {
                        toggled |= whole_cell_toggle(ui, on, &theme)
                            .on_hover_text(if on {
                                "Whole cell: on (Alt+W)\nA cell must equal the text; \
                                 numbers match by value (2.45 finds 2.4500)"
                            } else {
                                "Whole cell: off (Alt+W)\nCells containing the text match"
                            })
                            .clicked();
                    }
                    let label = if self.query.is_empty() {
                        String::new()
                    } else {
                        status_text(status)
                    };
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        close |= ui
                            .add(egui::Button::new(icons::text(Icon::Close, 12.0)).frame(false))
                            .on_hover_text("Close (Esc)")
                            .clicked();
                        next |= ui
                            .add(egui::Button::new(icons::text(Icon::ArrowDown, 12.0)).frame(false))
                            .on_hover_text("Next (Enter, F3)")
                            .clicked();
                        prev |= ui
                            .add(egui::Button::new(icons::text(Icon::ArrowUp, 12.0)).frame(false))
                            .on_hover_text("Previous (Shift+Enter, Shift+F3)")
                            .clicked();
                        ui.add(
                            egui::Label::new(RichText::new(label).small().color(theme.text_dim))
                                .truncate(),
                        );
                    });
                });
            });
        });

        if toggled && let Some(on) = &mut self.whole_cell {
            *on = !*on;
            if !self.query.is_empty() {
                self.submit_pending = true;
            }
        }

        // Debounced search as you type.
        if let Some(t) = self.edited_at {
            let waited = now - t;
            if waited >= self.debounce {
                self.edited_at = None;
                if self.current() != self.searched {
                    self.submit_pending = true;
                }
            } else {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_secs_f64(
                        self.debounce - waited,
                    ));
            }
        }
        // Enter / arrows on a query that hasn't been searched yet start the search.
        if (next || prev) && self.current() != self.searched {
            self.submit_pending = true;
            next = false;
            prev = false;
        }
        if self.submit_pending {
            self.submit_pending = false;
            self.edited_at = None;
            self.searched = self.current();
            out.search = Some(self.searched.clone());
        }
        out.next = next;
        out.prev = prev;
        if close {
            self.close();
            ui.memory_mut(|m| m.surrender_focus(edit_id));
            out.closed = true;
        }
        out
    }
}

/// The whole-cell toggle: "ab" framed like a cell holding just that text,
/// tinted while on.
fn whole_cell_toggle(ui: &mut egui::Ui, on: bool, theme: &Theme) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(TOGGLE, Sense::CLICK);
    let painter = ui.painter();
    let fg = if on || resp.hovered() {
        theme.text
    } else {
        theme.text_dim
    };
    if on {
        painter.rect_filled(rect, 4.0, theme.accent.gamma_multiply(0.25));
        painter.rect_stroke(
            rect,
            4.0,
            egui::Stroke::new(1.0, theme.accent),
            egui::StrokeKind::Inside,
        );
    } else if resp.hovered() {
        painter.rect_filled(rect, 4.0, theme.tab_hover_bg);
    }
    let cell = Rect::from_center_size(rect.center(), egui::vec2(17.0, 12.0));
    painter.rect_stroke(
        cell,
        1.5,
        egui::Stroke::new(1.0, fg),
        egui::StrokeKind::Inside,
    );
    painter.text(
        cell.center() - egui::vec2(0.0, 0.5),
        egui::Align2::CENTER_CENTER,
        "ab",
        egui::FontId::proportional(theme.metrics.small_text_size),
        fg,
    );
    resp
}

/// "Searching… 34%", "No results", "3 of 1,234+", "57 matches".
pub fn status_text(s: &FindStatus) -> String {
    match s {
        FindStatus::Idle => String::new(),
        FindStatus::Searching { progress: None } => "Searching…".into(),
        FindStatus::Searching { progress: Some(p) } => {
            format!("Searching… {:.0}%", (p * 100.0).clamp(0.0, 100.0))
        }
        FindStatus::Results {
            current,
            total,
            more,
        } => {
            let more = if *more { "+" } else { "" };
            match current {
                Some(c) => format!(
                    "{} of {}{more}",
                    fmt_int(*c as u64 + 1),
                    fmt_int(*total as u64)
                ),
                None => format!(
                    "{}{more} match{}",
                    fmt_int(*total as u64),
                    if *total == 1 && more.is_empty() {
                        ""
                    } else {
                        "es"
                    }
                ),
            }
        }
        FindStatus::NoResults => "No results".into(),
        FindStatus::Message(m) => m.clone(),
    }
}

/// 1234567 → "1,234,567".
pub fn fmt_int(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thousands() {
        assert_eq!(fmt_int(0), "0");
        assert_eq!(fmt_int(999), "999");
        assert_eq!(fmt_int(1000), "1,000");
        assert_eq!(fmt_int(1234567), "1,234,567");
    }

    #[test]
    fn statuses() {
        assert_eq!(status_text(&FindStatus::Idle), "");
        assert_eq!(
            status_text(&FindStatus::Searching {
                progress: Some(0.349)
            }),
            "Searching… 35%"
        );
        assert_eq!(
            status_text(&FindStatus::Results {
                current: Some(2),
                total: 1234,
                more: true
            }),
            "3 of 1,234+"
        );
        assert_eq!(
            status_text(&FindStatus::Results {
                current: None,
                total: 1,
                more: false
            }),
            "1 match"
        );
        assert_eq!(status_text(&FindStatus::NoResults), "No results");
    }

    #[test]
    fn set_searched_does_not_search_again() {
        let mut bar = FindBar::new("x", 0.2).with_whole_cell();
        let q = FindQuery {
            text: "2.45".into(),
            whole_cell: true,
        };
        bar.set_searched(&q);
        assert_eq!(bar.current(), q);
        assert_eq!(bar.searched(), &q);
        bar.open_with(None);
        assert!(!bar.submit_pending, "already searched");

        // A bar without the toggle keeps whole_cell off.
        let mut plain = FindBar::new("x", 0.2);
        plain.set_searched(&q);
        assert!(!plain.current().whole_cell);
    }
}
