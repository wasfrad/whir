//! Output encodings. Raw file bytes arrive in the source's encoding (UTF-8, or
//! Windows-1252; UTF-16 files are read from their UTF-8 temp copy), edited text
//! as UTF-8. The sink converts both to the file's encoding and counts bytes.

use std::io::{self, Write};

use crate::dialect::Encoding;
use crate::parse::{decode_into, encode_cp1252_lossy};

/// How the saved file is encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutEncoding {
    /// Source is UTF-8 (a BOM only if the file had one).
    Utf8 { bom: bool },
    /// Source and output Windows-1252.
    Cp1252,
    /// A Windows-1252 file whose edits need characters it lacks: the whole
    /// file is converted to UTF-8 with a BOM (so Excel reads it right).
    Cp1252ToUtf8Bom,
    /// Source is the UTF-8 temp copy of a UTF-16 file.
    Utf16 { be: bool, bom: bool },
}

impl OutEncoding {
    /// The encoding for a file read as `original` (`had_bom`: it started with one).
    pub fn keep(original: Encoding, had_bom: bool) -> Self {
        match original {
            Encoding::Utf8 => OutEncoding::Utf8 { bom: had_bom },
            Encoding::Windows1252 => OutEncoding::Cp1252,
            Encoding::Utf16Le => OutEncoding::Utf16 {
                be: false,
                bom: had_bom,
            },
            Encoding::Utf16Be => OutEncoding::Utf16 {
                be: true,
                bom: had_bom,
            },
        }
    }

    /// How the written file reads back (its own bytes, before any transcoding).
    pub fn file_encoding(self) -> Encoding {
        match self {
            OutEncoding::Utf8 { .. } | OutEncoding::Cp1252ToUtf8Bom => Encoding::Utf8,
            OutEncoding::Cp1252 => Encoding::Windows1252,
            OutEncoding::Utf16 { be: false, .. } => Encoding::Utf16Le,
            OutEncoding::Utf16 { be: true, .. } => Encoding::Utf16Be,
        }
    }

    pub fn bom(self) -> &'static [u8] {
        match self {
            OutEncoding::Utf8 { bom: true } | OutEncoding::Cp1252ToUtf8Bom => b"\xEF\xBB\xBF",
            OutEncoding::Utf16 {
                be: false,
                bom: true,
            } => b"\xFF\xFE",
            OutEncoding::Utf16 {
                be: true,
                bom: true,
            } => b"\xFE\xFF",
            _ => b"",
        }
    }

    /// Bytes of output per byte of source, roughly (disk space check).
    pub fn growth(self) -> u64 {
        match self {
            OutEncoding::Utf16 { .. } => 2,
            _ => 1,
        }
    }
}

pub struct Sink<W: Write> {
    out: W,
    pos: u64,
    enc: OutEncoding,
    scratch: Vec<u8>,
    text: String,
}

impl<W: Write> Sink<W> {
    pub fn new(out: W, enc: OutEncoding) -> Self {
        Self {
            out,
            pos: 0,
            enc,
            scratch: Vec::new(),
            text: String::new(),
        }
    }

    /// Bytes written so far.
    pub fn position(&self) -> u64 {
        self.pos
    }

    pub fn into_inner(self) -> W {
        self.out
    }

    fn put(&mut self, b: &[u8]) -> io::Result<()> {
        self.out.write_all(b)?;
        self.pos += b.len() as u64;
        Ok(())
    }

    fn put_utf16(&mut self, s: &str) -> io::Result<()> {
        let be = matches!(self.enc, OutEncoding::Utf16 { be: true, .. });
        self.scratch.clear();
        for u in s.encode_utf16() {
            self.scratch
                .extend_from_slice(&if be { u.to_be_bytes() } else { u.to_le_bytes() });
        }
        let buf = std::mem::take(&mut self.scratch);
        let r = self.put(&buf);
        self.scratch = buf;
        r
    }

    pub fn bom(&mut self) -> io::Result<()> {
        let bom = self.enc.bom();
        self.put(bom)
    }

    /// Bytes in the source's encoding.
    pub fn raw(&mut self, b: &[u8]) -> io::Result<()> {
        match self.enc {
            OutEncoding::Utf8 { .. } | OutEncoding::Cp1252 => self.put(b),
            OutEncoding::Cp1252ToUtf8Bom => {
                if b.is_ascii() {
                    return self.put(b);
                }
                let mut t = std::mem::take(&mut self.text);
                t.clear();
                decode_into(b, Encoding::Windows1252, &mut t);
                let r = self.put(t.as_bytes());
                self.text = t;
                r
            }
            OutEncoding::Utf16 { .. } => {
                let s = String::from_utf8_lossy(b).into_owned();
                self.put_utf16(&s)
            }
        }
    }

    /// Edited text.
    pub fn text(&mut self, s: &str) -> io::Result<()> {
        match self.enc {
            OutEncoding::Utf8 { .. } | OutEncoding::Cp1252ToUtf8Bom => self.put(s.as_bytes()),
            OutEncoding::Cp1252 => {
                if s.is_ascii() {
                    return self.put(s.as_bytes());
                }
                let mut buf = std::mem::take(&mut self.scratch);
                buf.clear();
                encode_cp1252_lossy(s, &mut buf);
                let r = self.put(&buf);
                self.scratch = buf;
                r
            }
            OutEncoding::Utf16 { .. } => self.put_utf16(s),
        }
    }

    /// Structural ASCII: delimiters, quotes, line endings.
    pub fn ascii(&mut self, b: &[u8]) -> io::Result<()> {
        debug_assert!(b.is_ascii());
        match self.enc {
            OutEncoding::Utf16 { be, .. } => {
                self.scratch.clear();
                for &c in b {
                    self.scratch
                        .extend_from_slice(&if be { [0, c] } else { [c, 0] });
                }
                let buf = std::mem::take(&mut self.scratch);
                let r = self.put(&buf);
                self.scratch = buf;
                r
            }
            _ => self.put(b),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodings() {
        let mut s = Sink::new(Vec::new(), OutEncoding::Cp1252);
        s.raw(b"caf\xE9,").unwrap();
        s.text("€uro").unwrap();
        assert_eq!(s.position(), 9);
        assert_eq!(s.into_inner(), b"caf\xE9,\x80uro");

        let mut s = Sink::new(Vec::new(), OutEncoding::Cp1252ToUtf8Bom);
        s.bom().unwrap();
        s.raw(b"caf\xE9,").unwrap();
        s.text("😀").unwrap();
        assert_eq!(s.into_inner(), "\u{feff}café,😀".as_bytes());

        let mut s = Sink::new(
            Vec::new(),
            OutEncoding::Utf16 {
                be: true,
                bom: true,
            },
        );
        s.bom().unwrap();
        s.raw("é".as_bytes()).unwrap();
        s.ascii(b",").unwrap();
        s.text("😀").unwrap();
        let want: Vec<u8> = "\u{feff}é,😀"
            .encode_utf16()
            .flat_map(|u| u.to_be_bytes())
            .collect();
        assert_eq!(s.position(), want.len() as u64);
        assert_eq!(s.into_inner(), want);
    }
}
