//! The timesheet: the week at a glance and where its time is put down. A
//! row per project, a column per day, hours as they go on the timecard;
//! click a day and type its hours, or add a project from the last row. The
//! entries behind a day are listed under it, and the week is closed out to
//! CSV from here.

use egui::text::{CCursor, CCursorRange, LayoutJob, TextFormat, TextWrapping};
use egui::{
    Align, Align2, CornerRadius, FontId, Id, Key, Layout, Modifiers, RichText, Sense, Stroke,
    StrokeKind, UiBuilder, pos2, vec2,
};
use whir_core::icons::{self, Icon};
use whir_core::kit;
use whir_core::theme::Theme;
use whir_core::time::{Stamp, civil_from_days, format_clock, format_date, format_hm, monday_of};
use whir_core::timesheet::{Entry, Grid, Rounding, State, Store, Week, csv_paths, timesheet_tsv};
use whir_core::{DocCx, DocRequest};

use super::{clock_text, edit_line, heading, note, parse_clock, parse_duration, toast};

const DAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const HEAD_H: f32 = 36.0;
const ROW_H: f32 = 34.0;
const CUSTOMER_H: f32 = 30.0;
const DAY_W: f32 = 74.0;
const TOTAL_W: f32 = 86.0;
const PROJECT_MIN: f32 = 220.0;
/// Inside a cell, from its edges to the hours.
const CELL_PAD: f32 = 12.0;
const MIDNIGHT: u32 = 24 * 3600;

/// An entry's start, end and note, as changed.
type Span = (Stamp, Stamp, String);

pub struct View {
    /// The week shown.
    monday: i64,
    /// The cell whose entries are listed: a timer, and a day (0 = Monday).
    pub(super) selected: Option<(u32, usize)>,
    /// Hours being typed into the selected cell.
    editing: Option<Editing>,
    /// Projects added to the week from its last row (no time yet).
    added: Vec<u32>,
    adder: Adder,
    pub(super) confirm_close: bool,
    /// Closed weeks: Monday, when, rounded hours. Read when first shown.
    archive: Option<Vec<(i64, Stamp, f64)>>,
    /// Earlier weeks with time in them that are still open.
    pub(super) open_before: Option<Vec<i64>>,
    error: Option<String>,
}

struct Editing {
    text: String,
    /// What the cell showed: unchanged text changes nothing (the exact time
    /// isn't snapped to its rounding).
    shown: String,
    /// Take the keyboard (and select it all) when next drawn.
    focus: bool,
}

/// The last row: a customer, then one of its projects; or a new one of
/// either being named.
#[derive(Default)]
struct Adder {
    customer: Option<u32>,
    new_customer: Option<String>,
    /// A new project's name and code.
    new_project: Option<(String, String)>,
    focus: bool,
}

/// What a frame of the table asks for, done once it's drawn.
enum Act {
    /// Type into this cell; it shows this.
    Edit(u32, usize, String),
    /// Put down what was typed, then perhaps go on to the cell beside it
    /// (with what that one shows).
    Commit {
        timer: u32,
        day: usize,
        text: String,
        shown: String,
        next: Option<(usize, String)>,
    },
    Cancel,
    /// A project's row, to put its hours in.
    AddRow(u32),
    NewCustomer(String),
    NewProject(u32, String, String),
}

/// "October 26 – November 1, 2026".
fn week_title(monday: i64) -> String {
    let (year, ..) = civil_from_days(monday + 6);
    format!(
        "{} – {}, {year}",
        format_date(&Stamp::midnight(monday).local(), false),
        format_date(&Stamp::midnight(monday + 6).local(), false)
    )
}

fn hours(h: f64) -> String {
    if h == 0.0 {
        return String::new();
    }
    let text = format!("{h:.2}");
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// A project's name, its code dimmed beside it; cut short to `width`.
fn name_job(name: &str, code: &str, width: f32, theme: &Theme) -> LayoutJob {
    let font = FontId::proportional(theme.metrics.body_text_size);
    let mut job = LayoutJob::default();
    job.append(name, 0.0, TextFormat::simple(font.clone(), theme.text));
    if !code.is_empty() {
        job.append(code, 10.0, TextFormat::simple(font, theme.text_dim));
    }
    job.wrap = TextWrapping {
        max_width: width.max(10.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    job
}

/// Give a text field the keyboard with all of its text selected.
fn prime(ctx: &egui::Context, id: Id, text: &str) {
    let mut state = egui::text_edit::TextEditState::default();
    let n = text.chars().count();
    state
        .cursor
        .set_char_range(Some(CCursorRange::two(CCursor::new(0), CCursor::new(n))));
    state.store(ctx, id);
    ctx.memory_mut(|m| m.request_focus(id));
}

/// Seconds for hours as typed: "2", "1.5", "1:30", "90m"; "" or "0" for
/// none. `None` if it isn't a length of time.
fn typed_seconds(text: &str) -> Option<i64> {
    let t = text.trim();
    if t.chars().all(|c| matches!(c, '0' | ':' | '.' | ' ')) {
        return Some(0);
    }
    parse_duration(t)
}

impl View {
    pub fn new() -> View {
        View {
            monday: monday_of(Stamp::now().day),
            selected: None,
            editing: None,
            added: Vec::new(),
            adder: Adder::default(),
            confirm_close: false,
            archive: None,
            open_before: None,
            error: None,
        }
    }

    /// The timesheet changed on disk: what was read from it goes.
    pub fn forget(&mut self) {
        self.archive = None;
        self.open_before = None;
    }

    fn go(&mut self, monday: i64) {
        self.monday = monday;
        self.selected = None;
        self.editing = None;
        self.added.clear();
        self.adder = Adder::default();
        self.error = None;
    }

    fn change<R>(
        &mut self,
        store: &mut Store,
        change: impl FnOnce(&mut whir_core::timesheet::Txn) -> R,
    ) -> Option<R> {
        match store.update(change) {
            Ok(r) => {
                self.error = None;
                self.forget();
                Some(r)
            }
            Err(e) => {
                self.error = Some(e);
                None
            }
        }
    }

    /// Type into a cell, if it can be: not in a closed week, and not the
    /// running project's today (its time is still going up).
    fn edit(&mut self, timer: u32, day: usize, shown: String, closed: bool, live: bool) -> bool {
        if closed || live {
            return false;
        }
        self.selected = Some((timer, day));
        self.editing = Some(Editing {
            text: shown.clone(),
            shown,
            focus: true,
        });
        true
    }

    /// Put down `text` as `timer`'s hours on `day` (0 = Monday) of the week
    /// shown, unless it's what the cell showed already.
    pub(super) fn set_cell(
        &mut self,
        store: &mut Store,
        timer: u32,
        day: usize,
        text: &str,
        shown: &str,
    ) {
        if text.trim() == shown.trim() {
            return;
        }
        match typed_seconds(text) {
            Some(seconds) => {
                let date = self.monday + day as i64;
                self.change(store, |tx| tx.set_day(timer, date, seconds));
            }
            None => {
                self.error = Some(format!(
                    "“{}” isn't a length of time: try 2, 1.5, 1:30 or 90m.",
                    text.trim()
                ))
            }
        }
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        store: Option<&mut Store>,
        cx: &mut DocCx<'_>,
        theme: &Theme,
    ) {
        let Some(store) = store else {
            heading(ui, "Timesheet", theme);
            note(
                ui,
                "The Documents folder isn't available, so there is nowhere to keep time.",
                theme,
            );
            return;
        };
        let now = Stamp::now();
        let this_week = monday_of(now.day);
        let state = store.state().clone();
        let week = store.week(self.monday).clone();
        let running = state
            .running
            .as_ref()
            .filter(|r| r.start < Stamp::midnight(self.monday + 7));
        let grid = Grid::of(&week, running, now);
        let rounding = state.options.rounding;

        self.header(ui, store, &state, &week, now, cx, theme);
        ui.add_space(10.0);
        self.banners(ui, store, &week, this_week, theme);
        self.running_line(ui, store, &state, now, theme);

        self.table(ui, store, &state, &week, &grid, now, theme);
        ui.add_space(8.0);
        note(
            ui,
            &format!(
                "Exact {} · each day rounded to {} per project, as on the timecard. Click a day \
                 to type its hours.",
                format_clock(grid.total()),
                rounding.label().to_lowercase()
            ),
            theme,
        );
        ui.add_space(16.0);

        // The entries behind the chosen day.
        if let Some((timer, day)) = self.selected {
            self.entries(ui, store, &state, &week, timer, day, now, theme);
            ui.add_space(16.0);
        }

        ui.add_space(8.0);
        self.archive(ui, store, cx, theme);
        self.close_dialog(ui, store, &state, &week, now, cx, theme);
    }

    /// The week, where it stands, and what can be done with it.
    #[allow(clippy::too_many_arguments)]
    fn header(
        &mut self,
        ui: &mut egui::Ui,
        store: &mut Store,
        state: &State,
        week: &Week,
        now: Stamp,
        cx: &mut DocCx<'_>,
        theme: &Theme,
    ) {
        let this_week = monday_of(now.day);
        let running = state
            .running
            .as_ref()
            .filter(|r| r.start < Stamp::midnight(self.monday + 7));
        ui.horizontal(|ui| {
            if ui
                .button(icons::text(Icon::ChevronLeft, 11.0))
                .on_hover_text("The week before")
                .clicked()
            {
                self.go(self.monday - 7);
            }
            if ui
                .button(icons::text(Icon::ChevronRight, 11.0))
                .on_hover_text("The week after")
                .clicked()
            {
                self.go(self.monday + 7);
            }
            ui.add_space(8.0);
            ui.label(
                RichText::new(week_title(self.monday))
                    .heading()
                    .color(theme.text),
            );
            ui.add_space(6.0);
            let (status, color) = match week.closed {
                Some(at) => (
                    format!("Closed {}", format_date(&at.local(), false)),
                    theme.text_dim,
                ),
                None => ("Open".to_owned(), theme.accent),
            };
            egui::Frame::NONE
                .fill(color.gamma_multiply(0.14))
                .corner_radius(10.0)
                .inner_margin(egui::Margin::symmetric(8, 2))
                .show(ui, |ui| {
                    ui.label(RichText::new(status).small().color(color));
                });
            if self.monday != this_week && ui.button("This week").clicked() {
                self.go(this_week);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                match week.closed {
                    Some(_) => {
                        if ui.button("Reopen").clicked() {
                            let monday = self.monday;
                            self.change(store, |tx| tx.reopen_week(monday));
                        }
                        let (sheet, _) = csv_paths(store.dir(), self.monday);
                        if sheet.exists() && ui.button("Open timesheet").clicked() {
                            cx.requests.push(DocRequest::Open(vec![sheet]));
                        }
                    }
                    None => {
                        if kit::primary(ui, "Close week…", theme).clicked() {
                            self.confirm_close = true;
                        }
                        if ui
                            .button("Copy")
                            .on_hover_text("The week as a table, to paste into Excel or an email")
                            .clicked()
                        {
                            ui.ctx().copy_text(timesheet_tsv(state, week, running, now));
                            toast(cx, "Copied the week");
                        }
                    }
                }
            });
        });
    }

    /// An earlier week left open, a problem, or a closed week changed since.
    fn banners(
        &mut self,
        ui: &mut egui::Ui,
        store: &mut Store,
        week: &Week,
        this_week: i64,
        theme: &Theme,
    ) {
        // Monday morning: last week's timecard.
        if self.monday == this_week {
            let open_before = self.open_before.get_or_insert_with(|| {
                store
                    .mondays()
                    .into_iter()
                    .filter(|&m| m < this_week)
                    .filter(|&m| {
                        let w = store.week(m);
                        w.closed.is_none() && !w.entries.is_empty()
                    })
                    .collect()
            });
            if let Some(&m) = open_before.last() {
                egui::Frame::NONE
                    .fill(theme.accent.gamma_multiply(0.12))
                    .corner_radius(6.0)
                    .inner_margin(egui::Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!(
                                    "The week of {} is still open.",
                                    format_date(&Stamp::midnight(m).local(), false)
                                ))
                                .color(theme.text),
                            );
                            if ui.button("Review and close it").clicked() {
                                self.go(m);
                            }
                        });
                    });
                ui.add_space(10.0);
            }
        }
        if let Some(e) = &self.error {
            ui.label(RichText::new(e).color(theme.danger));
            ui.add_space(8.0);
        }
        if week.closed.is_some() && week.changed_since_closed() > 0 {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!(
                        "{} entries changed since the week was closed; its CSVs don't have them.",
                        week.changed_since_closed()
                    ))
                    .color(theme.text_dim),
                );
                if ui.button("Close it again").clicked() {
                    self.confirm_close = true;
                }
            });
            ui.add_space(8.0);
        }
    }

    /// What is being timed now, to the second (the dock's stopwatch).
    fn running_line(
        &mut self,
        ui: &mut egui::Ui,
        store: &mut Store,
        state: &State,
        now: Stamp,
        theme: &Theme,
    ) {
        let Some(r) = &state.running else { return };
        let monday = monday_of(now.day);
        let week = store.week(monday).clone();
        let today = Grid::of(&week, Some(r), now).cell(r.timer, (now.day - monday) as usize);
        let mut pause = false;
        egui::Frame::NONE
            .fill(theme.accent.gamma_multiply(0.08))
            .corner_radius(8.0)
            .inner_margin(egui::Margin::symmetric(14, 8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let (dot, _) = ui.allocate_exact_size(vec2(10.0, 16.0), Sense::hover());
                    ui.painter().circle_filled(dot.center(), 4.0, theme.accent);
                    ui.label(RichText::new(state.label(r.timer)).color(theme.text));
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(format_clock(today))
                            .size(theme.metrics.title_text_size)
                            .strong()
                            .color(theme.accent),
                    );
                    ui.label(RichText::new("today").color(theme.text_dim));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        pause = ui.button("Pause").clicked();
                    });
                });
            });
        ui.add_space(12.0);
        if pause {
            self.change(store, |tx| tx.toggle(now));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn table(
        &mut self,
        ui: &mut egui::Ui,
        store: &mut Store,
        state: &State,
        week: &Week,
        grid: &Grid,
        now: Stamp,
        theme: &Theme,
    ) {
        let rounding = state.options.rounding;
        let closed = week.closed.is_some();
        let today = (0..7).find(|&d| self.monday + d as i64 == now.day);
        // The running project's today: still going up, so not typed into.
        let live = state.running.as_ref().map(|r| r.timer).zip(today);
        let mut rows: Vec<u32> = grid.rows.iter().map(|(t, _)| *t).collect();
        for &t in &self.added {
            if !rows.contains(&t) {
                rows.push(t);
            }
        }
        rows.sort_by_key(|&t| {
            let (c, p) = state.names(t);
            (c.to_lowercase(), p.to_lowercase())
        });

        let mut acts = Vec::new();
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            let width = ui
                .available_width()
                .max(PROJECT_MIN + 7.0 * DAY_W + TOTAL_W);
            let left = ui.cursor().left();
            let project_w = width - 7.0 * DAY_W - TOTAL_W;
            // Where day `d` starts (7: the total's column).
            let x = |d: usize| left + project_w + d as f32 * DAY_W;
            let right = left + width;
            let painter = ui.painter().clone();
            // The card and today's column go under everything: drawn last,
            // once the table's height is known, in this place.
            let back = painter.add(egui::Shape::Noop);
            let top = ui.cursor().top();
            let small = FontId::proportional(theme.metrics.body_text_size);
            let body = FontId::proportional(theme.metrics.body_text_size);

            let (head, _) = ui.allocate_exact_size(vec2(width, HEAD_H), Sense::hover());
            let radius = CornerRadius {
                nw: 8,
                ne: 8,
                sw: 0,
                se: 0,
            };
            painter.rect_filled(head, radius, theme.header_bg);
            let y = head.center().y;
            painter.text(
                pos2(left + 14.0, y),
                Align2::LEFT_CENTER,
                "Project",
                small.clone(),
                theme.text_dim,
            );
            for (d, name) in DAYS.iter().enumerate() {
                let (_, _, date) = civil_from_days(self.monday + d as i64);
                let color = if today == Some(d) {
                    theme.accent
                } else {
                    theme.text_dim
                };
                painter.text(
                    pos2(x(d + 1) - CELL_PAD, y),
                    Align2::RIGHT_CENTER,
                    format!("{name} {date}"),
                    small.clone(),
                    color,
                );
            }
            painter.text(
                pos2(right - 14.0, y),
                Align2::RIGHT_CENTER,
                "Total",
                small.clone(),
                theme.text_dim,
            );

            if rows.is_empty() {
                let (r, _) = ui.allocate_exact_size(vec2(width, ROW_H + 8.0), Sense::hover());
                painter.text(
                    pos2(left + 14.0, r.center().y),
                    Align2::LEFT_CENTER,
                    "No time this week yet.",
                    body.clone(),
                    theme.text_dim,
                );
            }
            let mut customer = None;
            for &t in &rows {
                let (c, p) = state.names(t);
                if customer != Some(c) {
                    let (r, _) = ui.allocate_exact_size(vec2(width, CUSTOMER_H), Sense::hover());
                    if customer.is_some() {
                        let line = egui::Rect::from_min_max(
                            pos2(left + 1.0, r.top()),
                            pos2(right - 1.0, r.top() + 1.0),
                        );
                        painter.rect_filled(line, 0.0, theme.separator.gamma_multiply(0.6));
                    }
                    customer = Some(c);
                    painter.text(
                        pos2(left + 14.0, r.center().y + 2.0),
                        Align2::LEFT_CENTER,
                        c,
                        FontId::proportional(theme.metrics.body_text_size),
                        theme.text_dim,
                    );
                }
                let (row, row_resp) = ui.allocate_exact_size(vec2(width, ROW_H), Sense::hover());
                if row_resp.contains_pointer() {
                    let inner = row.shrink2(vec2(1.0, 0.0));
                    painter.rect_filled(inner, 0.0, theme.drawer_item_hover.gamma_multiply(0.6));
                }
                let code = state.timer(t).map_or("", |t| t.code.as_str());
                let job = name_job(p, code, project_w - 40.0, theme);
                let galley = painter.layout_job(job);
                let at = pos2(left + 28.0, row.center().y - galley.size().y / 2.0);
                painter.galley(at, galley, theme.text);
                for d in 0..7 {
                    let cell = egui::Rect::from_min_max(
                        pos2(x(d), row.top()),
                        pos2(x(d + 1), row.bottom()),
                    );
                    let shown = rounding.format(grid.cell(t, d));
                    let is_live = live == Some((t, d));
                    if let Some(act) = self.cell(
                        ui, cell, t, d, shown, rounding, grid, is_live, closed, theme,
                    ) {
                        acts.push(act);
                    }
                }
                painter.text(
                    pos2(right - 14.0, row.center().y),
                    Align2::RIGHT_CENTER,
                    hours(grid.rounded_row(t, rounding)),
                    FontId::proportional(theme.metrics.body_text_size),
                    theme.text,
                );
            }

            // The last row: another project for the week.
            if !closed {
                let (r, _) = ui.allocate_exact_size(vec2(width, ROW_H + 10.0), Sense::hover());
                let line = egui::Rect::from_min_max(
                    pos2(left + 1.0, r.top()),
                    pos2(right - 1.0, r.top() + 1.0),
                );
                painter.rect_filled(line, 0.0, theme.separator.gamma_multiply(0.6));
                let area = egui::Rect::from_min_max(
                    pos2(left + 10.0, r.top() + 5.0),
                    pos2(right - 10.0, r.bottom() - 5.0),
                );
                if let Some(act) = self.adder(ui, area, state, &rows, theme) {
                    acts.push(act);
                }
            }

            // Totals.
            let (r, _) = ui.allocate_exact_size(vec2(width, ROW_H + 2.0), Sense::hover());
            let radius = CornerRadius {
                nw: 0,
                ne: 0,
                sw: 8,
                se: 8,
            };
            painter.rect_filled(r, radius, theme.header_bg);
            let y = r.center().y;
            painter.text(
                pos2(left + 14.0, y),
                Align2::LEFT_CENTER,
                "Total",
                body.clone(),
                theme.text,
            );
            for d in 0..7 {
                painter.text(
                    pos2(x(d + 1) - CELL_PAD, y),
                    Align2::RIGHT_CENTER,
                    hours(grid.rounded_day(d, rounding)),
                    body.clone(),
                    theme.text,
                );
            }
            painter.text(
                pos2(right - 14.0, y),
                Align2::RIGHT_CENTER,
                hours(grid.rounded_total(rounding)),
                FontId::proportional(theme.metrics.title_text_size),
                theme.accent,
            );

            let card = egui::Rect::from_min_max(pos2(left, top), pos2(right, r.bottom()));
            let mut under = vec![egui::Shape::rect_filled(card, 8.0, theme.content_bg)];
            if let Some(d) = today {
                let column = egui::Rect::from_min_max(pos2(x(d), top), pos2(x(d + 1), r.bottom()));
                under.push(egui::Shape::rect_filled(
                    column,
                    0.0,
                    theme.accent.gamma_multiply(0.06),
                ));
            }
            under.push(egui::Shape::rect_stroke(
                card,
                8.0,
                Stroke::new(1.0, theme.separator),
                StrokeKind::Inside,
            ));
            painter.set(back, egui::Shape::Vec(under));
        });

        for act in acts {
            self.act(ui.ctx(), store, act);
        }
    }

    /// A day's cell: its hours, or the field they're typed in.
    #[allow(clippy::too_many_arguments)]
    fn cell(
        &mut self,
        ui: &mut egui::Ui,
        cell: egui::Rect,
        timer: u32,
        day: usize,
        shown: String,
        rounding: Rounding,
        grid: &Grid,
        live: bool,
        closed: bool,
        theme: &Theme,
    ) -> Option<Act> {
        let inner = cell.shrink2(vec2(5.0, 4.0));
        let id = Id::new(("whir.timesheet.cell", timer, day));
        let selected = self.selected == Some((timer, day));
        if let Some(ed) = self.editing.as_mut().filter(|_| selected) {
            if ed.focus {
                prime(ui.ctx(), id, &ed.text);
                ed.focus = false;
            }
            // Keys the cell acts on, before the field sees them.
            let key = if ui.memory(|m| m.has_focus(id)) {
                ui.input_mut(|i| {
                    [
                        (Modifiers::NONE, Key::Escape),
                        (Modifiers::SHIFT, Key::Tab),
                        (Modifiers::NONE, Key::Tab),
                        (Modifiers::NONE, Key::Enter),
                    ]
                    .into_iter()
                    .find(|&(m, k)| i.consume_key(m, k))
                })
            } else {
                None
            };
            let response = ui.put(
                inner,
                egui::TextEdit::singleline(&mut ed.text)
                    .id(id)
                    .font(FontId::proportional(theme.metrics.body_text_size))
                    .horizontal_align(Align::Max)
                    .margin(vec2(6.0, 2.0)),
            );
            // The cell beside this one, along the row, and what it shows.
            let beside = |d: Option<usize>| {
                d.filter(|&d| d < 7)
                    .map(|d| (d, rounding.format(grid.cell(timer, d))))
            };
            let commit = |next| Act::Commit {
                timer,
                day,
                text: ed.text.clone(),
                shown: ed.shown.clone(),
                next,
            };
            return match key {
                Some((_, Key::Escape)) => Some(Act::Cancel),
                Some((m, Key::Tab)) if m.shift => Some(commit(beside(day.checked_sub(1)))),
                Some((_, Key::Tab)) => Some(commit(beside(Some(day + 1)))),
                Some(_) => Some(commit(None)),
                None if response.lost_focus() => Some(commit(None)),
                None => None,
            };
        }

        let editable = !closed && !live;
        let sense = if editable {
            Sense::click()
        } else {
            Sense::hover()
        };
        let response = ui.interact(inner, id, sense);
        let painter = ui.painter();
        if selected {
            painter.rect_filled(inner, 5.0, theme.drawer_item_active);
        } else if editable && response.hovered() {
            painter.rect_filled(inner, 5.0, theme.content_bg);
            painter.rect_stroke(
                inner,
                5.0,
                Stroke::new(1.0, theme.separator),
                StrokeKind::Inside,
            );
        }
        if live {
            painter.circle_filled(
                pos2(inner.left() + 8.0, inner.center().y),
                3.5,
                theme.accent,
            );
        }
        painter.text(
            pos2(cell.right() - CELL_PAD, cell.center().y),
            Align2::RIGHT_CENTER,
            &shown,
            FontId::proportional(theme.metrics.body_text_size),
            if live { theme.accent } else { theme.text },
        );
        let response = match (live, closed) {
            (true, _) => response.on_hover_text("Running: pause it to change today's hours"),
            (_, true) => response.on_hover_text("Reopen the week to change it"),
            _ => response,
        };
        if response.clicked() {
            return Some(Act::Edit(timer, day, shown));
        }
        // Read-only, it still shows its entries.
        if !editable && ui.input(|i| i.pointer.primary_clicked()) && response.hovered() {
            self.selected = if selected { None } else { Some((timer, day)) };
        }
        None
    }

    /// The last row's pickers: a customer, then one of its projects (or a
    /// new one of either).
    fn adder(
        &mut self,
        ui: &mut egui::Ui,
        area: egui::Rect,
        state: &State,
        rows: &[u32],
        theme: &Theme,
    ) -> Option<Act> {
        let mut act = None;
        let builder = UiBuilder::new()
            .max_rect(area)
            .layout(Layout::left_to_right(Align::Center));
        ui.scope_builder(builder, |ui| {
            ui.spacing_mut().item_spacing = vec2(8.0, 0.0);
            let a = &mut self.adder;
            let enter = |ui: &egui::Ui| ui.input(|i| i.key_pressed(Key::Enter));
            match &mut a.new_customer {
                Some(name) => {
                    let r = ui.add(
                        egui::TextEdit::singleline(name)
                            .hint_text("New customer's name")
                            .desired_width(170.0),
                    );
                    if std::mem::take(&mut a.focus) {
                        r.request_focus();
                    }
                    if r.lost_focus() {
                        if enter(ui) && !name.trim().is_empty() {
                            act = Some(Act::NewCustomer(name.trim().to_owned()));
                        }
                        a.new_customer = None;
                    }
                }
                None => {
                    let chosen = a.customer.and_then(|c| state.customer(c));
                    let text = match chosen {
                        Some(c) => RichText::new(&c.name).color(theme.text),
                        None => RichText::new("Customer…").color(theme.text_dim),
                    };
                    egui::ComboBox::from_id_salt("whir.timesheet.customer")
                        .icon(kit::combo_chevron)
                        .width(170.0)
                        .selected_text(text)
                        .show_ui(ui, |ui| {
                            for (c, _) in state.by_customer(false, true) {
                                let on = a.customer == Some(c.id);
                                if ui.selectable_label(on, c.name.as_str()).clicked() {
                                    a.customer = Some(c.id);
                                    a.new_project = None;
                                }
                            }
                            ui.separator();
                            if ui.selectable_label(false, "New customer…").clicked() {
                                a.new_customer = Some(String::new());
                                a.focus = true;
                            }
                        });
                }
            }
            let Some(cid) = a.customer.filter(|_| a.new_customer.is_none()) else {
                ui.label(
                    RichText::new("then a project, and type its hours in a day")
                        .color(theme.text_dim),
                );
                return;
            };
            match &mut a.new_project {
                Some((name, code)) => {
                    let n = ui.add(
                        egui::TextEdit::singleline(name)
                            .hint_text("New project's name")
                            .desired_width(190.0),
                    );
                    let c = ui.add(
                        egui::TextEdit::singleline(code)
                            .hint_text("Code (optional)")
                            .desired_width(120.0),
                    );
                    let cancel = ui.button("Cancel").clicked();
                    if std::mem::take(&mut a.focus) {
                        n.request_focus();
                    }
                    // Enter in either field adds it; Esc or Cancel doesn't.
                    let done = (n.lost_focus() || c.lost_focus()) && enter(ui);
                    if done && !name.trim().is_empty() {
                        act = Some(Act::NewProject(
                            cid,
                            name.trim().to_owned(),
                            code.trim().to_owned(),
                        ));
                        a.new_project = None;
                    } else if cancel || ui.input(|i| i.key_pressed(Key::Escape)) {
                        a.new_project = None;
                    }
                }
                None => {
                    let mut projects: Vec<_> = state
                        .timers
                        .iter()
                        .filter(|t| t.customer == cid && !t.archived && !rows.contains(&t.id))
                        .collect();
                    projects.sort_by_key(|t| t.name.to_lowercase());
                    egui::ComboBox::from_id_salt(("whir.timesheet.project", cid))
                        .icon(kit::combo_chevron)
                        .width(190.0)
                        .selected_text(RichText::new("Project…").color(theme.text_dim))
                        .show_ui(ui, |ui| {
                            for t in projects {
                                if ui.selectable_label(false, t.name.as_str()).clicked() {
                                    act = Some(Act::AddRow(t.id));
                                }
                            }
                            ui.separator();
                            if ui.selectable_label(false, "New project…").clicked() {
                                a.new_project = Some((String::new(), String::new()));
                                a.focus = true;
                            }
                        });
                }
            }
        });
        act
    }

    /// Do what the table asked for.
    fn act(&mut self, ctx: &egui::Context, store: &mut Store, act: Act) {
        let today = (0..7).find(|&d| self.monday + d as i64 == Stamp::now().day);
        match act {
            Act::Edit(timer, day, shown) => {
                // A click on another cell came with this one's commit.
                self.edit(timer, day, shown, false, false);
            }
            Act::Commit {
                timer,
                day,
                text,
                shown,
                next,
            } => {
                self.editing = None;
                ctx.memory_mut(|m| m.surrender_focus(Id::new(("whir.timesheet.cell", timer, day))));
                self.set_cell(store, timer, day, &text, &shown);
                if let Some((d, shown)) = next {
                    let live = store
                        .state()
                        .running
                        .as_ref()
                        .is_some_and(|r| r.timer == timer && Some(d) == today);
                    self.edit(timer, d, shown, false, live);
                }
            }
            Act::Cancel => {
                self.editing = None;
            }
            Act::AddRow(timer) => {
                self.added.push(timer);
                self.adder = Adder::default();
                self.edit(timer, today.unwrap_or(0), String::new(), false, false);
            }
            Act::NewCustomer(name) => {
                if let Some(id) = self.change(store, |tx| tx.state.add_customer(&name)) {
                    self.adder = Adder {
                        customer: Some(id),
                        ..Adder::default()
                    };
                }
            }
            Act::NewProject(customer, name, code) => {
                if let Some(id) =
                    self.change(store, |tx| tx.state.add_timer(customer, &name, &code))
                {
                    self.act(ctx, store, Act::AddRow(id));
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn entries(
        &mut self,
        ui: &mut egui::Ui,
        store: &mut Store,
        state: &State,
        week: &Week,
        timer: u32,
        day: usize,
        now: Stamp,
        theme: &Theme,
    ) {
        let date = self.monday + day as i64;
        let title = format!(
            "{} — {}",
            state.label(timer),
            format_date(&Stamp::midnight(date).local(), false)
        );
        ui.label(RichText::new(title).heading().color(theme.text));
        ui.add_space(4.0);
        let entries: Vec<Entry> = week
            .entries
            .iter()
            .filter(|e| e.timer == timer && e.day() == date)
            .cloned()
            .collect();
        let closed = week.closed.is_some();
        let mut change: Option<(Entry, Option<Span>)> = None;
        egui::Grid::new(("whir.entries", timer, date))
            .spacing(vec2(8.0, 4.0))
            .min_col_width(0.0)
            .show(ui, |ui| {
                for e in &entries {
                    let key = (e.timer, e.start.seconds(), e.end.seconds());
                    let id = |part: &str| Id::new(("whir.entry", key, part.to_owned()));
                    let end_secs = if e.end.day > e.day() {
                        MIDNIGHT
                    } else {
                        e.end.secs
                    };
                    if closed {
                        ui.label(clock_text(e.start.secs));
                        ui.label("–");
                        ui.label(clock_text(end_secs));
                        ui.label(RichText::new(format_clock(e.seconds())).color(theme.text_dim));
                        ui.label(RichText::new(&e.note).color(theme.text));
                        ui.end_row();
                        continue;
                    }
                    let start = edit_line(ui, id("start"), &clock_text(e.start.secs), 54.0, "");
                    ui.label("–");
                    let end = edit_line(ui, id("end"), &clock_text(end_secs), 54.0, "");
                    ui.label(RichText::new(format_clock(e.seconds())).color(theme.text_dim));
                    let note = edit_line(ui, id("note"), &e.note, 340.0, "What was it for?");
                    if ui.button("Delete").clicked() {
                        change = Some((e.clone(), None));
                    }
                    ui.end_row();
                    if start.is_some() || end.is_some() || note.is_some() {
                        let s = start.as_deref().map_or(Some(e.start.secs), parse_clock);
                        let f = end.as_deref().map_or(Some(end_secs), parse_end);
                        match (s, f) {
                            (Some(s), Some(f)) if s < f => {
                                let start = Stamp::at(date, 0, 0, 0).plus(s as i64);
                                let end = Stamp::at(date, 0, 0, 0).plus(f as i64);
                                let note = note.unwrap_or_else(|| e.note.clone());
                                change = Some((e.clone(), Some((start, end, note))));
                            }
                            _ => {
                                self.error =
                                    Some("A start before its end, like 9:05 and 10:30.".into())
                            }
                        }
                    }
                }
                if let Some(r) = state.running.as_ref().filter(|r| r.timer == timer)
                    && r.start < Stamp::midnight(date + 1)
                    && now.day >= date
                {
                    let from = if r.start.day < date { 0 } else { r.start.secs };
                    let to = if now.day > date { MIDNIGHT } else { now.secs };
                    ui.label(clock_text(from));
                    ui.label("–");
                    ui.label(RichText::new("now").color(theme.accent));
                    ui.label(RichText::new(format_clock((to - from) as i64)).color(theme.accent));
                    let what = if r.note.is_empty() {
                        "(running)".to_owned()
                    } else {
                        format!("{} (running)", r.note)
                    };
                    ui.label(RichText::new(what).color(theme.text_dim));
                    ui.end_row();
                }
            });
        if entries.is_empty() && state.running.as_ref().is_none_or(|r| r.timer != timer) {
            note(ui, "No entries.", theme);
        }
        if let Some((old, new)) = change {
            let monday = self.monday;
            let result = self.change(store, |tx| {
                let week = tx.week_mut(monday);
                let Some(i) = week.entries.iter().position(|e| *e == old) else {
                    return false;
                };
                let e = week.entries.remove(i);
                if let Some((start, end, note)) = new {
                    tx.add(e.timer, start, end, &note);
                }
                true
            });
            if result == Some(false) {
                self.error = Some("That entry was changed meanwhile; try again.".into());
            }
        }
    }

    fn archive(&mut self, ui: &mut egui::Ui, store: &mut Store, cx: &mut DocCx<'_>, theme: &Theme) {
        let rounding = store.state().options.rounding;
        let archive = self.archive.get_or_insert_with(|| {
            let mut closed: Vec<(i64, Stamp, f64)> = store
                .mondays()
                .into_iter()
                .filter_map(|m| {
                    let w = store.week(m);
                    let at = w.closed?;
                    let total = Grid::of(w, None, Stamp::default()).rounded_total(rounding);
                    Some((m, at, total))
                })
                .collect();
            closed.reverse();
            closed
        });
        if archive.is_empty() {
            return;
        }
        heading(ui, "Closed weeks", theme);
        let mut go = None;
        egui::Grid::new("whir.archive")
            .spacing(vec2(14.0, 4.0))
            .show(ui, |ui| {
                for &(monday, at, total) in archive.iter() {
                    if ui
                        .link(RichText::new(week_title(monday)).color(theme.text))
                        .clicked()
                    {
                        go = Some(monday);
                    }
                    ui.label(RichText::new(format!("{} h", hours(total))).color(theme.text));
                    ui.label(
                        RichText::new(format!("closed {}", format_date(&at.local(), false)))
                            .color(theme.text_dim),
                    );
                    let (sheet, _) = csv_paths(store.dir(), monday);
                    if sheet.exists() {
                        if ui.button("Open").clicked() {
                            cx.requests.push(DocRequest::Open(vec![sheet.clone()]));
                        }
                        if ui.button("Show in Explorer").clicked() {
                            cx.requests.push(DocRequest::Reveal(sheet));
                        }
                    }
                    ui.end_row();
                }
            });
        if let Some(m) = go {
            self.go(m);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn close_dialog(
        &mut self,
        ui: &mut egui::Ui,
        store: &mut Store,
        state: &State,
        week: &Week,
        now: Stamp,
        cx: &mut DocCx<'_>,
        theme: &Theme,
    ) {
        if !self.confirm_close {
            return;
        }
        let ctx = ui.ctx().clone();
        let monday = self.monday;
        let running = state
            .running
            .as_ref()
            .is_some_and(|r| r.start < Stamp::midnight(monday + 7));
        let total =
            Grid::of(week, state.running.as_ref(), now).rounded_total(state.options.rounding);
        let mut close = false;
        let mut cancel = false;
        let modal = kit::dialog(&ctx, Id::new("whir.close_week"), 420.0, theme, |ui| {
            kit::dialog_title(ui, format!("Close out {}?", week_title(monday)), theme);
            note(
                ui,
                &format!(
                    "Its timesheet ({} h) is written to the Time folder as CSV, with every \
                         entry beside it. You can reopen it later.",
                    hours(total)
                ),
                theme,
            );
            if running {
                ui.add_space(4.0);
                note(
                    ui,
                    &format!(
                        "The running timer's time so far (since {}) goes in; it keeps running.",
                        format_hm(
                            &state
                                .running
                                .as_ref()
                                .map(|r| r.start)
                                .unwrap_or(now)
                                .local()
                        )
                    ),
                    theme,
                );
            }
            kit::dialog_actions(ui, |ui| {
                cancel = kit::action(ui, "Cancel", theme).clicked();
                close = kit::primary(ui, "Close week", theme).clicked();
            });
        });
        if cancel || (modal.should_close() && !close) {
            self.confirm_close = false;
        }
        if close {
            self.confirm_close = false;
            match self.change(store, |tx| tx.close_week(monday, now)) {
                Some(Ok(path)) => {
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    toast(cx, format!("Week closed: {name}"));
                }
                Some(Err(e)) => self.error = Some(e),
                // The store said why.
                None => {}
            }
        }
    }
}

/// An end time: as [`parse_clock`], and "24:00" for midnight.
fn parse_end(text: &str) -> Option<u32> {
    match text.trim() {
        "24:00" | "24" => Some(MIDNIGHT),
        t => parse_clock(t),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hours_are_read_as_typed_into_a_cell() {
        assert_eq!(typed_seconds("2"), Some(7200));
        assert_eq!(typed_seconds("1.5"), Some(5400));
        assert_eq!(typed_seconds("1:30"), Some(5400));
        assert_eq!(typed_seconds("90m"), Some(5400));
        for none in ["", " ", "0", "0:00", "0.0"] {
            assert_eq!(typed_seconds(none), Some(0), "{none:?}");
        }
        assert_eq!(typed_seconds("lots"), None);
    }

    #[test]
    fn a_cell_takes_the_hours_typed_and_a_closed_week_none() {
        let dir = std::env::temp_dir().join(format!("whir-sheet-cells-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = Store::open(dir.clone());
        let t = store
            .update(|tx| {
                let c = tx.state.add_customer("Contoso");
                tx.state.add_timer(c, "Automation", "")
            })
            .unwrap();
        let mut view = View::new();
        let monday = view.monday;
        let tuesday = |store: &mut Store| {
            let week = store.week(monday).clone();
            Grid::of(&week, None, Stamp::now()).cell(t, 1)
        };
        view.set_cell(&mut store, t, 1, "2", "");
        assert_eq!(tuesday(&mut store), 7200);
        view.set_cell(&mut store, t, 1, "1:30", "2");
        assert_eq!(tuesday(&mut store), 5400);
        // As it was shown: nothing changes (the exact time stays exact).
        view.set_cell(&mut store, t, 1, " 1.5 ", "1.5");
        assert_eq!(tuesday(&mut store), 5400);
        view.set_cell(&mut store, t, 1, "", "1.5");
        assert_eq!(tuesday(&mut store), 0);
        // Not a length of time: said so, and nothing written.
        view.set_cell(&mut store, t, 1, "soon", "");
        assert!(view.error.is_some());
        assert_eq!(tuesday(&mut store), 0);

        // Closed, or the running project's today: not typed into.
        assert!(!view.edit(t, 1, String::new(), true, false));
        assert!(!view.edit(t, 1, String::new(), false, true));
        assert!(view.editing.is_none());
        assert!(view.edit(t, 1, "2".into(), false, false));
        assert_eq!(view.selected, Some((t, 1)));
        let _ = std::fs::remove_dir_all(dir);
    }
}
