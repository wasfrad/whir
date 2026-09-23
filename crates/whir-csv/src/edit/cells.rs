//! Cell values that differ from the file.
//!
//! Ordered by row key, so a pass over the file advances a single cursor (an
//! unedited row costs one comparison). Values are `Arc<str>`: undo records and
//! worker snapshots share them instead of copying.

use std::collections::BTreeMap;
use std::ops::RangeBounds;
use std::sync::Arc;

use super::keys::{ColKey, RowKey};

/// One row's overrides, sorted by column key.
pub type RowCells = Vec<(ColKey, Arc<str>)>;

/// The override for `c` in a row's cells.
pub fn lookup(cells: &RowCells, c: ColKey) -> Option<&Arc<str>> {
    cells
        .binary_search_by_key(&c, |e| e.0)
        .ok()
        .map(|i| &cells[i].1)
}

#[derive(Clone, Debug, Default)]
pub struct CellStore {
    rows: BTreeMap<RowKey, RowCells>,
    count: usize,
}

impl CellStore {
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Number of overridden cells.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.count
    }

    pub fn get(&self, r: RowKey, c: ColKey) -> Option<&Arc<str>> {
        lookup(self.rows.get(&r)?, c)
    }

    pub fn row(&self, r: RowKey) -> Option<&RowCells> {
        self.rows.get(&r)
    }

    /// Rows with overrides, in key order, within `range`.
    pub fn range(
        &self,
        range: impl RangeBounds<RowKey>,
    ) -> std::collections::btree_map::Range<'_, RowKey, RowCells> {
        self.rows.range(range)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&RowKey, &RowCells)> {
        self.rows.iter()
    }

    /// Set (`Some`) or clear (`None`) one override. Returns the previous one.
    pub fn set(&mut self, r: RowKey, c: ColKey, v: Option<Arc<str>>) -> Option<Arc<str>> {
        match v {
            Some(v) => {
                let row = self.rows.entry(r).or_default();
                match row.binary_search_by_key(&c, |e| e.0) {
                    Ok(i) => Some(std::mem::replace(&mut row[i].1, v)),
                    Err(i) => {
                        row.insert(i, (c, v));
                        self.count += 1;
                        None
                    }
                }
            }
            None => {
                let row = self.rows.get_mut(&r)?;
                let i = row.binary_search_by_key(&c, |e| e.0).ok()?;
                let (_, old) = row.remove(i);
                self.count -= 1;
                if row.is_empty() {
                    self.rows.remove(&r);
                }
                Some(old)
            }
        }
    }

    /// Remove a whole row's overrides.
    pub fn take_row(&mut self, r: RowKey) -> Option<RowCells> {
        let row = self.rows.remove(&r)?;
        self.count -= row.len();
        Some(row)
    }

    /// Put back overrides removed by [`Self::take_row`].
    pub fn put_row(&mut self, r: RowKey, cells: RowCells) {
        if cells.is_empty() {
            return;
        }
        self.count += cells.len();
        if let Some(old) = self.rows.insert(r, cells) {
            debug_assert!(false, "put_row over existing overrides");
            self.count -= old.len();
        }
    }

    /// Remove every override in column `c` (column deleted).
    pub fn take_col(&mut self, c: ColKey) -> Vec<(RowKey, Arc<str>)> {
        let mut out = Vec::new();
        self.rows.retain(|&r, row| {
            if let Ok(i) = row.binary_search_by_key(&c, |e| e.0) {
                out.push((r, row.remove(i).1));
            }
            !row.is_empty()
        });
        self.count -= out.len();
        out
    }

    /// Row keys with overrides in `range`.
    pub fn keys_in(&self, range: impl RangeBounds<RowKey>) -> Vec<RowKey> {
        self.rows.range(range).map(|(k, _)| *k).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_clear_take() {
        let mut s = CellStore::default();
        let (r, c) = (RowKey::orig(1), ColKey::orig(2));
        assert_eq!(s.set(r, c, Some("a".into())), None);
        assert_eq!(s.set(r, ColKey::orig(0), Some("b".into())).as_deref(), None);
        assert_eq!(s.set(r, c, Some("c".into())).as_deref(), Some("a"));
        assert_eq!(s.len(), 2);
        assert_eq!(s.get(r, c).map(|v| &**v), Some("c"));
        assert_eq!(s.row(r).unwrap()[0].0, ColKey::orig(0)); // sorted
        assert_eq!(s.set(r, c, None).as_deref(), Some("c"));
        assert_eq!(s.set(r, c, None), None);
        assert_eq!(s.len(), 1);
        s.set(RowKey::added(0), c, Some("x".into()));
        let taken = s.take_col(ColKey::orig(0));
        assert_eq!(taken.len(), 1);
        assert_eq!(s.len(), 1);
        assert!(s.row(r).is_none());
        let row = s.take_row(RowKey::added(0)).unwrap();
        assert!(s.is_empty());
        s.put_row(RowKey::added(0), row);
        assert_eq!(s.len(), 1);
    }
}
