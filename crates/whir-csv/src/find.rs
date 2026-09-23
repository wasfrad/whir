//! Background search across the whole table, streaming matches as found.
//! Matches name rows by key, so they stay valid while rows are inserted or
//! deleted; edited cells are searched as they are now.

use std::ops::ControlFlow;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use whir_core::Waker;

use crate::dialect::Encoding;
use crate::edit::RowKey;
use crate::parse::decode_into;
use crate::stream::{self, Field, StreamInput};

pub const MAX_MATCHES: usize = 1_000_000;

pub enum FindMsg {
    /// (row, column position) pairs.
    Matches(Vec<(RowKey, u32)>),
    Done {
        truncated: bool,
    },
}

pub fn spawn(
    input: StreamInput,
    encoding: Encoding,
    query: String,
    cancel: Arc<AtomicBool>,
    tx: Sender<FindMsg>,
    waker: Waker,
) {
    std::thread::Builder::new()
        .name("csv-find".into())
        .spawn(move || {
            let truncated = run(&input, encoding, &query, &cancel, &tx, &waker);
            if !cancel.load(Ordering::Relaxed) {
                let _ = tx.send(FindMsg::Done { truncated });
                waker.wake();
            }
        })
        .expect("spawn csv find");
}

/// Case-insensitive matching of one query against cell values.
pub struct Matcher {
    needle_lower: String,
    /// ASCII needle over UTF-8 bytes: fold ASCII case in place, SIMD search.
    ascii: bool,
    finder: memchr::memmem::Finder<'static>,
    encoding: Encoding,
    decoded: String,
}

impl Matcher {
    pub fn new(query: &str, encoding: Encoding) -> Self {
        let needle_lower = query.to_lowercase();
        Self {
            ascii: needle_lower.is_ascii() && encoding != Encoding::Windows1252,
            finder: memchr::memmem::Finder::new(needle_lower.as_bytes()).into_owned(),
            needle_lower,
            encoding,
            decoded: String::new(),
        }
    }

    /// Raw file bytes (the file's encoding).
    pub fn bytes(&mut self, field: &[u8]) -> bool {
        if self.ascii {
            field.len() >= self.needle_lower.len() && {
                let lowered: Vec<u8>;
                let hay = if field.iter().any(u8::is_ascii_uppercase) {
                    lowered = field.to_ascii_lowercase();
                    &lowered[..]
                } else {
                    field
                };
                self.finder.find(hay).is_some()
            }
        } else {
            self.decoded.clear();
            decode_into(field, self.encoding, &mut self.decoded);
            self.decoded.to_lowercase().contains(&self.needle_lower)
        }
    }

    /// Edited text.
    pub fn text(&self, s: &str) -> bool {
        !s.is_empty() && s.to_lowercase().contains(&self.needle_lower)
    }

    pub fn field(&mut self, f: Field<'_>) -> bool {
        match f {
            Field::File(b) => self.bytes(b),
            Field::Text(s) => self.text(s),
        }
    }
}

fn run(
    input: &StreamInput,
    encoding: Encoding,
    query: &str,
    cancel: &AtomicBool,
    tx: &Sender<FindMsg>,
    waker: &Waker,
) -> bool {
    let mut m = Matcher::new(query, encoding);
    let mut batch = Vec::new();
    let mut found = 0usize;
    let mut truncated = false;
    let mut last_send = Instant::now();
    let _ = stream::for_each(input, cancel, |row, _| {
        if row.key.is_header() {
            return ControlFlow::Continue(()); // the header isn't searched
        }
        let before = batch.len();
        match row.plain_fields() {
            // Unedited row: straight over the parsed fields.
            Some(fields) => {
                for (col, field) in fields.iter().enumerate() {
                    if m.bytes(field) {
                        batch.push((row.key, col as u32));
                    }
                }
            }
            None => {
                for col in 0..row.len() {
                    if m.field(row.field(col)) {
                        batch.push((row.key, col as u32));
                    }
                }
            }
        }
        found += batch.len() - before;
        if found >= MAX_MATCHES {
            truncated = true;
            return ControlFlow::Break(());
        }
        if !batch.is_empty()
            && (batch.len() >= 4096 || last_send.elapsed() > Duration::from_millis(100))
        {
            let _ = tx.send(FindMsg::Matches(std::mem::take(&mut batch)));
            waker.wake();
            last_send = Instant::now();
        }
        ControlFlow::Continue(())
    });
    if !batch.is_empty() {
        let _ = tx.send(FindMsg::Matches(batch));
        waker.wake();
    }
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::{ColKey, Edits};
    use crate::testutil::input_for;
    use std::sync::mpsc::channel;

    fn find_in(input: StreamInput, q: &str) -> Vec<(RowKey, u32)> {
        let (tx, rx) = channel();
        run(
            &input,
            Encoding::Utf8,
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

    fn find(data: &[u8], q: &str) -> Vec<(u64, u32)> {
        find_in(input_for(data, Default::default()), q)
            .into_iter()
            .map(|(k, c)| (k.as_orig().unwrap(), c))
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

    #[test]
    fn searches_edits_and_added_rows() {
        let data = b"name\nalpha\nbeta\n";
        let mut e = Edits::default();
        e.set_base_rows(2);
        let mut tx = e.tx("edit");
        let added = tx.insert_rows(0, 1, None);
        tx.set_cells([
            (added[0], ColKey::orig(0), Some("Alphabet".into())),
            (RowKey::orig(0), ColKey::orig(0), Some("gamma".into())),
        ]);
        let _ = tx.commit(None, None);
        let got = find_in(input_for(data, e.snapshot()), "alpha");
        assert_eq!(got, [(added[0], 0)]);
    }
}
