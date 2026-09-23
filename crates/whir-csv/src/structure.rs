//! Inserting and deleting rows and columns.
//!
//! In a sorted view the sort stays a view: rows inserted above display row `d`
//! go into the file right before the row shown there and appear at `d`;
//! deleted rows leave both the file order and the view. Nothing re-sorts.

use std::sync::Arc;

use egui::Context;
use whir_core::find_bar::fmt_int;

use crate::edit::{ColMeta, DisplayAt, MAX_EDIT_ROWS, RowKey};
use crate::grid::{CellPos, Selection};
use crate::loaded::Loaded;
use crate::ops::sel_range;

/// Most rows one insert adds.
const MAX_INSERT_ROWS: u64 = 1_000_000;

fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{} {}", fmt_int(n), if n == 1 { one } else { many })
}

impl Loaded {
    /// The selection's real rows and columns, if any.
    fn sel_bounds(&self) -> Option<((u64, u64), (usize, usize))> {
        let s = self.grid.sel?;
        let (r0, r1) = s.rows();
        let (c0, c1) = s.cols();
        Some(((r0, r1), (c0, c1)))
    }

    /// Whole columns are selected (every row), but not the whole table.
    fn whole_columns(&self) -> bool {
        let rows = self.rows();
        let cols = self.grid.cols();
        self.sel_bounds().is_some_and(|((r0, r1), (c0, c1))| {
            rows > 0 && r0 == 0 && r1 + 1 >= rows && !(c0 == 0 && c1 + 1 >= cols)
        })
    }

    /// Ctrl+Shift+=: rows above, or columns when whole columns are selected.
    pub(crate) fn insert_auto(&mut self) {
        if self.whole_columns() {
            self.insert_cols(true);
        } else {
            self.insert_rows(true);
        }
    }

    /// Ctrl+−: the selection's rows, or whole selected columns.
    pub(crate) fn delete_auto(&mut self) {
        if self.whole_columns() {
            self.delete_cols();
        } else {
            self.delete_rows();
        }
    }

    /// Insert as many empty rows as the selection spans, above or below it.
    pub(crate) fn insert_rows(&mut self, above: bool) {
        if !self.check(self.editable().rows) {
            return;
        }
        let rows = self.rows();
        let Some(((r0, r1), _)) = self.sel_bounds() else {
            return;
        };
        let (r0, r1) = (r0.min(rows), r1.min(rows));
        let n = (r1 - r0 + 1).min(MAX_INSERT_ROWS);
        if rows + n >= MAX_EDIT_ROWS {
            self.set_status("The table can't have more rows.", self.now);
            return;
        }
        // Display position of the new rows, and their place in the file.
        let d = if above { r0 } else { (r1 + 1).min(rows) };
        let base = self.base_rows();
        let logical = if d >= rows {
            self.edits.logical_len(base)
        } else if above || self.order.is_none() {
            let k = self.view().key(d);
            k.and_then(|k| self.edits.index_of(k, base))
                .unwrap_or_else(|| self.edits.logical_len(base))
        } else {
            // Below a row of a sorted view: right after it in the file.
            let k = self.view().key(r1.min(rows - 1));
            k.and_then(|k| self.edits.index_of(k, base))
                .map_or_else(|| self.edits.logical_len(base), |i| i + 1)
        };
        let display = self.order.as_ref().map(|o| DisplayAt {
            at: d,
            generation: o.generation,
        });
        let cols = self.grid.cols();
        let before = self.grid.sel.map(sel_range);
        let after = Selection {
            anchor: CellPos { row: d, col: 0 },
            cursor: CellPos {
                row: d + n - 1,
                col: cols.saturating_sub(1),
            },
        };
        let mut tx = self.edits.tx("Insert rows");
        tx.insert_rows(logical, n as u32, display);
        let applied = tx.commit(before, Some(sel_range(after)));
        self.follow(&applied.effects);
        self.grid.sel = Some(after);
        self.grid.reveal(after.anchor);
        self.set_status(format!("Inserted {}", plural(n, "row", "rows")), self.now);
    }

    /// Delete the selection's rows.
    pub(crate) fn delete_rows(&mut self) {
        if !self.check(self.editable().rows) {
            return;
        }
        let rows = self.rows();
        let Some(((r0, r1), (c0, _))) = self.sel_bounds() else {
            return;
        };
        if r0 >= rows {
            return; // only the "+" row
        }
        let r1 = r1.min(rows - 1);
        let base = self.base_rows();
        let before = self.grid.sel.map(sel_range);
        let (logical, display) = match &self.order {
            None => ((r0..=r1).collect::<Vec<u64>>(), None),
            Some(o) => {
                let keys: Vec<RowKey> = (r0..=r1).filter_map(|d| o.key(d)).collect();
                let mut logical: Vec<u64> = keys
                    .iter()
                    .filter_map(|&k| self.edits.index_of(k, base))
                    .collect();
                logical.sort_unstable();
                let at = DisplayAt {
                    at: r0,
                    generation: o.generation,
                };
                (logical, Some((at, keys)))
            }
        };
        let n = logical.len() as u64;
        let cursor = CellPos {
            row: r0.min((rows - n).saturating_sub(1)),
            col: c0,
        };
        let mut tx = self.edits.tx("Delete rows");
        tx.delete_rows(&logical, display);
        let applied = tx.commit(before, Some(sel_range(Selection::single(cursor))));
        self.follow(&applied.effects);
        self.grid.sel = Some(Selection::single(cursor));
        self.grid.reveal(cursor);
        self.set_status(
            format!("Deleted {} · Ctrl+Z to undo", plural(n, "row", "rows")),
            self.now,
        );
    }

    /// Insert as many columns as the selection spans, left or right of it.
    pub(crate) fn insert_cols(&mut self, left: bool) {
        if !self.check(self.editable().cols) {
            return;
        }
        let cols = self.grid.cols();
        let Some((_, (c0, c1))) = self.sel_bounds() else {
            return;
        };
        let (c0, c1) = (c0.min(cols), c1.min(cols));
        let n = c1 - c0 + 1;
        let at = if left { c0 } else { (c1 + 1).min(cols) };
        let rows = self.rows();
        let before = self.grid.sel.map(sel_range);
        let after = Selection {
            anchor: CellPos { row: 0, col: at },
            cursor: CellPos {
                row: rows.saturating_sub(1),
                col: at + n - 1,
            },
        };
        let mut tx = self.edits.tx("Insert columns");
        let keys = tx.insert_cols(at, n, cols);
        tx.set_cells(keys.iter().enumerate().map(|(i, &k)| {
            let name: Arc<str> = Arc::from(format!("Column {}", at + i + 1));
            (RowKey::HEADER, k, Some(name))
        }));
        let applied = tx.commit(before, Some(sel_range(after)));
        self.follow(&applied.effects);
        self.grid.sel = Some(after);
        self.grid.reveal_col(at);
        self.set_status(
            format!("Inserted {}", plural(n as u64, "column", "columns")),
            self.now,
        );
    }

    /// Delete the selection's columns (a table keeps at least one).
    pub(crate) fn delete_cols(&mut self) {
        if !self.check(self.editable().cols) {
            return;
        }
        let cols = self.grid.cols();
        let Some(((r0, _), (c0, c1))) = self.sel_bounds() else {
            return;
        };
        if c0 >= cols {
            return; // only the "+" column
        }
        let c1 = c1.min(cols - 1);
        if c0 == 0 && c1 + 1 == cols {
            self.set_status("A table needs at least one column.", self.now);
            return;
        }
        let positions: Vec<usize> = (c0..=c1).collect();
        let meta: Vec<ColMeta> = positions
            .iter()
            .map(|&c| ColMeta {
                width: self.grid.widths[c],
                numeric: self.grid.numeric.get(c).copied().unwrap_or(false),
            })
            .collect();
        let n = positions.len() as u64;
        let cursor = CellPos {
            row: r0.min(self.rows().saturating_sub(1)),
            col: c0.min(cols - positions.len() - 1),
        };
        let before = self.grid.sel.map(sel_range);
        let mut tx = self.edits.tx("Delete columns");
        tx.delete_cols(&positions, meta, cols);
        let applied = tx.commit(before, Some(sel_range(Selection::single(cursor))));
        self.follow(&applied.effects);
        self.grid.sel = Some(Selection::single(cursor));
        self.set_status(
            format!(
                "Deleted {} · Ctrl+Z to undo",
                plural(n, "column", "columns")
            ),
            self.now,
        );
    }

    /// The "+" header: add a column at the end and rename it right away.
    pub(crate) fn append_column(&mut self, ctx: &Context) {
        if !self.check(self.editable().cols) {
            return;
        }
        let cols = self.grid.cols();
        let before = self.grid.sel.map(sel_range);
        let mut tx = self.edits.tx("Add column");
        let k = tx.insert_cols(cols, 1, cols)[0];
        let name: Arc<str> = Arc::from(format!("Column {}", cols + 1));
        tx.set_cells([(RowKey::HEADER, k, Some(name))]);
        let applied = tx.commit(before, before);
        self.follow(&applied.effects);
        self.start_rename(ctx, cols);
    }
}
