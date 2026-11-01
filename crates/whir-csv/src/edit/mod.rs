//! The edit overlay: everything the user changed, on top of the untouched file.
//!
//! Pure data, no egui. Three copy-on-write parts — the row order (piece table),
//! the column map and the cell overrides — each `None`/empty while unchanged, so
//! "dirty" is simply "the overlay isn't empty". A snapshot for a worker thread
//! (find, sort, save) is three `Arc` clones.
//!
//! Every user action is one [`Tx`] and becomes one undo [`Step`]. Operations
//! return [`Effect`]s that the document applies to view state (sort order,
//! column widths, find results), keeping this module testable on its own.
//!
//! The overlay stays honest: a value equal to the file's is stored as "no
//! override" (the caller normalizes), and deleting rows or columns moves their
//! overrides into the undo step — so undoing everything leaves a clean document.

mod cells;
mod cols;
mod keys;
mod rows;

use std::collections::VecDeque;
use std::sync::Arc;

pub use cells::{CellStore, RowCells, lookup};
pub use cols::ColMap;
pub use keys::{ColKey, MAX_EDIT_ROWS, RowKey};
pub use rows::{Piece, Removed, RowOrder};

/// Undo history keeps roughly this many recorded entries (cells, rows, keys).
const UNDO_BUDGET: usize = 4_000_000;

/// A selection in display coordinates `(row, col)`, restored by undo/redo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelRange {
    pub anchor: (u64, usize),
    pub cursor: (u64, usize),
}

/// View state of a deleted column, restored by undo.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColMeta {
    pub width: f32,
    pub numeric: bool,
}

/// Where rows sit in a sorted view: display position `at` in sort
/// generation `generation` (positions from another sort are meaningless).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisplayAt {
    pub at: u64,
    pub generation: u64,
}

/// What changed, for the view to follow.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    Cells(Vec<(RowKey, ColKey)>),
    /// `keys` in display order; `display` places them in a sorted view.
    RowsInserted {
        keys: Vec<RowKey>,
        display: Option<DisplayAt>,
    },
    RowsRemoved {
        keys: Vec<RowKey>,
    },
    /// Positions ascending; `meta` restores widths of undeleted columns.
    ColsInserted {
        positions: Vec<usize>,
        meta: Option<Vec<ColMeta>>,
    },
    ColsRemoved {
        positions: Vec<usize>,
    },
}

/// The result of a transaction, undo or redo.
#[derive(Debug, Default)]
pub struct Applied {
    pub effects: Vec<Effect>,
    pub select: Option<SelRange>,
    pub label: &'static str,
}

#[derive(Clone)]
struct CellChange {
    row: RowKey,
    col: ColKey,
    /// `None` = no override (the file's value).
    old: Option<Arc<str>>,
    new: Option<Arc<str>>,
}

enum Op {
    Cells(Vec<CellChange>),
    InsertRows {
        at: u64,
        piece: Piece,
        display: Option<DisplayAt>,
    },
    DeleteRows {
        runs: Removed,
        /// The deleted keys in display order (logical order when unsorted).
        keys: Vec<RowKey>,
        display: Option<DisplayAt>,
        stash: Vec<(RowKey, RowCells)>,
    },
    InsertCols {
        at: usize,
        keys: Vec<ColKey>,
    },
    DeleteCols {
        removed: Vec<(usize, ColKey)>,
        meta: Vec<ColMeta>,
        stash: ColStash,
    },
}

/// Overrides of deleted columns, kept by the undo step.
type ColStash = Vec<(ColKey, Vec<(RowKey, Arc<str>)>)>;

impl Op {
    fn weight(&self) -> usize {
        1 + match self {
            Op::Cells(c) => c.len(),
            Op::InsertRows { .. } | Op::InsertCols { .. } => 1,
            Op::DeleteRows {
                runs, keys, stash, ..
            } => runs.len() + keys.len() + stash.iter().map(|s| s.1.len()).sum::<usize>(),
            Op::DeleteCols { removed, stash, .. } => {
                removed.len() + stash.iter().map(|s| s.1.len()).sum::<usize>()
            }
        }
    }
}

struct Step {
    label: &'static str,
    ops: Vec<Op>,
    before: Option<SelRange>,
    after: Option<SelRange>,
    weight: usize,
}

/// A copy of the overlay for a worker thread: O(1).
#[derive(Clone, Default)]
pub struct EditSnapshot {
    pub rows: Option<Arc<RowOrder>>,
    pub cols: Option<Arc<ColMap>>,
    pub cells: Arc<CellStore>,
}

impl EditSnapshot {
    pub fn is_empty(&self) -> bool {
        self.rows.is_none() && self.cols.is_none() && self.cells.is_empty()
    }
}

#[derive(Default)]
pub struct Edits {
    rows: Option<Arc<RowOrder>>,
    cols: Option<Arc<ColMap>>,
    cells: Arc<CellStore>,
    /// Data rows in the file (the identity order's length).
    base_rows: u32,
    next_row: u32,
    next_col: u32,
    undo: VecDeque<Step>,
    redo: Vec<Step>,
    undo_weight: usize,
    /// History budget override (tests); 0 = [`UNDO_BUDGET`].
    budget: usize,
    /// Bumped on every change (caches key off it).
    pub rev: u64,
}

impl Edits {
    /// The file's data row count, known once indexing is done. Row inserts
    /// and deletes are only allowed after this.
    pub fn set_base_rows(&mut self, n: u64) {
        debug_assert!(self.rows.is_none() || self.base_rows as u64 == n);
        self.base_rows = n.min(MAX_EDIT_ROWS) as u32;
    }

    pub fn snapshot(&self) -> EditSnapshot {
        EditSnapshot {
            rows: self.rows.clone(),
            cols: self.cols.clone(),
            cells: self.cells.clone(),
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.rows.is_some() || self.cols.is_some() || !self.cells.is_empty()
    }

    pub fn rows(&self) -> Option<&RowOrder> {
        self.rows.as_deref()
    }

    pub fn cols(&self) -> Option<&ColMap> {
        self.cols.as_deref()
    }

    #[cfg(test)]
    pub fn cells(&self) -> &CellStore {
        &self.cells
    }

    pub fn get(&self, r: RowKey, c: ColKey) -> Option<&str> {
        self.cells.get(r, c).map(|v| &**v)
    }

    pub fn row(&self, r: RowKey) -> Option<&RowCells> {
        self.cells.row(r)
    }

    /// Logical data rows, given the file's current row count.
    pub fn logical_len(&self, base: u64) -> u64 {
        self.rows.as_ref().map_or(base, |o| o.len())
    }

    /// Row at logical position `i`.
    pub fn key(&self, i: u64, base: u64) -> Option<RowKey> {
        match &self.rows {
            Some(o) => o.key(i),
            None => (i < base.min(MAX_EDIT_ROWS)).then(|| RowKey::orig(i)),
        }
    }

    /// Logical position of `k`.
    pub fn index_of(&self, k: RowKey, base: u64) -> Option<u64> {
        match &self.rows {
            Some(o) => o.index_of(k),
            None => k.as_orig().filter(|&r| r < base),
        }
    }

    pub fn col_key(&self, c: usize) -> ColKey {
        match &self.cols {
            Some(m) => m.key(c),
            None => ColKey::orig(c),
        }
    }

    pub fn col_of(&self, k: ColKey) -> Option<usize> {
        match &self.cols {
            Some(m) => m.index_of(k),
            None => k.as_orig(),
        }
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// Group changes into one undo step.
    pub fn tx(&mut self, label: &'static str) -> Tx<'_> {
        Tx {
            e: self,
            label,
            ops: Vec::new(),
            effects: Vec::new(),
        }
    }

    pub fn undo(&mut self) -> Option<Applied> {
        let step = self.undo.pop_back()?;
        self.undo_weight -= step.weight;
        let mut effects = Vec::new();
        for op in step.ops.iter().rev() {
            self.revert(op, &mut effects);
        }
        self.rev += 1;
        let applied = Applied {
            effects,
            select: step.before,
            label: step.label,
        };
        self.redo.push(step);
        Some(applied)
    }

    pub fn redo(&mut self) -> Option<Applied> {
        let mut step = self.redo.pop()?;
        let mut effects = Vec::new();
        for op in step.ops.iter_mut() {
            self.reapply(op, &mut effects);
        }
        self.rev += 1;
        let applied = Applied {
            effects,
            select: step.after,
            label: step.label,
        };
        self.push_undo(step);
        Some(applied)
    }

    fn push_undo(&mut self, step: Step) {
        self.undo_weight += step.weight;
        self.undo.push_back(step);
        // Drop the oldest steps over budget (always keep the latest).
        let budget = if self.budget == 0 {
            UNDO_BUDGET
        } else {
            self.budget
        };
        while self.undo_weight > budget && self.undo.len() > 1 {
            if let Some(old) = self.undo.pop_front() {
                self.undo_weight -= old.weight;
            }
        }
    }

    // --- primitive operations (shared by Tx, undo and redo) -------------------

    fn rows_mut(&mut self) -> &mut RowOrder {
        let base = self.base_rows;
        Arc::make_mut(
            self.rows
                .get_or_insert_with(|| Arc::new(RowOrder::identity(base))),
        )
    }

    fn settle_rows(&mut self) {
        if self.rows.as_ref().is_some_and(|o| o.is_identity()) {
            self.rows = None;
        }
    }

    fn cols_mut(&mut self, width: usize) -> &mut ColMap {
        Arc::make_mut(
            self.cols
                .get_or_insert_with(|| Arc::new(ColMap::identity(width))),
        )
    }

    fn settle_cols(&mut self) {
        if self.cols.as_ref().is_some_and(|m| m.is_identity()) {
            self.cols = None;
        }
    }

    fn set_cell(&mut self, r: RowKey, c: ColKey, v: Option<Arc<str>>) {
        Arc::make_mut(&mut self.cells).set(r, c, v);
    }

    /// Move the overrides of removed rows out of the store.
    fn stash_rows(&mut self, runs: &Removed) -> Vec<(RowKey, RowCells)> {
        if self.cells.is_empty() {
            return Vec::new();
        }
        let mut keys = Vec::new();
        for (_, piece) in runs {
            let (a, b) = match *piece {
                Piece::Orig { start, len } => (
                    RowKey::orig(start as u64),
                    RowKey::orig((start + len - 1) as u64),
                ),
                Piece::New { first, len } => (RowKey::added(first), RowKey::added(first + len - 1)),
            };
            keys.extend(self.cells.keys_in(a..=b));
        }
        if keys.is_empty() {
            return Vec::new();
        }
        let cells = Arc::make_mut(&mut self.cells);
        keys.into_iter()
            .filter_map(|k| cells.take_row(k).map(|c| (k, c)))
            .collect()
    }

    fn unstash_rows(&mut self, stash: &[(RowKey, RowCells)]) {
        if stash.is_empty() {
            return;
        }
        let cells = Arc::make_mut(&mut self.cells);
        for (k, row) in stash {
            cells.put_row(*k, row.clone());
        }
    }

    fn stash_cols(&mut self, keys: &[ColKey]) -> ColStash {
        if self.cells.is_empty() {
            return Vec::new();
        }
        let cells = Arc::make_mut(&mut self.cells);
        keys.iter()
            .map(|&k| (k, cells.take_col(k)))
            .filter(|(_, v)| !v.is_empty())
            .collect()
    }

    fn unstash_cols(&mut self, stash: &ColStash) {
        if stash.is_empty() {
            return;
        }
        let cells = Arc::make_mut(&mut self.cells);
        for (c, entries) in stash {
            for (r, v) in entries {
                cells.set(*r, *c, Some(v.clone()));
            }
        }
    }

    fn revert(&mut self, op: &Op, effects: &mut Vec<Effect>) {
        match op {
            Op::Cells(changes) => {
                for ch in changes.iter().rev() {
                    self.set_cell(ch.row, ch.col, ch.old.clone());
                }
                effects.push(Effect::Cells(
                    changes.iter().map(|c| (c.row, c.col)).collect(),
                ));
            }
            Op::InsertRows { at, piece, .. } => {
                self.rows_mut().remove_range(*at, piece.len() as u64);
                self.settle_rows();
                effects.push(Effect::RowsRemoved {
                    keys: piece.keys().collect(),
                });
            }
            Op::DeleteRows {
                runs,
                keys,
                display,
                stash,
            } => {
                self.rows_mut().restore(runs);
                self.settle_rows();
                self.unstash_rows(stash);
                effects.push(Effect::RowsInserted {
                    keys: keys.clone(),
                    display: *display,
                });
            }
            Op::InsertCols { at, keys } => {
                let positions: Vec<usize> = (*at..*at + keys.len()).collect();
                let width = at + keys.len();
                let removed = self.cols_mut(width).remove(&positions);
                debug_assert!(removed.iter().map(|r| r.1).eq(keys.iter().copied()));
                self.settle_cols();
                effects.push(Effect::ColsRemoved { positions });
            }
            Op::DeleteCols {
                removed,
                meta,
                stash,
            } => {
                self.cols_mut(0).restore(removed);
                self.settle_cols();
                self.unstash_cols(stash);
                effects.push(Effect::ColsInserted {
                    positions: removed.iter().map(|r| r.0).collect(),
                    meta: Some(meta.clone()),
                });
            }
        }
    }

    fn reapply(&mut self, op: &mut Op, effects: &mut Vec<Effect>) {
        match op {
            Op::Cells(changes) => {
                for ch in changes.iter() {
                    self.set_cell(ch.row, ch.col, ch.new.clone());
                }
                effects.push(Effect::Cells(
                    changes.iter().map(|c| (c.row, c.col)).collect(),
                ));
            }
            Op::InsertRows { at, piece, display } => {
                self.rows_mut().insert(*at, *piece);
                self.settle_rows();
                effects.push(Effect::RowsInserted {
                    keys: piece.keys().collect(),
                    display: *display,
                });
            }
            Op::DeleteRows {
                runs, keys, stash, ..
            } => {
                self.rows_mut().remove_runs(runs);
                self.settle_rows();
                *stash = self.stash_rows(runs);
                effects.push(Effect::RowsRemoved { keys: keys.clone() });
            }
            Op::InsertCols { at, keys } => {
                self.cols_mut(*at).insert(*at, keys);
                self.settle_cols();
                effects.push(Effect::ColsInserted {
                    positions: (*at..*at + keys.len()).collect(),
                    meta: None,
                });
            }
            Op::DeleteCols { removed, stash, .. } => {
                let positions: Vec<usize> = removed.iter().map(|r| r.0).collect();
                let width = positions.last().map_or(0, |p| p + 1);
                self.cols_mut(width).remove(&positions);
                self.settle_cols();
                let keys: Vec<ColKey> = removed.iter().map(|r| r.1).collect();
                *stash = self.stash_cols(&keys);
                effects.push(Effect::ColsRemoved { positions });
            }
        }
    }
}

/// One user action. Changes apply immediately; [`Tx::commit`] records the
/// undo step and returns the effects.
#[must_use = "commit the transaction to record its undo step"]
pub struct Tx<'a> {
    e: &'a mut Edits,
    label: &'static str,
    ops: Vec<Op>,
    effects: Vec<Effect>,
}

impl Tx<'_> {
    /// The overlay as changed so far (keys of rows added in this step).
    #[cfg(test)]
    pub fn edits(&self) -> &Edits {
        self.e
    }

    /// Set cells: `None` clears the override (back to the file's value — the
    /// caller passes `None` for values equal to the file's).
    pub fn set_cells(
        &mut self,
        changes: impl IntoIterator<Item = (RowKey, ColKey, Option<Arc<str>>)>,
    ) {
        let mut recorded = Vec::new();
        for (row, col, new) in changes {
            let old = self.e.cells.get(row, col).cloned();
            if old.as_deref() == new.as_deref() {
                continue;
            }
            // Copy-on-write only once something really changes (a worker may
            // hold a snapshot).
            Arc::make_mut(&mut self.e.cells).set(row, col, new.clone());
            recorded.push(CellChange { row, col, old, new });
        }
        if !recorded.is_empty() {
            self.effects.push(Effect::Cells(
                recorded.iter().map(|c| (c.row, c.col)).collect(),
            ));
            self.ops.push(Op::Cells(recorded));
        }
    }

    /// Add `n` empty rows at logical position `at`; `display` places them in a
    /// sorted view. Returns their keys.
    pub fn insert_rows(&mut self, at: u64, n: u32, display: Option<DisplayAt>) -> Vec<RowKey> {
        if n == 0 {
            return Vec::new();
        }
        let piece = Piece::New {
            first: self.e.next_row,
            len: n,
        };
        self.e.next_row += n;
        let at = at.min(self.e.logical_len(self.e.base_rows as u64));
        self.e.rows_mut().insert(at, piece);
        self.e.settle_rows();
        let keys: Vec<RowKey> = piece.keys().collect();
        self.effects.push(Effect::RowsInserted {
            keys: keys.clone(),
            display,
        });
        self.ops.push(Op::InsertRows { at, piece, display });
        keys
    }

    /// Delete rows at these logical positions (ascending). `display`: when
    /// sorted, the first display position and the keys in display order.
    pub fn delete_rows(
        &mut self,
        logical_sorted: &[u64],
        display: Option<(DisplayAt, Vec<RowKey>)>,
    ) -> Vec<RowKey> {
        if logical_sorted.is_empty() {
            return Vec::new();
        }
        let runs = self.e.rows_mut().remove_sorted(logical_sorted);
        self.e.settle_rows();
        let logical_keys: Vec<RowKey> = runs.iter().flat_map(|(_, p)| p.keys()).collect();
        let stash = self.e.stash_rows(&runs);
        let (display, keys) = match display {
            Some((d, keys)) => (Some(d), keys),
            None => (None, logical_keys.clone()),
        };
        self.effects
            .push(Effect::RowsRemoved { keys: keys.clone() });
        self.ops.push(Op::DeleteRows {
            runs,
            keys,
            display,
            stash,
        });
        logical_keys
    }

    /// Add `n` columns at position `at`. `width` is the current column count
    /// (made explicit the first time columns change). Returns their keys.
    pub fn insert_cols(&mut self, at: usize, n: usize, width: usize) -> Vec<ColKey> {
        if n == 0 {
            return Vec::new();
        }
        let keys: Vec<ColKey> = (0..n as u32)
            .map(|i| ColKey::added(self.e.next_col + i))
            .collect();
        self.e.next_col += n as u32;
        self.e.cols_mut(width.max(at)).insert(at, &keys);
        self.e.settle_cols();
        self.effects.push(Effect::ColsInserted {
            positions: (at..at + n).collect(),
            meta: None,
        });
        self.ops.push(Op::InsertCols {
            at,
            keys: keys.clone(),
        });
        keys
    }

    /// Delete columns at these positions (ascending); `meta` is their view
    /// state (restored by undo).
    pub fn delete_cols(
        &mut self,
        positions: &[usize],
        meta: Vec<ColMeta>,
        width: usize,
    ) -> Vec<ColKey> {
        if positions.is_empty() {
            return Vec::new();
        }
        let removed = self.e.cols_mut(width).remove(positions);
        self.e.settle_cols();
        let keys: Vec<ColKey> = removed.iter().map(|r| r.1).collect();
        let stash = self.e.stash_cols(&keys);
        self.effects.push(Effect::ColsRemoved {
            positions: positions.to_vec(),
        });
        self.ops.push(Op::DeleteCols {
            removed,
            meta,
            stash,
        });
        keys
    }

    /// Record the step. Nothing changed → no step, no effects.
    pub fn commit(self, before: Option<SelRange>, after: Option<SelRange>) -> Applied {
        let Tx {
            e,
            label,
            ops,
            effects,
        } = self;
        if ops.is_empty() {
            return Applied {
                effects,
                select: None,
                label,
            };
        }
        let weight = ops.iter().map(Op::weight).sum();
        e.redo.clear();
        e.rev += 1;
        e.push_undo(Step {
            label,
            ops,
            before,
            after,
            weight,
        });
        Applied {
            effects,
            select: after,
            label,
        }
    }
}

#[cfg(test)]
mod tests;
