//! Quote-aware row boundary scanning: the hot loop behind indexing, find and sort.
//!
//! Rules (shared with the field parser in `parse.rs`, which must agree exactly):
//! - a `"` opens a quoted field only at the start of a field;
//! - inside quotes `""` is an escaped quote and a lone `"` closes the quotes;
//! - after the closing quote, bytes up to the next delimiter/terminator are
//!   part of the field (lenient, like Excel);
//! - any other `"` is an ordinary character.
//!
//! Chunks without any `"` take a SIMD fast path (`memchr` for terminators).

use crate::dialect::Dialect;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanError {
    /// A quoted field grew beyond [`MAX_QUOTED_FIELD`]: the file almost certainly
    /// has an unbalanced quote. The caller should rescan with quoting off.
    Unbalanced,
}

/// Longest quoted field we believe is real.
pub const MAX_QUOTED_FIELD: u64 = 8 << 20;

#[derive(Clone, Debug)]
pub struct RowScanner {
    dialect: Dialect,
    in_quotes: bool,
    /// Inside quotes, the previous chunk ended on a `"`: escape or close?
    pending_quote: bool,
    field_start: bool,
    quoted_bytes: u64,
}

impl RowScanner {
    pub fn new(dialect: Dialect) -> Self {
        Self {
            dialect,
            in_quotes: false,
            pending_quote: false,
            field_start: true,
            quoted_bytes: 0,
        }
    }

    /// Scan `chunk`, whose first byte is at absolute offset `base`. Calls
    /// `on_row_start(offset)` with the offset just past each row terminator.
    pub fn feed(
        &mut self,
        chunk: &[u8],
        base: u64,
        mut on_row_start: impl FnMut(u64),
    ) -> Result<(), ScanError> {
        let d = self.dialect;
        let (delim, term) = (d.delimiter, d.terminator);

        // Fast path: no quotes anywhere in play.
        if !self.in_quotes
            && !self.pending_quote
            && (!d.quoting || memchr::memchr(b'"', chunk).is_none())
        {
            for pos in memchr::memchr_iter(term, chunk) {
                on_row_start(base + pos as u64 + 1);
            }
            if let Some(&last) = chunk.last() {
                self.field_start = last == term || last == delim;
            }
            return Ok(());
        }

        let len = chunk.len();
        let mut i = 0;
        while i < len {
            if self.in_quotes {
                if self.pending_quote {
                    self.pending_quote = false;
                    if chunk[i] == b'"' {
                        i += 1; // escaped quote
                        continue;
                    }
                    self.in_quotes = false;
                    self.field_start = false;
                    continue; // re-examine this byte outside quotes
                }
                match memchr::memchr(b'"', &chunk[i..]) {
                    None => {
                        self.quoted_bytes += (len - i) as u64;
                        i = len;
                    }
                    Some(k) => {
                        self.quoted_bytes += k as u64;
                        let q = i + k;
                        if q + 1 < len {
                            if chunk[q + 1] == b'"' {
                                i = q + 2;
                            } else {
                                self.in_quotes = false;
                                self.field_start = false;
                                i = q + 1;
                            }
                        } else {
                            self.pending_quote = true;
                            i = len;
                        }
                    }
                }
                if self.quoted_bytes > MAX_QUOTED_FIELD {
                    return Err(ScanError::Unbalanced);
                }
                continue;
            }

            if self.field_start && chunk[i] == b'"' {
                self.in_quotes = true;
                self.quoted_bytes = 0;
                self.field_start = false;
                i += 1;
                continue;
            }
            match memchr::memchr3(delim, term, b'"', &chunk[i..]) {
                None => {
                    self.field_start = false;
                    i = len;
                }
                Some(k) => {
                    let p = i + k;
                    if k > 0 {
                        self.field_start = false;
                    }
                    let b = chunk[p];
                    if b == delim {
                        self.field_start = true;
                    } else if b == term {
                        self.field_start = true;
                        on_row_start(base + p as u64 + 1);
                    } else {
                        // A quote inside an unquoted field is literal. (A quote at
                        // field start with k == 0 was handled above.)
                        self.field_start = false;
                    }
                    i = p + 1;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(data: &[u8], chunk: usize, quoting: bool) -> Vec<u64> {
        let mut s = RowScanner::new(Dialect {
            quoting,
            ..Dialect::default()
        });
        let mut out = vec![];
        for (i, c) in data.chunks(chunk.max(1)).enumerate() {
            s.feed(c, (i * chunk) as u64, |o| out.push(o)).unwrap();
        }
        out
    }

    #[test]
    fn plain_rows() {
        assert_eq!(rows(b"a,b\n1,2\n3,4\n", 1024, true), [4, 8, 12]);
        assert_eq!(rows(b"a,b\r\n1,2\r\n", 1024, true), [5, 10]);
    }

    #[test]
    fn quoted_newlines_and_escapes_at_every_chunk_size() {
        let data = b"id,note\n1,\"multi\nline\"\n2,\"say \"\"hi\"\"\nthere\"\n3,x\"y\n4,\"\"\n";
        let expected = rows(data, 1 << 20, true);
        assert_eq!(expected.len(), 5);
        assert_eq!(expected[0], 8);
        for chunk in 1..data.len() {
            assert_eq!(rows(data, chunk, true), expected, "chunk size {chunk}");
        }
    }

    #[test]
    fn quote_mid_field_is_literal() {
        // 5"x is five inches, not an opening quote.
        assert_eq!(rows(b"a,5\"x\nb,c\n", 1024, true), [6, 10]);
    }

    #[test]
    fn quoting_off_treats_quotes_as_text() {
        assert_eq!(rows(b"a,\"b\nc\"\nd\n", 1024, false), [5, 8, 10]);
    }

    #[test]
    fn unbalanced_quote_is_detected() {
        let mut data = b"a,\"never closed\n".to_vec();
        data.extend(std::iter::repeat_n(b'x', (MAX_QUOTED_FIELD + 10) as usize));
        let mut s = RowScanner::new(Dialect::default());
        let r = s.feed(&data, 0, |_| {});
        assert_eq!(r, Err(ScanError::Unbalanced));
    }
}
