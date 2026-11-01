//! whir's own controls, for what egui's widgets don't do the way a Windows
//! tool should, so every surface draws them the same way. Stock buttons,
//! fields and menus need nothing here: [`Theme::apply`] styles them.

use std::cell::RefCell;
use std::sync::Arc;
use std::time::Duration;

use egui::{
    Align, Align2, Atom, AtomExt as _, Button, Color32, ComboBox, Context, FontId, Frame, Galley,
    Id, Key, LayerId, Layout, Margin, Modal, ModalResponse, Order, Painter, Pos2, Rect, Response,
    RichText, Sense, Stroke, StrokeKind, TextEdit, Ui, UiBuilder, Vec2,
    containers::menu::MenuButton,
    pos2,
    style::WidgetVisuals,
    text::{LayoutJob, TextFormat, TextWrapping},
    vec2,
};

use crate::icons::{self, Icon};
use crate::theme::{SEMIBOLD, Theme, family};

/// Buttons a person chooses between are at least this wide, however short their
/// word, so a row of them reads as a row.
const ACTION_WIDTH: f32 = 76.0;

/// A button that is the one thing to do here.
pub fn primary(ui: &mut Ui, text: &str, theme: &Theme) -> Response {
    ui.scope(|ui| {
        as_primary(ui, theme);
        action(ui, text, theme)
    })
    .inner
}

/// Buttons added to `ui` from here on are [`primary`] ones, for when they have
/// a place to fill rather than a size of their own.
pub fn as_primary(ui: &mut Ui, theme: &Theme) {
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
}

/// A button among choices (a dialog's, a banner's).
pub fn action(ui: &mut Ui, text: &str, theme: &Theme) -> Response {
    let size = vec2(ACTION_WIDTH, theme.metrics.control_height);
    ui.add(Button::new(text).min_size(size))
}

/// A line of text cut short with "…" past `max_width`, its colour left to
/// whoever paints it.
pub fn one_line(painter: &Painter, text: &str, font: FontId, max_width: f32) -> Arc<Galley> {
    let mut job = LayoutJob::single_section(
        text.replace(['\n', '\r', '\t'], " "),
        TextFormat::simple(font, Color32::PLACEHOLDER),
    );
    job.wrap = TextWrapping {
        max_width: max_width.max(10.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    painter.layout_job(job)
}

/// How wide `text` comes out in [`tabular`].
pub fn tabular_width(painter: &Painter, text: &str, font: &FontId) -> f32 {
    let width = |c: char| {
        let galley = painter.layout_no_wrap(c.to_string(), font.clone(), Color32::PLACEHOLDER);
        galley.size().x
    };
    let digit = ('0'..='9').map(width).fold(0.0, f32::max);
    text.chars()
        .map(|c| if c.is_ascii_digit() { digit } else { width(c) })
        .sum()
}

/// Figures that keep still as they change: each digit in a cell as wide as the
/// widest, for a ticking clock (egui has no tabular figures to ask a font for).
/// `at` is where `align` puts the text. Returns where it went.
pub fn tabular(
    painter: &Painter,
    at: Pos2,
    align: Align2,
    text: &str,
    font: &FontId,
    color: Color32,
) -> Rect {
    let glyph = |c: char| painter.layout_no_wrap(c.to_string(), font.clone(), color);
    let digit = ('0'..='9').map(|c| glyph(c).size().x).fold(0.0, f32::max);
    let size = vec2(tabular_width(painter, text, font), glyph('0').size().y);
    let rect = align.anchor_size(at, size);
    let mut x = rect.left();
    for c in text.chars() {
        let galley = glyph(c);
        let width = galley.size().x;
        let cell = if c.is_ascii_digit() { digit } else { width };
        painter.galley(pos2(x + (cell - width) / 2.0, rect.top()), galley, color);
        x += cell;
    }
    rect
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

/// [`menu_entry`] that opens a menu of its own. egui's own arrow is a character
/// whir's fonts don't have.
pub fn submenu_entry(label: &str) -> Button<'_> {
    let gutter = RichText::new("")
        .atom_size(Vec2::splat(16.0))
        .atom_align(Align2::CENTER_CENTER);
    Button::new((gutter, label)).right_text(icons::text(Icon::ChevronRight, 9.0))
}

/// A menu's entry, as [`read_menu`] found it.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub icon: Option<Icon>,
    pub label: String,
    pub shortcut: String,
    pub enabled: bool,
}

/// A menu being read rather than drawn.
struct Reading {
    entries: Vec<Entry>,
    press: Option<String>,
}

thread_local! {
    static READING: RefCell<Option<Reading>> = const { RefCell::new(None) };
}

/// Run `add`, which adds a menu's entries, without any of them being drawn:
/// what it would have shown comes back. The entry labelled `press` answers as
/// though it had been clicked. This is the command palette's way into the menus.
pub fn read_menu(press: Option<&str>, add: impl FnOnce()) -> Vec<Entry> {
    let reading = Reading {
        entries: Vec::new(),
        press: press.map(str::to_owned),
    };
    READING.set(Some(reading));
    add();
    READING
        .take()
        .map_or_else(Vec::new, |reading| reading.entries)
}

/// A menu is being read, not drawn.
pub fn reading() -> bool {
    READING.with_borrow(Option::is_some)
}

/// While a menu is being read, note this entry down, and say if it's pressed.
fn read_entry(icon: Option<Icon>, label: &str, shortcut: &str, enabled: bool) -> Option<bool> {
    READING.with_borrow_mut(|reading| {
        let reading = reading.as_mut()?;
        reading.entries.push(Entry {
            icon,
            label: label.to_owned(),
            shortcut: shortcut.to_owned(),
            enabled,
        });
        Some(enabled && reading.press.as_deref() == Some(label))
    })
}

/// [`menu_entry`], added: closes the menu and returns true when it's picked.
pub fn entry(ui: &mut Ui, icon: Option<Icon>, label: &str, shortcut: &str, enabled: bool) -> bool {
    if let Some(pressed) = read_entry(icon, label, shortcut, enabled) {
        return pressed;
    }
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

/// [`entry`] that's one of several, highlighted when it's the one in use.
pub fn radio_entry(
    ui: &mut Ui,
    icon: Option<Icon>,
    label: &str,
    shortcut: &str,
    selected: bool,
) -> bool {
    if let Some(pressed) = read_entry(icon, label, shortcut, true) {
        return pressed;
    }
    let picked = ui
        .add(menu_entry(icon, label, shortcut).selected(selected))
        .clicked();
    if picked {
        ui.close();
    }
    picked
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
/// the accent colour beside the one you're on. `compact`, only its icon, with
/// its name for a tooltip.
pub fn nav_row(
    ui: &mut Ui,
    icon: Icon,
    label: &str,
    on: bool,
    compact: bool,
    theme: &Theme,
) -> bool {
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
    if compact {
        icons::paint(painter, rect.center(), icon, m.icon_size, text);
        return response.on_hover_text(label).clicked();
    }
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

/// How wide a setting's drop-down is, so they line up down a page.
pub const CONTROL_WIDTH: f32 = 220.0;
/// Inside a card, from its edge to what's in it.
pub const CARD_PAD: Vec2 = vec2(16.0, 10.0);
/// A row's words need this much room beside its control, or the control goes
/// under them.
const WORDS_MIN: f32 = 170.0;
const WORDS_GAP: f32 = 16.0;

/// Text in the semibold face.
pub fn strong(size: f32) -> FontId {
    FontId::new(size, family(SEMIBOLD))
}

/// The top of a page: its title and what it's for, with what can be done from
/// it on the right.
pub fn page_header(
    ui: &mut Ui,
    title: &str,
    about: &str,
    theme: &Theme,
    actions: impl FnOnce(&mut Ui),
) {
    let m = theme.metrics;
    ui.horizontal(|ui| {
        ui.set_min_height(m.tool_size);
        let text = RichText::new(title)
            .font(strong(m.display_text_size))
            .color(theme.text);
        ui.label(text);
        ui.with_layout(Layout::right_to_left(Align::Center), actions);
    });
    if !about.is_empty() {
        ui.add_space(2.0);
        ui.label(RichText::new(about).color(theme.text_dim));
    }
    ui.add_space(18.0);
}

/// A surface for things that belong together, with lines between its rows.
pub struct Card<'a> {
    pub ui: &'a mut Ui,
    pub theme: &'a Theme,
    first: bool,
}

/// A card under a heading of its own.
pub fn group<R>(
    ui: &mut Ui,
    title: &str,
    theme: &Theme,
    add: impl FnOnce(&mut Card<'_>) -> R,
) -> R {
    if !title.is_empty() {
        let text = RichText::new(title)
            .font(strong(theme.metrics.body_text_size))
            .color(theme.text);
        ui.label(text);
        ui.add_space(6.0);
    }
    let out = card(ui, theme, add);
    ui.add_space(22.0);
    out
}

/// A card as wide as there's room for.
pub fn card<R>(ui: &mut Ui, theme: &Theme, add: impl FnOnce(&mut Card<'_>) -> R) -> R {
    Frame::NONE
        .fill(theme.card_bg)
        .stroke(Stroke::new(1.0, theme.separator))
        .corner_radius(theme.metrics.corner_radius as u8)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;
            add(&mut Card {
                ui,
                theme,
                first: true,
            })
        })
        .inner
}

impl Card<'_> {
    /// The line above a row, but for the first.
    pub fn rule(&mut self) {
        if std::mem::take(&mut self.first) {
            return;
        }
        let x = self.ui.max_rect().x_range();
        let y = self.ui.cursor().top();
        let y = self.ui.painter().round_to_pixel_center(y);
        let line = Stroke::new(1.0, self.theme.separator);
        self.ui.painter().hline(x, y, line);
    }

    /// A row of whatever's needed, its padding seen to.
    pub fn row<R>(&mut self, add: impl FnOnce(&mut Ui) -> R) -> R {
        self.rule();
        Frame::NONE
            .inner_margin(Margin::symmetric(CARD_PAD.x as i8, CARD_PAD.y as i8))
            .show(self.ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 3.0;
                add(ui)
            })
            .inner
    }

    /// A row of [`inline_text`]s: their words line up with the words of the
    /// card's other rows, and it's a little closer to its neighbours.
    pub fn field_row<R>(&mut self, add: impl FnOnce(&mut Ui) -> R) -> R {
        self.rule();
        let x = (CARD_PAD.x - INLINE_MARGIN) as i8;
        Frame::NONE
            .inner_margin(Margin::symmetric(x, 5))
            .show(self.ui, |ui| {
                ui.set_width(ui.available_width());
                add(ui)
            })
            .inner
    }

    /// A setting: what it is on the left, its control on the right, in a space
    /// `width` wide; or under the words, when there isn't the room.
    pub fn setting(&mut self, title: &str, about: &str, width: f32, control: impl FnOnce(&mut Ui)) {
        self.rule();
        let m = self.theme.metrics;
        let ui = &mut *self.ui;
        let full = ui.available_width();
        let inner = full - 2.0 * CARD_PAD.x;
        let stacked = inner - width - WORDS_GAP < WORDS_MIN;
        let words = if stacked {
            inner
        } else {
            inner - width - WORDS_GAP
        };
        let painter = ui.painter().clone();
        let body = FontId::proportional(m.body_text_size);
        let small = FontId::proportional(m.small_text_size);
        let title = painter.layout(title.to_owned(), body, self.theme.text, words);
        let about = (!about.is_empty())
            .then(|| painter.layout(about.to_owned(), small, self.theme.text_dim, words));
        let text = title.size().y + about.as_ref().map_or(0.0, |a| a.size().y + 1.0);
        let body = if stacked {
            text + 8.0 + m.control_height
        } else {
            text.max(m.control_height)
        };
        let size = vec2(full, body + 2.0 * CARD_PAD.y);
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        let area = rect.shrink2(CARD_PAD);
        let top = if stacked {
            area.top()
        } else {
            area.center().y - text / 2.0
        };
        let below = title.size().y + 1.0;
        painter.galley(pos2(area.left(), top), title, self.theme.text);
        if let Some(about) = about {
            let at = pos2(area.left(), top + below);
            painter.galley(at, about, self.theme.text_dim);
        }
        let (at, layout) = if stacked {
            (
                pos2(area.left(), area.bottom() - m.control_height),
                Layout::left_to_right(Align::Center),
            )
        } else {
            (
                pos2(
                    area.right() - width,
                    area.center().y - m.control_height / 2.0,
                ),
                Layout::right_to_left(Align::Center),
            )
        };
        let slot = Rect::from_min_size(at, vec2(width.min(inner), m.control_height));
        let mut slot = ui.new_child(UiBuilder::new().max_rect(slot).layout(layout));
        control(&mut slot);
    }

    /// A setting that's on or off; true when it was changed.
    pub fn switch(&mut self, title: &str, about: &str, on: &mut bool) -> bool {
        let theme = self.theme;
        let mut changed = false;
        self.setting(title, about, SWITCH.x, |ui| {
            changed = toggle(ui, on, theme).changed();
        });
        changed
    }

    /// A setting with a list to pick from; what was picked, if it changed.
    pub fn choice<T: Copy + PartialEq>(
        &mut self,
        title: &str,
        about: &str,
        current: T,
        options: &[T],
        label: impl Fn(T) -> String,
    ) -> Option<T> {
        let mut picked = None;
        self.setting(title, about, CONTROL_WIDTH, |ui| {
            picked = dropdown(ui, Id::new(title), current, options, label);
        });
        picked
    }
}

const SWITCH: Vec2 = vec2(40.0, 20.0);

/// A switch, as Windows' are: for something that takes effect at once.
pub fn toggle(ui: &mut Ui, on: &mut bool, theme: &Theme) -> Response {
    let (rect, mut response) = ui.allocate_exact_size(SWITCH, Sense::CLICK);
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let t = ui.ctx().animate_bool_with_time(response.id, *on, 0.1);
    let hovered = response.hovered();
    let radius = rect.height() / 2.0;
    let painter = ui.painter();
    let fill = if hovered {
        theme.accent_fill_hover
    } else {
        theme.accent_fill
    };
    if t < 1.0 {
        let edge = if hovered { theme.text } else { theme.text_dim };
        painter.rect_filled(rect, radius, theme.field_bg);
        painter.rect_stroke(rect, radius, Stroke::new(1.0, edge), StrokeKind::Inside);
    }
    if t > 0.0 {
        painter.rect_filled(rect, radius, fill.gamma_multiply(t));
    }
    let x = egui::lerp((rect.left() + radius)..=(rect.right() - radius), t);
    let knob = if t > 0.5 {
        theme.on_accent
    } else if hovered {
        theme.text
    } else {
        theme.text_dim
    };
    let size = if hovered { 6.5 } else { 6.0 };
    painter.circle_filled(pos2(x, rect.center().y), size, knob);
    if response.has_focus() {
        let ring = Stroke::new(1.5, theme.accent);
        painter.rect_stroke(rect.expand(2.0), radius + 2.0, ring, StrokeKind::Middle);
    }
    response
}

/// A drop-down as wide as the room it's given; what was picked, if it changed.
pub fn dropdown<T: Copy + PartialEq>(
    ui: &mut Ui,
    id: Id,
    current: T,
    options: &[T],
    label: impl Fn(T) -> String,
) -> Option<T> {
    let mut picked = current;
    ComboBox::from_id_salt(id)
        .icon(combo_chevron)
        .width(ui.available_width())
        .selected_text(label(current))
        .show_ui(ui, |ui| {
            for &option in options {
                ui.selectable_value(&mut picked, option, label(option));
            }
        });
    (picked != current).then_some(picked)
}

/// How far [`inline_text`]'s words sit in from its edge: start it this far left
/// to line them up with text.
pub const INLINE_MARGIN: f32 = 6.0;

/// One [`inline_text`].
pub struct Inline<'a> {
    pub id: Id,
    pub current: &'a str,
    /// Shown when there's nothing there.
    pub hint: &'a str,
    pub width: f32,
    pub font: FontId,
    pub color: Color32,
}

/// What can be edited where it stands: plain text until the pointer comes
/// near, then a field. Keeps what's typed to itself until Enter or leaving it,
/// then returns it; Esc puts it back.
pub fn inline_text(ui: &mut Ui, field: Inline<'_>, theme: &Theme) -> Option<String> {
    let m = theme.metrics;
    let Inline {
        id,
        current,
        hint,
        width,
        font,
        color,
    } = field;
    let size = vec2(width, m.control_height);
    let mut text = ui
        .data_mut(|d| d.get_temp::<String>(id))
        .unwrap_or_else(|| current.to_owned());
    let focused = ui.memory(|mem| mem.has_focus(id));
    let hovered = ui.rect_contains_pointer(Rect::from_min_size(ui.cursor().min, size));
    let (fill, edge) = if focused {
        (theme.field_bg, theme.accent)
    } else if hovered {
        (theme.field_bg, theme.border_strong)
    } else {
        (Color32::TRANSPARENT, Color32::TRANSPARENT)
    };
    let frame = Frame::NONE
        .fill(fill)
        .stroke(Stroke::new(1.0, edge))
        .corner_radius(m.control_radius)
        .inner_margin(Margin::symmetric(INLINE_MARGIN as i8, 3));
    let field = TextEdit::singleline(&mut text)
        .id(id)
        .hint_text(RichText::new(hint).color(theme.text_faint))
        .font(font)
        .text_color(color)
        .frame(frame);
    let response = ui.add_sized(size, field);
    let left = response.lost_focus();
    if response.has_focus() {
        ui.data_mut(|d| d.insert_temp(id, text));
        return None;
    }
    ui.data_mut(|d| d.remove::<String>(id));
    let cancelled = ui.input(|i| i.key_pressed(Key::Escape));
    (left && !cancelled && text.trim() != current).then(|| text.trim().to_owned())
}

/// An icon and a word, with no button around them until the pointer's there.
pub fn icon_action(ui: &mut Ui, icon: Icon, text: &str, theme: &Theme) -> Response {
    let button = Button::new((icons::text(icon, 12.0), text))
        .frame_when_inactive(false)
        .min_size(vec2(0.0, theme.metrics.control_height));
    ui.add(button)
}

/// A row in a list of results: an icon, its words (laid out, with the letters
/// that matched picked out), and `side` on the right. `on`: where the arrow keys
/// have got to.
pub fn result_row(
    ui: &mut Ui,
    icon: Option<Icon>,
    words: LayoutJob,
    side: &str,
    on: bool,
    theme: &Theme,
) -> Response {
    const HEIGHT: f32 = 30.0;
    let m = theme.metrics;
    let size = vec2(ui.available_width(), HEIGHT);
    let (rect, response) = ui.allocate_exact_size(size, Sense::CLICK);
    let painter = ui.painter();
    if on {
        painter.rect_filled(rect, m.control_radius, theme.drawer_item_active);
    } else if response.hovered() {
        painter.rect_filled(rect, m.control_radius, theme.drawer_item_hover);
    }
    let middle = rect.center().y;
    if let Some(icon) = icon {
        let at = pos2(rect.left() + 16.0, middle);
        icons::paint(painter, at, icon, 14.0, theme.text_dim);
    }
    let small = FontId::proportional(m.small_text_size);
    let side = painter.layout_no_wrap(side.to_owned(), small, theme.text_dim);
    let side_x = rect.right() - 10.0 - side.size().x;
    painter.galley(
        pos2(side_x, middle - side.size().y / 2.0),
        side,
        theme.text_dim,
    );
    let mut words = words;
    words.wrap.max_width = words.wrap.max_width.min(side_x - rect.left() - 44.0);
    let words = painter.layout_job(words);
    let at = pos2(rect.left() + 32.0, middle - words.size().y / 2.0);
    painter.galley(at, words, theme.text);
    response
}

/// The ⋯ at the end of a row, a square as high as a control, and the menu
/// behind it.
pub fn more_button(ui: &mut Ui, theme: &Theme, add: impl FnOnce(&mut Ui)) {
    let button = Button::new(icons::text(Icon::More, 14.0))
        .frame_when_inactive(false)
        .min_size(Vec2::splat(theme.metrics.control_height));
    ui.scope(|ui| {
        // Or it comes out wider than rows have made room for.
        ui.spacing_mut().button_padding = Vec2::ZERO;
        MenuButton::from_button(button).ui(ui, |ui| {
            ui.set_min_width(190.0);
            add(ui);
        });
    });
}

/// A word or two that says how something stands.
pub fn pill(ui: &mut Ui, text: &str, color: Color32, theme: &Theme) {
    const PAD: Vec2 = vec2(8.0, 2.0);
    let font = FontId::proportional(theme.metrics.small_text_size);
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, color);
    let size = galley.size() + 2.0 * PAD;
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let fill = color.gamma_multiply(0.14);
    ui.painter().rect_filled(rect, size.y / 2.0, fill);
    ui.painter().galley(rect.min + PAD, galley, color);
}

/// Something worth saying across the top of a page, tinted `color`.
pub fn banner<R>(ui: &mut Ui, color: Color32, theme: &Theme, add: impl FnOnce(&mut Ui) -> R) -> R {
    Frame::NONE
        .fill(color.gamma_multiply(0.12))
        .corner_radius(theme.metrics.corner_radius as u8)
        .inner_margin(Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(add).inner
        })
        .inner
}

#[cfg(test)]
mod tests {
    use egui::{Event, Modifiers, RawInput};

    use super::*;
    use crate::theme::test_context;

    /// Frames of one [`inline_text`], and what it handed back.
    struct Field {
        ctx: Context,
        id: Id,
        current: String,
        got: Option<String>,
        time: f64,
    }

    impl Field {
        fn new(current: &str) -> Field {
            Field {
                ctx: test_context(),
                id: Id::new("field"),
                current: current.to_owned(),
                got: None,
                time: 0.0,
            }
        }

        fn frame(&mut self, events: Vec<Event>) {
            self.time += 0.05;
            let theme = Theme::light();
            let raw = RawInput {
                time: Some(self.time),
                events,
                ..RawInput::default()
            };
            let Self {
                ctx,
                id,
                current,
                got,
                ..
            } = self;
            let mut out = ctx.run_ui(raw, |ui| {
                let field = Inline {
                    id: *id,
                    current,
                    hint: "",
                    width: 120.0,
                    font: FontId::proportional(13.0),
                    color: theme.text,
                };
                if let Some(text) = inline_text(ui, field, &theme) {
                    *got = Some(text);
                }
            });
            out.textures_delta.clear();
        }

        fn key(&mut self, key: Key) {
            self.frame(vec![Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            }]);
        }

        /// Give it the keyboard, and type.
        fn typing(&mut self, text: &str) {
            self.frame(Vec::new());
            self.ctx.memory_mut(|m| m.request_focus(self.id));
            self.frame(Vec::new());
            self.frame(vec![Event::Text(text.to_owned())]);
        }
    }

    #[test]
    fn a_field_hands_over_what_was_typed_on_enter() {
        let mut f = Field::new("");
        f.typing("Rack audit");
        assert_eq!(f.got, None, "not while it's still being typed");
        f.key(Key::Enter);
        f.frame(Vec::new());
        assert_eq!(f.got.as_deref(), Some("Rack audit"));
    }

    #[test]
    fn esc_puts_a_field_back() {
        let mut f = Field::new("");
        f.typing("Rack audit");
        f.key(Key::Escape);
        f.frame(Vec::new());
        assert_eq!(f.got, None);
    }

    #[test]
    fn a_field_left_as_it_was_changes_nothing() {
        let mut f = Field::new("Rack audit");
        f.typing("");
        f.key(Key::Enter);
        f.frame(Vec::new());
        assert_eq!(f.got, None);
    }

    #[test]
    fn a_menu_read_is_not_drawn_and_answers_for_the_entry_pressed() {
        let ctx = test_context();
        let mut entries = Vec::new();
        let mut pressed = (false, false, false);
        let mut out = ctx.run_ui(RawInput::default(), |ui| {
            entries = read_menu(Some("Two"), || {
                pressed.0 = entry(ui, None, "One", "", true);
                pressed.1 = radio_entry(ui, Some(Icon::Check), "Two", "Ctrl+2", false);
                pressed.2 = entry(ui, None, "Three", "", false);
            });
        });
        out.textures_delta.clear();
        assert_eq!(pressed, (false, true, false));
        let labels: Vec<&str> = entries.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(labels, ["One", "Two", "Three"]);
        assert_eq!(entries[1].shortcut, "Ctrl+2");
        assert!(!entries[2].enabled);
        assert!(!reading());
    }
}
