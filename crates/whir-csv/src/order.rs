//! Display order of a sorted view: which row key shows at each display
//! position, and the inverse.
//!
//! A sort is a view, not an edit: editing a cell doesn't re-sort (the header
//! arrow dims instead), inserted rows appear where the user put them and
//! deleted ones leave the order. 8 bytes per row, like a plain permutation.

use std::collections::HashMap;

use crate::edit::RowKey;
use crate::sort::SortSpec;

/// Marks an original row that isn't in the order.
const ABSENT: u32 = u32::MAX;

pub struct Order {
    /// `None`: a custom order (the sorted column was deleted).
    pub spec: Option<SortSpec>,
    /// Display position → row.
    keys: Vec<RowKey>,
    /// Original data row → display position.
    inv_orig: Vec<u32>,
    /// Added row id → display position.
    inv_new: HashMap<u32, u32>,
    /// Identifies this sort (positions recorded for undo are only valid within it).
    pub generation: u64,
    /// Cells changed since sorting: the order may no longer match the values.
    pub stale: bool,
    /// Compared as numbers (vs. natural text order).
    #[cfg_attr(not(test), allow(dead_code))]
    pub numeric: bool,
}

impl Order {
    /// Build from the display order (on the sort thread).
    pub fn from_keys(spec: Option<SortSpec>, keys: Vec<RowKey>, numeric: bool) -> Self {
        let mut o = Self {
            spec,
            keys,
            inv_orig: Vec::new(),
            inv_new: HashMap::new(),
            generation: 0,
            stale: false,
            numeric,
        };
        o.rebuild_inverse();
        o
    }

    fn rebuild_inverse(&mut self) {
        let max_orig = self
            .keys
            .iter()
            .filter_map(|k| k.as_orig())
            .max()
            .map_or(0, |m| m as usize + 1);
        self.inv_orig = vec![ABSENT; max_orig];
        self.inv_new.clear();
        self.reindex_from(0);
    }

    /// Point the inverse of every key from display position `d` on.
    fn reindex_from(&mut self, d: usize) {
        for (i, k) in self.keys.iter().enumerate().skip(d) {
            if let Some(r) = k.as_orig() {
                let r = r as usize;
                if r >= self.inv_orig.len() {
                    self.inv_orig.resize(r + 1, ABSENT);
                }
                self.inv_orig[r] = i as u32;
            } else if let Some(id) = k.as_added() {
                self.inv_new.insert(id, i as u32);
            }
        }
    }

    fn forget(&mut self, k: RowKey) {
        if let Some(r) = k.as_orig() {
            if let Some(slot) = self.inv_orig.get_mut(r as usize) {
                *slot = ABSENT;
            }
        } else if let Some(id) = k.as_added() {
            self.inv_new.remove(&id);
        }
    }

    pub fn len(&self) -> u64 {
        self.keys.len() as u64
    }

    pub fn key(&self, d: u64) -> Option<RowKey> {
        self.keys.get(d as usize).copied()
    }

    pub fn display_of(&self, k: RowKey) -> Option<u64> {
        let d = if let Some(r) = k.as_orig() {
            *self.inv_orig.get(r as usize)?
        } else {
            *self.inv_new.get(&k.as_added()?)?
        };
        (d != ABSENT).then_some(d as u64)
    }

    /// Show `keys` at display position `d` (clamped). O(n − d).
    pub fn insert(&mut self, d: u64, keys: &[RowKey]) {
        let d = (d as usize).min(self.keys.len());
        self.keys.splice(d..d, keys.iter().copied());
        self.reindex_from(d);
    }

    /// Remove display positions `d0..=d1`. O(n − d0).
    pub fn remove(&mut self, d0: u64, d1: u64) -> Vec<RowKey> {
        let (d0, d1) = (
            d0 as usize,
            (d1 as usize).min(self.keys.len().saturating_sub(1)),
        );
        if d0 > d1 {
            return Vec::new();
        }
        let removed: Vec<RowKey> = self.keys.drain(d0..=d1).collect();
        for &k in &removed {
            self.forget(k);
        }
        self.reindex_from(d0);
        removed
    }

    /// Remove these keys wherever they are. O(n).
    pub fn remove_keys(&mut self, keys: &[RowKey]) {
        let mut first = usize::MAX;
        let mut drop = vec![false; self.keys.len()];
        for &k in keys {
            if let Some(d) = self.display_of(k) {
                drop[d as usize] = true;
                first = first.min(d as usize);
                self.forget(k);
            }
        }
        if first == usize::MAX {
            return;
        }
        let mut i = 0;
        self.keys.retain(|_| {
            i += 1;
            !drop[i - 1]
        });
        self.reindex_from(first);
    }

    /// Rename every key (after a save: rows get their new file positions).
    pub fn translate(&mut self, mut f: impl FnMut(RowKey) -> RowKey) {
        for k in &mut self.keys {
            *k = f(*k);
        }
        self.rebuild_inverse();
    }

    pub fn keys(&self) -> &[RowKey] {
        &self.keys
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(o: &Order) {
        for (d, k) in o.keys().iter().enumerate() {
            assert_eq!(o.display_of(*k), Some(d as u64));
            assert_eq!(o.key(d as u64), Some(*k));
        }
    }

    #[test]
    fn inverse_follows_inserts_and_removals() {
        let keys: Vec<RowKey> = [3, 0, 2, 1].iter().map(|&r| RowKey::orig(r)).collect();
        let mut o = Order::from_keys(None, keys, false);
        check(&o);
        assert_eq!(o.display_of(RowKey::orig(3)), Some(0));
        o.insert(1, &[RowKey::added(0), RowKey::added(1)]);
        check(&o);
        assert_eq!(o.len(), 6);
        assert_eq!(o.display_of(RowKey::orig(0)), Some(3));
        let removed = o.remove(2, 3);
        assert_eq!(removed, [RowKey::added(1), RowKey::orig(0)]);
        assert_eq!(o.display_of(RowKey::orig(0)), None);
        assert_eq!(o.display_of(RowKey::added(1)), None);
        check(&o);
        o.remove_keys(&[RowKey::orig(3), RowKey::orig(1)]);
        check(&o);
        assert_eq!(o.keys(), [RowKey::added(0), RowKey::orig(2)]);
        o.insert(99, &[RowKey::orig(7)]); // clamped to the end, inverse grows
        check(&o);
        assert_eq!(o.display_of(RowKey::orig(7)), Some(2));
    }

    #[test]
    fn translate_renames_keys() {
        let keys = vec![RowKey::added(0), RowKey::orig(1), RowKey::orig(0)];
        let mut o = Order::from_keys(None, keys, false);
        // After a save the logical order was [orig 0, added 0, orig 1].
        o.translate(|k| match k {
            k if k == RowKey::orig(0) => RowKey::orig(0),
            k if k == RowKey::added(0) => RowKey::orig(1),
            _ => RowKey::orig(2),
        });
        check(&o);
        assert_eq!(
            o.keys(),
            [RowKey::orig(1), RowKey::orig(2), RowKey::orig(0)]
        );
        assert_eq!(o.display_of(RowKey::added(0)), None);
    }
}
