//! Clipboard text → cells. Always read as TSV, Excel's clipboard format, with
//! the file parser's own rules on `\t`: quoted cells may hold tabs, line
//! breaks and `""`. Commas never split, so "1,234" or "Smith, John" paste
//! into one cell whatever the file's delimiter is.

use crate::dialect::Dialect;
use crate::parse::{FieldBuf, parse_row};

/// Longest clipboard text taken, and most cells.
const MAX_PASTE_BYTES: usize = 64 << 20;
pub const MAX_PASTE_CELLS: usize = 1_000_000;
pub const TOO_LARGE: &str = "Paste is limited to 1,000,000 cells.";

/// Parsed clipboard: rows of cells (rows may be shorter than `width`).
#[derive(Debug, PartialEq, Eq)]
pub struct Clip {
    pub rows: Vec<Vec<String>>,
    pub width: usize,
}

impl Clip {
    pub fn cells(&self) -> usize {
        self.rows.iter().map(Vec::len).sum()
    }
}

pub fn parse_clipboard(text: &str) -> Result<Clip, &'static str> {
    if text.len() > MAX_PASTE_BYTES {
        return Err(TOO_LARGE);
    }
    if text.is_empty() {
        return Ok(Clip {
            rows: Vec::new(),
            width: 0,
        });
    }
    // Copies end with one line break (the platform turned CRLF into LF).
    let body = text
        .strip_suffix("\r\n")
        .or_else(|| text.strip_suffix('\n'))
        .unwrap_or(text);
    let d = Dialect {
        delimiter: b'\t',
        terminator: b'\n',
        quoting: true,
    };
    let mut rows = Vec::new();
    let mut cells = 0usize;
    let mut fields = FieldBuf::default();
    let mut rest = body.as_bytes();
    if rest.is_empty() {
        // A single empty cell ("\n" on the clipboard).
        rows.push(vec![String::new()]);
    }
    let mut ended_line = false;
    while let Some(n) = parse_row(rest, &d, true, &mut fields) {
        cells += fields.len();
        if cells > MAX_PASTE_CELLS {
            return Err(TOO_LARGE);
        }
        rows.push(
            fields
                .iter()
                .map(|f| String::from_utf8_lossy(f).into_owned())
                .collect(),
        );
        ended_line = !fields.open_quote && rest[..n].last() == Some(&b'\n');
        rest = &rest[n..];
    }
    // "a\n" (after dropping the final line break): an empty last row follows.
    if ended_line {
        rows.push(vec![String::new()]);
    }
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    Ok(Clip { rows, width })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(s: &str) -> Vec<Vec<String>> {
        parse_clipboard(s).unwrap().rows
    }

    #[test]
    fn excel_quoting_is_undone() {
        assert_eq!(
            rows("\"multi\nline\"\t\"tab\there\"\t\"say \"\"hi\"\"\"\n"),
            [["multi\nline", "tab\there", "say \"hi\""]]
        );
        // A single quoted value comes out unquoted.
        assert_eq!(rows("\"x\""), [["x"]]);
    }

    #[test]
    fn one_trailing_newline_is_dropped() {
        assert_eq!(rows("a\tb\n1\t2\n"), [["a", "b"], ["1", "2"]]);
        assert_eq!(rows("a\n\n"), [vec!["a"], vec![""]]);
        assert_eq!(rows("a\r\n"), [["a"]]);
        assert_eq!(rows("\n"), [[""]]);
        assert!(rows("").is_empty());
    }

    #[test]
    fn commas_never_split() {
        assert_eq!(rows("1,234"), [["1,234"]]);
        assert_eq!(rows("Smith, John\n"), [["Smith, John"]]);
    }

    #[test]
    fn empty_and_ragged_cells() {
        assert_eq!(rows("a\t\n"), [["a", ""]]);
        let c = parse_clipboard("a\tb\tc\nd\n").unwrap();
        assert_eq!(c.width, 3);
        assert_eq!(c.rows[1], ["d"]);
        assert_eq!(c.cells(), 4);
    }

    #[test]
    fn caps() {
        let big = "x\t".repeat(MAX_PASTE_CELLS + 1);
        assert_eq!(parse_clipboard(&big), Err(TOO_LARGE));
    }
}
