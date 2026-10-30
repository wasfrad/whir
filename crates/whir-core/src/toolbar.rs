//! Widgets for the toolbar strip between the tabs and the document: a tool
//! button that can also be held for a menu, and the menu itself.

use std::time::Duration;

use egui::{Color32, CursorIcon, Id, Key, Modifiers, Pos2, Rect, Sense, Shape, Stroke, pos2, vec2};

use crate::theme::Theme;

/// Seconds a tool button is held before its menu opens.
const HOLD: f64 = 0.35;
const BUTTON: egui::Vec2 = vec2(30.0, 24.0);
const SWATCH: egui::Vec2 = vec2(24.0, 24.0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Glyph {
    Highlighter,
    Text,
    Camera,
    Gear,
    Flag,
}

/// What happened to a [`tool_button`] this frame.
pub struct Pressed {
    /// A plain click.
    pub clicked: bool,
    /// Held down, or right-clicked: open the button's menu.
    pub menu: bool,
    pub response: egui::Response,
}

/// A toolbar button. `on` tints it (a tool that is active); `ink` draws a bar
/// of that colour under the glyph; `has_menu` marks its corner and reports a
/// long press or right-click as [`Pressed::menu`].
pub fn tool_button(
    ui: &mut egui::Ui,
    glyph: Glyph,
    on: bool,
    ink: Option<Color32>,
    has_menu: bool,
    theme: &Theme,
) -> Pressed {
    let (rect, response) = ui.allocate_exact_size(BUTTON, Sense::CLICK);

    // A long press opens the menu once; the click that ends it is swallowed.
    let fired_id = response.id.with("held");
    let mut fired = ui.data(|d| d.get_temp::<bool>(fired_id)).unwrap_or(false);
    let mut menu = has_menu && response.secondary_clicked();
    let (now, pressed_at, primary_down) = ui.input(|i| {
        (
            i.time,
            i.pointer.press_start_time(),
            i.pointer.primary_down(),
        )
    });
    if has_menu
        && !fired
        && primary_down
        && response.is_pointer_button_down_on()
        && let Some(t0) = pressed_at
    {
        let held = now - t0;
        if held >= HOLD {
            fired = true;
            menu = true;
            ui.data_mut(|d| d.insert_temp(fired_id, true));
        } else {
            ui.ctx()
                .request_repaint_after(Duration::from_secs_f64(HOLD - held));
        }
    }
    let clicked = response.clicked() && !fired;
    if fired && !primary_down {
        ui.data_mut(|d| d.remove::<bool>(fired_id));
    }

    let painter = ui.painter();
    let fg = if on || response.hovered() {
        theme.text
    } else {
        theme.text_dim
    };
    if on {
        painter.rect_filled(rect, 4.0, theme.accent.gamma_multiply(0.25));
        painter.rect_stroke(
            rect,
            4.0,
            Stroke::new(1.0, theme.accent),
            egui::StrokeKind::Inside,
        );
    } else if response.is_pointer_button_down_on() {
        painter.rect_filled(rect, 4.0, theme.caption_pressed);
    } else if response.hovered() {
        painter.rect_filled(rect, 4.0, theme.tab_hover_bg);
    }
    if response.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let lift = if ink.is_some() { 2.5 } else { 0.0 };
    paint_glyph(painter, rect.center() - vec2(0.0, lift), glyph, fg);
    if let Some(ink) = ink {
        let y = rect.center().y + 7.5;
        painter.line_segment(
            [
                pos2(rect.center().x - 7.0, y),
                pos2(rect.center().x + 7.0, y),
            ],
            Stroke::new(2.5, ink),
        );
    }
    if has_menu {
        // A corner tick: there is more behind this button.
        let (r, b) = (rect.right() - 3.0, rect.bottom() - 3.0);
        painter.add(Shape::convex_polygon(
            vec![pos2(r, b - 4.0), pos2(r, b), pos2(r - 4.0, b)],
            fg.gamma_multiply(0.7),
            Stroke::NONE,
        ));
    }
    Pressed {
        clicked,
        menu,
        response,
    }
}

/// Line icons, like the rest of the chrome (no font has these glyphs).
fn paint_glyph(painter: &egui::Painter, c: Pos2, glyph: Glyph, color: Color32) {
    let s = Stroke::new(1.3, color);
    let at = |x: f32, y: f32| c + vec2(x, y);
    match glyph {
        Glyph::Highlighter => {
            // A marker leaning to the right, its chisel tip at the lower left.
            painter.add(Shape::closed_line(
                vec![at(1.0, -7.0), at(6.0, -2.0), at(0.5, 3.5), at(-4.5, -1.5)],
                s,
            ));
            painter.add(Shape::line(
                vec![at(-4.5, -1.5), at(-6.0, 5.0), at(0.5, 3.5)],
                s,
            ));
        }
        Glyph::Text => {
            // A serif capital T.
            painter.add(Shape::line(
                vec![at(-5.5, -4.5), at(-5.5, -6.5), at(5.5, -6.5), at(5.5, -4.5)],
                s,
            ));
            painter.line_segment([at(0.0, -6.5), at(0.0, 5.0)], s);
            painter.line_segment([at(-2.5, 5.0), at(2.5, 5.0)], s);
        }
        Glyph::Camera => {
            let body = Rect::from_center_size(at(0.0, 1.0), vec2(15.0, 10.0));
            painter.rect_stroke(body, 2.0, s, egui::StrokeKind::Middle);
            painter.add(Shape::line(
                vec![at(-3.5, -4.0), at(-2.0, -6.5), at(2.0, -6.5), at(3.5, -4.0)],
                s,
            ));
            painter.circle_stroke(at(0.0, 1.0), 2.8, s);
        }
        Glyph::Gear => paint_gear(painter, c, 7.0, color),
        Glyph::Flag => paint_flag(painter, c, color, false),
    }
}

/// A pennant on a pole (a todo), filled when `solid`.
pub fn paint_flag(painter: &egui::Painter, c: Pos2, color: Color32, solid: bool) {
    let s = Stroke::new(1.3, color);
    let at = |x: f32, y: f32| c + vec2(x, y);
    painter.line_segment([at(-4.5, -6.5), at(-4.5, 7.0)], s);
    let cloth = vec![at(-4.5, -6.0), at(5.5, -3.0), at(-4.5, 0.5)];
    if solid {
        painter.add(Shape::convex_polygon(cloth, color, Stroke::NONE));
    } else {
        painter.add(Shape::closed_line(cloth, s));
    }
}

/// A tick (done).
pub fn paint_check(painter: &egui::Painter, c: Pos2, color: Color32) {
    let s = Stroke::new(1.5, color);
    let at = |x: f32, y: f32| c + vec2(x, y);
    painter.add(Shape::line(
        vec![at(-5.0, 0.5), at(-1.5, 4.0), at(5.5, -4.0)],
        s,
    ));
}

/// A cog with eight teeth, `radius` to their tips (Settings).
pub fn paint_gear(painter: &egui::Painter, c: Pos2, radius: f32, color: Color32) {
    let s = Stroke::new(1.3, color);
    let teeth = 8;
    let inner = radius * 0.72;
    let points: Vec<Pos2> = (0..teeth * 4)
        .map(|i| {
            // Each tooth: up, across the top, down, across the gap.
            let a = (i as f32 + 0.5) / (teeth * 4) as f32 * std::f32::consts::TAU;
            let r = if i % 4 < 2 { radius } else { inner };
            c + r * vec2(a.cos(), a.sin())
        })
        .collect();
    painter.add(Shape::closed_line(points, s));
    painter.circle_stroke(c, radius * 0.32, s);
}

/// A row of colour swatches, `current` ringed. Returns the one clicked.
pub fn swatches(
    ui: &mut egui::Ui,
    colours: &[Color32],
    current: usize,
    theme: &Theme,
) -> Option<usize> {
    let mut picked = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        for (i, &colour) in colours.iter().enumerate() {
            let (rect, resp) = ui.allocate_exact_size(SWATCH, Sense::CLICK);
            let painter = ui.painter();
            if resp.hovered() {
                painter.rect_filled(rect, 5.0, theme.tab_hover_bg);
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            let chip = Rect::from_center_size(rect.center(), vec2(14.0, 14.0));
            painter.rect_filled(chip, 3.0, colour);
            // A hairline, so white (or black, in the dark) still shows.
            painter.rect_stroke(
                chip,
                3.0,
                Stroke::new(1.0, theme.text_dim.gamma_multiply(0.35)),
                egui::StrokeKind::Inside,
            );
            if i == current {
                painter.rect_stroke(
                    chip.expand(2.5),
                    5.0,
                    Stroke::new(1.5, theme.accent),
                    egui::StrokeKind::Middle,
                );
            }
            if resp.clicked() {
                picked = Some(i);
            }
        }
    });
    picked
}

pub fn open_flyout(ctx: &egui::Context, id: Id) {
    egui::Popup::open_id(ctx, id);
}

/// A small popup under a tool button, open after [`open_flyout`]. It closes
/// when `add` returns true (something was picked), on a press outside it, or
/// on Esc. (Clicks alone must not close it: the release that ends the long
/// press which opened it is one.)
pub fn flyout(ui: &mut egui::Ui, id: Id, button: Rect, add: impl FnOnce(&mut egui::Ui) -> bool) {
    let ctx = ui.ctx().clone();
    if !egui::Popup::is_id_open(&ctx, id) {
        return;
    }
    // Esc closes the flyout only, not whatever the document does with Esc.
    let esc = ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
    let shown = egui::Popup::new(id, ctx.clone(), button, ui.layer_id())
        .open_memory(None)
        .close_behavior(egui::PopupCloseBehavior::IgnoreClicks)
        .gap(4.0)
        .show(add);
    let Some(shown) = shown else { return };
    let pressed_outside = ctx.input(|i| {
        i.pointer.any_pressed()
            && i.pointer
                .interact_pos()
                .is_some_and(|p| !shown.response.rect.contains(p) && !button.contains(p))
    });
    if shown.inner || esc || pressed_outside {
        egui::Popup::close_id(&ctx, id);
    }
}

#[cfg(test)]
mod tests {
    use egui::{Event, PointerButton, RawInput};

    use super::*;

    struct Harness {
        ctx: egui::Context,
        time: f64,
        at: Pos2,
    }

    impl Harness {
        fn new() -> Self {
            let mut h = Self {
                ctx: egui::Context::default(),
                time: 0.0,
                at: Pos2::ZERO,
            };
            // Lay the button out once, then rest the pointer on it.
            h.frame(0.05, vec![]);
            let at = h.at;
            h.frame(0.05, vec![Event::PointerMoved(at)]);
            h
        }

        /// One frame `dt` seconds later: (clicked, menu).
        fn frame(&mut self, dt: f64, events: Vec<Event>) -> (bool, bool) {
            self.time += dt;
            let raw = RawInput {
                events,
                time: Some(self.time),
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(300.0, 200.0))),
                ..Default::default()
            };
            let mut seen = (false, false);
            let mut at = self.at;
            let out = self.ctx.run_ui(raw, |ui| {
                let p = tool_button(ui, Glyph::Highlighter, false, None, true, &Theme::dark());
                seen = (p.clicked, p.menu);
                at = p.response.rect.center();
            });
            out.drop_without_applying_deltas();
            self.at = at;
            seen
        }

        fn button(&self, button: PointerButton, pressed: bool) -> Vec<Event> {
            vec![Event::PointerButton {
                pos: self.at,
                button,
                pressed,
                modifiers: Modifiers::NONE,
            }]
        }
    }

    #[test]
    fn a_short_press_is_a_click() {
        let mut h = Harness::new();
        let down = h.button(PointerButton::Primary, true);
        assert_eq!(h.frame(0.05, down), (false, false));
        let up = h.button(PointerButton::Primary, false);
        assert_eq!(h.frame(0.05, up), (true, false));
    }

    #[test]
    fn holding_opens_the_menu_once_and_swallows_the_click() {
        let mut h = Harness::new();
        let down = h.button(PointerButton::Primary, true);
        assert_eq!(h.frame(0.05, down), (false, false));
        assert_eq!(h.frame(0.2, vec![]), (false, false));
        // Past the hold time.
        assert_eq!(h.frame(0.2, vec![]), (false, true));
        assert_eq!(h.frame(0.05, vec![]), (false, false));
        let up = h.button(PointerButton::Primary, false);
        assert_eq!(h.frame(0.05, up), (false, false));
        // The next press is an ordinary click again.
        let down = h.button(PointerButton::Primary, true);
        h.frame(0.3, down);
        let up = h.button(PointerButton::Primary, false);
        assert_eq!(h.frame(0.05, up), (true, false));
    }

    #[test]
    fn a_right_click_opens_the_menu() {
        let mut h = Harness::new();
        let down = h.button(PointerButton::Secondary, true);
        assert_eq!(h.frame(0.05, down), (false, false));
        let up = h.button(PointerButton::Secondary, false);
        assert_eq!(h.frame(0.05, up), (false, true));
    }
}
