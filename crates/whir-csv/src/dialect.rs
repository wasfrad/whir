//! Encoding and delimiter detection from a sample of the file.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf16Le,
    Utf16Be,
    /// Anything that isn't valid UTF-8: decoded as Windows-1252 (Excel's "CSV" on Western systems).
    Windows1252,
}

impl Encoding {
    pub fn label(self) -> &'static str {
        match self {
            Encoding::Utf8 => "UTF-8",
            Encoding::Utf16Le => "UTF-16 LE",
            Encoding::Utf16Be => "UTF-16 BE",
            Encoding::Windows1252 => "Windows-1252",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dialect {
    pub delimiter: u8,
    /// Row terminator: `\n` (LF and CRLF files) or `\r` (old Mac files).
    pub terminator: u8,
    /// Honour `"` quoting. Turned off if the file has unbalanced quotes.
    pub quoting: bool,
}

impl Default for Dialect {
    fn default() -> Self {
        Self {
            delimiter: b',',
            terminator: b'\n',
            quoting: true,
        }
    }
}

impl Dialect {
    pub fn delimiter_label(&self) -> &'static str {
        match self.delimiter {
            b',' => "Comma",
            b'\t' => "Tab",
            b';' => "Semicolon",
            b'|' => "Pipe",
            _ => "Custom",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Sniffed {
    pub encoding: Encoding,
    /// Bytes of byte-order mark to skip.
    pub bom_len: u64,
    pub dialect: Dialect,
}

/// Inspect the first bytes of a file (64 KB is plenty).
pub fn sniff(sample: &[u8], extension: Option<&str>) -> Sniffed {
    let (encoding, bom_len) = detect_encoding(sample);
    // Delimiter sniffing works on bytes; for UTF-16 the caller sniffs again after transcoding.
    let body = &sample[bom_len as usize..];
    let terminator =
        if memchr::memchr(b'\n', body).is_none() && memchr::memchr(b'\r', body).is_some() {
            b'\r'
        } else {
            b'\n'
        };
    let ext = extension.map(|e| e.to_ascii_lowercase());
    let delimiter = match ext.as_deref() {
        Some("tsv" | "tab") => b'\t',
        _ => detect_delimiter(body, terminator),
    };
    Sniffed {
        encoding,
        bom_len,
        dialect: Dialect {
            delimiter,
            terminator,
            quoting: true,
        },
    }
}

fn detect_encoding(s: &[u8]) -> (Encoding, u64) {
    if s.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return (Encoding::Utf8, 3);
    }
    if s.starts_with(&[0xFF, 0xFE]) {
        return (Encoding::Utf16Le, 2);
    }
    if s.starts_with(&[0xFE, 0xFF]) {
        return (Encoding::Utf16Be, 2);
    }
    // BOM-less UTF-16: lots of NULs in alternating positions.
    if s.len() >= 16 {
        let n = s.len().min(4096) & !1;
        let odd_zero = s[..n]
            .iter()
            .skip(1)
            .step_by(2)
            .filter(|&&b| b == 0)
            .count();
        let even_zero = s[..n].iter().step_by(2).filter(|&&b| b == 0).count();
        let half = n / 2;
        if odd_zero * 10 > half * 7 && even_zero * 10 < half {
            return (Encoding::Utf16Le, 0);
        }
        if even_zero * 10 > half * 7 && odd_zero * 10 < half {
            return (Encoding::Utf16Be, 0);
        }
    }
    match std::str::from_utf8(s) {
        Ok(_) => (Encoding::Utf8, 0),
        // A multi-byte sequence cut off by the end of the sample is still UTF-8.
        Err(e) if e.error_len().is_none() => (Encoding::Utf8, 0),
        Err(_) => (Encoding::Windows1252, 0),
    }
}

/// Pick the delimiter that splits the first lines into the most consistent
/// number of fields (ignoring delimiters inside quotes).
fn detect_delimiter(body: &[u8], terminator: u8) -> u8 {
    let mut best = (b',', 0.0f64);
    for &d in b",\t;|" {
        let counts = field_counts(body, d, terminator, 64);
        if counts.is_empty() {
            continue;
        }
        // Most common field count and how many lines share it.
        let mut freq = std::collections::HashMap::new();
        for &c in &counts {
            *freq.entry(c).or_insert(0usize) += 1;
        }
        let (&mode, &n) = freq.iter().max_by_key(|(c, n)| (**n, **c)).unwrap();
        if mode < 2 {
            continue;
        }
        let consistency = n as f64 / counts.len() as f64;
        let score = consistency * 100.0 + (mode as f64).min(50.0);
        if score > best.1 {
            best = (d, score);
        }
    }
    best.0
}

fn field_counts(body: &[u8], delim: u8, term: u8, max_lines: usize) -> Vec<usize> {
    let mut out = Vec::new();
    let mut fields = 1;
    let mut in_quotes = false;
    let mut field_start = true;
    let mut i = 0;
    // Drop the last (possibly truncated) line of the sample.
    let end = memchr::memrchr(term, body).map_or(body.len(), |p| p + 1);
    while i < end && out.len() < max_lines {
        let b = body[i];
        if in_quotes {
            if b == b'"' {
                if body.get(i + 1) == Some(&b'"') {
                    i += 1;
                } else {
                    in_quotes = false;
                }
            }
        } else if b == b'"' && field_start {
            in_quotes = true;
            field_start = false;
        } else if b == delim {
            fields += 1;
            field_start = true;
            i += 1;
            continue;
        } else if b == term {
            out.push(fields);
            fields = 1;
            field_start = true;
            i += 1;
            continue;
        } else {
            field_start = false;
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delimiters() {
        assert_eq!(
            sniff(b"a,b,c\n1,2,3\n4,5,6\n", None).dialect.delimiter,
            b','
        );
        assert_eq!(
            sniff(b"a;b;c\n1,5;2;3\n4;5;6\n", None).dialect.delimiter,
            b';'
        );
        assert_eq!(sniff(b"a\tb\tc\n1\t2\t3\n", None).dialect.delimiter, b'\t');
        assert_eq!(sniff(b"a|b\n1|2\n", None).dialect.delimiter, b'|');
        // Commas inside quotes don't count.
        assert_eq!(
            sniff(b"name;note\n\"x\";\"a,b,c,d\"\n\"y\";\"e,f\"\n", None)
                .dialect
                .delimiter,
            b';'
        );
        assert_eq!(sniff(b"a,b\n1,2\n", Some("TSV")).dialect.delimiter, b'\t');
    }

    #[test]
    fn encodings() {
        assert_eq!(sniff(b"\xEF\xBB\xBFa,b\n", None).encoding, Encoding::Utf8);
        assert_eq!(sniff(b"\xEF\xBB\xBFa,b\n", None).bom_len, 3);
        assert_eq!(
            sniff(b"\xFF\xFEa\0,\0b\0", None).encoding,
            Encoding::Utf16Le
        );
        assert_eq!(
            sniff("caf\u{e9},x\n".as_bytes(), None).encoding,
            Encoding::Utf8
        );
        assert_eq!(sniff(b"caf\xE9,x\n", None).encoding, Encoding::Windows1252);
        let le: Vec<u8> = "name,value\n1,2\n3,4\n"
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        assert_eq!(sniff(&le, None).encoding, Encoding::Utf16Le);
    }

    #[test]
    fn terminators() {
        assert_eq!(sniff(b"a,b\r\n1,2\r\n", None).dialect.terminator, b'\n');
        assert_eq!(sniff(b"a,b\r1,2\r", None).dialect.terminator, b'\r');
    }
}
