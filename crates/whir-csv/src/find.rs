//! Background search across the whole file, streaming matches as found.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use whir_core::Waker;

use crate::dialect::{Dialect, Encoding};
use crate::parse::{FieldBuf, RowReader, decode_into};
use crate::source::ByteSource;

pub const MAX_MATCHES: usize = 1_000_000;

pub enum FindMsg {
    /// (data row, column) pairs; data row 0 is the first row after the header.
    Matches(Vec<(u32, u32)>),
    Done {
        truncated: bool,
    },
}

#[allow(clippy::too_many_arguments)]
pub fn spawn(
    source: Arc<dyn ByteSource>,
    dialect: Dialect,
    encoding: Encoding,
    first_row_offset: u64,
    query: String,
    cancel: Arc<AtomicBool>,
    tx: Sender<FindMsg>,
    waker: Waker,
) {
    std::thread::Builder::new()
        .name("csv-find".into())
        .spawn(move || {
            let truncated = run(
                &*source,
                dialect,
                encoding,
                first_row_offset,
                &query,
                &cancel,
                &tx,
                &waker,
            );
            if !cancel.load(Ordering::Relaxed) {
                let _ = tx.send(FindMsg::Done { truncated });
                waker.wake();
            }
        })
        .expect("spawn csv find");
}

#[allow(clippy::too_many_arguments)]
fn run(
    src: &dyn ByteSource,
    dialect: Dialect,
    encoding: Encoding,
    offset: u64,
    query: &str,
    cancel: &AtomicBool,
    tx: &Sender<FindMsg>,
    waker: &Waker,
) -> bool {
    let needle_lower = query.to_lowercase();
    let ascii = needle_lower.is_ascii() && encoding != Encoding::Windows1252;
    let finder = memchr::memmem::Finder::new(needle_lower.as_bytes());
    let mut reader = RowReader::new(src, dialect, offset);
    let mut fields = FieldBuf::default();
    if reader.next_row(&mut fields).ok().flatten().is_none() {
        return false; // no header, nothing to search
    }
    let mut batch = Vec::new();
    let mut found = 0usize;
    let mut last_send = Instant::now();
    let mut decoded = String::new();
    let mut row: u32 = 0;
    while let Ok(Some(_)) = reader.next_row(&mut fields) {
        if row % 8192 == 0 && cancel.load(Ordering::Relaxed) {
            return false;
        }
        for (col, field) in fields.iter().enumerate() {
            let hit = if ascii {
                // ASCII needle: fold ASCII case in place, then SIMD substring search.
                field.len() >= needle_lower.len() && {
                    let lowered: Vec<u8>;
                    let hay = if field.iter().any(u8::is_ascii_uppercase) {
                        lowered = field.to_ascii_lowercase();
                        &lowered[..]
                    } else {
                        field
                    };
                    finder.find(hay).is_some()
                }
            } else {
                decoded.clear();
                decode_into(field, encoding, &mut decoded);
                decoded.to_lowercase().contains(&needle_lower)
            };
            if hit {
                batch.push((row, col as u32));
                found += 1;
                if found >= MAX_MATCHES {
                    let _ = tx.send(FindMsg::Matches(std::mem::take(&mut batch)));
                    waker.wake();
                    return true;
                }
            }
        }
        row = row.saturating_add(1);
        if !batch.is_empty()
            && (batch.len() >= 4096 || last_send.elapsed() > Duration::from_millis(100))
        {
            let _ = tx.send(FindMsg::Matches(std::mem::take(&mut batch)));
            waker.wake();
            last_send = Instant::now();
        }
    }
    if !batch.is_empty() {
        let _ = tx.send(FindMsg::Matches(batch));
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    struct Mem(Vec<u8>);
    impl ByteSource for Mem {
        fn len(&self) -> u64 {
            self.0.len() as u64
        }
        fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
            let o = (offset as usize).min(self.0.len());
            let n = buf.len().min(self.0.len() - o);
            buf[..n].copy_from_slice(&self.0[o..o + n]);
            Ok(n)
        }
    }

    fn find(data: &[u8], q: &str) -> Vec<(u32, u32)> {
        let (tx, rx) = channel();
        let src = Mem(data.to_vec());
        run(
            &src,
            Dialect::default(),
            Encoding::Utf8,
            0,
            q,
            &AtomicBool::new(false),
            &tx,
            &Waker::noop(),
        );
        drop(tx);
        rx.iter()
            .flat_map(|m| match m {
                FindMsg::Matches(v) => v,
                FindMsg::Done { .. } => vec![],
            })
            .collect()
    }

    #[test]
    fn case_insensitive_across_fields_and_quoted_newlines() {
        let data = b"name,note\nAlice,\"likes\nCATS\"\nbob,dogs\ncarol,cats and dogs\n";
        assert_eq!(find(data, "cats"), [(0, 1), (2, 1)]);
        assert_eq!(find(data, "BOB"), [(1, 0)]);
        assert_eq!(find(data, "name"), []); // header isn't searched
        assert_eq!(find("x\nÉcole\n".as_bytes(), "école"), [(0, 0)]);
    }
}
