//! The dock's panel, under its bar: the todo card, the "were you away?"
//! question, "switch milestones?", and either today's times (the ⌄) or the
//! milestone picker (the milestone on the bar). Rows of fixed heights in points, so
//! the panel's size is known before anything is drawn, and its window placed
//! to fit; egui draws them, with the window's buttons and fades.

use egui::{
    Align2, Button, Color32, CornerRadius, FontId, Id, Rect as Area, ScrollArea, Sense, Stroke,
    StrokeKind, Ui, UiBuilder, pos2, vec2,
};
use whir_core::icons::{self, Icon};
use whir_core::theme::Theme;
use whir_core::{kit, toolbar};

use crate::bar::{self, regular, semibold};
use crate::geom::Rect;

/// What a click on part of the panel does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    /// Time this milestone (picked from the list).
    Pick(u32),
    /// Yes: stop the running one and time this instead.
    Switch(u32),
    /// No: leave the running one be.
    KeepRunning,
    /// The away time counts.
    Keep,
    /// It doesn't.
    Drop,
    /// Open the timesheet in whir.
    Timesheet,
    /// Set up projects and milestones in whir.
    Milestones,
    /// Open that todo's note.
    Todo(usize),
    /// Mark it done.
    TodoDone(usize),
    /// The rest of the todos, in whir.
    MoreTodos,
}

/// What the panel is open to, besides what must be seen anyway.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    /// Today's time on each milestone (the ⌄).
    Today,
    /// Every milestone, to pick one (the milestone on the bar).
    Picker,
}

/// A milestone with time today.
pub struct DayRow {
    pub project: String,
    pub milestone: String,
    /// "1:02:33".
    pub time: String,
    pub running: bool,
}

pub struct MilestoneRow {
    pub id: u32,
    pub name: String,
    /// Its time today, "1:02:33" (empty for none).
    pub today: String,
    pub running: bool,
}

pub struct Group {
    pub project: String,
    pub milestones: Vec<MilestoneRow>,
}

/// Picking another milestone while one runs: say so first.
pub struct Confirm {
    pub to: u32,
    /// "Contoso · Automation".
    pub to_label: String,
    /// "Aardvark Oil · Project Meetings".
    pub from_label: String,
    /// Its time today.
    pub from_time: String,
}

/// Everything the panel shows.
#[derive(Default)]
pub struct Model {
    pub todos: Vec<String>,
    pub more_todos: usize,
    /// "Away 0:47 (10:02 – 10:49)".
    pub away: Option<String>,
    pub confirm: Option<Confirm>,
    pub view: Option<View>,
    pub today: Vec<DayRow>,
    /// "Thursday, October 29".
    pub date: String,
    /// Today's time on everything.
    pub total: String,
    pub groups: Vec<Group>,
    /// There is a timesheet folder to keep time in.
    pub available: bool,
}

impl Model {
    /// Nothing to show: no panel.
    pub fn is_empty(&self) -> bool {
        self.todos.is_empty()
            && self.away.is_none()
            && self.confirm.is_none()
            && self.view.is_none()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tone {
    Normal,
    Dim,
    Accent,
    Flag,
}

/// A line: a dot or an icon, text, and more text on the right.
#[derive(Debug)]
struct Line {
    /// A place for a dot (keeping names in line under a heading), and
    /// whether there is one in it.
    dot: Option<bool>,
    icon: Option<(Icon, Tone)>,
    text: String,
    tone: Tone,
    bold: bool,
    right: String,
    right_tone: Tone,
}

#[derive(Debug)]
enum Look {
    Heading(String),
    Row(Line),
    /// Project, milestone and time, in columns.
    Day {
        dot: bool,
        project: String,
        milestone: String,
        time: String,
    },
    Button {
        label: String,
        primary: bool,
    },
    Tick,
    Rule,
}

#[derive(Debug)]
pub struct Item {
    pub rect: Rect,
    pub hit: Option<Hit>,
    look: Look,
}

/// Rows and what's around them, in points.
const PAD: i32 = 8;
const LINE: i32 = 30;
const SMALL: i32 = 24;
const HEADING: i32 = 24;
const RULE: i32 = 9;

fn row(text: impl Into<String>, tone: Tone) -> Line {
    Line {
        dot: None,
        icon: None,
        text: text.into(),
        tone,
        bold: false,
        right: String::new(),
        right_tone: Tone::Dim,
    }
}

/// The panel's rows for `model`, `width` points wide.
pub fn layout(model: &Model, width: i32) -> Vec<Item> {
    let (pad, line, small, heading, rule) = (PAD, LINE, SMALL, HEADING, RULE);
    let (left, right) = (pad, width - pad);
    let mut items: Vec<Item> = Vec::new();
    let mut y = pad;
    let mut push = |rect: Rect, look: Look, hit: Option<Hit>| {
        items.push(Item { rect, hit, look });
    };
    let full = |y: i32, h: i32| Rect::new(left, y, right, y + h);
    let square = |y: i32, h: i32| Rect::new(right - h, y, right, y + h);
    // A rule before each section but the first.
    let mut sections = 0;
    let mut section = |push: &mut dyn FnMut(Rect, Look, Option<Hit>), y: &mut i32| {
        if sections > 0 {
            push(full(*y, rule), Look::Rule, None);
            *y += rule;
        }
        sections += 1;
    };

    if !model.todos.is_empty() {
        section(&mut push, &mut y);
        push(full(y, heading), Look::Heading("Todo".into()), None);
        y += heading;
        for (i, title) in model.todos.iter().enumerate() {
            let text = Rect::new(left, y, right - line - 2, y + line);
            let look = Look::Row(Line {
                icon: Some((Icon::Flag, Tone::Flag)),
                ..row(title.clone(), Tone::Normal)
            });
            push(text, look, Some(Hit::Todo(i)));
            push(square(y, line), Look::Tick, Some(Hit::TodoDone(i)));
            y += line;
        }
        if model.more_todos > 0 {
            let look = Look::Row(row(format!("{} more in whir", model.more_todos), Tone::Dim));
            push(full(y, small), look, Some(Hit::MoreTodos));
            y += small;
        }
    }

    // Two buttons side by side.
    let pair = |push: &mut dyn FnMut(Rect, Look, Option<Hit>), y: i32, a, b| {
        let (a, ha, primary): (&str, Hit, bool) = a;
        let (b, hb): (&str, Hit) = b;
        let gap = 6;
        let half = (right - left - gap) / 2;
        push(
            Rect::new(left, y, left + half, y + line),
            Look::Button {
                label: a.into(),
                primary,
            },
            Some(ha),
        );
        push(
            Rect::new(right - half, y, right, y + line),
            Look::Button {
                label: b.into(),
                primary: false,
            },
            Some(hb),
        );
    };

    if let Some(away) = &model.away {
        section(&mut push, &mut y);
        let look = Look::Row(Line {
            icon: Some((Icon::Stopwatch, Tone::Accent)),
            ..row(away.clone(), Tone::Normal)
        });
        push(full(y, line), look, None);
        y += line + 2;
        pair(
            &mut push,
            y,
            ("Keep it", Hit::Keep, false),
            ("Drop it", Hit::Drop),
        );
        y += line;
    }

    if let Some(c) = &model.confirm {
        section(&mut push, &mut y);
        let look = Look::Row(Line {
            bold: true,
            ..row(format!("Switch to {}?", c.to_label), Tone::Normal)
        });
        push(full(y, line), look, None);
        y += line;
        let look = Look::Row(Line {
            right: c.from_time.clone(),
            right_tone: Tone::Accent,
            ..row(format!("{} is running", c.from_label), Tone::Dim)
        });
        push(full(y, small), look, None);
        y += small + 4;
        pair(
            &mut push,
            y,
            ("Switch", Hit::Switch(c.to), true),
            ("Keep it running", Hit::KeepRunning),
        );
        y += line;
    }

    match model.view {
        Some(View::Today) => {
            section(&mut push, &mut y);
            if !model.available {
                push(full(y, line), Look::Row(row(UNAVAILABLE, Tone::Dim)), None);
                y += line;
            } else if model.today.is_empty() {
                push(
                    full(y, line),
                    Look::Row(row("Nothing tracked today yet", Tone::Dim)),
                    None,
                );
                y += line;
            }
            for d in &model.today {
                let look = Look::Day {
                    dot: d.running,
                    project: d.project.clone(),
                    milestone: d.milestone.clone(),
                    time: d.time.clone(),
                };
                push(full(y, line), look, None);
                y += line;
            }
            push(full(y, rule), Look::Rule, None);
            y += rule;
            let look = Look::Row(Line {
                bold: true,
                right: model.total.clone(),
                right_tone: Tone::Accent,
                ..row(model.date.clone(), Tone::Normal)
            });
            push(full(y, line), look, Some(Hit::Timesheet));
            y += line;
        }
        Some(View::Picker) => {
            section(&mut push, &mut y);
            if !model.available {
                push(full(y, line), Look::Row(row(UNAVAILABLE, Tone::Dim)), None);
                y += line;
            }
            for g in &model.groups {
                push(full(y, heading), Look::Heading(g.project.clone()), None);
                y += heading;
                for t in &g.milestones {
                    let look = Look::Row(Line {
                        dot: Some(t.running),
                        right: t.today.clone(),
                        ..row(
                            t.name.clone(),
                            if t.running {
                                Tone::Accent
                            } else {
                                Tone::Normal
                            },
                        )
                    });
                    push(full(y, line), look, Some(Hit::Pick(t.id)));
                    y += line;
                }
            }
            if model.available {
                if !model.groups.is_empty() {
                    push(full(y, rule), Look::Rule, None);
                    y += rule;
                }
                let label = if model.groups.is_empty() {
                    "Add projects and milestones…"
                } else {
                    "Projects & milestones…"
                };
                let look = Look::Row(Line {
                    icon: Some((Icon::Plus, Tone::Accent)),
                    ..row(label, Tone::Normal)
                });
                push(full(y, line), look, Some(Hit::Milestones));
                y += line;
            }
        }
        None => {}
    }
    let _ = y;
    items
}

const UNAVAILABLE: &str = "The Documents folder isn't available.";

/// How tall the panel is with these rows, in points.
pub fn height(items: &[Item]) -> i32 {
    items
        .iter()
        .map(|i| i.rect.bottom)
        .max()
        .map_or(0, |b| b + PAD)
}

fn tone(theme: &Theme, t: Tone) -> Color32 {
    match t {
        Tone::Normal => theme.text,
        Tone::Dim => theme.text_dim,
        Tone::Accent => theme.accent,
        Tone::Flag => theme.note_glyph,
    }
}

/// Text from `x`, in the middle of `row`, cut short at `end`.
fn words(ui: &Ui, row: Area, x: f32, end: f32, text: &str, font: FontId, color: Color32) {
    if end > x && !text.is_empty() {
        let galley = kit::one_line(ui.painter(), text, font, end - x);
        let y = row.center().y - galley.size().y / 2.0;
        ui.painter().galley(pos2(x, y), galley, color);
    }
}

/// The running one's dot, in the place kept for it either way so that names
/// line up.
fn dot(ui: &Ui, row: Area, x: f32, theme: &Theme) {
    let at = pos2(x + 3.5, row.center().y);
    ui.painter().circle_filled(at, 3.5, theme.accent);
}

/// Draw the panel. Returns what was clicked. `from_top`: start at its top again.
pub fn show(
    ui: &mut Ui,
    items: &[Item],
    corners: CornerRadius,
    from_top: bool,
    theme: &Theme,
) -> Option<Hit> {
    let whole = ui.max_rect();
    let (fill, line) = bar::sheet(theme);
    ui.painter()
        .rect(whole, corners, fill, line, StrokeKind::Inside);
    let inside = whole.shrink(1.0);
    let tall = height(items) as f32;
    let mut clicked = None;
    ui.scope_builder(UiBuilder::new().max_rect(inside), |ui| {
        let mut scroll = ScrollArea::vertical().auto_shrink(false);
        if from_top {
            scroll = scroll.vertical_scroll_offset(0.0);
        }
        scroll.show(ui, |ui| {
            // Or the space after the rows counts as more to scroll to.
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            let size = vec2(inside.width(), tall - 2.0);
            let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
            let origin = rect.min - vec2(1.0, 1.0);
            for (i, item) in items.iter().enumerate() {
                let r = bar::area(item.rect, 1.0).translate(origin.to_vec2());
                if ui.is_rect_visible(r) && draw_item(ui, i, item, r, theme) {
                    clicked = item.hit;
                }
            }
        });
    });
    clicked
}

/// A row that can be clicked, with the toolbar's fade behind it.
fn clickable(ui: &mut Ui, i: usize, r: Area, theme: &Theme) -> egui::Response {
    let response = ui.interact(r, Id::new(("whir.dock.panel", i)), Sense::CLICK);
    if let Some(fill) = toolbar::fill_of(ui, &response, false, theme) {
        let radius = theme.metrics.control_radius;
        ui.painter().rect_filled(r, radius, fill);
    }
    response
}

/// Draw a row. True if it was clicked.
fn draw_item(ui: &mut Ui, i: usize, item: &Item, r: Area, theme: &Theme) -> bool {
    let m = theme.metrics;
    let body = m.body_text_size;
    if let Look::Button { label, primary } = &item.look {
        return ui
            .scope(|ui| {
                if *primary {
                    kit::as_primary(ui, theme);
                }
                ui.put(r, Button::new(label.as_str())).clicked()
            })
            .inner;
    }
    let response = item.hit.map(|_| clickable(ui, i, r, theme));
    match &item.look {
        Look::Heading(name) => {
            let font = semibold(m.small_text_size);
            let (x, end) = (r.left() + 6.0, r.right() - 6.0);
            words(ui, r, x, end, name, font, theme.text_dim);
        }
        Look::Row(Line {
            dot: place,
            icon: glyph,
            text: label,
            tone: t,
            bold,
            right,
            right_tone,
        }) => {
            let mut x = r.left() + 8.0;
            if let Some(on) = place {
                if *on {
                    dot(ui, r, x, theme);
                }
                x += 14.0;
            }
            if let Some((g, gt)) = glyph {
                let at = pos2(x + 7.0, r.center().y);
                icons::paint(ui.painter(), at, *g, m.icon_size, tone(theme, *gt));
                x += 24.0;
            }
            let end = r.right() - 8.0;
            let font = if *bold { semibold(body) } else { regular(body) };
            let at = pos2(end, r.center().y);
            let color = tone(theme, *right_tone);
            let taken = if right.is_empty() {
                0.0
            } else {
                let align = Align2::RIGHT_CENTER;
                let place = kit::tabular(ui.painter(), at, align, right, &font, color);
                place.width() + 10.0
            };
            words(ui, r, x, end - taken, label, font, tone(theme, *t));
        }
        Look::Day {
            dot: running,
            project,
            milestone,
            time,
        } => {
            let x = r.left() + 8.0;
            if *running {
                dot(ui, r, x, theme);
            }
            let x = x + 14.0;
            let end = r.right() - 8.0;
            let clock = semibold(body);
            let at = pos2(end, r.center().y);
            let width = kit::tabular_width(ui.painter(), "00:00:00", &clock);
            let align = Align2::RIGHT_CENTER;
            kit::tabular(ui.painter(), at, align, time, &clock, theme.accent);
            let names = end - width - 10.0 - x;
            let split = x + names * 0.4;
            words(ui, r, x, split - 6.0, project, clock, theme.text);
            words(
                ui,
                r,
                split,
                x + names,
                milestone,
                regular(body),
                theme.milestone,
            );
        }
        Look::Tick => {
            let color = if response.as_ref().is_some_and(|h| h.hovered()) {
                theme.text
            } else {
                theme.text_dim
            };
            icons::paint(ui.painter(), r.center(), Icon::Check, 14.0, color);
        }
        Look::Rule => {
            let y = ui.painter().round_to_pixel_center(r.center().y);
            let ends = [pos2(r.left() + 4.0, y), pos2(r.right() - 4.0, y)];
            ui.painter()
                .line_segment(ends, Stroke::new(1.0, theme.separator));
        }
        Look::Button { .. } => {}
    }
    response.is_some_and(|h| h.clicked())
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::stage::Stage;

    fn model() -> Model {
        Model {
            todos: vec!["Fix Acme invoice".into(), "Call Bob".into()],
            more_todos: 1,
            away: Some("Away 0:47".into()),
            confirm: Some(Confirm {
                to: 9,
                to_label: "Contoso · Automation".into(),
                from_label: "Aardvark Oil · Project Meetings".into(),
                from_time: "1:12:40".into(),
            }),
            view: Some(View::Picker),
            today: vec![
                DayRow {
                    project: "Aardvark Oil".into(),
                    milestone: "Project Meetings".into(),
                    time: "1:12:40".into(),
                    running: true,
                },
                DayRow {
                    project: "Contoso".into(),
                    milestone: "Fuel Reconciliation".into(),
                    time: "2:03:11".into(),
                    running: false,
                },
            ],
            date: "Thursday, October 29".into(),
            total: "3:15:51".into(),
            groups: vec![
                Group {
                    project: "Aardvark Oil".into(),
                    milestones: vec![MilestoneRow {
                        id: 7,
                        name: "Project Meetings".into(),
                        today: "1:12:40".into(),
                        running: true,
                    }],
                },
                Group {
                    project: "Contoso".into(),
                    milestones: vec![MilestoneRow {
                        id: 9,
                        name: "Automation".into(),
                        today: String::new(),
                        running: false,
                    }],
                },
            ],
            available: true,
        }
    }

    fn assert_tidy(items: &[Item], width: i32) {
        let clickable: Vec<&Item> = items.iter().filter(|i| i.hit.is_some()).collect();
        for (i, a) in clickable.iter().enumerate() {
            assert!(a.rect.left >= 0 && a.rect.right <= width, "{a:?}");
            for b in &clickable[i + 1..] {
                assert!(a.rect.intersect(b.rect).is_empty(), "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn every_action_has_a_place_and_rows_dont_overlap() {
        let mut m = model();
        let items = layout(&m, 380);
        let hits: Vec<Hit> = items.iter().filter_map(|i| i.hit).collect();
        for h in [
            Hit::Todo(0),
            Hit::TodoDone(1),
            Hit::MoreTodos,
            Hit::Keep,
            Hit::Drop,
            Hit::Switch(9),
            Hit::KeepRunning,
            Hit::Pick(7),
            Hit::Pick(9),
            Hit::Milestones,
        ] {
            assert!(hits.contains(&h), "{h:?} missing");
        }
        assert_tidy(&items, 380);
        let pick = items.iter().find(|i| i.hit == Some(Hit::Pick(9))).unwrap();
        assert!(height(&items) > pick.rect.bottom);

        // Today: the times (only to read) and the day's total (the timesheet).
        m.view = Some(View::Today);
        let items = layout(&m, 380);
        assert_eq!(
            items
                .iter()
                .filter(|i| matches!(i.look, Look::Day { .. }))
                .count(),
            2
        );
        assert!(items.iter().any(|i| i.hit == Some(Hit::Timesheet)));
        assert!(!items.iter().any(|i| matches!(i.hit, Some(Hit::Pick(_)))));
        assert_tidy(&items, 380);
    }

    #[test]
    fn closed_it_shows_only_what_must_be_seen() {
        let mut m = model();
        m.view = None;
        let items = layout(&m, 380);
        assert!(!items.iter().any(|i| matches!(i.hit, Some(Hit::Pick(_)))));
        assert!(items.iter().any(|i| i.hit == Some(Hit::Keep)));
        m.todos.clear();
        m.away = None;
        m.confirm = None;
        assert!(m.is_empty());
        assert!(layout(&m, 380).is_empty());
        // Nothing to pick from yet: the way to set some up.
        let empty = Model {
            view: Some(View::Picker),
            available: true,
            ..Model::default()
        };
        let items = layout(&empty, 380);
        assert!(items.iter().any(|i| i.hit == Some(Hit::Milestones)));
    }

    fn frame(stage: &mut Stage, items: &[Item], size: (i32, i32), theme: &Theme) -> Option<Hit> {
        let corners = CornerRadius::same(8);
        let mut clicked = None;
        stage.run(size, |ui| {
            clicked = show(ui, items, corners, false, theme).or(clicked);
        });
        clicked
    }

    #[test]
    fn every_row_that_does_something_answers_a_click_at_every_scale() {
        let theme = Theme::dark();
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let items = layout(&model(), 380);
            let size = (
                (380.0 * scale) as i32,
                (height(&items) as f32 * scale).ceil() as i32,
            );
            let mut stage = Stage::new(&theme, scale);
            frame(&mut stage, &items, size, &theme);
            for item in items.iter().filter(|i| i.hit.is_some()) {
                let at = (
                    ((item.rect.left + item.rect.right) as f32 / 2.0 * scale) as i32,
                    ((item.rect.top + item.rect.bottom) as f32 / 2.0 * scale) as i32,
                );
                stage.pointer_moved(at);
                frame(&mut stage, &items, size, &theme);
                stage.button(at, true);
                assert_eq!(frame(&mut stage, &items, size, &theme), None);
                stage.button(at, false);
                let clicked = frame(&mut stage, &items, size, &theme);
                assert_eq!(clicked, item.hit, "at {scale}");
            }
        }
    }

    #[test]
    fn too_tall_for_the_room_it_scrolls() {
        let theme = Theme::dark();
        let items = layout(&model(), 380);
        let size = (380, 120);
        let mut stage = Stage::new(&theme, 1.0);
        frame(&mut stage, &items, size, &theme);
        let last = items.iter().rfind(|i| i.hit.is_some()).unwrap();
        assert!(last.rect.top > size.1, "it starts out of sight");
        stage.pointer_moved((190, 60));
        frame(&mut stage, &items, size, &theme);
        for _ in 0..80 {
            stage.wheel(-10.0);
            frame(&mut stage, &items, size, &theme);
            std::thread::sleep(std::time::Duration::from_millis(4));
        }
        let at = (190, size.1 - 18);
        stage.pointer_moved(at);
        frame(&mut stage, &items, size, &theme);
        stage.button(at, true);
        frame(&mut stage, &items, size, &theme);
        stage.button(at, false);
        assert_eq!(frame(&mut stage, &items, size, &theme), last.hit);
    }

    /// `cargo test -p whir-capture -- --ignored preview` writes the panel, in
    /// both themes, to PNGs in `WHIR_PREVIEW_DIR` (to look at).
    #[test]
    #[ignore = "writes pictures to look at"]
    fn preview() {
        let Some(dir) = std::env::var_os("WHIR_PREVIEW_DIR") else {
            return;
        };
        for (name, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
            for s in [1.0, 1.5] {
                for (view, m) in [
                    ("picker", model()),
                    ("today", {
                        let mut m = model();
                        m.view = Some(View::Today);
                        m.confirm = None;
                        m
                    }),
                ] {
                    let items = layout(&m, 380);
                    let size = (
                        (380.0 * s) as i32,
                        (height(&items) as f32 * s).ceil() as i32,
                    );
                    let mut stage = Stage::new(&theme, s);
                    frame(&mut stage, &items, size, &theme);
                    if let Some(pick) = items.iter().find(|i| i.hit == Some(Hit::Pick(9))) {
                        stage.pointer_moved((
                            ((pick.rect.left + 40) as f32 * s) as i32,
                            ((pick.rect.top + 10) as f32 * s) as i32,
                        ));
                        frame(&mut stage, &items, size, &theme);
                        std::thread::sleep(std::time::Duration::from_millis(120));
                        frame(&mut stage, &items, size, &theme);
                        frame(&mut stage, &items, size, &theme);
                    }
                    let path =
                        std::path::Path::new(&dir).join(format!("panel-{view}-{name}-{s}.png"));
                    stage.save_png(&path);
                }
            }
        }
    }

    #[test]
    fn it_draws() {
        let theme = Theme::dark();
        let items = layout(&model(), 380);
        let h = height(&items);
        let mut stage = Stage::new(&theme, 1.0);
        frame(&mut stage, &items, (380, h), &theme);
        let surface = stage.surface().unwrap();
        // Opaque inside, and some text drawn in the text colour's brightness.
        assert_eq!(surface.pixel(190, h / 2) >> 24, 0xff);
        let lit = (0..h).any(|y| (0..380).any(|x| (surface.pixel(x, y) >> 16) & 0xff > 0xc0));
        assert!(lit);
    }
}
