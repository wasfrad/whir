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

/// How rows are written, so rewritten rows match the rest of the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Style {
    /// `\r\n`, `\n` or `\r`.
    pub line_ending: &'static [u8],
    /// The file ends with a line ending.
    pub final_newline: bool,
    /// (Nearly) every field is quoted: new values are quoted too.
    pub quote_all: bool,
}

impl Default for Style {
    /// Excel's CSV style: CRLF, final newline, quotes only where needed.
    fn default() -> Self {
        Self {
            line_ending: b"\r\n",
            final_newline: true,
            quote_all: false,
        }
    }
}

/// Detect the writing style from `sample` (the first bytes after the BOM;
/// `whole` if it is the entire file) and the file's last byte.
pub fn sniff_style(sample: &[u8], whole: bool, last_byte: Option<u8>, d: &Dialect) -> Style {
    let line_ending: &'static [u8] = if d.terminator == b'\r' {
        b"\r"
    } else {
        let lf = memchr::memchr_iter(b'\n', sample).count();
        let crlf = memchr::memmem::find_iter(sample, b"\r\n").count();
        // No line ending at all (a single row): Windows default.
        if crlf >= lf - crlf { b"\r\n" } else { b"\n" }
    };
    let final_newline = match last_byte {
        None => true,
        Some(b) => b == d.terminator,
    };
    // Quote style from the complete rows of the sample.
    let mut quote_all = false;
    if d.quoting {
        let mut fields = crate::parse::FieldBuf::default();
        let (mut quoted, mut non_empty) = (0usize, 0usize);
        let mut rest = sample;
        while let Some(n) = crate::parse::parse_row(rest, d, whole, &mut fields) {
            for i in 0..fields.len() {
                if fields.quoted(i) {
                    quoted += 1;
                    non_empty += 1;
                } else if !fields.get(i).unwrap_or_default().is_empty() {
                    non_empty += 1;
                }
            }
            rest = &rest[n..];
            if non_empty > 20_000 {
                break;
            }
        }
        quote_all = non_empty > 0 && quoted * 10 >= non_empty * 9;
    }
    Style {
        line_ending,
        final_newline,
        quote_all,
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

    fn style(s: &[u8]) -> Style {
        let d = sniff(s, None).dialect;
        sniff_style(s, true, s.last().copied(), &d)
    }

    #[test]
    fn styles() {
        let s = style(b"a,b\r\n1,2\r\n");
        assert_eq!(s.line_ending, b"\r\n");
        assert!(s.final_newline);
        assert!(!s.quote_all);
        let s = style(b"a,b\n1,2");
        assert_eq!(s.line_ending, b"\n");
        assert!(!s.final_newline);
        assert_eq!(style(b"a,b\r1,2\r").line_ending, b"\r");
        assert_eq!(style(b"a,b").line_ending, b"\r\n");
        assert!(style(b"").final_newline);
        // Mixed: the majority wins.
        assert_eq!(style(b"a\r\nb\r\nc\nd\r\n").line_ending, b"\r\n");
        assert_eq!(style(b"a\nb\nc\r\nd\n").line_ending, b"\n");
        assert!(style(b"\"a\",\"b\"\n\"1\",\"\"\n").quote_all);
        assert!(!style(b"\"a\",b\n1,\"x\"\n").quote_all);
    }
}
