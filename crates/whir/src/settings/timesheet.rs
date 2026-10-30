//! The timesheet: a week at a glance (a row per project, a column per day,
//! hours as they go on the timecard), the entries behind a day, adding and
//! changing them, and closing the week out to CSV.

use egui::{Align, Color32, Id, Layout, RichText, Sense, vec2};
use whir_core::theme::Theme;
use whir_core::time::{Stamp, civil_from_days, format_date, format_duration, format_hm, monday_of};
use whir_core::timesheet::{Entry, Grid, State, Store, Week, csv_paths, timesheet_tsv};
use whir_core::{DocCx, DocRequest};

use super::{clock_text, edit_line, heading, note, parse_clock, parse_duration, primary, toast};

const DAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const PROJECT_W: f32 = 250.0;
const CELL: egui::Vec2 = vec2(62.0, 26.0);
const MIDNIGHT: u32 = 24 * 3600;

/// An entry's start, end and note, as changed.
type Span = (Stamp, Stamp, String);

pub struct View {
    /// The week shown.
    monday: i64,
    /// The cell whose entries are listed: a timer, and a day (0 = Monday).
    pub(super) selected: Option<(u32, usize)>,
    adding: Option<Draft>,
    pub(super) confirm_close: bool,
    /// Closed weeks: Monday, when, rounded hours. Read when first shown.
    archive: Option<Vec<(i64, Stamp, f64)>>,
    /// Earlier weeks with time in them that are still open.
    pub(super) open_before: Option<Vec<i64>>,
    error: Option<String>,
}

/// Time being added by hand.
struct Draft {
    timer: u32,
    day: usize,
    start: String,
    end: String,
    length: String,
    note: String,
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

/// A day's cell: its hours, centred; clickable.
fn cell(ui: &mut egui::Ui, text: &str, selected: bool, today: bool, theme: &Theme) -> bool {
    let (rect, response) = ui.allocate_exact_size(CELL, Sense::CLICK);
    let fill = if selected {
        theme.drawer_item_active
    } else if response.hovered() {
        theme.drawer_item_hover
    } else if today {
        theme.accent.gamma_multiply(0.08)
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect.shrink(1.0), 4.0, fill);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(13.5),
        theme.text,
    );
    response.clicked()
}

/// A fixed-width label in the grid.
fn fixed(ui: &mut egui::Ui, text: RichText, width: f32) {
    ui.allocate_ui_with_layout(
        vec2(width, CELL.y),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.set_min_width(width);
            ui.add(egui::Label::new(text).truncate());
        },
    );
}

fn centered(ui: &mut egui::Ui, text: RichText) {
    ui.allocate_ui_with_layout(
        CELL,
        Layout::centered_and_justified(egui::Direction::TopDown),
        |ui| {
            ui.label(text);
        },
    );
}

impl View {
    pub fn new() -> View {
        View {
            monday: monday_of(Stamp::now().day),
            selected: None,
            adding: None,
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
        self.adding = None;
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

        // The week, and what can be done with it.
        ui.horizontal(|ui| {
            if ui
                .button(RichText::new(" ‹ ").size(16.0))
                .on_hover_text("The week before")
                .clicked()
            {
                self.go(self.monday - 7);
            }
            if ui
                .button(RichText::new(" › ").size(16.0))
                .on_hover_text("The week after")
                .clicked()
            {
                self.go(self.monday + 7);
            }
            ui.add_space(6.0);
            ui.label(
                RichText::new(week_title(self.monday))
                    .size(17.0)
                    .color(theme.text),
            );
            if self.monday != this_week && ui.button("This week").clicked() {
                self.go(this_week);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                match week.closed {
                    Some(at) => {
                        if ui.button("Reopen").clicked() {
                            let monday = self.monday;
                            self.change(store, |tx| tx.reopen_week(monday));
                        }
                        let (sheet, _) = csv_paths(store.dir(), self.monday);
                        if sheet.exists() && ui.button("Open timesheet").clicked() {
                            cx.requests.push(DocRequest::Open(vec![sheet]));
                        }
                        let when = format_date(&at.local(), false);
                        ui.label(RichText::new(format!("Closed {when}")).color(theme.text_dim));
                    }
                    None => {
                        if primary(ui, "Close week…", theme) {
                            self.confirm_close = true;
                        }
                        if ui
                            .button("Copy")
                            .on_hover_text("The week as a table, to paste into Excel or an email")
                            .clicked()
                        {
                            ui.ctx()
                                .copy_text(timesheet_tsv(&state, &week, running, now));
                            toast(cx, "Copied the week");
                        }
                    }
                }
            });
        });
        ui.add_space(6.0);

        // An earlier week left open (Monday morning, last week's timecard).
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
                ui.add_space(8.0);
            }
        }
        if let Some(e) = &self.error {
            ui.label(RichText::new(e).color(theme.danger));
            ui.add_space(6.0);
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
            ui.add_space(6.0);
        }

        self.grid(ui, &state, &grid, now, theme);
        ui.add_space(6.0);
        note(
            ui,
            &format!(
                "Exact time {} · each day rounded to {} per project, as on the timecard.",
                format_duration(grid.total()),
                rounding.label().to_lowercase()
            ),
            theme,
        );
        ui.add_space(12.0);

        // The entries behind the chosen day, and adding time.
        if let Some((timer, day)) = self.selected {
            self.entries(ui, store, &state, &week, timer, day, now, theme);
            ui.add_space(12.0);
        }
        if self.adding.is_some() {
            self.add_form(ui, store, &state, theme);
        } else if !state.by_customer(false, false).is_empty() && ui.button("+ Add time").clicked() {
            let today = (now.day - self.monday).clamp(0, 6) as usize;
            let (timer, day) = self.selected.unwrap_or((
                state
                    .last
                    .or(state.timers.first().map(|t| t.id))
                    .unwrap_or(0),
                today,
            ));
            self.adding = Some(Draft {
                timer,
                day,
                start: String::new(),
                end: String::new(),
                length: String::new(),
                note: String::new(),
            });
        }

        ui.add_space(24.0);
        self.archive(ui, store, cx, theme);
        self.close_dialog(ui, store, &state, &week, now, cx, theme);
    }

    fn grid(&mut self, ui: &mut egui::Ui, state: &State, grid: &Grid, now: Stamp, theme: &Theme) {
        if grid.rows.is_empty() {
            note(ui, "No time this week yet.", theme);
            return;
        }
        let rounding = state.options.rounding;
        let today = (now.day - self.monday) as usize;
        let mut rows: Vec<u32> = grid.rows.iter().map(|(t, _)| *t).collect();
        rows.sort_by_key(|&t| {
            let (c, p) = state.names(t);
            (c.to_lowercase(), p.to_lowercase())
        });
        egui::Grid::new(("whir.timesheet", self.monday))
            .spacing(vec2(2.0, 2.0))
            .show(ui, |ui| {
                fixed(ui, RichText::new(""), PROJECT_W);
                for (d, name) in DAYS.iter().enumerate() {
                    let (_, _, day) = civil_from_days(self.monday + d as i64);
                    let color = if d == today {
                        theme.accent
                    } else {
                        theme.text_dim
                    };
                    centered(ui, RichText::new(format!("{name} {day}")).color(color));
                }
                centered(ui, RichText::new("Total").color(theme.text_dim));
                ui.end_row();

                let mut customer = None;
                for t in rows {
                    let (c, p) = state.names(t);
                    if customer != Some(c) {
                        customer = Some(c);
                        fixed(ui, RichText::new(c).strong().color(theme.text), PROJECT_W);
                        ui.end_row();
                    }
                    let code = state.timer(t).map_or("", |t| t.code.as_str());
                    let label = if code.is_empty() {
                        RichText::new(format!("   {p}")).color(theme.text)
                    } else {
                        RichText::new(format!("   {p}  ·  {code}")).color(theme.text)
                    };
                    fixed(ui, label, PROJECT_W);
                    for d in 0..7 {
                        let text = rounding.format(grid.cell(t, d));
                        let on = self.selected == Some((t, d));
                        if cell(ui, &text, on, d == today, theme) {
                            self.selected = if on { None } else { Some((t, d)) };
                            self.adding = None;
                        }
                    }
                    centered(
                        ui,
                        RichText::new(hours(grid.rounded_row(t, rounding)))
                            .strong()
                            .color(theme.text),
                    );
                    ui.end_row();
                }

                fixed(
                    ui,
                    RichText::new("Total").strong().color(theme.text),
                    PROJECT_W,
                );
                for d in 0..7 {
                    let h = grid.rounded_day(d, rounding);
                    centered(ui, RichText::new(hours(h)).strong().color(theme.text));
                }
                centered(
                    ui,
                    RichText::new(hours(grid.rounded_total(rounding)))
                        .strong()
                        .color(theme.accent),
                );
                ui.end_row();
            });
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
        ui.label(RichText::new(title).size(14.5).color(theme.text));
        ui.add_space(4.0);
        let entries: Vec<Entry> = week
            .entries
            .iter()
            .filter(|e| e.timer == timer && e.day() == date)
            .cloned()
            .collect();
        let mut change: Option<(Entry, Option<Span>)> = None;
        egui::Grid::new(("whir.entries", timer, date))
            .spacing(vec2(8.0, 4.0))
            .show(ui, |ui| {
                for e in &entries {
                    let key = (e.timer, e.start.seconds(), e.end.seconds());
                    let id = |part: &str| Id::new(("whir.entry", key, part.to_owned()));
                    let end_secs = if e.end.day > e.day() {
                        MIDNIGHT
                    } else {
                        e.end.secs
                    };
                    let start = edit_line(ui, id("start"), &clock_text(e.start.secs), 54.0, "");
                    ui.label("–");
                    let end = edit_line(ui, id("end"), &clock_text(end_secs), 54.0, "");
                    ui.label(RichText::new(format_duration(e.seconds())).color(theme.text_dim));
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
                    ui.label(
                        RichText::new(format_duration((to - from) as i64)).color(theme.text_dim),
                    );
                    ui.label(RichText::new(format!("{} (running)", r.note)).color(theme.text_dim));
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

    fn add_form(&mut self, ui: &mut egui::Ui, store: &mut Store, state: &State, theme: &Theme) {
        let Some(draft) = &mut self.adding else {
            return;
        };
        ui.label(RichText::new("Add time").size(14.5).color(theme.text));
        ui.add_space(4.0);
        let mut add = false;
        let mut cancel = false;
        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_id_salt("whir.add.timer")
                .width(240.0)
                .selected_text(state.label(draft.timer))
                .show_ui(ui, |ui| {
                    for (c, timers) in state.by_customer(false, false) {
                        for t in timers {
                            let label = format!("{} · {}", c.name, t.name);
                            ui.selectable_value(&mut draft.timer, t.id, label);
                        }
                    }
                });
            egui::ComboBox::from_id_salt("whir.add.day")
                .width(110.0)
                .selected_text(day_label(self.monday, draft.day))
                .show_ui(ui, |ui| {
                    for d in 0..7 {
                        ui.selectable_value(&mut draft.day, d, day_label(self.monday, d));
                    }
                });
            for (text, width, hint) in [
                (&mut draft.start, 54.0, "From"),
                (&mut draft.end, 54.0, "To"),
                (&mut draft.length, 64.0, "or 1:30"),
            ] {
                ui.add(
                    egui::TextEdit::singleline(text)
                        .desired_width(width)
                        .hint_text(hint),
                );
            }
            ui.add(
                egui::TextEdit::singleline(&mut draft.note)
                    .desired_width(300.0)
                    .hint_text("What was it for?"),
            );
            add = primary(ui, "Add", theme);
            cancel = ui.button("Cancel").clicked();
        });
        if cancel {
            self.adding = None;
            return;
        }
        if !add {
            return;
        }
        let date = self.monday + draft.day as i64;
        let midnight = Stamp::midnight(date);
        let start = parse_clock(&draft.start);
        let end = parse_end(&draft.end);
        let length = parse_duration(&draft.length);
        let (timer, note) = (draft.timer, draft.note.clone());
        let monday = self.monday;
        let span = match (start, end, length) {
            (Some(s), Some(e), _) if s < e => Some((s, e)),
            (Some(s), None, Some(l)) => Some((s, (s as i64 + l).min(MIDNIGHT as i64) as u32)),
            (None, Some(e), Some(l)) => Some(((e as i64 - l).max(0) as u32, e)),
            (None, None, Some(l)) => {
                // After the day's last entry (or from nine).
                let week = store.week(monday);
                let from = week
                    .entries
                    .iter()
                    .filter(|e| e.day() == date)
                    .map(|e| e.end.since(midnight) as u32)
                    .max()
                    .unwrap_or(9 * 3600);
                Some((from, (from as i64 + l).min(MIDNIGHT as i64) as u32))
            }
            _ => None,
        };
        let Some((s, e)) = span.filter(|(s, e)| s < e) else {
            self.error = Some("Give a start and an end (9:05, 10:30), or a length (1:30).".into());
            return;
        };
        if state.timer(timer).is_none() {
            self.error = Some("Choose a project.".into());
            return;
        }
        let (start, end) = (midnight.plus(s as i64), midnight.plus(e as i64));
        if self
            .change(store, |tx| tx.add(timer, start, end, &note))
            .is_some()
        {
            self.adding = None;
            self.selected = Some((timer, (date - monday) as usize));
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
        let modal = egui::Modal::new(Id::new("whir.close_week"))
            .backdrop_color(theme.modal_backdrop)
            .frame(
                egui::Frame::popup(&ctx.global_style())
                    .inner_margin(20.0)
                    .corner_radius(10.0),
            )
            .show(&ctx, |ui| {
                ui.set_width(420.0);
                ui.label(
                    RichText::new(format!("Close out {}?", week_title(monday)))
                        .size(16.0)
                        .color(theme.text),
                );
                ui.add_space(6.0);
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
                ui.add_space(16.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    cancel = ui.button("  Cancel  ").clicked();
                    close = primary(ui, "Close week", theme);
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

fn day_label(monday: i64, d: usize) -> String {
    let (_, _, day) = civil_from_days(monday + d as i64);
    format!("{} {day}", DAYS[d])
}
