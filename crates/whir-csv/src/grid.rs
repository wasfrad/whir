//! The virtualized table: only visible rows *and* columns are laid out and
//! painted, positions are tracked in f64 so billions of points scroll exactly.
//!
//! Cells are read through a [`SheetView`] (edits and sort applied). The grid
//! owns selection, scrolling and column widths; everything else the user asks
//! for (copy, paste, edit, insert, sort, ...) is reported as an [`Action`] for
//! the document to apply. All interactions are non-focusable: a focused egui
//! widget would be "clicked" by Space/Enter.

use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{
    Align2, Color32, CursorIcon, Event, FontId, Key, PointerButton, Pos2, Rect, Sense, Stroke, Ui,
    pos2,
};
use whir_core::scroll::{ScrollAxis, scrollbar};
use whir_core::theme::Theme;

use crate::editor::Mode;
use crate::keys::{CTRL, CTRL_SHIFT, NONE, SHIFT, consume_exact, take_text};
use crate::menu::{self, MenuCx, MenuHit};
use crate::sort::{Direction, SortSpec};
use crate::view::SheetView;

pub const ROW_H: f32 = 22.0;
pub const HEADER_H: f32 = 26.0;
pub const PAD: f32 = 8.0;
const MIN_COL_W: f32 = 36.0;
pub const DEFAULT_COL_W: f32 = 110.0;
/// The "+" column after the last one.
const GHOST_COL_W: f32 = 30.0;
const RESIZE_GRIP: f32 = 4.0;
/// Characters laid out per cell; one line never shows more.
const MAX_CELL_CHARS: usize = 400;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellPos {
    /// Display row (0 = first data row, in current sort order). `rows()` is
    /// the ghost row.
    pub row: u64,
    /// `cols()` is the ghost column.
    pub col: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: CellPos,
    pub cursor: CellPos,
}

impl Selection {
    pub fn single(c: CellPos) -> Self {
        Self {
            anchor: c,
            cursor: c,
        }
    }

    pub fn rows(&self) -> (u64, u64) {
        (
            self.anchor.row.min(self.cursor.row),
            self.anchor.row.max(self.cursor.row),
        )
    }

    pub fn cols(&self) -> (usize, usize) {
        (
            self.anchor.col.min(self.cursor.col),
            self.anchor.col.max(self.cursor.col),
        )
    }

    pub fn cell_count(&self) -> u64 {
        let (r0, r1) = self.rows();
        let (c0, c1) = self.cols();
        (r1 - r0 + 1) * (c1 - c0 + 1) as u64
    }

    pub fn contains(&self, c: CellPos) -> bool {
        let (r0, r1) = self.rows();
        let (c0, c1) = self.cols();
        (r0..=r1).contains(&c.row) && (c0..=c1).contains(&c.col)
    }

    /// Limited to real cells (`rows` × `cols`); `None` if nothing is left.
    pub fn clamped(&self, rows: u64, cols: usize) -> Option<Selection> {
        if rows == 0 || cols == 0 {
            return None;
        }
        let (r0, r1) = self.rows();
        let (c0, c1) = self.cols();
        if r0 >= rows || c0 >= cols {
            return None;
        }
        Some(Selection {
            anchor: CellPos { row: r0, col: c0 },
            cursor: CellPos {
                row: r1.min(rows - 1),
                col: c1.min(cols - 1),
            },
        })
    }
}

/// Whether an kind of change is allowed now, and why not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gate {
    Yes,
    No(&'static str),
}

impl Gate {
    pub fn allowed(self) -> bool {
        self == Gate::Yes
    }

    pub fn reason(self) -> &'static str {
        match self {
            Gate::Yes => "",
            Gate::No(r) => r,
        }
    }
}

/// What editing the document allows this frame.
#[derive(Clone, Copy, Debug)]
pub struct Editable {
    pub cells: Gate,
    pub rows: Gate,
    pub cols: Gate,
    /// Show the "+" row / column (the file can grow at all).
    pub ghost_row: bool,
    pub ghost_col: bool,
}

impl Editable {
    pub const READ_ONLY: Editable = Editable {
        cells: Gate::No("This file is read-only."),
        rows: Gate::No("This file is read-only."),
        cols: Gate::No("This file is read-only."),
        ghost_row: false,
        ghost_col: false,
    };
}

/// Something the user asked for, applied by the document.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Copy,
    Cut,
    Paste(String),
    /// Menu "Paste": ask the shell for the clipboard (arrives as `Paste`).
    RequestPaste,
    Undo,
    Redo,
    FillDown,
    FillRight,
    /// Delete key.
    Clear,
    /// Start editing the cursor cell; `seed` replaces its value (typing).
    Edit {
        mode: Mode,
        seed: Option<String>,
    },
    /// Ctrl+Shift+= / Ctrl+numpad +: rows above, or columns when whole
    /// columns are selected.
    InsertAuto,
    /// Ctrl+−: the selection's rows, or whole selected columns.
    DeleteAuto,
    InsertRows {
        above: bool,
    },
    DeleteRows,
    InsertCols {
        left: bool,
    },
    DeleteCols,
    Rename(usize),
    /// The "+" column header.
    AppendColumn,
    /// Header click: ascending → descending → original order.
    ToggleSort(usize),
    Sort(usize, Direction),
    OriginalOrder,
}

/// Per-frame inputs.
pub struct GridIo<'a, 'v> {
    pub view: &'a mut SheetView<'v>,
    pub sort: Option<SortSpec>,
    /// The sort is still computing, or cells changed since: dim the arrow.
    pub sort_dim: bool,
    /// A custom order is shown (sorted, or the sorted column was deleted).
    pub sorted: bool,
    /// Find matches as (display row, col), sorted.
    pub matches: &'a [(u64, u32)],
    pub current_match: Option<usize>,
    /// The grid owns the keyboard this frame (no text field has focus).
    pub keyboard: bool,
    pub editable: Editable,
}

#[derive(Default)]
pub struct GridOut {
    pub actions: Vec<Action>,
}

#[derive(Clone, Copy, Debug)]
enum Drag {
    None,
    Cells,
    Rows,
    Resize {
        col: usize,
        start_w: f32,
        start_x: f32,
    },
}

#[derive(Clone, Copy, Debug)]
enum Hit {
    Corner,
    Header { col: usize, border: Option<usize> },
    GhostHeader,
    Gutter { row: u64 },
    Body(CellPos),
    Outside,
}

pub struct Grid {
    pub widths: Vec<f32>,
    pub numeric: Vec<bool>,
    pub sy: ScrollAxis,
    pub sx: ScrollAxis,
    pub sel: Option<Selection>,
    drag: Drag,
    /// Column Tab started from (Enter returns to it, like Excel).
    pub tab_origin: Option<usize>,
    /// What the open context menu is about.
    menu_hit: Option<MenuHit>,
    /// Screen geometry from the last frame (for reveal() and the editor).
    body: Rect,
    rect: Rect,
    gutter_w: f32,
    /// Real rows last frame (the ghost row is at this index).
    rows: u64,
    ghost_row: bool,
    ghost_col: bool,
}

impl Default for Grid {
    fn default() -> Self {
        Self {
            widths: Vec::new(),
            numeric: Vec::new(),
            sy: ScrollAxis::default(),
            sx: ScrollAxis::default(),
            sel: None,
            drag: Drag::None,
            tab_origin: None,
            menu_hit: None,
            body: Rect::NOTHING,
            rect: Rect::NOTHING,
            gutter_w: 0.0,
            rows: 0,
            ghost_row: false,
            ghost_col: false,
        }
    }
}

fn digits(n: u64) -> usize {
    n.max(1).ilog10() as usize + 1
}

impl Grid {
    pub fn cols(&self) -> usize {
        self.widths.len()
    }

    pub fn ensure_cols(&mut self, n: usize) {
        if n > self.widths.len() {
            self.widths.resize(n, DEFAULT_COL_W);
            self.numeric.resize(n, false);
        }
    }

    fn col_starts(&self) -> Vec<f64> {
        let mut v = Vec::with_capacity(self.widths.len() + 1);
        let mut x = 0.0f64;
        v.push(0.0);
        for w in &self.widths {
            x += *w as f64;
            v.push(x);
        }
        v
    }

    fn col_x(&self, col: usize) -> f64 {
        self.widths[..col.min(self.widths.len())]
            .iter()
            .map(|&w| w as f64)
            .sum()
    }

    /// Width of `col` (the ghost column counts as a default one when editing).
    pub fn col_width(&self, col: usize) -> f32 {
        self.widths.get(col).copied().unwrap_or(DEFAULT_COL_W)
    }

    /// The data area (below the header, right of the row numbers), last frame.
    pub fn body_rect(&self) -> Rect {
        self.body
    }

    /// Screen rect of a body cell, last frame's layout (may be off screen).
    pub fn cell_rect(&self, row: u64, col: usize) -> Rect {
        let x = self.body.left() + (self.col_x(col) - self.sx.offset) as f32;
        let y = self.body.top() + (row as f64 * ROW_H as f64 - self.sy.offset) as f32;
        Rect::from_min_size(pos2(x, y), egui::vec2(self.col_width(col), ROW_H))
    }

    /// Screen rect of a header cell, last frame's layout.
    pub fn header_rect(&self, col: usize) -> Rect {
        let x = self.body.left() + (self.col_x(col) - self.sx.offset) as f32;
        Rect::from_min_size(
            pos2(x, self.rect.top()),
            egui::vec2(self.col_width(col), HEADER_H),
        )
    }

    /// The header strip (right of the corner), last frame.
    pub fn header_strip(&self) -> Rect {
        Rect::from_min_max(
            pos2(self.body.left(), self.rect.top()),
            pos2(self.rect.right(), self.body.top()),
        )
    }

    /// Scroll so `cell` is fully visible.
    pub fn reveal(&mut self, cell: CellPos) {
        let top = cell.row as f64 * ROW_H as f64;
        self.sy.reveal(top, top + ROW_H as f64);
        let a = self.col_x(cell.col);
        let b = a + self.col_width(cell.col) as f64;
        self.sx.reveal(a, b);
    }

    /// Scroll so column `col` is visible (header editing).
    pub fn reveal_col(&mut self, col: usize) {
        let a = self.col_x(col);
        self.sx.reveal(a, a + self.col_width(col) as f64);
    }

    pub fn select_all(&mut self, rows: u64) {
        if rows == 0 || self.cols() == 0 {
            return;
        }
        self.sel = Some(Selection {
            anchor: CellPos { row: 0, col: 0 },
            cursor: CellPos {
                row: rows - 1,
                col: self.cols() - 1,
            },
        });
    }

    fn hit(&self, p: Pos2, rect: Rect, starts: &[f64]) -> Hit {
        if !rect.contains(p) {
            return Hit::Outside;
        }
        let gutter_w = self.gutter_w;
        let rows = self.rows + self.ghost_row as u64;
        let cols = self.widths.len();
        let in_header = p.y < rect.top() + HEADER_H;
        let in_gutter = p.x < rect.left() + gutter_w;
        let content_x = |x: f32| self.sx.offset + (x - (rect.left() + gutter_w)) as f64;
        let col_at = |x: f32| -> Option<usize> {
            let cx = content_x(x);
            let i = starts.partition_point(|&s| s <= cx);
            if i >= 1 && i < starts.len() {
                Some(i - 1)
            } else if self.ghost_col && cx >= starts[cols] && cx < starts[cols] + GHOST_COL_W as f64
            {
                Some(cols)
            } else {
                None
            }
        };
        let row_at = |y: f32| -> Option<u64> {
            let content_y = self.sy.offset + (y - (rect.top() + HEADER_H)) as f64;
            let r = (content_y / ROW_H as f64).floor();
            (r >= 0.0 && (r as u64) < rows).then_some(r as u64)
        };
        match (in_header, in_gutter) {
            (true, true) => Hit::Corner,
            (true, false) => {
                // Column borders are grabbable within a few points either side.
                let cx = content_x(p.x);
                let border = starts
                    .iter()
                    .enumerate()
                    .skip(1)
                    .find(|(_, s)| (cx - **s).abs() <= RESIZE_GRIP as f64)
                    .map(|(i, _)| i - 1);
                match col_at(p.x) {
                    Some(col) if col == cols => match border {
                        Some(b) => Hit::Header {
                            col: b,
                            border: Some(b),
                        },
                        None => Hit::GhostHeader,
                    },
                    Some(col) => Hit::Header { col, border },
                    None => match border {
                        Some(b) => Hit::Header {
                            col: b,
                            border: Some(b),
                        },
                        None => Hit::Outside,
                    },
                }
            }
            (false, true) => row_at(p.y).map_or(Hit::Outside, |row| Hit::Gutter { row }),
            (false, false) => match (row_at(p.y), col_at(p.x)) {
                (Some(row), Some(col)) => Hit::Body(CellPos { row, col }),
                _ => Hit::Outside,
            },
        }
    }

    pub fn show(&mut self, ui: &mut Ui, rect: Rect, io: GridIo<'_, '_>, theme: &Theme) -> GridOut {
        let mut out = GridOut::default();
        let font = FontId::proportional(theme.metrics.body_text_size);
        let digit_w = ui.fonts_mut(|f| f.glyph_width(&font, '0'));
        let rows = io.view.rows();
        self.rows = rows;
        self.ghost_row = io.editable.ghost_row;
        self.ghost_col = io.editable.ghost_col;
        let shown_rows = rows + self.ghost_row as u64;
        let gutter_w = (digits(shown_rows.max(1)) as f32 * digit_w + 2.0 * PAD + 6.0).max(40.0);
        self.gutter_w = gutter_w;
        let body = Rect::from_min_max(
            pos2(rect.left() + gutter_w, rect.top() + HEADER_H),
            rect.max,
        );
        self.body = body;
        self.rect = rect;
        let starts = self.col_starts();
        let cols = self.widths.len();
        let content_w = *starts.last().unwrap_or(&0.0)
            + if self.ghost_col {
                GHOST_COL_W as f64
            } else {
                0.0
            };

        self.sy.viewport = body.height() as f64;
        self.sy.content = shown_rows as f64 * ROW_H as f64 + 4.0;
        self.sx.viewport = body.width() as f64;
        self.sx.content = content_w + 24.0;
        self.sy.clamp();
        self.sx.clamp();

        let id = ui.id().with("csv-grid");
        let resp = ui.interact(rect, id, Sense::CLICK | Sense::DRAG);
        let pointer = ui.input(|i| i.pointer.interact_pos().or(i.pointer.hover_pos()));
        let mods = ui.input(|i| i.modifiers);

        // --- mouse -----------------------------------------------------------
        if resp.hovered() {
            let d = ui.input(|i| i.smooth_scroll_delta);
            if d != egui::Vec2::ZERO {
                self.sy.scroll_by(-d.y as f64);
                self.sx.scroll_by(-d.x as f64);
            }
        }
        let hover_hit = pointer.map(|p| self.hit(p, rect, &starts));
        if let Some(Hit::Header {
            border: Some(_), ..
        }) = hover_hit
        {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeColumn);
        }
        if matches!(self.drag, Drag::Resize { .. }) {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeColumn);
        }

        let pressed = ui.input(|i| i.pointer.button_pressed(PointerButton::Primary));
        if pressed
            && resp.hovered()
            && let Some(p) = pointer
        {
            self.tab_origin = None;
            match self.hit(p, rect, &starts) {
                Hit::Header {
                    border: Some(col), ..
                } => {
                    self.drag = Drag::Resize {
                        col,
                        start_w: self.widths[col],
                        start_x: p.x,
                    };
                }
                Hit::Header { .. } | Hit::GhostHeader | Hit::Outside => self.drag = Drag::None,
                Hit::Corner => {
                    self.select_all(rows);
                    self.drag = Drag::None;
                }
                Hit::Gutter { row } => {
                    let last = cols.saturating_sub(1);
                    let anchor = match (mods.shift, self.sel) {
                        (true, Some(s)) => CellPos {
                            row: s.anchor.row,
                            col: 0,
                        },
                        _ => CellPos { row, col: 0 },
                    };
                    self.sel = Some(Selection {
                        anchor,
                        cursor: CellPos { row, col: last },
                    });
                    self.drag = Drag::Rows;
                }
                Hit::Body(cell) => {
                    self.sel = Some(match (mods.shift, self.sel) {
                        (true, Some(s)) => Selection {
                            anchor: s.anchor,
                            cursor: cell,
                        },
                        _ => Selection::single(cell),
                    });
                    self.drag = Drag::Cells;
                }
            }
        }
        // Right button: select what was clicked unless it's inside the
        // selection (like Excel), then open the menu for it.
        let secondary = ui.input(|i| i.pointer.button_pressed(PointerButton::Secondary));
        if secondary
            && resp.hovered()
            && let Some(p) = pointer
        {
            self.menu_hit = self.context_select(self.hit(p, rect, &starts), rows);
        }
        if resp.double_clicked() {
            match hover_hit {
                Some(Hit::Header {
                    border: Some(col), ..
                }) => self.autofit(ui, col, io.view, &font),
                Some(Hit::Body(_)) => {
                    out.actions.push(Action::Edit {
                        mode: Mode::Edit,
                        seed: None,
                    });
                }
                _ => {}
            }
        }
        if resp.clicked() {
            match hover_hit {
                Some(Hit::Header { col, border: None }) => {
                    out.actions.push(Action::ToggleSort(col));
                }
                Some(Hit::GhostHeader) => out.actions.push(Action::AppendColumn),
                _ => {}
            }
        }

        let down = ui.input(|i| i.pointer.primary_down());
        if down && let Some(p) = pointer {
            match self.drag {
                Drag::Resize {
                    col,
                    start_w,
                    start_x,
                } => {
                    self.widths[col] = (start_w + p.x - start_x).max(MIN_COL_W);
                }
                Drag::Cells | Drag::Rows => {
                    // Auto-scroll while dragging past the edges.
                    let mut dy = 0.0;
                    let mut dx = 0.0;
                    if p.y < body.top() {
                        dy = (p.y - body.top()) as f64;
                    } else if p.y > body.bottom() {
                        dy = (p.y - body.bottom()) as f64;
                    }
                    if matches!(self.drag, Drag::Cells) {
                        if p.x < body.left() {
                            dx = (p.x - body.left()) as f64;
                        } else if p.x > body.right() {
                            dx = (p.x - body.right()) as f64;
                        }
                    }
                    if dy != 0.0 || dx != 0.0 {
                        self.sy.scroll_by(dy * 0.5);
                        self.sx.scroll_by(dx * 0.5);
                        ui.ctx().request_repaint();
                    }
                    let clamped = pos2(
                        p.x.clamp(body.left() + 1.0, body.right() - 1.0),
                        p.y.clamp(body.top() + 1.0, body.bottom() - 1.0),
                    );
                    if let (Hit::Body(cell), Some(sel)) =
                        (self.hit(clamped, rect, &starts), &mut self.sel)
                    {
                        // Dragging extends onto the "+" row/column only from there.
                        let row = cell.row.min(rows.saturating_sub(1).max(sel.anchor.row));
                        let col = cell.col.min(cols.saturating_sub(1).max(sel.anchor.col));
                        if matches!(self.drag, Drag::Rows) {
                            sel.cursor = CellPos {
                                row,
                                col: cols.saturating_sub(1),
                            };
                        } else {
                            sel.cursor = CellPos { row, col };
                        }
                    }
                }
                Drag::None => {}
            }
        } else {
            self.drag = Drag::None;
        }

        // --- keyboard ----------------------------------------------------------
        let menu_open = resp.context_menu_opened();
        if io.keyboard && !menu_open {
            self.keyboard(ui, rows, &io.editable, &mut out);
        }

        // --- paint -------------------------------------------------------------
        let starts = self.col_starts();
        let cols = self.widths.len();
        let painter = ui.painter().clone();
        painter.rect_filled(rect, 0.0, theme.content_bg);
        let body_p = painter.with_clip_rect(body);

        let first_row = (self.sy.offset / ROW_H as f64).floor() as u64;
        let y0 = body.top() - (self.sy.offset - first_row as f64 * ROW_H as f64) as f32;
        let n_visible =
            ((body.height() / ROW_H).ceil() as u64 + 1).min(shown_rows.saturating_sub(first_row));
        let first_col = starts
            .partition_point(|&s| s <= self.sx.offset)
            .saturating_sub(1);
        let x_of = |c: usize| body.left() + (starts[c.min(cols)] - self.sx.offset) as f32;
        let last_col = {
            let mut c = first_col;
            while c < cols && x_of(c) < body.right() {
                c += 1;
            }
            c
        };
        // Right edge of column `c` (the ghost column is narrow).
        let x_end = |c: usize| {
            if c < cols {
                x_of(c + 1)
            } else {
                x_of(cols) + GHOST_COL_W
            }
        };
        let row_y = |r: u64| y0 + (r - first_row) as f32 * ROW_H;
        let last_visible = first_row + n_visible;

        // Zebra striping.
        for r in first_row..last_visible {
            if r % 2 == 1 {
                let y = row_y(r);
                body_p.rect_filled(
                    Rect::from_min_max(pos2(body.left(), y), pos2(body.right(), y + ROW_H)),
                    0.0,
                    theme.gutter_bg.gamma_multiply(0.6),
                );
            }
        }

        // Selection.
        if let Some(sel) = self.sel {
            let (r0, r1) = sel.rows();
            let (c0, c1) = sel.cols();
            let max_col = cols + self.ghost_col as usize;
            if r1 >= first_row && r0 < last_visible && c0 < max_col {
                let top = if r0 < first_row {
                    body.top() - 1.0
                } else {
                    row_y(r0)
                };
                let bottom = if r1 >= last_visible {
                    body.bottom() + 1.0
                } else {
                    row_y(r1) + ROW_H
                };
                let c1 = c1.min(max_col - 1);
                let r = Rect::from_min_max(pos2(x_of(c0), top), pos2(x_end(c1), bottom));
                body_p.rect_filled(r, 0.0, theme.selection_bg);
            }
        }

        // Find highlights.
        if !io.matches.is_empty() && n_visible > 0 {
            let lo = io.matches.partition_point(|&(r, _)| r < first_row);
            let hi = io.matches.partition_point(|&(r, _)| r < last_visible);
            for (k, &(r, c)) in io.matches[lo..hi].iter().enumerate() {
                let c = c as usize;
                if c >= cols {
                    continue;
                }
                let cell = Rect::from_min_max(
                    pos2(x_of(c), row_y(r)),
                    pos2(x_of(c + 1), row_y(r) + ROW_H),
                );
                body_p.rect_filled(cell.shrink(1.0), 2.0, theme.highlight_bg);
                if io.current_match == Some(lo + k) {
                    body_p.rect_stroke(
                        cell.shrink(1.0),
                        2.0,
                        Stroke::new(1.5, theme.accent),
                        egui::StrokeKind::Inside,
                    );
                }
            }
        }

        // Cell text.
        let text_color = theme.text;
        let mut max_cells = 0usize;
        // Numbers are never elided: numeric columns grow to fit what scrolls into view.
        let mut grow: Vec<(usize, f32)> = Vec::new();
        // Per visible row: (logical number, added, has edits).
        let mut labels: Vec<(Option<u64>, bool, bool)> = Vec::with_capacity(n_visible as usize);
        for r in first_row..last_visible.min(rows) {
            let Some(row) = io.view.row(r) else {
                labels.push((None, false, false));
                continue;
            };
            labels.push((io.view.logical_of(row.key()), row.is_new(), row.has_edits()));
            max_cells = max_cells.max(row.len());
            let y = row_y(r);
            for c in first_col..last_col.min(row.len()) {
                let cell = row.cell(c);
                let x = x_of(c);
                let w = self.widths[c];
                let rect_c = Rect::from_min_max(pos2(x, y), pos2(x + w, y + ROW_H));
                if cell.edited {
                    paint_edited(&body_p, rect_c, theme);
                }
                if cell.text.is_empty() {
                    continue;
                }
                let numeric = self.numeric.get(c).copied().unwrap_or(false);
                if numeric && cell.text.len() <= 40 {
                    let needed = text_width(ui, cell.text, &font) + 2.0 * PAD + 2.0;
                    if needed > w {
                        grow.push((c, needed.min(360.0)));
                    }
                }
                paint_cell_text(&body_p, rect_c, cell.text, &font, text_color, numeric);
            }
        }

        // Grid lines.
        let line = Stroke::new(1.0 / ui.ctx().pixels_per_point(), theme.grid_line);
        let lines_bottom = if n_visible > 0 {
            (row_y(last_visible - 1) + ROW_H).min(body.bottom())
        } else {
            body.top()
        };
        for c in first_col.max(1)..=last_col.min(cols) {
            let x = body_p.round_to_pixel_center(x_of(c));
            body_p.line_segment([pos2(x, body.top()), pos2(x, lines_bottom)], line);
        }
        if self.ghost_row && rows >= first_row && rows < last_visible {
            // The "+" row: a faint top rule.
            let y = body_p.round_to_pixel_center(row_y(rows));
            body_p.line_segment([pos2(body.left(), y), pos2(body.right(), y)], line);
        }

        // Active cell.
        if let Some(sel) = self.sel {
            let c = sel.cursor;
            if c.row >= first_row && c.row < last_visible && c.col < cols + self.ghost_col as usize
            {
                let r = Rect::from_min_max(
                    pos2(x_of(c.col), row_y(c.row)),
                    pos2(x_end(c.col), row_y(c.row) + ROW_H),
                );
                body_p.rect_stroke(
                    r,
                    0.0,
                    Stroke::new(2.0, theme.selection_stroke),
                    egui::StrokeKind::Inside,
                );
            }
        }

        // Header.
        let header = Rect::from_min_max(
            pos2(body.left(), rect.top()),
            pos2(rect.right(), body.top()),
        );
        let header_p = painter.with_clip_rect(header);
        header_p.rect_filled(header, 0.0, theme.header_bg);
        let sel_cols = self.sel.map(|s| s.cols());
        let head = io.view.header();
        for c in first_col..last_col {
            let x = x_of(c);
            let w = self.widths[c];
            let cell = Rect::from_min_max(pos2(x, header.top()), pos2(x + w, header.bottom()));
            if sel_cols.is_some_and(|(a, b)| c >= a && c <= b) {
                header_p.rect_filled(cell, 0.0, theme.selection_bg.gamma_multiply(0.6));
            }
            let label = head.cell(c);
            if label.edited {
                paint_edited(&header_p, cell, theme);
            }
            let sorted = io.sort.filter(|s| s.column == c);
            let arrow_w = if sorted.is_some() { 14.0 } else { 0.0 };
            let text_rect = Rect::from_min_max(cell.min, pos2(cell.max.x - arrow_w, cell.max.y));
            paint_cell_text(&header_p, text_rect, label.text, &font, theme.text, false);
            if let Some(s) = sorted {
                let cx = cell.right() - PAD - 2.0;
                let cy = cell.center().y;
                let (a, b) = if s.direction == Direction::Ascending {
                    (-3.0, 2.0)
                } else {
                    (3.0, -2.0)
                };
                let color = if io.sort_dim {
                    theme.text_dim
                } else {
                    theme.accent
                };
                header_p.add(egui::Shape::convex_polygon(
                    vec![
                        pos2(cx - 4.0, cy + b),
                        pos2(cx + 4.0, cy + b),
                        pos2(cx, cy + a),
                    ],
                    color,
                    Stroke::NONE,
                ));
            }
            let bx = header_p.round_to_pixel_center(x + w);
            header_p.line_segment(
                [
                    pos2(bx, header.top() + 5.0),
                    pos2(bx, header.bottom() - 5.0),
                ],
                Stroke::new(1.0, theme.separator),
            );
        }
        if self.ghost_col && last_col == cols {
            let cell = Rect::from_min_max(
                pos2(x_of(cols), header.top()),
                pos2(x_of(cols) + GHOST_COL_W, header.bottom()),
            );
            let hovered = matches!(hover_hit, Some(Hit::GhostHeader));
            if hovered {
                header_p.rect_filled(
                    cell.shrink(3.0),
                    4.0,
                    theme.selection_bg.gamma_multiply(0.5),
                );
            }
            header_p.text(
                cell.center(),
                Align2::CENTER_CENTER,
                "+",
                font.clone(),
                if hovered {
                    theme.accent
                } else {
                    theme.text_dim
                },
            );
        }
        header_p.line_segment(
            [
                pos2(header.left(), header.bottom() - 0.5),
                pos2(header.right(), header.bottom() - 0.5),
            ],
            Stroke::new(1.0, theme.separator),
        );

        // Gutter: the row number a save would write (unsorted: the file row).
        let gutter = Rect::from_min_max(
            pos2(rect.left(), body.top()),
            pos2(body.left(), rect.bottom()),
        );
        let gutter_p = painter.with_clip_rect(gutter);
        gutter_p.rect_filled(gutter, 0.0, theme.gutter_bg);
        let sel_rows = self.sel.map(|s| s.rows());
        let small = FontId::proportional(theme.metrics.small_text_size);
        for (i, r) in (first_row..last_visible).enumerate() {
            let y = row_y(r);
            let row_rect =
                Rect::from_min_max(pos2(gutter.left(), y), pos2(gutter.right(), y + ROW_H));
            if sel_rows.is_some_and(|(a, b)| r >= a && r <= b) {
                gutter_p.rect_filled(row_rect, 0.0, theme.selection_bg.gamma_multiply(0.6));
            }
            if r == rows {
                // The "+" row.
                gutter_p.text(
                    pos2(gutter.right() - PAD, y + ROW_H / 2.0),
                    Align2::RIGHT_CENTER,
                    "+",
                    font.clone(),
                    theme.text_dim,
                );
                continue;
            }
            let Some(&(n, added, edited)) = labels.get(i) else {
                continue;
            };
            if let Some(n) = n {
                gutter_p.text(
                    pos2(gutter.right() - PAD, y + ROW_H / 2.0),
                    Align2::RIGHT_CENTER,
                    (n + 1).to_string(),
                    small.clone(),
                    if added {
                        theme.accent.gamma_multiply(0.8)
                    } else {
                        theme.text_dim
                    },
                );
            }
            if added {
                gutter_p.text(
                    pos2(gutter.left() + 5.0, y + ROW_H / 2.0),
                    Align2::LEFT_CENTER,
                    "+",
                    small.clone(),
                    theme.accent.gamma_multiply(0.7),
                );
            }
            if edited || added {
                gutter_p.rect_filled(
                    Rect::from_min_max(
                        pos2(gutter.right() - 2.0, y + 1.0),
                        pos2(gutter.right(), y + ROW_H - 1.0),
                    ),
                    0.0,
                    theme.accent,
                );
            }
        }
        let gx = painter.round_to_pixel_center(gutter.right());
        painter.line_segment(
            [pos2(gx, rect.top()), pos2(gx, rect.bottom())],
            Stroke::new(1.0, theme.separator),
        );

        // Corner.
        let corner = Rect::from_min_max(rect.min, pos2(body.left(), body.top()));
        painter.rect_filled(corner, 0.0, theme.header_bg);

        // Scrollbars.
        let vbar = Rect::from_min_max(
            pos2(body.right() - 12.0, body.top()),
            pos2(body.right(), body.bottom() - 12.0),
        );
        let hbar = Rect::from_min_max(
            pos2(body.left(), body.bottom() - 12.0),
            pos2(body.right() - 12.0, body.bottom()),
        );
        scrollbar(ui, id.with("vbar"), vbar, &mut self.sy, true);
        scrollbar(ui, id.with("hbar"), hbar, &mut self.sx, false);

        // Context menu for what was right-clicked.
        let menu_cx = self.menu_hit.map(|hit| MenuCx {
            hit,
            sel: self.sel.and_then(|s| s.clamped(rows.max(1), cols.max(1))),
            rows,
            cols,
            sort: io.sort,
            sorted: io.sorted,
            editable: io.editable,
        });
        resp.context_menu(|ui| {
            if let Some(cx) = &menu_cx {
                menu::show(ui, cx, &mut out.actions);
            } else {
                ui.close();
            }
        });

        // Rows wider than the sampled header grow the column set.
        if max_cells > self.widths.len() {
            self.ensure_cols(max_cells);
        }
        if !grow.is_empty() {
            for (c, w) in grow {
                if let Some(cw) = self.widths.get_mut(c) {
                    *cw = cw.max(w);
                }
            }
            ui.ctx().request_repaint();
        }
        out
    }

    /// Adjust the selection for a right-click and say what the menu is about.
    fn context_select(&mut self, hit: Hit, rows: u64) -> Option<MenuHit> {
        let cols = self.cols();
        let last_col = cols.saturating_sub(1);
        match hit {
            Hit::Body(cell) => {
                if !self.sel.is_some_and(|s| s.contains(cell)) {
                    self.sel = Some(Selection::single(cell));
                }
                Some(MenuHit::Cells)
            }
            Hit::Gutter { row } => {
                let inside = self.sel.is_some_and(|s| {
                    let (a, b) = s.rows();
                    row >= a && row <= b && s.cols() == (0, last_col)
                });
                if !inside {
                    self.sel = Some(Selection {
                        anchor: CellPos { row, col: 0 },
                        cursor: CellPos { row, col: last_col },
                    });
                }
                Some(MenuHit::Rows)
            }
            Hit::Header { col, border: None } => {
                let inside = self.sel.is_some_and(|s| {
                    let (a, b) = s.cols();
                    col >= a && col <= b && s.rows() == (0, rows.saturating_sub(1))
                });
                if !inside && rows > 0 {
                    self.sel = Some(Selection {
                        anchor: CellPos { row: 0, col },
                        cursor: CellPos { row: rows - 1, col },
                    });
                }
                Some(MenuHit::Header(col))
            }
            _ => None,
        }
    }

    fn keyboard(&mut self, ui: &mut Ui, rows: u64, editable: &Editable, out: &mut GridOut) {
        // 1. Clipboard.
        ui.input_mut(|i| {
            if i.events.iter().any(|e| matches!(e, Event::Copy)) {
                out.actions.push(Action::Copy);
            }
            i.events.retain(|e| match e {
                Event::Cut => {
                    out.actions.push(Action::Cut);
                    false
                }
                Event::Paste(s) => {
                    out.actions.push(Action::Paste(s.clone()));
                    false
                }
                _ => true,
            });
        });
        // 2–4. Document shortcuts.
        let mut select_all = false;
        let mut select_rows = false;
        let mut select_cols = false;
        let mut tabbed = false;
        let mut moves: Vec<(Key, bool)> = Vec::new();
        ui.input_mut(|i| {
            if consume_exact(i, CTRL_SHIFT, Key::Z) {
                out.actions.push(Action::Redo);
            }
            if consume_exact(i, CTRL, Key::Z) {
                out.actions.push(Action::Undo);
            }
            if consume_exact(i, CTRL, Key::Y) {
                out.actions.push(Action::Redo);
            }
            select_all = consume_exact(i, CTRL, Key::A);
            if consume_exact(i, CTRL, Key::D) {
                out.actions.push(Action::FillDown);
            }
            if consume_exact(i, CTRL, Key::R) {
                out.actions.push(Action::FillRight);
            }
            if consume_exact(i, CTRL_SHIFT, Key::Equals)
                || consume_exact(i, CTRL, Key::Plus)
                || consume_exact(i, CTRL_SHIFT, Key::Plus)
            {
                out.actions.push(Action::InsertAuto);
            }
            if consume_exact(i, CTRL, Key::Minus) {
                out.actions.push(Action::DeleteAuto);
            }
            // 5. Whole rows / columns.
            if consume_exact(i, SHIFT, Key::Space) {
                select_rows = true;
                i.events
                    .retain(|e| !matches!(e, Event::Text(t) if t == " "));
            }
            select_cols = consume_exact(i, CTRL, Key::Space);
            // 6. Edit / clear.
            if consume_exact(i, NONE, Key::F2) {
                out.actions.push(Action::Edit {
                    mode: Mode::Edit,
                    seed: None,
                });
            }
            if consume_exact(i, NONE, Key::Backspace) {
                out.actions.push(Action::Edit {
                    mode: Mode::Enter,
                    seed: Some(String::new()),
                });
            }
            if consume_exact(i, NONE, Key::Delete) {
                out.actions.push(Action::Clear);
            }
            // 7. Enter / Tab move (exact modifiers).
            for (mods, key) in [
                (SHIFT, Key::Enter),
                (NONE, Key::Enter),
                (SHIFT, Key::Tab),
                (NONE, Key::Tab),
            ] {
                while consume_exact(i, mods, key) {
                    moves.push((key, mods.shift));
                    tabbed |= key == Key::Tab;
                }
            }
        });
        if tabbed {
            // Tab must not also move egui's focus to some button.
            ui.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
        }
        if select_all {
            self.select_all(rows);
        }
        let cols = self.cols();
        if let Some(s) = self.sel.as_mut() {
            if select_rows && cols > 0 {
                s.anchor.col = 0;
                s.cursor.col = cols - 1;
            }
            if select_cols && rows > 0 {
                s.anchor.row = 0;
                s.cursor.row = rows - 1;
            }
        }
        // The "+" row and column are reachable with the keyboard too.
        let max_row = (rows + editable.ghost_row as u64).saturating_sub(1);
        let max_col = (cols + editable.ghost_col as usize).saturating_sub(1);
        if rows + editable.ghost_row as u64 == 0 || cols + editable.ghost_col as usize == 0 {
            return;
        }
        for (key, back) in moves {
            let mut c = self
                .sel
                .map(|s| s.cursor)
                .unwrap_or(CellPos { row: 0, col: 0 });
            match (key, back) {
                (Key::Enter, false) => {
                    c.row = (c.row + 1).min(max_row);
                    if let Some(o) = self.tab_origin.take() {
                        c.col = o.min(max_col);
                    }
                }
                (Key::Enter, true) => {
                    c.row = c.row.saturating_sub(1);
                    self.tab_origin = None;
                }
                (Key::Tab, false) => {
                    self.tab_origin.get_or_insert(c.col);
                    c.col = (c.col + 1).min(max_col);
                }
                (_, _) => {
                    c.col = c.col.saturating_sub(1);
                }
            }
            self.sel = Some(Selection::single(c));
            self.reveal(c);
        }

        // 8. Arrows, pages, Home/End.
        let page = ((self.body.height() / ROW_H).floor() as u64)
            .saturating_sub(1)
            .max(1);
        let last_row = rows.saturating_sub(1);
        let last_col = cols.saturating_sub(1);
        let mut moved = None::<(CellPos, bool)>;
        ui.input_mut(|i| {
            let cur = self
                .sel
                .map(|s| s.cursor)
                .unwrap_or(CellPos { row: 0, col: 0 });
            let keys = [
                Key::ArrowUp,
                Key::ArrowDown,
                Key::ArrowLeft,
                Key::ArrowRight,
                Key::PageUp,
                Key::PageDown,
                Key::Home,
                Key::End,
            ];
            for key in keys {
                for (mods, extend, jump) in [
                    (CTRL_SHIFT, true, true),
                    (SHIFT, true, false),
                    (CTRL, false, true),
                    (NONE, false, false),
                ] {
                    if !consume_exact(i, mods, key) {
                        continue;
                    }
                    // Extending a selection stays on real cells.
                    let (mr, mc) = if extend {
                        (last_row, last_col)
                    } else {
                        (max_row, max_col)
                    };
                    let mut c = cur;
                    match (key, jump) {
                        (Key::ArrowUp, false) => c.row = c.row.saturating_sub(1),
                        (Key::ArrowDown, false) => c.row = (c.row + 1).min(mr),
                        (Key::ArrowLeft, false) => c.col = c.col.saturating_sub(1),
                        (Key::ArrowRight, false) => c.col = (c.col + 1).min(mc),
                        (Key::ArrowUp, true) => c.row = 0,
                        (Key::ArrowDown, true) => c.row = last_row,
                        (Key::ArrowLeft, true) | (Key::Home, false) => c.col = 0,
                        (Key::ArrowRight, true) | (Key::End, false) => c.col = last_col,
                        (Key::PageUp, _) => c.row = c.row.saturating_sub(page),
                        (Key::PageDown, _) => c.row = (c.row + page).min(last_row.max(c.row)),
                        (Key::Home, true) => c = CellPos { row: 0, col: 0 },
                        (Key::End, true) => {
                            c = CellPos {
                                row: last_row,
                                col: last_col,
                            }
                        }
                        _ => {}
                    }
                    moved = Some((c, extend));
                }
            }
        });
        if let Some((c, extend)) = moved {
            self.tab_origin = None;
            self.sel = Some(match (extend, self.sel) {
                (true, Some(s)) => Selection {
                    anchor: s.anchor,
                    cursor: c,
                },
                _ => Selection::single(c),
            });
            self.reveal(c);
        }

        // 9. Typing starts editing the cursor cell.
        let typed = ui.input_mut(take_text);
        if !typed.is_empty() && self.sel.is_some() {
            out.actions.push(Action::Edit {
                mode: Mode::Enter,
                seed: Some(typed),
            });
        }
    }

    /// Double-click on a header border: fit the column to its visible content.
    fn autofit(&mut self, ui: &Ui, col: usize, view: &mut SheetView<'_>, font: &FontId) {
        let first_row = (self.sy.offset / ROW_H as f64).floor() as u64;
        let n = (self.body.height() / ROW_H).ceil() as u64 + 1;
        let mut w: f32 = text_width(ui, view.header().cell(col).text, font);
        for r in first_row..(first_row + n).min(view.rows()) {
            if let Some(row) = view.row(r) {
                w = w.max(text_width(ui, row.cell(col).text, font));
            }
        }
        if let Some(cw) = self.widths.get_mut(col) {
            *cw = (w + 2.0 * PAD + 2.0).clamp(MIN_COL_W, 800.0);
        }
    }
}

/// Edited cells: a tinted background and a small corner mark.
fn paint_edited(painter: &egui::Painter, cell: Rect, theme: &Theme) {
    painter.rect_filled(cell.shrink(0.5), 0.0, theme.edited_bg);
    let m = 5.0;
    painter.add(egui::Shape::convex_polygon(
        vec![
            cell.left_top() + egui::vec2(1.0, 1.0),
            cell.left_top() + egui::vec2(1.0 + m, 1.0),
            cell.left_top() + egui::vec2(1.0, 1.0 + m),
        ],
        theme.edited_mark,
        Stroke::NONE,
    ));
}

pub fn text_width(ui: &Ui, text: &str, font: &FontId) -> f32 {
    let text: String = text.chars().take(MAX_CELL_CHARS).collect();
    ui.painter()
        .layout_no_wrap(text, font.clone(), Color32::WHITE)
        .size()
        .x
}

fn paint_cell_text(
    painter: &egui::Painter,
    cell: Rect,
    text: &str,
    font: &FontId,
    color: Color32,
    right_align: bool,
) {
    let avail = cell.width() - 2.0 * PAD;
    if avail < 4.0 || text.is_empty() {
        return;
    }
    whir_core::theme::note_text(text);
    // One visual line: newlines inside quoted cells show as spaces.
    let mut s: String = text.chars().take(MAX_CELL_CHARS).collect();
    if s.contains(['\n', '\r', '\t']) {
        s = s.replace(['\n', '\r', '\t'], " ");
    }
    let mut job = LayoutJob::single_section(s, TextFormat::simple(font.clone(), color));
    job.wrap = TextWrapping {
        max_width: avail,
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    let galley = painter.layout_job(job);
    let x = if right_align {
        cell.right() - PAD - galley.size().x
    } else {
        cell.left() + PAD
    };
    let y = cell.center().y - galley.size().y / 2.0;
    painter
        .with_clip_rect(cell.intersect(painter.clip_rect()))
        .galley(pos2(x, y), galley, color);
}

/// Estimate column widths and numeric-ness from the header and the first
/// `sample_rows` rows.
pub fn measure_columns(
    ui: &Ui,
    grid: &mut Grid,
    view: &mut SheetView<'_>,
    sample_rows: u64,
    theme: &Theme,
) {
    let font = FontId::proportional(theme.metrics.body_text_size);
    let avg_char = ui.fonts_mut(|f| {
        let s = "etaoinshrdlucmfwyp0123456789";
        s.chars().map(|c| f.glyph_width(&font, c)).sum::<f32>() / s.chars().count() as f32
    });
    let head = view.header();
    let header: Vec<String> = (0..head.len())
        .map(|c| head.cell(c).text.to_owned())
        .collect();
    let mut max_chars: Vec<usize> = header.iter().map(|h| h.chars().count()).collect();
    let mut numeric_count: Vec<u64> = vec![0; header.len()];
    let mut non_empty: Vec<u64> = vec![0; header.len()];
    for r in 0..sample_rows.min(view.rows()) {
        let Some(row) = view.row(r) else { break };
        for c in 0..row.len() {
            if c >= max_chars.len() {
                max_chars.resize(c + 1, 0);
                numeric_count.resize(c + 1, 0);
                non_empty.resize(c + 1, 0);
            }
            let cell = row.cell(c).text;
            max_chars[c] = max_chars[c].max(cell.chars().take(80).count());
            if !cell.trim().is_empty() {
                non_empty[c] += 1;
                numeric_count[c] += crate::sort::parse_number(cell.as_bytes()).is_some() as u64;
            }
        }
    }
    let n = max_chars.len();
    grid.widths = (0..n)
        .map(|c| {
            let h = header.get(c).map_or("", String::as_str);
            let header_w = text_width(ui, h, &font) + 2.0 * PAD + 16.0;
            let content_w = max_chars[c].min(48) as f32 * avg_char + 2.0 * PAD + 4.0;
            header_w.max(content_w).clamp(56.0, 360.0)
        })
        .collect();
    grid.numeric = (0..n)
        .map(|c| non_empty[c] > 0 && numeric_count[c] * 10 >= non_empty[c] * 9)
        .collect();
}
