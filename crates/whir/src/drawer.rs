//! The app-wide drawer: Notes and Images tabs, independent of the open document.
//! Notes include tables and put open todos first; Images is a grid of
//! fixed-size capture thumbnails, newest first. PDFs and CSVs initially fold it down to its header,
//! but either drawer tab or Ctrl+E opens it again.
//!
//! The selected tab, shared height and each pane's manual fold state are remembered.
//! Automatic folding never changes those preferences.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use egui::{
    Align2, Color32, FontId, Frame, Id, Key, Modifiers, Panel, Rect, Sense, Stroke, pos2, vec2,
};
use whir_core::captures::{CaptureEntry, CapturesSnapshot};
use whir_core::icons::{self, Icon};
use whir_core::kit;
use whir_core::library::{EntryKind, LibraryEntry, LibrarySnapshot};
use whir_core::marks::{self, Kind};
use whir_core::theme::Theme;
use whir_core::todos::State as Todo;
use whir_core::{DocKind, OpenCx, settings};
use whir_image::{Thumb, Thumbs};

use crate::keys::consume_exact;
use crate::reltime::{self, SystemFormats};
use crate::tab_bar::paint_kind_glyph;

/// Which library the drawer shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Every note and table in the notes folder.
    Notes,
    /// The screenshots, as a thumbnail grid.
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

enum HeaderAction {
    Toggle,
    Select(Mode),
}

/// Fold state and keyboard selection for one library.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Pane {
    open: bool,
    /// The keyboard's place in the list.
    selected: usize,
}

const MIN_HEIGHT: f32 = 96.0;
const DEFAULT_HEIGHT: f32 = 170.0;
/// Notes are laid out in columns at least this wide.
const NOTE_COLUMN: f32 = 280.0;
/// Thumbnail dimensions in points, independent of the drawer's height.
const SHOT_THUMB_H: f32 = 100.0;
const SHOT_ASPECT: f32 = 1.6;
const SHOT_LABEL_H: f32 = 20.0;
const SHOT_GAP: f32 = 10.0;

pub struct Drawer {
    panes: [Pane; 2],
    mode: Mode,
    height: f32,
    document: Option<(u64, DocKind)>,
    /// PDF/CSV activation folds the drawer without overwriting manual preferences.
    auto_collapsed: bool,
    /// Ctrl+E: arrows, Enter, Delete and Esc belong to the drawer.
    focused: bool,
    last_active: Option<PathBuf>,
    /// A newly opened capture may not have reached the background list yet.
    pending_active: bool,
    scroll_to: bool,
    /// Made the first time the image grid is shown.
    thumbs: Option<Thumbs>,
    /// The unfolded pane last frame (opening one may need a rescan).
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
    /// Flag a note as a todo, mark it done, or not.
    pub flag: Option<(PathBuf, Flag)>,
}

/// What to do with a note's todo flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flag {
    Open,
    Done,
    Clear,
    /// Open if it isn't; done if it is.
    Toggle,
}

fn load_pane(mode: Mode) -> Pane {
    Pane {
        open: settings::get_bool(&format!("drawer.{}.open", mode.key()), true),
        selected: 0,
    }
}

fn saved_height(shared: Option<&str>, legacy: Option<&str>) -> f32 {
    let parse = |s: &str| s.parse::<f32>().ok().filter(|h| h.is_finite());
    shared
        .and_then(parse)
        .or_else(|| legacy.and_then(parse))
        .unwrap_or(DEFAULT_HEIGHT)
        .max(MIN_HEIGHT)
}

impl Default for Drawer {
    fn default() -> Self {
        let mode = match settings::get_str("drawer.tab").as_deref() {
            Some("shots") => Mode::Shots,
            _ => Mode::Notes,
        };
        // Migrate the selected pane's old height once, so changing tabs and
        // restarting cannot bring back the other pane's size.
        let height = saved_height(
            settings::get_str("drawer.height").as_deref(),
            settings::get_str(&format!("drawer.{}.height", mode.key())).as_deref(),
        );
        settings::set_str("drawer.height", &height.to_string());
        Self {
            panes: [load_pane(Mode::Notes), load_pane(Mode::Shots)],
            mode,
            height,
            document: None,
            auto_collapsed: false,
            focused: false,
            last_active: None,
            pending_active: false,
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

    fn is_open(&self) -> bool {
        self.panes[self.mode as usize].open && !self.auto_collapsed
    }

    fn set_open(&mut self, open: bool) {
        self.auto_collapsed = false;
        if !open {
            self.focused = false;
        }
        let mode = self.mode;
        if self.pane(mode).open != open {
            self.pane(mode).open = open;
            settings::set_bool(&format!("drawer.{}.open", mode.key()), open);
        }
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Only a document change applies the default fold; repainting must not
    /// undo the user's decision to unfold it on a PDF or table.
    pub fn sync_document(&mut self, document: Option<(u64, DocKind)>) {
        if self.document != document {
            self.document = document;
            self.focused = false;
            self.auto_collapsed =
                document.is_some_and(|(_, kind)| matches!(kind, DocKind::Pdf | DocKind::Csv));
        }
    }

    fn select(&mut self, mode: Mode) {
        if self.mode != mode {
            self.mode = mode;
            self.last_active = None;
            self.pending_active = false;
            settings::set_str("drawer.tab", mode.key());
        }
        self.set_open(true);
        self.scroll_to = true;
    }

    /// Ctrl+E: unfold the selected pane and take the keyboard; again to
    /// give it back and fold it.
    pub fn toggle(&mut self) {
        if self.focused && self.is_open() {
            self.set_open(false);
        } else {
            self.focused = true;
            self.scroll_to = true;
            self.set_open(true);
        }
    }

    fn follow_active(&mut self, active: Option<&Path>, index: impl FnOnce() -> Option<usize>) {
        if self.last_active.as_deref() != active {
            self.last_active = active.map(Path::to_path_buf);
            self.pending_active = active.is_some();
            self.scroll_to = true;
        }
        if self.pending_active
            && let Some(at) = index()
        {
            self.pane(self.mode).selected = at;
            self.pending_active = false;
            self.scroll_to = true;
        }
    }

    /// Keyboard, while the drawer has it. Call before the document's UI, with
    /// the drawer shown, the length of its list and the path of item `i`.
    pub fn keys(
        &mut self,
        ui: &mut egui::Ui,
        n: usize,
        path_at: impl Fn(usize) -> Option<PathBuf>,
        out: &mut Out,
    ) {
        if !self.focused || !self.is_open() {
            self.focused = false;
            return;
        }
        let mode = self.mode;
        let columns = ui
            .ctx()
            .data(|d| d.get_temp::<usize>(Id::new(("whir.drawer.columns", mode.key()))))
            .unwrap_or(1);
        let (mut open, mut delete, mut close, mut step) = (false, false, false, 0isize);
        let mut flag = false;
        ui.input_mut(|i| {
            let none = Modifiers::NONE;
            let (across, down) = (1, columns as isize);
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
            flag = mode == Mode::Notes && consume_exact(i, none, Key::F);
        });
        if n > 0 && step != 0 {
            self.pending_active = false;
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
            } else if flag {
                out.flag = Some((path, Flag::Toggle));
            }
        }
        if close {
            self.focused = false;
        }
    }

    /// The drawer along the bottom of `ui`, before the document takes what is left.
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        lib: &LibrarySnapshot,
        shots: &CapturesSnapshot,
        cx: &OpenCx,
        active: Option<&Path>,
        status: &str,
        theme: &Theme,
    ) -> Out {
        let mut out = Out::default();
        let mode = self.mode;
        let open = self.is_open();
        let shown = open.then_some(mode);
        if shown != self.shown {
            self.shown = shown;
            out.rescan = shown;
        }
        let listed_active = active.filter(|p| match mode {
            Mode::Notes => cx.library.contains(p),
            Mode::Shots => cx.captures.contains(p),
        });
        self.follow_active(listed_active, || match mode {
            Mode::Notes => lib.entries.iter().position(|e| is(active, &e.path)),
            Mode::Shots => shots.entries.iter().position(|e| is(active, &e.path)),
        });
        let header_h = theme.metrics.drawer_header_h;
        let frame = Frame::NONE.fill(theme.drawer_bg);
        let id = Id::new("whir.drawer");
        let mut action = None;
        let drawn = if !open {
            Panel::bottom(id.with("folded"))
                .exact_size(header_h)
                .resizable(false)
                .show_separator_line(false)
                .frame(frame)
                .show(ui, |ui| {
                    action = self.header(ui, lib, shots, cx, false, status, theme, &mut out);
                })
                .response
                .rect
        } else {
            let max = (ui.available_height() - 140.0).max(MIN_HEIGHT);
            let shown = Panel::bottom(id)
                .resizable(true)
                .show_separator_line(false)
                .default_size(self.height.min(max))
                .size_range(MIN_HEIGHT..=max)
                .frame(frame)
                .show(ui, |ui| {
                    ui.expand_to_include_rect(ui.max_rect());
                    let all = ui.max_rect();
                    let head = Rect::from_min_size(all.min, vec2(all.width(), header_h));
                    let mut head_ui = ui.new_child(egui::UiBuilder::new().max_rect(head));
                    action =
                        self.header(&mut head_ui, lib, shots, cx, true, status, theme, &mut out);
                    let body = Rect::from_min_max(pos2(all.left(), head.bottom()), all.max);
                    let mut body_ui = ui
                        .new_child(egui::UiBuilder::new().max_rect(body.shrink2(vec2(10.0, 4.0))));
                    match mode {
                        Mode::Notes => self.notes(&mut body_ui, lib, active, theme, &mut out),
                        Mode::Shots => {
                            self.images(&mut body_ui, shots, cx, active, theme, &mut out)
                        }
                    }
                });
            // Dragged taller or shorter: remembered once it's let go.
            let height = shown.response.rect.height();
            let released = ui.input(|i| !i.pointer.any_down());
            if (height - self.height).abs() > 0.5 && released {
                self.height = height;
                settings::set_str("drawer.height", &format!("{height:.0}"));
            }
            shown.response.rect
        };
        if let Some(action) = action {
            match action {
                HeaderAction::Toggle => {
                    self.set_open(!open);
                    self.scroll_to = !open;
                }
                HeaderAction::Select(mode) => self.select(mode),
            }
            ui.ctx().request_repaint();
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

    /// Both tabs stay visible when folded. Only the chevron and unused
    /// header space fold it; selecting a tab always opens that library.
    #[allow(clippy::too_many_arguments)]
    fn header(
        &mut self,
        ui: &mut egui::Ui,
        lib: &LibrarySnapshot,
        shots: &CapturesSnapshot,
        cx: &OpenCx,
        open: bool,
        status: &str,
        theme: &Theme,
        out: &mut Out,
    ) -> Option<HeaderAction> {
        let rect = ui.max_rect();
        let painter = &ui.painter().clone();
        painter.line_segment(
            [rect.left_top(), rect.right_top()],
            Stroke::new(1.0, theme.separator),
        );
        let fold_rect = Rect::from_min_size(rect.min, vec2(28.0, rect.height()));
        let fold = ui
            .interact(fold_rect, Id::new("whir.drawer.fold"), Sense::CLICK)
            .on_hover_text(if open {
                "Collapse drawer"
            } else {
                "Expand drawer"
            });
        let mut action = fold.clicked().then_some(HeaderAction::Toggle);
        let chevron = if open {
            Icon::ChevronDown
        } else {
            Icon::ChevronRight
        };
        icons::paint(
            painter,
            pos2(rect.left() + 14.0, rect.center().y),
            chevron,
            10.0,
            theme.text_dim,
        );
        let mut left = fold_rect.right();
        for (mode, title, count) in [
            (Mode::Notes, "Notes", lib.entries.len()),
            (Mode::Shots, "Images", shots.entries.len()),
        ] {
            let selected = mode == self.mode;
            let color = if selected { theme.text } else { theme.text_dim };
            let name = painter.layout_no_wrap(
                title.to_owned(),
                FontId::proportional(theme.metrics.body_text_size),
                color,
            );
            let count = painter.layout_no_wrap(
                count.to_string(),
                FontId::proportional(theme.metrics.small_text_size),
                theme.text_dim,
            );
            let tab_rect = Rect::from_min_size(
                pos2(left, rect.top() + 3.0),
                vec2(name.size().x + count.size().x + 26.0, rect.height() - 6.0),
            );
            let resp = ui.interact(
                tab_rect,
                Id::new(("whir.drawer.tab", mode.key())),
                Sense::CLICK,
            );
            if selected || resp.hovered() {
                painter.rect_filled(
                    tab_rect,
                    4.0,
                    if selected {
                        theme.drawer_item_active
                    } else {
                        theme.drawer_item_hover
                    },
                );
            }
            if selected {
                painter.line_segment(
                    [tab_rect.left_bottom(), tab_rect.right_bottom()],
                    Stroke::new(2.0, theme.accent),
                );
            }
            let name_w = name.size().x;
            painter.galley(
                pos2(left + 10.0, rect.center().y - name.size().y / 2.0),
                name,
                color,
            );
            painter.galley(
                pos2(left + 16.0 + name_w, rect.center().y - count.size().y / 2.0),
                count,
                theme.text_dim,
            );
            if resp.clicked() {
                action = Some(HeaderAction::Select(mode));
            }
            left = tab_rect.right() + 4.0;
        }
        // Capturing is always a click away, whichever library is showing.
        let capture_rect = Rect::from_min_size(pos2(left, rect.top()), vec2(0.0, rect.height()));
        let mut capture_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(capture_rect.with_max_x(rect.right()))
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        let capture = whir_core::toolbar::labelled_button(
            &mut capture_ui,
            Icon::Camera,
            "Capture",
            false,
            theme,
        )
        .on_hover_text("Take a screenshot (Ctrl+PrtSc)");
        if capture.clicked() {
            out.capture = true;
        }
        left = capture.rect.right() + 4.0;
        let spare = Rect::from_min_max(
            pos2(left, rect.top()),
            pos2((rect.right() - 74.0).max(left), rect.bottom()),
        );
        let resp = ui.interact(spare, Id::new("whir.drawer.header"), Sense::CLICK);
        if resp.clicked() {
            action = Some(HeaderAction::Toggle);
        }
        if self.focused {
            painter
                .with_clip_rect(spare.intersect(ui.clip_rect()))
                .text(
                    pos2(left + 10.0, rect.center().y),
                    Align2::LEFT_CENTER,
                    match self.mode {
                        Mode::Notes => "↑ ↓ ← → · Enter · F to flag · Delete · Esc",
                        Mode::Shots => "↑ ↓ ← → · Enter · Delete · Esc",
                    },
                    FontId::proportional(theme.metrics.small_text_size),
                    theme.text_dim,
                );
        }
        let mut buttons = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(Rect::from_min_max(
                    pos2(spare.right(), rect.top()),
                    pos2(rect.right() - 10.0, rect.bottom()),
                ))
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        match self.mode {
            Mode::Notes => {
                if let Some(dir) = cx.library.dir()
                    && icon_button(&mut buttons, Icon::Folder, "folder", theme)
                        .on_hover_text("Open the notes folder")
                        .clicked()
                {
                    out.reveal = Some(dir.to_path_buf());
                }
                if icon_button(&mut buttons, Icon::Plus, "new-note", theme)
                    .on_hover_text("New note (Ctrl+N)")
                    .clicked()
                {
                    out.new_note = true;
                }
            }
            Mode::Shots => {
                if let Some(dir) = cx.captures.dir()
                    && icon_button(&mut buttons, Icon::Folder, "folder", theme)
                        .on_hover_text("Open the screenshots folder")
                        .clicked()
                {
                    out.reveal = Some(dir.to_path_buf());
                }
            }
        }
        if !status.is_empty() {
            // The document's line, right up against the buttons.
            let width = spare.width() - 16.0;
            let galley = kit::one_line(
                painter,
                status,
                FontId::proportional(theme.metrics.small_text_size),
                width.max(0.0),
            );
            let at = pos2(
                spare.right() - 6.0 - galley.size().x,
                rect.center().y - galley.size().y / 2.0,
            );
            if !self.focused {
                painter.galley(at, galley, theme.text_dim);
            }
        }
        action
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
            .data_mut(|d| d.insert_temp(Id::new(("whir.drawer.columns", "notes")), columns));
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
                        self.pending_active = false;
                        self.pane(Mode::Notes).selected = idx;
                    }
                }
            }
        });
        ui.ctx()
            .data_mut(|d| d.insert_temp(Id::new("whir.drawer.notes.off"), res.state.offset.y));
    }

    /// Fixed-size thumbnails wrap into rows; a taller drawer reveals more rows.
    fn images(
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
        let card_w = SHOT_THUMB_H * SHOT_ASPECT;
        let card_h = SHOT_THUMB_H + SHOT_LABEL_H;
        let row_h = card_h + SHOT_GAP;
        let scroll_to = std::mem::take(&mut self.scroll_to);
        let scroll = egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            // Selection jumps must keep up with rapid arrow/Home/End input.
            .animated(false)
            .id_salt("whir.drawer.shots.grid");
        let thumbs = self
            .thumbs
            .get_or_insert_with(|| Thumbs::new(cx.textures.clone(), cx.waker.clone()));
        thumbs.begin_frame();
        let now_sys = SystemTime::now();
        let now = whir_core::time::now_local();
        let focused = self.focused;
        let mut clicked = None;
        scroll.show_viewport(ui, |ui, viewport| {
            // Measure inside the scroll area so the scrollbar can't obscure a column.
            let width = ui.available_width();
            let columns = ((width + SHOT_GAP) / (card_w + SHOT_GAP)).floor().max(1.0) as usize;
            let columns_id = Id::new(("whir.drawer.columns", "shots"));
            let previous_columns = ui.ctx().data(|d| d.get_temp::<usize>(columns_id));
            ui.ctx().data_mut(|d| d.insert_temp(columns_id, columns));
            let rows = entries.len().div_ceil(columns);
            let (grid, _) =
                ui.allocate_exact_size(vec2(width, rows as f32 * row_h), Sense::hover());
            let card_at = |index: usize| {
                Rect::from_min_size(
                    grid.min
                        + vec2(
                            (index % columns) as f32 * (card_w + SHOT_GAP),
                            (index / columns) as f32 * row_h + 4.0,
                        ),
                    vec2(card_w, card_h),
                )
            };
            if scroll_to || (focused && previous_columns != Some(columns)) {
                ui.scroll_to_rect(card_at(selected), None);
            }
            let first = (viewport.top() / row_h).floor().max(0.0) as usize;
            let last = ((viewport.bottom() / row_h).ceil() as usize).min(rows);
            for row in first..last {
                for col in 0..columns {
                    let idx = row * columns + col;
                    let Some(e) = entries.get(idx) else { break };
                    let on = focused && idx == selected;
                    if shot_card(
                        ui,
                        card_at(idx),
                        SHOT_THUMB_H,
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
            }
        });
        thumbs.end_frame();
        if let Some(idx) = clicked {
            self.pending_active = false;
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
    let mark = pos2(rect.left() + 14.0, rect.center().y);
    match e.todo {
        Some(Todo::Open { .. }) => {
            icons::paint(painter, mark, Icon::FlagFilled, 14.0, theme.note_glyph)
        }
        Some(Todo::Done { .. }) => icons::paint(painter, mark, Icon::Check, 13.0, theme.text_dim),
        None => paint_kind_glyph(painter, mark, kind, theme),
    }
    let done = matches!(e.todo, Some(Todo::Done { .. }));
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
    let title = kit::one_line(
        painter,
        &e.title,
        FontId::proportional(theme.metrics.body_text_size),
        rect.right() - 16.0 - age_w - left,
    );
    let color = if done { theme.text_dim } else { theme.text };
    painter.galley(
        pos2(left, rect.center().y - title.size().y / 2.0),
        title,
        color,
    );
    if resp.hovered() && !e.preview.is_empty() {
        let preview = e.preview.clone();
        resp.clone().on_hover_text_at_pointer(preview);
    }
    let clicked = resp.clicked();
    if clicked {
        out.open = Some(e.path.clone());
    }
    row_menu(&resp, id.with("menu"), &e.path, Some(e.todo), theme, out);
    clicked
}

/// One screenshot in the grid: its picture with its marks, and
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
        icons::paint(painter, frame.center(), Icon::Cloud, 20.0, theme.text_dim);
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
                    Kind::Freehand(_) | Kind::Arrow(_) => marks::paint_drawing(
                        &on_thumb,
                        mark,
                        |point| image.min + point.to_vec2() * per_pixel,
                        per_pixel,
                    ),
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
    let text = kit::one_line(painter, &label, small, card.width());
    painter.galley(
        pos2(card.left(), frame.bottom() + 4.0),
        text,
        if active { theme.text } else { theme.text_dim },
    );
    let clicked = resp.clicked();
    if clicked {
        out.open = Some(e.path.clone());
    }
    row_menu(&resp, id.with("menu"), &e.path, None, theme, out);
    clicked
}

/// Right-click on a note (`todo`: its flag) or a screenshot (`None`).
fn row_menu(
    resp: &egui::Response,
    id: Id,
    path: &Path,
    todo: Option<Option<Todo>>,
    theme: &Theme,
    out: &mut Out,
) {
    egui::Popup::context_menu(resp).id(id).show(|ui| {
        if ui.add(kit::menu_entry(None, "Open", "")).clicked() {
            out.open = Some(path.to_owned());
            ui.close();
        }
        if let Some(todo) = todo {
            let choices: &[(&str, Flag)] = match todo {
                None => &[("Flag as todo", Flag::Open)],
                Some(Todo::Open { .. }) => {
                    &[("Mark done", Flag::Done), ("Remove flag", Flag::Clear)]
                }
                Some(Todo::Done { .. }) => {
                    &[("Flag again", Flag::Open), ("Remove flag", Flag::Clear)]
                }
            };
            for &(label, flag) in choices {
                if ui
                    .add(kit::menu_entry(Some(Icon::Flag), label, ""))
                    .clicked()
                {
                    out.flag = Some((path.to_owned(), flag));
                    ui.close();
                }
            }
        }
        if ui
            .add(kit::menu_entry(Some(Icon::Folder), "Show in Explorer", ""))
            .clicked()
        {
            out.reveal = Some(path.to_owned());
            ui.close();
        }
        ui.separator();
        let delete = egui::RichText::new("Delete").color(theme.danger);
        if ui
            .add(kit::menu_entry(Some(Icon::Delete), delete, ""))
            .clicked()
        {
            out.delete = Some(path.to_owned());
            ui.close();
        }
    });
}

/// A small frameless icon button.
fn icon_button(ui: &mut egui::Ui, icon: Icon, name: &str, theme: &Theme) -> egui::Response {
    let (_, rect) = ui.allocate_space(vec2(24.0, 24.0));
    let resp = ui.interact(rect, Id::new(("whir.drawer.action", name)), Sense::CLICK);
    let painter = ui.painter();
    if resp.hovered() {
        painter.rect_filled(rect, theme.metrics.control_radius, theme.drawer_item_hover);
    }
    icons::paint(painter, rect.center(), icon, 15.0, theme.text_dim);
    resp
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

    fn drawer() -> Drawer {
        settings::memory_only();
        Drawer {
            mode: Mode::Notes,
            height: DEFAULT_HEIGHT,
            panes: [Pane {
                open: true,
                selected: 0,
            }; 2],
            ..Drawer::default()
        }
    }

    #[test]
    fn the_selected_library_is_independent_of_the_document() {
        let mut d = drawer();
        for mode in [Mode::Shots, Mode::Notes] {
            d.select(mode);
            for (id, kind) in [
                DocKind::Note,
                DocKind::Image,
                DocKind::Pdf,
                DocKind::Csv,
                DocKind::Settings,
                DocKind::Error,
            ]
            .into_iter()
            .enumerate()
            {
                d.sync_document(Some((id as u64, kind)));
                assert_eq!(d.mode(), mode);
                assert_eq!(d.is_open(), !matches!(kind, DocKind::Pdf | DocKind::Csv));
            }
            d.sync_document(None);
            assert_eq!(d.mode(), mode);
            assert!(d.is_open());
        }
    }

    #[test]
    fn pdfs_and_tables_fold_only_on_activation_and_allow_either_library() {
        let mut d = drawer();
        d.select(Mode::Shots);
        for kind in [DocKind::Pdf, DocKind::Csv] {
            d.sync_document(Some((7, kind)));
            assert!(!d.is_open());
            assert!(
                d.panes[Mode::Shots as usize].open,
                "automatic fold isn't a preference"
            );
            for mode in [Mode::Notes, Mode::Shots] {
                d.select(mode);
                assert!(d.is_open());
                d.sync_document(Some((7, kind)));
                assert!(
                    d.is_open(),
                    "repainting must not collapse a manual expansion"
                );
            }
            d.sync_document(Some((8, kind)));
            assert!(!d.is_open(), "another document of the same kind also folds");
            d.sync_document(Some((9, DocKind::Image)));
            assert!(d.is_open());
            assert_eq!(d.mode(), Mode::Shots);
        }
    }

    #[test]
    fn shared_height_prefers_the_new_setting_and_migrates_the_old_one() {
        assert_eq!(saved_height(Some("260"), Some("120")), 260.0);
        assert_eq!(saved_height(None, Some("240")), 240.0);
        assert_eq!(saved_height(None, None), DEFAULT_HEIGHT);
        for invalid in ["invalid", "NaN", "inf"] {
            assert_eq!(saved_height(Some(invalid), Some("120")), 120.0);
            assert_eq!(saved_height(None, Some(invalid)), DEFAULT_HEIGHT);
        }
        assert_eq!(saved_height(Some("20"), Some("240")), MIN_HEIGHT);
    }

    #[test]
    fn manual_fold_and_shared_height_survive_document_changes() {
        let mut d = drawer();
        d.height = 240.0;
        d.set_open(false);
        d.sync_document(Some((1, DocKind::Csv)));
        d.sync_document(Some((2, DocKind::Note)));
        assert!(!d.is_open());
        d.select(Mode::Shots);
        assert!(d.is_open());
        assert_eq!(d.height, 240.0);
        d.select(Mode::Notes);
        assert!(d.is_open());
        assert_eq!(d.height, 240.0);
    }

    #[test]
    fn ctrl_e_uses_the_selected_library_and_releases_focus_when_tabs_change() {
        let mut d = drawer();
        d.select(Mode::Shots);
        d.sync_document(Some((7, DocKind::Pdf)));
        d.toggle();
        assert_eq!(d.mode(), Mode::Shots);
        assert!(d.focused && d.is_open());
        d.toggle();
        assert!(!d.focused && !d.is_open());
        d.toggle();
        assert!(d.focused && d.is_open());
        d.sync_document(Some((8, DocKind::Note)));
        assert!(!d.focused && d.is_open());
        assert_eq!(d.mode(), Mode::Shots);
    }

    #[test]
    fn a_capture_is_selected_and_scrolled_to_when_its_refresh_arrives() {
        let mut d = drawer();
        d.select(Mode::Shots);
        d.pane(Mode::Shots).selected = 8;
        let path = Path::new(r"C:\notes\Screenshots\new.png");
        d.follow_active(Some(path), || None);
        assert!(d.pending_active);
        // A frame can draw the old list and consume its scroll request.
        d.scroll_to = false;
        d.follow_active(Some(path), || Some(0));
        assert_eq!(d.pane(Mode::Shots).selected, 0);
        assert!(!d.pending_active && d.scroll_to);
        // Later repaints must not override keyboard navigation.
        d.pane(Mode::Shots).selected = 3;
        d.scroll_to = false;
        d.follow_active(Some(path), || Some(0));
        assert_eq!(d.pane(Mode::Shots).selected, 3);
        assert!(!d.scroll_to);
    }

    struct Harness {
        ctx: egui::Context,
        drawer: Drawer,
        cx: OpenCx,
        shots: CapturesSnapshot,
        size: egui::Vec2,
        time: f64,
    }

    impl Harness {
        fn new() -> Self {
            let ctx = egui::Context::default();
            ctx.set_fonts(whir_platform::fonts::definitions(whir_core::theme::FONTS));
            Self {
                ctx,
                drawer: drawer(),
                cx: cx(Path::new(r"C:\whir-drawer-test")),
                shots: CapturesSnapshot::default(),
                size: vec2(800.0, 600.0),
                time: 0.0,
            }
        }

        fn with_images(name: &str, count: usize) -> Self {
            let root = std::env::temp_dir()
                .join(format!("whir-drawer-images-{name}-{}", std::process::id(),));
            let dir = root.join("Screenshots");
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&dir).unwrap();
            for i in 0..count {
                std::fs::write(dir.join(format!("shot-{i:03}.png")), b"fixture").unwrap();
            }
            let (tx, rx) = std::sync::mpsc::channel();
            let waker = whir_core::Waker::new(move || {
                let _ = tx.send(());
            });
            let mut h = Self::new();
            h.cx = cx(&root);
            h.cx.captures = whir_core::Captures::at(dir, waker);
            h.cx.captures.scan();
            rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
            h.shots = h.cx.captures.snapshot();
            assert_eq!(h.shots.entries.len(), count);
            // Layout tests need cards, not background decoding or GPU textures.
            for entry in std::sync::Arc::make_mut(&mut h.shots.entries) {
                entry.cloud_only = true;
            }
            h.drawer.select(Mode::Shots);
            std::fs::remove_dir_all(root).unwrap();
            h
        }

        fn frame(&mut self, events: Vec<egui::Event>) -> Out {
            self.time += 0.05;
            let input = egui::RawInput {
                events,
                time: Some(self.time),
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, self.size)),
                ..Default::default()
            };
            let mut out = Out::default();
            let output = self.ctx.run_ui(input, |ui| {
                let mut keys = Out::default();
                if self.drawer.mode() == Mode::Shots {
                    self.drawer.keys(
                        ui,
                        self.shots.entries.len(),
                        |i| self.shots.entries.get(i).map(|e| e.path.clone()),
                        &mut keys,
                    );
                }
                out = self.drawer.show(
                    ui,
                    &LibrarySnapshot::default(),
                    &self.shots,
                    &self.cx,
                    None,
                    "",
                    &Theme::light(),
                );
                out.open = keys.open.or(out.open.take());
                out.delete = keys.delete.or(out.delete.take());
                egui::CentralPanel::default().show(ui, |_| {});
            });
            output.drop_without_applying_deltas();
            out
        }

        fn settle(&mut self) {
            self.frame(vec![]);
            self.frame(vec![]);
        }

        fn rect(&self, id: Id) -> Rect {
            self.ctx
                .read_response(id)
                .expect("drawer control exists")
                .rect
        }

        fn image_id(&self, index: usize) -> Id {
            Id::new(("whir.drawer.shot", &self.shots.entries[index].path))
        }

        fn image_visible(&self, index: usize) -> bool {
            self.ctx
                .read_response(self.image_id(index))
                .is_some_and(|r| r.interact_rect.contains_rect(r.rect))
        }

        fn visible_images(&self) -> usize {
            (0..self.shots.entries.len())
                .filter(|&i| self.image_visible(i))
                .count()
        }

        fn press_key(&mut self, key: Key) -> Out {
            let event = |pressed| egui::Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers: Modifiers::NONE,
            };
            let out = self.frame(vec![event(true)]);
            self.frame(vec![event(false)]);
            out
        }

        fn finish_scroll(&mut self) {
            for _ in 0..12 {
                self.frame(vec![]);
            }
        }

        fn resize_drawer(&mut self, delta: f32) {
            let from = pos2(400.0, self.rect(Id::new("whir.drawer.fold")).top());
            let to = from + vec2(0.0, delta);
            self.frame(vec![egui::Event::PointerMoved(from)]);
            self.frame(vec![egui::Event::PointerButton {
                pos: from,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            }]);
            self.frame(vec![egui::Event::PointerMoved(to)]);
            self.frame(vec![egui::Event::PointerButton {
                pos: to,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            }]);
            self.settle();
        }

        fn click(&mut self, pos: egui::Pos2) -> Out {
            self.frame(vec![egui::Event::PointerMoved(pos)]);
            let button = |pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            };
            self.frame(vec![button(true)]);
            self.frame(vec![button(false)])
        }
    }

    #[test]
    fn expanding_images_shows_more_rows_without_enlarging_thumbnails() {
        let mut h = Harness::with_images("resize", 24);
        h.settle();
        let size = h.rect(h.image_id(0)).size();
        let visible = h.visible_images();
        assert!(visible > 0);
        h.resize_drawer(-200.0);
        assert_eq!(h.rect(h.image_id(0)).size(), size);
        assert!(
            h.visible_images() > visible,
            "extra height shows more images"
        );
        h.resize_drawer(200.0);
        assert_eq!(h.rect(h.image_id(0)).size(), size);
        assert_eq!(h.visible_images(), visible);
    }

    #[test]
    fn image_grid_reflows_and_keyboard_navigation_follows_its_rows() {
        let mut h = Harness::with_images("keyboard", 25);
        h.settle();
        h.resize_drawer(-200.0);
        let size = h.rect(h.image_id(0)).size();
        let columns_id = Id::new(("whir.drawer.columns", "shots"));
        let wide_columns = h.ctx.data(|d| d.get_temp::<usize>(columns_id)).unwrap();
        h.size.x = 540.0;
        h.settle();
        let columns = h.ctx.data(|d| d.get_temp::<usize>(columns_id)).unwrap();
        assert!(columns < wide_columns);
        assert_eq!(h.rect(h.image_id(0)).size(), size);
        let next_row = h.rect(h.image_id(columns));
        assert_eq!(next_row.left(), h.rect(h.image_id(0)).left());
        assert!(next_row.top() > h.rect(h.image_id(0)).bottom());
        h.drawer.toggle();
        h.press_key(Key::ArrowDown);
        assert_eq!(h.drawer.pane(Mode::Shots).selected, columns);
        h.press_key(Key::ArrowRight);
        assert_eq!(h.drawer.pane(Mode::Shots).selected, columns + 1);
        h.press_key(Key::ArrowUp);
        assert_eq!(h.drawer.pane(Mode::Shots).selected, 1);
        h.press_key(Key::End);
        let last = h.shots.entries.len() - 1;
        assert_eq!(h.drawer.pane(Mode::Shots).selected, last);
        h.finish_scroll();
        assert!(
            h.image_visible(last),
            "keyboard selection scrolls into view"
        );
        assert_eq!(
            h.press_key(Key::Enter).open,
            Some(h.shots.entries[last].path.clone())
        );
    }

    #[test]
    fn image_keyboard_navigation_can_reverse_before_scrolling_finishes() {
        let mut h = Harness::with_images("rapid-keys", 100);
        h.settle();
        h.drawer.toggle();
        h.press_key(Key::End);
        h.press_key(Key::Home);
        h.finish_scroll();
        assert_eq!(h.drawer.pane(Mode::Shots).selected, 0);
        assert!(
            h.image_visible(0),
            "rapid navigation must keep the selection in view"
        );
    }

    #[test]
    fn image_grid_scrolls_vertically_and_only_draws_visible_rows() {
        let mut h = Harness::with_images("scroll", 100);
        h.settle();
        h.resize_drawer(-150.0);
        let size = h.rect(h.image_id(0)).size();
        assert!(h.ctx.read_response(h.image_id(99)).is_none());
        let pos = h.rect(h.image_id(0)).center();
        h.frame(vec![egui::Event::PointerMoved(pos)]);
        h.frame(vec![egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: vec2(0.0, -300.0),
            phase: egui::TouchPhase::Move,
            modifiers: Modifiers::NONE,
        }]);
        h.finish_scroll();
        let first = (0..100).find(|&i| h.image_visible(i)).unwrap();
        assert!(first > 0, "wheel moves down through the grid");
        assert!(h.ctx.read_response(h.image_id(0)).is_none());
        assert!(h.ctx.read_response(h.image_id(99)).is_none());
        assert_eq!(h.rect(h.image_id(first)).size(), size);
        let out = h.click(h.rect(h.image_id(first)).center());
        assert_eq!(out.open, Some(h.shots.entries[first].path.clone()));
    }

    #[test]
    fn switching_libraries_keeps_the_resized_drawer_height() {
        let mut h = Harness::new();
        h.settle();
        let fold = Id::new("whir.drawer.fold");
        for (mode, delta) in [(Mode::Notes, -90.0), (Mode::Shots, 40.0)] {
            h.click(h.rect(Id::new(("whir.drawer.tab", mode.key()))).center());
            h.settle();
            let top = h.rect(fold).top();
            h.resize_drawer(delta);
            let resized_top = h.rect(fold).top();
            assert!(
                (resized_top - top - delta).abs() < 1.0,
                "the edge was dragged"
            );
            for tab in [Mode::Shots, Mode::Notes] {
                h.click(h.rect(Id::new(("whir.drawer.tab", tab.key()))).center());
                h.settle();
                assert_eq!(h.rect(fold).top(), resized_top, "switching to {tab:?}");
            }
            h.drawer.sync_document(Some((1, DocKind::Pdf)));
            h.settle();
            assert!(!h.drawer.is_open());
            h.click(h.rect(Id::new(("whir.drawer.tab", "shots"))).center());
            h.settle();
            assert_eq!(h.rect(fold).top(), resized_top, "unfolding on a PDF");
            h.drawer.sync_document(Some((2, DocKind::Note)));
            h.settle();
        }
    }

    #[test]
    fn both_tabs_work_when_folded_and_actions_do_not_toggle_the_drawer() {
        let mut h = Harness::new();
        h.drawer.sync_document(Some((1, DocKind::Pdf)));
        h.settle();
        let notes = Id::new(("whir.drawer.tab", "notes"));
        let images = Id::new(("whir.drawer.tab", "shots"));
        assert!(!h.drawer.is_open());
        assert!(h.rect(notes).top() >= 570.0);
        assert!(h.rect(images).top() >= 570.0);
        h.click(h.rect(images).center());
        assert!(h.drawer.is_open());
        assert_eq!(h.drawer.mode(), Mode::Shots);
        assert_eq!(h.frame(vec![]).rescan, Some(Mode::Shots));
        h.settle();
        let out = h.click(h.rect(Id::new(("whir.drawer.action", "folder"))).center());
        assert_eq!(out.reveal, h.cx.captures.dir().map(Path::to_path_buf));
        assert!(h.drawer.is_open(), "folder button isn't a fold toggle");
        h.click(h.rect(notes).center());
        assert_eq!(h.drawer.mode(), Mode::Notes);
        assert!(h.drawer.is_open());
        h.settle();
        h.click(h.rect(Id::new("whir.drawer.fold")).center());
        assert!(!h.drawer.is_open());
        h.settle();
        h.click(h.rect(notes).center());
        assert!(
            h.drawer.is_open(),
            "the selected tab also unfolds the drawer"
        );
    }
}
