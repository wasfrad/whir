//! Column positions after inserts and deletes.
//!
//! The first `cols.len()` positions are explicit; positions after that are the
//! file's remaining columns in order (`tail`, `tail + 1`, ...), so ragged rows
//! wider than anything seen when the map was made keep their extra fields.

use super::keys::ColKey;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColMap {
    cols: Vec<ColKey>,
    /// File column shown at position `cols.len()`.
    tail: u32,
}

impl ColMap {
    /// The file's own columns, the first `width` of them explicit.
    pub fn identity(width: usize) -> Self {
        Self {
            cols: (0..width).map(ColKey::orig).collect(),
            tail: width as u32,
        }
    }

    pub fn is_identity(&self) -> bool {
        self.tail as usize == self.cols.len()
            && self
                .cols
                .iter()
                .enumerate()
                .all(|(i, k)| k.as_orig() == Some(i))
    }

    /// Explicit positions (every row is at least this wide once columns changed).
    pub fn len(&self) -> usize {
        self.cols.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cols.is_empty()
    }

    /// The column at position `c`.
    pub fn key(&self, c: usize) -> ColKey {
        match self.cols.get(c) {
            Some(&k) => k,
            None => ColKey::orig(self.tail as usize + (c - self.cols.len())),
        }
    }

    /// Position of `k` (`None` if deleted).
    pub fn index_of(&self, k: ColKey) -> Option<usize> {
        if let Some(f) = k.as_orig()
            && f >= self.tail as usize
        {
            return Some(self.cols.len() + (f - self.tail as usize));
        }
        self.cols.iter().position(|&x| x == k)
    }

    /// Positions of a row with `base_len` file fields.
    pub fn row_len(&self, base_len: usize) -> usize {
        self.cols.len() + base_len.saturating_sub(self.tail as usize)
    }

    /// Make positions `0..n` explicit.
    pub fn materialize_to(&mut self, n: usize) {
        while self.cols.len() < n {
            self.cols.push(ColKey::orig(self.tail as usize));
            self.tail += 1;
        }
    }

    /// Insert `keys` so the first lands at position `at`.
    pub fn insert(&mut self, at: usize, keys: &[ColKey]) {
        self.materialize_to(at);
        self.cols.splice(at..at, keys.iter().copied());
    }

    /// Remove these positions (ascending). Returns `(position, key)` pairs;
    /// re-inserting them in order restores the map.
    pub fn remove(&mut self, positions: &[usize]) -> Vec<(usize, ColKey)> {
        if let Some(&last) = positions.last() {
            self.materialize_to(last + 1);
        }
        let removed: Vec<(usize, ColKey)> = positions.iter().map(|&p| (p, self.cols[p])).collect();
        for &p in positions.iter().rev() {
            self.cols.remove(p);
        }
        removed
    }

    /// Undo [`Self::remove`].
    pub fn restore(&mut self, removed: &[(usize, ColKey)]) {
        for &(p, k) in removed {
            self.insert(p, &[k]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ragged_extras_keep_their_order() {
        let mut m = ColMap::identity(3);
        assert!(m.is_identity());
        m.insert(1, &[ColKey::added(0)]);
        assert!(!m.is_identity());
        assert_eq!(m.key(0), ColKey::orig(0));
        assert_eq!(m.key(1), ColKey::added(0));
        assert_eq!(m.key(2), ColKey::orig(1));
        assert_eq!(m.key(3), ColKey::orig(2));
        // A ragged row with 6 fields: file columns 3..5 follow.
        assert_eq!(m.key(4), ColKey::orig(3));
        assert_eq!(m.key(6), ColKey::orig(5));
        assert_eq!(m.index_of(ColKey::orig(5)), Some(6));
        assert_eq!(m.row_len(6), 7);
        assert_eq!(m.row_len(2), 4);

        let removed = m.remove(&[0, 5]); // position 5 is implicit: materialized
        assert_eq!(removed, vec![(0, ColKey::orig(0)), (5, ColKey::orig(4))]);
        assert_eq!(m.index_of(ColKey::orig(0)), None);
        assert_eq!(m.key(0), ColKey::added(0));
        assert_eq!(m.key(4), ColKey::orig(5));
        m.restore(&removed);
        assert_eq!(m.key(0), ColKey::orig(0));
        assert_eq!(m.key(5), ColKey::orig(4));
        assert_eq!(m.key(6), ColKey::orig(5));

        // Remove the added column again: back to identity.
        m.remove(&[1]);
        assert!(m.is_identity());
    }
}
