//! The virtualized table: only visible rows *and* columns are laid out and
//! painted, positions are tracked in f64 so billions of points scroll exactly.

use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{
    Align2, Color32, CursorIcon, Event, FontId, Key, Modifiers, PointerButton, Pos2, Rect, Sense,
    Stroke, Ui, pos2,
};
use whir_core::scroll::{ScrollAxis, scrollbar};
use whir_core::theme::Theme;

use crate::sort::{Direction, SortSpec};
use crate::table::Table;

pub const ROW_H: f32 = 22.0;
pub const HEADER_H: f32 = 26.0;
const PAD: f32 = 8.0;
const MIN_COL_W: f32 = 36.0;
pub const DEFAULT_COL_W: f32 = 110.0;
const RESIZE_GRIP: f32 = 4.0;
/// Characters laid out per cell; one line never shows more.
const MAX_CELL_CHARS: usize = 400;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellPos {
    /// Display row (0 = first data row, in current sort order).
    pub row: u64,
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
    Gutter { row: u64 },
    Body(CellPos),
    Outside,
}

/// Per-frame inputs.
pub struct GridIo<'a> {
    pub table: &'a mut Table,
    /// Data rows (excluding the header).
    pub rows: u64,
    /// Display order → data row, when sorted.
    pub perm: Option<&'a [u32]>,
    pub sort: Option<SortSpec>,
    pub sorting: bool,
    /// Find matches as (display row, col), sorted.
    pub matches: &'a [(u64, u32)],
    pub current_match: Option<usize>,
    /// The grid owns the keyboard this frame (the find box doesn't have focus).
    pub keyboard: bool,
}

#[derive(Default)]
pub struct GridOut {
    pub sort_clicked: Option<usize>,
    pub copy: bool,
}

pub struct Grid {
    pub widths: Vec<f32>,
    pub numeric: Vec<bool>,
    pub header: Vec<String>,
    pub sy: ScrollAxis,
    pub sx: ScrollAxis,
    pub sel: Option<Selection>,
    drag: Drag,
    /// Screen geometry from the last frame (for reveal()).
    body: Rect,
}

impl Default for Grid {
    fn default() -> Self {
        Self {
            widths: Vec::new(),
            numeric: Vec::new(),
            header: Vec::new(),
            sy: ScrollAxis::default(),
            sx: ScrollAxis::default(),
            sel: None,
            drag: Drag::None,
            body: Rect::NOTHING,
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

    fn ensure_cols(&mut self, n: usize) {
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

    /// Scroll so `cell` is fully visible.
    pub fn reveal(&mut self, cell: CellPos) {
        let top = cell.row as f64 * ROW_H as f64;
        self.sy.reveal(top, top + ROW_H as f64);
        let starts = self.col_starts();
        if let (Some(&a), Some(&b)) = (starts.get(cell.col), starts.get(cell.col + 1)) {
            self.sx.reveal(a, b);
        }
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

    fn hit(&self, p: Pos2, rect: Rect, gutter_w: f32, starts: &[f64], rows: u64) -> Hit {
        if !rect.contains(p) {
            return Hit::Outside;
        }
        let in_header = p.y < rect.top() + HEADER_H;
        let in_gutter = p.x < rect.left() + gutter_w;
        let col_at = |x: f32| -> Option<usize> {
            let content_x = self.sx.offset + (x - (rect.left() + gutter_w)) as f64;
            let i = starts.partition_point(|&s| s <= content_x);
            (i >= 1 && i < starts.len()).then(|| i - 1)
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
                let content_x = self.sx.offset + (p.x - (rect.left() + gutter_w)) as f64;
                let border = starts
                    .iter()
                    .enumerate()
                    .skip(1)
                    .find(|(_, s)| (content_x - **s).abs() <= RESIZE_GRIP as f64)
                    .map(|(i, _)| i - 1);
                match col_at(p.x) {
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

    pub fn show(&mut self, ui: &mut Ui, rect: Rect, io: GridIo<'_>, theme: &Theme) -> GridOut {
        let mut out = GridOut::default();
        let font = FontId::proportional(theme.metrics.body_text_size);
        let digit_w = ui.fonts_mut(|f| f.glyph_width(&font, '0'));
        let max_row_label = io.rows.max(1);
        let gutter_w = (digits(max_row_label) as f32 * digit_w + 2.0 * PAD).max(36.0);
        let body = Rect::from_min_max(
            pos2(rect.left() + gutter_w, rect.top() + HEADER_H),
            rect.max,
        );
        self.body = body;
        let starts = self.col_starts();
        let content_w = *starts.last().unwrap_or(&0.0);

        self.sy.viewport = body.height() as f64;
        self.sy.content = io.rows as f64 * ROW_H as f64 + 4.0;
        self.sx.viewport = body.width() as f64;
        self.sx.content = content_w + 24.0;
        self.sy.clamp();
        self.sx.clamp();

        let id = ui.id().with("csv-grid");
        let resp = ui.interact(rect, id, Sense::click_and_drag());
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
        let hover_hit = pointer.map(|p| self.hit(p, rect, gutter_w, &starts, io.rows));
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
        if pressed && resp.hovered() {
            if let Some(p) = pointer {
                match self.hit(p, rect, gutter_w, &starts, io.rows) {
                    Hit::Header {
                        border: Some(col), ..
                    } => {
                        self.drag = Drag::Resize {
                            col,
                            start_w: self.widths[col],
                            start_x: p.x,
                        };
                    }
                    Hit::Header { .. } | Hit::Outside => self.drag = Drag::None,
                    Hit::Corner => {
                        self.select_all(io.rows);
                        self.drag = Drag::None;
                    }
                    Hit::Gutter { row } => {
                        let last = self.cols().saturating_sub(1);
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
        }
        if resp.double_clicked()
            && let Some(Hit::Header {
                border: Some(col), ..
            }) = hover_hit
        {
            self.autofit(ui, col, io.table, io.perm, &font);
        }
        if resp.clicked()
            && let Some(Hit::Header { col, border: None }) = hover_hit
        {
            out.sort_clicked = Some(col);
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
                    if let (Hit::Body(cell), Some(sel)) = (
                        self.hit(clamped, rect, gutter_w, &starts, io.rows),
                        &mut self.sel,
                    ) {
                        if matches!(self.drag, Drag::Rows) {
                            sel.cursor = CellPos {
                                row: cell.row,
                                col: self.widths.len().saturating_sub(1),
                            };
                        } else {
                            sel.cursor = cell;
                        }
                    }
                }
                Drag::None => {}
            }
        } else {
            self.drag = Drag::None;
        }

        // --- keyboard ----------------------------------------------------------
        if io.keyboard {
            self.keyboard(ui, io.rows, &mut out);
        }

        // --- paint -------------------------------------------------------------
        let starts = self.col_starts();
        let painter = ui.painter().clone();
        painter.rect_filled(rect, 0.0, theme.content_bg);
        let body_p = painter.with_clip_rect(body);

        let first_row = (self.sy.offset / ROW_H as f64).floor() as u64;
        let y0 = body.top() - (self.sy.offset - first_row as f64 * ROW_H as f64) as f32;
        let n_visible =
            ((body.height() / ROW_H).ceil() as u64 + 1).min(io.rows.saturating_sub(first_row));
        let first_col = starts
            .partition_point(|&s| s <= self.sx.offset)
            .saturating_sub(1);
        let x_of = |c: usize| body.left() + (starts[c] - self.sx.offset) as f32;
        let last_col = {
            let mut c = first_col;
            while c < self.widths.len() && x_of(c) < body.right() {
                c += 1;
            }
            c
        };
        let row_y = |r: u64| y0 + (r - first_row) as f32 * ROW_H;

        // Zebra striping.
        for i in 0..n_visible {
            let r = first_row + i;
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
            let last_visible = first_row + n_visible;
            if r1 >= first_row && r0 < last_visible && c0 < self.widths.len() {
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
                let c1 = c1.min(self.widths.len() - 1);
                let r = Rect::from_min_max(pos2(x_of(c0), top), pos2(x_of(c1 + 1), bottom));
                body_p.rect_filled(r, 0.0, theme.selection_bg);
            }
        }

        // Find highlights.
        if !io.matches.is_empty() && n_visible > 0 {
            let lo = io.matches.partition_point(|&(r, _)| r < first_row);
            let hi = io
                .matches
                .partition_point(|&(r, _)| r < first_row + n_visible);
            for (k, &(r, c)) in io.matches[lo..hi].iter().enumerate() {
                let c = c as usize;
                if c >= self.widths.len() {
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
        for i in 0..n_visible {
            let r = first_row + i;
            let data_row = io
                .perm
                .map_or(r, |p| p.get(r as usize).copied().unwrap_or(0) as u64);
            let Some((block, idx)) = io.table.row(data_row + 1) else {
                continue;
            };
            let Some(row) = block.row(idx) else { continue };
            max_cells = max_cells.max(row.len());
            let y = row_y(r);
            for c in first_col..last_col.min(row.len()) {
                let text = row.cell(c);
                if text.is_empty() {
                    continue;
                }
                let x = x_of(c);
                let w = self.widths[c];
                let cell = Rect::from_min_max(pos2(x, y), pos2(x + w, y + ROW_H));
                let numeric = self.numeric.get(c).copied().unwrap_or(false);
                if numeric && text.len() <= 40 {
                    let needed = text_width(ui, text, &font) + 2.0 * PAD + 2.0;
                    if needed > w {
                        grow.push((c, needed.min(360.0)));
                    }
                }
                paint_cell_text(&body_p, cell, text, &font, text_color, numeric);
            }
        }

        // Grid lines.
        let line = Stroke::new(1.0 / ui.ctx().pixels_per_point(), theme.grid_line);
        for c in first_col..=last_col.min(self.widths.len()) {
            if c == 0 {
                continue;
            }
            let x = body_p.round_to_pixel_center(x_of(c));
            let bottom = if n_visible > 0 {
                row_y(first_row + n_visible - 1) + ROW_H
            } else {
                body.top()
            };
            body_p.line_segment(
                [pos2(x, body.top()), pos2(x, bottom.min(body.bottom()))],
                line,
            );
        }

        // Active cell.
        if let Some(sel) = self.sel {
            let c = sel.cursor;
            if c.row >= first_row && c.row < first_row + n_visible && c.col < self.widths.len() {
                let r = Rect::from_min_max(
                    pos2(x_of(c.col), row_y(c.row)),
                    pos2(x_of(c.col + 1), row_y(c.row) + ROW_H),
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
        for c in first_col..last_col {
            let x = x_of(c);
            let w = self.widths[c];
            let cell = Rect::from_min_max(pos2(x, header.top()), pos2(x + w, header.bottom()));
            if sel_cols.is_some_and(|(a, b)| c >= a && c <= b) {
                header_p.rect_filled(cell, 0.0, theme.selection_bg.gamma_multiply(0.6));
            }
            let sorted = io.sort.filter(|s| s.column == c);
            let arrow_w = if sorted.is_some() { 14.0 } else { 0.0 };
            let label = self.header.get(c).map(String::as_str).unwrap_or("");
            let text_rect = Rect::from_min_max(cell.min, pos2(cell.max.x - arrow_w, cell.max.y));
            paint_cell_text(&header_p, text_rect, label, &font, theme.text, false);
            if let Some(s) = sorted {
                let cx = cell.right() - PAD - 2.0;
                let cy = cell.center().y;
                let (a, b) = if s.direction == Direction::Ascending {
                    (-3.0, 2.0)
                } else {
                    (3.0, -2.0)
                };
                let color = if io.sorting {
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
        header_p.line_segment(
            [
                pos2(header.left(), header.bottom() - 0.5),
                pos2(header.right(), header.bottom() - 0.5),
            ],
            Stroke::new(1.0, theme.separator),
        );

        // Gutter with row numbers (original numbers when sorted).
        let gutter = Rect::from_min_max(
            pos2(rect.left(), body.top()),
            pos2(body.left(), rect.bottom()),
        );
        let gutter_p = painter.with_clip_rect(gutter);
        gutter_p.rect_filled(gutter, 0.0, theme.gutter_bg);
        let sel_rows = self.sel.map(|s| s.rows());
        let small = FontId::proportional(theme.metrics.small_text_size);
        for i in 0..n_visible {
            let r = first_row + i;
            let y = row_y(r);
            if sel_rows.is_some_and(|(a, b)| r >= a && r <= b) {
                gutter_p.rect_filled(
                    Rect::from_min_max(pos2(gutter.left(), y), pos2(gutter.right(), y + ROW_H)),
                    0.0,
                    theme.selection_bg.gamma_multiply(0.6),
                );
            }
            let n = io
                .perm
                .map_or(r, |p| p.get(r as usize).copied().unwrap_or(0) as u64)
                + 1;
            gutter_p.text(
                pos2(gutter.right() - PAD, y + ROW_H / 2.0),
                Align2::RIGHT_CENTER,
                n.to_string(),
                small.clone(),
                theme.text_dim,
            );
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

        // Rows wider than the sampled header grow the column set.
        if max_cells > self.widths.len() {
            self.ensure_cols(max_cells);
            self.header.resize(max_cells, String::new());
        }
        if !grow.is_empty() {
            for (c, w) in grow {
                self.widths[c] = self.widths[c].max(w);
            }
            ui.ctx().request_repaint();
        }
        out
    }

    fn keyboard(&mut self, ui: &mut Ui, rows: u64, out: &mut GridOut) {
        if ui.input(|i| i.events.iter().any(|e| matches!(e, Event::Copy))) {
            out.copy = true;
        }
        if rows == 0 || self.cols() == 0 {
            return;
        }
        let page = ((self.body.height() / ROW_H).floor() as u64)
            .saturating_sub(1)
            .max(1);
        let last_row = rows - 1;
        let last_col = self.cols() - 1;
        let mut moved = None::<(CellPos, bool)>;
        ui.input_mut(|i| {
            if i.consume_key(Modifiers::CTRL, Key::A) {
                self.select_all(rows);
            }
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
                    (Modifiers::CTRL | Modifiers::SHIFT, true, true),
                    (Modifiers::SHIFT, true, false),
                    (Modifiers::CTRL, false, true),
                    (Modifiers::NONE, false, false),
                ] {
                    if !i.consume_key(mods, key) {
                        continue;
                    }
                    let mut c = cur;
                    match (key, jump) {
                        (Key::ArrowUp, false) => c.row = c.row.saturating_sub(1),
                        (Key::ArrowDown, false) => c.row = (c.row + 1).min(last_row),
                        (Key::ArrowLeft, false) => c.col = c.col.saturating_sub(1),
                        (Key::ArrowRight, false) => c.col = (c.col + 1).min(last_col),
                        (Key::ArrowUp, true) => c.row = 0,
                        (Key::ArrowDown, true) => c.row = last_row,
                        (Key::ArrowLeft, true) | (Key::Home, false) => c.col = 0,
                        (Key::ArrowRight, true) | (Key::End, false) => c.col = last_col,
                        (Key::PageUp, _) => c.row = c.row.saturating_sub(page),
                        (Key::PageDown, _) => c.row = (c.row + page).min(last_row),
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
            self.sel = Some(match (extend, self.sel) {
                (true, Some(s)) => Selection {
                    anchor: s.anchor,
                    cursor: c,
                },
                _ => Selection::single(c),
            });
            self.reveal(c);
        }
    }

    /// Double-click on a header border: fit the column to its visible content.
    fn autofit(
        &mut self,
        ui: &Ui,
        col: usize,
        table: &mut Table,
        perm: Option<&[u32]>,
        font: &FontId,
    ) {
        let first_row = (self.sy.offset / ROW_H as f64).floor() as u64;
        let n = (self.body.height() / ROW_H).ceil() as u64 + 1;
        let mut w: f32 = self
            .header
            .get(col)
            .map_or(0.0, |h| text_width(ui, h, font));
        for r in first_row..first_row + n {
            let data_row = perm.map_or(r, |p| p.get(r as usize).copied().unwrap_or(0) as u64);
            if let Some((block, i)) = table.row(data_row + 1)
                && let Some(row) = block.row(i)
            {
                w = w.max(text_width(ui, row.cell(col), font));
            }
        }
        if let Some(cw) = self.widths.get_mut(col) {
            *cw = (w + 2.0 * PAD + 2.0).clamp(MIN_COL_W, 800.0);
        }
    }
}

fn text_width(ui: &Ui, text: &str, font: &FontId) -> f32 {
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

/// Estimate column widths and numeric-ness from a sample of rows.
pub fn measure_columns(
    ui: &Ui,
    grid: &mut Grid,
    table: &mut Table,
    sample_rows: u64,
    theme: &Theme,
) {
    let font = FontId::proportional(theme.metrics.body_text_size);
    let avg_char = ui.fonts_mut(|f| {
        let s = "etaoinshrdlucmfwyp0123456789";
        s.chars().map(|c| f.glyph_width(&font, c)).sum::<f32>() / s.chars().count() as f32
    });
    let header: Vec<String> = match table.row(0) {
        Some((b, i)) => b
            .row(i)
            .map(|r| (0..r.len()).map(|c| r.cell(c).to_owned()).collect())
            .unwrap_or_default(),
        None => Vec::new(),
    };
    let mut max_chars: Vec<usize> = header.iter().map(|h| h.chars().count()).collect();
    let mut numeric_count: Vec<u64> = vec![0; header.len()];
    let mut non_empty: Vec<u64> = vec![0; header.len()];
    for r in 1..=sample_rows {
        let Some((b, i)) = table.row(r) else { break };
        let Some(row) = b.row(i) else { break };
        for c in 0..row.len() {
            if c >= max_chars.len() {
                max_chars.resize(c + 1, 0);
                numeric_count.resize(c + 1, 0);
                non_empty.resize(c + 1, 0);
            }
            let cell = row.cell(c);
            max_chars[c] = max_chars[c].max(cell.chars().take(80).count());
            if !cell.trim().is_empty() {
                non_empty[c] += 1;
                numeric_count[c] += crate::sort::parse_number(cell.as_bytes()).is_some() as u64;
            }
        }
    }
    let n = max_chars.len();
    grid.header = header;
    grid.header.resize(n, String::new());
    grid.widths = (0..n)
        .map(|c| {
            let header_w = text_width(ui, &grid.header[c], &font) + 2.0 * PAD + 16.0;
            let content_w = max_chars[c].min(48) as f32 * avg_char + 2.0 * PAD + 4.0;
            header_w.max(content_w).clamp(56.0, 360.0)
        })
        .collect();
    grid.numeric = (0..n)
        .map(|c| non_empty[c] > 0 && numeric_count[c] * 10 >= non_empty[c] * 9)
        .collect();
}
