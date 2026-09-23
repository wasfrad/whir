//! Changes to many cells at once: paste, cut, clear, fill down/right, and
//! Ctrl+Enter's fill. Each is one undo step.
//!
//! Values equal to the file's aren't stored as edits (so clearing an empty
//! cell or pasting back the original leaves the document clean) — checked
//! when at most 10k rows are touched; bigger changes store values as given.

use std::sync::Arc;

use whir_core::find_bar::fmt_int;

use crate::edit::{ColKey, DisplayAt, MAX_EDIT_ROWS, RowKey};
use crate::grid::{CellPos, Gate, Selection};
use crate::loaded::Loaded;
use crate::ops::{COMPARE_ROWS, base_value_of, sel_range};
use crate::paste::{MAX_PASTE_CELLS, parse_clipboard};

/// Clear and fill touch at most this many cells.
const MAX_BULK_CELLS: u64 = 1_000_000;

impl Loaded {
    /// Set cells of `range` (display coordinates, real cells) to what
    /// `f(row, col, current text)` says (`None`: leave it). One undo step.
    /// Returns how many cells were set.
    pub(crate) fn edit_range(
        &mut self,
        range: Selection,
        label: &'static str,
        mut f: impl FnMut(u64, usize, &str) -> Option<Arc<str>>,
    ) -> usize {
        let (r0, r1) = range.rows();
        let (c0, c1) = range.cols();
        let compare = r1 - r0 < COMPARE_ROWS;
        let col_keys: Vec<ColKey> = (c0..=c1).map(|c| self.edits.col_key(c)).collect();
        let mut wanted: Vec<(RowKey, ColKey, Arc<str>)> = Vec::new();
        for r in r0..=r1 {
            let Some(row) = self.view().row(r) else {
                continue;
            };
            let key = row.key();
            for (j, c) in (c0..=c1).enumerate() {
                if let Some(v) = f(r, c, row.cell(c).text) {
                    wanted.push((key, col_keys[j], v));
                }
            }
        }
        let n = wanted.len();
        let changes: Vec<_> = wanted
            .into_iter()
            .map(|(k, ck, v)| {
                let v = if compare && base_value_of(&mut self.table, k, ck) == *v {
                    None
                } else {
                    Some(v)
                };
                (k, ck, v)
            })
            .collect();
        let before = self.grid.sel.map(sel_range);
        let mut tx = self.edits.tx(label);
        tx.set_cells(changes);
        let applied = tx.commit(before, Some(sel_range(range)));
        self.follow(&applied.effects);
        n
    }

    /// The selection limited to real cells, if within `limit` cells.
    fn bulk_range(&mut self, what: &str) -> Option<Selection> {
        let range = self.grid.sel?.clamped(self.rows(), self.grid.cols())?;
        if range.cell_count() > MAX_BULK_CELLS {
            self.set_status(
                format!("{what} is limited to {} cells.", fmt_int(MAX_BULK_CELLS)),
                self.now,
            );
            return None;
        }
        Some(range)
    }

    /// Delete key / the second half of Cut: empty the selected cells.
    pub(crate) fn clear(&mut self, label: &'static str) {
        if !self.check(self.editable().cells) {
            return;
        }
        let Some(range) = self.bulk_range(label) else {
            return;
        };
        let empty: Arc<str> = Arc::from("");
        self.edit_range(range, label, |_, _, cur| {
            (!cur.is_empty()).then(|| empty.clone())
        });
    }

    /// Ctrl+D copies the selection's top row down (one row: from the row
    /// above); Ctrl+R copies its left column to the right.
    pub(crate) fn fill(&mut self, down: bool) {
        if !self.check(self.editable().cells) {
            return;
        }
        let label = if down { "Fill down" } else { "Fill right" };
        let Some(range) = self.bulk_range(label) else {
            return;
        };
        let (r0, r1) = range.rows();
        let (c0, c1) = range.cols();
        if down {
            let (src, first) = if r0 == r1 {
                (r0.checked_sub(1), r0)
            } else {
                (Some(r0), r0 + 1)
            };
            let Some(src) = src.and_then(|r| self.view().key(r)) else {
                return;
            };
            let values: Vec<Arc<str>> = (c0..=c1)
                .map(|c| {
                    let ck = self.edits.col_key(c);
                    Arc::from(self.value(src, ck))
                })
                .collect();
            let target = Selection {
                anchor: CellPos {
                    row: first,
                    col: c0,
                },
                cursor: CellPos { row: r1, col: c1 },
            };
            self.edit_range(target, label, |_, c, _| Some(values[c - c0].clone()));
        } else {
            let (src, first) = if c0 == c1 {
                (c0.checked_sub(1), c0)
            } else {
                (Some(c0), c0 + 1)
            };
            let Some(src) = src else {
                return;
            };
            let sk = self.edits.col_key(src);
            let mut values: Vec<Arc<str>> = Vec::with_capacity((r1 - r0 + 1) as usize);
            for r in r0..=r1 {
                let v = match self.view().key(r) {
                    Some(k) => self.value(k, sk),
                    None => String::new(),
                };
                values.push(Arc::from(v));
            }
            let target = Selection {
                anchor: CellPos {
                    row: r0,
                    col: first,
                },
                cursor: CellPos { row: r1, col: c1 },
            };
            self.edit_range(target, label, |r, _, _| {
                Some(values[(r - r0) as usize].clone())
            });
        }
        self.grid.sel = Some(range);
    }

    /// Paste clipboard text (TSV) at the selection. A single value fills a
    /// larger selection; more rows or columns than the table has are added
    /// (at the end of the view when sorted) if that's possible right now.
    pub(crate) fn paste(&mut self, text: &str) {
        let gates = self.editable();
        if !self.check(gates.cells) {
            return;
        }
        let clip = match parse_clipboard(text) {
            Ok(c) => c,
            Err(e) => {
                self.set_status(e, self.now);
                return;
            }
        };
        if clip.rows.is_empty() || clip.width == 0 {
            return;
        }
        let Some(sel) = self.grid.sel else { return };
        let rows = self.rows();
        let cols = self.grid.cols();
        if clip.rows.len() == 1
            && clip.width == 1
            && sel.cell_count() > 1
            && let Some(range) = sel.clamped(rows, cols)
        {
            let v: Arc<str> = Arc::from(clip.rows[0][0].as_str());
            let n = self.edit_range(range, "Paste", |_, _, _| Some(v.clone()));
            self.grid.sel = Some(range);
            self.set_status(format!("Pasted into {} cells", fmt_int(n as u64)), self.now);
            return;
        }
        let (r0, _) = sel.rows();
        let (c0, _) = sel.cols();
        let mut n_rows = clip.rows.len() as u64;
        let mut n_cols = clip.width;
        let mut note: Option<&'static str> = None;
        // Growing the table is only possible when the gates allow it.
        if r0 + n_rows > rows {
            let limit_ok = r0 + n_rows < MAX_EDIT_ROWS;
            match gates.rows {
                Gate::Yes if limit_ok => {}
                Gate::Yes => {
                    n_rows = rows.saturating_sub(r0);
                    note = Some("The table can't have more rows.");
                }
                Gate::No(why) => {
                    n_rows = rows.saturating_sub(r0);
                    note = Some(why);
                }
            }
        }
        if c0 + n_cols > cols
            && let Gate::No(why) = gates.cols
        {
            n_cols = cols.saturating_sub(c0);
            note = Some(why);
        }
        if n_rows == 0 || n_cols == 0 {
            if let Some(why) = note {
                self.set_status(why, self.now);
            }
            return;
        }
        let extra_rows = (r0 + n_rows).saturating_sub(rows);
        let extra_cols = (c0 + n_cols).saturating_sub(cols);
        // Keys of the rows and columns that already exist.
        let row_keys: Vec<Option<RowKey>> = (r0..(r0 + n_rows).min(rows))
            .map(|d| self.view().key(d))
            .collect();
        let col_keys: Vec<ColKey> = (c0..(c0 + n_cols).min(cols))
            .map(|c| self.edits.col_key(c))
            .collect();
        let compare = n_rows <= COMPARE_ROWS;
        let before = self.grid.sel.map(sel_range);
        let after = Selection {
            anchor: CellPos { row: r0, col: c0 },
            cursor: CellPos {
                row: r0 + n_rows - 1,
                col: c0 + n_cols - 1,
            },
        };
        let order_at = self.order.as_ref().map(|o| DisplayAt {
            at: o.len(),
            generation: o.generation,
        });
        let logical_end = self.edits.logical_len(self.base_rows());
        let Loaded { edits, table, .. } = &mut *self;
        let mut tx = edits.tx("Paste");
        let new_cols = tx.insert_cols(cols, extra_cols, cols);
        tx.set_cells(new_cols.iter().enumerate().map(|(i, &k)| {
            let name: Arc<str> = Arc::from(format!("Column {}", cols + i + 1));
            (RowKey::HEADER, k, Some(name))
        }));
        let new_rows = tx.insert_rows(logical_end, extra_rows as u32, order_at);
        let mut changes = Vec::with_capacity(clip.cells().min(MAX_PASTE_CELLS));
        for (i, row) in clip.rows.iter().take(n_rows as usize).enumerate() {
            let d = r0 + i as u64;
            let key = if d < rows {
                row_keys[i]
            } else {
                Some(new_rows[(d - rows) as usize])
            };
            let Some(key) = key else { continue };
            for (j, v) in row.iter().take(n_cols).enumerate() {
                let c = c0 + j;
                let ck = if c < cols {
                    col_keys[j]
                } else {
                    new_cols[c - cols]
                };
                // Added rows/columns start empty: comparing is free there.
                let check = compare || key.is_added() || ck.is_added();
                let v = if check && base_value_of(table, key, ck) == *v {
                    None
                } else {
                    Some(Arc::from(v.as_str()))
                };
                changes.push((key, ck, v));
            }
        }
        tx.set_cells(changes);
        let applied = tx.commit(before, Some(sel_range(after)));
        self.follow(&applied.effects);
        self.grid.sel = Some(after);
        self.grid.reveal(after.anchor);
        let cells = n_rows * n_cols as u64;
        let msg = match note {
            Some(why) => format!(
                "Pasted {} cells; the rest didn't fit. {why}",
                fmt_int(cells)
            ),
            None => format!(
                "Pasted {} cell{}",
                fmt_int(cells),
                if cells == 1 { "" } else { "s" }
            ),
        };
        self.set_status(msg, self.now);
    }
}
