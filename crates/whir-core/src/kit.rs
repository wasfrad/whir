//! whir's own controls, for what egui's widgets don't do the way a Windows
//! tool should, so every surface draws them the same way. Stock buttons,
//! fields and menus need nothing here: [`Theme::apply`] styles them.

use std::time::Duration;

use egui::{
    Align, Align2, Atom, AtomExt as _, Button, Context, FontId, Frame, Id, LayerId, Layout, Modal,
    ModalResponse, Order, Rect, Response, RichText, Sense, Stroke, StrokeKind, TextStyle,
    TextWrapMode, Ui, Vec2, WidgetText, pos2, style::WidgetVisuals, vec2,
};

use crate::icons::{self, Icon};
use crate::theme::Theme;

/// Buttons a person chooses between are at least this wide, however short their
/// word, so a row of them reads as a row.
const ACTION_WIDTH: f32 = 76.0;

/// A button that is the one thing to do here.
pub fn primary(ui: &mut Ui, text: &str, theme: &Theme) -> Response {
    ui.scope(|ui| {
        let w = &mut ui.visuals_mut().widgets;
        for (state, fill) in [
            (&mut w.inactive, theme.accent_fill),
            (&mut w.hovered, theme.accent_fill_hover),
            (&mut w.active, theme.accent_fill),
        ] {
            state.weak_bg_fill = fill;
            state.bg_stroke = Stroke::NONE;
            state.fg_stroke.color = theme.on_accent;
        }
        action(ui, text, theme)
    })
    .inner
}

/// A button among choices (a dialog's, a banner's).
pub fn action(ui: &mut Ui, text: &str, theme: &Theme) -> Response {
    let size = vec2(ACTION_WIDTH, theme.metrics.control_height);
    ui.add(Button::new(text).min_size(size))
}

/// A check box: filled with the accent colour when on, as Windows' are.
pub fn checkbox(
    ui: &mut Ui,
    on: &mut bool,
    text: impl Into<WidgetText>,
    theme: &Theme,
) -> Response {
    const BOX: f32 = 16.0;
    const GAP: f32 = 8.0;
    let galley = text.into().into_galley(
        ui,
        Some(TextWrapMode::Extend),
        f32::INFINITY,
        TextStyle::Body,
    );
    let height = galley.size().y.max(theme.metrics.control_height);
    let size = vec2(BOX + GAP + galley.size().x, height);
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::CLICK);
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let hovered = response.hovered();
    let mark = Rect::from_center_size(
        pos2(rect.left() + BOX / 2.0, rect.center().y),
        Vec2::splat(BOX),
    );
    let radius = theme.metrics.control_radius;
    let painter = ui.painter();
    if *on {
        let fill = if hovered {
            theme.accent_fill_hover
        } else {
            theme.accent_fill
        };
        painter.rect_filled(mark, radius, fill);
        icons::paint(painter, mark.center(), Icon::Check, 11.0, theme.on_accent);
    } else {
        let edge = if hovered {
            theme.text_dim
        } else {
            theme.border_strong
        };
        painter.rect_filled(mark, radius, theme.field_bg);
        painter.rect_stroke(mark, radius, Stroke::new(1.0, edge), StrokeKind::Inside);
    }
    if response.has_focus() {
        let ring = Stroke::new(1.5, theme.accent);
        painter.rect_stroke(mark.expand(2.0), radius + 2.0, ring, StrokeKind::Middle);
    }
    let at = pos2(mark.right() + GAP, rect.center().y - galley.size().y / 2.0);
    painter.galley(at, galley, theme.text);
    response
}

/// The arrow of a drop-down, for `ComboBox::icon`.
pub fn combo_chevron(ui: &Ui, rect: Rect, visuals: &WidgetVisuals, _open: bool) {
    let color = visuals.fg_stroke.color;
    icons::paint(ui.painter(), rect.center(), Icon::ChevronDown, 10.0, color);
}

/// An entry in a menu: its icon in a gutter all the entries share, its
/// shortcut on the right. Add it with `ui.add` or `ui.add_enabled`.
pub fn menu_entry<'a>(
    icon: Option<Icon>,
    label: impl Into<Atom<'a>>,
    shortcut: &'a str,
) -> Button<'a> {
    let gutter = match icon {
        Some(icon) => icons::text(icon, 14.0),
        None => RichText::new(""),
    };
    let gutter = gutter
        .atom_size(Vec2::splat(16.0))
        .atom_align(Align2::CENTER_CENTER);
    Button::new((gutter, label)).shortcut_text(shortcut)
}

/// [`menu_entry`], added: closes the menu and returns true when it's picked.
pub fn entry(ui: &mut Ui, icon: Option<Icon>, label: &str, shortcut: &str, enabled: bool) -> bool {
    let picked = ui
        .add_enabled(enabled, menu_entry(icon, label, shortcut))
        .clicked();
    if picked {
        ui.close();
    }
    picked
}

/// [`entry`] that shows whether it's on with a tick in the gutter.
pub fn check_entry(ui: &mut Ui, on: bool, label: &str, shortcut: &str) -> bool {
    entry(ui, on.then_some(Icon::Check), label, shortcut, true)
}

/// A short line between groups of tools.
pub fn bar_separator(ui: &mut Ui, theme: &Theme) {
    let (rect, _) = ui.allocate_exact_size(vec2(9.0, 16.0), Sense::hover());
    let x = ui.painter().round_to_pixel_center(rect.center().x);
    ui.painter().line_segment(
        [pos2(x, rect.top()), pos2(x, rect.bottom())],
        Stroke::new(1.0, theme.separator),
    );
}

/// What floats over a document: a find bar, a banner.
pub fn floating(theme: &Theme) -> Frame {
    Frame::NONE
        .fill(theme.surface_raised)
        .stroke(Stroke::new(1.0, theme.border_strong))
        .corner_radius(theme.metrics.corner_radius as u8)
        .shadow(theme.popup_shadow())
}

/// A question that has to be answered before going on.
pub fn dialog(
    ctx: &Context,
    id: Id,
    width: f32,
    theme: &Theme,
    add: impl FnOnce(&mut Ui),
) -> ModalResponse<()> {
    let frame = Frame::popup(&ctx.global_style())
        .inner_margin(24.0)
        .corner_radius(theme.metrics.corner_radius as u8);
    Modal::new(id)
        .backdrop_color(theme.modal_backdrop)
        .frame(frame)
        .show(ctx, |ui| {
            ui.set_width(width);
            add(ui);
        })
}

pub fn dialog_title(ui: &mut Ui, text: impl Into<String>, theme: &Theme) {
    ui.label(RichText::new(text).heading().color(theme.text));
    ui.add_space(8.0);
}

/// A dialog's buttons, along its bottom edge from the right: the first added
/// is rightmost, so add Cancel first.
pub fn dialog_actions(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    ui.add_space(20.0);
    ui.with_layout(Layout::right_to_left(Align::Center), add);
}

/// A message that goes away by itself: `toast` is what it says and when it
/// ends (in `ui.input` time), and is cleared then.
pub fn toast(ui: &Ui, area: Rect, toast: &mut Option<(String, f64)>, theme: &Theme) {
    /// Seconds it takes to fade.
    const FADE: f64 = 0.3;
    let Some((text, until)) = toast else { return };
    let left = *until - ui.input(|i| i.time);
    if left <= 0.0 {
        *toast = None;
        return;
    }
    let alpha = (left / FADE).clamp(0.0, 1.0) as f32;
    let painter = ui
        .ctx()
        .layer_painter(LayerId::new(Order::Foreground, Id::new("whir.toast")));
    let m = theme.metrics;
    let galley = painter.layout_no_wrap(
        text.clone(),
        FontId::proportional(m.body_text_size),
        theme.text.gamma_multiply(alpha),
    );
    let pad = vec2(14.0, 8.0);
    let size = galley.size() + pad * 2.0;
    let rect = Rect::from_center_size(
        pos2(area.center().x, area.bottom() - 28.0 - size.y / 2.0),
        size,
    );
    let mut shadow = theme.popup_shadow();
    shadow.color = shadow.color.gamma_multiply(alpha);
    painter.add(shadow.as_shape(rect, m.corner_radius));
    painter.rect_filled(
        rect,
        m.corner_radius,
        theme.surface_raised.gamma_multiply(alpha),
    );
    painter.rect_stroke(
        rect,
        m.corner_radius,
        Stroke::new(1.0, theme.border_strong.gamma_multiply(alpha)),
        StrokeKind::Inside,
    );
    painter.galley(rect.min + pad, galley, theme.text);
    // Nothing moves until the fade starts.
    let wait = if left > FADE { left - FADE } else { 0.03 };
    ui.ctx()
        .request_repaint_after(Duration::from_secs_f64(wait));
}

/// A place with nothing in it yet: what belongs here, and what to do about
/// it. Buttons go below it.
pub fn empty_state(ui: &mut Ui, icon: Icon, title: &str, hint: &str, theme: &Theme) {
    ui.label(icons::text(icon, 32.0).color(theme.text_faint));
    ui.add_space(10.0);
    ui.label(RichText::new(title).heading().color(theme.text));
    ui.add_space(4.0);
    ui.label(RichText::new(hint).color(theme.text_dim));
}

/// A row in a list of places, as Windows Settings marks them: a short bar in
/// the accent colour beside the one you're on.
pub fn nav_row(ui: &mut Ui, icon: Icon, label: &str, on: bool, theme: &Theme) -> bool {
    let m = theme.metrics;
    let size = vec2(ui.available_width(), 32.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::CLICK);
    let painter = ui.painter();
    if on {
        painter.rect_filled(rect, m.control_radius, theme.drawer_item_hover);
        let bar = Rect::from_center_size(pos2(rect.left() + 1.5, rect.center().y), vec2(3.0, 16.0));
        painter.rect_filled(bar, 1.5, theme.accent);
    } else if response.hovered() {
        painter.rect_filled(rect, m.control_radius, theme.drawer_item_hover);
    }
    let text = if on { theme.text } else { theme.text_dim };
    let at = pos2(rect.left() + 20.0, rect.center().y);
    icons::paint(painter, at, icon, m.icon_size, text);
    painter.text(
        pos2(rect.left() + 40.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(m.body_text_size),
        text,
    );
    response.clicked()
}
