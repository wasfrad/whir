//! Streaming row/field parser over a [`ByteSource`], plus text decoding.
//! Row boundaries follow exactly the same rules as `scan.rs`.

use std::io;

use crate::dialect::{Dialect, Encoding};
use crate::source::ByteSource;

/// The fields of one row, unescaped, stored back to back.
#[derive(Default, Debug)]
pub struct FieldBuf {
    pub data: Vec<u8>,
    ends: Vec<usize>,
    /// Per field: it started with a quote. A save keeps the original quoting
    /// style of fields it rewrites.
    quoted: Vec<bool>,
    /// The row ended inside a quoted field (an unclosed quote at the end of the
    /// file): its raw bytes can't be followed by more rows.
    pub open_quote: bool,
}

impl FieldBuf {
    pub fn clear(&mut self) {
        self.data.clear();
        self.ends.clear();
        self.quoted.clear();
        self.open_quote = false;
    }

    fn end_field(&mut self, quoted: bool) {
        self.ends.push(self.data.len());
        self.quoted.push(quoted);
    }

    pub fn len(&self) -> usize {
        self.ends.len()
    }

    pub fn get(&self, i: usize) -> Option<&[u8]> {
        let end = *self.ends.get(i)?;
        let start = if i == 0 { 0 } else { self.ends[i - 1] };
        Some(&self.data[start..end])
    }

    /// Field `i` was written in quotes.
    pub fn quoted(&self, i: usize) -> bool {
        self.quoted.get(i).copied().unwrap_or(false)
    }

    pub fn iter(&self) -> impl Iterator<Item = &[u8]> {
        (0..self.len()).map(|i| self.get(i).unwrap())
    }
}

/// Parse one row from the start of `s`. Returns the bytes consumed (including
/// the terminator), or `None` if `s` ends mid-row and more data is coming.
pub fn parse_row(s: &[u8], d: &Dialect, eof: bool, out: &mut FieldBuf) -> Option<usize> {
    out.clear();
    if s.is_empty() {
        return None;
    }
    let (delim, term) = (d.delimiter, d.terminator);
    let mut i = 0;
    loop {
        // Quoted section at field start.
        let quoted = d.quoting && s.get(i) == Some(&b'"');
        if quoted {
            i += 1;
            loop {
                match memchr::memchr(b'"', &s[i..]) {
                    None => {
                        if !eof {
                            return None;
                        }
                        out.data.extend_from_slice(&s[i..]);
                        out.end_field(true);
                        out.open_quote = true;
                        return Some(s.len());
                    }
                    Some(k) => {
                        out.data.extend_from_slice(&s[i..i + k]);
                        i += k + 1;
                        match s.get(i) {
                            Some(b'"') => {
                                out.data.push(b'"');
                                i += 1;
                            }
                            None if !eof => return None, // can't tell escape from close yet
                            _ => break,
                        }
                    }
                }
            }
        }
        // Unquoted part (or the remainder after a closing quote).
        match memchr::memchr2(delim, term, &s[i..]) {
            None => {
                if !eof {
                    return None;
                }
                out.data.extend_from_slice(&s[i..]);
                strip_cr(out, term);
                out.end_field(quoted);
                return Some(s.len());
            }
            Some(k) => {
                out.data.extend_from_slice(&s[i..i + k]);
                i += k;
                if s[i] == delim {
                    out.end_field(quoted);
                    i += 1;
                } else {
                    strip_cr(out, term);
                    out.end_field(quoted);
                    return Some(i + 1);
                }
            }
        }
    }
}

/// CRLF files: the `\r` before `\n` belongs to the terminator, not the last field.
fn strip_cr(out: &mut FieldBuf, term: u8) {
    if term == b'\n' && out.data.last() == Some(&b'\r') {
        let start = out.ends.last().copied().unwrap_or(0);
        if out.data.len() > start {
            out.data.pop();
        }
    }
}

/// Sequential row reader starting at a row boundary.
pub struct RowReader<'a> {
    src: &'a dyn ByteSource,
    dialect: Dialect,
    /// File offset of `buf[0]`.
    base: u64,
    buf: Vec<u8>,
    start: usize,
    end: usize,
    eof: bool,
    limit: u64,
    /// `buf` range of the row returned last (always whole: a row only parses
    /// once all of it is in the buffer).
    last: (usize, usize),
}

const READ_CHUNK: usize = 256 << 10;

impl<'a> RowReader<'a> {
    pub fn new(src: &'a dyn ByteSource, dialect: Dialect, offset: u64) -> Self {
        Self::with_limit(src, dialect, offset, u64::MAX)
    }

    /// Stop at `limit` (e.g. the end of what the indexer has seen).
    pub fn with_limit(src: &'a dyn ByteSource, dialect: Dialect, offset: u64, limit: u64) -> Self {
        Self {
            src,
            dialect,
            base: offset,
            buf: Vec::new(),
            start: 0,
            end: 0,
            eof: false,
            limit: limit.min(src.len()),
            last: (0, 0),
        }
    }

    /// The raw bytes of the row returned by the last `next_row`, terminator
    /// included (none for a last row without one).
    pub fn last_raw(&self) -> &[u8] {
        &self.buf[self.last.0..self.last.1]
    }

    /// File offset of the next row.
    pub fn position(&self) -> u64 {
        self.base + self.start as u64
    }

    fn fill(&mut self) -> io::Result<()> {
        // Keep the unconsumed tail, grow if a single row doesn't fit.
        if self.start > 0 {
            self.buf.copy_within(self.start..self.end, 0);
            self.base += self.start as u64;
            self.end -= self.start;
            self.start = 0;
        }
        if self.buf.len() - self.end < READ_CHUNK / 2 {
            let new_len = (self.buf.len() * 2).max(READ_CHUNK);
            self.buf.resize(new_len, 0);
        }
        let file_pos = self.base + self.end as u64;
        let room = (self.buf.len() - self.end).min(self.limit.saturating_sub(file_pos) as usize);
        let n = if room == 0 {
            0
        } else {
            self.src
                .read_at(file_pos, &mut self.buf[self.end..self.end + room])?
        };
        if n == 0 {
            self.eof = true;
        }
        self.end += n;
        Ok(())
    }

    /// Read the next row into `fields`. Returns its file offset, or `None` at the end.
    pub fn next_row(&mut self, fields: &mut FieldBuf) -> io::Result<Option<u64>> {
        self.last = (0, 0);
        loop {
            let s = &self.buf[self.start..self.end];
            if s.is_empty() && self.eof {
                fields.clear();
                return Ok(None);
            }
            if let Some(n) = parse_row(s, &self.dialect, self.eof, fields) {
                let offset = self.position();
                self.last = (self.start, self.start + n);
                self.start += n;
                return Ok(Some(offset));
            }
            if self.eof {
                fields.clear();
                return Ok(None);
            }
            self.fill()?;
        }
    }

    /// Skip `n` rows (fewer at the end of the data). Returns how many were skipped.
    pub fn skip_rows(&mut self, n: u64, fields: &mut FieldBuf) -> io::Result<u64> {
        let mut done = 0;
        while done < n && self.next_row(fields)?.is_some() {
            done += 1;
        }
        Ok(done)
    }
}

/// Decode field bytes for display.
pub fn decode_into(bytes: &[u8], encoding: Encoding, out: &mut String) {
    match encoding {
        Encoding::Windows1252 => out.extend(bytes.iter().map(|&b| cp1252(b))),
        // UTF-16 sources are transcoded to UTF-8 before parsing.
        _ => match std::str::from_utf8(bytes) {
            Ok(s) => out.push_str(s),
            Err(_) => out.push_str(&String::from_utf8_lossy(bytes)),
        },
    }
}

const CP1252_HIGH: [char; 32] = [
    '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8d}', 'Ž', '\u{8f}',
    '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9d}', 'ž', 'Ÿ',
];

fn cp1252(b: u8) -> char {
    match b {
        0x80..=0x9F => CP1252_HIGH[(b - 0x80) as usize],
        _ => b as char,
    }
}

/// The Windows-1252 byte for `c`, if it has one.
pub fn cp1252_byte(c: char) -> Option<u8> {
    match c as u32 {
        0..=0x7F | 0xA0..=0xFF => Some(c as u8),
        _ => CP1252_HIGH
            .iter()
            .position(|&h| h == c)
            .map(|i| 0x80 + i as u8),
    }
}

/// Encode text as Windows-1252; characters it lacks become `?`.
pub fn encode_cp1252_lossy(s: &str, out: &mut Vec<u8>) {
    if s.is_ascii() {
        out.extend_from_slice(s.as_bytes());
    } else {
        out.extend(s.chars().map(|c| cp1252_byte(c).unwrap_or(b'?')));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::RowScanner;
    use crate::source::MemSource;

    fn all_rows(data: &[u8]) -> Vec<(u64, Vec<String>)> {
        // Tiny reads stress the refill path.
        let src = MemSource::with_max_read(data.to_vec(), 7);
        let mut r = RowReader::new(&src, Dialect::default(), 0);
        let mut f = FieldBuf::default();
        let mut out = vec![];
        while let Some(off) = r.next_row(&mut f).unwrap() {
            out.push((
                off,
                f.iter()
                    .map(|b| String::from_utf8_lossy(b).into_owned())
                    .collect(),
            ));
        }
        out
    }

    #[test]
    fn parses_quotes_escapes_crlf() {
        let rows = all_rows(b"a,b,c\r\n1,\"x,y\",\"he said \"\"hi\"\"\"\r\n2,\"multi\nline\",\r\n");
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].1, ["a", "b", "c"]);
        assert_eq!(rows[1].1, ["1", "x,y", "he said \"hi\""]);
        assert_eq!(rows[2].1, ["2", "multi\nline", ""]);
    }

    #[test]
    fn last_row_without_terminator_and_empty_fields() {
        let rows = all_rows(b"a,,c\n,\n1,2");
        assert_eq!(rows[0].1, ["a", "", "c"]);
        assert_eq!(rows[1].1, ["", ""]);
        assert_eq!(rows[2].1, ["1", "2"]);
    }

    #[test]
    fn parser_and_scanner_agree_on_row_starts() {
        let data = b"h1,h2\n\"a\nb\",c\n5\"x,y\n\"q\"\"q\",\"\"\n\"tail\"junk,z\nlast";
        let parsed: Vec<u64> = all_rows(data).iter().map(|r| r.0).collect();
        let mut scanned = vec![0u64];
        let mut s = RowScanner::new(Dialect::default());
        s.feed(data, 0, |o| {
            if (o as usize) < data.len() {
                scanned.push(o)
            }
        })
        .unwrap();
        assert_eq!(parsed, scanned);
    }

    #[test]
    fn windows_1252() {
        let mut s = String::new();
        decode_into(b"caf\xE9 \x80", Encoding::Windows1252, &mut s);
        assert_eq!(s, "café €");
        let mut b = Vec::new();
        encode_cp1252_lossy("café € 😀", &mut b);
        assert_eq!(b, b"caf\xE9 \x80 ?");
        for byte in 0u8..=255 {
            assert_eq!(cp1252_byte(cp1252(byte)), Some(byte));
        }
    }

    #[test]
    fn raw_rows_and_quoted_bits() {
        let data = b"a,\"b\"\r\n\"x\ny\",5\"z\n\"\"\n\"open";
        let src = MemSource::with_max_read(data.to_vec(), 3);
        let mut r = RowReader::new(&src, Dialect::default(), 0);
        let mut f = FieldBuf::default();
        let mut raws = Vec::new();
        let mut quoted = Vec::new();
        while r.next_row(&mut f).unwrap().is_some() {
            raws.push(r.last_raw().to_vec());
            quoted.push((0..f.len()).map(|i| f.quoted(i)).collect::<Vec<_>>());
            assert_eq!(f.open_quote, raws.len() == 4);
        }
        assert_eq!(
            raws,
            [
                &b"a,\"b\"\r\n"[..],
                b"\"x\ny\",5\"z\n",
                b"\"\"\n",
                b"\"open"
            ]
        );
        assert_eq!(raws.concat(), data);
        assert_eq!(
            quoted,
            [vec![false, true], vec![true, false], vec![true], vec![true]]
        );
    }
}
