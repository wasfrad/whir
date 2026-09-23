//! Random access to rows: checkpoint lookup + a small LRU of parsed 64-row blocks.

use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use crate::dialect::{Dialect, Encoding};
use crate::index::{BLOCK_ROWS, IndexState, SharedIndex};
use crate::parse::{FieldBuf, RowReader, decode_into};
use crate::source::ByteSource;

/// Longest cell text kept for display/copy.
pub const MAX_CELL_BYTES: usize = 32 << 10;

/// Up to 64 consecutive rows, decoded to text.
pub struct Block {
    text: String,
    cells: Vec<(u32, u32)>,
    /// Per row: range into `cells`.
    rows: Vec<(u32, u32)>,
    /// Indices into `cells` of values cut at [`MAX_CELL_BYTES`] (ascending, rare).
    truncated: Vec<u32>,
}

impl Block {
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn row(&self, i: usize) -> Option<RowView<'_>> {
        let &(start, end) = self.rows.get(i)?;
        Some(RowView {
            block: self,
            first: start,
            cells: &self.cells[start as usize..end as usize],
        })
    }
}

#[derive(Clone, Copy)]
pub struct RowView<'a> {
    block: &'a Block,
    /// Index of this row's first cell in `block.cells`.
    first: u32,
    cells: &'a [(u32, u32)],
}

impl<'a> RowView<'a> {
    pub fn len(&self) -> usize {
        self.cells.len()
    }

    pub fn cell(&self, i: usize) -> &'a str {
        match self.cells.get(i) {
            Some(&(s, e)) => &self.block.text[s as usize..e as usize],
            None => "",
        }
    }

    /// Cell `i` is longer than what was kept for display.
    pub fn is_truncated(&self, i: usize) -> bool {
        !self.block.truncated.is_empty()
            && i < self.cells.len()
            && self
                .block
                .truncated
                .binary_search(&(self.first + i as u32))
                .is_ok()
    }
}

/// Snapshot of indexing progress, copied once per frame.
#[derive(Clone, Debug, Default)]
pub struct Progress {
    /// Rows safe to display (the last row is held back while indexing).
    pub rows: u64,
    pub scanned: u64,
    pub total: u64,
    pub done: bool,
    pub quoting_disabled: bool,
    /// Indexing stopped early (read error): `rows` is incomplete.
    pub error: Option<String>,
}

pub struct Table {
    pub source: Arc<dyn ByteSource>,
    pub dialect: Dialect,
    pub encoding: Encoding,
    pub index: SharedIndex,
    cache: HashMap<u64, Rc<Block>>,
    lru: VecDeque<u64>,
    capacity: usize,
    fields: FieldBuf,
    quoting_disabled_seen: bool,
}

impl Table {
    pub fn new(
        source: Arc<dyn ByteSource>,
        dialect: Dialect,
        encoding: Encoding,
        index: SharedIndex,
    ) -> Self {
        // An index that already has quoting off (a save of such a file) isn't
        // a restart: only a change seen later is.
        let quoting_disabled_seen = index.read().map(|s| s.quoting_disabled).unwrap_or(false);
        Self {
            source,
            dialect,
            encoding,
            index,
            cache: HashMap::new(),
            lru: VecDeque::new(),
            capacity: 96,
            fields: FieldBuf::default(),
            quoting_disabled_seen,
        }
    }

    /// Scattered access (sorted views) touches more blocks per screen.
    pub fn set_cache_capacity(&mut self, blocks: usize) {
        self.capacity = blocks.max(8);
        self.evict();
    }

    pub fn progress(&self) -> Progress {
        let s = self.index.read().map(|s| s.clone()).unwrap_or_default();
        Self::progress_of(&s)
    }

    fn progress_of(s: &IndexState) -> Progress {
        Progress {
            rows: if s.done {
                s.rows
            } else {
                s.rows.saturating_sub(1)
            },
            scanned: s.scanned,
            total: s.total,
            done: s.done,
            quoting_disabled: s.quoting_disabled,
            error: s.error.clone(),
        }
    }

    /// Call once per frame. Returns true if the indexer restarted with quoting
    /// off (unbalanced quotes): every parsed block is stale and was dropped.
    pub fn sync(&mut self) -> bool {
        let disabled = self
            .index
            .read()
            .map(|s| s.quoting_disabled)
            .unwrap_or(false);
        if disabled == self.quoting_disabled_seen {
            return false;
        }
        self.quoting_disabled_seen = disabled;
        self.cache.clear();
        self.lru.clear();
        true
    }

    /// Effective dialect (quoting may have been disabled by the indexer).
    pub fn effective_dialect(&self) -> Dialect {
        let disabled = self
            .index
            .read()
            .map(|s| s.quoting_disabled)
            .unwrap_or(false);
        Dialect {
            quoting: self.dialect.quoting && !disabled,
            ..self.dialect
        }
    }

    fn evict(&mut self) {
        while self.lru.len() > self.capacity {
            if let Some(b) = self.lru.pop_front() {
                self.cache.remove(&b);
            }
        }
    }

    /// Row `file_row` (0 = header), via its block.
    pub fn row(&mut self, file_row: u64) -> Option<(Rc<Block>, usize)> {
        let block = self.block(file_row / BLOCK_ROWS)?;
        let i = (file_row % BLOCK_ROWS) as usize;
        (i < block.len()).then_some((block, i))
    }

    pub fn block(&mut self, b: u64) -> Option<Rc<Block>> {
        if let Some(block) = self.cache.get(&b) {
            let block = block.clone();
            if let Some(pos) = self.lru.iter().position(|&x| x == b) {
                self.lru.remove(pos);
            }
            self.lru.push_back(b);
            return Some(block);
        }
        let (offset, limit, visible_rows, done) = {
            let s = self.index.read().ok()?;
            let offset = *s.checkpoints.get(b as usize)?;
            // Never read past what the indexer has confirmed.
            let limit = s
                .checkpoints
                .get(b as usize + 1)
                .copied()
                .unwrap_or(if s.done { s.total } else { s.scanned });
            (offset, limit, Self::progress_of(&s).rows, s.done)
        };
        let first = b * BLOCK_ROWS;
        let want = (visible_rows.saturating_sub(first)).min(BLOCK_ROWS) as usize;
        let block = Rc::new(self.parse_block(offset, limit, want).ok()?);
        // Partial trailing blocks change while indexing: only cache complete ones.
        if block.len() == BLOCK_ROWS as usize || done {
            self.cache.insert(b, block.clone());
            self.lru.push_back(b);
            self.evict();
        }
        Some(block)
    }

    fn parse_block(&mut self, offset: u64, limit: u64, rows: usize) -> std::io::Result<Block> {
        let dialect = self.effective_dialect();
        let mut reader = RowReader::with_limit(&*self.source, dialect, offset, limit);
        let mut block = Block {
            text: String::new(),
            cells: Vec::new(),
            rows: Vec::with_capacity(rows),
            truncated: Vec::new(),
        };
        for _ in 0..rows {
            if reader.next_row(&mut self.fields)?.is_none() {
                break;
            }
            let start = block.cells.len() as u32;
            for field in self.fields.iter() {
                let s = block.text.len();
                if field.len() > MAX_CELL_BYTES {
                    block.truncated.push(block.cells.len() as u32);
                }
                let field = &field[..field.len().min(MAX_CELL_BYTES)];
                decode_into(field, self.encoding, &mut block.text);
                block.cells.push((s as u32, block.text.len() as u32));
            }
            block.rows.push((start, block.cells.len() as u32));
        }
        Ok(block)
    }

    /// Every cell of `file_row`, untruncated (editing a long cell).
    pub fn read_full_row(&mut self, file_row: u64) -> Option<Vec<String>> {
        let (offset, limit) = {
            let s = self.index.read().ok()?;
            let b = (file_row / BLOCK_ROWS) as usize;
            let offset = *s.checkpoints.get(b)?;
            let limit = s.checkpoints.get(b + 1).copied().unwrap_or(if s.done {
                s.total
            } else {
                s.scanned
            });
            (offset, limit)
        };
        let mut reader =
            RowReader::with_limit(&*self.source, self.effective_dialect(), offset, limit);
        let skip = file_row % BLOCK_ROWS;
        if reader.skip_rows(skip, &mut self.fields).ok()? != skip {
            return None;
        }
        reader.next_row(&mut self.fields).ok()??;
        Some(
            self.fields
                .iter()
                .map(|f| {
                    let mut s = String::new();
                    decode_into(f, self.encoding, &mut s);
                    s
                })
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index;
    use crate::source::MemSource;
    use std::sync::RwLock;

    fn table_of(data: &[u8]) -> Table {
        let src = MemSource::new(data.to_vec());
        let idx = index::build_sync(&src, 0, Dialect::default());
        Table::new(
            Arc::new(src),
            Dialect::default(),
            Encoding::Utf8,
            Arc::new(RwLock::new(idx)),
        )
    }

    #[test]
    fn long_cells_are_truncated_and_read_in_full() {
        let long = "x".repeat(MAX_CELL_BYTES + 10);
        let data = format!("a,b\n1,{long}\n2,3\n");
        let mut t = table_of(data.as_bytes());
        let (b, i) = t.row(1).unwrap();
        let row = b.row(i).unwrap();
        assert!(!row.is_truncated(0));
        assert!(row.is_truncated(1));
        assert_eq!(row.cell(1).len(), MAX_CELL_BYTES);
        let (b, i) = t.row(2).unwrap();
        assert!(!b.row(i).unwrap().is_truncated(1));
        let full = t.read_full_row(1).unwrap();
        assert_eq!(full, ["1".to_owned(), long]);
        assert_eq!(t.read_full_row(0).unwrap(), ["a", "b"]);
        assert!(t.read_full_row(3).is_none());
    }
}
