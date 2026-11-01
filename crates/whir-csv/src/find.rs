//! Background search across the whole table, streaming matches as found.
//! Matches name rows by key, so they stay valid while rows are inserted or
//! deleted; edited cells are searched as they are now.
//!
//! Most rows can't match. While the table's rows and columns are the file's
//! own, the search looks for the query in the raw bytes of a large block
//! first and parses only the rows it occurs in (plus rows with edited cells):
//! the cost is a memchr-speed pass, not a parse of every field.

use std::io;
use std::ops::ControlFlow;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use memchr::memmem::Finder;
use whir_core::{FindQuery, Waker};

use crate::dialect::Encoding;
use crate::edit::{MAX_EDIT_ROWS, RowKey};
use crate::parse::{FieldBuf, decode_into, parse_row};
use crate::scan::RowScanner;
use crate::stream::{self, Field, LogicalRow, StreamInput};

pub const MAX_MATCHES: usize = 1_000_000;
/// Bytes the fast path reads at a time.
const BLOCK: usize = 1 << 20;
/// Rows between progress updates on the slow path.
const PROGRESS_EVERY: u64 = 4096;

pub enum FindMsg {
    /// (row, column position) pairs.
    Matches(Vec<(RowKey, u32)>),
    Done {
        truncated: bool,
    },
}

/// `progress` receives the source bytes searched so far.
pub fn spawn(
    input: StreamInput,
    encoding: Encoding,
    query: FindQuery,
    cancel: Arc<AtomicBool>,
    progress: Arc<AtomicU64>,
    tx: Sender<FindMsg>,
    waker: Waker,
) {
    std::thread::Builder::new()
        .name("csv-find".into())
        .spawn(move || {
            let truncated = run(
                &input, encoding, &query, &cancel, &progress, &tx, &waker, BLOCK,
            );
            if !cancel.load(Ordering::Relaxed) {
                let _ = tx.send(FindMsg::Done { truncated });
                waker.wake();
            }
        })
        .expect("spawn csv find");
}

/// Case-insensitive matching of one query against cell values.
pub struct Matcher {
    whole_cell: bool,
    /// The query lowercased (and trimmed, for whole cells).
    needle: String,
    /// The query as a number (whole cells only).
    number: Option<Number>,
    /// ASCII needle over UTF-8 bytes: fold ASCII case in place, SIMD search.
    ascii: bool,
    finder: Finder<'static>,
    encoding: Encoding,
    decoded: String,
    lowered: Vec<u8>,
}

/// A decimal number, normalized: no leading zeros before the point, no
/// trailing zeros after it, and zero is never negative.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Number {
    neg: bool,
    int: Vec<u8>,
    frac: Vec<u8>,
}

/// `[+|-][$]digits[.digits]` (sign and `$` in either order), surrounding
/// spaces allowed: `(negative, integer digits, fraction digits)`, normalized.
fn number_parts(b: &[u8]) -> Option<(bool, &[u8], &[u8])> {
    let b = b.trim_ascii();
    let mut i = 0;
    let (mut neg, mut sign, mut dollar) = (false, false, false);
    loop {
        match b.get(i) {
            Some(&c @ (b'-' | b'+')) if !sign => {
                neg = c == b'-';
                sign = true;
            }
            Some(b'$') if !dollar => dollar = true,
            _ => break,
        }
        i += 1;
    }
    let digits = |from: usize| from + b[from..].iter().take_while(|c| c.is_ascii_digit()).count();
    let int_end = digits(i);
    let mut int = &b[i..int_end];
    let mut frac: &[u8] = &[];
    let mut end = int_end;
    if b.get(end) == Some(&b'.') {
        let frac_end = digits(end + 1);
        frac = &b[end + 1..frac_end];
        end = frac_end;
    }
    if end != b.len() || (int.is_empty() && frac.is_empty()) {
        return None;
    }
    while let [b'0', rest @ ..] = int {
        int = rest;
    }
    while let [rest @ .., b'0'] = frac {
        frac = rest;
    }
    if int.is_empty() && frac.is_empty() {
        neg = false;
    }
    Some((neg, int, frac))
}

impl Number {
    fn parse(s: &str) -> Option<Self> {
        number_parts(s.as_bytes()).map(|(neg, int, frac)| Self {
            neg,
            int: int.to_vec(),
            frac: frac.to_vec(),
        })
    }

    fn equals(&self, b: &[u8]) -> bool {
        number_parts(b).is_some_and(|(neg, int, frac)| {
            neg == self.neg && int == self.int.as_slice() && frac == self.frac.as_slice()
        })
    }

    /// Bytes every way of writing this number contains ("2.45" in "$02.4500").
    fn core(&self) -> Vec<u8> {
        if self.int.is_empty() && self.frac.is_empty() {
            return b"0".to_vec();
        }
        let mut v = self.int.clone();
        if !self.frac.is_empty() {
            v.push(b'.');
            v.extend_from_slice(&self.frac);
        }
        v
    }
}

/// A byte string that the raw file bytes of every matching row contain.
pub struct Prefilter {
    finder: Finder<'static>,
    /// Search ASCII-lowercased bytes (the needle has letters).
    fold: bool,
}

impl Matcher {
    pub fn new(query: &FindQuery, encoding: Encoding) -> Self {
        let whole_cell = query.whole_cell;
        let needle_lower = if whole_cell {
            query.text.trim().to_lowercase()
        } else {
            query.text.to_lowercase()
        };
        Self {
            whole_cell,
            number: whole_cell.then(|| Number::parse(&query.text)).flatten(),
            ascii: needle_lower.is_ascii() && encoding != Encoding::Windows1252,
            finder: Finder::new(needle_lower.as_bytes()).into_owned(),
            needle: needle_lower,
            encoding,
            decoded: String::new(),
            lowered: Vec::new(),
        }
    }

    /// Nothing can match (a whole-cell query of only spaces).
    pub fn matches_nothing(&self) -> bool {
        self.whole_cell && self.needle.is_empty()
    }

    /// What to look for in raw row bytes before parsing them, if anything
    /// safely can be: an ASCII needle (non-ASCII case folding isn't byte-wise),
    /// without quotes when quoting is on (they're doubled in the file).
    pub fn prefilter(&self, quoting: bool) -> Option<Prefilter> {
        if !self.needle.is_ascii() {
            return None;
        }
        let bytes = match &self.number {
            Some(n) => n.core(),
            None => self.needle.as_bytes().to_vec(),
        };
        let bytes = if quoting {
            bytes
                .split(|&b| b == b'"')
                .max_by_key(|s| s.len())
                .unwrap_or_default()
                .to_vec()
        } else {
            bytes
        };
        if bytes.is_empty() {
            return None;
        }
        Some(Prefilter {
            fold: bytes.iter().any(u8::is_ascii_alphabetic),
            finder: Finder::new(&bytes).into_owned(),
        })
    }

    /// Raw file bytes (the file's encoding).
    pub fn bytes(&mut self, field: &[u8]) -> bool {
        if self.whole_cell {
            if self.number.as_ref().is_some_and(|n| n.equals(field)) {
                return true;
            }
            if self.ascii {
                return field
                    .trim_ascii()
                    .eq_ignore_ascii_case(self.needle.as_bytes());
            }
            self.decoded.clear();
            decode_into(field, self.encoding, &mut self.decoded);
            return !self.needle.is_empty() && self.decoded.trim().to_lowercase() == self.needle;
        }
        if self.ascii {
            if field.len() < self.needle.len() {
                return false;
            }
            let hay = if field.iter().any(u8::is_ascii_uppercase) {
                self.lowered.clear();
                self.lowered.extend_from_slice(field);
                self.lowered.make_ascii_lowercase();
                &self.lowered[..]
            } else {
                field
            };
            self.finder.find(hay).is_some()
        } else {
            self.decoded.clear();
            decode_into(field, self.encoding, &mut self.decoded);
            self.decoded.to_lowercase().contains(&self.needle)
        }
    }

    /// Edited text.
    pub fn text(&self, s: &str) -> bool {
        if self.whole_cell {
            return self.number.as_ref().is_some_and(|n| n.equals(s.as_bytes()))
                || (!self.needle.is_empty() && s.trim().to_lowercase() == self.needle);
        }
        !s.is_empty() && s.to_lowercase().contains(&self.needle)
    }

    pub fn field(&mut self, f: Field<'_>) -> bool {
        match f {
            Field::File(b) => self.bytes(b),
            Field::Text(s) => self.text(s),
        }
    }
}

/// Matches on their way to the document, in batches.
struct Sink<'a> {
    tx: &'a Sender<FindMsg>,
    waker: &'a Waker,
    batch: Vec<(RowKey, u32)>,
    sent: usize,
    last_send: Instant,
}

impl Sink<'_> {
    /// Test one row, queueing its matches. False once the cap is reached.
    fn row(&mut self, row: &LogicalRow<'_>, m: &mut Matcher) -> bool {
        match row.plain_fields() {
            // Unedited row: straight over the parsed fields.
            Some(fields) => {
                for (col, field) in fields.iter().enumerate() {
                    if m.bytes(field) {
                        self.batch.push((row.key, col as u32));
                    }
                }
            }
            None => {
                for col in 0..row.len() {
                    if m.field(row.field(col)) {
                        self.batch.push((row.key, col as u32));
                    }
                }
            }
        }
        if self.sent + self.batch.len() >= MAX_MATCHES {
            return false;
        }
        if !self.batch.is_empty()
            && (self.batch.len() >= 4096 || self.last_send.elapsed() > Duration::from_millis(100))
        {
            self.flush();
        }
        true
    }

    fn flush(&mut self) {
        if !self.batch.is_empty() {
            self.sent += self.batch.len();
            let _ = self
                .tx
                .send(FindMsg::Matches(std::mem::take(&mut self.batch)));
            self.waker.wake();
            self.last_send = Instant::now();
        }
    }
}

/// Search, returning whether the results are incomplete (capped, a read
/// error, or more rows than keys can name). `block`: the fast path's read size.
#[allow(clippy::too_many_arguments)]
fn run(
    input: &StreamInput,
    encoding: Encoding,
    query: &FindQuery,
    cancel: &AtomicBool,
    progress: &AtomicU64,
    tx: &Sender<FindMsg>,
    waker: &Waker,
    block: usize,
) -> bool {
    let mut m = Matcher::new(query, encoding);
    if m.matches_nothing() {
        return false;
    }
    let mut sink = Sink {
        tx,
        waker,
        batch: Vec::new(),
        sent: 0,
        last_send: Instant::now(),
    };
    // The fast path needs the file's own rows and columns.
    let own_layout = input.edits.rows.is_none() && input.edits.cols.is_none();
    let result = match m.prefilter(input.dialect.quoting) {
        Some(pre) if own_layout => {
            run_fast(input, &mut m, &pre, cancel, progress, &mut sink, block)
        }
        _ => run_slow(input, &mut m, cancel, progress, &mut sink),
    };
    sink.flush();
    !matches!(result, Ok(true)) && !cancel.load(Ordering::Relaxed)
}

/// Every row, parsed (edited row order or columns, or a needle that can't be
/// looked for in raw bytes). `Ok(false)`: stopped early.
fn run_slow(
    input: &StreamInput,
    m: &mut Matcher,
    cancel: &AtomicBool,
    progress: &AtomicU64,
    sink: &mut Sink<'_>,
) -> io::Result<bool> {
    let mut n = 0u64;
    stream::for_each(input, cancel, |row, pos| {
        n += 1;
        if n.is_multiple_of(PROGRESS_EVERY) {
            progress.store(pos, Ordering::Relaxed);
        }
        if row.key.is_header() {
            return ControlFlow::Continue(()); // the header isn't searched
        }
        if sink.row(row, m) {
            ControlFlow::Continue(())
        } else {
            ControlFlow::Break(())
        }
    })
}

/// Blocks of whole rows: find the prefilter in the raw bytes, parse only the
/// rows it's in and the rows with edited cells (their text isn't in the file).
fn run_fast(
    input: &StreamInput,
    m: &mut Matcher,
    pre: &Prefilter,
    cancel: &AtomicBool,
    progress: &AtomicU64,
    sink: &mut Sink<'_>,
    block: usize,
) -> io::Result<bool> {
    let src = &*input.source;
    let dialect = input.dialect;
    let cells = &*input.edits.cells;
    let mut edited = cells.range(..RowKey::HEADER).map(|(k, _)| *k).peekable();
    let mut scanner = RowScanner::new(dialect);
    let mut buf = vec![0u8; block.max(64)];
    let mut folded = Vec::new();
    let mut fields = FieldBuf::default();
    // File offset of `buf[0]`; bytes read into `buf`; bytes of `buf` done.
    let mut base = input.header_offset;
    let mut filled = 0usize;
    let mut done = 0usize;
    // The file row starting at `done` (0 = the header).
    let mut next_row = 0u64;
    // Row ends (positions in `buf`) found past `done`.
    let mut ends: Vec<usize> = Vec::new();
    let mut candidates: Vec<usize> = Vec::new();
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(false);
        }
        // Drop what's done; grow for a row longer than the buffer.
        if done > 0 {
            buf.copy_within(done..filled, 0);
            ends.iter_mut().for_each(|e| *e -= done);
            base += done as u64;
            filled -= done;
            done = 0;
        }
        if filled == buf.len() {
            buf.resize(buf.len() * 2, 0);
        }
        let n = src.read_at(base + filled as u64, &mut buf[filled..])?;
        let eof = n == 0;
        if n > 0 {
            let at = base + filled as u64;
            scanner
                .feed(&buf[filled..filled + n], at, |o| {
                    ends.push((o - base) as usize)
                })
                .map_err(|_| io::Error::other("unbalanced quotes"))?;
            filled += n;
        }
        // The last row of the file may have no terminator.
        if eof && filled > ends.last().copied().unwrap_or(0) {
            ends.push(filled);
        }
        if let Some(&end) = ends.last() {
            let rows = ends.len() as u64;
            if next_row + rows > MAX_EDIT_ROWS + 1 {
                return Err(io::Error::other(stream::TOO_MANY_ROWS));
            }
            // Rows holding the needle...
            candidates.clear();
            let hay = &buf[..end];
            let hay = if pre.fold {
                folded.clear();
                folded.extend_from_slice(hay);
                folded.make_ascii_lowercase();
                &folded[..]
            } else {
                hay
            };
            let mut pos = 0;
            while let Some(p) = pre.finder.find(&hay[pos..end]) {
                let k = ends.partition_point(|&e| e <= pos + p);
                candidates.push(k);
                pos = ends[k];
                if pos >= end {
                    break;
                }
            }
            // ...and rows with edited cells.
            while let Some(r) = edited.peek().and_then(|k| k.as_orig()) {
                let file_row = r + 1;
                if file_row >= next_row + rows {
                    break;
                }
                edited.next();
                if file_row >= next_row {
                    candidates.push((file_row - next_row) as usize);
                }
            }
            candidates.sort_unstable();
            candidates.dedup();
            for &k in &candidates {
                let file_row = next_row + k as u64;
                if file_row == 0 {
                    continue; // the header isn't searched
                }
                let start = if k == 0 { 0 } else { ends[k - 1] };
                let raw = &buf[start..ends[k]];
                parse_row(raw, &dialect, true, &mut fields);
                let key = RowKey::orig(file_row - 1);
                let row = LogicalRow::file_row(key, &fields, raw, cells.row(key));
                if !sink.row(&row, m) {
                    return Ok(false);
                }
            }
            next_row += rows;
            done = end;
            ends.clear();
        }
        progress.store(base + done as u64, Ordering::Relaxed);
        if eof {
            return Ok(true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::{ColKey, Edits};
    use crate::source::MemSource;
    use crate::testutil::{Rng, input_for};
    use std::sync::mpsc::channel;

    /// Matches and the truncated flag; `fast: false` forces the slow path.
    fn search(input: &StreamInput, q: &FindQuery, fast: bool, block: usize) -> Vec<(RowKey, u32)> {
        let (tx, rx) = channel();
        let mut m = Matcher::new(q, Encoding::Utf8);
        let mut sink = Sink {
            tx: &tx,
            waker: &Waker::noop(),
            batch: Vec::new(),
            sent: 0,
            last_send: Instant::now(),
        };
        let (cancel, progress) = (AtomicBool::new(false), AtomicU64::new(0));
        if !m.matches_nothing() {
            let pre = m.prefilter(input.dialect.quoting);
            let r = match pre {
                Some(pre) if fast => {
                    run_fast(input, &mut m, &pre, &cancel, &progress, &mut sink, block)
                }
                _ => run_slow(input, &mut m, &cancel, &progress, &mut sink),
            };
            assert!(r.unwrap());
        }
        sink.flush();
        drop(tx);
        rx.iter()
            .flat_map(|m| match m {
                FindMsg::Matches(v) => v,
                FindMsg::Done { .. } => vec![],
            })
            .collect()
    }

    fn find_in(input: StreamInput, q: &str) -> Vec<(RowKey, u32)> {
        let (tx, rx) = channel();
        run(
            &input,
            Encoding::Utf8,
            &FindQuery::new(q),
            &AtomicBool::new(false),
            &AtomicU64::new(0),
            &tx,
            &Waker::noop(),
            BLOCK,
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

    fn whole(q: &str) -> FindQuery {
        FindQuery {
            text: q.into(),
            whole_cell: true,
        }
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

    #[test]
    fn numbers() {
        fn n(s: &str) -> Option<(bool, &[u8], &[u8])> {
            number_parts(s.as_bytes())
        }
        assert_eq!(n("2.4500"), Some((false, &b"2"[..], &b"45"[..])));
        assert_eq!(n(" $002.45 "), Some((false, &b"2"[..], &b"45"[..])));
        assert_eq!(n("-$0.50"), Some((true, &b""[..], &b"5"[..])));
        assert_eq!(n("$-.5"), Some((true, &b""[..], &b"5"[..])));
        assert_eq!(n("-0.00"), Some((false, &b""[..], &b""[..])));
        assert_eq!(n("12."), Some((false, &b"12"[..], &b""[..])));
        for bad in [
            "", ".", "-", "$", "2.4.5", "1,234", "2.45x", "--2", "$$2", "1e5",
        ] {
            assert_eq!(n(bad), None, "{bad}");
        }
    }

    #[test]
    fn whole_cells_and_numbers_by_value() {
        let data = b"price,name\n2.45,Citgo\n2.4500,citgo inc\n12.45,\" CITGO \"\n$2.45,x\n2.451,y\n\"2.45\",-2.45\n";
        let got = |q: FindQuery| -> Vec<(u64, u32)> {
            search(&input_for(data, Default::default()), &q, true, BLOCK)
                .into_iter()
                .map(|(k, c)| (k.as_orig().unwrap(), c))
                .collect()
        };
        assert_eq!(got(whole("2.45")), [(0, 0), (1, 0), (3, 0), (5, 0)]);
        assert_eq!(got(whole("2.450")), [(0, 0), (1, 0), (3, 0), (5, 0)]);
        assert_eq!(got(whole("-2.45")), [(5, 1)]);
        assert_eq!(got(whole("citgo")), [(0, 1), (2, 1)]);
        assert_eq!(got(whole("  ")), []);
        // Substring mode is unchanged.
        assert_eq!(got(FindQuery::new("2.45")).len(), 7);
    }

    #[test]
    fn whole_cell_edited_text() {
        let m = Matcher::new(&whole("2.45"), Encoding::Utf8);
        assert!(m.text("2.4500"));
        assert!(m.text(" 2.45"));
        assert!(!m.text("12.45"));
        let m = Matcher::new(&whole("Citgo"), Encoding::Utf8);
        assert!(m.text(" CITGO "));
        assert!(!m.text("Citgo Inc"));
    }

    #[test]
    fn prefilter_needles() {
        let pre = |q: FindQuery, quoting: bool| {
            Matcher::new(&q, Encoding::Utf8)
                .prefilter(quoting)
                .map(|p| {
                    (
                        String::from_utf8(p.finder.needle().to_vec()).unwrap(),
                        p.fold,
                    )
                })
        };
        assert_eq!(
            pre(FindQuery::new("Citgo"), true),
            Some(("citgo".into(), true))
        );
        assert_eq!(
            pre(FindQuery::new("2.45"), true),
            Some(("2.45".into(), false))
        );
        assert_eq!(pre(whole("$02.4500"), true), Some(("2.45".into(), false)));
        assert_eq!(pre(whole("0.0"), true), Some(("0".into(), false)));
        // Quotes are doubled inside quoted fields: look for the longest part without one.
        assert_eq!(
            pre(FindQuery::new("say \"hello\""), true),
            Some(("hello".into(), true))
        );
        assert_eq!(
            pre(FindQuery::new("say \"hello\""), false),
            Some(("say \"hello\"".into(), true))
        );
        assert_eq!(pre(FindQuery::new("\""), true), None);
        assert_eq!(pre(FindQuery::new("école"), true), None);
    }

    #[test]
    fn quote_in_query_matches_escaped_quotes() {
        let data = b"a\n\"say \"\"hello\"\"\"\nhello\n";
        assert_eq!(find(data, "\"hello\""), [(0, 0)]);
    }

    /// The fast path finds exactly what parsing every row finds: random
    /// tables with quotes, embedded newlines, CRLF, ragged rows and edits,
    /// read in tiny blocks and tiny reads.
    #[test]
    fn fast_path_agrees_with_parsing_everything() {
        let words = [
            "2.45",
            "2.4500",
            "$2.45",
            "12.45",
            "Citgo",
            "CITGO",
            "cit\"go",
            "x",
            "",
            "go",
            " 2.45 ",
            "a,b",
            "line\nbreak",
            "\r",
            "0",
            "0.0",
        ];
        let queries = [
            FindQuery::new("2.45"),
            FindQuery::new("citgo"),
            FindQuery::new("go"),
            FindQuery::new("\"go"),
            FindQuery::new("a,b"),
            FindQuery::new("0"),
            whole("2.45"),
            whole("citgo"),
            whole("0"),
            whole("x"),
        ];
        let mut rng = Rng::new(42);
        for round in 0..60 {
            let crlf = round % 2 == 0;
            let nl = if crlf { "\r\n" } else { "\n" };
            let mut data = String::from("h1,h2,h3");
            data.push_str(nl);
            let rows = 1 + rng.below(40);
            for _ in 0..rows {
                let cols = 1 + rng.below(4);
                for c in 0..cols {
                    if c > 0 {
                        data.push(',');
                    }
                    let w = words[rng.below(words.len() as u64) as usize];
                    if w.contains([',', '"', '\n', '\r']) || rng.below(4) == 0 {
                        data.push('"');
                        data.push_str(&w.replace('"', "\"\""));
                        data.push('"');
                    } else {
                        data.push_str(w);
                    }
                }
                data.push_str(nl);
            }
            if rng.below(3) == 0 {
                // No final newline.
                data.truncate(data.len() - nl.len());
            }
            let base = input_for(data.as_bytes(), Default::default());
            let data_rows = base.index.read().unwrap().rows.saturating_sub(1);
            // Some edited cells.
            let mut e = Edits::default();
            e.set_base_rows(data_rows);
            if data_rows > 0 {
                let mut tx = e.tx("edit");
                let r = rng.below(data_rows);
                let w = words[rng.below(words.len() as u64) as usize];
                tx.set_cells([(
                    RowKey::orig(r),
                    ColKey::orig(rng.below(5) as usize),
                    Some(w.into()),
                )]);
                let _ = tx.commit(None, None);
            }
            let mut input = base.clone();
            input.edits = e.snapshot();
            // Tiny reads stress the refill paths.
            input.source = Arc::new(MemSource::with_max_read(data.as_bytes().to_vec(), 7));
            for q in &queries {
                let slow = search(&input, q, false, BLOCK);
                for block in [64, 100, 4096] {
                    let fast = search(&input, q, true, block);
                    assert_eq!(
                        fast, slow,
                        "round {round} query {q:?} block {block}\n{data:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn progress_reaches_the_end() {
        let data = b"a,b\n1,2\n3,4\n";
        let input = input_for(data, Default::default());
        let (tx, _rx) = channel();
        let progress = AtomicU64::new(0);
        run(
            &input,
            Encoding::Utf8,
            &FindQuery::new("3"),
            &AtomicBool::new(false),
            &progress,
            &tx,
            &Waker::noop(),
            BLOCK,
        );
        assert_eq!(progress.load(Ordering::Relaxed), input.source.len());
    }

    /// Fast path vs parsing every row, over a file of fuel prices (or the
    /// file `WHIR_BENCH_CSV` names).
    /// `cargo test -p whir-csv --release bench_find -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn bench_find() {
        use crate::dialect::{Dialect, sniff};
        use std::sync::RwLock;
        let path = match std::env::var_os("WHIR_BENCH_CSV") {
            Some(p) => std::path::PathBuf::from(p),
            None => {
                let path = crate::testutil::test_dir("bench-find").join("rack.csv");
                let mut data = b"Date,Terminal,Supplier,Product,Price,Change\r\n".to_vec();
                let mut rng = Rng::new(7);
                for i in 0..2_000_000u64 {
                    let p = 20_000 + rng.below(20_000);
                    data.extend(
                        format!(
                            "2026-09-{:02},Terminal {},Supplier {},ULSD #2,{}.{:04},-0.{:04}\r\n",
                            1 + i % 28,
                            rng.below(300),
                            rng.below(40),
                            p / 10_000,
                            p % 10_000,
                            rng.below(300)
                        )
                        .as_bytes(),
                    );
                }
                std::fs::write(&path, &data).unwrap();
                path
            }
        };
        let src = whir_core::FileSource::open(&path).unwrap();
        let mut head = vec![0u8; 64 << 10];
        let n = crate::source::ByteSource::read_at(&*src, 0, &mut head).unwrap();
        let s = sniff(&head[..n], Some("csv"));
        let index = crate::index::build_sync(&*src, s.bom_len, s.dialect);
        let mb = crate::source::ByteSource::len(&*src) as f64 / 1e6;
        let input = StreamInput {
            source: src,
            dialect: Dialect {
                quoting: !index.quoting_disabled,
                ..s.dialect
            },
            header_offset: s.bom_len,
            index: Arc::new(RwLock::new(index)),
            edits: Default::default(),
        };
        for q in [
            FindQuery::new("3.1416"),
            whole("3.1416"),
            FindQuery::new("supplier 7"),
        ] {
            for fast in [false, true] {
                let t = Instant::now();
                let hits = search(&input, &q, fast, BLOCK).len();
                let secs = t.elapsed().as_secs_f64();
                println!(
                    "{:>6} {q:?}: {hits} matches in {:.0} ms ({:.0} MB/s)",
                    if fast { "fast" } else { "parse" },
                    secs * 1000.0,
                    mb / secs
                );
            }
        }
    }
}
