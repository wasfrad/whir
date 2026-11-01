//! The dock's bar, in egui: a toolbar like the window's, on a floating strip
//! that lies against the edge of the screen. Where things go comes from
//! [`crate::place`], which also positions the window around them.

use egui::{
    Align2, Color32, CornerRadius, FontFamily, FontId, Id, Rect as Area, Response, Sense, Stroke,
    StrokeKind, Ui, pos2, vec2,
};
use whir_core::icons::{self, Icon};
use whir_core::theme::{self, Theme};
use whir_core::{kit, toolbar};

use crate::clock::Strip;
use crate::geom::Rect;
use crate::place::{Edge, Layout, Slot};

/// The grip's handle, in points.
const HANDLE: (f32, f32) = (28.0, 3.0);
/// The time, and a number where an icon usually goes.
const FIGURES: f32 = 14.0;

/// What the bar shows, besides its shape.
pub struct Bar {
    pub edge: Edge,
    pub layout: Layout,
    pub strip: Strip,
    /// Seconds left before a delayed capture.
    pub countdown: Option<u32>,
    pub delay: u32,
    /// Today's times are open (the ⌄ is lit).
    pub today: bool,
    /// The panel is below the bar (the ⌄ points at it).
    pub below: bool,
}

pub fn semibold(size: f32) -> FontId {
    FontId::new(size, theme::family(theme::SEMIBOLD))
}

pub fn regular(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

/// The fill and hairline of what floats on the desktop: the bar and its panel,
/// as the window's popups are.
pub fn sheet(theme: &Theme) -> (Color32, Stroke) {
    (theme.surface_raised, Stroke::new(1.0, theme.border_strong))
}

/// Pixels of the surface, in egui's points.
pub fn area(r: Rect, scale: f32) -> Area {
    Area::from_min_max(
        pos2(r.left as f32 / scale, r.top as f32 / scale),
        pos2(r.right as f32 / scale, r.bottom as f32 / scale),
    )
}

/// Where the project is on the bar; with none to show (no time either),
/// what to do for one takes the time's place too.
fn project_rect(l: &Layout, edge: Edge, strip: &Strip) -> Rect {
    let r = l.slot_rect(edge, Slot::Project);
    match strip.project {
        Some(_) => r,
        None => Rect::new(l.slot_rect(edge, Slot::Time).left, r.top, r.right, r.bottom),
    }
}

/// A place on the bar that can be clicked, with the toolbar's fade behind it.
fn button(ui: &mut Ui, at: Area, slot: Slot, on: bool, theme: &Theme) -> Response {
    let response = ui.interact(at, Id::new(("whir.dock.bar", slot as u8)), Sense::CLICK);
    if let Some(fill) = toolbar::fill_of(ui, &response, on, theme) {
        let radius = theme.metrics.control_radius;
        ui.painter().rect_filled(at, radius, fill);
    }
    response
}

/// Rounded on the side away from the screen, and running past the window on the
/// side against it, so there's no hairline along the screen's edge.
pub fn against_edge(whole: Area, edge: Edge, radius: u8) -> (Area, CornerRadius) {
    let r = radius;
    let beyond = 4.0;
    let mut shape = whole;
    let corners = match edge {
        Edge::Left => {
            shape.min.x -= beyond;
            CornerRadius {
                ne: r,
                se: r,
                ..CornerRadius::ZERO
            }
        }
        Edge::Right => {
            shape.max.x += beyond;
            CornerRadius {
                nw: r,
                sw: r,
                ..CornerRadius::ZERO
            }
        }
        Edge::Top => {
            shape.min.y -= beyond;
            CornerRadius {
                sw: r,
                se: r,
                ..CornerRadius::ZERO
            }
        }
        Edge::Bottom => {
            shape.max.y += beyond;
            CornerRadius {
                nw: r,
                ne: r,
                ..CornerRadius::ZERO
            }
        }
    };
    (shape, corners)
}

/// The whole dock: the bar and its grip. Returns what was clicked.
pub fn show(ui: &mut Ui, bar: &Bar, theme: &Theme) -> Option<Slot> {
    let scale = ui.ctx().pixels_per_point();
    let at = |r: Rect| area(r, scale);
    let (edge, l, strip) = (bar.edge, bar.layout, &bar.strip);
    let m = theme.metrics;
    let (w, h) = l.size(edge);
    let painter = ui.painter().clone();

    let (fill, line) = sheet(theme);
    let (shape, corners) = against_edge(at(Rect::new(0, 0, w, h)), edge, m.corner_radius as u8);
    painter.rect(shape, corners, fill, line, StrokeKind::Inside);

    // The grip: a handle, along the bar on every edge but the sides.
    let grip = at(l.grip_rect(edge));
    let size = if edge.is_side() {
        vec2(HANDLE.1, HANDLE.0)
    } else {
        vec2(HANDLE.0, HANDLE.1)
    };
    let handle = Area::from_center_size(grip.center(), size);
    painter.rect_filled(handle, HANDLE.1 / 2.0, theme.drawer_grip);

    let divider = at(l.divider(edge));
    let x = painter.round_to_pixel_center(divider.left());
    painter.line_segment(
        [pos2(x, divider.top()), pos2(x, divider.bottom())],
        Stroke::new(1.0, theme.separator),
    );

    let mut clicked = None;
    let mut press = |slot: Slot, response: Response| {
        if response.clicked() {
            clicked = Some(slot);
        }
    };
    let figures = semibold(FIGURES);
    let icon = |r: Area, icon: Icon, size: f32, color: Color32| {
        icons::paint(&painter, r.center(), icon, size, color);
    };
    let word = |r: Area, text: String, color: Color32| {
        painter.text(
            r.center(),
            Align2::CENTER_CENTER,
            text,
            figures.clone(),
            color,
        );
    };

    // Counting down: the seconds left, where the camera was.
    let r = at(l.slot_rect(edge, Slot::Capture));
    let response = button(ui, r, Slot::Capture, false, theme);
    match bar.countdown {
        Some(n) => word(r, n.to_string(), theme.accent),
        None => icon(r, Icon::Camera, m.icon_size, theme.text),
    }
    press(Slot::Capture, response);

    let r = at(l.slot_rect(edge, Slot::Delay));
    let response = button(ui, r, Slot::Delay, bar.delay > 0, theme);
    match bar.delay {
        0 => {
            let color = if response.hovered() {
                theme.text
            } else {
                theme.text_dim
            };
            icon(r, Icon::Stopwatch, m.icon_size, color);
        }
        secs => word(r, format!("{secs}s"), theme.accent),
    }
    press(Slot::Delay, response);

    let r = at(l.slot_rect(edge, Slot::Play));
    // Running: a chip around the pause and the time.
    if strip.playing {
        let chip = r.union(at(l.slot_rect(edge, Slot::Time)));
        painter.rect_filled(chip, m.control_radius, theme.drawer_item_active);
    }
    let response = button(ui, r, Slot::Play, false, theme);
    let (glyph, color) = match (&strip.project, strip.playing) {
        (_, true) => (Icon::Pause, theme.accent),
        (Some(_), false) => (Icon::Play, theme.text),
        (None, false) => (Icon::Play, theme.text_dim),
    };
    icon(r, glyph, m.icon_size, color);
    press(Slot::Play, response);

    let r = at(l.slot_rect(edge, Slot::Time));
    if !strip.time.is_empty() {
        let color = if strip.playing {
            theme.accent
        } else {
            theme.text_dim
        };
        kit::tabular(
            &painter,
            r.center(),
            Align2::CENTER_CENTER,
            &strip.time,
            &figures,
            color,
        );
    }

    // The customer over the project, or what to do for one.
    let r = at(project_rect(&l, edge, strip));
    let response = button(ui, r, Slot::Project, false, theme);
    let inset = 6.0;
    let width = r.width() - inset * 2.0;
    match &strip.project {
        Some((customer, project)) => {
            let top = kit::one_line(&painter, customer, semibold(m.body_text_size), width);
            let under = kit::one_line(&painter, project, regular(m.small_text_size), width);
            let height = top.size().y + under.size().y;
            let y = r.center().y - height / 2.0;
            let x = r.left() + inset;
            let below = y + top.size().y;
            painter.galley(pos2(x, y), top, theme.text);
            painter.galley(pos2(x, below), under, theme.project);
        }
        None => {
            let hint = kit::one_line(&painter, strip.hint, regular(m.body_text_size), width);
            let y = r.center().y - hint.size().y / 2.0;
            painter.galley(pos2(r.left() + inset, y), hint, theme.text_dim);
        }
    }
    press(Slot::Project, response);

    // The ⌄ points at the panel: down to open it below, up to close.
    let r = at(l.slot_rect(edge, Slot::More));
    let response = button(ui, r, Slot::More, bar.today, theme);
    let down = bar.below != bar.today;
    let glyph = if down {
        Icon::ChevronDown
    } else {
        Icon::ChevronUp
    };
    let color = if bar.today {
        theme.accent
    } else {
        theme.text_dim
    };
    icon(r, glyph, 12.0, color);
    press(Slot::More, response);

    clicked
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::stage::Stage;

    fn strip(playing: bool) -> Strip {
        Strip {
            project: Some(("Aardvark Oil".into(), "Customer Meetings".into())),
            playing,
            time: "1:12:40".into(),
            hint: "Choose a project",
        }
    }

    fn none() -> Strip {
        Strip {
            project: None,
            playing: false,
            time: String::new(),
            hint: "Choose a project",
        }
    }

    fn bar(edge: Edge, dpi: u32, strip: Strip) -> Bar {
        Bar {
            edge,
            layout: crate::place::layout(dpi),
            strip,
            countdown: None,
            delay: 0,
            today: false,
            below: edge != Edge::Bottom,
        }
    }

    fn frame(stage: &mut Stage, bar: &Bar, theme: &Theme) -> Option<Slot> {
        let mut clicked = None;
        stage.run(bar.layout.size(bar.edge), |ui| {
            clicked = show(ui, bar, theme).or(clicked);
        });
        clicked
    }

    #[test]
    fn the_bar_draws_on_every_edge() {
        let theme = Theme::light();
        for edge in Edge::ALL {
            let b = bar(edge, 96, strip(true));
            let mut stage = Stage::new(&theme, 1.0);
            frame(&mut stage, &b, &theme);
            let (w, h) = b.layout.size(edge);
            let at = |x: i32, y: i32| {
                let s = stage.surface().unwrap();
                assert_eq!((s.width, s.height), (w, h));
                s.pixel(x, y)
            };
            // Opaque in the middle of the bar; text drawn on it (darker than
            // the light background somewhere in the project's slot).
            let r = b.layout.bar_rect(edge);
            assert_eq!(
                at((r.left + r.right) / 2, r.top + 2) >> 24,
                0xff,
                "{edge:?}"
            );
            let p = b.layout.slot_rect(edge, Slot::Project);
            let inked =
                (p.top..p.bottom).any(|y| (p.left..p.right).any(|x| (at(x, y) >> 8) & 0xff < 0x80));
            assert!(inked, "{edge:?}: no project name");
        }
    }

    #[test]
    fn its_outer_corners_are_round_and_the_screens_side_is_square() {
        let theme = Theme::dark();
        let b = bar(Edge::Top, 96, strip(false));
        let mut stage = Stage::new(&theme, 1.0);
        frame(&mut stage, &b, &theme);
        let (w, h) = b.layout.size(Edge::Top);
        let s = stage.surface().unwrap();
        assert_eq!(s.pixel(0, h - 1) >> 24, 0, "away from the screen's edge");
        assert_eq!(s.pixel(w - 1, h - 1) >> 24, 0);
        assert_eq!(s.pixel(0, 0) >> 24, 0xff, "against it");
        assert_eq!(s.pixel(w - 1, 0) >> 24, 0xff);
    }

    #[test]
    fn every_slot_answers_a_click_at_every_scale() {
        let theme = Theme::dark();
        for dpi in [96, 120, 144, 192] {
            let b = bar(Edge::Top, dpi, strip(true));
            let mut stage = Stage::new(&theme, dpi as f32 / 96.0);
            frame(&mut stage, &b, &theme);
            for slot in Slot::ALL {
                let r = b.layout.slot_rect(Edge::Top, slot);
                let at = ((r.left + r.right) / 2, (r.top + r.bottom) / 2);
                stage.pointer_moved(at);
                frame(&mut stage, &b, &theme);
                stage.button(at, true);
                assert_eq!(frame(&mut stage, &b, &theme), None);
                stage.button(at, false);
                let clicked = frame(&mut stage, &b, &theme);
                // The time is only there to read.
                let expected = (slot != Slot::Time).then_some(slot);
                assert_eq!(clicked, expected, "{slot:?} at {dpi}");
            }
        }
    }

    #[test]
    fn with_no_project_the_time_is_a_way_to_choose_one() {
        let theme = Theme::dark();
        let b = bar(Edge::Top, 96, none());
        let mut stage = Stage::new(&theme, 1.0);
        frame(&mut stage, &b, &theme);
        let r = b.layout.slot_rect(Edge::Top, Slot::Time);
        let at = ((r.left + r.right) / 2, (r.top + r.bottom) / 2);
        stage.pointer_moved(at);
        frame(&mut stage, &b, &theme);
        stage.button(at, true);
        frame(&mut stage, &b, &theme);
        stage.button(at, false);
        assert_eq!(frame(&mut stage, &b, &theme), Some(Slot::Project));
    }

    /// `cargo test -p whir-capture -- --ignored preview` writes the bar, in
    /// both themes, to PNGs in `WHIR_PREVIEW_DIR` (to look at).
    #[test]
    #[ignore = "writes pictures to look at"]
    fn preview_bar() {
        let Some(dir) = std::env::var_os("WHIR_PREVIEW_DIR") else {
            return;
        };
        for (name, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
            for dpi in [96, 144] {
                for (what, edge, strip) in [
                    ("running", Edge::Top, strip(true)),
                    ("paused", Edge::Top, strip(false)),
                    ("none", Edge::Top, none()),
                    ("left", Edge::Left, strip(true)),
                ] {
                    let mut b = bar(edge, dpi, strip);
                    b.delay = if what == "paused" { 3 } else { 0 };
                    let mut stage = Stage::new(&theme, dpi as f32 / 96.0);
                    frame(&mut stage, &b, &theme);
                    if what == "paused" {
                        let r = b.layout.slot_rect(edge, Slot::Project);
                        stage.pointer_moved(((r.left + r.right) / 2, (r.top + r.bottom) / 2));
                        frame(&mut stage, &b, &theme);
                        std::thread::sleep(std::time::Duration::from_millis(120));
                        frame(&mut stage, &b, &theme);
                        frame(&mut stage, &b, &theme);
                    }
                    let path =
                        std::path::Path::new(&dir).join(format!("bar-{what}-{name}-{dpi}.png"));
                    stage.save_png(&path);
                }
            }
        }
    }

    /// The window's popup shadow, for pictures of the whole dock.
    #[test]
    #[ignore = "writes pictures to look at"]
    fn preview_shadow() {
        let Some(dir) = std::env::var_os("WHIR_PREVIEW_DIR") else {
            return;
        };
        for (name, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
            let look = theme.popup_shadow();
            let (size, shape) = ((419, 92), Rect::new(22, 0, 397, 48));
            let s = crate::paint::Shadow {
                shape,
                radius: 8.0,
                drop: look.offset[1] as i32,
                blur: look.blur as f32,
                strength: look.color.a() as f32 / 255.0,
            };
            let path = std::path::Path::new(&dir).join(format!("shadow-bar-{name}.png"));
            crate::paint::shadow(size, s).unwrap().save_png(&path);
            let (size, shape) = ((419, 520), Rect::new(22, 22, 397, 476));
            let s = crate::paint::Shadow { shape, ..s };
            let path = std::path::Path::new(&dir).join(format!("shadow-panel-{name}.png"));
            crate::paint::shadow(size, s).unwrap().save_png(&path);
        }
    }
}
