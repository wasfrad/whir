//! Stable identities for rows and columns.
//!
//! Everything that outlives a frame (overrides, sort order, find results, undo)
//! refers to rows and columns by key, never by position: positions shift with
//! every insert/delete, keys don't. Logical positions are computed on demand.

/// Files with more data rows than this are read-only (keys are 31 bits).
pub const MAX_EDIT_ROWS: u64 = 0x7FFF_FFFF;

const HEADER: u32 = 0x7FFF_FFFF;
const NEW_BIT: u32 = 0x8000_0000;

/// A row: original data row `r` (0 = first row after the header) is `r`, the
/// header is `0x7FFF_FFFF`, user-added rows are `0x8000_0000 | id`.
///
/// The ordering matters: original rows sort by file position, then the header,
/// then added rows — so a cursor over overrides advances with a file pass.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct RowKey(u32);

impl RowKey {
    pub const HEADER: RowKey = RowKey(HEADER);

    /// Original data row `r`.
    #[inline]
    pub fn orig(r: u64) -> Self {
        debug_assert!(r < MAX_EDIT_ROWS);
        RowKey(r.min(MAX_EDIT_ROWS - 1) as u32)
    }

    /// A row the user added.
    #[inline]
    pub fn added(id: u32) -> Self {
        debug_assert!(id < NEW_BIT);
        RowKey(NEW_BIT | id)
    }

    /// The original data row, if this is one.
    #[inline]
    pub fn as_orig(self) -> Option<u64> {
        (self.0 < HEADER).then_some(self.0 as u64)
    }

    /// The id of an added row.
    #[inline]
    pub fn as_added(self) -> Option<u32> {
        (self.0 & NEW_BIT != 0).then_some(self.0 & !NEW_BIT)
    }

    #[inline]
    pub fn is_header(self) -> bool {
        self.0 == HEADER
    }

    #[inline]
    pub fn is_added(self) -> bool {
        self.0 & NEW_BIT != 0
    }
}

/// A column: original file column `c` is `c`, user-added columns are
/// `0x8000_0000 | id`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct ColKey(u32);

impl ColKey {
    #[inline]
    pub fn orig(c: usize) -> Self {
        debug_assert!((c as u64) < NEW_BIT as u64);
        ColKey(c as u32)
    }

    #[inline]
    pub fn added(id: u32) -> Self {
        debug_assert!(id < NEW_BIT);
        ColKey(NEW_BIT | id)
    }

    /// The file column, if this is one.
    #[inline]
    pub fn as_orig(self) -> Option<usize> {
        (self.0 & NEW_BIT == 0).then_some(self.0 as usize)
    }

    #[inline]
    pub fn is_added(self) -> bool {
        self.0 & NEW_BIT != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordering_and_round_trips() {
        assert!(RowKey::orig(5) < RowKey::HEADER);
        assert!(RowKey::HEADER < RowKey::added(0));
        assert!(RowKey::orig(MAX_EDIT_ROWS - 1) < RowKey::HEADER);
        assert_eq!(RowKey::orig(7).as_orig(), Some(7));
        assert_eq!(RowKey::orig(7).as_added(), None);
        assert_eq!(RowKey::added(3).as_added(), Some(3));
        assert_eq!(RowKey::added(3).as_orig(), None);
        assert_eq!(RowKey::HEADER.as_orig(), None);
        assert_eq!(RowKey::HEADER.as_added(), None);
        assert!(ColKey::orig(1) < ColKey::added(0));
        assert_eq!(ColKey::orig(4).as_orig(), Some(4));
        assert_eq!(ColKey::added(4).as_orig(), None);
    }
}
