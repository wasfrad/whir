//! Widgets for the toolbar strip between the tabs and the document: a tool
//! button that can also be held for a menu, and the menu itself.

use std::time::Duration;

use egui::{Color32, Id, Key, Modifiers, Rect, Sense, Shape, Stroke, pos2, vec2};

use crate::icons::{self, Icon};
use crate::theme::Theme;

/// Seconds a tool button is held before its menu opens.
const HOLD: f64 = 0.35;
const SWATCH: egui::Vec2 = vec2(24.0, 24.0);
/// Seconds a tool takes to light up under the pointer.
const HOVER_FADE: f32 = 0.08;

/// What happened to a [`tool_button`] this frame.
pub struct Pressed {
    /// A plain click.
    pub clicked: bool,
    /// Held down, or right-clicked: open the button's menu.
    pub menu: bool,
    pub response: egui::Response,
}

/// A toolbar button. `on` tints it (a tool that is active); `ink` draws a bar
/// of that colour under the icon; `has_menu` marks its corner and reports a
/// long press or right-click as [`Pressed::menu`].
pub fn tool_button(
    ui: &mut egui::Ui,
    icon: Icon,
    on: bool,
    ink: Option<Color32>,
    has_menu: bool,
    theme: &Theme,
) -> Pressed {
    let m = theme.metrics;
    let (rect, response) = ui.allocate_exact_size(egui::Vec2::splat(m.tool_size), Sense::CLICK);

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
    let fg = if on {
        theme.accent
    } else if response.hovered() {
        theme.text
    } else {
        theme.text_dim
    };
    if let Some(fill) = fill_of(ui, &response, on, theme) {
        painter.rect_filled(rect, m.control_radius, fill);
    }
    let lift = if ink.is_some() { 2.5 } else { 0.0 };
    icons::paint(
        painter,
        rect.center() - vec2(0.0, lift),
        icon,
        m.icon_size,
        fg,
    );
    if let Some(ink) = ink {
        let y = rect.center().y + 8.5;
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

/// What's behind a tool: tinted while on, a quick fade in and out on hover.
pub fn fill_of(
    ui: &egui::Ui,
    response: &egui::Response,
    on: bool,
    theme: &Theme,
) -> Option<Color32> {
    let id = response.id.with("hover");
    let hover = ui
        .ctx()
        .animate_bool_with_time(id, response.hovered(), HOVER_FADE);
    if on {
        Some(theme.drawer_item_active)
    } else if response.is_pointer_button_down_on() {
        Some(theme.control_pressed)
    } else if hover > 0.0 {
        Some(theme.control_hover.gamma_multiply(hover))
    } else {
        None
    }
}

/// A tool that does one thing, greyed out when it can't: true when clicked.
pub fn tool(ui: &mut egui::Ui, icon: Icon, tip: &str, enabled: bool, theme: &Theme) -> bool {
    ui.add_enabled_ui(enabled, |ui| {
        let pressed = tool_button(ui, icon, false, None, false, theme);
        let clicked = pressed.clicked;
        pressed.response.on_hover_text(tip);
        clicked
    })
    .inner
}

/// A tool that stays on: true when clicked.
pub fn toggle(ui: &mut egui::Ui, icon: Icon, on: bool, tip: &str, theme: &Theme) -> bool {
    let pressed = tool_button(ui, icon, on, None, false, theme);
    let clicked = pressed.clicked;
    pressed.response.on_hover_text(tip);
    clicked
}

/// Words in the toolbar that are also a button: a page number, a zoom.
pub fn readout_button<'a>(text: &'a str, width: f32, theme: &Theme) -> egui::Button<'a> {
    egui::Button::new(text)
        .frame_when_inactive(false)
        .min_size(vec2(width, theme.metrics.tool_size))
}

pub fn readout(ui: &mut egui::Ui, text: &str, width: f32, theme: &Theme) -> egui::Response {
    ui.add(readout_button(text, width, theme))
}

/// A toolbar button with a word beside its icon, tinted while `on`: a state
/// worth reading at a glance.
pub fn labelled_button(
    ui: &mut egui::Ui,
    icon: Icon,
    label: &str,
    on: bool,
    theme: &Theme,
) -> egui::Response {
    let m = theme.metrics;
    let font = egui::FontId::proportional(m.body_text_size);
    let fg = if on { theme.accent } else { theme.text_dim };
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, fg);
    let size = vec2(
        8.0 + m.icon_size + 6.0 + galley.size().x + 10.0,
        m.tool_size,
    );
    let (rect, response) = ui.allocate_exact_size(size, Sense::CLICK);
    let fg = if !on && response.hovered() {
        theme.text
    } else {
        fg
    };
    let painter = ui.painter();
    if let Some(fill) = fill_of(ui, &response, on, theme) {
        painter.rect_filled(rect, m.control_radius, fill);
    }
    let center = pos2(rect.left() + 8.0 + m.icon_size / 2.0, rect.center().y);
    icons::paint(painter, center, icon, m.icon_size, fg);
    let at = pos2(
        rect.left() + 8.0 + m.icon_size + 6.0,
        rect.center().y - galley.size().y / 2.0,
    );
    painter.galley(at, galley, fg);
    response
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
                painter.rect_filled(rect, theme.metrics.control_radius, theme.control_hover);
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
    use egui::{Event, PointerButton, Pos2, RawInput};

    use super::*;

    struct Harness {
        ctx: egui::Context,
        time: f64,
        at: Pos2,
    }

    impl Harness {
        fn new() -> Self {
            let mut h = Self {
                ctx: crate::theme::test_context(),
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
                let p = tool_button(ui, Icon::Highlighter, false, None, true, &Theme::dark());
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
