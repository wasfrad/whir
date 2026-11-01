//! One pass over the table as the user sees it: the file's rows with the edit
//! overlay applied, in logical order. Find, sort and save all read through it.
//!
//! Without row inserts/deletes this is a single sequential read from the header
//! to EOF (no index needed, so find works while indexing). With them, it walks
//! the piece table: original runs are read in file order (seeking forward via
//! the index checkpoints across gaps), added rows come from the overlay.

use std::io;
use std::ops::ControlFlow;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::dialect::Dialect;
use crate::edit::{ColKey, ColMap, EditSnapshot, MAX_EDIT_ROWS, Piece, RowCells, RowKey, lookup};
use crate::index::{BLOCK_ROWS, SharedIndex};
use crate::parse::{FieldBuf, RowReader};
use crate::source::ByteSource;

/// Everything a worker needs to read the logical table. Cheap to clone.
#[derive(Clone)]
pub struct StreamInput {
    pub source: Arc<dyn ByteSource>,
    /// Effective dialect (quoting may be off).
    pub dialect: Dialect,
    /// Where the header row starts (after a BOM).
    pub header_offset: u64,
    /// Only used to seek when rows were inserted or deleted.
    pub index: SharedIndex,
    pub edits: EditSnapshot,
}

/// A field's value: raw file bytes (the file's encoding) or edited text (UTF-8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field<'a> {
    File(&'a [u8]),
    Text(&'a str),
}

/// One row of the logical table.
pub struct LogicalRow<'a> {
    pub key: RowKey,
    fields: Option<&'a FieldBuf>,
    raw: Option<&'a [u8]>,
    cells: Option<&'a RowCells>,
    cols: Option<&'a ColMap>,
}

impl<'a> LogicalRow<'a> {
    /// A file row read outside [`for_each`], when the table's rows and columns
    /// are the file's own (find's fast path). `cells`: its edited cells.
    pub fn file_row(
        key: RowKey,
        fields: &'a FieldBuf,
        raw: &'a [u8],
        cells: Option<&'a RowCells>,
    ) -> Self {
        Self {
            key,
            fields: Some(fields),
            raw: Some(raw),
            cells,
            cols: None,
        }
    }

    /// The row's exact file bytes when nothing about it changed.
    pub fn unchanged_raw(&self) -> Option<&'a [u8]> {
        match (self.cells, self.cols, self.fields) {
            // A row ending inside an unclosed quote can't be followed by more
            // rows verbatim: it is rewritten (with a closing quote) instead.
            (None, None, Some(f)) if !f.open_quote => self.raw,
            _ => None,
        }
    }

    /// The parsed fields, when they are exactly what the row shows (fast paths).
    pub fn plain_fields(&self) -> Option<&'a FieldBuf> {
        match (self.cells, self.cols) {
            (None, None) => self.fields,
            _ => None,
        }
    }

    /// Fields in the file (0 for added rows).
    pub fn base_len(&self) -> usize {
        self.fields.map_or(0, |f| f.len())
    }

    fn col_key(&self, c: usize) -> ColKey {
        match self.cols {
            Some(m) => m.key(c),
            None => ColKey::orig(c),
        }
    }

    fn col_of(&self, k: ColKey) -> Option<usize> {
        match self.cols {
            Some(m) => m.index_of(k),
            None => k.as_orig(),
        }
    }

    /// One past the last edited position (0 if none).
    pub fn edited_len(&self) -> usize {
        self.cells.map_or(0, |cells| {
            cells
                .iter()
                .filter_map(|(k, _)| self.col_of(*k))
                .map(|p| p + 1)
                .max()
                .unwrap_or(0)
        })
    }

    /// Positions in this row: its fields (mapped through column changes) and
    /// anything edited beyond them.
    pub fn len(&self) -> usize {
        let base = self.base_len();
        let n = match self.cols {
            Some(m) => m.row_len(base),
            None => base,
        };
        n.max(self.edited_len())
    }

    /// The value at position `c` (empty past the end).
    pub fn field(&self, c: usize) -> Field<'a> {
        let k = self.col_key(c);
        if let Some(v) = self.cells.and_then(|cells| lookup(cells, k)) {
            return Field::Text(v);
        }
        match k.as_orig().and_then(|f| self.fields?.get(f)) {
            Some(b) => Field::File(b),
            None => Field::Text(""),
        }
    }

    /// The file field at position `c` was written in quotes.
    pub fn was_quoted(&self, c: usize) -> bool {
        match (self.col_key(c).as_orig(), self.fields) {
            (Some(f), Some(fields)) => fields.quoted(f),
            _ => false,
        }
    }
}

/// Rows between cancel checks.
const CANCEL_EVERY: u64 = 4096;

/// The stream's error for files with more rows than keys can name.
pub const TOO_MANY_ROWS: &str = "the file has more than 2,147,483,647 rows";

/// Walks the overrides of original rows alongside a file pass (keys ascend).
struct Cursor<'c>(std::iter::Peekable<std::collections::btree_map::Range<'c, RowKey, RowCells>>);

impl<'c> Cursor<'c> {
    fn get(&mut self, key: RowKey) -> Option<&'c RowCells> {
        while let Some((k, _)) = self.0.peek() {
            if **k < key {
                self.0.next();
            } else {
                break;
            }
        }
        match self.0.peek() {
            Some((k, row)) if **k == key => Some(*row),
            _ => None,
        }
    }
}

/// Call `f` with the header, then every data row in logical order. `f` also
/// gets the source bytes consumed so far (progress). Returns `Ok(false)` if
/// cancelled or stopped by `f`.
pub fn for_each(
    input: &StreamInput,
    cancel: &AtomicBool,
    mut f: impl FnMut(&LogicalRow<'_>, u64) -> ControlFlow<()>,
) -> io::Result<bool> {
    let src = &*input.source;
    let snap = &input.edits;
    let cols = snap.cols.as_deref();
    let cells = &*snap.cells;
    let mut fields = FieldBuf::default();
    let mut reader = RowReader::new(src, input.dialect, input.header_offset);

    // Header: the file's first row, or one made of edits for an empty file.
    let header_cells = cells.row(RowKey::HEADER);
    if reader.next_row(&mut fields)?.is_some() {
        let row = LogicalRow {
            key: RowKey::HEADER,
            fields: Some(&fields),
            raw: Some(reader.last_raw()),
            cells: header_cells,
            cols,
        };
        if f(&row, reader.position()).is_break() {
            return Ok(false);
        }
    } else if !snap.is_empty() {
        let row = LogicalRow {
            key: RowKey::HEADER,
            fields: None,
            raw: None,
            cells: header_cells,
            cols,
        };
        if f(&row, reader.position()).is_break() {
            return Ok(false);
        }
    }

    // Overrides of original rows, visited in file order with one cursor.
    let mut cursor = Cursor(cells.range(..RowKey::HEADER).peekable());

    let Some(order) = snap.rows.as_deref() else {
        // The file's own order: read straight through.
        let has_cells = !cells.is_empty();
        let mut r = 0u64;
        while reader.next_row(&mut fields)?.is_some() {
            if r.is_multiple_of(CANCEL_EVERY) && cancel.load(Ordering::Relaxed) {
                return Ok(false);
            }
            if r >= MAX_EDIT_ROWS {
                // Beyond what row keys can name: stopping quietly would let a
                // save write a truncated copy, so this is an error.
                return Err(io::Error::other(TOO_MANY_ROWS));
            }
            let key = RowKey::orig(r);
            let row = LogicalRow {
                key,
                fields: Some(&fields),
                raw: Some(reader.last_raw()),
                cells: if has_cells { cursor.get(key) } else { None },
                cols,
            };
            if f(&row, reader.position()).is_break() {
                return Ok(false);
            }
            r += 1;
        }
        return Ok(true);
    };

    let mut emitted = 0u64;
    // File row (0 = header) the reader returns next.
    let mut next_file_row = 1u64;
    for piece in order.pieces() {
        match *piece {
            Piece::Orig { start, len } => {
                let first = start as u64 + 1;
                debug_assert!(first >= next_file_row, "original rows stay in order");
                if first > next_file_row {
                    let gap = first - next_file_row;
                    if gap < BLOCK_ROWS {
                        reader.skip_rows(gap, &mut fields)?;
                    } else {
                        let offset = input
                            .index
                            .read()
                            .ok()
                            .and_then(|s| s.checkpoints.get((first / BLOCK_ROWS) as usize).copied())
                            .ok_or_else(|| io::Error::other("the row index is incomplete"))?;
                        reader = RowReader::new(src, input.dialect, offset);
                        reader.skip_rows(first % BLOCK_ROWS, &mut fields)?;
                    }
                    next_file_row = first;
                }
                for i in 0..len {
                    if emitted.is_multiple_of(CANCEL_EVERY) && cancel.load(Ordering::Relaxed) {
                        return Ok(false);
                    }
                    if reader.next_row(&mut fields)?.is_none() {
                        return Err(io::Error::other("the file is shorter than its index"));
                    }
                    next_file_row += 1;
                    let key = RowKey::orig((start + i) as u64);
                    let row = LogicalRow {
                        key,
                        fields: Some(&fields),
                        raw: Some(reader.last_raw()),
                        cells: cursor.get(key),
                        cols,
                    };
                    if f(&row, reader.position()).is_break() {
                        return Ok(false);
                    }
                    emitted += 1;
                }
            }
            Piece::New { first, len } => {
                for i in 0..len {
                    if emitted.is_multiple_of(CANCEL_EVERY) && cancel.load(Ordering::Relaxed) {
                        return Ok(false);
                    }
                    let key = RowKey::added(first + i);
                    let row = LogicalRow {
                        key,
                        fields: None,
                        raw: None,
                        cells: cells.row(key),
                        cols,
                    };
                    if f(&row, reader.position()).is_break() {
                        return Ok(false);
                    }
                    emitted += 1;
                }
            }
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests;
