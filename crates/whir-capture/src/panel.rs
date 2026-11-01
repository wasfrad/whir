//! The dock's panel, under its bar: the todo card, the "were you away?"
//! question, "switch projects?", and either today's times (the ⌄) or the
//! project picker (the project on the bar). Rows of fixed heights, so the
//! layout is plain arithmetic and the same for drawing and for clicks; drawn
//! in software like the bar.

use whir_core::theme::{Color32, Theme};

use whir_core::icons::Icon;

use crate::geom::{Point, Rect};
use crate::paint::{Canvas, Surface};
use crate::text::{self, Align, Font, Text};

const FLAG: char = Icon::Flag.glyph();
const CHECK: char = Icon::Check.glyph();
const ADD: char = Icon::Plus.glyph();
const CLOCK: char = Icon::Stopwatch.glyph();

/// What a click on part of the panel does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    /// Time this project (picked from the list).
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
    /// Set up customers and projects in whir.
    Projects,
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
    /// Today's time on each project (the ⌄).
    Today,
    /// Every project, to pick one (the project on the bar).
    Picker,
}

/// A project with time today.
pub struct DayRow {
    pub customer: String,
    pub project: String,
    /// "1:02:33".
    pub time: String,
    pub running: bool,
}

pub struct ProjectRow {
    pub id: u32,
    pub name: String,
    /// Its time today, "1:02:33" (empty for none).
    pub today: String,
    pub running: bool,
}

pub struct Group {
    pub customer: String,
    pub projects: Vec<ProjectRow>,
}

/// Picking another project while one runs: say so first.
pub struct Confirm {
    pub to: u32,
    /// "Contoso · Automation".
    pub to_label: String,
    /// "Aardvark Oil · Customer Meetings".
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
    Project,
}

/// A line: a dot or an icon, text, and more text on the right.
#[derive(Debug)]
struct Line {
    /// A place for a dot (keeping names in line under a heading), and
    /// whether there is one in it.
    dot: Option<bool>,
    icon: Option<(char, Tone)>,
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
    /// Customer, project and time, in columns.
    Day {
        dot: bool,
        customer: String,
        project: String,
        time: String,
    },
    Button {
        label: String,
        primary: bool,
    },
    Icon(char, Tone),
    Rule,
}

#[derive(Debug)]
pub struct Item {
    pub rect: Rect,
    pub hit: Option<Hit>,
    look: Look,
}

/// Sizes at a display scale (1.0 = 96 dpi).
#[derive(Clone, Copy)]
struct Px(f32);

impl Px {
    fn of(self, v: f32) -> i32 {
        (v * self.0).round() as i32
    }
}

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

/// The panel's rows for `model`, `width` pixels wide, at scale `s`.
pub fn layout(model: &Model, width: i32, s: f32) -> Vec<Item> {
    let p = Px(s);
    let (pad, line, small, heading, rule) =
        (p.of(8.0), p.of(30.0), p.of(24.0), p.of(24.0), p.of(9.0));
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
            let text = Rect::new(left, y, right - line - p.of(2.0), y + line);
            let look = Look::Row(Line {
                icon: Some((FLAG, Tone::Flag)),
                ..row(title.clone(), Tone::Normal)
            });
            push(text, look, Some(Hit::Todo(i)));
            push(
                square(y, line),
                Look::Icon(CHECK, Tone::Dim),
                Some(Hit::TodoDone(i)),
            );
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
        let gap = p.of(6.0);
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
            icon: Some((CLOCK, Tone::Accent)),
            ..row(away.clone(), Tone::Normal)
        });
        push(full(y, line), look, None);
        y += line + p.of(2.0);
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
        y += small + p.of(4.0);
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
                    customer: d.customer.clone(),
                    project: d.project.clone(),
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
                push(full(y, heading), Look::Heading(g.customer.clone()), None);
                y += heading;
                for t in &g.projects {
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
                    "Add customers and projects…"
                } else {
                    "Customers & projects…"
                };
                let look = Look::Row(Line {
                    icon: Some((ADD, Tone::Accent)),
                    ..row(label, Tone::Normal)
                });
                push(full(y, line), look, Some(Hit::Projects));
                y += line;
            }
        }
        None => {}
    }
    let _ = y;
    items
}

const UNAVAILABLE: &str = "The Documents folder isn't available.";

/// How tall the panel is with these rows.
pub fn height(items: &[Item], s: f32) -> i32 {
    items
        .iter()
        .map(|i| i.rect.bottom)
        .max()
        .map_or(0, |b| b + Px(s).of(8.0))
}

/// The row under `p` (panel pixels, scrolled by `scroll`) that does something.
pub fn hit_at(items: &[Item], p: Point, scroll: i32) -> Option<usize> {
    let p = (p.0, p.1 + scroll);
    items
        .iter()
        .position(|i| i.hit.is_some() && i.rect.contains(p))
}

pub fn rgb(c: Color32) -> [u8; 3] {
    let [r, g, b, _] = c.to_srgba_unmultiplied();
    [r, g, b]
}

fn alpha(c: Color32) -> f32 {
    c.to_srgba_unmultiplied()[3] as f32 / 255.0
}

/// Draw the panel: `size` pixels, its content scrolled up by `scroll`,
/// rounded at the corners in `radii` (TL, TR, BR, BL).
pub fn draw(
    items: &[Item],
    size: (i32, i32),
    scroll: i32,
    hover: Option<usize>,
    radii: [f32; 4],
    theme: &Theme,
    s: f32,
) -> Option<Surface> {
    let mut surface = Surface::new(size.0, size.1)?;
    let (w, h) = size;
    let clip = Rect::new(1, 1, w - 1, h - 1);
    let mut texts = Vec::new();
    {
        let mut c = surface.canvas();
        c.round_rect(Rect::new(0, 0, w, h), radii, rgb(theme.drawer_border), 1.0);
        c.round_rect(
            clip,
            radii.map(|r| (r - 1.0).max(0.0)),
            rgb(theme.drawer_bg),
            1.0,
        );
        for (i, item) in items.iter().enumerate() {
            let rect = item.rect.translate(0, -scroll);
            if rect.intersect(clip).is_empty() {
                continue;
            }
            let hovered = hover == Some(i);
            draw_item(&mut c, &mut texts, item, rect, hovered, theme, s, clip);
        }
    }
    surface.draw_text(&texts, Some(clip));
    Some(surface)
}

fn tone(theme: &Theme, t: Tone) -> [u8; 3] {
    rgb(match t {
        Tone::Normal => theme.text,
        Tone::Dim => theme.text_dim,
        Tone::Accent => theme.accent,
        Tone::Flag => theme.note_glyph,
        Tone::Project => theme.project,
    })
}

/// Puts a line of text between two x positions: the text, its font, colour
/// and alignment.
type PutLine<'a> = dyn FnMut(i32, i32, &str, Font, [u8; 3], Align) + 'a;

#[allow(clippy::too_many_arguments)]
fn draw_item(
    c: &mut Canvas,
    texts: &mut Vec<Text>,
    item: &Item,
    r: Rect,
    hovered: bool,
    theme: &Theme,
    s: f32,
    clip: Rect,
) {
    let p = Px(s);
    let (body, small) = (13.5 * s, 12.5 * s);
    let middle = (r.top + r.bottom) as f32 / 2.0;
    let radius = 5.0 * s;
    if hovered && !matches!(item.look, Look::Button { .. }) {
        c.round_rect(
            r.intersect(clip),
            [radius; 4],
            rgb(theme.drawer_item_hover),
            1.0,
        );
    }
    // A line of text between `x0` and `x1`.
    let mut line = |x0: i32, x1: i32, s: &str, font: Font, color, align| {
        if x1 > x0 && !s.is_empty() {
            texts.push(Text::new(s, font, color, Rect::new(x0, r.top, x1, r.bottom)).align(align));
        }
    };
    let icon =
        |c: &mut Canvas, line: &mut PutLine, x: i32, glyph: char, px: f32, color: [u8; 3]| {
            let size = p.of(24.0);
            if text::has_icons() {
                line(
                    x,
                    x + size,
                    &glyph.to_string(),
                    Font::icons(px),
                    color,
                    Align::Center,
                );
            } else {
                c.dot(((x + size / 2) as f32, middle), 3.0 * s, color);
            }
        };
    match &item.look {
        Look::Heading(name) => line(
            r.left + p.of(6.0),
            r.right - p.of(6.0),
            name,
            Font::ui(12.0 * s).semibold(),
            tone(theme, Tone::Dim),
            Align::Left,
        ),
        Look::Row(Line {
            dot,
            icon: glyph,
            text: label,
            tone: t,
            bold,
            right,
            right_tone,
        }) => {
            let mut x = r.left + p.of(8.0);
            if let Some(on) = dot {
                if *on {
                    c.dot(
                        (x as f32 + 3.5 * s, middle),
                        3.5 * s,
                        tone(theme, Tone::Accent),
                    );
                }
                x += p.of(14.0);
            }
            if let Some((g, gt)) = glyph {
                icon(c, &mut line, x - p.of(4.0), *g, 15.0 * s, tone(theme, *gt));
                x += p.of(22.0);
            }
            let end = r.right - p.of(8.0);
            let px = if *t == Tone::Dim && !*bold {
                small
            } else {
                body
            };
            let mut font = Font::ui(px);
            if *bold {
                font = font.semibold();
            }
            let right_w = match text::measure(right, font.tabular()) {
                Some((w, _)) if !right.is_empty() => {
                    line(
                        end - w - 2,
                        end,
                        right,
                        font.tabular(),
                        tone(theme, *right_tone),
                        Align::Right,
                    );
                    w + p.of(10.0)
                }
                _ => 0,
            };
            line(x, end - right_w, label, font, tone(theme, *t), Align::Left);
        }
        Look::Day {
            dot,
            customer,
            project,
            time,
        } => {
            let x = r.left + p.of(8.0);
            if *dot {
                c.dot(
                    (x as f32 + 3.5 * s, middle),
                    3.5 * s,
                    tone(theme, Tone::Accent),
                );
            }
            // The dot's place is kept either way: the names line up.
            let x = x + p.of(14.0);
            let end = r.right - p.of(8.0);
            let clock = Font::ui(body).semibold().tabular();
            let time_w = text::measure("00:00:00", clock).map_or(p.of(60.0), |(w, _)| w);
            line(
                end - time_w,
                end,
                time,
                clock,
                tone(theme, Tone::Accent),
                Align::Right,
            );
            let names = end - time_w - p.of(10.0) - x;
            let split = x + names * 2 / 5;
            line(
                x,
                split - p.of(6.0),
                customer,
                Font::ui(body).semibold(),
                tone(theme, Tone::Normal),
                Align::Left,
            );
            line(
                split,
                x + names,
                project,
                Font::ui(body),
                tone(theme, Tone::Project),
                Align::Left,
            );
        }
        Look::Button { label, primary } => {
            let (fill, opacity, color) = if *primary {
                let o = if hovered { 0.85 } else { 1.0 };
                (theme.accent, o, [255, 255, 255])
            } else {
                let f = if hovered {
                    theme.drawer_item_active
                } else {
                    theme.drawer_item_hover
                };
                (f, alpha(f).max(0.6), tone(theme, Tone::Normal))
            };
            c.round_rect(r.intersect(clip), [radius; 4], rgb(fill), opacity);
            line(
                r.left + p.of(4.0),
                r.right - p.of(4.0),
                label,
                Font::ui(small).semibold(),
                color,
                Align::Center,
            );
        }
        Look::Icon(glyph, t) => {
            let color = if hovered { Tone::Normal } else { *t };
            let x = (r.left + r.right) / 2 - p.of(12.0);
            icon(c, &mut line, x, *glyph, 14.0 * s, tone(theme, color));
        }
        Look::Rule => {
            let y = middle as i32;
            c.round_rect(
                Rect::new(r.left + p.of(4.0), y, r.right - p.of(4.0), y + 1),
                [0.0; 4],
                rgb(theme.drawer_border),
                1.0,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> Model {
        Model {
            todos: vec!["Fix Acme invoice".into(), "Call Bob".into()],
            more_todos: 1,
            away: Some("Away 0:47".into()),
            confirm: Some(Confirm {
                to: 9,
                to_label: "Contoso · Automation".into(),
                from_label: "Aardvark Oil · Customer Meetings".into(),
                from_time: "1:12:40".into(),
            }),
            view: Some(View::Picker),
            today: vec![
                DayRow {
                    customer: "Aardvark Oil".into(),
                    project: "Customer Meetings".into(),
                    time: "1:12:40".into(),
                    running: true,
                },
                DayRow {
                    customer: "Contoso".into(),
                    project: "Fuel Reconciliation".into(),
                    time: "2:03:11".into(),
                    running: false,
                },
            ],
            date: "Thursday, October 29".into(),
            total: "3:15:51".into(),
            groups: vec![
                Group {
                    customer: "Aardvark Oil".into(),
                    projects: vec![ProjectRow {
                        id: 7,
                        name: "Customer Meetings".into(),
                        today: "1:12:40".into(),
                        running: true,
                    }],
                },
                Group {
                    customer: "Contoso".into(),
                    projects: vec![ProjectRow {
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
        let items = layout(&m, 380, 1.0);
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
            Hit::Projects,
        ] {
            assert!(hits.contains(&h), "{h:?} missing");
        }
        assert_tidy(&items, 380);
        // Clicks find them, scrolled or not.
        let pick = items.iter().find(|i| i.hit == Some(Hit::Pick(9))).unwrap();
        let p = (pick.rect.left + 5, pick.rect.top + 5);
        let at = hit_at(&items, p, 0).unwrap();
        assert_eq!(items[at].hit, Some(Hit::Pick(9)));
        let at = hit_at(&items, (p.0, p.1 - 40), 40).unwrap();
        assert_eq!(items[at].hit, Some(Hit::Pick(9)));
        assert!(height(&items, 1.0) > pick.rect.bottom);

        // Today: the times (only to read) and the day's total (the timesheet).
        m.view = Some(View::Today);
        let items = layout(&m, 380, 1.0);
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
        let items = layout(&m, 380, 1.0);
        assert!(!items.iter().any(|i| matches!(i.hit, Some(Hit::Pick(_)))));
        assert!(items.iter().any(|i| i.hit == Some(Hit::Keep)));
        m.todos.clear();
        m.away = None;
        m.confirm = None;
        assert!(m.is_empty());
        assert!(layout(&m, 380, 1.0).is_empty());
        // Nothing to pick from yet: the way to set some up.
        let empty = Model {
            view: Some(View::Picker),
            available: true,
            ..Model::default()
        };
        let items = layout(&empty, 380, 1.0);
        assert!(items.iter().any(|i| i.hit == Some(Hit::Projects)));
    }

    #[test]
    fn it_scales_with_the_display() {
        let small = height(&layout(&model(), 380, 1.0), 1.0);
        let big = height(&layout(&model(), 760, 2.0), 2.0);
        assert!((big - 2 * small).abs() <= 8, "{small} {big}");
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
                    let width = (380.0 * s) as i32;
                    let items = layout(&m, width, s);
                    let h = height(&items, s);
                    let hover = items.iter().position(|i| i.hit == Some(Hit::Pick(9)));
                    let mut surface = draw(
                        &items,
                        (width, h),
                        0,
                        hover,
                        [0.0, 0.0, 9.0, 9.0],
                        &theme,
                        s,
                    )
                    .unwrap();
                    let path =
                        std::path::Path::new(&dir).join(format!("panel-{view}-{name}-{s}.png"));
                    surface.save_png(&path);
                }
            }
        }
    }

    #[test]
    fn it_draws() {
        let items = layout(&model(), 380, 1.0);
        let h = height(&items, 1.0);
        let mut surface = draw(
            &items,
            (380, h),
            0,
            Some(3),
            [0.0, 0.0, 9.0, 9.0],
            &Theme::dark(),
            1.0,
        )
        .unwrap();
        let px = surface.pixels();
        // Opaque inside, and some text drawn in the text colour's brightness.
        assert_eq!(px[(h as usize / 2) * 380 + 190] >> 24, 0xff);
        assert!(px.iter().any(|&p| (p >> 16) & 0xff > 0xc0));
    }
}
