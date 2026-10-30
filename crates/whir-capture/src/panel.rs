//! The dock's panel, beside its strip: the todo card, the "were you away?"
//! question, and the time drawer. Rows of fixed heights, so the layout is
//! plain arithmetic and the same for drawing and for clicks; drawn in
//! software like the strip.

use whir_core::theme::{Color32, Theme};

use crate::geom::{Point, Rect};
use crate::paint::{Canvas, Surface, icon_mask, label_mask};

const FACE: &str = "Segoe UI";
const FLAG: char = '\u{E7C1}';
const CHECK: char = '\u{E73E}';
const STOP: char = '\u{E71A}';
const EDIT: char = '\u{E70F}';
const ADD: char = '\u{E710}';
const CLOCK: char = '\u{E917}';

/// What a click on part of the panel does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    /// Start this timer (switching from the running one).
    Timer(u32),
    Stop,
    /// Write what the running timer's time is for.
    Note,
    /// The away time counts.
    Keep,
    /// It doesn't.
    Drop,
    /// Open the timesheet in whir.
    Timesheet,
    /// Set up timers in whir.
    AddTimer,
    /// Open that todo's note.
    Todo(usize),
    /// Mark it done.
    TodoDone(usize),
    /// The rest of the todos, in whir.
    MoreTodos,
}

pub struct TimerRow {
    pub id: u32,
    pub name: String,
    /// Time on it today, "1:05" (empty for none).
    pub today: String,
    pub running: bool,
}

pub struct Group {
    pub customer: String,
    pub timers: Vec<TimerRow>,
}

/// The running timer.
pub struct RunningRow {
    pub label: String,
    pub elapsed: String,
    pub note: String,
}

/// Everything the panel shows.
#[derive(Default)]
pub struct Model {
    pub todos: Vec<String>,
    pub more_todos: usize,
    /// "Away 0:47 (10:02 – 10:49)".
    pub away: Option<String>,
    /// The time drawer is open.
    pub expanded: bool,
    pub running: Option<RunningRow>,
    pub groups: Vec<Group>,
    /// "Today 4:03 · Week 21:30".
    pub totals: String,
    /// There is a timesheet folder to keep time in.
    pub available: bool,
}

impl Model {
    /// Nothing to show: no panel.
    pub fn is_empty(&self) -> bool {
        self.todos.is_empty() && self.away.is_none() && !self.expanded
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tone {
    Normal,
    Dim,
    Accent,
    Flag,
}

#[derive(Debug)]
enum Look {
    Heading(String),
    Row {
        dot: bool,
        icon: Option<(char, Tone)>,
        text: String,
        tone: Tone,
        bold: bool,
        right: String,
    },
    Note {
        text: String,
        placeholder: bool,
    },
    Button(String),
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

/// The panel's rows for `model`, `width` pixels wide, at scale `s`.
pub fn layout(model: &Model, width: i32, s: f32) -> Vec<Item> {
    let p = Px(s);
    let (pad, row, small_row, heading, rule) =
        (p.of(8.0), p.of(30.0), p.of(24.0), p.of(24.0), p.of(9.0));
    let (left, right) = (pad, width - pad);
    let mut items = Vec::new();
    let mut y = pad;
    let push = |items: &mut Vec<Item>, rect: Rect, look: Look, hit: Option<Hit>| {
        items.push(Item { rect, hit, look });
    };
    let full = |y: i32, h: i32| Rect::new(left, y, right, y + h);
    let square = |y: i32, h: i32| Rect::new(right - h, y, right, y + h);

    if !model.todos.is_empty() {
        push(
            &mut items,
            full(y, heading),
            Look::Heading("Todo".into()),
            None,
        );
        y += heading;
        for (i, title) in model.todos.iter().enumerate() {
            let text = Rect::new(left, y, right - row - p.of(2.0), y + row);
            let look = Look::Row {
                dot: false,
                icon: Some((FLAG, Tone::Flag)),
                text: title.clone(),
                tone: Tone::Normal,
                bold: false,
                right: String::new(),
            };
            push(&mut items, text, look, Some(Hit::Todo(i)));
            push(
                &mut items,
                square(y, row),
                Look::Icon(CHECK, Tone::Dim),
                Some(Hit::TodoDone(i)),
            );
            y += row;
        }
        if model.more_todos > 0 {
            let look = Look::Row {
                dot: false,
                icon: None,
                text: format!("{} more in whir", model.more_todos),
                tone: Tone::Dim,
                bold: false,
                right: String::new(),
            };
            push(&mut items, full(y, small_row), look, Some(Hit::MoreTodos));
            y += small_row;
        }
    }

    if let Some(away) = &model.away {
        if !items.is_empty() {
            push(&mut items, full(y, rule), Look::Rule, None);
            y += rule;
        }
        let look = Look::Row {
            dot: false,
            icon: Some((CLOCK, Tone::Accent)),
            text: away.clone(),
            tone: Tone::Normal,
            bold: false,
            right: String::new(),
        };
        push(&mut items, full(y, row), look, None);
        y += row + p.of(2.0);
        let gap = p.of(6.0);
        let half = (right - left - gap) / 2;
        let keep = Rect::new(left, y, left + half, y + row);
        let drop = Rect::new(right - half, y, right, y + row);
        push(
            &mut items,
            keep,
            Look::Button("Keep it".into()),
            Some(Hit::Keep),
        );
        push(
            &mut items,
            drop,
            Look::Button("Drop it".into()),
            Some(Hit::Drop),
        );
        y += row;
    }

    if model.expanded {
        if !items.is_empty() {
            push(&mut items, full(y, rule), Look::Rule, None);
            y += rule;
        }
        match &model.running {
            Some(r) => {
                let look = Look::Row {
                    dot: true,
                    icon: None,
                    text: r.label.clone(),
                    tone: Tone::Accent,
                    bold: true,
                    right: r.elapsed.clone(),
                };
                push(
                    &mut items,
                    Rect::new(left, y, right - row - p.of(2.0), y + row),
                    look,
                    None,
                );
                push(
                    &mut items,
                    square(y, row),
                    Look::Icon(STOP, Tone::Normal),
                    Some(Hit::Stop),
                );
                y += row;
                let look = Look::Note {
                    text: if r.note.is_empty() {
                        "What are you working on?".into()
                    } else {
                        r.note.clone()
                    },
                    placeholder: r.note.is_empty(),
                };
                push(&mut items, full(y, small_row), look, Some(Hit::Note));
                y += small_row;
            }
            None => {
                let look = Look::Row {
                    dot: false,
                    icon: None,
                    text: "No timer running".into(),
                    tone: Tone::Dim,
                    bold: false,
                    right: String::new(),
                };
                push(&mut items, full(y, row), look, None);
                y += row;
            }
        }
        push(&mut items, full(y, rule), Look::Rule, None);
        y += rule;

        if !model.available {
            let look = Look::Row {
                dot: false,
                icon: None,
                text: "The Documents folder isn't available.".into(),
                tone: Tone::Dim,
                bold: false,
                right: String::new(),
            };
            push(&mut items, full(y, row), look, None);
            y += row;
        } else if model.groups.is_empty() {
            let look = Look::Row {
                dot: false,
                icon: Some((ADD, Tone::Accent)),
                text: "Add customers and projects…".into(),
                tone: Tone::Normal,
                bold: false,
                right: String::new(),
            };
            push(&mut items, full(y, row), look, Some(Hit::AddTimer));
            y += row;
        }
        for g in &model.groups {
            push(
                &mut items,
                full(y, heading),
                Look::Heading(g.customer.clone()),
                None,
            );
            y += heading;
            for t in &g.timers {
                let look = Look::Row {
                    dot: t.running,
                    icon: None,
                    text: t.name.clone(),
                    tone: if t.running {
                        Tone::Accent
                    } else {
                        Tone::Normal
                    },
                    bold: false,
                    right: t.today.clone(),
                };
                push(&mut items, full(y, row), look, Some(Hit::Timer(t.id)));
                y += row;
            }
        }

        push(&mut items, full(y, rule), Look::Rule, None);
        y += rule;
        let button = p.of(92.0);
        let look = Look::Row {
            dot: false,
            icon: None,
            text: model.totals.clone(),
            tone: Tone::Dim,
            bold: false,
            right: String::new(),
        };
        let add = square(y, row);
        let sheet = Rect::new(
            add.left - p.of(4.0) - button,
            y,
            add.left - p.of(4.0),
            y + row,
        );
        push(
            &mut items,
            Rect::new(left, y, sheet.left - p.of(4.0), y + row),
            look,
            None,
        );
        push(
            &mut items,
            sheet,
            Look::Button("Timesheet".into()),
            Some(Hit::Timesheet),
        );
        push(
            &mut items,
            add,
            Look::Icon(ADD, Tone::Normal),
            Some(Hit::AddTimer),
        );
        y += row;
    }
    let _ = y;
    items
}

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

fn unmultiplied(c: Color32) -> ([u8; 3], f32) {
    let [r, g, b, a] = c.to_srgba_unmultiplied();
    ([r, g, b], a as f32 / 255.0)
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
    let mut c = surface.canvas();
    let (bg, _) = unmultiplied(theme.drawer_bg);
    let (border, _) = unmultiplied(theme.drawer_border);
    let (w, h) = size;
    c.round_rect(Rect::new(0, 0, w, h), radii, border, 1.0);
    c.round_rect(
        Rect::new(1, 1, w - 1, h - 1),
        radii.map(|r| (r - 1.0).max(0.0)),
        bg,
        1.0,
    );
    // The content, clipped to inside the border.
    let clip = Rect::new(1, 1, w - 1, h - 1);
    for (i, item) in items.iter().enumerate() {
        let rect = item.rect.translate(0, -scroll);
        if rect.intersect(clip).is_empty() {
            continue;
        }
        draw_item(&mut c, item, rect, hover == Some(i), theme, s, clip);
    }
    Some(surface)
}

fn tone(theme: &Theme, t: Tone) -> [u8; 3] {
    let c = match t {
        Tone::Normal => theme.text,
        Tone::Dim => theme.text_dim,
        Tone::Accent => theme.accent,
        Tone::Flag => theme.note_glyph,
    };
    unmultiplied(c).0
}

fn draw_item(
    c: &mut Canvas,
    item: &Item,
    r: Rect,
    hovered: bool,
    theme: &Theme,
    s: f32,
    clip: Rect,
) {
    let p = Px(s);
    let (body, small) = (p.of(14.0), p.of(12.5));
    let middle = (r.top + r.bottom) / 2;
    let radius = 5.0 * s;
    if hovered && !matches!(item.look, Look::Button(_)) {
        let (hover, _) = unmultiplied(theme.drawer_item_hover);
        c.round_rect(r.intersect(clip), [radius; 4], hover, 1.0);
    }
    // Text, its middle on `middle`, clipped by the panel.
    let text = |c: &mut Canvas, x: i32, s: &str, px: i32, bold: bool, max: i32, color| {
        if max <= 0 || s.is_empty() {
            return 0;
        }
        match label_mask(s, FACE, px, bold, max) {
            Some(m) => {
                let y = middle - m.height / 2;
                if y >= clip.top && y + m.height <= clip.bottom {
                    c.mask_at((x, y), &m, color);
                }
                m.width
            }
            None => 0,
        }
    };
    match &item.look {
        Look::Heading(s) => {
            text(
                c,
                r.left + p.of(6.0),
                s,
                p.of(12.0),
                true,
                r.width() - p.of(12.0),
                tone(theme, Tone::Dim),
            );
        }
        Look::Row {
            dot,
            icon,
            text: label,
            tone: t,
            bold,
            right,
        } => {
            let mut x = r.left + p.of(8.0);
            if *dot {
                c.dot(
                    (x as f32 + 3.5 * s, middle as f32),
                    3.5 * s,
                    tone(theme, Tone::Accent),
                );
                x += p.of(14.0);
            }
            if let Some((glyph, gt)) = icon {
                if let Some(m) = icon_mask(*glyph, p.of(15.0)) {
                    c.mask((x + p.of(8.0), middle), &m, tone(theme, *gt));
                }
                x += p.of(24.0);
            }
            let end = r.right - p.of(8.0);
            let right_w = match label_mask(right, FACE, small, false, r.width()) {
                Some(m) if !right.is_empty() => {
                    let at = (end - m.width, middle - m.height / 2);
                    c.mask_at(at, &m, tone(theme, Tone::Dim));
                    m.width + p.of(8.0)
                }
                _ => 0,
            };
            let px = if *t == Tone::Dim { small } else { body };
            text(c, x, label, px, *bold, end - right_w - x, tone(theme, *t));
        }
        Look::Note {
            text: note,
            placeholder,
        } => {
            let x = r.left + p.of(8.0);
            if let Some(m) = icon_mask(EDIT, p.of(12.0)) {
                c.mask((x + p.of(6.0), middle), &m, tone(theme, Tone::Dim));
            }
            let t = if *placeholder {
                Tone::Dim
            } else {
                Tone::Normal
            };
            text(
                c,
                x + p.of(20.0),
                note,
                small,
                false,
                r.right - x - p.of(28.0),
                tone(theme, t),
            );
        }
        Look::Button(label) => {
            let fill = if hovered {
                theme.drawer_item_active
            } else {
                theme.drawer_item_hover
            };
            let (fill, alpha) = unmultiplied(fill);
            c.round_rect(r.intersect(clip), [radius; 4], fill, alpha.max(0.6));
            if let Some(m) = label_mask(label, FACE, small, true, r.width() - p.of(8.0)) {
                let at = ((r.left + r.right - m.width) / 2, middle - m.height / 2);
                c.mask_at(at, &m, tone(theme, Tone::Normal));
            }
        }
        Look::Icon(glyph, t) => {
            if let Some(m) = icon_mask(*glyph, p.of(14.0)) {
                let color = if hovered { Tone::Normal } else { *t };
                c.mask(((r.left + r.right) / 2, middle), &m, tone(theme, color));
            }
        }
        Look::Rule => {
            let (line, _) = unmultiplied(theme.drawer_border);
            c.round_rect(
                Rect::new(r.left + p.of(4.0), middle, r.right - p.of(4.0), middle + 1),
                [0.0; 4],
                line,
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
            expanded: true,
            running: Some(RunningRow {
                label: "Acme · Onboarding".into(),
                elapsed: "1:23".into(),
                note: String::new(),
            }),
            groups: vec![Group {
                customer: "Acme".into(),
                timers: vec![
                    TimerRow {
                        id: 7,
                        name: "Onboarding".into(),
                        today: "1:23".into(),
                        running: true,
                    },
                    TimerRow {
                        id: 9,
                        name: "Support".into(),
                        today: String::new(),
                        running: false,
                    },
                ],
            }],
            totals: "Today 1:23 · Week 9:10".into(),
            available: true,
        }
    }

    #[test]
    fn every_action_has_a_place_and_rows_dont_overlap() {
        let items = layout(&model(), 300, 1.0);
        let hits: Vec<Hit> = items.iter().filter_map(|i| i.hit).collect();
        for h in [
            Hit::Todo(0),
            Hit::TodoDone(1),
            Hit::MoreTodos,
            Hit::Keep,
            Hit::Drop,
            Hit::Stop,
            Hit::Note,
            Hit::Timer(7),
            Hit::Timer(9),
            Hit::Timesheet,
            Hit::AddTimer,
        ] {
            assert!(hits.contains(&h), "{h:?} missing");
        }
        let clickable: Vec<&Item> = items.iter().filter(|i| i.hit.is_some()).collect();
        for (i, a) in clickable.iter().enumerate() {
            assert!(a.rect.left >= 0 && a.rect.right <= 300, "{a:?}");
            for b in &clickable[i + 1..] {
                assert!(a.rect.intersect(b.rect).is_empty(), "{a:?} overlaps {b:?}");
            }
        }
        // Clicks find them, scrolled or not.
        let support = items.iter().find(|i| i.hit == Some(Hit::Timer(9))).unwrap();
        let p = (support.rect.left + 5, support.rect.top + 5);
        let at = hit_at(&items, p, 0).unwrap();
        assert_eq!(items[at].hit, Some(Hit::Timer(9)));
        let at = hit_at(&items, (p.0, p.1 - 40), 40).unwrap();
        assert_eq!(items[at].hit, Some(Hit::Timer(9)));
        assert!(height(&items, 1.0) > support.rect.bottom);
    }

    #[test]
    fn collapsed_it_shows_only_what_must_be_seen() {
        let mut m = model();
        m.expanded = false;
        let items = layout(&m, 300, 1.0);
        assert!(
            items
                .iter()
                .all(|i| !matches!(i.hit, Some(Hit::Timer(_)) | Some(Hit::Stop)))
        );
        assert!(items.iter().any(|i| i.hit == Some(Hit::Keep)));
        m.todos.clear();
        m.away = None;
        assert!(m.is_empty());
        assert!(layout(&m, 300, 1.0).is_empty());
    }

    #[test]
    fn it_scales_with_the_display() {
        let small = height(&layout(&model(), 300, 1.0), 1.0);
        let big = height(&layout(&model(), 600, 2.0), 2.0);
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
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(
                None,
                windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
            );
        }
        for (name, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
            for s in [1.0, 1.5] {
                let width = (300.0 * s) as i32;
                let items = layout(&model(), width, s);
                let h = height(&items, s);
                let hover = items.iter().position(|i| i.hit == Some(Hit::Timer(9)));
                let mut surface = draw(
                    &items,
                    (width, h),
                    0,
                    hover,
                    [0.0, 9.0, 9.0, 9.0],
                    &theme,
                    s,
                )
                .unwrap();
                let bytes: Vec<u8> = surface
                    .pixels()
                    .iter()
                    .flat_map(|p| p.to_le_bytes())
                    .collect();
                let png = whir_image::encode_png(
                    &bytes,
                    width as u32,
                    h as u32,
                    width as usize * 4,
                    whir_image::PixelLayout::PremultipliedBgra,
                )
                .unwrap();
                let path = std::path::Path::new(&dir).join(format!("panel-{name}-{s}.png"));
                std::fs::write(path, png).unwrap();
            }
        }
    }

    #[test]
    fn it_draws() {
        let items = layout(&model(), 300, 1.0);
        let h = height(&items, 1.0);
        let mut surface = draw(
            &items,
            (300, h),
            0,
            Some(3),
            [0.0, 9.0, 9.0, 0.0],
            &Theme::dark(),
            1.0,
        )
        .unwrap();
        let px = surface.pixels();
        // Opaque inside, and some text drawn in the text colour's brightness.
        assert_eq!(px[(h as usize / 2) * 300 + 150] >> 24, 0xff);
        assert!(px.iter().any(|&p| (p >> 16) & 0xff > 0xc0));
    }
}
