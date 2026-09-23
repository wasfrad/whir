//! The notes drawer: a slim panel at the right edge, hidden until you reach for
//! it. Resting the pointer against the edge slides out a narrow strip; resting
//! on the strip opens the list of saved notes and tables, newest first.
//!
//! Every document has an overlay scrollbar along that same edge, so the drawer
//! defers to it: it never appears while a button is held or a scrollbar is in
//! use, nor when the pointer is on a scrollbar thumb, and it waits for a
//! deliberate pause at the edge.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use egui::{
    Align2, Area, CursorIcon, FontId, Id, Key, Modifiers, Order, Pos2, Rect, Sense, Stroke, pos2,
    vec2,
};
use whir_core::DocKind;
use whir_core::library::{EntryKind, LibraryEntry, LibrarySnapshot};
use whir_core::theme::Theme;

use crate::keys::consume_exact;
use crate::reltime::{self, SystemFormats};
use crate::tab_bar::paint_kind_glyph;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Hidden,
    /// The strip is out; the list isn't.
    Peek,
    Open,
}

/// Seconds the pointer must rest in the edge band before the strip peeks out.
const PEEK_DWELL: f64 = 0.15;
/// Seconds resting on the strip before the drawer opens.
const OPEN_DWELL: f64 = 0.25;
/// Seconds away before the strip hides again.
const PEEK_LINGER: f64 = 0.4;
/// Seconds away before the open drawer closes (unless pinned).
const OPEN_LINGER: f64 = 0.35;
/// No peeking this long after a click, a release or a scrollbar drag.
const QUIET_AFTER_INPUT: f64 = 0.4;
/// After backing off for a scrollbar thumb.
const QUIET_AFTER_THUMB: f64 = 0.6;

/// One frame's worth of what the drawer needs to decide (pure; see tests).
#[derive(Clone, Debug)]
pub struct Input {
    pub now: f64,
    pub pointer: Option<Pos2>,
    /// The document area (below the tab strip).
    pub content: Rect,
    pub any_down: bool,
    pub released: bool,
    /// Something (a scrollbar, a selection) is being dragged.
    pub dragging: bool,
    /// Scrollbar thumbs drawn this frame.
    pub thumbs: Vec<Rect>,
    /// A modal/popup is open, the window is being resized, or it's too narrow.
    pub blocked: bool,
    /// The pointer is over the drawer as drawn last frame (with some slack).
    pub over_panel: bool,
    /// One of the drawer's own menus is open.
    pub menu_open: bool,
    pub trigger: f32,
    pub strip: f32,
}

pub struct Drawer {
    phase: Phase,
    pinned: bool,
    band_since: Option<f64>,
    strip_since: Option<f64>,
    away_since: Option<f64>,
    quiet_until: f64,
    /// The pointer entered the edge with a button held (a drag): not a reach.
    entered_down: bool,
    /// Keyboard selection (pinned mode).
    selected: usize,
    panel: Rect,
    scroll_to_selected: bool,
}

impl Default for Input {
    fn default() -> Self {
        Self {
            now: 0.0,
            pointer: None,
            content: Rect::NOTHING,
            any_down: false,
            released: false,
            dragging: false,
            thumbs: Vec::new(),
            blocked: false,
            over_panel: false,
            menu_open: false,
            trigger: 6.0,
            strip: 14.0,
        }
    }
}

impl Default for Drawer {
    fn default() -> Self {
        Self {
            phase: Phase::Hidden,
            pinned: false,
            band_since: None,
            strip_since: None,
            away_since: None,
            quiet_until: 0.0,
            entered_down: false,
            selected: 0,
            panel: Rect::NOTHING,
            scroll_to_selected: false,
        }
    }
}

/// What the user asked the drawer to do this frame.
#[derive(Default)]
pub struct Out {
    pub open: Option<PathBuf>,
    pub new_note: bool,
    /// Show this file (or folder) in Explorer.
    pub reveal: Option<PathBuf>,
    pub delete: Option<PathBuf>,
    /// The strip just peeked: a good moment to rescan the folder.
    pub peeked: bool,
    /// Seconds until the drawer needs another frame.
    pub wake: Option<f64>,
}

impl Drawer {
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Open and stay open (Ctrl+E); again to close.
    pub fn toggle(&mut self) {
        if self.phase == Phase::Open && self.pinned {
            self.close();
        } else {
            self.phase = Phase::Open;
            self.pinned = true;
            self.away_since = None;
            self.scroll_to_selected = true;
        }
    }

    pub fn close(&mut self) {
        self.phase = Phase::Hidden;
        self.pinned = false;
        self.band_since = None;
        self.strip_since = None;
        self.away_since = None;
    }

    /// Opened with the keyboard: it owns ↑ ↓ Enter Delete Esc.
    pub fn has_keyboard(&self) -> bool {
        self.phase == Phase::Open && self.pinned
    }

    /// The state machine. Returns (seconds until it needs another frame, just peeked).
    pub fn update(&mut self, i: &Input) -> (Option<f64>, bool) {
        let now = i.now;
        if i.released || i.dragging {
            self.quiet_until = self.quiet_until.max(now + QUIET_AFTER_INPUT);
        }
        let c = i.content;
        let in_band = i
            .pointer
            .is_some_and(|p| p.x >= c.right() - i.trigger && p.y >= c.top() && p.y <= c.bottom());
        if !in_band {
            self.entered_down = false;
        } else if i.any_down && self.band_since.is_none() && self.phase == Phase::Hidden {
            self.entered_down = true;
        }
        let near_thumb = i.pointer.is_some_and(|p| {
            i.thumbs
                .iter()
                .any(|t| t.expand2(vec2(4.0, 8.0)).contains(p))
        });
        let suppressed = i.any_down
            || i.dragging
            || self.entered_down
            || now < self.quiet_until
            || near_thumb
            || i.blocked;

        let mut peeked = false;
        let wake = match self.phase {
            Phase::Hidden => {
                if in_band && !suppressed {
                    let since = *self.band_since.get_or_insert(now);
                    if now - since >= PEEK_DWELL {
                        self.phase = Phase::Peek;
                        self.band_since = None;
                        self.strip_since = None;
                        self.away_since = None;
                        peeked = true;
                        Some(0.0)
                    } else {
                        Some(since + PEEK_DWELL - now)
                    }
                } else {
                    self.band_since = None;
                    // Waiting out a quiet period with the pointer at the edge.
                    (in_band && now < self.quiet_until && !i.any_down)
                        .then_some(self.quiet_until - now)
                }
            }
            Phase::Peek => {
                let over_strip = i.pointer.is_some_and(|p| {
                    p.x >= c.right() - i.strip - 8.0 && p.y >= c.top() && p.y <= c.bottom()
                });
                if near_thumb || i.blocked {
                    self.close();
                    self.quiet_until = now + QUIET_AFTER_THUMB;
                    None
                } else if over_strip && !i.any_down {
                    self.away_since = None;
                    let since = *self.strip_since.get_or_insert(now);
                    if now - since >= OPEN_DWELL {
                        self.phase = Phase::Open;
                        self.strip_since = None;
                        Some(0.0)
                    } else {
                        Some(since + OPEN_DWELL - now)
                    }
                } else if over_strip {
                    self.strip_since = None;
                    None
                } else {
                    self.strip_since = None;
                    let since = *self.away_since.get_or_insert(now);
                    if now - since >= PEEK_LINGER {
                        self.close();
                        None
                    } else {
                        Some(since + PEEK_LINGER - now)
                    }
                }
            }
            Phase::Open => {
                if self.pinned || i.over_panel || i.menu_open {
                    self.away_since = None;
                    None
                } else {
                    let since = *self.away_since.get_or_insert(now);
                    if now - since >= OPEN_LINGER {
                        self.close();
                        None
                    } else {
                        Some(since + OPEN_LINGER - now)
                    }
                }
            }
        };
        (wake, peeked)
    }

    /// Keyboard navigation while pinned open. Call before the document's UI.
    pub fn keys(&mut self, ui: &mut egui::Ui, entries: &[LibraryEntry], out: &mut Out) {
        if !self.has_keyboard() {
            return;
        }
        let n = entries.len();
        let mut delete = false;
        let mut open = false;
        let mut close = false;
        ui.input_mut(|i| {
            let none = Modifiers::NONE;
            let mut step = |d: isize| {
                if n > 0 {
                    self.selected = (self.selected as isize + d).clamp(0, n as isize - 1) as usize;
                    self.scroll_to_selected = true;
                }
            };
            if consume_exact(i, none, Key::ArrowDown) {
                step(1);
            }
            if consume_exact(i, none, Key::ArrowUp) {
                step(-1);
            }
            if consume_exact(i, none, Key::PageDown) {
                step(8);
            }
            if consume_exact(i, none, Key::PageUp) {
                step(-8);
            }
            if consume_exact(i, none, Key::Home) {
                step(-(n as isize));
            }
            if consume_exact(i, none, Key::End) {
                step(n as isize);
            }
            open = consume_exact(i, none, Key::Enter);
            delete = consume_exact(i, none, Key::Delete);
            close = consume_exact(i, none, Key::Escape);
        });
        if let Some(e) = entries.get(self.selected) {
            if open {
                out.open = Some(e.path.clone());
                self.close();
            } else if delete {
                out.delete = Some(e.path.clone());
            }
        }
        if close {
            self.close();
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        content: Rect,
        lib: &LibrarySnapshot,
        library_dir: Option<&Path>,
        active: Option<&Path>,
        blocked: bool,
        theme: &Theme,
    ) -> Out {
        let m = theme.metrics;
        let mut out = Out::default();
        let activity = whir_core::scroll::activity(ctx);
        let menu_open = egui::Popup::is_any_open(ctx);
        let (pointer, any_down, released, pressed) = ctx.input(|i| {
            (
                i.pointer.hover_pos(),
                i.pointer.any_down(),
                i.pointer.any_released(),
                i.pointer.any_pressed(),
            )
        });
        let slack = if self.phase == Phase::Open { 16.0 } else { 8.0 };
        let over_panel = pointer.is_some_and(|p| self.panel.expand(slack).contains(p));
        // A click elsewhere closes an open drawer.
        if self.phase == Phase::Open && pressed && !over_panel && !menu_open {
            self.close();
        }
        let input = Input {
            now: ctx.input(|i| i.time),
            pointer,
            content,
            any_down,
            released,
            dragging: activity.dragging || ctx.dragged_id().is_some(),
            thumbs: activity.thumbs.clone(),
            blocked: blocked || content.width() < 480.0,
            over_panel,
            menu_open,
            trigger: m.drawer_trigger,
            strip: m.drawer_strip,
        };
        let (wake, peeked) = self.update(&input);
        out.wake = wake;
        out.peeked = peeked;

        // Slide: the panel is always laid out full width and moved in from the right.
        let id = Id::new("whir.drawer");
        let easing = egui::emath::easing::cubic_out;
        let p = ctx.animate_bool_with_time_and_easing(
            id.with("peek"),
            self.phase != Phase::Hidden,
            0.12,
            easing,
        );
        let open_time = if self.phase == Phase::Open {
            0.18
        } else {
            0.14
        };
        let o = ctx.animate_bool_with_time_and_easing(
            id.with("open"),
            self.phase == Phase::Open,
            open_time,
            easing,
        );
        let width = m.drawer_width.min(content.width() * 0.45).max(240.0);
        let visible = m.drawer_strip * p + (width - m.drawer_strip) * o;
        if visible < 0.5 {
            self.panel = Rect::NOTHING;
            return out;
        }
        let panel = Rect::from_min_size(
            pos2(content.right() - visible, content.top()),
            vec2(width, content.height()),
        );
        self.panel = Rect::from_min_max(panel.min, pos2(content.right(), panel.bottom()));

        Area::new(id)
            .order(Order::Foreground)
            .fixed_pos(panel.min)
            .constrain(false)
            .movable(false)
            .interactable(true)
            .show(ctx, |ui| {
                ui.set_clip_rect(self.panel.expand2(vec2(24.0, 0.0)));
                let painter = ui.painter();
                // Soft shadow on the left, then the panel.
                for k in 0..6 {
                    let a = (6 - k) as f32 / 6.0;
                    painter.rect_filled(
                        Rect::from_min_max(
                            pos2(panel.left() - k as f32 * 2.0 - 2.0, panel.top()),
                            pos2(panel.left(), panel.bottom()),
                        ),
                        0.0,
                        theme.drawer_shadow.gamma_multiply(a * 0.18 * p),
                    );
                }
                painter.rect_filled(panel, 0.0, theme.drawer_bg);
                painter.line_segment(
                    [panel.left_top(), panel.left_bottom()],
                    Stroke::new(1.0, theme.drawer_border),
                );
                // The spine (what peeks out): a grip in the middle.
                let spine = Rect::from_min_max(
                    panel.min,
                    pos2(panel.left() + m.drawer_strip, panel.bottom()),
                );
                let spine_resp = ui.interact(spine, id.with("spine"), Sense::CLICK);
                for d in [-6.0, 0.0, 6.0] {
                    painter.circle_filled(
                        pos2(spine.center().x, spine.center().y + d),
                        1.5,
                        theme.drawer_grip,
                    );
                }
                if self.phase == Phase::Peek {
                    let wheel = ui.input(|i| i.smooth_scroll_delta != egui::Vec2::ZERO);
                    if spine_resp.clicked() || (spine_resp.hovered() && wheel) {
                        self.phase = Phase::Open;
                    }
                    if spine_resp.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    }
                }

                let body = Rect::from_min_max(pos2(spine.right(), panel.top()), panel.max);
                let mut ui =
                    ui.new_child(egui::UiBuilder::new().max_rect(body.shrink2(vec2(10.0, 10.0))));
                self.contents(&mut ui, lib, library_dir, active, theme, &mut out);
            });
        out
    }

    fn contents(
        &mut self,
        ui: &mut egui::Ui,
        lib: &LibrarySnapshot,
        library_dir: Option<&Path>,
        active: Option<&Path>,
        theme: &Theme,
        out: &mut Out,
    ) {
        let m = theme.metrics;
        let entries = &lib.entries;
        // Header: "Notes 12" + new note + open folder.
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Notes").size(15.0).color(theme.text));
            if !entries.is_empty() {
                ui.label(
                    egui::RichText::new(entries.len().to_string())
                        .size(m.small_text_size)
                        .color(theme.text_dim),
                );
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(dir) = library_dir
                    && icon_button(ui, Icon::Folder, theme)
                        .on_hover_text("Open the notes folder")
                        .clicked()
                {
                    out.reveal = Some(dir.to_path_buf());
                }
                if icon_button(ui, Icon::Plus, theme)
                    .on_hover_text("New note (Ctrl+N)")
                    .clicked()
                {
                    out.new_note = true;
                    self.close();
                }
            });
        });
        ui.add_space(6.0);

        if entries.is_empty() {
            ui.add_space(24.0);
            if lib.scanning && !lib.scanned {
                ui.vertical_centered(|ui| ui.add(egui::Spinner::new().size(16.0)));
            } else if let Some(e) = &lib.error {
                ui.label(egui::RichText::new(e).color(theme.text_dim));
            } else {
                ui.vertical_centered(|ui| {
                    ui.label(egui::RichText::new("No notes yet").color(theme.text));
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(
                            "Notes you write are saved automatically to Documents\\whir.",
                        )
                        .size(m.small_text_size)
                        .color(theme.text_dim),
                    );
                    ui.add_space(10.0);
                    if ui.button("New note").clicked() {
                        out.new_note = true;
                        self.close();
                    }
                });
            }
            return;
        }
        self.selected = self.selected.min(entries.len() - 1);

        let now_sys = SystemTime::now();
        let now = whir_core::time::now_local();
        let row_h = m.drawer_row_h;
        let mut scroll = egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .id_salt("whir.drawer.list");
        if std::mem::take(&mut self.scroll_to_selected) && self.has_keyboard() {
            let target = self.selected as f32 * row_h;
            let view = ui.available_height();
            let off = ui
                .ctx()
                .data(|d| d.get_temp::<f32>(Id::new("whir.drawer.off")))
                .unwrap_or(0.0);
            let off = if target < off {
                target
            } else if target + row_h > off + view {
                target + row_h - view
            } else {
                off
            };
            scroll = scroll.vertical_scroll_offset(off);
        }
        let res = scroll.show_rows(ui, row_h, entries.len(), |ui, range| {
            for idx in range {
                let e = &entries[idx];
                let (rect, resp) =
                    ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::CLICK);
                let is_active = active.is_some_and(|a| whir_core::library::paths_equal(a, &e.path));
                let painter = ui.painter();
                if is_active {
                    painter.rect_filled(rect, 6.0, theme.drawer_item_active);
                } else if resp.hovered() || (self.has_keyboard() && idx == self.selected) {
                    painter.rect_filled(rect, 6.0, theme.drawer_item_hover);
                }
                let kind = match e.kind {
                    EntryKind::Note => DocKind::Note,
                    EntryKind::Table => DocKind::Csv,
                };
                paint_kind_glyph(
                    painter,
                    pos2(rect.left() + 14.0, rect.top() + 17.0),
                    kind,
                    theme,
                );
                let text_left = rect.left() + 30.0;
                let when = match (
                    whir_core::time::to_local(e.modified),
                    now_sys.duration_since(e.modified),
                ) {
                    (Some(then), Ok(age)) => {
                        reltime::label(&now, &then, age.as_secs() as i64, &SystemFormats)
                    }
                    (Some(then), Err(_)) => reltime::label(&now, &then, -1, &SystemFormats),
                    _ => String::new(),
                };
                let small = FontId::proportional(m.small_text_size);
                let when_g = painter.layout_no_wrap(when, small.clone(), theme.text_dim);
                let when_w = when_g.size().x;
                painter.galley(
                    pos2(rect.right() - 8.0 - when_w, rect.top() + 10.0),
                    when_g,
                    theme.text_dim,
                );
                whir_core::theme::note_text(&e.title);
                let title = elide(
                    painter,
                    &e.title,
                    FontId::proportional(13.5),
                    rect.right() - 16.0 - when_w - text_left,
                );
                painter.galley(pos2(text_left, rect.top() + 8.0), title, theme.text);
                if !e.preview.is_empty() {
                    whir_core::theme::note_text(&e.preview);
                    let prev = elide(painter, &e.preview, small, rect.right() - 8.0 - text_left);
                    painter.galley(pos2(text_left, rect.top() + 29.0), prev, theme.text_dim);
                }
                if e.cloud_only {
                    whir_core::theme::note_text("☁");
                    painter.text(
                        pos2(rect.right() - 8.0, rect.bottom() - 8.0),
                        Align2::RIGHT_BOTTOM,
                        "☁",
                        FontId::proportional(10.0),
                        theme.text_dim,
                    );
                }
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                if resp.clicked() {
                    out.open = Some(e.path.clone());
                    self.selected = idx;
                }
                let path = e.path.clone();
                let popup = egui::Popup::context_menu(&resp).id(Id::new(("whir.drawer.menu", idx)));
                popup.show(|ui| {
                    if ui.button("Open").clicked() {
                        out.open = Some(path.clone());
                        ui.close();
                    }
                    if ui.button("Show in Explorer").clicked() {
                        out.reveal = Some(path.clone());
                        ui.close();
                    }
                    ui.separator();
                    if ui
                        .button(egui::RichText::new("Delete").color(theme.danger))
                        .clicked()
                    {
                        out.delete = Some(path.clone());
                        ui.close();
                    }
                });
            }
        });
        ui.ctx()
            .data_mut(|d| d.insert_temp(Id::new("whir.drawer.off"), res.state.offset.y));
        if out.open.is_some() {
            self.close();
        }
    }
}

#[derive(Clone, Copy)]
enum Icon {
    Plus,
    Folder,
}

/// A small frameless icon button (drawn with lines: no font needs these glyphs).
fn icon_button(ui: &mut egui::Ui, icon: Icon, theme: &Theme) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::CLICK);
    let painter = ui.painter();
    if resp.hovered() {
        painter.rect_filled(rect, 5.0, theme.drawer_item_hover);
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let c = rect.center();
    let s = Stroke::new(1.3, theme.text_dim);
    match icon {
        Icon::Plus => {
            painter.line_segment([c - vec2(5.5, 0.0), c + vec2(5.5, 0.0)], s);
            painter.line_segment([c - vec2(0.0, 5.5), c + vec2(0.0, 5.5)], s);
        }
        Icon::Folder => {
            let r = Rect::from_center_size(c + vec2(0.0, 0.5), vec2(13.0, 10.0));
            let pts = [
                r.left_bottom(),
                r.left_top(),
                pos2(r.left() + 4.5, r.top()),
                pos2(r.left() + 6.0, r.top() + 1.8),
                pos2(r.right(), r.top() + 1.8),
                r.right_bottom(),
                r.left_bottom(),
            ];
            painter.add(egui::Shape::line(pts.to_vec(), s));
        }
    }
    resp
}

fn elide(
    painter: &egui::Painter,
    text: &str,
    font: FontId,
    max_width: f32,
) -> std::sync::Arc<egui::Galley> {
    use egui::text::{LayoutJob, TextFormat, TextWrapping};
    let mut job = LayoutJob::single_section(
        text.replace(['\n', '\r', '\t'], " "),
        TextFormat::simple(font, egui::Color32::PLACEHOLDER),
    );
    job.wrap = TextWrapping {
        max_width: max_width.max(10.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    painter.layout_job(job)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn content() -> Rect {
        Rect::from_min_max(pos2(0.0, 36.0), pos2(1000.0, 700.0))
    }

    fn at(now: f64, x: f32) -> Input {
        Input {
            now,
            pointer: Some(pos2(x, 300.0)),
            content: content(),
            trigger: 6.0,
            strip: 14.0,
            ..Default::default()
        }
    }

    #[test]
    fn peeks_after_dwell_then_opens() {
        let mut d = Drawer::default();
        let (wake, _) = d.update(&at(0.0, 998.0));
        assert_eq!(d.phase(), Phase::Hidden);
        assert!((wake.unwrap() - PEEK_DWELL).abs() < 1e-9);
        let (_, peeked) = d.update(&at(0.2, 998.0));
        assert!(peeked);
        assert_eq!(d.phase(), Phase::Peek);
        d.update(&at(0.25, 995.0));
        assert_eq!(d.phase(), Phase::Peek);
        d.update(&at(0.55, 995.0));
        assert_eq!(d.phase(), Phase::Open);
        // Leaving the open drawer closes it after a moment.
        let mut i = at(1.0, 400.0);
        d.update(&i);
        assert_eq!(d.phase(), Phase::Open);
        i.now = 1.4;
        d.update(&i);
        assert_eq!(d.phase(), Phase::Hidden);
    }

    #[test]
    fn stays_out_of_the_way_of_scrollbars_and_drags() {
        let mut d = Drawer::default();
        // Button held: a drag, never a reach.
        let mut i = at(0.0, 998.0);
        i.any_down = true;
        d.update(&i);
        i.any_down = false;
        i.now = 0.5;
        d.update(&i);
        assert_eq!(d.phase(), Phase::Hidden, "entered with a button down");
        // Leave and come back: now it peeks.
        d.update(&at(0.6, 500.0));
        d.update(&at(0.7, 998.0));
        d.update(&at(0.9, 998.0));
        assert_eq!(d.phase(), Phase::Peek);

        // A scrollbar thumb under the pointer: back off immediately.
        let mut i = at(1.0, 996.0);
        i.thumbs = vec![Rect::from_min_max(pos2(990.0, 250.0), pos2(998.0, 350.0))];
        d.update(&i);
        assert_eq!(d.phase(), Phase::Hidden);
        // ...and stay quiet for a moment even at the bare edge.
        d.update(&at(1.2, 998.0));
        d.update(&at(1.4, 998.0));
        assert_eq!(d.phase(), Phase::Hidden);
        d.update(&at(1.7, 998.0));
        d.update(&at(1.9, 998.0));
        assert_eq!(d.phase(), Phase::Peek);

        // Just after a click / scrollbar drag: quiet.
        let mut d = Drawer::default();
        let mut i = at(0.0, 998.0);
        i.released = true;
        d.update(&i);
        d.update(&at(0.3, 998.0));
        assert_eq!(d.phase(), Phase::Hidden);
    }

    #[test]
    fn pinned_ignores_hover_out() {
        let mut d = Drawer::default();
        d.toggle();
        assert_eq!(d.phase(), Phase::Open);
        d.update(&at(0.0, 100.0));
        d.update(&at(5.0, 100.0));
        assert_eq!(d.phase(), Phase::Open);
        d.toggle();
        assert_eq!(d.phase(), Phase::Hidden);
    }

    #[test]
    fn blocked_never_peeks() {
        let mut d = Drawer::default();
        let mut i = at(0.0, 999.0);
        i.blocked = true;
        d.update(&i);
        i.now = 1.0;
        d.update(&i);
        assert_eq!(d.phase(), Phase::Hidden);
    }
}
