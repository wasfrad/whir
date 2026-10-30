//! The drawer along the bottom of the window: what's near the open document.
//! A screenshot's tab has a filmstrip of the screenshots, newest first; a
//! note's (or a table's in the notes folder) has every note, todos first.
//! Other images and PDFs have none, though Ctrl+E brings up the notes there
//! too, until the tab changes.
//!
//! It folds down to its header and is dragged taller or shorter by its top
//! edge: a taller filmstrip has bigger pictures, a taller notes list more
//! rows. Both are remembered, for each kind.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use egui::{
    Align2, Color32, CursorIcon, FontId, Frame, Id, Key, Modifiers, Panel, Rect, Sense, Stroke,
    pos2, vec2,
};
use whir_core::captures::{CaptureEntry, CapturesSnapshot};
use whir_core::library::{EntryKind, LibraryEntry, LibrarySnapshot};
use whir_core::marks::{self, Kind};
use whir_core::theme::Theme;
use whir_core::{DocKind, OpenCx, settings};
use whir_image::{Thumb, Thumbs};

use crate::keys::consume_exact;
use crate::reltime::{self, SystemFormats};
use crate::tab_bar::paint_kind_glyph;

/// Which drawer a tab has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Every note and table in the notes folder.
    Notes,
    /// The screenshots, as a filmstrip.
    Shots,
}

impl Mode {
    fn key(self) -> &'static str {
        match self {
            Mode::Notes => "notes",
            Mode::Shots => "shots",
        }
    }
}

/// The drawer for a tab showing `kind` (from `path`): screenshots for a
/// capture, notes for a note or a table in the notes folder, else none.
pub fn mode_for(kind: DocKind, path: Option<&Path>, cx: &OpenCx) -> Option<Mode> {
    match kind {
        DocKind::Image if path.is_some_and(|p| cx.captures.contains(p)) => Some(Mode::Shots),
        DocKind::Note => Some(Mode::Notes),
        DocKind::Csv if path.is_some_and(|p| cx.library.contains(p)) => Some(Mode::Notes),
        _ => None,
    }
}

/// Folded or not, and how tall, for one kind of drawer.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Pane {
    open: bool,
    height: f32,
    /// The keyboard's place in the list.
    selected: usize,
}

const MIN_HEIGHT: f32 = 96.0;
const DEFAULT_HEIGHT: f32 = 170.0;
/// Notes are laid out in columns at least this wide.
const NOTE_COLUMN: f32 = 280.0;
/// A filmstrip picture's box is this wide for its height.
const SHOT_ASPECT: f32 = 1.6;
const SHOT_LABEL_H: f32 = 20.0;
const SHOT_GAP: f32 = 10.0;

pub struct Drawer {
    panes: [Pane; 2],
    /// Ctrl+E on a tab without a drawer: its notes drawer, while that tab
    /// stays active.
    summoned: Option<u64>,
    /// Ctrl+E: arrows, Enter, Delete and Esc are the drawer's (while the
    /// tab it was pressed on stays the active one).
    focused: bool,
    focus_tab: Option<u64>,
    /// The active document last frame (its picture is scrolled into view).
    last_active: Option<PathBuf>,
    scroll_to: bool,
    /// Made the first time the filmstrip is shown.
    thumbs: Option<Thumbs>,
    /// The mode shown last frame (a new one may want its folder rescanned).
    shown: Option<Mode>,
}

/// What the user asked the drawer to do this frame.
#[derive(Default)]
pub struct Out {
    pub open: Option<PathBuf>,
    pub new_note: bool,
    /// Show this file (or folder) in Explorer.
    pub reveal: Option<PathBuf>,
    pub delete: Option<PathBuf>,
    /// Open the capture dock.
    pub capture: bool,
    /// A drawer came into view: rescan its folder if the list is old.
    pub rescan: Option<Mode>,
}

fn load_pane(mode: Mode) -> Pane {
    let key = |k: &str| format!("drawer.{}.{k}", mode.key());
    Pane {
        open: settings::get_bool(&key("open"), true),
        height: settings::get_str(&key("height"))
            .and_then(|h| h.parse::<f32>().ok())
            .filter(|h| h.is_finite())
            .unwrap_or(DEFAULT_HEIGHT)
            .max(MIN_HEIGHT),
        selected: 0,
    }
}

impl Default for Drawer {
    fn default() -> Self {
        Self {
            panes: [load_pane(Mode::Notes), load_pane(Mode::Shots)],
            summoned: None,
            focused: false,
            focus_tab: None,
            last_active: None,
            scroll_to: false,
            thumbs: None,
            shown: None,
        }
    }
}

impl Drawer {
    fn pane(&mut self, mode: Mode) -> &mut Pane {
        &mut self.panes[mode as usize]
    }

    fn set_open(&mut self, mode: Mode, open: bool) {
        if self.pane(mode).open != open {
            self.pane(mode).open = open;
            settings::set_bool(&format!("drawer.{}.open", mode.key()), open);
        }
    }

    /// The drawer the active tab has, if any (summoned ones included).
    pub fn mode(&mut self, tab: Option<u64>, own: Option<Mode>) -> Option<Mode> {
        if self.summoned.is_some() && self.summoned != tab {
            self.summoned = None;
        }
        if self.focus_tab != tab {
            self.focused = false;
        }
        own.or(self.summoned.map(|_| Mode::Notes))
    }

    /// Ctrl+E: unfold the drawer and take the keyboard; again to give it
    /// back and fold it. A tab without a drawer gets the notes.
    pub fn toggle(&mut self, tab: Option<u64>, own: Option<Mode>) {
        if self.focus_tab != tab {
            self.focus_tab = tab;
            self.focused = false;
        }
        let mode = match own {
            Some(m) => m,
            None if self.summoned.is_some() && self.summoned == tab => {
                self.summoned = None;
                self.focused = false;
                return;
            }
            None => {
                self.summoned = tab;
                Mode::Notes
            }
        };
        if self.focused && self.pane(mode).open {
            self.focused = false;
            self.set_open(mode, false);
        } else {
            self.focused = true;
            self.scroll_to = true;
            self.set_open(mode, true);
        }
    }

    /// Keyboard, while the drawer has it. Call before the document's UI, with
    /// the drawer shown, the length of its list and the path of item `i`.
    pub fn keys(
        &mut self,
        ui: &mut egui::Ui,
        mode: Option<Mode>,
        n: usize,
        path_at: impl Fn(usize) -> Option<PathBuf>,
        out: &mut Out,
    ) {
        let Some(mode) = mode.filter(|_| self.focused) else {
            self.focused = false;
            return;
        };
        let columns = ui
            .ctx()
            .data(|d| d.get_temp::<usize>(Id::new("whir.drawer.columns")))
            .unwrap_or(1);
        let (mut open, mut delete, mut close, mut step) = (false, false, false, 0isize);
        ui.input_mut(|i| {
            let none = Modifiers::NONE;
            let (across, down) = match mode {
                Mode::Shots => (1, 1),
                Mode::Notes => (1, columns as isize),
            };
            for (key, d) in [
                (Key::ArrowLeft, -across),
                (Key::ArrowRight, across),
                (Key::ArrowUp, -down),
                (Key::ArrowDown, down),
                (Key::PageUp, -8 * down),
                (Key::PageDown, 8 * down),
                (Key::Home, -(n as isize)),
                (Key::End, n as isize),
            ] {
                if consume_exact(i, none, key) {
                    step += d;
                }
            }
            open = consume_exact(i, none, Key::Enter);
            delete = consume_exact(i, none, Key::Delete);
            close = consume_exact(i, none, Key::Escape);
        });
        if n > 0 && step != 0 {
            let pane = self.pane(mode);
            pane.selected = (pane.selected as isize + step).clamp(0, n as isize - 1) as usize;
            self.scroll_to = true;
        }
        let selected = self.pane(mode).selected;
        if let Some(path) = path_at(selected) {
            if open {
                out.open = Some(path);
                self.focused = false;
            } else if delete {
                out.delete = Some(path);
            }
        }
        if close {
            self.focused = false;
            if self.summoned.is_some() {
                self.summoned = None;
            }
        }
    }

    /// The drawer for `mode` along the bottom of `ui` (before the document
    /// takes what is left).
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        mode: Option<Mode>,
        lib: &LibrarySnapshot,
        shots: &CapturesSnapshot,
        cx: &OpenCx,
        active: Option<&Path>,
        theme: &Theme,
    ) -> Out {
        let mut out = Out::default();
        if mode != self.shown {
            self.shown = mode;
            out.rescan = mode;
        }
        let Some(mode) = mode else {
            self.focused = false;
            return out;
        };
        if self.last_active.as_deref() != active {
            self.last_active = active.map(Path::to_path_buf);
            self.scroll_to = true;
            // The keyboard starts from the document that's open.
            let at = match mode {
                Mode::Notes => lib.entries.iter().position(|e| is(active, &e.path)),
                Mode::Shots => shots.entries.iter().position(|e| is(active, &e.path)),
            };
            if let Some(at) = at {
                self.pane(mode).selected = at;
            }
        }
        let header_h = theme.metrics.drawer_header_h;
        let pane = *self.pane(mode);
        let frame = Frame::NONE.fill(theme.drawer_bg);
        let id = Id::new(("whir.drawer", mode.key()));
        let mut toggle = false;
        let drawn = if !pane.open {
            Panel::bottom(id.with("folded"))
                .exact_size(header_h)
                .resizable(false)
                .show_separator_line(false)
                .frame(frame)
                .show(ui, |ui| {
                    toggle = self.header(ui, mode, lib, shots, cx, false, theme, &mut out);
                })
                .response
                .rect
        } else {
            let max = (ui.available_height() - 140.0).max(MIN_HEIGHT);
            let shown = Panel::bottom(id)
                .resizable(true)
                .show_separator_line(false)
                .default_size(pane.height.min(max))
                .size_range(MIN_HEIGHT..=max)
                .frame(frame)
                .show(ui, |ui| {
                    ui.expand_to_include_rect(ui.max_rect());
                    let all = ui.max_rect();
                    let head = Rect::from_min_size(all.min, vec2(all.width(), header_h));
                    let mut head_ui = ui.new_child(egui::UiBuilder::new().max_rect(head));
                    toggle = self.header(&mut head_ui, mode, lib, shots, cx, true, theme, &mut out);
                    let body = Rect::from_min_max(pos2(all.left(), head.bottom()), all.max);
                    let mut body_ui = ui
                        .new_child(egui::UiBuilder::new().max_rect(body.shrink2(vec2(10.0, 4.0))));
                    match mode {
                        Mode::Notes => self.notes(&mut body_ui, lib, active, theme, &mut out),
                        Mode::Shots => {
                            self.filmstrip(&mut body_ui, shots, cx, active, theme, &mut out)
                        }
                    }
                });
            // Dragged taller or shorter: remembered once it's let go.
            let height = shown.response.rect.height();
            let released = ui.input(|i| !i.pointer.any_down());
            if (height - pane.height).abs() > 0.5 && released {
                self.pane(mode).height = height;
                settings::set_str(
                    &format!("drawer.{}.height", mode.key()),
                    &format!("{height:.0}"),
                );
            }
            shown.response.rect
        };
        if toggle {
            self.set_open(mode, !pane.open);
            self.scroll_to = !pane.open;
        }
        // A click elsewhere gives the keyboard back to the document.
        let pressed_at = ui.input(|i| {
            i.pointer
                .any_pressed()
                .then(|| i.pointer.interact_pos())
                .flatten()
        });
        if pressed_at.is_some_and(|p| !drawn.contains(p)) {
            self.focused = false;
        }
        if out.open.is_some() {
            self.focused = false;
        }
        out
    }

    /// The header: what's listed and how many, the drawer's buttons, and a
    /// fold toggle. Returns whether it was clicked to fold or unfold.
    #[allow(clippy::too_many_arguments)]
    fn header(
        &mut self,
        ui: &mut egui::Ui,
        mode: Mode,
        lib: &LibrarySnapshot,
        shots: &CapturesSnapshot,
        cx: &OpenCx,
        open: bool,
        theme: &Theme,
        out: &mut Out,
    ) -> bool {
        let rect = ui.max_rect();
        let painter = ui.painter();
        painter.line_segment(
            [rect.left_top(), rect.right_top()],
            Stroke::new(1.0, theme.separator),
        );
        let resp = ui.interact(rect, Id::new("whir.drawer.header"), Sense::CLICK);
        if resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        let (title, count) = match mode {
            Mode::Notes => ("Notes", lib.entries.len()),
            Mode::Shots => ("Screenshots", shots.entries.len()),
        };
        // ▸ folded, ▾ open.
        let c = pos2(rect.left() + 16.0, rect.center().y);
        let s = Stroke::new(1.3, theme.text_dim);
        let pts = if open {
            vec![
                c + vec2(-4.0, -2.0),
                c + vec2(0.0, 2.5),
                c + vec2(4.0, -2.0),
            ]
        } else {
            vec![
                c + vec2(-2.0, -4.0),
                c + vec2(2.5, 0.0),
                c + vec2(-2.0, 4.0),
            ]
        };
        painter.add(egui::Shape::line(pts, s));
        let name = painter.layout_no_wrap(title.to_owned(), FontId::proportional(13.5), theme.text);
        let name_w = name.size().x;
        painter.galley(
            pos2(rect.left() + 28.0, rect.center().y - name.size().y / 2.0),
            name,
            theme.text,
        );
        if count > 0 {
            painter.text(
                pos2(rect.left() + 34.0 + name_w, rect.center().y),
                Align2::LEFT_CENTER,
                count.to_string(),
                FontId::proportional(theme.metrics.small_text_size),
                theme.text_dim,
            );
        }
        if self.focused {
            painter.text(
                pos2(rect.left() + 60.0 + name_w, rect.center().y),
                Align2::LEFT_CENTER,
                "↑ ↓ ← → · Enter · Delete · Esc",
                FontId::proportional(theme.metrics.small_text_size),
                theme.text_dim,
            );
        }
        let mut buttons = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rect.shrink2(vec2(10.0, 0.0)))
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        let mut clicked_button = false;
        match mode {
            Mode::Notes => {
                if let Some(dir) = cx.library.dir()
                    && icon_button(&mut buttons, Icon::Folder, theme)
                        .on_hover_text("Open the notes folder")
                        .clicked()
                {
                    out.reveal = Some(dir.to_path_buf());
                    clicked_button = true;
                }
                if icon_button(&mut buttons, Icon::Plus, theme)
                    .on_hover_text("New note (Ctrl+N)")
                    .clicked()
                {
                    out.new_note = true;
                    clicked_button = true;
                }
            }
            Mode::Shots => {
                if let Some(dir) = cx.captures.dir()
                    && icon_button(&mut buttons, Icon::Folder, theme)
                        .on_hover_text("Open the screenshots folder")
                        .clicked()
                {
                    out.reveal = Some(dir.to_path_buf());
                    clicked_button = true;
                }
                if icon_button(&mut buttons, Icon::Camera, theme)
                    .on_hover_text("Open the capture dock")
                    .clicked()
                {
                    out.capture = true;
                    clicked_button = true;
                }
            }
        }
        resp.clicked() && !clicked_button
    }

    fn notes(
        &mut self,
        ui: &mut egui::Ui,
        lib: &LibrarySnapshot,
        active: Option<&Path>,
        theme: &Theme,
        out: &mut Out,
    ) {
        let entries = &lib.entries;
        if entries.is_empty() {
            empty(
                ui,
                lib.scanning && !lib.scanned,
                lib.error.as_deref(),
                theme,
                |ui| {
                    ui.label(egui::RichText::new("No notes yet").color(theme.text));
                    if ui.button("New note").clicked() {
                        out.new_note = true;
                    }
                },
            );
            return;
        }
        let row_h = theme.metrics.drawer_row_h;
        let columns = ((ui.available_width() / NOTE_COLUMN).floor() as usize).max(1);
        ui.ctx()
            .data_mut(|d| d.insert_temp(Id::new("whir.drawer.columns"), columns));
        let rows = entries.len().div_ceil(columns);
        let selected = self.pane(Mode::Notes).selected.min(entries.len() - 1);
        self.pane(Mode::Notes).selected = selected;
        let column_w = ui.available_width() / columns as f32;
        let mut scroll = egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .id_salt("whir.drawer.notes");
        if std::mem::take(&mut self.scroll_to) {
            let target = (selected / columns) as f32 * row_h;
            let view = ui.available_height();
            let off_id = Id::new("whir.drawer.notes.off");
            let off = ui.ctx().data(|d| d.get_temp::<f32>(off_id)).unwrap_or(0.0);
            scroll = scroll.vertical_scroll_offset(if target < off {
                target
            } else if target + row_h > off + view {
                target + row_h - view
            } else {
                off
            });
        }
        let now_sys = SystemTime::now();
        let now = whir_core::time::now_local();
        let focused = self.focused;
        let res = scroll.show_rows(ui, row_h, rows, |ui, range| {
            for row in range {
                let top = ui.cursor().top();
                ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::hover());
                for col in 0..columns {
                    let idx = row * columns + col;
                    let Some(e) = entries.get(idx) else { break };
                    let left = ui.max_rect().left() + col as f32 * column_w;
                    let rect =
                        Rect::from_min_size(pos2(left, top), vec2(column_w - 6.0, row_h - 2.0));
                    let on = focused && idx == selected;
                    if note_row(
                        ui,
                        rect,
                        e,
                        is(active, &e.path),
                        on,
                        &now,
                        now_sys,
                        theme,
                        out,
                    ) {
                        self.pane(Mode::Notes).selected = idx;
                    }
                }
            }
        });
        ui.ctx()
            .data_mut(|d| d.insert_temp(Id::new("whir.drawer.notes.off"), res.state.offset.y));
    }

    /// The screenshots side by side, as tall as the drawer.
    fn filmstrip(
        &mut self,
        ui: &mut egui::Ui,
        shots: &CapturesSnapshot,
        cx: &OpenCx,
        active: Option<&Path>,
        theme: &Theme,
        out: &mut Out,
    ) {
        let entries = &shots.entries;
        if entries.is_empty() {
            empty(
                ui,
                shots.scanning && !shots.scanned,
                shots.error.as_deref(),
                theme,
                |ui| {
                    ui.label(egui::RichText::new("No screenshots yet").color(theme.text));
                    if ui.button("Capture…").clicked() {
                        out.capture = true;
                    }
                },
            );
            return;
        }
        let selected = self.pane(Mode::Shots).selected.min(entries.len() - 1);
        self.pane(Mode::Shots).selected = selected;
        let thumb_h = (ui.available_height() - SHOT_LABEL_H - 12.0).max(40.0);
        let card_w = thumb_h * SHOT_ASPECT;
        let step = card_w + SHOT_GAP;
        let area = ui.max_rect();
        // The wheel turns the strip (it only goes sideways).
        if ui.rect_contains_pointer(area) {
            ui.input_mut(|i| {
                if i.smooth_scroll_delta.x == 0.0 {
                    i.smooth_scroll_delta.x = i.smooth_scroll_delta.y;
                    i.smooth_scroll_delta.y = 0.0;
                }
            });
        }
        let off_id = Id::new("whir.drawer.shots.off");
        let mut scroll = egui::ScrollArea::horizontal()
            .auto_shrink([false, false])
            .id_salt("whir.drawer.shots");
        if std::mem::take(&mut self.scroll_to) {
            let target = selected as f32 * step;
            let view = area.width();
            let off = ui.ctx().data(|d| d.get_temp::<f32>(off_id)).unwrap_or(0.0);
            scroll = scroll.horizontal_scroll_offset(if target < off {
                target
            } else if target + card_w > off + view {
                target + card_w - view
            } else {
                off
            });
        }
        let thumbs = self
            .thumbs
            .get_or_insert_with(|| Thumbs::new(cx.textures.clone(), cx.waker.clone()));
        thumbs.begin_frame();
        let now_sys = SystemTime::now();
        let now = whir_core::time::now_local();
        let focused = self.focused;
        let mut clicked = None;
        let res = scroll.show_viewport(ui, |ui, viewport| {
            let total = vec2(entries.len() as f32 * step, ui.available_height());
            let (strip, _) = ui.allocate_exact_size(total, Sense::hover());
            let first = (viewport.left() / step).floor().max(0.0) as usize;
            let last = ((viewport.right() / step).ceil() as usize).min(entries.len());
            for idx in first..last {
                let e = &entries[idx];
                let left = strip.left() + idx as f32 * step;
                let card = Rect::from_min_size(
                    pos2(left, strip.top() + 4.0),
                    vec2(card_w, thumb_h + SHOT_LABEL_H),
                );
                let on = focused && idx == selected;
                if shot_card(
                    ui,
                    card,
                    thumb_h,
                    e,
                    thumbs,
                    is(active, &e.path),
                    on,
                    &now,
                    now_sys,
                    theme,
                    out,
                ) {
                    clicked = Some(idx);
                }
            }
        });
        thumbs.end_frame();
        ui.ctx()
            .data_mut(|d| d.insert_temp(off_id, res.state.offset.x));
        if let Some(idx) = clicked {
            self.pane(Mode::Shots).selected = idx;
        }
    }
}

fn is(active: Option<&Path>, path: &Path) -> bool {
    active.is_some_and(|a| whir_core::library::paths_equal(a, path))
}

/// An empty list: still reading it, why it can't be read, or what to do.
fn empty(
    ui: &mut egui::Ui,
    scanning: bool,
    error: Option<&str>,
    theme: &Theme,
    what_to_do: impl FnOnce(&mut egui::Ui),
) {
    ui.add_space(12.0);
    ui.vertical_centered(|ui| {
        if scanning {
            ui.add(egui::Spinner::new().size(16.0));
        } else if let Some(e) = error {
            ui.label(egui::RichText::new(e).color(theme.text_dim));
        } else {
            what_to_do(ui);
        }
    });
}

fn when(now: &whir_core::time::LocalTime, now_sys: SystemTime, t: SystemTime) -> String {
    match (whir_core::time::to_local(t), now_sys.duration_since(t)) {
        (Some(then), Ok(age)) => reltime::label(now, &then, age.as_secs() as i64, &SystemFormats),
        (Some(then), Err(_)) => reltime::label(now, &then, -1, &SystemFormats),
        _ => String::new(),
    }
}

/// One note in the list: its kind, title and age. True if clicked.
#[allow(clippy::too_many_arguments)]
fn note_row(
    ui: &mut egui::Ui,
    rect: Rect,
    e: &LibraryEntry,
    active: bool,
    selected: bool,
    now: &whir_core::time::LocalTime,
    now_sys: SystemTime,
    theme: &Theme,
    out: &mut Out,
) -> bool {
    let id = Id::new(("whir.drawer.note", &e.path));
    let resp = ui.interact(rect, id, Sense::CLICK);
    let painter = ui.painter();
    if active {
        painter.rect_filled(rect, 6.0, theme.drawer_item_active);
    } else if resp.hovered() || selected {
        painter.rect_filled(rect, 6.0, theme.drawer_item_hover);
    }
    if selected {
        painter.rect_stroke(
            rect,
            6.0,
            Stroke::new(1.0, theme.accent),
            egui::StrokeKind::Inside,
        );
    }
    let kind = match e.kind {
        EntryKind::Note => DocKind::Note,
        EntryKind::Table => DocKind::Csv,
    };
    paint_kind_glyph(
        painter,
        pos2(rect.left() + 14.0, rect.center().y),
        kind,
        theme,
    );
    let small = FontId::proportional(theme.metrics.small_text_size);
    let age = painter.layout_no_wrap(when(now, now_sys, e.modified), small, theme.text_dim);
    let age_w = age.size().x;
    painter.galley(
        pos2(
            rect.right() - 8.0 - age_w,
            rect.center().y - age.size().y / 2.0,
        ),
        age,
        theme.text_dim,
    );
    let left = rect.left() + 30.0;
    whir_core::theme::note_text(&e.title);
    let title = elide(
        painter,
        &e.title,
        FontId::proportional(13.5),
        rect.right() - 16.0 - age_w - left,
    );
    painter.galley(
        pos2(left, rect.center().y - title.size().y / 2.0),
        title,
        theme.text,
    );
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        if !e.preview.is_empty() {
            let preview = e.preview.clone();
            resp.clone().on_hover_text_at_pointer(preview);
        }
    }
    let clicked = resp.clicked();
    if clicked {
        out.open = Some(e.path.clone());
    }
    row_menu(&resp, id.with("menu"), &e.path, theme, out);
    clicked
}

/// One screenshot in the filmstrip: its picture with its highlights, and
/// when it was taken. True if clicked.
#[allow(clippy::too_many_arguments)]
fn shot_card(
    ui: &mut egui::Ui,
    card: Rect,
    thumb_h: f32,
    e: &CaptureEntry,
    thumbs: &mut Thumbs,
    active: bool,
    selected: bool,
    now: &whir_core::time::LocalTime,
    now_sys: SystemTime,
    theme: &Theme,
    out: &mut Out,
) -> bool {
    let id = Id::new(("whir.drawer.shot", &e.path));
    let resp = ui.interact(card, id, Sense::CLICK);
    let painter = ui.painter();
    let frame = Rect::from_min_size(card.min, vec2(card.width(), thumb_h));
    painter.rect_filled(frame, 5.0, theme.canvas_bg);
    if e.cloud_only {
        // Not downloaded: showing it would download it.
        whir_core::theme::note_text("☁");
        painter.text(
            frame.center(),
            Align2::CENTER_CENTER,
            "☁",
            FontId::proportional(18.0),
            theme.text_dim,
        );
    } else if let Thumb::Ready {
        texture,
        size,
        full_size,
    } = thumbs.get(&e.path, (e.modified, e.size))
    {
        let size = vec2(size[0] as f32, size[1] as f32);
        let scale = ((frame.width() - 4.0) / size.x).min((frame.height() - 4.0) / size.y);
        let image = Rect::from_center_size(frame.center(), size * scale);
        let whole = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        painter.image(texture, image, whole, Color32::WHITE);
        if let Some(set) = &e.marks
            && set.size == full_size
        {
            let per_pixel = image.width() / full_size[0] as f32;
            let on_thumb = painter.with_clip_rect(image);
            for mark in &set.marks {
                let min = image.min + vec2(mark.x as f32, mark.y as f32) * per_pixel;
                let size = vec2(mark.w as f32, mark.h as f32) * per_pixel;
                let r = Rect::from_min_size(min, size);
                match &mark.kind {
                    Kind::Highlight(ink) => marks::paint(&on_thumb, r, *ink),
                    Kind::Text(t) => {
                        // To the half point: every thumbnail's own size of
                        // text would crowd the font atlas.
                        let points = t.size as f32 * per_pixel;
                        let points = ((points * 2.0).round() / 2.0).max(0.5);
                        let scale = points / t.size as f32;
                        // The thumbnail is the image unturned.
                        let turns = (4 - t.turn % 4) % 4;
                        marks::paint_text(&on_thumb, r, t, turns, scale);
                    }
                }
            }
        }
    }
    let outline = if active {
        Some(Stroke::new(2.0, theme.accent))
    } else if selected {
        Some(Stroke::new(1.0, theme.accent))
    } else if resp.hovered() {
        Some(Stroke::new(1.0, theme.text_dim))
    } else {
        None
    };
    if let Some(s) = outline {
        painter.rect_stroke(frame, 5.0, s, egui::StrokeKind::Outside);
    }
    let label = when(now, now_sys, e.created);
    let label = if label.is_empty() {
        e.name.clone()
    } else {
        label
    };
    let small = FontId::proportional(theme.metrics.small_text_size);
    let text = elide(painter, &label, small, card.width());
    painter.galley(
        pos2(card.left(), frame.bottom() + 4.0),
        text,
        if active { theme.text } else { theme.text_dim },
    );
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let clicked = resp.clicked();
    if clicked {
        out.open = Some(e.path.clone());
    }
    row_menu(&resp, id.with("menu"), &e.path, theme, out);
    clicked
}

/// Right-click on a note or a screenshot.
fn row_menu(resp: &egui::Response, id: Id, path: &Path, theme: &Theme, out: &mut Out) {
    egui::Popup::context_menu(resp).id(id).show(|ui| {
        if ui.button("Open").clicked() {
            out.open = Some(path.to_owned());
            ui.close();
        }
        if ui.button("Show in Explorer").clicked() {
            out.reveal = Some(path.to_owned());
            ui.close();
        }
        ui.separator();
        if ui
            .button(egui::RichText::new("Delete").color(theme.danger))
            .clicked()
        {
            out.delete = Some(path.to_owned());
            ui.close();
        }
    });
}

#[derive(Clone, Copy)]
enum Icon {
    Plus,
    Folder,
    Camera,
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
        Icon::Camera => {
            let body = Rect::from_center_size(c + vec2(0.0, 1.0), vec2(14.0, 9.5));
            painter.rect_stroke(body, 2.0, s, egui::StrokeKind::Middle);
            let hump = [
                c + vec2(-3.0, -3.75),
                c + vec2(-1.75, -6.0),
                c + vec2(1.75, -6.0),
                c + vec2(3.0, -3.75),
            ];
            painter.add(egui::Shape::line(hump.to_vec(), s));
            painter.circle_stroke(c + vec2(0.0, 1.0), 2.5, s);
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

    fn cx(dir: &Path) -> OpenCx {
        struct NoTextures;
        impl whir_core::TextureRegistry for NoTextures {
            fn create(
                self: std::sync::Arc<Self>,
                _: &whir_core::TextureDesc,
                _: &[u8],
                _: usize,
            ) -> Result<whir_core::UserTexture, String> {
                Err("none".into())
            }
            fn max_texture_side(&self) -> u32 {
                4096
            }
            fn release(&self, _: u64) {}
        }
        let waker = whir_core::Waker::noop();
        OpenCx {
            textures: std::sync::Arc::new(NoTextures),
            library: whir_core::Library::at(dir.to_owned(), waker.clone()),
            captures: whir_core::Captures::at(dir.join("Screenshots"), waker.clone()),
            waker,
        }
    }

    #[test]
    fn each_kind_of_tab_has_its_drawer() {
        let dir = PathBuf::from(r"C:\notes");
        let cx = cx(&dir);
        let shot = dir.join("Screenshots").join("Screenshot 1.png");
        let photo = PathBuf::from(r"C:\photos\cat.png");
        let table = dir.join("budget.csv");
        let elsewhere = PathBuf::from(r"C:\data\rack.csv");
        assert_eq!(
            mode_for(DocKind::Image, Some(&shot), &cx),
            Some(Mode::Shots)
        );
        assert_eq!(mode_for(DocKind::Image, Some(&photo), &cx), None);
        assert_eq!(mode_for(DocKind::Note, None, &cx), Some(Mode::Notes));
        assert_eq!(mode_for(DocKind::Csv, Some(&table), &cx), Some(Mode::Notes));
        assert_eq!(mode_for(DocKind::Csv, Some(&elsewhere), &cx), None);
        assert_eq!(mode_for(DocKind::Pdf, Some(&photo), &cx), None);
        assert_eq!(mode_for(DocKind::Settings, None, &cx), None);
    }

    #[test]
    fn ctrl_e_summons_the_notes_only_while_the_tab_stays() {
        whir_core::settings::memory_only();
        let mut d = Drawer::default();
        // A PDF's tab (id 7): no drawer of its own.
        assert_eq!(d.mode(Some(7), None), None);
        d.toggle(Some(7), None);
        assert_eq!(d.mode(Some(7), None), Some(Mode::Notes));
        assert!(d.focused);
        // Another tab: gone.
        assert_eq!(d.mode(Some(8), None), None);
        assert_eq!(d.mode(Some(7), None), None);
        // A note's tab: Ctrl+E takes the keyboard, then folds it away.
        d.toggle(Some(9), Some(Mode::Notes));
        assert!(d.focused && d.panes[Mode::Notes as usize].open);
        d.toggle(Some(9), Some(Mode::Notes));
        assert!(!d.focused && !d.panes[Mode::Notes as usize].open);
        d.toggle(Some(9), Some(Mode::Notes));
        assert!(d.focused && d.panes[Mode::Notes as usize].open);
    }
}
