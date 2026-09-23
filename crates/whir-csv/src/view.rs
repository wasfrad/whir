//! What the grid shows: the file's rows seen through the edit overlay and the
//! sort order, addressed by display position.
//!
//! With no edits and no sort, `row(d)` is exactly the file's row `d + 1`, read
//! from the table's block cache — the viewer's fast path is unchanged.

use std::rc::Rc;

use crate::edit::{ColKey, ColMap, Edits, RowCells, RowKey, lookup};
use crate::order::Order;
use crate::table::{Block, RowView, Table};

pub struct SheetView<'a> {
    pub table: &'a mut Table,
    pub edits: &'a Edits,
    pub order: Option<&'a Order>,
    /// Data rows the file has (so far, while indexing).
    pub base_rows: u64,
}

impl<'a> SheetView<'a> {
    /// Rows shown (excluding the header and the ghost row).
    pub fn rows(&self) -> u64 {
        match self.order {
            Some(o) => o.len(),
            None => self.edits.logical_len(self.base_rows),
        }
    }

    pub fn key(&self, display: u64) -> Option<RowKey> {
        match self.order {
            Some(o) => o.key(display),
            None => self.edits.key(display, self.base_rows),
        }
    }

    pub fn display_of(&self, k: RowKey) -> Option<u64> {
        match self.order {
            Some(o) => o.display_of(k),
            None => self.edits.index_of(k, self.base_rows),
        }
    }

    /// Position in the file that a save would write (the row number shown).
    pub fn logical_of(&self, k: RowKey) -> Option<u64> {
        self.edits.index_of(k, self.base_rows)
    }

    pub fn col_key(&self, c: usize) -> ColKey {
        self.edits.col_key(c)
    }

    pub fn col_of(&self, k: ColKey) -> Option<usize> {
        self.edits.col_of(k)
    }

    pub fn row(&mut self, display: u64) -> Option<RowRead<'a>> {
        let key = self.key(display)?;
        self.row_by_key(key)
    }

    pub fn row_by_key(&mut self, key: RowKey) -> Option<RowRead<'a>> {
        let edits: &'a Edits = self.edits;
        let base = match key.as_orig() {
            Some(r) => Some(self.table.row(r + 1)?),
            None => None,
        };
        Some(RowRead {
            key,
            base,
            cells: edits.row(key),
            cols: edits.cols(),
        })
    }

    /// The header row (empty for an empty file).
    pub fn header(&mut self) -> RowRead<'a> {
        let edits: &'a Edits = self.edits;
        RowRead {
            key: RowKey::HEADER,
            base: self.table.row(0),
            cells: edits.row(RowKey::HEADER),
            cols: edits.cols(),
        }
    }
}

/// One row, readable cell by cell.
pub struct RowRead<'a> {
    key: RowKey,
    base: Option<(Rc<Block>, usize)>,
    cells: Option<&'a RowCells>,
    cols: Option<&'a ColMap>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell<'r> {
    pub text: &'r str,
    pub edited: bool,
    /// Only part of a very long value is loaded.
    pub truncated: bool,
}

impl RowRead<'_> {
    pub fn key(&self) -> RowKey {
        self.key
    }

    fn base_row(&self) -> Option<RowView<'_>> {
        self.base.as_ref().and_then(|(b, i)| b.row(*i))
    }

    fn col_key(&self, c: usize) -> ColKey {
        match self.cols {
            Some(m) => m.key(c),
            None => ColKey::orig(c),
        }
    }

    /// Positions in this row (fields mapped through column changes, plus
    /// anything edited beyond them).
    pub fn len(&self) -> usize {
        let base = self.base_row().map_or(0, |r| r.len());
        let mut n = match self.cols {
            Some(m) => m.row_len(base),
            None => base,
        };
        if let Some(cells) = self.cells {
            for (k, _) in cells {
                let p = match self.cols {
                    Some(m) => m.index_of(*k),
                    None => k.as_orig(),
                };
                if let Some(p) = p {
                    n = n.max(p + 1);
                }
            }
        }
        n
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn cell(&self, c: usize) -> Cell<'_> {
        let k = self.col_key(c);
        if let Some(v) = self.cells.and_then(|cells| lookup(cells, k)) {
            return Cell {
                text: v,
                edited: true,
                truncated: false,
            };
        }
        match (k.as_orig(), self.base_row()) {
            (Some(f), Some(row)) => Cell {
                text: row.cell(f),
                edited: false,
                truncated: row.is_truncated(f),
            },
            _ => Cell {
                text: "",
                edited: false,
                truncated: false,
            },
        }
    }

    /// A row the user added.
    pub fn is_new(&self) -> bool {
        self.key.is_added()
    }

    /// Any cell of this row was edited.
    pub fn has_edits(&self) -> bool {
        self.cells.is_some()
    }
}
