//! Background row indexing: one sequential pass records the byte offset of
//! every 64th row. 10M rows cost ~1.25 MB. Results are published as they
//! grow, so the grid shows (and scrolls) the top of the file immediately.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use whir_core::Waker;

use crate::dialect::Dialect;
use crate::scan::{RowScanner, ScanError};
use crate::source::ByteSource;

/// Rows per checkpoint.
pub const BLOCK_ROWS: u64 = 64;

#[derive(Clone, Debug, Default)]
pub struct IndexState {
    /// `checkpoints[k]` = file offset of row `k * BLOCK_ROWS` (row 0 = header).
    pub checkpoints: Vec<u64>,
    /// Rows found so far (including the header row).
    pub rows: u64,
    /// Bytes scanned so far; rows are final up to here.
    pub scanned: u64,
    pub total: u64,
    pub done: bool,
    /// Quoting was disabled after finding an unbalanced quote.
    pub quoting_disabled: bool,
    pub error: Option<String>,
}

pub type SharedIndex = Arc<RwLock<IndexState>>;

pub fn spawn(
    source: Arc<dyn ByteSource>,
    start: u64,
    dialect: Dialect,
    shared: SharedIndex,
    waker: Waker,
    cancel: Arc<AtomicBool>,
) {
    std::thread::Builder::new()
        .name("csv-index".into())
        .spawn(move || {
            index_all(&*source, start, dialect, &shared, &waker, &cancel);
            waker.wake();
        })
        .expect("spawn csv indexer");
}

/// Index a small source (a new blank table) on the calling thread.
pub fn build_sync(src: &dyn ByteSource, start: u64, dialect: Dialect) -> IndexState {
    let shared: SharedIndex = Default::default();
    index_all(
        src,
        start,
        dialect,
        &shared,
        &Waker::noop(),
        &AtomicBool::new(false),
    );
    shared.read().map(|s| s.clone()).unwrap_or_default()
}

fn index_all(
    src: &dyn ByteSource,
    start: u64,
    mut dialect: Dialect,
    shared: &SharedIndex,
    waker: &Waker,
    cancel: &AtomicBool,
) {
    loop {
        match run(src, start, dialect, shared, waker, cancel) {
            Err(ScanError::Unbalanced) if dialect.quoting => {
                // Start over, treating quotes as plain text.
                dialect.quoting = false;
                if let Ok(mut s) = shared.write() {
                    *s = IndexState {
                        quoting_disabled: true,
                        ..IndexState::default()
                    };
                }
            }
            _ => break,
        }
    }
}

fn run(
    src: &dyn ByteSource,
    start: u64,
    dialect: Dialect,
    shared: &SharedIndex,
    waker: &Waker,
    cancel: &AtomicBool,
) -> Result<(), ScanError> {
    let total = src.len();
    let mut scanner = RowScanner::new(dialect);
    // Pending checkpoints/rows not yet published.
    let mut new_checkpoints: Vec<u64> = Vec::with_capacity(1024);
    let mut rows: u64 = 0;
    if start < total {
        new_checkpoints.push(start);
        rows = 1;
    }
    let mut buf = vec![0u8; 1 << 20];
    let mut offset = start;
    let mut last_wake = Instant::now();
    let publish =
        |rows: u64, scanned: u64, cps: &mut Vec<u64>, done: bool, error: Option<String>| {
            if let Ok(mut s) = shared.write() {
                s.checkpoints.append(cps);
                s.rows = rows;
                s.scanned = scanned;
                s.total = total;
                s.done = done;
                if error.is_some() {
                    s.error = error;
                }
            }
        };
    publish(rows, offset, &mut new_checkpoints, false, None);

    while offset < total {
        if cancel.load(Ordering::Relaxed) {
            return Ok(());
        }
        let n = match src.read_at(offset, &mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                publish(
                    rows,
                    offset,
                    &mut new_checkpoints,
                    true,
                    Some(e.to_string()),
                );
                return Ok(());
            }
        };
        scanner.feed(&buf[..n], offset, |row_start| {
            if row_start < total {
                if rows.is_multiple_of(BLOCK_ROWS) {
                    new_checkpoints.push(row_start);
                }
                rows += 1;
            }
        })?;
        offset += n as u64;
        // Publish every chunk (cheap); wake the UI at most ~10x per second.
        publish(rows, offset, &mut new_checkpoints, false, None);
        if last_wake.elapsed() > Duration::from_millis(100) {
            last_wake = Instant::now();
            waker.wake();
        }
    }
    scanner.finish()?;
    publish(rows, offset.max(total), &mut new_checkpoints, true, None);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::MemSource;

    fn index(data: Vec<u8>) -> IndexState {
        let shared: SharedIndex = Default::default();
        let src: Arc<dyn ByteSource> = Arc::new(MemSource::new(data));
        spawn(
            src,
            0,
            Dialect::default(),
            shared.clone(),
            Waker::noop(),
            Default::default(),
        );
        loop {
            if let Ok(s) = shared.read()
                && s.done
            {
                return s.clone();
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn counts_rows_and_checkpoints() {
        let mut data = b"id,value\n".to_vec();
        for i in 0..1000 {
            data.extend(format!("{i},\"v\n{i}\"\n").as_bytes());
        }
        let s = index(data.clone());
        assert_eq!(s.rows, 1001);
        assert_eq!(s.checkpoints.len(), 1001usize.div_ceil(64));
        assert_eq!(s.checkpoints[0], 0);
        // Checkpoint 1 is row 64 = data row 63 ("63,...").
        let off = s.checkpoints[1] as usize;
        assert!(data[off..].starts_with(b"63,"));
        assert!(!s.quoting_disabled);
    }

    #[test]
    fn sync_build_matches_background() {
        let mut data = b"\xEF\xBB\xBFh\r\n".to_vec();
        for i in 0..200 {
            data.extend(format!("{i}\r\n").as_bytes());
        }
        let a = build_sync(&MemSource::new(data.clone()), 3, Dialect::default());
        assert!(a.done);
        assert_eq!(a.rows, 201);
        assert_eq!(a.checkpoints[0], 3);
        assert_eq!(a.checkpoints.len(), 4);
    }

    #[test]
    fn no_trailing_empty_row() {
        assert_eq!(index(b"a\nb\n".to_vec()).rows, 2);
        assert_eq!(index(b"a\nb".to_vec()).rows, 2);
        assert_eq!(index(Vec::new()).rows, 0);
    }

    #[test]
    fn unbalanced_quote_at_eof_falls_back_to_plain_lines() {
        for tail_rows in [2, 300_000] {
            for final_newline in [false, true] {
                // The fallback must also split earlier, valid multiline fields.
                let mut data = b"id,note\n0,\"multi\nline\"\n".to_vec();
                // Publish several checkpoints before the malformed field.
                for _ in 0..128 {
                    data.extend_from_slice(b"0,ok\n");
                }
                data.extend_from_slice(b"1,\"oops\n");
                for _ in 0..tail_rows {
                    data.extend_from_slice(b"2,value\n");
                }
                if !final_newline {
                    data.pop();
                }
                let total = data.len() as u64;
                assert!(total < crate::scan::MAX_QUOTED_FIELD);
                let checkpoints: Vec<u64> = std::iter::once(0)
                    .chain(
                        memchr::memchr_iter(b'\n', &data)
                            .map(|i| i as u64 + 1)
                            .filter(|&offset| offset < total),
                    )
                    .step_by(BLOCK_ROWS as usize)
                    .collect();
                let s = index(data);
                assert!(
                    s.quoting_disabled,
                    "{tail_rows} rows, final newline: {final_newline}"
                );
                assert_eq!(s.rows, tail_rows + 132);
                assert_eq!(s.checkpoints, checkpoints);
                assert_eq!(s.scanned, total);
                assert_eq!(s.total, total);
                assert!(s.done && s.error.is_none());
            }
        }
    }

    #[test]
    fn quoted_field_closed_at_eof_keeps_quoting() {
        for data in [
            b"id,note\n1,\"multi\nline\"".as_slice(),
            b"id,note\n1,\"\"",
            b"id,note\n1,\"say \"\"hi\"\"\"",
        ] {
            let s = index(data.to_vec());
            assert!(!s.quoting_disabled);
            assert_eq!(s.rows, 2);
            assert!(s.done && s.error.is_none());
        }
    }

    #[test]
    fn unbalanced_quote_falls_back_to_plain_lines() {
        let mut data = b"a,\"oops\n".to_vec();
        for _ in 0..(crate::scan::MAX_QUOTED_FIELD / 8 + 10) {
            data.extend(b"1,2,3,4\n");
        }
        let s = index(data);
        assert!(s.quoting_disabled);
        assert_eq!(s.rows, crate::scan::MAX_QUOTED_FIELD / 8 + 11);
    }
}
