//! A note on disk ↔ a title and a body.
//!
//! A library note is plain Markdown whose first line may be an H1 (`# Title`):
//! that line is lifted into the title field. Everything else about the file —
//! encoding, BOM, line endings, the blank line after the title, the final
//! newline — is remembered so writing back an unchanged part is byte-identical.

use whir_core::library::parse_h1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextEncoding {
    Utf8 {
        bom: bool,
    },
    Utf16Le,
    Utf16Be,
    /// Legacy Western Windows text (anything that isn't valid UTF-8).
    Cp1252,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Eol {
    Lf,
    CrLf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format {
    pub encoding: TextEncoding,
    pub eol: Eol,
    pub final_newline: bool,
}

impl Format {
    /// New notes: UTF-8 without BOM, LF, trailing newline.
    pub const NEW: Format = Format {
        encoding: TextEncoding::Utf8 { bom: false },
        eol: Eol::Lf,
        final_newline: true,
    };
}

/// How the title line sat in the original file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    /// The title as parsed, and its raw line (reused while the title is unchanged).
    pub title: Option<(String, String)>,
    /// A newline followed the title line.
    pub title_nl: bool,
    /// A blank line separated the title from the body.
    pub blank_after_title: bool,
}

impl Header {
    pub const NEW: Header = Header {
        title: None,
        title_nl: false,
        blank_after_title: true,
    };
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parsed {
    pub title: String,
    pub body: String,
    pub header: Header,
    pub format: Format,
}

#[derive(Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// Contains NUL bytes: a binary file.
    NotText,
}

/// Bytes → text, detecting the encoding (BOMs, else UTF-8, else Windows-1252).
pub fn decode(bytes: &[u8]) -> Result<(String, TextEncoding), DecodeError> {
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return Ok((utf16(rest, false), TextEncoding::Utf16Le));
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return Ok((utf16(rest, true), TextEncoding::Utf16Be));
    }
    let (body, bom) = match bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        Some(rest) => (rest, true),
        None => (bytes, false),
    };
    if body[..body.len().min(8192)].contains(&0) {
        return Err(DecodeError::NotText);
    }
    match std::str::from_utf8(body) {
        Ok(s) => Ok((s.to_owned(), TextEncoding::Utf8 { bom })),
        Err(_) if bom => Ok((
            String::from_utf8_lossy(body).into_owned(),
            TextEncoding::Utf8 { bom },
        )),
        Err(_) => Ok((
            body.iter().map(|&b| cp1252_char(b)).collect(),
            TextEncoding::Cp1252,
        )),
    }
}

fn utf16(bytes: &[u8], big_endian: bool) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| {
            if big_endian {
                u16::from_be_bytes([c[0], c[1]])
            } else {
                u16::from_le_bytes([c[0], c[1]])
            }
        })
        .collect();
    String::from_utf16_lossy(&units)
}

const CP1252_HIGH: [char; 32] = [
    '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8d}', 'Ž', '\u{8f}',
    '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9d}', 'ž', 'Ÿ',
];

fn cp1252_char(b: u8) -> char {
    match b {
        0x80..=0x9F => CP1252_HIGH[(b - 0x80) as usize],
        _ => b as char,
    }
}

fn cp1252_byte(c: char) -> Option<u8> {
    match c as u32 {
        0..=0x7F | 0xA0..=0xFF => Some(c as u8),
        _ => CP1252_HIGH
            .iter()
            .position(|&h| h == c)
            .map(|i| 0x80 + i as u8),
    }
}

/// Split text into title and body. `lift_title`: library notes lift a leading H1.
pub fn parse(text: &str, encoding: TextEncoding, lift_title: bool) -> Parsed {
    let crlf = text.matches("\r\n").count();
    let lf = text.matches('\n').count() - crlf;
    let eol = if crlf > 0 && crlf >= lf {
        Eol::CrLf
    } else {
        Eol::Lf
    };
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let final_newline = normalized.ends_with('\n');
    let core = if final_newline {
        &normalized[..normalized.len() - 1]
    } else {
        &normalized[..]
    };
    let format = Format {
        encoding,
        eol,
        final_newline,
    };

    let first_line = core.split('\n').next().unwrap_or("");
    let title = if lift_title {
        parse_h1(first_line)
    } else {
        None
    };
    let Some(title) = title else {
        return Parsed {
            title: String::new(),
            body: core.to_owned(),
            header: Header {
                title: None,
                ..Header::NEW
            },
            format,
        };
    };
    let (title_nl, rest) = match core.find('\n') {
        Some(i) => (true, &core[i + 1..]),
        None => (false, ""),
    };
    let (blank_after_title, body) = match rest.strip_prefix('\n') {
        Some(b) => (true, b),
        None => (false, rest),
    };
    Parsed {
        title: title.clone(),
        body: body.to_owned(),
        header: Header {
            title: Some((title, first_line.to_owned())),
            title_nl,
            blank_after_title,
        },
        format,
    }
}

/// `# Title`, escaping a trailing `#` run that would read as a closing sequence.
fn title_line(title: &str) -> String {
    let title = title.replace(['\r', '\n'], " ");
    let trimmed = title.trim_end_matches('#');
    if trimmed.len() < title.len() && (trimmed.is_empty() || trimmed.ends_with([' ', '\t'])) {
        format!("# {}\\#", &title[..title.len() - 1])
    } else {
        format!("# {title}")
    }
}

/// Title + body → file text (still `\n` line endings).
pub fn compose(title: &str, body: &str, header: &Header, format: &Format) -> String {
    let mut out = String::with_capacity(title.len() + body.len() + 16);
    if !title.is_empty() {
        match &header.title {
            Some((orig, raw)) if orig == title => out.push_str(raw),
            _ => out.push_str(&title_line(title)),
        }
        if !body.is_empty() {
            out.push('\n');
            if header.blank_after_title {
                out.push('\n');
            }
        } else if header.title_nl {
            out.push('\n');
        }
    }
    out.push_str(body);
    if format.final_newline && !(out.is_empty() && body.is_empty() && title.is_empty()) {
        out.push('\n');
    }
    out
}

/// File text → bytes in `format`. `Err` lists why the original encoding
/// couldn't be kept; the caller then saves as UTF-8 with a BOM.
pub fn encode(text: &str, format: &Format) -> Result<Vec<u8>, char> {
    let text = match format.eol {
        Eol::Lf => std::borrow::Cow::Borrowed(text),
        Eol::CrLf => std::borrow::Cow::Owned(text.replace('\n', "\r\n")),
    };
    Ok(match format.encoding {
        TextEncoding::Utf8 { bom } => {
            let mut v = Vec::with_capacity(text.len() + 3);
            if bom {
                v.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
            }
            v.extend_from_slice(text.as_bytes());
            v
        }
        TextEncoding::Utf16Le | TextEncoding::Utf16Be => {
            let be = format.encoding == TextEncoding::Utf16Be;
            let mut v = Vec::with_capacity(text.len() * 2 + 2);
            v.extend_from_slice(if be { &[0xFE, 0xFF] } else { &[0xFF, 0xFE] });
            for u in text.encode_utf16() {
                v.extend_from_slice(&if be { u.to_be_bytes() } else { u.to_le_bytes() });
            }
            v
        }
        TextEncoding::Cp1252 => {
            let mut v = Vec::with_capacity(text.len());
            for c in text.chars() {
                v.push(cp1252_byte(c).ok_or(c)?);
            }
            v
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(bytes: &[u8], lift: bool) {
        let (text, enc) = decode(bytes).unwrap();
        let p = parse(&text, enc, lift);
        let out = encode(&compose(&p.title, &p.body, &p.header, &p.format), &p.format).unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out),
            String::from_utf8_lossy(bytes),
            "roundtrip of {bytes:?}"
        );
        assert_eq!(out, bytes);
    }

    #[test]
    fn unchanged_notes_roundtrip_byte_for_byte() {
        for case in [
            &b"# Title\n\nbody\n"[..],
            b"# Title\nbody",
            b"# Title\n",
            b"# Title\n\n",
            b"# Title",
            b"#   Spaced   ##\n\ntext\n",
            b"# C\\# rocks\n\nx\n",
            b"\xEF\xBB\xBF# Bom\r\n\r\nwindows\r\nlines\r\n",
            b"no title here\nsecond\n",
            b"---\nfront: matter\n---\n# Not lifted\n",
            b"",
            b"just text",
            b"Setext\n======\n",
        ] {
            roundtrip(case, true);
            roundtrip(case, false);
        }
        let utf16: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain(
                "# Hi\r\n\r\nthere\r\n"
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes),
            )
            .collect();
        roundtrip(&utf16, true);
        roundtrip(b"caf\xE9 \x80\n", false);
    }

    #[test]
    fn lifting_titles() {
        let p = parse(
            "# Plan\n\n- item\n",
            TextEncoding::Utf8 { bom: false },
            true,
        );
        assert_eq!(p.title, "Plan");
        assert_eq!(p.body, "- item");
        let p = parse(
            "# Plan\n\n- item\n",
            TextEncoding::Utf8 { bom: false },
            false,
        );
        assert_eq!(p.title, "");
        assert_eq!(p.body, "# Plan\n\n- item");
        let p = parse("## Sub\nx", TextEncoding::Utf8 { bom: false }, true);
        assert_eq!(p.title, "");
    }

    #[test]
    fn editing_title_rewrites_only_the_header() {
        let (t, e) = decode(b"#  Old  \n\nbody\n").unwrap();
        let p = parse(&t, e, true);
        let text = compose("New", &p.body, &p.header, &p.format);
        assert_eq!(text, "# New\n\nbody\n");
        // Body-only edit keeps the raw (oddly spaced) title line.
        let text = compose(&p.title, "changed", &p.header, &p.format);
        assert_eq!(text, "#  Old  \n\nchanged\n");
        // Clearing the title drops the heading and its blank line.
        let text = compose("", &p.body, &p.header, &p.format);
        assert_eq!(text, "body\n");
    }

    #[test]
    fn new_notes() {
        let f = Format::NEW;
        assert_eq!(compose("T", "hello", &Header::NEW, &f), "# T\n\nhello\n");
        assert_eq!(compose("T", "", &Header::NEW, &f), "# T\n");
        assert_eq!(compose("", "just body", &Header::NEW, &f), "just body\n");
        assert_eq!(compose("a #", "", &Header::NEW, &f), "# a \\#\n");
        let p = parse("# a \\#\n", TextEncoding::Utf8 { bom: false }, true);
        assert_eq!(p.title, "a #");
    }

    #[test]
    fn encodings() {
        assert_eq!(decode(b"a\0b"), Err(DecodeError::NotText));
        assert_eq!(
            decode(b"caf\xE9").unwrap(),
            ("café".to_owned(), TextEncoding::Cp1252)
        );
        let f = Format {
            encoding: TextEncoding::Cp1252,
            eol: Eol::Lf,
            final_newline: false,
        };
        assert_eq!(encode("café €", &f).unwrap(), b"caf\xE9 \x80");
        assert_eq!(encode("日本", &f), Err('日'));
    }
}
