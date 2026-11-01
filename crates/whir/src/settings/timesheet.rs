//! The timesheet: the week at a glance and where its time is put down. A
//! row per project, a column per day, hours as they go on the timecard;
//! click a day and type its hours, or add a project from the last row. The
//! entries behind a day are listed under it, and the week is closed out to
//! CSV from here. The table gives way as the window narrows: see [`Columns`].

use egui::text::{CCursor, CCursorRange, LayoutJob, TextFormat, TextWrapping};
use egui::{
    Align, Align2, CornerRadius, FontId, Id, Key, Layout, Modifiers, Rect, RichText, Sense, Stroke,
    StrokeKind, TextEdit, UiBuilder, pos2, vec2,
};
use whir_core::icons::Icon;
use whir_core::kit::{self, Inline};
use whir_core::theme::Theme;
use whir_core::time::{Stamp, civil_from_days, format_clock, format_date, format_hm, monday_of};
use whir_core::timesheet::{Entry, Grid, Rounding, State, Store, Week, csv_paths, timesheet_tsv};
use whir_core::{DocCx, DocRequest, toolbar};

use super::{clock_text, no_folder, note, parse_clock, parse_duration, toast};
use crate::fuzzy;

const DAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const HEAD_H: f32 = 36.0;
const ROW_H: f32 = 34.0;
/// How far a day's column and the total's can go, each way.
const DAY_MAX: f32 = 74.0;
const DAY_MIN: f32 = 44.0;
const TOTAL_MAX: f32 = 86.0;
const TOTAL_MIN: f32 = 56.0;
/// What customer and project need to have a column each.
const LABELS_MIN: f32 = 300.0;
/// The least they can have between them, in one column.
const LABEL_MIN: f32 = 150.0;
const CUSTOMER_MAX: f32 = 220.0;
/// How much of a shared column a customer's name can have: the project is what
/// tells the rows apart.
const CUSTOMER_SHARE: f32 = 0.45;
/// A day's column has room for its date from here up.
const DATES_FROM: f32 = 60.0;
/// From the table's sides to what's in it.
const SIDE: f32 = 14.0;
/// Below this the week's title and what can be done with it take a row each.
const HEADER_WRAP: f32 = 640.0;
/// The list of projects to add, and how many it shows.
const PICKER_WIDTH: f32 = 340.0;
const PICKER_ROWS: usize = 8;
/// An entry's times.
const CLOCK_W: f32 = 50.0;
const LENGTH_W: f32 = 62.0;
const MIDNIGHT: u32 = 24 * 3600;

/// An entry's start, end and note, as changed.
type Span = (Stamp, Stamp, String);

/// How wide the table's columns are in the room there is. The days give way
/// first, then customer and project share a column; past that, the table stops
/// shrinking and scrolls sideways.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Columns {
    width: f32,
    /// `None`: it shares the project's column.
    customer: Option<f32>,
    project: f32,
    day: f32,
    total: f32,
}

impl Columns {
    fn fit(room: f32) -> Columns {
        let width = room.max(LABEL_MIN + 7.0 * DAY_MIN + TOTAL_MIN);
        let spare = (width - LABELS_MIN - TOTAL_MAX) / 7.0;
        let day = spare.clamp(DAY_MIN, DAY_MAX).floor();
        let total = if day < DATES_FROM {
            TOTAL_MIN
        } else {
            TOTAL_MAX
        };
        let labels = width - 7.0 * day - total;
        let customer = (labels >= LABELS_MIN).then(|| (labels * 0.42).min(CUSTOMER_MAX).floor());
        Columns {
            width,
            customer,
            project: labels - customer.unwrap_or(0.0),
            day,
            total,
        }
    }

    /// Where day `d` starts, from the table's left edge (7: the total's column).
    fn x(self, d: usize) -> f32 {
        self.width - self.total - (7 - d) as f32 * self.day
    }

    fn dates(self) -> bool {
        self.day >= DATES_FROM
    }

    /// Inside a cell, from its edge to the hours.
    fn pad(self) -> f32 {
        if self.dates() { 12.0 } else { 8.0 }
    }
}

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

/// The last row: the list of projects to add, or a new one in its dialog.
#[derive(Default)]
struct Adder {
    query: String,
    /// Where the arrow keys have got to.
    selected: usize,
    /// The field still needs the keyboard.
    focus: bool,
    form: Option<Form>,
}

/// A new project, in its dialog.
#[derive(Default)]
struct Form {
    /// `None`: a new one, called `new_customer`.
    customer: Option<u32>,
    new_customer: String,
    name: String,
    code: String,
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
    NewProject(Form),
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

/// Words in a space of their own, in line with an [`kit::inline_text`]'s.
fn fixed(ui: &mut egui::Ui, width: f32, text: &str, color: egui::Color32, theme: &Theme) {
    let m = theme.metrics;
    let size = vec2(width, m.control_height);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().text(
        rect.left_center() + vec2(kit::INLINE_MARGIN, 0.0),
        Align2::LEFT_CENTER,
        text,
        FontId::proportional(m.body_text_size),
        color,
    );
}

/// Between an entry's start and its end.
fn dash(ui: &mut egui::Ui, theme: &Theme) {
    ui.label(RichText::new("–").color(theme.text_dim));
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
            no_folder(ui, "Timesheet", theme);
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
        ui.add_space(12.0);
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
        ui.add_space(20.0);

        // The entries behind the chosen day.
        if let Some((timer, day)) = self.selected {
            self.entries(ui, store, &state, &week, timer, day, now, theme);
            ui.add_space(20.0);
        }

        self.archive(ui, store, cx, theme);
        self.close_dialog(ui, store, &state, &week, now, cx, theme);
        self.new_project_dialog(ui, store, &state, theme);
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
        let wrap = ui.available_width() < HEADER_WRAP;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.set_min_height(theme.metrics.tool_size);
            if toolbar::tool(ui, Icon::ChevronLeft, "The week before", true, theme) {
                self.go(self.monday - 7);
            }
            if toolbar::tool(ui, Icon::ChevronRight, "The week after", true, theme) {
                self.go(self.monday + 7);
            }
            ui.add_space(8.0);
            let title = RichText::new(week_title(self.monday))
                .font(kit::strong(theme.metrics.title_text_size))
                .color(theme.text);
            ui.label(title);
            ui.add_space(8.0);
            match week.closed {
                Some(at) => {
                    let when = format!("Closed {}", format_date(&at.local(), false));
                    kit::pill(ui, &when, theme.text_dim, theme);
                }
                None => kit::pill(ui, "Open", theme.accent, theme),
            }
            if !wrap {
                ui.add_space(8.0);
                self.this_week(ui, this_week, theme);
                self.actions(ui, store, state, week, now, cx, theme);
            }
        });
        if wrap {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                self.this_week(ui, this_week, theme);
                self.actions(ui, store, state, week, now, cx, theme);
            });
        }
    }

    /// The way back from another week.
    fn this_week(&mut self, ui: &mut egui::Ui, this_week: i64, theme: &Theme) {
        if self.monday != this_week && kit::action(ui, "This week", theme).clicked() {
            self.go(this_week);
        }
    }

    /// What can be done with the week, against the right-hand side.
    #[allow(clippy::too_many_arguments)]
    fn actions(
        &mut self,
        ui: &mut egui::Ui,
        store: &mut Store,
        state: &State,
        week: &Week,
        now: Stamp,
        cx: &mut DocCx<'_>,
        theme: &Theme,
    ) {
        let running = state
            .running
            .as_ref()
            .filter(|r| r.start < Stamp::midnight(self.monday + 7));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            match week.closed {
                Some(_) => {
                    if kit::action(ui, "Reopen", theme).clicked() {
                        let monday = self.monday;
                        self.change(store, |tx| tx.reopen_week(monday));
                    }
                    let (sheet, _) = csv_paths(store.dir(), self.monday);
                    if sheet.exists() && kit::action(ui, "Open timesheet", theme).clicked() {
                        cx.requests.push(DocRequest::Open(vec![sheet]));
                    }
                }
                None => {
                    if kit::primary(ui, "Close week…", theme).clicked() {
                        self.confirm_close = true;
                    }
                    if kit::action(ui, "Copy", theme)
                        .on_hover_text("The week as a table, to paste into Excel or an email")
                        .clicked()
                    {
                        ui.ctx().copy_text(timesheet_tsv(state, week, running, now));
                        toast(cx, "Copied the week");
                    }
                }
            }
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
                kit::banner(ui, theme.accent, theme, |ui| {
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
                ui.add_space(10.0);
            }
        }
        if let Some(e) = &self.error {
            kit::banner(ui, theme.danger, theme, |ui| {
                ui.label(RichText::new(e).color(theme.text));
            });
            ui.add_space(10.0);
        }
        if week.closed.is_some() && week.changed_since_closed() > 0 {
            kit::banner(ui, theme.warning, theme, |ui| {
                ui.label(
                    RichText::new(format!(
                        "{} entries changed since the week was closed; its CSVs don't have them.",
                        week.changed_since_closed()
                    ))
                    .color(theme.text),
                );
                if ui.button("Close it again").clicked() {
                    self.confirm_close = true;
                }
            });
            ui.add_space(10.0);
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
        let m = theme.metrics;
        let mut pause = false;
        kit::banner(ui, theme.accent, theme, |ui| {
            let clock = kit::strong(m.title_text_size);
            let time = format_clock(today);
            let width = kit::tabular_width(ui.painter(), &time, &clock);
            let room = ui.available_width() - width - 130.0;

            let (dot, _) = ui.allocate_exact_size(vec2(10.0, 16.0), Sense::hover());
            ui.painter().circle_filled(dot.center(), 4.0, theme.accent);
            let font = FontId::proportional(m.body_text_size);
            let what = kit::one_line(ui.painter(), &state.label(r.timer), font, room);
            let (at, _) = ui.allocate_exact_size(what.size(), Sense::hover());
            ui.painter().galley(at.min, what, theme.text);
            ui.add_space(4.0);
            let size = vec2(width, m.control_height);
            let (at, _) = ui.allocate_exact_size(size, Sense::hover());
            let left = at.left_center();
            kit::tabular(
                ui.painter(),
                left,
                Align2::LEFT_CENTER,
                &time,
                &clock,
                theme.accent,
            );
            ui.label(RichText::new("today").color(theme.text_dim));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                pause = ui.button("Pause").clicked();
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

        let columns = Columns::fit(ui.available_width());
        let mut acts = Vec::new();
        egui::ScrollArea::horizontal()
            .id_salt("whir.timesheet.table")
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                let width = columns.width;
                let left = ui.cursor().left();
                // Where day `d` starts (7: the total's column).
                let x = |d: usize| left + columns.x(d);
                let right = left + width;
                let project_x = left + columns.customer.unwrap_or(0.0);
                let painter = ui.painter().clone();
                // The card and today's column go under everything: drawn last,
                // once the table's height is known, in this place.
                let back = painter.add(egui::Shape::Noop);
                let top = ui.cursor().top();
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
                if columns.customer.is_some() {
                    painter.text(
                        pos2(left + SIDE, y),
                        Align2::LEFT_CENTER,
                        "Customer",
                        body.clone(),
                        theme.text_dim,
                    );
                }
                painter.text(
                    pos2(project_x + SIDE, y),
                    Align2::LEFT_CENTER,
                    "Project",
                    body.clone(),
                    theme.text_dim,
                );
                for (d, name) in DAYS.iter().enumerate() {
                    let (_, _, date) = civil_from_days(self.monday + d as i64);
                    let color = if today == Some(d) {
                        theme.accent
                    } else {
                        theme.text_dim
                    };
                    let text = if columns.dates() {
                        format!("{name} {date}")
                    } else {
                        (*name).to_owned()
                    };
                    painter.text(
                        pos2(x(d + 1) - columns.pad(), y),
                        Align2::RIGHT_CENTER,
                        text,
                        body.clone(),
                        color,
                    );
                }
                painter.text(
                    pos2(right - SIDE, y),
                    Align2::RIGHT_CENTER,
                    "Total",
                    body.clone(),
                    theme.text_dim,
                );

                if rows.is_empty() {
                    let (r, _) = ui.allocate_exact_size(vec2(width, ROW_H + 8.0), Sense::hover());
                    painter.text(
                        pos2(left + SIDE, r.center().y),
                        Align2::LEFT_CENTER,
                        "No time this week yet.",
                        body.clone(),
                        theme.text_dim,
                    );
                }
                for (i, &t) in rows.iter().enumerate() {
                    let (customer, project) = state.names(t);
                    let (row, row_resp) =
                        ui.allocate_exact_size(vec2(width, ROW_H), Sense::hover());
                    if i > 0 {
                        let line = Stroke::new(1.0, theme.separator.gamma_multiply(0.6));
                        let y = painter.round_to_pixel_center(row.top());
                        painter.hline((left + 1.0)..=(right - 1.0), y, line);
                    }
                    if row_resp.contains_pointer() {
                        let inner = row.shrink2(vec2(1.0, 0.0));
                        painter.rect_filled(
                            inner,
                            0.0,
                            theme.drawer_item_hover.gamma_multiply(0.6),
                        );
                    }
                    let code = state.timer(t).map_or("", |t| t.code.as_str());
                    let middle = row.center().y;
                    let mut at_x = left + SIDE;
                    let mut room = columns.project - 2.0 * SIDE;
                    let (name_room, shared) = match columns.customer {
                        Some(column) => (column - SIDE, false),
                        None => (room * CUSTOMER_SHARE, !customer.is_empty()),
                    };
                    let name = kit::one_line(&painter, customer, body.clone(), name_room);
                    let mut elided = name.elided;
                    let width = name.size().x;
                    let at = pos2(at_x, middle - name.size().y / 2.0);
                    painter.galley(at, name, theme.text_dim);
                    if shared {
                        let dot = kit::one_line(&painter, " · ", body.clone(), room);
                        let at = pos2(at_x + width, middle - dot.size().y / 2.0);
                        at_x += width + dot.size().x;
                        room -= width + dot.size().x;
                        painter.galley(at, dot, theme.text_dim);
                    } else {
                        at_x = project_x + SIDE;
                    }
                    let galley = painter.layout_job(name_job(project, code, room, theme));
                    elided |= galley.elided;
                    let at = pos2(at_x, middle - galley.size().y / 2.0);
                    painter.galley(at, galley, theme.text);
                    // All of a name that's been cut short.
                    if elided {
                        row_resp.on_hover_text(state.label(t));
                    }
                    for d in 0..7 {
                        let cell =
                            Rect::from_min_max(pos2(x(d), row.top()), pos2(x(d + 1), row.bottom()));
                        let shown = rounding.format(grid.cell(t, d));
                        let is_live = live == Some((t, d));
                        if let Some(act) = self.cell(
                            ui,
                            cell,
                            columns.pad(),
                            t,
                            d,
                            shown,
                            rounding,
                            grid,
                            is_live,
                            closed,
                            theme,
                        ) {
                            acts.push(act);
                        }
                    }
                    kit::tabular(
                        &painter,
                        pos2(right - SIDE, middle),
                        Align2::RIGHT_CENTER,
                        &hours(grid.rounded_row(t, rounding)),
                        &body,
                        theme.text,
                    );
                }

                // The last row: another project for the week.
                if !closed {
                    let (r, _) = ui.allocate_exact_size(vec2(width, ROW_H + 6.0), Sense::hover());
                    let line = Stroke::new(1.0, theme.separator.gamma_multiply(0.6));
                    let y = painter.round_to_pixel_center(r.top());
                    painter.hline((left + 1.0)..=(right - 1.0), y, line);
                    let area = Rect::from_min_max(
                        pos2(left + 6.0, r.top() + 4.0),
                        pos2(right - 10.0, r.bottom() - 4.0),
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
                    pos2(left + SIDE, y),
                    Align2::LEFT_CENTER,
                    "Total",
                    body.clone(),
                    theme.text,
                );
                for d in 0..7 {
                    kit::tabular(
                        &painter,
                        pos2(x(d + 1) - columns.pad(), y),
                        Align2::RIGHT_CENTER,
                        &hours(grid.rounded_day(d, rounding)),
                        &body,
                        theme.text,
                    );
                }
                kit::tabular(
                    &painter,
                    pos2(right - SIDE, y),
                    Align2::RIGHT_CENTER,
                    &hours(grid.rounded_total(rounding)),
                    &FontId::proportional(theme.metrics.title_text_size),
                    theme.accent,
                );

                let card = Rect::from_min_max(pos2(left, top), pos2(right, r.bottom()));
                let mut under = vec![egui::Shape::rect_filled(card, 8.0, theme.card_bg)];
                if let Some(d) = today {
                    let column = Rect::from_min_max(pos2(x(d), top), pos2(x(d + 1), r.bottom()));
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

    /// A day's cell: its hours, or the field they're typed in. `pad`: from its
    /// edge to the hours.
    #[allow(clippy::too_many_arguments)]
    fn cell(
        &mut self,
        ui: &mut egui::Ui,
        cell: Rect,
        pad: f32,
        timer: u32,
        day: usize,
        shown: String,
        rounding: Rounding,
        grid: &Grid,
        live: bool,
        closed: bool,
        theme: &Theme,
    ) -> Option<Act> {
        let inset = (pad - 7.0).clamp(2.0, 5.0);
        let inner = cell.shrink2(vec2(inset, 4.0));
        let id = Id::new(("whir.timesheet.cell", timer, day));
        let selected = self.selected == Some((timer, day));
        if let Some(ed) = self.editing.as_mut().filter(|_| selected) {
            if ed.focus {
                prime(ui.ctx(), id, &ed.text);
                ed.focus = false;
            }
            // Keys the cell acts on, before the field sees them. Esc doesn't
            // ask who has the keyboard: egui has let go of it by now, and the
            // field would take that for leaving, and put the hours down.
            let focused = ui.memory(|m| m.has_focus(id));
            let key = ui.input_mut(|i| {
                if i.consume_key(Modifiers::NONE, Key::Escape) {
                    return Some((Modifiers::NONE, Key::Escape));
                }
                [
                    (Modifiers::SHIFT, Key::Tab),
                    (Modifiers::NONE, Key::Tab),
                    (Modifiers::NONE, Key::Enter),
                ]
                .into_iter()
                .filter(|_| focused)
                .find(|&(m, k)| i.consume_key(m, k))
            });
            // On its own, not added to the table: the row's place is settled.
            let mut slot = ui.new_child(
                UiBuilder::new()
                    .max_rect(inner)
                    .layout(Layout::left_to_right(Align::Center)),
            );
            let response = slot.add_sized(
                inner.size(),
                TextEdit::singleline(&mut ed.text)
                    .id(id)
                    .font(FontId::proportional(theme.metrics.body_text_size))
                    .horizontal_align(Align::Max)
                    .margin(vec2(pad - inset, 2.0)),
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
            painter.rect_filled(inner, 5.0, theme.field_bg);
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
        kit::tabular(
            painter,
            pos2(cell.right() - pad, cell.center().y),
            Align2::RIGHT_CENTER,
            &shown,
            &FontId::proportional(theme.metrics.body_text_size),
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

    /// The last row's button, and the list of projects behind it: found by a few
    /// letters of their customer or name.
    fn adder(
        &mut self,
        ui: &mut egui::Ui,
        area: Rect,
        state: &State,
        rows: &[u32],
        theme: &Theme,
    ) -> Option<Act> {
        let id = Id::new("whir.timesheet.adder");
        let mut slot = ui.new_child(
            UiBuilder::new()
                .max_rect(area)
                .layout(Layout::left_to_right(Align::Center)),
        );
        let button = kit::icon_action(&mut slot, Icon::Plus, "Add a project", theme);
        if button.clicked() {
            self.adder = Adder {
                focus: true,
                ..Adder::default()
            };
            egui::Popup::toggle_id(ui.ctx(), id);
        }
        let mut act = None;
        let a = &mut self.adder;
        toolbar::flyout(ui, id, button.rect, |ui| {
            ui.set_width(PICKER_WIDTH);
            ui.spacing_mut().item_spacing.y = 2.0;
            let (up, down, enter) = ui.input_mut(|i| {
                (
                    i.consume_key(Modifiers::NONE, Key::ArrowUp),
                    i.consume_key(Modifiers::NONE, Key::ArrowDown),
                    i.key_pressed(Key::Enter),
                )
            });
            let field = ui.add(
                TextEdit::singleline(&mut a.query)
                    .hint_text("Find a project")
                    .desired_width(f32::INFINITY)
                    .return_key(None),
            );
            if std::mem::take(&mut a.focus) {
                field.request_focus();
            }
            if field.changed() {
                a.selected = 0;
            }
            ui.add_space(4.0);
            let projects: Vec<(u32, String, &str)> = state
                .by_customer(false, false)
                .into_iter()
                .flat_map(|(_, timers)| timers)
                .filter(|t| !rows.contains(&t.id))
                .map(|t| (t.id, state.label(t.id), t.code.as_str()))
                .collect();
            let mut found = fuzzy::rank(&a.query, projects.iter().map(|p| p.1.as_str()));
            found.truncate(PICKER_ROWS);
            let last = found.len().saturating_sub(1);
            a.selected = match (up, down) {
                (true, _) if a.selected == 0 => last,
                (true, _) => a.selected - 1,
                (_, true) if a.selected >= last => 0,
                (_, true) => a.selected + 1,
                _ => a.selected.min(last),
            };
            let font = FontId::proportional(theme.metrics.body_text_size);
            let colors = (theme.text, theme.accent);
            let mut picked = None;
            for (row, (i, hit)) in found.iter().enumerate() {
                let (timer, label, code) = &projects[*i];
                let words = fuzzy::job(label, &hit.at, &font, colors, PICKER_WIDTH);
                let on = row == a.selected;
                let icon = Some(Icon::People);
                if kit::result_row(ui, icon, words, code, on, theme).clicked() {
                    picked = Some(*timer);
                }
            }
            if found.is_empty() {
                ui.add_space(4.0);
                let why = if a.query.trim().is_empty() {
                    "Every project is on the timecard."
                } else {
                    "No project by that name."
                };
                note(ui, why, theme);
                ui.add_space(4.0);
            }
            if enter {
                picked = picked.or(found.get(a.selected).map(|(i, _)| projects[*i].0));
            }
            ui.separator();
            let words = fuzzy::job("New project…", &[], &font, colors, PICKER_WIDTH);
            let plus = Some(Icon::Plus);
            let new = kit::result_row(ui, plus, words, "", false, theme).clicked();
            if new {
                a.form = Some(Form {
                    customer: state.by_customer(false, true).first().map(|(c, _)| c.id),
                    ..Form::default()
                });
            }
            if let Some(timer) = picked {
                act = Some(Act::AddRow(timer));
            }
            picked.is_some() || new
        });
        act
    }

    /// A new project: its customer (or a new one), its name and its code.
    fn new_project_dialog(
        &mut self,
        ui: &mut egui::Ui,
        store: &mut Store,
        state: &State,
        theme: &Theme,
    ) {
        let Some(form) = &mut self.adder.form else {
            return;
        };
        let ctx = ui.ctx().clone();
        let customers: Vec<Option<u32>> = state
            .by_customer(false, true)
            .iter()
            .map(|(c, _)| Some(c.id))
            .chain([None])
            .collect();
        let name_of = |c: Option<u32>| match c.and_then(|c| state.customer(c)) {
            Some(c) => c.name.clone(),
            None => "New customer…".to_owned(),
        };
        let mut add = false;
        let mut cancel = false;
        let id = Id::new("whir.timesheet.new_project");
        let modal = kit::dialog(&ctx, id, 360.0, theme, |ui| {
            kit::dialog_title(ui, "New project", theme);
            ui.spacing_mut().item_spacing.y = 8.0;
            let picked = kit::dropdown(ui, id.with("customer"), form.customer, &customers, name_of);
            if let Some(c) = picked {
                form.customer = c;
            }
            let wide = f32::INFINITY;
            if form.customer.is_none() {
                let field = TextEdit::singleline(&mut form.new_customer)
                    .hint_text("New customer's name")
                    .desired_width(wide);
                ui.add(field);
            }
            let field = TextEdit::singleline(&mut form.name)
                .hint_text("Project")
                .desired_width(wide);
            let name = ui.add(field);
            if !ui.ctx().memory(|m| m.focused().is_some()) {
                name.request_focus();
            }
            let field = TextEdit::singleline(&mut form.code)
                .hint_text("Code (optional)")
                .desired_width(wide);
            ui.add(field);
            let named = !form.name.trim().is_empty()
                && (form.customer.is_some() || !form.new_customer.trim().is_empty());
            kit::dialog_actions(ui, |ui| {
                cancel = kit::action(ui, "Cancel", theme).clicked();
                ui.add_enabled_ui(named, |ui| {
                    add = kit::primary(ui, "Add", theme).clicked();
                });
            });
            add |= named && ui.input(|i| i.key_pressed(Key::Enter));
        });
        if add {
            if let Some(form) = self.adder.form.take() {
                self.act(&ctx, store, Act::NewProject(form));
            }
        } else if cancel || modal.should_close() {
            self.adder.form = None;
        }
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
            Act::NewProject(form) => {
                let made = self.change(store, |tx| {
                    let customer = form
                        .customer
                        .unwrap_or_else(|| tx.state.add_customer(form.new_customer.trim()));
                    tx.state
                        .add_timer(customer, form.name.trim(), form.code.trim())
                });
                if let Some(id) = made {
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
        let m = theme.metrics;
        let date = self.monday + day as i64;
        let title = format!(
            "{} — {}",
            state.label(timer),
            format_date(&Stamp::midnight(date).local(), false)
        );
        let title = RichText::new(title)
            .font(kit::strong(m.body_text_size))
            .color(theme.text);
        ui.label(title);
        ui.add_space(6.0);
        let entries: Vec<Entry> = week
            .entries
            .iter()
            .filter(|e| e.timer == timer && e.day() == date)
            .cloned()
            .collect();
        let closed = week.closed.is_some();
        let running = state
            .running
            .as_ref()
            .filter(|r| r.timer == timer && r.start < Stamp::midnight(date + 1) && now.day >= date);
        if entries.is_empty() && running.is_none() {
            note(ui, "No entries.", theme);
            return;
        }
        let mut change: Option<(Entry, Option<Span>)> = None;
        let mut mistake = false;
        let font = FontId::proportional(m.body_text_size);
        kit::card(ui, theme, |c| {
            for e in &entries {
                let end_secs = if e.end.day > e.day() {
                    MIDNIGHT
                } else {
                    e.end.secs
                };
                let key = (e.timer, e.start.seconds(), e.end.seconds());
                let id = |part: &str| Id::new(("whir.entry", key, part.to_owned()));
                let from = clock_text(e.start.secs);
                let to = clock_text(end_secs);
                let clock = |part: &str, current| Inline {
                    id: id(part),
                    current,
                    hint: "",
                    width: CLOCK_W,
                    font: font.clone(),
                    color: theme.text,
                };
                c.field_row(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    ui.horizontal(|ui| {
                        if closed {
                            fixed(ui, CLOCK_W, &from, theme.text, theme);
                            dash(ui, theme);
                            fixed(ui, CLOCK_W, &to, theme.text, theme);
                            fixed(
                                ui,
                                LENGTH_W,
                                &format_clock(e.seconds()),
                                theme.text_dim,
                                theme,
                            );
                            ui.label(RichText::new(&e.note).color(theme.text));
                            return;
                        }
                        let start = kit::inline_text(ui, clock("start", &from), theme);
                        dash(ui, theme);
                        let end = kit::inline_text(ui, clock("end", &to), theme);
                        fixed(
                            ui,
                            LENGTH_W,
                            &format_clock(e.seconds()),
                            theme.text_dim,
                            theme,
                        );
                        let room = ui.available_width() - m.tool_size - 4.0;
                        let what = Inline {
                            id: id("note"),
                            current: &e.note,
                            hint: "What was it for?",
                            width: room.max(60.0),
                            font: font.clone(),
                            color: theme.text,
                        };
                        let note = kit::inline_text(ui, what, theme);
                        if toolbar::tool(ui, Icon::Delete, "Delete this entry", true, theme) {
                            change = Some((e.clone(), None));
                        }
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
                                _ => mistake = true,
                            }
                        }
                    });
                });
            }
            if let Some(r) = running {
                let from = if r.start.day < date { 0 } else { r.start.secs };
                let to = if now.day > date { MIDNIGHT } else { now.secs };
                c.field_row(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    ui.horizontal(|ui| {
                        fixed(ui, CLOCK_W, &clock_text(from), theme.text, theme);
                        dash(ui, theme);
                        fixed(ui, CLOCK_W, "now", theme.accent, theme);
                        let length = format_clock((to - from) as i64);
                        fixed(ui, LENGTH_W, &length, theme.accent, theme);
                        let what = if r.note.is_empty() {
                            "(running)".to_owned()
                        } else {
                            format!("{} (running)", r.note)
                        };
                        ui.label(RichText::new(what).color(theme.text_dim));
                    });
                });
            }
        });
        if mistake {
            self.error = Some("A start before its end, like 9:05 and 10:30.".into());
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
        let mut go = None;
        kit::group(ui, "Closed weeks", theme, |g| {
            for &(monday, at, total) in archive.iter() {
                let about = format!(
                    "{} h · closed {}",
                    hours(total),
                    format_date(&at.local(), false)
                );
                let (sheet, _) = csv_paths(store.dir(), monday);
                g.setting(&week_title(monday), &about, 112.0, |ui| {
                    if sheet.exists() {
                        kit::more_button(ui, theme, |ui| {
                            if kit::entry(ui, None, "Open timesheet", "", true) {
                                cx.requests.push(DocRequest::Open(vec![sheet.clone()]));
                            }
                            let reveal = Some(Icon::Folder);
                            if kit::entry(ui, reveal, "Show in Explorer", "", true) {
                                cx.requests.push(DocRequest::Reveal(sheet.clone()));
                            }
                        });
                    }
                    if kit::action(ui, "View", theme).clicked() {
                        go = Some(monday);
                    }
                });
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

    #[test]
    fn the_table_gives_way_a_step_at_a_time() {
        // All the room it could want: every column at its widest, two for names.
        let wide = Columns::fit(1120.0);
        assert_eq!((wide.day, wide.total), (DAY_MAX, TOTAL_MAX));
        assert_eq!(wide.customer, Some(CUSTOMER_MAX.min(516.0 * 0.42).floor()));
        assert!(wide.dates());

        // The days give way first, and customer and project stay apart.
        let narrower = Columns::fit(800.0);
        assert!(narrower.day < DAY_MAX && narrower.day >= DAY_MIN);
        assert!(narrower.customer.is_some());
        assert!(!narrower.dates());
        assert_eq!(narrower.total, TOTAL_MIN);

        // Then they share a column.
        let shared = Columns::fit(600.0);
        assert_eq!(shared.day, DAY_MIN);
        assert_eq!(shared.customer, None);

        // And past the least it can be, the table stops shrinking.
        let least = LABEL_MIN + 7.0 * DAY_MIN + TOTAL_MIN;
        assert_eq!(Columns::fit(300.0), Columns::fit(least));
        assert_eq!(Columns::fit(300.0).width, least);
    }

    #[test]
    fn the_columns_always_add_up_to_the_table() {
        for room in (300..1400).step_by(7) {
            let c = Columns::fit(room as f32);
            let labels = c.customer.unwrap_or(0.0) + c.project;
            assert!(
                (labels + 7.0 * c.day + c.total - c.width).abs() < 0.01,
                "{room}: {c:?}"
            );
            assert!(
                c.project >= LABEL_MIN.min(LABELS_MIN - CUSTOMER_MAX),
                "{room}: {c:?}"
            );
            assert_eq!(c.x(0), labels, "{room}");
            assert_eq!(c.x(7), c.width - c.total, "{room}");
        }
    }
}
