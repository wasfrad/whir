//! The logical row order after inserts and deletes: a piece table over the
//! file's rows and the rows the user added.
//!
//! Original rows are never moved, only removed (and restored by undo), so they
//! always appear in increasing file order. That keeps key → position lookups
//! a binary search and lets a save read the file in one sequential pass.

use super::keys::RowKey;

/// A run of consecutive rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Piece {
    /// Original data rows `start..start + len`.
    Orig { start: u32, len: u32 },
    /// Added rows with ids `first..first + len`.
    New { first: u32, len: u32 },
}

impl Piece {
    pub fn len(&self) -> u32 {
        match *self {
            Piece::Orig { len, .. } | Piece::New { len, .. } => len,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Key of the `i`-th row of the run.
    pub fn key(&self, i: u32) -> RowKey {
        match *self {
            Piece::Orig { start, .. } => RowKey::orig((start + i) as u64),
            Piece::New { first, .. } => RowKey::added(first + i),
        }
    }

    /// Rows `a..b` of the run.
    fn slice(&self, a: u32, b: u32) -> Piece {
        match *self {
            Piece::Orig { start, .. } => Piece::Orig {
                start: start + a,
                len: b - a,
            },
            Piece::New { first, .. } => Piece::New {
                first: first + a,
                len: b - a,
            },
        }
    }

    /// `other` continues this run.
    fn joins(&self, other: &Piece) -> bool {
        match (*self, *other) {
            (Piece::Orig { start, len }, Piece::Orig { start: s2, .. }) => start + len == s2,
            (Piece::New { first, len }, Piece::New { first: f2, .. }) => first + len == f2,
            _ => false,
        }
    }

    fn grow(&mut self, by: u32) {
        match self {
            Piece::Orig { len, .. } | Piece::New { len, .. } => *len += by,
        }
    }

    pub fn keys(&self) -> impl Iterator<Item = RowKey> + '_ {
        (0..self.len()).map(|i| self.key(i))
    }
}

/// A removed run and the logical position it had. Re-inserting a list of these
/// in order restores the rows exactly.
pub type Removed = Vec<(u64, Piece)>;

#[derive(Clone, Debug)]
pub struct RowOrder {
    /// Data rows in the file.
    base: u32,
    pieces: Vec<Piece>,
    /// Cumulative logical end of each piece.
    ends: Vec<u64>,
    /// Indices of the `Orig` pieces (their starts ascend).
    orig: Vec<u32>,
    /// `(first id, piece index)` of the `New` pieces, sorted by id.
    new: Vec<(u32, u32)>,
}

impl RowOrder {
    /// The file's own order: `base` data rows.
    pub fn identity(base: u32) -> Self {
        let mut o = Self {
            base,
            pieces: if base > 0 {
                vec![Piece::Orig {
                    start: 0,
                    len: base,
                }]
            } else {
                Vec::new()
            },
            ends: Vec::new(),
            orig: Vec::new(),
            new: Vec::new(),
        };
        o.rebuild();
        o
    }

    /// Exactly the file's rows, in file order.
    pub fn is_identity(&self) -> bool {
        match self.pieces.as_slice() {
            [] => self.base == 0,
            [Piece::Orig { start: 0, len }] => *len == self.base,
            _ => false,
        }
    }

    pub fn pieces(&self) -> &[Piece] {
        &self.pieces
    }

    pub fn len(&self) -> u64 {
        self.ends.last().copied().unwrap_or(0)
    }

    /// Piece index and offset of logical row `i` (< len).
    fn locate(&self, i: u64) -> (usize, u32) {
        let p = self.ends.partition_point(|&e| e <= i);
        let start = if p == 0 { 0 } else { self.ends[p - 1] };
        (p, (i - start) as u32)
    }

    /// Logical start of piece `p`.
    fn start_of(&self, p: usize) -> u64 {
        if p == 0 { 0 } else { self.ends[p - 1] }
    }

    /// The row at logical position `i`.
    pub fn key(&self, i: u64) -> Option<RowKey> {
        if i >= self.len() {
            return None;
        }
        let (p, off) = self.locate(i);
        Some(self.pieces[p].key(off))
    }

    /// Logical position of `k` (`None` if deleted or the header).
    pub fn index_of(&self, k: RowKey) -> Option<u64> {
        if let Some(r) = k.as_orig() {
            let r = r as u32;
            let n = self.orig.partition_point(
                |&p| matches!(self.pieces[p as usize], Piece::Orig { start, .. } if start <= r),
            );
            let p = *self.orig.get(n.checked_sub(1)?)? as usize;
            match self.pieces[p] {
                Piece::Orig { start, len } if r < start + len => {
                    Some(self.start_of(p) + (r - start) as u64)
                }
                _ => None,
            }
        } else if let Some(id) = k.as_added() {
            let n = self.new.partition_point(|&(f, _)| f <= id);
            let (first, p) = *self.new.get(n.checked_sub(1)?)?;
            let len = self.pieces[p as usize].len();
            (id < first + len).then(|| self.start_of(p as usize) + (id - first) as u64)
        } else {
            None
        }
    }

    /// Insert `piece` so its first row lands at logical position `at` (≤ len).
    pub fn insert(&mut self, at: u64, piece: Piece) {
        if piece.is_empty() {
            return;
        }
        let at = at.min(self.len());
        let idx = if at == self.len() {
            self.pieces.len()
        } else {
            let (p, off) = self.locate(at);
            if off > 0 {
                let whole = self.pieces[p];
                self.pieces[p] = whole.slice(0, off);
                self.pieces.insert(p + 1, whole.slice(off, whole.len()));
                p + 1
            } else {
                p
            }
        };
        self.pieces.insert(idx, piece);
        self.normalize();
    }

    /// Re-insert runs returned by a removal.
    pub fn restore(&mut self, removed: &Removed) {
        for &(at, piece) in removed {
            self.insert(at, piece);
        }
    }

    /// Remove `n` rows starting at logical position `at`.
    pub fn remove_range(&mut self, at: u64, n: u64) -> Removed {
        self.remove_ranges(&[(at, at + n)])
    }

    /// Remove the rows at these logical positions (ascending, no duplicates).
    pub fn remove_sorted(&mut self, positions: &[u64]) -> Removed {
        let mut ranges: Vec<(u64, u64)> = Vec::new();
        for &i in positions {
            match ranges.last_mut() {
                Some(r) if r.1 == i => r.1 += 1,
                _ => ranges.push((i, i + 1)),
            }
        }
        self.remove_ranges(&ranges)
    }

    /// Remove the runs recorded by an earlier removal (redo).
    pub fn remove_runs(&mut self, removed: &Removed) -> Removed {
        let ranges: Vec<(u64, u64)> = removed
            .iter()
            .map(|&(at, p)| (at, at + p.len() as u64))
            .collect();
        self.remove_ranges(&ranges)
    }

    /// `ranges`: `[start, end)` logical ranges, ascending and disjoint. One pass.
    fn remove_ranges(&mut self, ranges: &[(u64, u64)]) -> Removed {
        let mut kept = Vec::with_capacity(self.pieces.len() + ranges.len());
        let mut removed: Removed = Vec::new();
        let mut ri = 0;
        let mut pos = 0u64;
        for &piece in &self.pieces {
            let (s, e) = (pos, pos + piece.len() as u64);
            let mut cur = s;
            while ri < ranges.len() && ranges[ri].0 < e {
                let (rs, re) = ranges[ri];
                let a = rs.max(cur);
                let b = re.min(e);
                if a > cur {
                    kept.push(piece.slice((cur - s) as u32, (a - s) as u32));
                }
                if b > a {
                    let run = piece.slice((a - s) as u32, (b - s) as u32);
                    // Adjacent runs from neighbouring pieces stay separate
                    // entries; restore() re-inserts them in order.
                    removed.push((a, run));
                }
                cur = cur.max(b);
                if re <= e {
                    ri += 1;
                } else {
                    break; // the range continues into the next piece
                }
            }
            if cur < e {
                kept.push(piece.slice((cur - s) as u32, (e - s) as u32));
            }
            pos = e;
        }
        self.pieces = kept;
        self.normalize();
        removed
    }

    /// Merge neighbouring runs, drop empty ones, recompute the lookups.
    fn normalize(&mut self) {
        let mut out: Vec<Piece> = Vec::with_capacity(self.pieces.len());
        for p in self.pieces.drain(..) {
            if p.is_empty() {
                continue;
            }
            match out.last_mut() {
                Some(last) if last.joins(&p) => last.grow(p.len()),
                _ => out.push(p),
            }
        }
        self.pieces = out;
        self.rebuild();
    }

    fn rebuild(&mut self) {
        self.ends.clear();
        self.orig.clear();
        self.new.clear();
        let mut end = 0u64;
        for (i, p) in self.pieces.iter().enumerate() {
            end += p.len() as u64;
            self.ends.push(end);
            match *p {
                Piece::Orig { .. } => self.orig.push(i as u32),
                Piece::New { first, .. } => self.new.push((first, i as u32)),
            }
        }
        self.new.sort_unstable();
    }

    /// Every key in logical order.
    #[cfg(test)]
    pub fn keys(&self) -> impl Iterator<Item = RowKey> + '_ {
        self.pieces.iter().flat_map(|p| p.keys())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(o: &RowOrder) {
        let keys: Vec<RowKey> = o.keys().collect();
        assert_eq!(keys.len() as u64, o.len());
        for (i, k) in keys.iter().enumerate() {
            assert_eq!(o.key(i as u64), Some(*k), "key({i})");
            assert_eq!(o.index_of(*k), Some(i as u64), "index_of({k:?})");
        }
        // Original rows stay in file order.
        let origs: Vec<u64> = keys.iter().filter_map(|k| k.as_orig()).collect();
        assert!(origs.windows(2).all(|w| w[0] < w[1]));
    }

    fn orig(r: u64) -> RowKey {
        RowKey::orig(r)
    }

    #[test]
    fn insert_split_remove_restore() {
        let mut o = RowOrder::identity(10);
        assert!(o.is_identity());
        check(&o);
        o.insert(0, Piece::New { first: 0, len: 2 });
        o.insert(7, Piece::New { first: 2, len: 1 }); // splits the orig run
        o.insert(o.len(), Piece::New { first: 3, len: 1 });
        check(&o);
        assert_eq!(o.len(), 14);
        assert_eq!(o.key(0), Some(RowKey::added(0)));
        assert_eq!(o.key(7), Some(RowKey::added(2)));
        assert_eq!(o.key(8), Some(orig(5)));
        assert_eq!(o.key(13), Some(RowKey::added(3)));
        assert!(!o.is_identity());

        // Remove across piece boundaries, then restore.
        let before: Vec<RowKey> = o.keys().collect();
        let removed = o.remove_range(1, 8);
        check(&o);
        assert_eq!(o.len(), 6);
        assert_eq!(o.index_of(RowKey::added(2)), None);
        assert_eq!(o.index_of(orig(3)), None);
        o.restore(&removed);
        check(&o);
        assert_eq!(o.keys().collect::<Vec<_>>(), before);

        // Scattered removal + redo via remove_runs.
        let removed = o.remove_sorted(&[0, 2, 3, 9, 13]);
        check(&o);
        let after: Vec<RowKey> = o.keys().collect();
        o.restore(&removed);
        assert_eq!(o.keys().collect::<Vec<_>>(), before);
        o.remove_runs(&removed);
        assert_eq!(o.keys().collect::<Vec<_>>(), after);
    }

    #[test]
    fn splitting_new_pieces_and_normalizing_back() {
        let mut o = RowOrder::identity(3);
        o.insert(1, Piece::New { first: 0, len: 4 });
        o.insert(3, Piece::New { first: 4, len: 1 }); // inside the new run
        check(&o);
        assert_eq!(
            o.keys().collect::<Vec<_>>(),
            [
                orig(0),
                RowKey::added(0),
                RowKey::added(1),
                RowKey::added(4),
                RowKey::added(2),
                RowKey::added(3),
                orig(1),
                orig(2)
            ]
        );
        // Deleting every added row collapses back to the identity.
        o.remove_range(1, 5);
        check(&o);
        assert!(o.is_identity());
        assert_eq!(o.pieces().len(), 1);
    }

    #[test]
    fn edges() {
        let mut o = RowOrder::identity(0);
        assert!(o.is_identity());
        assert_eq!(o.key(0), None);
        o.insert(5, Piece::New { first: 0, len: 1 }); // clamped to the end
        check(&o);
        assert_eq!(o.len(), 1);
        let mut o = RowOrder::identity(5);
        let r = o.remove_range(0, 5);
        assert_eq!(o.len(), 0);
        assert_eq!(r, vec![(0, Piece::Orig { start: 0, len: 5 })]);
        assert_eq!(o.index_of(orig(0)), None);
        assert_eq!(o.index_of(RowKey::HEADER), None);
        o.restore(&r);
        assert!(o.is_identity());
    }
}
