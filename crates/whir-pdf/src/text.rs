//! A page's text as PDFium sees it: one UTF-16 unit per PDFium char index.
//!
//! Everything else (selection, search, copy) works on those indices, so this
//! module holds the character classes, word boundaries and the rules for
//! turning a range of units back into clipboard text. Pure: no PDFium here.

use std::ops::Range;

use crate::geom::PageGeom;

/// What the PDF thread extracts from one page.
#[derive(Default)]
pub struct PageText {
    /// One unit per PDFium char index (`FPDFText_GetUnicode`, not
    /// `FPDFText_GetText`, which skips characters and shifts indices).
    pub chars: Vec<u16>,
    /// Indices PDFium flags as line-end hyphens (sorted).
    pub hyphens: Vec<u32>,
    /// Boxes and lines, only when the UI asked for them (visible pages).
    pub geom: Option<PageGeom>,
    /// Geometry was asked for but can't exist (degenerate page transform):
    /// asking again won't help.
    pub no_geom: bool,
    /// PDFium couldn't load the page or its text: treat it as empty.
    pub failed: bool,
}

impl PageText {
    pub fn failed() -> Self {
        Self {
            failed: true,
            ..Self::default()
        }
    }
}

// --- character classes ------------------------------------------------------

pub const CR: u16 = 0x0D;
pub const LF: u16 = 0x0A;
const SOFT_HYPHEN: u16 = 0x00AD;

#[inline]
pub fn is_break(u: u16) -> bool {
    u == CR || u == LF
}

/// Whitespace, line breaks included.
#[inline]
pub fn is_space(u: u16) -> bool {
    matches!(
        u,
        0x09..=0x0D
            | 0x20
            | 0x85
            | 0xA0
            | 0x1680
            | 0x2000..=0x200A
            | 0x2028
            | 0x2029
            | 0x202F
            | 0x205F
            | 0x3000
    )
}

/// Units that carry no visible text: controls (PDFium uses a few C1 codes as
/// markers), soft hyphens, zero-width characters, BOMs and non-characters.
/// Search skips them inside a match and copy drops them.
#[inline]
pub fn is_ignorable(u: u16) -> bool {
    matches!(
        u,
        0x00..=0x08
            | 0x0E..=0x1F
            | 0x7F..=0x84
            | 0x86..=0x9F
            | SOFT_HYPHEN
            | 0x200B..=0x200D
            | 0xFEFF
            | 0xFFFE
            | 0xFFFF
    )
}

#[inline]
pub fn is_combining(u: u16) -> bool {
    (0x0300..=0x036F).contains(&u)
}

#[inline]
pub fn is_high_surrogate(u: u16) -> bool {
    (0xD800..=0xDBFF).contains(&u)
}

#[inline]
pub fn is_low_surrogate(u: u16) -> bool {
    (0xDC00..=0xDFFF).contains(&u)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    LineBreak,
    Space,
    Word,
    Cjk,
    Punct,
    Ignorable,
}

pub fn class(u: u16) -> Class {
    if is_break(u) {
        Class::LineBreak
    } else if is_space(u) {
        Class::Space
    } else if is_ignorable(u) {
        Class::Ignorable
    } else if matches!(u, 0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF) {
        Class::Cjk
    } else if u == b'_' as u16
        || is_combining(u)
        || (0xD800..=0xDFFF).contains(&u)
        || char::from_u32(u as u32).is_some_and(char::is_alphanumeric)
    {
        Class::Word
    } else {
        Class::Punct
    }
}

fn is_digit(u: u16) -> bool {
    char::from_u32(u as u32).is_some_and(|c| c.is_numeric())
}

/// Joins two word characters: `don't`, `3.14`, `1,000`.
fn joins(before: u16, mid: u16, after: u16) -> bool {
    let word = |u| class(u) == Class::Word;
    match mid {
        0x27 | 0x2019 => word(before) && word(after),
        0x2E | 0x2C => is_digit(before) && is_digit(after),
        _ => false,
    }
}

/// The "word" around index `i` for double-click selection: a run of word
/// characters (with apostrophes and decimal separators inside), a run of
/// punctuation, a run of spaces, or a run of CJK ideographs/kana.
/// Ignorable units attach to their neighbours. Never crosses a line break
/// unless `i` is on one.
pub fn word_at(chars: &[u16], i: u32) -> Range<u32> {
    let n = chars.len() as u32;
    if n == 0 {
        return 0..0;
    }
    let i = i.min(n - 1);
    // An ignorable belongs to the character before it (else the one after).
    let seed = (0..=i)
        .rev()
        .find(|&k| class(chars[k as usize]) != Class::Ignorable)
        .or_else(|| (i..n).find(|&k| class(chars[k as usize]) != Class::Ignorable));
    let Some(seed) = seed else {
        return i..i + 1;
    };
    let at = |k: u32| chars[k as usize];
    let mut c = class(at(seed));
    // The '.' of "3.14" belongs to the number.
    if c == Class::Punct && seed > 0 && seed + 1 < n && joins(at(seed - 1), at(seed), at(seed + 1))
    {
        c = Class::Word;
    }
    let fits = |k: u32| {
        let ck = class(at(k));
        ck == c || ck == Class::Ignorable
    };

    let mut lo = seed;
    while lo > 0 {
        if fits(lo - 1) {
            lo -= 1;
        } else if c == Class::Word && lo >= 2 && joins(at(lo - 2), at(lo - 1), at(lo)) {
            lo -= 2;
        } else {
            break;
        }
    }
    let mut hi = seed + 1;
    while hi < n {
        if fits(hi) {
            hi += 1;
        } else if c == Class::Word && hi + 1 < n && joins(at(hi - 1), at(hi), at(hi + 1)) {
            hi += 2;
        } else {
            break;
        }
    }
    // Keep ignorables inside the unit, not dangling at its edges.
    while lo < seed && class(at(lo)) == Class::Ignorable {
        lo += 1;
    }
    while hi > seed + 1 && class(at(hi - 1)) == Class::Ignorable {
        hi -= 1;
    }
    lo.min(i)..hi.max(i + 1)
}

/// Ranges search may skip inside a match: a line-end hyphen (PDFium's flag,
/// or a soft hyphen before a break) plus the line break after it, so
/// "hyphenated" finds "hyphen-⏎ated". Sorted, non-overlapping, `[start, end)`.
pub fn soft_ranges(chars: &[u16], hyphens: &[u32]) -> Vec<(u32, u32)> {
    let n = chars.len() as u32;
    let mut out: Vec<(u32, u32)> = Vec::new();
    let mut push = |h: u32, require_break: bool| {
        let mut j = h + 1;
        // Stray spaces/markers between the hyphen and the break.
        while j < n
            && !is_break(chars[j as usize])
            && (is_space(chars[j as usize]) || is_ignorable(chars[j as usize]))
        {
            j += 1;
        }
        let breaks_at = j;
        while j < n && is_break(chars[j as usize]) {
            j += 1;
        }
        let end = if j > breaks_at { j } else { h + 1 };
        if require_break && j == breaks_at {
            return;
        }
        if out.last().is_none_or(|l| l.1 <= h) {
            out.push((h, end));
        }
    };
    let mut hy = hyphens.iter().copied().filter(|&h| h < n).peekable();
    for k in 0..n {
        if hy.peek() == Some(&k) {
            hy.next();
            push(k, false);
        } else if chars[k as usize] == SOFT_HYPHEN {
            push(k, true);
        }
    }
    out
}

// --- copy -----------------------------------------------------------------------

/// Append `range` of a page as clipboard text: breaks become `\n`, line-end
/// hyphens `-`, soft hyphens `-` only before a break, other ignorable units
/// vanish, and trailing spaces before a newline are trimmed. Stops once `out`
/// reaches `cap` bytes; returns false if it did.
pub fn push_copy(
    out: &mut String,
    chars: &[u16],
    hyphens: &[u32],
    range: Range<u32>,
    cap: usize,
) -> bool {
    let n = chars.len() as u32;
    let (start, end) = (range.start.min(n), range.end.min(n));
    let mut hy = hyphens.partition_point(|&h| h < start);
    let mut k = start;
    while k < end {
        if out.len() >= cap {
            return false;
        }
        let u = chars[k as usize];
        while hyphens.get(hy).is_some_and(|&h| h < k) {
            hy += 1;
        }
        if hyphens.get(hy) == Some(&k) {
            hy += 1;
            out.push('-');
            k += 1;
            continue;
        }
        if u == CR || u == LF {
            // \r\n, a lone \r or a lone \n: one newline.
            if u == CR && k + 1 < end && chars[k as usize + 1] == LF {
                k += 1;
            }
            push_newline(out);
            k += 1;
            continue;
        }
        if u == SOFT_HYPHEN {
            let next = chars[k as usize + 1..].iter().find(|&&c| !is_ignorable(c));
            if next.is_some_and(|&c| is_break(c)) {
                out.push('-');
            }
            k += 1;
            continue;
        }
        if is_ignorable(u) {
            k += 1;
            continue;
        }
        // Surrogate pairs decode together; lone halves become U+FFFD.
        if is_high_surrogate(u) && k + 1 < end && is_low_surrogate(chars[k as usize + 1]) {
            let c = char::decode_utf16([u, chars[k as usize + 1]])
                .next()
                .and_then(Result::ok)
                .unwrap_or(char::REPLACEMENT_CHARACTER);
            out.push(c);
            k += 2;
            continue;
        }
        out.push(char::from_u32(u as u32).unwrap_or(char::REPLACEMENT_CHARACTER));
        k += 1;
    }
    true
}

fn push_newline(out: &mut String) {
    let trimmed = out.trim_end_matches([' ', '\t']).len();
    out.truncate(trimmed);
    out.push('\n');
}

/// Separate two pages' text with a newline (unless one is already there).
pub fn push_page_break(out: &mut String) {
    if !out.is_empty() && !out.ends_with('\n') {
        push_newline(out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    fn word(s: &str, i: u32) -> String {
        let c = u(s);
        let r = word_at(&c, i);
        String::from_utf16_lossy(&c[r.start as usize..r.end as usize])
    }

    #[test]
    fn classes() {
        assert_eq!(class(b'a' as u16), Class::Word);
        assert_eq!(class(0x00E9), Class::Word);
        assert_eq!(class(b'7' as u16), Class::Word);
        assert_eq!(class(b'_' as u16), Class::Word);
        assert_eq!(class(0x0301), Class::Word);
        assert_eq!(class(b'.' as u16), Class::Punct);
        assert_eq!(class(0x00A0), Class::Space);
        assert_eq!(class(0x3000), Class::Space);
        assert_eq!(class(CR), Class::LineBreak);
        assert_eq!(class(0x00AD), Class::Ignorable);
        assert_eq!(class(0x0094), Class::Ignorable);
        assert_eq!(class(0x0085), Class::Space);
        assert_eq!(class(0x4E2D), Class::Cjk);
        assert_eq!(class(0x3042), Class::Cjk);
    }

    #[test]
    fn words() {
        assert_eq!(word("hello world", 1), "hello");
        assert_eq!(word("hello world", 7), "world");
        assert_eq!(word("hello world", 5), " ");
        assert_eq!(word("pi is 3.14 ok", 6), "3.14");
        assert_eq!(word("pi is 3.14 ok", 7), "3.14");
        assert_eq!(word("pi is 3.14 ok", 9), "3.14");
        assert_eq!(word("1,000 items", 0), "1,000");
        assert_eq!(word("end. Next", 1), "end");
        assert_eq!(word("end. Next", 3), ".");
        assert_eq!(word("I don't know", 3), "don't");
        assert_eq!(word("it\u{2019}s", 0), "it\u{2019}s");
        assert_eq!(word("wait...!? no", 5), "...!?");
        assert_eq!(word("a   \r\nb", 2), "   ");
        assert_eq!(word("abc\r\ndef", 1), "abc");
        assert_eq!(word("abc\r\ndef", 4), "\r\n");
        assert_eq!(word("中文字 abc", 1), "中文字");
        assert_eq!(word("中文abc", 0), "中文");
        // Soft hyphens are part of the word, but never dangle at an edge.
        assert_eq!(word("hy\u{00AD}phen x", 0), "hy\u{00AD}phen");
        // Clicking an ignorable seeds from the character before it.
        assert_eq!(word("ab\u{200B} cd", 2), "ab\u{200B}");
        assert_eq!(word("\u{200B}ab", 0), "\u{200B}ab");
        assert_eq!(word("", 0), "");
        assert_eq!(word("abc", 99), "abc");
        // Sentence punctuation after a word isn't a joiner.
        assert_eq!(word("dogs' toys", 0), "dogs");
    }

    fn copy(s: &str, hyphens: &[u32]) -> String {
        let c = u(s);
        let mut out = String::new();
        assert!(push_copy(
            &mut out,
            &c,
            hyphens,
            0..c.len() as u32,
            usize::MAX
        ));
        out
    }

    #[test]
    fn copy_rules() {
        assert_eq!(
            copy("one\r\ntwo\rthree\nfour", &[]),
            "one\ntwo\nthree\nfour"
        );
        assert_eq!(copy("a\r\n\r\nb", &[]), "a\n\nb");
        // Flagged hyphen, whatever unit PDFium used for it.
        assert_eq!(copy("hy\u{2}\r\nphen", &[2]), "hy-\nphen");
        assert_eq!(copy("hy\u{FFFE}\r\nphen", &[2]), "hy-\nphen");
        // Soft hyphen: '-' before a break, dropped elsewhere.
        assert_eq!(copy("co\u{AD}\r\nop", &[]), "co-\nop");
        assert_eq!(copy("co\u{AD}op", &[]), "coop");
        // Other ignorables vanish.
        assert_eq!(copy("a\u{200B}b\u{FEFF}c\u{0}d\u{94}e", &[]), "abcde");
        // Trailing spaces before a newline are trimmed; others kept.
        assert_eq!(copy("end   \r\n  next ", &[]), "end\n  next ");
        // Surrogate pairs decode; lone halves don't panic.
        assert_eq!(copy("x\u{1F600}y", &[]), "x\u{1F600}y");
        let lone = [0xD800u16, b'a' as u16];
        let mut out = String::new();
        push_copy(&mut out, &lone, &[], 0..2, usize::MAX);
        assert_eq!(out, "\u{FFFD}a");
    }

    #[test]
    fn copy_subrange_and_pages() {
        let c = u("hello world");
        let mut out = String::new();
        push_copy(&mut out, &c, &[], 6..u32::MAX, usize::MAX);
        push_page_break(&mut out);
        push_copy(&mut out, &c, &[], 0..5, usize::MAX);
        push_page_break(&mut out);
        push_page_break(&mut out);
        assert_eq!(out, "world\nhello\n");
        let mut capped = String::new();
        assert!(!push_copy(&mut capped, &c, &[], 0..11, 4));
        assert_eq!(capped, "hell");
    }

    #[test]
    fn soft() {
        // Flagged hyphen + the break after it.
        let c = u("hyphen-\r\nated x");
        assert_eq!(soft_ranges(&c, &[6]), vec![(6, 9)]);
        // Flag without a break: just the hyphen.
        assert_eq!(soft_ranges(&u("ab-cd"), &[2]), vec![(2, 3)]);
        // Soft hyphen before a break counts; inside a word it's merely ignorable.
        assert_eq!(soft_ranges(&u("co\u{AD}\r\nop"), &[]), vec![(2, 5)]);
        assert_eq!(soft_ranges(&u("co\u{AD}op"), &[]), vec![]);
        // Stray space before the break; out-of-range flags ignored.
        assert_eq!(soft_ranges(&u("a- \r\nb"), &[1, 99]), vec![(1, 5)]);
    }
}
