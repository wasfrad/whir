//! Find in all tabs: a results panel along the bottom of the window, like
//! Notepad++'s search results. One query runs in every open table at once;
//! results are grouped by file, and going to one switches to its tab with the
//! cell selected.
//!
//! The documents own the results — each tab's own find shows the same query,
//! so F3 in a tab steps through that file's matches and the panel follows —
//! and the panel only lists them, describing just the rows it draws.

use std::collections::HashSet;
use std::time::Duration;

use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{
    Align2, Event, FontId, Frame, Key, Modifiers, Panel, Rect, Sense, Stroke, UiBuilder, pos2, vec2,
};
use whir_core::find_bar::{FindBar, FindStatus, fmt_int};
use whir_core::theme::{self, Theme};
use whir_core::{FindQuery, SearchHit, SearchState};

use crate::app::Tab;
use crate::keys::consume_exact;
use crate::tab_bar::paint_kind_glyph;

const HEADER_H: f32 = 38.0;
const ROW_H: f32 = 24.0;
/// Left edge of a match's text; its row number is right-aligned before it.
const HIT_TEXT_X: f32 = 118.0;
/// Matches listed per file (F3 in the tab reaches the rest).
const MAX_LISTED: usize = 50_000;
const DEFAULT_HEIGHT: f32 = 240.0;
const MIN_HEIGHT: f32 = 110.0;

/// What the panel asks the shell to do.
#[derive(Default)]
pub struct Out {
    /// Search every table for this.
    pub search: Option<FindQuery>,
    /// Switch to this tab: a match in it was chosen.
    pub activate: Option<u64>,
}

pub struct SearchPanel {
    open: bool,
    bar: FindBar,
    /// The query the listed results are for, and the tabs it ran in.
    query: FindQuery,
    searched: Vec<u64>,
    /// Tab whose current match is the selected result.
    selected: Option<u64>,
    /// The (tab, match) the list last scrolled to.
    followed: Option<(u64, usize)>,
    collapsed: HashSet<u64>,
    /// The list has the keyboard: it was clicked, and nothing else took it since.
    list_focus: bool,
    scroll_to_selected: bool,
    /// A new search: the list starts at the top.
    scroll_to_top: bool,
    /// List scroll offset, last frame.
    offset: f32,
    list_rect: Rect,
}

impl Default for SearchPanel {
    fn default() -> Self {
        Self {
            open: false,
            bar: FindBar::new("Find in all open tables", 0.3).with_whole_cell(),
            query: FindQuery::default(),
            searched: Vec::new(),
            selected: None,
            followed: None,
            collapsed: HashSet::new(),
            list_focus: false,
            scroll_to_selected: false,
            scroll_to_top: false,
            offset: 0.0,
            list_rect: Rect::NOTHING,
        }
    }
}

/// One searched file, as listed.
struct Group {
    /// Index in `tabs`.
    tab: usize,
    id: u64,
    /// Its results for the panel's query (`None`: the tab searched for
    /// something else since).
    state: Option<SearchState>,
    /// The query the tab shows now, when it isn't the panel's.
    other: Option<FindQuery>,
    collapsed: bool,
}

impl Group {
    fn hits(&self) -> usize {
        self.state.as_ref().map_or(0, |s| s.hits)
    }

    fn listed(&self) -> usize {
        if self.collapsed {
            0
        } else {
            self.hits().min(MAX_LISTED)
        }
    }

    /// Rows in the list: the file, its matches, and "N more" past the cap.
    fn rows(&self) -> usize {
        1 + self.listed() + usize::from(!self.collapsed && self.hits() > MAX_LISTED)
    }
}

/// The whole search at a glance.
#[derive(Default)]
struct Summary {
    hits: usize,
    files_with_hits: usize,
    /// Searched tabs still open.
    files: usize,
    running: bool,
    progress: Option<f32>,
    truncated: bool,
}

/// What a list row is.
enum Row {
    File,
    Hit(usize),
    More,
}

impl SearchPanel {
    /// The panel's field or list has the keyboard: documents must leave keys alone.
    pub fn has_focus(&self, ctx: &egui::Context) -> bool {
        self.open && (self.list_focus || self.bar.has_focus(ctx))
    }

    /// Ctrl+Shift+F: open with the field focused and its text selected.
    pub fn focus(&mut self) {
        self.open = true;
        self.list_focus = false;
        self.bar.open_with(None);
    }

    /// A document asked to search every tab for `q`: show it as searched.
    pub fn show_query(&mut self, q: &FindQuery) {
        self.open = true;
        self.bar.set_searched(q);
    }

    /// The search for `q` started in these tabs.
    pub fn started(&mut self, q: FindQuery, tabs: Vec<u64>) {
        self.query = q;
        self.searched = tabs;
        self.selected = None;
        self.followed = None;
        self.collapsed.clear();
        self.scroll_to_top = true;
    }

    pub fn close(&mut self, ctx: &egui::Context) {
        if self.bar.has_focus(ctx) {
            ctx.memory_mut(|m| {
                if let Some(id) = m.focused() {
                    m.surrender_focus(id);
                }
            });
        }
        self.open = false;
        self.list_focus = false;
        self.bar.close();
    }

    fn groups(&self, tabs: &[Tab]) -> Vec<Group> {
        tabs.iter()
            .enumerate()
            .filter(|(_, t)| self.searched.contains(&t.id))
            .filter_map(|(i, t)| {
                let state = t.doc.search_state()?;
                let ours = state.query == self.query;
                // Finished without a match: counted, not listed.
                if ours && state.hits == 0 && !state.running {
                    return None;
                }
                Some(Group {
                    tab: i,
                    id: t.id,
                    other: (!ours).then(|| state.query.clone()),
                    state: ours.then_some(state),
                    collapsed: self.collapsed.contains(&t.id),
                })
            })
            .collect()
    }

    fn summary(&self, tabs: &[Tab], groups: &[Group]) -> Summary {
        let mut s = Summary {
            files: tabs
                .iter()
                .filter(|t| self.searched.contains(&t.id))
                .count(),
            ..Default::default()
        };
        let mut progress = Vec::new();
        for st in groups.iter().filter_map(|g| g.state.as_ref()) {
            s.hits += st.hits;
            s.files_with_hits += usize::from(st.hits > 0);
            s.truncated |= st.truncated;
            if st.running {
                s.running = true;
                progress.push(st.progress.unwrap_or(0.0));
            }
        }
        if !progress.is_empty() {
            s.progress = Some(progress.iter().sum::<f32>() / progress.len() as f32);
        }
        s
    }

    fn status(&self, s: &Summary) -> FindStatus {
        if self.query.is_empty() || self.bar.current() != self.query {
            return FindStatus::Idle;
        }
        if s.files == 0 {
            return FindStatus::Message("No tables open".into());
        }
        let files = |n: usize| if n == 1 { "file" } else { "files" };
        if s.hits == 0 {
            return if s.running {
                FindStatus::Searching {
                    progress: s.progress,
                }
            } else {
                FindStatus::Message(format!("No results in {} {}", s.files, files(s.files)))
            };
        }
        let more = if s.truncated { "+" } else { "" };
        let mut text = format!(
            "{}{more} match{} in {} of {} {}",
            fmt_int(s.hits as u64),
            if s.hits == 1 && more.is_empty() {
                ""
            } else {
                "es"
            },
            s.files_with_hits,
            s.files,
            files(s.files)
        );
        if s.running {
            text.push_str("  ·  searching…");
        }
        FindStatus::Message(text)
    }

    /// The match after (or before) the selected one, across files, wrapping
    /// around. Collapsed files are skipped unless `into_collapsed`.
    fn neighbor(
        &self,
        groups: &[Group],
        forward: bool,
        into_collapsed: bool,
    ) -> Option<(u64, usize)> {
        let hits: Vec<(u64, usize)> = groups
            .iter()
            .filter(|g| into_collapsed || !g.collapsed)
            .map(|g| (g.id, g.hits()))
            .filter(|&(_, n)| n > 0)
            .collect();
        if hits.is_empty() {
            return None;
        }
        let current = self.selected.and_then(|id| {
            let g = groups.iter().find(|g| g.id == id)?;
            let k = hits.iter().position(|&(h, _)| h == id)?;
            Some((k, g.state.as_ref()?.current?))
        });
        let n = hits.len();
        Some(match current {
            None if forward => (hits[0].0, 0),
            None => (hits[n - 1].0, hits[n - 1].1 - 1),
            Some((k, i)) if forward => {
                if i + 1 < hits[k].1 {
                    (hits[k].0, i + 1)
                } else {
                    (hits[(k + 1) % n].0, 0)
                }
            }
            Some((k, i)) => {
                if i > 0 {
                    (hits[k].0, (i - 1).min(hits[k].1 - 1))
                } else {
                    let p = (k + n - 1) % n;
                    (hits[p].0, hits[p].1 - 1)
                }
            }
        })
    }

    /// Select match `i` of tab `id` and bring it into view there.
    fn go(&mut self, tabs: &mut [Tab], id: u64, i: usize) -> Option<u64> {
        let t = tabs.iter_mut().find(|t| t.id == id)?;
        t.doc.reveal_hit(i);
        self.selected = Some(id);
        self.followed = Some((id, i));
        self.collapsed.remove(&id);
        self.scroll_to_selected = true;
        Some(id)
    }

    /// F4 / Shift+F4: the next / previous result, in any file. Returns the
    /// tab to switch to.
    pub fn step(&mut self, tabs: &mut [Tab], forward: bool) -> Option<u64> {
        if self.query.is_empty() {
            return None;
        }
        self.open = true;
        let groups = self.groups(tabs);
        let (id, i) = self.neighbor(&groups, forward, true)?;
        self.go(tabs, id, i)
    }

    /// Draw the panel (call before the document area) and report what the user did.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        tabs: &mut [Tab],
        active: Option<u64>,
        theme: &Theme,
    ) -> Out {
        let mut out = Out::default();
        let ctx = ui.ctx().clone();
        // A press outside the list, or another field taking focus, ends the
        // list's hold on the keyboard.
        if self.list_focus {
            let pressed_elsewhere = ctx.input(|i| {
                i.pointer.any_pressed()
                    && i.pointer
                        .press_origin()
                        .is_some_and(|p| !self.list_rect.contains(p))
            });
            if pressed_elsewhere || ctx.memory(|m| m.focused()).is_some() {
                self.list_focus = false;
            }
        }
        let max = (ui.available_height() - 120.0).max(MIN_HEIGHT);
        let mut open = self.open;
        Panel::bottom("whir.find_in_tabs")
            .resizable(true)
            .drag_to_open(false)
            .show_separator_line(false)
            .default_size(DEFAULT_HEIGHT)
            .size_range(MIN_HEIGHT..=max)
            .frame(Frame::NONE.fill(theme.drawer_bg))
            .show_collapsible(ui, &mut open, |ui| {
                self.contents(ui, tabs, active, theme, &mut out);
            });
        if self.open && !open {
            // Dragged shut.
            self.close(&ctx);
        }
        out
    }

    fn contents(
        &mut self,
        ui: &mut egui::Ui,
        tabs: &mut [Tab],
        active: Option<u64>,
        theme: &Theme,
        out: &mut Out,
    ) {
        let rect = ui.max_rect();
        // Claim the whole panel: egui sizes a panel by what its contents use,
        // and an empty list would shrink it for good.
        ui.expand_to_include_rect(rect);
        if !self.open {
            return; // sliding shut
        }
        let painter = ui.painter().clone();
        painter.line_segment(
            [rect.left_top(), rect.right_top()],
            Stroke::new(1.0, theme.separator),
        );

        // The active tab's current match is the selection (F3 there moves it).
        if let Some(id) = active
            && self.searched.contains(&id)
            && let Some(t) = tabs.iter().find(|t| t.id == id)
            && let Some(st) = t.doc.search_state().filter(|s| s.query == self.query)
            && let Some(c) = st.current
            && self.followed != Some((id, c))
        {
            self.selected = Some(id);
            self.followed = Some((id, c));
            self.collapsed.remove(&id);
            self.scroll_to_selected = true;
        }

        let mut groups = self.groups(tabs);
        let summary = self.summary(tabs, &groups);
        if summary.running {
            ui.ctx().request_repaint_after(Duration::from_millis(150));
        }

        // Header: title, then the docked find bar.
        let header = Rect::from_min_size(rect.min, vec2(rect.width(), HEADER_H));
        let title = painter.layout_no_wrap(
            "Find in all tabs".into(),
            FontId::new(theme.metrics.body_text_size, theme::family(theme::SEMIBOLD)),
            theme.text,
        );
        let title_w = title.size().x;
        painter.galley(
            pos2(
                header.left() + 14.0,
                header.center().y - title.size().y / 2.0,
            ),
            title,
            theme.text,
        );
        let bar_rect = Rect::from_min_max(
            pos2(header.left() + 14.0 + title_w + 16.0, header.top() + 2.0),
            pos2(header.right() - 10.0, header.bottom() - 2.0),
        );
        let field_w = (bar_rect.width() * 0.45).clamp(140.0, 340.0);
        let status = self.status(&summary);
        let bar = self.bar.show_docked(ui, bar_rect, field_w, &status);
        if let Some(q) = bar.search {
            out.search = Some(q);
        }
        let mut step = None;
        if bar.next {
            step = Some(true);
        }
        if bar.prev {
            step = Some(false);
        }
        if bar.closed {
            self.close(ui.ctx());
            return;
        }
        painter.line_segment(
            [
                pos2(rect.left(), header.bottom()),
                pos2(rect.right(), header.bottom()),
            ],
            Stroke::new(1.0, theme.separator),
        );

        let list = Rect::from_min_max(pos2(rect.left(), header.bottom() + 1.0), rect.max);
        self.list_rect = list;

        // The list's keys.
        let mut enter = false;
        if self.list_focus && self.open {
            ui.input_mut(|i| {
                let none = Modifiers::NONE;
                if consume_exact(i, none, Key::ArrowDown) {
                    step = Some(true);
                }
                if consume_exact(i, none, Key::ArrowUp) {
                    step = Some(false);
                }
                enter = consume_exact(i, none, Key::Enter);
                if consume_exact(i, none, Key::Escape) {
                    self.open = false;
                }
            });
            if !self.open {
                self.close(ui.ctx());
                return;
            }
        }
        if let Some(forward) = step {
            // From the field or the list: collapsed files stay folded.
            let into = !self.list_focus;
            if let Some((id, i)) = self.neighbor(&groups, forward, into) {
                out.activate = self.go(tabs, id, i);
                groups = self.groups(tabs);
            }
        } else if enter
            && let Some(id) = self.selected
            && let Some(c) = groups
                .iter()
                .find(|g| g.id == id)
                .and_then(|g| g.state.as_ref()?.current)
        {
            out.activate = self.go(tabs, id, c);
        }

        if groups.is_empty() {
            let hint = if self.query.is_empty() {
                "Type to search every open table at once.   Alt+W: whole cell   ·   F4: next result"
            } else if summary.files == 0 {
                "Open CSV files to search them together."
            } else {
                ""
            };
            painter.text(
                list.center(),
                Align2::CENTER_CENTER,
                hint,
                FontId::proportional(theme.metrics.small_text_size + 0.5),
                theme.text_dim,
            );
            return;
        }

        // Rows: each file, then its matches.
        let mut starts = Vec::with_capacity(groups.len());
        let mut total = 0usize;
        for g in &groups {
            starts.push(total);
            total += g.rows();
        }
        let locate = |item: usize| -> (usize, Row) {
            let g = starts.partition_point(|&s| s <= item) - 1;
            match item - starts[g] {
                0 => (g, Row::File),
                k if k - 1 < groups[g].listed() => (g, Row::Hit(k - 1)),
                _ => (g, Row::More),
            }
        };
        let selected_item = self.selected.and_then(|id| {
            let g = groups.iter().position(|g| g.id == id)?;
            let c = groups[g].state.as_ref()?.current?;
            (c < groups[g].listed()).then(|| starts[g] + 1 + c)
        });

        let mut scroll = egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .id_salt("whir.find_in_tabs.list");
        if std::mem::take(&mut self.scroll_to_top) {
            scroll = scroll.vertical_scroll_offset(0.0);
        } else if std::mem::take(&mut self.scroll_to_selected)
            && let Some(item) = selected_item
        {
            let y = item as f32 * ROW_H;
            let view = list.height();
            let off = if y < self.offset {
                y
            } else if y + ROW_H > self.offset + view {
                y + ROW_H - view
            } else {
                self.offset
            };
            scroll = scroll.vertical_scroll_offset(off);
        }

        let mut clicked: Option<(usize, Row)> = None;
        let query = self.query.clone();
        let selected = self.selected;
        let small = FontId::proportional(theme.metrics.small_text_size);
        let body = FontId::proportional(theme.metrics.body_text_size);
        let res = ui
            .scope_builder(UiBuilder::new().max_rect(list), |ui| {
                // Clicks between rows still give the list the keyboard.
                let bg = ui.interact(list, ui.id().with("bg"), Sense::click());
                if bg.clicked() {
                    self.list_focus = true;
                }
                // Rows touch: one row is exactly ROW_H (scrolling to one relies on it).
                ui.spacing_mut().item_spacing.y = 0.0;
                scroll.show_rows(ui, ROW_H, total, |ui, range| {
                    for item in range {
                        let (rect, resp) =
                            ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
                        let (g, row) = locate(item);
                        let grp = &groups[g];
                        let p = ui.painter();
                        let is_selected = matches!(row, Row::Hit(i)
                            if selected == Some(grp.id)
                                && grp.state.as_ref().and_then(|s| s.current) == Some(i));
                        if is_selected {
                            p.rect_filled(rect, 4.0, theme.drawer_item_active);
                        } else if resp.hovered() && !matches!(row, Row::More) {
                            p.rect_filled(rect, 4.0, theme.drawer_item_hover);
                        }
                        let cy = rect.center().y;
                        match row {
                            Row::File => {
                                let t = &tabs[grp.tab];
                                paint_file_row(p, rect, grp, t, theme, &small);
                            }
                            Row::Hit(i) => {
                                let hit = tabs[grp.tab].doc.search_hit(i).unwrap_or_default();
                                p.text(
                                    pos2(rect.left() + HIT_TEXT_X - 12.0, cy),
                                    Align2::RIGHT_CENTER,
                                    &hit.place,
                                    small.clone(),
                                    theme.text_dim,
                                );
                                let x = rect.left() + HIT_TEXT_X;
                                let job =
                                    hit_job(&hit, &query, theme, &body, &small, rect.right() - 8.0 - x);
                                let galley = p.layout_job(job);
                                p.galley(pos2(x, cy - galley.size().y / 2.0), galley, theme.text);
                            }
                            Row::More => {
                                let more = grp.hits() - MAX_LISTED;
                                p.text(
                                    pos2(rect.left() + HIT_TEXT_X, cy),
                                    Align2::LEFT_CENTER,
                                    format!(
                                        "…and {} more in this file: F3 in its tab steps through them",
                                        fmt_int(more as u64)
                                    ),
                                    small.clone(),
                                    theme.text_dim,
                                );
                            }
                        }
                        if resp.clicked() {
                            clicked = Some((g, row));
                        }
                    }
                })
            })
            .inner;
        self.offset = res.state.offset.y;

        if let Some((g, row)) = clicked {
            self.list_focus = true;
            ui.ctx().memory_mut(|m| {
                if let Some(id) = m.focused() {
                    m.surrender_focus(id);
                }
            });
            let id = groups[g].id;
            match row {
                Row::File => {
                    if !self.collapsed.remove(&id) {
                        self.collapsed.insert(id);
                    }
                }
                Row::Hit(i) => out.activate = self.go(tabs, id, i),
                Row::More => {}
            }
        }
    }
}

/// A file's row: chevron, kind glyph, name, how many matches, its folder.
fn paint_file_row(
    p: &egui::Painter,
    rect: Rect,
    g: &Group,
    t: &Tab,
    theme: &Theme,
    small: &FontId,
) {
    let cy = rect.center().y;
    // ▾ open / ▸ folded.
    let c = pos2(rect.left() + 14.0, cy);
    let pts = if g.collapsed {
        vec![
            c + vec2(-2.0, -4.0),
            c + vec2(2.5, 0.0),
            c + vec2(-2.0, 4.0),
        ]
    } else {
        vec![
            c + vec2(-4.0, -2.0),
            c + vec2(4.0, -2.0),
            c + vec2(0.0, 2.5),
        ]
    };
    p.add(egui::Shape::convex_polygon(
        pts,
        theme.text_dim,
        Stroke::NONE,
    ));
    paint_kind_glyph(p, pos2(rect.left() + 32.0, cy), t.doc.kind(), theme);

    let title = t.doc.title();
    theme::note_text(title);
    let name = p.layout_no_wrap(
        title.to_owned(),
        FontId::new(theme.metrics.body_text_size, theme::family(theme::SEMIBOLD)),
        theme.text,
    );
    let name_x = rect.left() + 46.0;
    let name_w = name.size().x;
    p.galley(pos2(name_x, cy - name.size().y / 2.0), name, theme.text);

    let note = match (&g.state, &g.other) {
        (Some(s), _) => {
            let n = fmt_int(s.hits as u64);
            let more = if s.truncated { "+" } else { "" };
            let mut text = format!(
                "{n}{more} match{}",
                if s.hits == 1 && !s.truncated {
                    ""
                } else {
                    "es"
                }
            );
            if s.running {
                match s.progress {
                    Some(pr) => text.push_str(&format!("  ·  searching {:.0}%", pr * 100.0)),
                    None => text.push_str("  ·  searching…"),
                }
            }
            text
        }
        (None, Some(q)) if q.is_empty() => "search cleared in this tab".to_owned(),
        (None, Some(q)) => format!("this tab now shows “{}”", q.text),
        (None, None) => String::new(),
    };
    let note_x = name_x + name_w + 10.0;
    let note = p.layout_no_wrap(note, small.clone(), theme.text_dim);
    let note_w = note.size().x;
    p.galley(pos2(note_x, cy - note.size().y / 2.0), note, theme.text_dim);

    // The folder, right-aligned, if there's room.
    if let Some(dir) = t.doc.path().and_then(|p| p.parent()) {
        let left = note_x + note_w + 24.0;
        let room = rect.right() - 10.0 - left;
        if room > 60.0 {
            let dir = dir.display().to_string();
            theme::note_text(&dir);
            let mut job =
                LayoutJob::single_section(dir, TextFormat::simple(small.clone(), theme.text_dim));
            job.wrap = TextWrapping {
                max_width: room,
                max_rows: 1,
                break_anywhere: true,
                overflow_character: Some('…'),
            };
            let g = p.layout_job(job);
            p.galley(
                pos2(rect.right() - 10.0 - g.size().x, cy - g.size().y / 2.0),
                g,
                theme.text_dim,
            );
        }
    }
}

/// A match on one line: its column, the value with the query highlighted,
/// then the rest of the row, cut off with "…".
fn hit_job(
    hit: &SearchHit,
    query: &FindQuery,
    theme: &Theme,
    body: &FontId,
    small: &FontId,
    max_width: f32,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    let dim = TextFormat::simple(small.clone(), theme.text_dim);
    theme::note_text(&hit.field);
    theme::note_text(&hit.text);
    theme::note_text(&hit.context);
    if !hit.field.is_empty() {
        let field: String = match hit.field.char_indices().nth(28) {
            Some((i, _)) => format!("{}…", &hit.field[..i]),
            None => hit.field.clone(),
        };
        job.append(&field, 0.0, dim.clone());
        job.append("  ", 0.0, dim.clone());
    }
    for (text, marked) in marked_runs(&hit.text, query) {
        let mut f = TextFormat::simple(body.clone(), theme.text);
        if marked {
            f.background = theme.highlight_bg;
        }
        job.append(text, 0.0, f);
    }
    if !hit.context.is_empty() {
        job.append(&hit.context, 28.0, dim);
    }
    job.wrap = TextWrapping {
        max_width: max_width.max(20.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    job
}

/// `text` split into runs, marked where the query matches: all of it for a
/// whole-cell search, each occurrence otherwise (ASCII; other text is
/// marked whole, since case folding can change its length).
fn marked_runs<'t>(text: &'t str, query: &FindQuery) -> Vec<(&'t str, bool)> {
    let needle = query.text.to_ascii_lowercase();
    if query.whole_cell || needle.is_empty() || !text.is_ascii() || !needle.is_ascii() {
        return vec![(text, true)];
    }
    let lower = text.to_ascii_lowercase();
    let mut runs = Vec::new();
    let mut at = 0;
    for (i, _) in lower.match_indices(&needle) {
        if i < at {
            continue;
        }
        if i > at {
            runs.push((&text[at..i], false));
        }
        runs.push((&text[i..i + needle.len()], true));
        at = i + needle.len();
    }
    if at < text.len() {
        runs.push((&text[at..], false));
    }
    if runs.iter().all(|&(_, m)| !m) {
        // Cut off before the match ("…"): mark the value.
        return vec![(text, true)];
    }
    runs
}

/// Keep only events a document may see while the panel has the keyboard:
/// pointer input, and the keys that steer a tab's own find (Ctrl+F, F3).
pub fn keep_for_documents(e: &Event) -> bool {
    match e {
        Event::Key { key, modifiers, .. } => {
            (*key == Key::F && modifiers.matches_exact(Modifiers::CTRL))
                || (*key == Key::F3
                    && (modifiers.matches_exact(Modifiers::NONE)
                        || modifiers.matches_exact(Modifiers::SHIFT)))
        }
        Event::Text(_) | Event::Paste(_) | Event::Copy | Event::Cut => false,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(id: u64, hits: usize, current: Option<usize>, collapsed: bool) -> Group {
        Group {
            tab: id as usize,
            id,
            state: Some(SearchState {
                query: FindQuery::new("x"),
                hits,
                running: false,
                progress: None,
                truncated: false,
                current,
            }),
            other: None,
            collapsed,
        }
    }

    #[test]
    fn stepping_crosses_files_and_wraps() {
        let mut p = SearchPanel::default();
        let groups = vec![
            group(1, 2, None, false),
            group(2, 0, None, false),
            group(3, 1, None, false),
        ];
        assert_eq!(p.neighbor(&groups, true, true), Some((1, 0)));
        assert_eq!(p.neighbor(&groups, false, true), Some((3, 0)));

        p.selected = Some(1);
        let groups = vec![group(1, 2, Some(1), false), group(3, 1, None, false)];
        assert_eq!(p.neighbor(&groups, true, true), Some((3, 0)));
        assert_eq!(p.neighbor(&groups, false, true), Some((1, 0)));

        p.selected = Some(3);
        let groups = vec![group(1, 2, None, false), group(3, 1, Some(0), false)];
        assert_eq!(p.neighbor(&groups, true, true), Some((1, 0)), "wraps");
        assert_eq!(p.neighbor(&groups, false, true), Some((1, 1)));
    }

    #[test]
    fn collapsed_files_are_skipped_from_the_list() {
        let p = SearchPanel {
            selected: Some(1),
            ..Default::default()
        };
        let groups = vec![
            group(1, 1, Some(0), false),
            group(2, 5, None, true),
            group(3, 1, None, false),
        ];
        assert_eq!(p.neighbor(&groups, true, false), Some((3, 0)));
        assert_eq!(p.neighbor(&groups, true, true), Some((2, 0)));
    }

    #[test]
    fn rows_per_file() {
        assert_eq!(group(1, 3, None, false).rows(), 4);
        assert_eq!(group(1, 3, None, true).rows(), 1);
        assert_eq!(group(1, MAX_LISTED + 5, None, false).rows(), MAX_LISTED + 2);
    }

    #[test]
    fn marks_each_occurrence() {
        let q = FindQuery::new("ab");
        assert_eq!(
            marked_runs("xAbyab", &q),
            [("x", false), ("Ab", true), ("y", false), ("ab", true)]
        );
        let whole = FindQuery {
            text: "2.45".into(),
            whole_cell: true,
        };
        assert_eq!(marked_runs("2.4500", &whole), [("2.4500", true)]);
        assert_eq!(
            marked_runs("école", &FindQuery::new("éc")),
            [("école", true)]
        );
    }

    #[test]
    fn documents_keep_pointer_and_find_keys() {
        let key = |key, modifiers| Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        };
        assert!(keep_for_documents(&key(Key::F3, Modifiers::NONE)));
        assert!(keep_for_documents(&key(Key::F3, Modifiers::SHIFT)));
        assert!(keep_for_documents(&key(Key::F, Modifiers::CTRL)));
        assert!(!keep_for_documents(&key(Key::A, Modifiers::NONE)));
        assert!(!keep_for_documents(&key(Key::Delete, Modifiers::NONE)));
        assert!(!keep_for_documents(&Event::Text("a".into())));
        assert!(keep_for_documents(&Event::PointerMoved(pos2(1.0, 1.0))));
    }
}
