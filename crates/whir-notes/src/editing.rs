//! Note-taking keystrokes as pure text edits: list continuation on Enter,
//! indent/outdent, task checkboxes, bold/italic/link wrapping.
//!
//! Positions are *char* indices (what egui's `CCursor` uses), never bytes.

use std::ops::Range;

/// Replace `replace` (chars) with `insert`, then select `select` (chars, in the new text).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub replace: Range<usize>,
    pub insert: String,
    pub select: Range<usize>,
}

/// Spaces per indent level (valid nesting under both `- ` and `1. `).
const INDENT: &str = "    ";

fn byte_of(text: &str, ci: usize) -> usize {
    text.char_indices().nth(ci).map_or(text.len(), |(b, _)| b)
}

fn char_of(text: &str, bi: usize) -> usize {
    text[..bi].chars().count()
}

fn line_start(text: &str, b: usize) -> usize {
    text[..b].rfind('\n').map_or(0, |i| i + 1)
}

fn line_end(text: &str, b: usize) -> usize {
    text[b..].find('\n').map_or(text.len(), |i| b + i)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Marker {
    Bullet(char),
    /// Number and its delimiter (`.` or `)`).
    Ordered(u64, char),
    /// A run of `>` quote markers, as written (e.g. "> > ").
    Quote(String),
}

/// A list/quote line's leading syntax.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Prefix {
    pub(crate) indent: String,
    pub(crate) marker: Marker,
    /// Spaces between the marker and the content (list items).
    pub(crate) spacing: String,
    /// `Some(checked)` for `- [ ] ` / `- [x] ` tasks.
    pub(crate) task: Option<bool>,
    /// Byte length of the whole prefix.
    pub(crate) len: usize,
}

pub(crate) fn parse_prefix(line: &str) -> Option<Prefix> {
    let indent_len = line.len() - line.trim_start_matches([' ', '\t']).len();
    let indent = line[..indent_len].to_owned();
    let rest = &line[indent_len..];
    if rest.starts_with('>') {
        let mut n = 0;
        let bytes = rest.as_bytes();
        while n < bytes.len() && (bytes[n] == b'>' || bytes[n] == b' ') {
            n += 1;
        }
        return Some(Prefix {
            indent,
            marker: Marker::Quote(rest[..n].to_owned()),
            spacing: String::new(),
            task: None,
            len: indent_len + n,
        });
    }
    let (marker, mlen) = match rest.chars().next()? {
        c @ ('-' | '*' | '+') => (Marker::Bullet(c), 1),
        c if c.is_ascii_digit() => {
            let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
            if digits > 9 {
                return None;
            }
            let delim = rest[digits..].chars().next()?;
            if delim != '.' && delim != ')' {
                return None;
            }
            (
                Marker::Ordered(rest[..digits].parse().ok()?, delim),
                digits + 1,
            )
        }
        _ => return None,
    };
    let after = &rest[mlen..];
    let spaces = after.len() - after.trim_start_matches(' ').len();
    // "-foo" isn't a list item; a bare "-" at line end is (an empty item).
    if spaces == 0 && !after.is_empty() {
        return None;
    }
    let spacing = after[..spaces].to_owned();
    let mut len = indent_len + mlen + spaces;
    let after = &after[spaces..];
    let task = if spaces > 0 && (after.starts_with("[ ] ") || after == "[ ]") {
        len += 4.min(after.len());
        Some(false)
    } else if spaces > 0
        && (after.starts_with("[x] ")
            || after.starts_with("[X] ")
            || after == "[x]"
            || after == "[X]")
    {
        len += 4.min(after.len());
        Some(true)
    } else {
        None
    };
    Some(Prefix {
        indent,
        marker,
        spacing: if spaces == 0 { " ".into() } else { spacing },
        task,
        len,
    })
}

impl Prefix {
    /// The prefix for the next item: same indent, next number, unchecked task.
    fn next(&self) -> String {
        let marker = match &self.marker {
            Marker::Bullet(c) => c.to_string(),
            Marker::Ordered(n, d) => format!("{}{d}", n + 1),
            Marker::Quote(q) => return format!("{}{q}", self.indent),
        };
        let task = if self.task.is_some() { "[ ] " } else { "" };
        format!("{}{marker}{}{task}", self.indent, self.spacing)
    }
}

/// Enter inside a list, task list or quote: continue it. On an empty item,
/// outdent it (if nested) or remove its marker. `None`: let Enter insert a
/// plain newline.
pub fn on_enter(text: &str, sel: Range<usize>) -> Option<Edit> {
    if sel.start != sel.end {
        return None;
    }
    let b = byte_of(text, sel.start);
    let (ls, le) = (line_start(text, b), line_end(text, b));
    let line = &text[ls..le];
    let prefix = parse_prefix(line)?;
    if b < ls + prefix.len {
        return None; // caret inside the marker: plain newline
    }
    let content = &line[prefix.len..];
    let ls_c = char_of(text, ls);
    if content.trim().is_empty() {
        // Empty item: step out of the list.
        let replacement = if !prefix.indent.is_empty() && !matches!(prefix.marker, Marker::Quote(_))
        {
            let outdented = outdent_str(&prefix.indent);
            let mut p = prefix.clone();
            p.indent = outdented.to_owned();
            let mut s = p.next();
            // Keep the same number when outdenting an ordered item.
            if let Marker::Ordered(n, d) = prefix.marker {
                let task = if prefix.task.is_some() { "[ ] " } else { "" };
                s = format!("{}{n}{d}{}{task}", p.indent, p.spacing);
            }
            s
        } else {
            String::new()
        };
        let end_c = ls_c + line.chars().count();
        let new_len = replacement.chars().count();
        return Some(Edit {
            replace: ls_c..end_c,
            insert: replacement,
            select: ls_c + new_len..ls_c + new_len,
        });
    }
    let insert = format!("\n{}", prefix.next());
    let caret = sel.start + insert.chars().count();
    Some(Edit {
        replace: sel.start..sel.start,
        insert,
        select: caret..caret,
    })
}

fn outdent_str(indent: &str) -> &str {
    if let Some(rest) = indent.strip_suffix('\t') {
        return rest;
    }
    let spaces = indent.len() - indent.trim_end_matches(' ').len();
    &indent[..indent.len() - spaces.min(INDENT.len())]
}

/// Tab / Shift+Tab. Indents every line the selection touches when it spans
/// lines or the caret is on a list/quote line. `None`: not ours (plain Tab).
pub fn indent(text: &str, sel: Range<usize>, outdent: bool) -> Option<Edit> {
    let (s, e) = (byte_of(text, sel.start), byte_of(text, sel.end));
    let multi = text[s..e].contains('\n');
    let first = line_start(text, s);
    if !multi && !outdent && parse_prefix(&text[first..line_end(text, s)]).is_none() {
        return None;
    }
    // A selection ending right at a line start doesn't include that line.
    let e_eff = if multi && e > s && text[..e].ends_with('\n') {
        e - 1
    } else {
        e
    };
    let last_end = line_end(text, e_eff);
    let block = &text[first..last_end];
    let first_c = char_of(text, first);

    let mut out = String::with_capacity(block.len() + 16);
    // (old char offset of line start within block, chars added (+) or removed (−))
    let mut deltas: Vec<(usize, isize)> = Vec::new();
    let mut old_c = 0usize;
    for (i, line) in block.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let d = if outdent {
            let lead = line.len() - line.trim_start_matches([' ', '\t']).len();
            let kept = outdent_str(&line[..lead]);
            // Indentation is ASCII: bytes == chars.
            let removed = lead - kept.len();
            out.push_str(kept);
            out.push_str(&line[lead..]);
            -(removed as isize)
        } else if line.is_empty() && multi {
            // Blank lines inside an indented block stay blank.
            0
        } else {
            out.push_str(INDENT);
            out.push_str(line);
            INDENT.len() as isize
        };
        deltas.push((old_c, d));
        old_c += line.chars().count() + 1;
    }
    if out == block {
        // Nothing to outdent: still swallow Shift+Tab (never insert a tab).
        return Some(Edit {
            replace: sel.clone(),
            insert: text_slice_chars(text, sel.clone()),
            select: sel,
        });
    }
    let map = |pos: usize| -> usize {
        let rel = pos - first_c;
        let mut shift: isize = 0;
        for &(start, d) in &deltas {
            if start > rel {
                break;
            }
            if d < 0 {
                // Positions inside removed indentation clamp to the line start.
                let within = (rel - start) as isize;
                shift += d.max(-within);
            } else {
                shift += d;
            }
        }
        (pos as isize + shift) as usize
    };
    let (a, b) = (map(sel.start), map(sel.end));
    Some(Edit {
        replace: first_c..first_c + block.chars().count(),
        insert: out,
        select: a..b,
    })
}

fn text_slice_chars(text: &str, r: Range<usize>) -> String {
    text.chars().skip(r.start).take(r.end - r.start).collect()
}

/// Ctrl+Enter or a checkbox click: `[ ]` ↔ `[x]`; a bullet becomes a task; a
/// plain line becomes `- [ ] line`.
pub fn toggle_task(text: &str, caret: usize) -> Option<Edit> {
    let b = byte_of(text, caret);
    let ls = line_start(text, b);
    let le = line_end(text, b);
    let line = &text[ls..le];
    let ls_c = char_of(text, ls);
    let (at, remove, insert) = match parse_prefix(line) {
        Some(p) if matches!(p.marker, Marker::Quote(_)) => (p.len, 0, "- [ ] ".to_owned()),
        Some(p) => match p.task {
            Some(checked) => {
                // The "[ ]" / "[x]" ends the prefix.
                let box_at = line[..p.len].rfind('[').unwrap_or(0);
                (box_at, 3, if checked { "[ ]" } else { "[x]" }.to_owned())
            }
            None => (p.len, 0, "[ ] ".to_owned()),
        },
        None => {
            let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
            (indent, 0, "- [ ] ".to_owned())
        }
    };
    let at_c = ls_c + char_of(line, at);
    let added = insert.chars().count() as isize - remove as isize;
    let new_caret = if caret >= at_c + remove {
        (caret as isize + added) as usize
    } else {
        caret
    };
    Some(Edit {
        replace: at_c..at_c + remove,
        insert,
        select: new_caret..new_caret,
    })
}

/// Ctrl+B / Ctrl+I: wrap the selection in `marker` (or unwrap it if already wrapped).
pub fn wrap(text: &str, sel: Range<usize>, marker: &str) -> Edit {
    let m = marker.chars().count();
    let chars: Vec<char> = text.chars().collect();
    let has = |at: usize| -> bool {
        at + m <= chars.len() && chars[at..at + m].iter().copied().eq(marker.chars())
    };
    if sel.start >= m && has(sel.start - m) && has(sel.end) {
        let inner: String = chars[sel.start..sel.end].iter().collect();
        return Edit {
            replace: sel.start - m..sel.end + m,
            insert: inner,
            select: sel.start - m..sel.end - m,
        };
    }
    let inner: String = chars[sel.start..sel.end].iter().collect();
    Edit {
        replace: sel.clone(),
        insert: format!("{marker}{inner}{marker}"),
        select: sel.start + m..sel.end + m,
    }
}

/// The list button: bullets on the selected lines, or off if they all have one.
pub fn toggle_list(text: &str, sel: Range<usize>) -> Edit {
    let first = line_start_char(text, sel.start);
    let chars: Vec<char> = text.chars().collect();
    let last = (sel.end..chars.len())
        .find(|&i| chars[i] == '\n')
        .unwrap_or(chars.len());
    let block: String = chars[first..last].iter().collect();
    let bullet = |line: &str| line.trim_start_matches([' ', '\t']).starts_with("- ");
    let all = block.split('\n').all(bullet);
    let lines: Vec<String> = block
        .split('\n')
        .map(|line| {
            let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
            let (lead, rest) = line.split_at(indent);
            match rest.strip_prefix("- ") {
                Some(item) if all => format!("{lead}{item}"),
                Some(_) => line.to_owned(),
                None => format!("{lead}- {rest}"),
            }
        })
        .collect();
    let insert = lines.join("\n");
    let end = first + insert.chars().count();
    Edit {
        replace: first..last,
        insert,
        select: end..end,
    }
}

/// Ctrl+K: `[selection](|)`, or `[|](url)` when the selection is a URL.
pub fn link(text: &str, sel: Range<usize>) -> Edit {
    let inner = text_slice_chars(text, sel.clone());
    let is_url = inner.starts_with("http://") || inner.starts_with("https://");
    if is_url {
        let caret = sel.start + 1;
        Edit {
            replace: sel,
            insert: format!("[]({inner})"),
            select: caret..caret,
        }
    } else {
        let caret = sel.start + inner.chars().count() + 3;
        Edit {
            replace: sel,
            insert: format!("[{inner}]()"),
            select: caret..caret,
        }
    }
}

/// Apply `e` to `text` (converting char positions to bytes).
pub fn apply(text: &mut String, e: &Edit) {
    let a = byte_of(text, e.replace.start);
    let b = byte_of(text, e.replace.end);
    text.replace_range(a..b, &e.insert);
}

/// Char index of the start of the line containing char `ci`.
pub fn line_start_char(text: &str, ci: usize) -> usize {
    char_of(text, line_start(text, byte_of(text, ci)))
}

#[cfg(test)]
mod list_tests {
    use super::*;

    fn run(text: &str, sel: Range<usize>) -> String {
        let mut text = text.to_owned();
        let edit = toggle_list(&text, sel);
        apply(&mut text, &edit);
        text
    }

    #[test]
    fn a_line_becomes_an_item_and_back() {
        assert_eq!(run("milk", 2..2), "- milk");
        assert_eq!(run("- milk", 3..3), "milk");
    }

    #[test]
    fn every_selected_line_gets_a_bullet() {
        assert_eq!(run("a\n- b\nc", 0..7), "- a\n- b\n- c");
        assert_eq!(run("- a\n- b", 0..7), "a\nb");
    }

    #[test]
    fn indents_stay_and_lines_outside_the_selection_are_left_alone() {
        assert_eq!(run("x\n  y\nz", 3..3), "x\n  - y\nz");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(
        text: &str,
        caret: usize,
        f: impl Fn(&str, Range<usize>) -> Option<Edit>,
    ) -> Option<(String, usize)> {
        let e = f(text, caret..caret)?;
        let mut t = text.to_owned();
        apply(&mut t, &e);
        assert_eq!(e.select.start, e.select.end);
        Some((t, e.select.start))
    }

    fn at_end(s: &str) -> usize {
        s.chars().count()
    }

    #[test]
    fn enter_continues_lists() {
        let t = "- one";
        assert_eq!(run(t, at_end(t), on_enter), Some(("- one\n- ".into(), 8)));
        let t = "  * nested";
        assert_eq!(run(t, at_end(t), on_enter).unwrap().0, "  * nested\n  * ");
        let t = "9) nine";
        assert_eq!(run(t, at_end(t), on_enter).unwrap().0, "9) nine\n10) ");
        let t = "1. a";
        assert_eq!(run(t, at_end(t), on_enter).unwrap().0, "1. a\n2. ");
        let t = "- [x] done";
        assert_eq!(run(t, at_end(t), on_enter).unwrap().0, "- [x] done\n- [ ] ");
        let t = "> quoted";
        assert_eq!(run(t, at_end(t), on_enter).unwrap().0, "> quoted\n> ");
        // Mid-line: the rest moves to the new item.
        assert_eq!(run("- ab", 3, on_enter).unwrap().0, "- a\n- b");
        // Not a list.
        assert_eq!(run("plain", 5, on_enter), None);
        assert_eq!(run("-dash", 5, on_enter), None);
        // Caret inside the marker.
        assert_eq!(run("- a", 1, on_enter), None);
    }

    #[test]
    fn enter_on_empty_items_steps_out() {
        let t = "- a\n- ";
        assert_eq!(run(t, at_end(t), on_enter), Some(("- a\n".into(), 4)));
        let t = "- a\n    - ";
        assert_eq!(run(t, at_end(t), on_enter), Some(("- a\n- ".into(), 6)));
        let t = "- [ ] ";
        assert_eq!(run(t, at_end(t), on_enter), Some((String::new(), 0)));
        let t = "> ";
        assert_eq!(run(t, at_end(t), on_enter), Some((String::new(), 0)));
    }

    #[test]
    fn indenting() {
        let e = indent("- a", 3..3, false).unwrap();
        let mut t = "- a".to_owned();
        apply(&mut t, &e);
        assert_eq!(t, "    - a");
        assert_eq!(e.select, 7..7);
        assert!(indent("plain", 2..2, false).is_none());

        let src = "a\nb\nc";
        let e = indent(src, 0..3, false).unwrap(); // "a\nb" selected
        let mut t = src.to_owned();
        apply(&mut t, &e);
        assert_eq!(t, "    a\n    b\nc");
        assert_eq!(e.select, 4..11);

        let src = "    a\n  b\n\tc";
        let e = indent(src, 0..src.chars().count(), true).unwrap();
        let mut t = src.to_owned();
        apply(&mut t, &e);
        assert_eq!(t, "a\nb\nc");
        assert_eq!(e.select, 0..5);

        // Selection ending at a line start excludes that line.
        let src = "a\nb\n";
        let e = indent(src, 0..2, false).unwrap();
        let mut t = src.to_owned();
        apply(&mut t, &e);
        assert_eq!(t, "    a\nb\n");
    }

    #[test]
    fn tasks() {
        assert_eq!(
            run("- [ ] x", 7, |t, s| toggle_task(t, s.start)).unwrap().0,
            "- [x] x"
        );
        assert_eq!(
            run("- [x] x", 7, |t, s| toggle_task(t, s.start)).unwrap().0,
            "- [ ] x"
        );
        assert_eq!(
            run("- x", 3, |t, s| toggle_task(t, s.start)),
            Some(("- [ ] x".into(), 7))
        );
        assert_eq!(
            run("  x", 3, |t, s| toggle_task(t, s.start)),
            Some(("  - [ ] x".into(), 9))
        );
        assert_eq!(
            run("é\nbuy 🍎", 7, |t, s| toggle_task(t, s.start))
                .unwrap()
                .0,
            "é\n- [ ] buy 🍎"
        );
    }

    #[test]
    fn wrapping_and_links() {
        let mut t = "a bold b".to_owned();
        let e = wrap(&t, 2..6, "**");
        apply(&mut t, &e);
        assert_eq!(t, "a **bold** b");
        assert_eq!(e.select, 4..8);
        let e = wrap(&t, 4..8, "**");
        apply(&mut t, &e);
        assert_eq!(t, "a bold b");
        let mut t = "see docs".to_owned();
        let e = link(&t, 4..8);
        apply(&mut t, &e);
        assert_eq!(t, "see [docs]()");
        assert_eq!(e.select, 11..11);
        let mut t = "https://x.io".to_owned();
        let e = link(&t, 0..12);
        apply(&mut t, &e);
        assert_eq!(t, "[](https://x.io)");
        assert_eq!(e.select, 1..1);
    }
}
