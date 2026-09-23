//! Markdown styled in place (iA Writer style): the source text is never
//! changed, markers stay visible but dimmed, and headings, emphasis, code,
//! quotes, lists, tasks and links get their look.
//!
//! The `LayoutJob` is memoised by content hash, so typing re-scans the text
//! once per change; epaint caches the layout itself per paragraph.

use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::sync::Arc;

use egui::text::{ByteIndex, LayoutJob, LayoutSection, TextFormat};
use egui::{Color32, FontFamily, FontId, Stroke};
use whir_core::theme::{self, Theme};

use crate::editing::{Marker, parse_prefix};

/// Colours and sizes for one theme.
#[derive(Clone, Debug)]
pub struct MdStyle {
    pub body_size: f32,
    pub code_size: f32,
    pub line_height: f32,
    pub text: Color32,
    pub syntax: Color32,
    pub list_marker: Color32,
    pub code_bg: Color32,
    pub quote_text: Color32,
    pub link: Color32,
    pub task_done: Color32,
    /// Identifies these values in the memo key.
    key: u64,
}

impl MdStyle {
    pub fn from_theme(t: &Theme) -> Self {
        let m = t.metrics;
        let mut s = Self {
            body_size: m.note_body_size,
            code_size: m.note_code_size,
            line_height: (m.note_body_size * m.note_line_factor).round(),
            text: t.text,
            syntax: t.md_syntax,
            list_marker: t.md_list_marker,
            code_bg: t.md_code_bg,
            quote_text: t.md_quote_text,
            link: t.md_link,
            task_done: t.md_task_done,
            key: 0,
        };
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for c in [
            s.text,
            s.syntax,
            s.list_marker,
            s.code_bg,
            s.quote_text,
            s.link,
            s.task_done,
        ] {
            c.to_array().hash(&mut h);
        }
        s.body_size.to_bits().hash(&mut h);
        s.code_size.to_bits().hash(&mut h);
        s.key = h.finish();
        s
    }

    fn base(&self) -> TextFormat {
        TextFormat {
            font_id: FontId::new(self.body_size, FontFamily::Proportional),
            color: self.text,
            line_height: Some(self.line_height),
            ..Default::default()
        }
    }

    fn mono(&self) -> TextFormat {
        TextFormat {
            font_id: FontId::new(self.code_size, FontFamily::Monospace),
            color: self.text,
            line_height: Some(self.line_height),
            ..Default::default()
        }
    }

    fn heading_size(&self, level: usize) -> f32 {
        match level {
            1 => 22.0,
            2 => 18.5,
            3 => 16.5,
            _ => self.body_size,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Text,
    Heading(u8),
    Quote,
    /// A ``` / ~~~ fence line.
    Fence,
    /// Inside a fenced block.
    Code,
    Rule,
    /// A task item; `box_at` is the char index of its `[`.
    Task {
        checked: bool,
        box_at: usize,
    },
    List,
}

/// One source line, for painting decorations behind the text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineInfo {
    /// Char range (excluding the newline).
    pub start: usize,
    pub end: usize,
    pub kind: LineKind,
}

/// Memoised styling of one note.
#[derive(Default)]
pub struct MdCache {
    key: u64,
    job: Option<Arc<LayoutJob>>,
    pub lines: Vec<LineInfo>,
    /// (byte range of the link text, URL).
    pub links: Vec<(Range<usize>, String)>,
    /// Bold text is present (the bold face must be loaded).
    pub rich: bool,
}

impl MdCache {
    /// The styled job for `text` (from cache when unchanged).
    pub fn job(&mut self, text: &str, style: &MdStyle, plain: Plain) -> Arc<LayoutJob> {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut h);
        style.key.hash(&mut h);
        (plain as u8).hash(&mut h);
        let key = h.finish();
        if let Some(job) = &self.job
            && self.key == key
        {
            return job.clone();
        }
        let built = match plain {
            Plain::No => build(text, style),
            Plain::Proportional => plain_job(text, style.base()),
            Plain::Monospace => plain_job(text, style.mono()),
        };
        self.key = key;
        self.lines = built.lines;
        self.links = built.links;
        self.rich = built.rich;
        if self.rich {
            theme::request_rich_fonts();
        }
        let job = Arc::new(built.job);
        self.job = Some(job.clone());
        job
    }

    /// The link (URL) under char index `ci`, if any.
    pub fn link_at(&self, text: &str, ci: usize) -> Option<&str> {
        let b = text.char_indices().nth(ci).map_or(text.len(), |(b, _)| b);
        self.links
            .iter()
            .find(|(r, _)| r.contains(&b))
            .map(|(_, u)| u.as_str())
    }
}

/// Styling off (big notes, `.txt` files).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plain {
    No,
    Proportional,
    Monospace,
}

fn plain_job(text: &str, format: TextFormat) -> Built {
    let mut job = LayoutJob::default();
    job.append(text, 0.0, format);
    Built {
        job,
        lines: Vec::new(),
        links: Vec::new(),
        rich: false,
    }
}

struct Built {
    job: LayoutJob,
    lines: Vec<LineInfo>,
    links: Vec<(Range<usize>, String)>,
    rich: bool,
}

/// Collects contiguous sections, merging neighbours with equal formats.
struct Sections {
    out: Vec<LayoutSection>,
}

impl Sections {
    fn push(&mut self, range: Range<usize>, format: TextFormat) {
        if range.is_empty() {
            return;
        }
        if let Some(last) = self.out.last_mut()
            && last.byte_range.end.0 == range.start
            && last.format == format
        {
            last.byte_range.end = ByteIndex(range.end);
            return;
        }
        self.out.push(LayoutSection {
            leading_space: 0.0,
            byte_range: ByteIndex(range.start)..ByteIndex(range.end),
            format,
        });
    }
}

fn dim(f: &TextFormat, color: Color32) -> TextFormat {
    TextFormat {
        color,
        underline: Stroke::NONE,
        strikethrough: Stroke::NONE,
        background: Color32::TRANSPARENT,
        ..f.clone()
    }
}

fn build(text: &str, st: &MdStyle) -> Built {
    let mut sections = Sections { out: Vec::new() };
    let mut lines = Vec::new();
    let mut links = Vec::new();
    let mut rich = false;
    let base = st.base();
    let mono = st.mono();
    let mut fence: Option<(u8, usize)> = None;
    let mut byte = 0usize;
    let mut char_pos = 0usize;

    for raw_line in text.split_inclusive('\n') {
        let line = raw_line.strip_suffix('\n').unwrap_or(raw_line);
        let start = byte;
        let end = start + line.len();
        let line_chars = line.chars().count();
        let info = |kind| LineInfo {
            start: char_pos,
            end: char_pos + line_chars,
            kind,
        };
        let trimmed = line.trim_start_matches(' ');
        let indent = line.len() - trimmed.len();

        let kind = if let Some((fc, flen)) = fence {
            let closes = indent <= 3 && {
                let run = trimmed.bytes().take_while(|&b| b == fc).count();
                run >= flen && trimmed[run..].trim().is_empty()
            };
            if closes {
                fence = None;
                sections.push(start..end, dim(&mono, st.syntax));
                LineKind::Fence
            } else {
                sections.push(start..end, mono.clone());
                LineKind::Code
            }
        } else if let Some((fc, flen)) = fence_open(trimmed).filter(|_| indent <= 3) {
            fence = Some((fc, flen));
            sections.push(start..end, dim(&mono, st.syntax));
            LineKind::Fence
        } else if let Some(level) = heading_level(trimmed).filter(|_| indent <= 3) {
            let size = st.heading_size(level);
            let marker_end = start + indent + level + (trimmed.len() > level) as usize;
            let head = TextFormat {
                font_id: FontId::new(size, theme::family(theme::SEMIBOLD)),
                line_height: Some((size * 1.4).round().max(st.line_height)),
                ..base.clone()
            };
            sections.push(start..marker_end.min(end), dim(&head, st.syntax));
            inline(
                text,
                marker_end.min(end)..end,
                &head,
                st,
                &mut sections,
                &mut links,
                &mut rich,
            );
            LineKind::Heading(level as u8)
        } else if is_rule(line) {
            sections.push(start..end, dim(&base, st.syntax));
            LineKind::Rule
        } else if let Some(p) = parse_prefix(line) {
            let prefix_end = start + p.len;
            match p.marker {
                Marker::Quote(_) => {
                    sections.push(start..prefix_end, dim(&base, st.syntax));
                    let q = TextFormat {
                        color: st.quote_text,
                        italics: true,
                        ..base.clone()
                    };
                    inline(
                        text,
                        prefix_end..end,
                        &q,
                        st,
                        &mut sections,
                        &mut links,
                        &mut rich,
                    );
                    LineKind::Quote
                }
                _ => {
                    let marker_fmt = dim(&base, st.list_marker);
                    match p.task {
                        Some(checked) => {
                            let box_start = start + line[..p.len].rfind('[').unwrap_or(0);
                            sections.push(start..box_start, marker_fmt);
                            // The box is painted as a checkbox; its text is hidden.
                            // (Monospace, so "[ ]" and "[x]" take the same width.)
                            sections.push(box_start..prefix_end, dim(&mono, Color32::TRANSPARENT));
                            let content = if checked {
                                TextFormat {
                                    color: st.task_done,
                                    strikethrough: Stroke::new(1.0, st.task_done),
                                    ..base.clone()
                                }
                            } else {
                                base.clone()
                            };
                            inline(
                                text,
                                prefix_end..end,
                                &content,
                                st,
                                &mut sections,
                                &mut links,
                                &mut rich,
                            );
                            let box_at = char_pos + line[..box_start - start].chars().count();
                            LineKind::Task { checked, box_at }
                        }
                        None => {
                            sections.push(start..prefix_end, marker_fmt);
                            inline(
                                text,
                                prefix_end..end,
                                &base,
                                st,
                                &mut sections,
                                &mut links,
                                &mut rich,
                            );
                            LineKind::List
                        }
                    }
                }
            }
        } else {
            inline(
                text,
                start..end,
                &base,
                st,
                &mut sections,
                &mut links,
                &mut rich,
            );
            LineKind::Text
        };
        lines.push(info(kind));
        // The newline belongs to the line's last format (keeps row heights right).
        if raw_line.len() > line.len() {
            let fmt = match sections.out.last() {
                Some(s) if !line.is_empty() && kind != LineKind::Code => s.format.clone(),
                _ if kind == LineKind::Code => mono.clone(),
                _ => base.clone(),
            };
            sections.push(end..end + 1, fmt);
        }
        byte = start + raw_line.len();
        char_pos += line_chars + (raw_line.len() > line.len()) as usize;
    }
    // Every byte must be covered, even when empty lines produced nothing.
    let mut job = LayoutJob {
        text: text.to_owned(),
        ..Default::default()
    };
    job.sections = fill_gaps(sections.out, text.len(), &base);
    Built {
        job,
        lines,
        links,
        rich,
    }
}

/// Sections covering `0..len` exactly, with `base` in any gaps.
fn fill_gaps(sections: Vec<LayoutSection>, len: usize, base: &TextFormat) -> Vec<LayoutSection> {
    let mut out: Vec<LayoutSection> = Vec::with_capacity(sections.len() + 2);
    let mut at = 0;
    for s in sections {
        if s.byte_range.start.0 > at {
            out.push(LayoutSection {
                leading_space: 0.0,
                byte_range: ByteIndex(at)..s.byte_range.start,
                format: base.clone(),
            });
        }
        at = s.byte_range.end.0;
        out.push(s);
    }
    if at < len || out.is_empty() {
        out.push(LayoutSection {
            leading_space: 0.0,
            byte_range: ByteIndex(at)..ByteIndex(len),
            format: base.clone(),
        });
    }
    out
}

fn fence_open(s: &str) -> Option<(u8, usize)> {
    let c = *s.as_bytes().first()?;
    if c != b'`' && c != b'~' {
        return None;
    }
    let run = s.bytes().take_while(|&b| b == c).count();
    // A backtick fence's info string can't contain backticks.
    (run >= 3 && !(c == b'`' && s[run..].contains('`'))).then_some((c, run))
}

fn heading_level(s: &str) -> Option<usize> {
    let n = s.bytes().take_while(|&b| b == b'#').count();
    ((1..=6).contains(&n) && (s.len() == n || s.as_bytes()[n] == b' ' || s.as_bytes()[n] == b'\t'))
        .then_some(n)
}

fn is_rule(line: &str) -> bool {
    let t = line.trim();
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return false;
    }
    let Some(c) = t.chars().next() else {
        return false;
    };
    matches!(c, '-' | '*' | '_')
        && t.chars().filter(|&x| x == c).count() >= 3
        && t.chars().all(|x| x == c || x == ' ')
}

/// Inline spans inside `range` of `text`.
fn inline(
    text: &str,
    range: Range<usize>,
    base: &TextFormat,
    st: &MdStyle,
    out: &mut Sections,
    links: &mut Vec<(Range<usize>, String)>,
    rich: &mut bool,
) {
    let s = &text[range.clone()];
    let bytes = s.as_bytes();
    let off = range.start;
    let mut i = 0;
    let mut run_start = 0;
    let flush = |out: &mut Sections, from: usize, to: usize| {
        out.push(off + from..off + to, base.clone());
    };
    let syntax = dim(base, st.syntax);
    while i < bytes.len() {
        let c = bytes[i];
        // \x escapes
        if c == b'\\' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_punctuation() {
            flush(out, run_start, i);
            out.push(off + i..off + i + 1, syntax.clone());
            out.push(off + i + 1..off + i + 2, base.clone());
            i += 2;
            run_start = i;
            continue;
        }
        // `code`
        if c == b'`' {
            let n = s[i..].bytes().take_while(|&b| b == b'`').count();
            if let Some(close) = find_backtick_run(s, i + n, n) {
                flush(out, run_start, i);
                // Its own (smaller) line height keeps the glyphs and their
                // background on the text's baseline instead of floating.
                let code = TextFormat {
                    font_id: FontId::new(st.code_size, FontFamily::Monospace),
                    background: st.code_bg,
                    line_height: None,
                    ..base.clone()
                };
                out.push(off + i..off + i + n, dim(&code, st.syntax));
                out.push(off + i + n..off + close, code.clone());
                out.push(off + close..off + close + n, dim(&code, st.syntax));
                i = close + n;
                run_start = i;
                continue;
            }
            i += n;
            continue;
        }
        // [text](url)
        if c == b'['
            && let Some((text_end, url_start, url_end)) = find_link(s, i)
        {
            flush(out, run_start, i);
            let link = TextFormat {
                color: st.link,
                underline: Stroke::new(1.0, st.link.gamma_multiply(0.6)),
                ..base.clone()
            };
            out.push(off + i..off + i + 1, syntax.clone());
            out.push(off + i + 1..off + text_end, link);
            out.push(off + text_end..off + url_end + 1, syntax.clone());
            links.push((
                off + i..off + url_end + 1,
                s[url_start..url_end].trim().to_owned(),
            ));
            i = url_end + 1;
            run_start = i;
            continue;
        }
        // <https://...> and bare URLs
        if (c == b'<' || c == b'h')
            && let Some((a, b, url)) = find_url(s, i)
        {
            flush(out, run_start, i);
            let link = TextFormat {
                color: st.link,
                underline: Stroke::new(1.0, st.link.gamma_multiply(0.6)),
                ..base.clone()
            };
            if c == b'<' {
                out.push(off + i..off + i + 1, syntax.clone());
                out.push(off + i + 1..off + b - 1, link);
                out.push(off + b - 1..off + b, syntax.clone());
            } else {
                out.push(off + a..off + b, link);
            }
            links.push((off + a..off + b, url));
            i = b;
            run_start = i;
            continue;
        }
        // **bold** / __bold__ / ~~strike~~ / *italic* / _italic_
        if matches!(c, b'*' | b'_' | b'~') {
            let n = s[i..].bytes().take_while(|&b| b == c).count();
            let double = n >= 2;
            let m = if c == b'~' {
                if !double {
                    i += n;
                    continue;
                }
                2
            } else if double {
                2
            } else {
                1
            };
            let left_ok = s[i + m..]
                .chars()
                .next()
                .is_some_and(|ch| !ch.is_whitespace());
            let word_ok = c != b'_'
                || s[..i]
                    .chars()
                    .next_back()
                    .is_none_or(|ch| !ch.is_alphanumeric());
            if left_ok
                && word_ok
                && let Some(close) = find_closer(s, i + m, c, m)
            {
                flush(out, run_start, i);
                let inner = match (c, m) {
                    (b'~', _) => TextFormat {
                        strikethrough: Stroke::new(1.0, base.color),
                        ..base.clone()
                    },
                    (_, 2) => {
                        *rich = true;
                        let family = if base.font_id.family == FontFamily::Monospace {
                            theme::family(theme::MONO_BOLD)
                        } else {
                            theme::family(theme::BOLD)
                        };
                        TextFormat {
                            font_id: FontId::new(base.font_id.size, family),
                            ..base.clone()
                        }
                    }
                    _ => TextFormat {
                        italics: true,
                        ..base.clone()
                    },
                };
                out.push(off + i..off + i + m, dim(base, st.syntax));
                inline(text, off + i + m..off + close, &inner, st, out, links, rich);
                out.push(off + close..off + close + m, dim(base, st.syntax));
                i = close + m;
                run_start = i;
                continue;
            }
            i += n;
            continue;
        }
        i += utf8_len(c);
    }
    flush(out, run_start, bytes.len());
}

fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
    .max(1)
}

/// Start of the next run of exactly `n` backticks at or after `from`.
fn find_backtick_run(s: &str, from: usize, n: usize) -> Option<usize> {
    let b = s.as_bytes();
    let mut i = from;
    while i < b.len() {
        if b[i] == b'`' {
            let run = s[i..].bytes().take_while(|&x| x == b'`').count();
            if run == n {
                return Some(i);
            }
            i += run;
        } else {
            i += 1;
        }
    }
    None
}

/// `[text](url)` starting at `i`: (end of text = index of ']', url start, url end = index of ')').
fn find_link(s: &str, i: usize) -> Option<(usize, usize, usize)> {
    let b = s.as_bytes();
    let mut depth = 0;
    let mut j = i;
    let text_end = loop {
        j += 1;
        let c = *b.get(j)?;
        match c {
            b'\\' => j += 1,
            b'[' => depth += 1,
            b']' if depth == 0 => break j,
            b']' => depth -= 1,
            _ => {}
        }
    };
    if b.get(text_end + 1) != Some(&b'(') || text_end == i + 1 {
        return None;
    }
    let url_start = text_end + 2;
    let close = s[url_start..].find(')')? + url_start;
    let url = &s[url_start..close];
    (!url.trim().is_empty() && !url.contains(char::is_whitespace))
        .then_some((text_end, url_start, close))
}

/// `<scheme://...>` or a bare `http(s)://...` at `i`: (start, end, url).
fn find_url(s: &str, i: usize) -> Option<(usize, usize, String)> {
    let is_scheme = |t: &str| t.starts_with("https://") || t.starts_with("http://");
    if s.as_bytes()[i] == b'<' {
        let rest = &s[i + 1..];
        if !is_scheme(rest) {
            return None;
        }
        let close = rest.find('>')?;
        let url = &rest[..close];
        if url.contains(char::is_whitespace) {
            return None;
        }
        return Some((i + 1, i + 1 + close + 1, url.to_owned()));
    }
    let rest = &s[i..];
    if !is_scheme(rest) {
        return None;
    }
    // Must start a word.
    if s[..i]
        .chars()
        .next_back()
        .is_some_and(|c| c.is_alphanumeric())
    {
        return None;
    }
    let mut end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    // Trailing punctuation belongs to the sentence, not the URL.
    while end > 0 && rest[..end].ends_with(['.', ',', ';', ':', '!', '?', ')', '\'', '"']) {
        end -= 1;
    }
    let url = &rest[..end];
    (url.len() > "https://".len()).then(|| (i, i + end, url.to_owned()))
}

/// Closing emphasis run of `m` × `c` after `from` (not preceded by whitespace;
/// `_` must end a word).
fn find_closer(s: &str, from: usize, c: u8, m: usize) -> Option<usize> {
    let b = s.as_bytes();
    let mut i = from;
    while i < b.len() {
        if b[i] == b'`' {
            // Don't close inside a code span.
            let n = s[i..].bytes().take_while(|&x| x == b'`').count();
            match find_backtick_run(s, i + n, n) {
                Some(close) => i = close + n,
                None => i += n,
            }
            continue;
        }
        if b[i] == c {
            let run = s[i..].bytes().take_while(|&x| x == c).count();
            let before_ok = i > from && !s[..i].ends_with(char::is_whitespace);
            let after_ok = c != b'_'
                || s[i + run..]
                    .chars()
                    .next()
                    .is_none_or(|ch| !ch.is_alphanumeric());
            if run >= m && before_ok && after_ok && (m == 2 || run == 1 || run >= 3) {
                return Some(if run > m && m == 1 { i + run - 1 } else { i });
            }
            i += run;
            continue;
        }
        i += utf8_len(b[i]);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style() -> MdStyle {
        MdStyle::from_theme(&Theme::dark())
    }

    fn check_cover(text: &str) -> Built {
        let b = build(text, &style());
        let mut at = 0;
        for s in &b.job.sections {
            assert_eq!(s.byte_range.start.0, at, "gap/overlap in {text:?}");
            assert!(text.is_char_boundary(s.byte_range.start.0));
            at = s.byte_range.end.0;
        }
        assert_eq!(at, text.len(), "coverage of {text:?}");
        b
    }

    /// Format covering byte offset `at`.
    fn fmt_at(b: &Built, at: usize) -> TextFormat {
        b.job
            .sections
            .iter()
            .find(|s| s.byte_range.contains(&ByteIndex(at)))
            .unwrap()
            .format
            .clone()
    }

    #[test]
    fn sections_cover_everything() {
        for t in [
            "",
            "\n",
            "plain",
            "# Head\n\nbody **bold** and *it* `code` [l](http://x) done\n",
            "```rust\nlet a = **b**;\n```\nafter",
            "- [ ] task\n- [x] done\n1. one\n> quote *x*\n---\n",
            "unclosed **bold and `code\n~~~\nopen fence",
            "é **ü** 日本 *语* `😀`",
            "snake_case_word and _real_ italics",
            "\\*not italic\\*",
        ] {
            check_cover(t);
        }
    }

    #[test]
    fn styles() {
        let st = style();
        let b = check_cover("a **bold** `c` snake_case _it_");
        assert_eq!(fmt_at(&b, 5).font_id.family, theme::family(theme::BOLD));
        assert!(b.rich);
        assert_eq!(fmt_at(&b, 12).font_id.family, FontFamily::Monospace);
        // snake_case stays plain
        let i = "a **bold** `c` snake_".len();
        assert!(!fmt_at(&b, i).italics);
        let i = "a **bold** `c` snake_case _".len();
        assert!(fmt_at(&b, i).italics);
        // Markers are dimmed, never removed.
        assert_eq!(fmt_at(&b, 2).color, st.syntax);
        assert_eq!(b.job.text, "a **bold** `c` snake_case _it_");
    }

    #[test]
    fn code_spans_win_over_emphasis() {
        let b = check_cover("`**not bold**`");
        assert!(!b.rich);
        assert_eq!(fmt_at(&b, 4).font_id.family, FontFamily::Monospace);
    }

    #[test]
    fn lines_and_links() {
        let t = "# T\n- [x] done\n```\ncode\n```\n> q\nsee https://example.com/a. ok";
        let b = check_cover(t);
        let kinds: Vec<LineKind> = b.lines.iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            [
                LineKind::Heading(1),
                LineKind::Task {
                    checked: true,
                    box_at: 6
                },
                LineKind::Fence,
                LineKind::Code,
                LineKind::Fence,
                LineKind::Quote,
                LineKind::Text
            ]
        );
        assert_eq!(b.links.len(), 1);
        assert_eq!(b.links[0].1, "https://example.com/a");
        let b = check_cover("[docs](https://d.io) and <http://x.io>");
        assert_eq!(b.links.len(), 2);
        assert_eq!(b.links[0].1, "https://d.io");
        assert_eq!(b.links[1].1, "http://x.io");
    }

    #[test]
    fn unclosed_fence_runs_to_end() {
        let b = check_cover("```\na\n# not a heading\n");
        assert!(b.lines[1..].iter().all(|l| l.kind == LineKind::Code));
    }

    #[test]
    fn heading_sizes() {
        let b = check_cover("# Big\n### Small");
        assert_eq!(fmt_at(&b, 3).font_id.size, 22.0);
        assert_eq!(fmt_at(&b, 11).font_id.size, 16.5);
    }
}
