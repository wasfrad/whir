//! Applying what the user asked for (grid [`Action`]s, editor commits, undo)
//! to a loaded table, and following the overlay's [`Effect`]s in the view
//! state (sort order, column widths, find results).
//!
//! Every change is one transaction in the overlay, so it is one undo step.
//! Gates say what is possible right now: nothing changes while a save runs,
//! and rows are only added or removed once the file is fully indexed and no
//! sort is being computed.

use std::collections::HashSet;
use std::sync::Arc;

use egui::Context;
use whir_core::Waker;
use whir_core::find_bar::fmt_int;

use crate::edit::{Applied, ColKey, DisplayAt, Edits, Effect, MAX_EDIT_ROWS, RowKey, SelRange};
use crate::editor::{ColTarget, Editor, EditorAction, Mode, Move, RowTarget};
use crate::find::Matcher;
use crate::grid::{Action, CellPos, DEFAULT_COL_W, Editable, Gate, Selection};
use crate::keys::take_text;
use crate::loaded::Loaded;

const SAVING: &str = "Saving… editing resumes when it's done.";
const INDEXING: &str = "Rows can be added or deleted once the whole file has been read.";
const SORTING: &str = "Sorting… try again when it's done.";
/// Longest value the in-cell editor takes.
const MAX_EDIT_BYTES: usize = 256 << 10;
/// Up to this many rows, values equal to the file's aren't stored as edits.
pub(crate) const COMPARE_ROWS: u64 = 10_000;

pub(crate) fn sel_range(s: Selection) -> SelRange {
    SelRange {
        anchor: (s.anchor.row, s.anchor.col),
        cursor: (s.cursor.row, s.cursor.col),
    }
}

fn selection(r: SelRange) -> Selection {
    Selection {
        anchor: CellPos {
            row: r.anchor.0,
            col: r.anchor.1,
        },
        cursor: CellPos {
            row: r.cursor.0,
            col: r.cursor.1,
        },
    }
}

/// The file's value of a cell (untruncated): "" for added rows and columns.
pub(crate) fn base_value_of(table: &mut crate::table::Table, k: RowKey, c: ColKey) -> String {
    let Some(f) = c.as_orig() else {
        return String::new();
    };
    let file_row = match k.as_orig() {
        Some(r) => r + 1,
        None if k.is_header() => 0,
        None => return String::new(),
    };
    let Some((block, i)) = table.row(file_row) else {
        return String::new();
    };
    let Some(row) = block.row(i) else {
        return String::new();
    };
    if row.is_truncated(f) {
        table
            .read_full_row(file_row)
            .and_then(|mut v| (f < v.len()).then(|| v.swap_remove(f)))
            .unwrap_or_default()
    } else {
        row.cell(f).to_owned()
    }
}

impl Loaded {
    pub fn is_dirty(&self) -> bool {
        self.edits.is_dirty()
    }

    /// Why this table can't be edited at all, if it can't.
    pub fn read_only_reason(&self) -> Option<&'static str> {
        (self.base_rows() >= MAX_EDIT_ROWS)
            .then_some("This file has too many rows to edit (over 2 billion).")
    }

    /// What editing is allowed right now.
    pub fn editable(&self) -> Editable {
        if let Some(r) = self.read_only_reason() {
            return Editable {
                cells: Gate::No(r),
                rows: Gate::No(r),
                cols: Gate::No(r),
                ghost_row: false,
                ghost_col: false,
            };
        }
        let p = self.table.progress();
        let indexed = p.done && p.error.is_none();
        let saving = self.saving.is_some();
        let sorting = self.sorting.is_some();
        let gate = |ok: bool, why: &'static str| if ok { Gate::Yes } else { Gate::No(why) };
        Editable {
            cells: gate(!saving, SAVING),
            rows: if saving {
                Gate::No(SAVING)
            } else if !indexed {
                Gate::No(INDEXING)
            } else {
                gate(!sorting, SORTING)
            },
            cols: if saving {
                Gate::No(SAVING)
            } else {
                gate(!sorting, SORTING)
            },
            ghost_row: indexed,
            ghost_col: true,
        }
    }

    /// Say why something can't happen; true if it can.
    pub(crate) fn check(&mut self, gate: Gate) -> bool {
        match gate {
            Gate::Yes => true,
            Gate::No(why) => {
                self.set_status(why, self.now);
                false
            }
        }
    }

    /// Finish anything half-done before the document is saved or hidden.
    pub fn commit_pending(&mut self) {
        self.commit_editor(None, Move::Leave);
    }

    /// The indexer turned quoting off (unbalanced quotes): rows were numbered
    /// differently, so everything keyed by row is void.
    pub fn on_reparse(&mut self) {
        self.measured_rows = 0;
        self.grid.sel = None;
        if let Some(ed) = self.editor.take() {
            self.stale_editor = Some(ed.id);
        }
        if self.order.is_some() {
            self.install_order(None);
        }
        if self.edits.is_dirty() {
            self.status = Some((
                "This file has an unbalanced quote, so it is now shown line by line. \
                 Your unsaved edits were dropped."
                    .into(),
                f64::INFINITY,
            ));
        }
        self.edits = Edits::default();
    }

    pub fn apply(&mut self, action: Action, ui: &mut egui::Ui, waker: &Waker) {
        // Anything the user does elsewhere ends the edit in progress first.
        if self.editor.is_some() {
            self.commit_editor(Some(ui.ctx()), Move::Leave);
        }
        let now = self.now;
        match action {
            Action::ToggleSort(col) => self.toggle_sort(col, waker, now),
            Action::Sort(col, dir) => self.sort(col, Some(dir), waker, now),
            Action::OriginalOrder => self.sort(0, None, waker, now),
            Action::Copy => self.copy(ui),
            Action::Edit { mode, seed } => self.start_edit(ui.ctx(), mode, seed),
            Action::Rename(col) => self.start_rename(ui.ctx(), col),
            Action::Undo => self.undo(),
            Action::Redo => self.redo(),
            Action::Paste(text) => self.paste(&text),
            Action::RequestPaste => {
                // The shell answers with an Event::Paste next frame.
                ui.ctx()
                    .send_viewport_cmd(egui::ViewportCommand::RequestPaste);
            }
            Action::Cut => {
                if self.check(self.editable().cells) {
                    self.copy(ui);
                    self.clear("Cut");
                }
            }
            Action::Clear => self.clear("Clear"),
            Action::FillDown => self.fill(true),
            Action::FillRight => self.fill(false),
            _ => {}
        }
    }

    fn copy(&mut self, ui: &egui::Ui) {
        let Some((text, cells, truncated)) = self.copy_text() else {
            return;
        };
        ui.ctx().copy_text(text);
        let msg = if truncated {
            format!(
                "Copied the first {} cells (selection too large)",
                fmt_int(cells)
            )
        } else {
            format!(
                "Copied {} cell{}",
                fmt_int(cells),
                if cells == 1 { "" } else { "s" }
            )
        };
        self.set_status(msg, self.now);
    }

    // --- values -------------------------------------------------------------

    /// The file's value of a cell (untruncated).
    pub(crate) fn base_value(&mut self, k: RowKey, c: ColKey) -> String {
        base_value_of(&mut self.table, k, c)
    }

    /// A cell's current value (edited or from the file).
    pub(crate) fn value(&mut self, k: RowKey, c: ColKey) -> String {
        match self.edits.get(k, c) {
            Some(v) => v.to_owned(),
            None => self.base_value(k, c),
        }
    }

    /// `v` as an override: `None` when it equals the file's value.
    pub(crate) fn normalized(&mut self, k: RowKey, c: ColKey, v: &str) -> Option<Arc<str>> {
        (self.base_value(k, c) != v).then(|| Arc::from(v))
    }

    // --- the cell editor ------------------------------------------------------

    /// Start editing the cursor cell (`seed`: typed text replacing the value).
    pub fn start_edit(&mut self, ctx: &Context, mode: Mode, seed: Option<String>) {
        let Some(sel) = self.grid.sel else { return };
        let cur = sel.cursor;
        let gates = self.editable();
        let rows = self.rows();
        let cols = self.grid.cols();
        let row = if cur.row >= rows {
            if !gates.ghost_row || !self.check(gates.rows) {
                return;
            }
            RowTarget::GhostRow
        } else {
            match self.view().key(cur.row) {
                Some(k) => RowTarget::Key(k),
                None => return,
            }
        };
        let col = if cur.col >= cols {
            if !self.check(gates.cols) {
                return;
            }
            ColTarget::GhostCol
        } else {
            ColTarget::Key(self.edits.col_key(cur.col))
        };
        if !self.check(gates.cells) {
            return;
        }
        let current = match (row, col) {
            (RowTarget::Key(k), ColTarget::Key(c)) => self.value(k, c),
            _ => String::new(),
        };
        if current.len() > MAX_EDIT_BYTES && seed.is_none() {
            self.set_status(
                "This cell is too long to edit here (over 256 KB).",
                self.now,
            );
            return;
        }
        // The typed text is the seed: the editor must not see it again.
        ctx.input_mut(take_text);
        let text = seed.unwrap_or_else(|| current.clone());
        let ed = Editor {
            row,
            col,
            text,
            original: current,
            mode,
            id: self.editor_id,
            rect: egui::Rect::NOTHING,
            sel_before: Some(sel),
        };
        ed.prime(ctx, false);
        self.grid.reveal(cur);
        self.editor = Some(ed);
    }

    /// Rename column `col` (header menu, or right after adding a column).
    pub fn start_rename(&mut self, ctx: &Context, col: usize) {
        if !self.check(self.editable().cols) || col >= self.grid.cols() {
            return;
        }
        let k = self.edits.col_key(col);
        let current = self.value(RowKey::HEADER, k);
        let ed = Editor {
            row: RowTarget::Header,
            col: ColTarget::Key(k),
            text: current.clone(),
            original: current,
            mode: Mode::Edit,
            id: self.editor_id,
            rect: egui::Rect::NOTHING,
            sel_before: self.grid.sel,
        };
        ed.prime(ctx, true);
        self.grid.reveal_col(col);
        self.editor = Some(ed);
    }

    /// The editor's keys (before the grid and the text field see them).
    pub fn editor_keys(&mut self, ui: &mut egui::Ui) {
        let Some(ed) = self.editor.as_mut() else {
            return;
        };
        match ed.consume_keys(ui) {
            Some(EditorAction::Commit(m)) => self.commit_editor(Some(ui.ctx()), m),
            Some(EditorAction::CommitFill) => self.commit_fill(ui.ctx()),
            Some(EditorAction::Cancel) => self.cancel_editor(Some(ui.ctx())),
            None => {}
        }
    }

    fn close_editor(&mut self, ctx: Option<&Context>) -> Option<Editor> {
        let ed = self.editor.take()?;
        match ctx {
            Some(ctx) => ed.close(ctx),
            None => self.stale_editor = Some(ed.id),
        }
        Some(ed)
    }

    pub fn cancel_editor(&mut self, ctx: Option<&Context>) {
        self.close_editor(ctx);
    }

    /// Where the editor's cell is (display row, col), if it still exists.
    fn editor_cell(&self, ed: &Editor) -> Option<CellPos> {
        let row = match ed.row {
            RowTarget::Key(k) => self.display_of(k)?,
            RowTarget::GhostRow | RowTarget::Header => self.rows(),
        };
        let col = match ed.col {
            ColTarget::Key(k) => self.edits.col_of(k)?,
            ColTarget::GhostCol => self.grid.cols(),
        };
        Some(CellPos { row, col })
    }

    /// Write the editor's value (one undo step), then move the cursor.
    pub fn commit_editor(&mut self, ctx: Option<&Context>, then: Move) {
        let Some(ed) = self.close_editor(ctx) else {
            return;
        };
        let Some(mut at) = self.editor_cell(&ed) else {
            return;
        };
        let header = ed.row == RowTarget::Header;
        if ed.text != ed.original {
            let before = ed.sel_before.map(sel_range);
            let rows = self.rows();
            let width = self.grid.cols();
            let order_at = self.order.as_ref().map(|o| DisplayAt {
                at: o.len(),
                generation: o.generation,
            });
            let logical_end = self.edits.logical_len(self.base_rows());
            let label = match (ed.row, ed.col) {
                (RowTarget::Header, _) => "Rename column",
                (RowTarget::GhostRow, _) => "Add row",
                (_, ColTarget::GhostCol) => "Add column",
                _ => "Edit cell",
            };
            // Values compared with the file's before the transaction borrows the overlay.
            let value = match (ed.row, ed.col) {
                (RowTarget::Key(r), ColTarget::Key(c)) => self.normalized(r, c, &ed.text),
                (RowTarget::Header, ColTarget::Key(c)) => {
                    self.normalized(RowKey::HEADER, c, &ed.text)
                }
                _ => Some(Arc::from(ed.text.as_str())),
            };
            let after = Some(sel_range(Selection::single(at)));
            let mut tx = self.edits.tx(label);
            let row = match ed.row {
                RowTarget::Key(k) => k,
                RowTarget::Header => RowKey::HEADER,
                RowTarget::GhostRow => tx.insert_rows(logical_end, 1, order_at)[0],
            };
            let col = match ed.col {
                ColTarget::Key(k) => k,
                ColTarget::GhostCol => {
                    let k = tx.insert_cols(width, 1, width)[0];
                    let name = if header {
                        ed.text.clone()
                    } else {
                        format!("Column {}", width + 1)
                    };
                    tx.set_cells([(RowKey::HEADER, k, Some(Arc::from(name)))]);
                    k
                }
            };
            if !(header && ed.col == ColTarget::GhostCol) {
                tx.set_cells([(row, col, value)]);
            }
            let applied = tx.commit(before, after);
            self.follow(&applied.effects);
            if ed.row == RowTarget::GhostRow {
                at.row = rows; // the new row sits where the "+" row was
            }
        }
        if !header && then != Move::Leave {
            self.move_after_commit(at, then);
        }
    }

    fn move_after_commit(&mut self, at: CellPos, then: Move) {
        let e = self.editable();
        let max_row = (self.rows() + e.ghost_row as u64).saturating_sub(1);
        let max_col = (self.grid.cols() + e.ghost_col as usize).saturating_sub(1);
        let mut c = at;
        match then {
            Move::Stay | Move::Leave => {}
            Move::Up => c.row = c.row.saturating_sub(1),
            Move::Down => c.row = (c.row + 1).min(max_row),
            Move::Left => c.col = c.col.saturating_sub(1),
            Move::Right => c.col = (c.col + 1).min(max_col),
            Move::Enter => {
                c.row = (c.row + 1).min(max_row);
                if let Some(o) = self.grid.tab_origin.take() {
                    c.col = o.min(max_col);
                }
            }
            Move::Tab => {
                self.grid.tab_origin.get_or_insert(at.col);
                c.col = (c.col + 1).min(max_col);
            }
            Move::BackTab => c.col = c.col.saturating_sub(1),
        }
        if !matches!(then, Move::Tab | Move::Enter | Move::Stay | Move::Leave) {
            self.grid.tab_origin = None;
        }
        self.grid.sel = Some(Selection::single(c));
        self.grid.reveal(c);
    }

    /// Ctrl+Enter: the editor's value into every selected cell.
    fn commit_fill(&mut self, ctx: &Context) {
        let sel = self.editor.as_ref().and_then(|e| e.sel_before);
        let range = sel.and_then(|s| s.clamped(self.rows(), self.grid.cols()));
        match range {
            Some(r) if r.cell_count() > 1 => {
                let Some(ed) = self.close_editor(Some(ctx)) else {
                    return;
                };
                let value: Arc<str> = Arc::from(ed.text.as_str());
                self.edit_range(r, "Fill", |_, _, _| Some(value.clone()));
                self.grid.sel = sel;
            }
            _ => self.commit_editor(Some(ctx), Move::Stay),
        }
    }

    /// Show the editor over its cell (after the grid, so it's on top).
    pub fn show_editor(&mut self, ui: &mut egui::Ui, theme: &whir_core::theme::Theme) {
        if let Some(id) = self.stale_editor.take() {
            crate::editor::forget(ui.ctx(), id);
        }
        let Some(ed) = self.editor.as_ref() else {
            return;
        };
        // A press anywhere else commits (the grid already moved the selection).
        let outside = ed.rect.is_positive()
            && ui.input(|i| {
                i.pointer.any_pressed()
                    && i.pointer
                        .press_origin()
                        .is_some_and(|p| !ed.rect.contains(p))
            });
        if outside {
            self.commit_editor(Some(ui.ctx()), Move::Leave);
            return;
        }
        let Some(at) = self.editor_cell(ed) else {
            self.cancel_editor(Some(ui.ctx()));
            return;
        };
        let header = ed.is_header();
        // Typing into a cell scrolled out of view brings it back.
        if !header
            && ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Text(_))))
            && !self
                .grid
                .body_rect()
                .contains_rect(self.grid.cell_rect(at.row, at.col))
        {
            self.grid.reveal(at);
        }
        let (cell, clip) = if header {
            (self.grid.header_rect(at.col), self.grid.header_strip())
        } else {
            (self.grid.cell_rect(at.row, at.col), self.grid.body_rect())
        };
        if let Some(ed) = self.editor.as_mut() {
            ed.show(ui, cell, clip, theme);
        }
    }

    // --- undo / redo --------------------------------------------------------

    fn undo(&mut self) {
        if !self.check(self.editable().cells) || self.sorting_blocks() {
            return;
        }
        match self.edits.undo() {
            Some(a) => self.after_history(a, "Undid"),
            None => self.set_status("Nothing to undo", self.now),
        }
    }

    fn redo(&mut self) {
        if !self.check(self.editable().cells) || self.sorting_blocks() {
            return;
        }
        match self.edits.redo() {
            Some(a) => self.after_history(a, "Redid"),
            None => self.set_status("Nothing to redo", self.now),
        }
    }

    /// Undo can add or remove rows, which a sort being computed can't follow.
    fn sorting_blocks(&mut self) -> bool {
        self.sorting.is_some() && !self.check(Gate::No(SORTING))
    }

    fn after_history(&mut self, a: Applied, verb: &str) {
        self.follow(&a.effects);
        if let Some(s) = a.select {
            let e = self.editable();
            let rows = self.rows() + e.ghost_row as u64;
            let cols = self.grid.cols() + e.ghost_col as usize;
            if let Some(s) = selection(s).clamped(rows, cols) {
                self.grid.sel = Some(s);
                self.grid.reveal(s.cursor);
            }
        }
        self.set_status(format!("{verb} {}", a.label.to_lowercase()), self.now);
    }

    // --- following effects ------------------------------------------------------

    /// Keep the view in step with a change of the overlay.
    pub(crate) fn follow(&mut self, effects: &[Effect]) {
        let mut remap = false;
        let mut refind = false;
        for e in effects {
            match e {
                Effect::Cells(cells) => {
                    // Sorting is a view: edits don't re-sort, the arrow dims.
                    if let Some(o) = &mut self.order {
                        o.stale = true;
                    }
                    self.retest_find(cells);
                }
                Effect::RowsInserted { keys, display } => {
                    if let Some(o) = &mut self.order {
                        let at = match display {
                            Some(d) if d.generation == o.generation => d.at,
                            _ => o.len(),
                        };
                        o.insert(at, keys);
                    }
                    remap = true;
                }
                Effect::RowsRemoved { keys } => {
                    if let Some(o) = &mut self.order {
                        o.remove_keys(keys);
                    }
                    remap = true;
                }
                Effect::ColsInserted { positions, meta } => {
                    for (i, &p) in positions.iter().enumerate() {
                        let m = meta.as_ref().and_then(|m| m.get(i));
                        let p = p.min(self.grid.widths.len());
                        self.grid
                            .widths
                            .insert(p, m.map_or(DEFAULT_COL_W, |m| m.width));
                        self.grid.numeric.insert(p, m.is_some_and(|m| m.numeric));
                        if let Some(spec) = self.order.as_mut().and_then(|o| o.spec.as_mut())
                            && spec.column >= p
                        {
                            spec.column += 1;
                        }
                    }
                    refind = true;
                }
                Effect::ColsRemoved { positions } => {
                    for &p in positions.iter().rev() {
                        if p < self.grid.widths.len() {
                            self.grid.widths.remove(p);
                            self.grid.numeric.remove(p);
                        }
                        if let Some(o) = self.order.as_mut() {
                            match o.spec {
                                // The sorted column is gone: keep the order as a custom one.
                                Some(s) if s.column == p => o.spec = None,
                                Some(ref mut s) if s.column > p => s.column -= 1,
                                _ => {}
                            }
                        }
                    }
                    refind = true;
                }
            }
            if matches!(e, Effect::ColsInserted { .. } | Effect::ColsRemoved { .. }) {
                // Widths are now the user's: never re-measure over them.
                self.measured_rows = u64::MAX;
            }
        }
        if refind && !self.find.searched.is_empty() {
            // Column positions moved: search again.
            let (q, waker) = (self.find.searched.clone(), self.waker.clone());
            self.start_find(q, &waker);
        } else if remap {
            self.remap_matches();
        }
    }

    /// Re-test edited cells against the current search and patch the results.
    fn retest_find(&mut self, cells: &[(RowKey, ColKey)]) {
        if self.find.searched.is_empty() || cells.is_empty() {
            return;
        }
        let m = Matcher::new(&self.find.searched, self.table.encoding);
        let mut touched: HashSet<(RowKey, u32)> = HashSet::with_capacity(cells.len());
        let mut hits = Vec::new();
        for &(r, ck) in cells {
            if r.is_header() {
                continue;
            }
            let Some(c) = self.edits.col_of(ck) else {
                continue;
            };
            let entry = (r, c as u32);
            if !touched.insert(entry) {
                continue;
            }
            let v = self.value(r, ck);
            if m.text(&v) {
                hits.push(entry);
            }
        }
        self.find.raw.retain(|e| !touched.contains(e));
        self.find.raw.extend(hits);
        if self.find.running {
            // The search may still reach these cells with their old values.
            self.find.ignore.extend(touched);
        }
        self.remap_matches();
    }
}
