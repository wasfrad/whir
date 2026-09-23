//! The find bar shared by the CSV and PDF viewers: a small overlay at the top
//! right with a search field, a match counter and ↑ ↓ ✕.
//!
//! The owner keeps the results; the bar only reports what the user asked for
//! ([`FindBarOut`]) and shows a [`FindStatus`].

use std::sync::atomic::{AtomicU64, Ordering};

use egui::text::{CCursor, CCursorRange};
use egui::{Align, EventFilter, Frame, Key, Layout, Modifiers, Rect, RichText, Sense, UiBuilder};

use crate::theme::Theme;

const SIZE: egui::Vec2 = egui::vec2(390.0, 36.0);
/// Longest query accepted.
const MAX_QUERY: usize = 1000;

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
    /// query, or the bar was just opened with text).
    pub search: Option<String>,
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
    /// The query the owner's results belong to (last emitted `search`).
    searched: String,
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
            searched: String::new(),
            focus_pending: false,
            select_all_pending: false,
            submit_pending: false,
            edited_at: None,
        }
    }

    fn edit_id(&self) -> egui::Id {
        self.id.with("edit")
    }

    /// The search field has keyboard focus (so the document must leave keys alone).
    pub fn has_focus(&self, ctx: &egui::Context) -> bool {
        self.open && ctx.memory(|m| m.has_focus(self.edit_id()))
    }

    /// The query the current results belong to.
    pub fn searched(&self) -> &str {
        &self.searched
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
        if !self.query.is_empty() && self.query != self.searched {
            self.submit_pending = true;
        }
    }

    pub fn close(&mut self) {
        self.open = false;
        self.edited_at = None;
    }

    /// Where the bar sits inside `area`: top right, below `top_inset`.
    pub fn rect(area: Rect, top_inset: f32) -> Rect {
        Rect::from_min_size(
            egui::pos2(area.right() - SIZE.x - 20.0, area.top() + top_inset + 8.0),
            SIZE,
        )
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
        let mut out = FindBarOut::default();
        if !self.open {
            return out;
        }
        let theme = Theme::current();
        let now = ui.input(|i| i.time);
        let rect = Self::rect(area, top_inset);
        let edit_id = self.edit_id();
        // Swallow clicks on the bar's background so they don't reach the document.
        ui.interact(rect, self.id.with("bg"), Sense::CLICK | Sense::DRAG);

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
        ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
            Frame::NONE
                .fill(theme.overlay_bg)
                .stroke(egui::Stroke::new(1.0, theme.separator))
                .corner_radius(8.0)
                .inner_margin(egui::Margin::symmetric(8, 4))
                .show(ui, |ui| {
                    ui.set_min_size(SIZE - egui::vec2(16.0, 8.0));
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
                            .desired_width(190.0);
                        let resp = ui.add(edit);
                        if resp.changed() {
                            self.edited_at = Some(now);
                        }
                        if resp.has_focus() {
                            let (shift_enter, enter, esc) = ui.input_mut(|i| {
                                (
                                    i.consume_key(Modifiers::SHIFT, Key::Enter),
                                    i.consume_key(Modifiers::NONE, Key::Enter),
                                    i.consume_key(Modifiers::NONE, Key::Escape),
                                )
                            });
                            next |= enter;
                            prev |= shift_enter;
                            close |= esc;
                        }
                        let label = if self.query.is_empty() {
                            String::new()
                        } else {
                            status_text(status)
                        };
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            close |= ui
                                .add(egui::Button::new("×").frame(false))
                                .on_hover_text("Close (Esc)")
                                .clicked();
                            next |= ui
                                .add(egui::Button::new("↓").frame(false))
                                .on_hover_text("Next (Enter, F3)")
                                .clicked();
                            prev |= ui
                                .add(egui::Button::new("↑").frame(false))
                                .on_hover_text("Previous (Shift+Enter, Shift+F3)")
                                .clicked();
                            ui.label(RichText::new(label).small().color(theme.text_dim));
                        });
                    });
                });
        });

        // Debounced search as you type.
        if let Some(t) = self.edited_at {
            let waited = now - t;
            if waited >= self.debounce {
                self.edited_at = None;
                if self.query != self.searched {
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
        if (next || prev) && self.query != self.searched {
            self.submit_pending = true;
            next = false;
            prev = false;
        }
        if self.submit_pending {
            self.submit_pending = false;
            self.edited_at = None;
            self.searched = self.query.clone();
            out.search = Some(self.query.clone());
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
        if i > 0 && (s.len() - i) % 3 == 0 {
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
}
