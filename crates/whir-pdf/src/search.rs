//! Find in the document: a case-insensitive scan of each page's UTF-16 text,
//! time-sliced on the UI thread as pages' text streams in from the PDF thread.
//!
//! Matching is per page (never across pages), on PDFium's char indices, so a
//! match maps straight to highlight rects. Whitespace in the query matches
//! any run of whitespace (PDFium's generated line breaks included), and a
//! word hyphenated at a line end matches the unhyphenated query.

use std::collections::VecDeque;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::select::TextPos;
use crate::text::{is_ignorable, is_space};

/// Stop counting here ("100,000+").
pub const MAX_MATCHES: usize = 100_000;

/// Case folding for one UTF-16 unit: lowercase when that's a single BMP
/// character, plus a few look-alikes people type differently (curly quotes,
/// non-breaking spaces, fullwidth ASCII). 128 KB, built on first search.
pub fn fold_table() -> &'static [u16; 65536] {
    static FOLD: OnceLock<Box<[u16; 65536]>> = OnceLock::new();
    FOLD.get_or_init(|| {
        let mut t: Box<[u16; 65536]> = vec![0u16; 65536]
            .into_boxed_slice()
            .try_into()
            .expect("65536 units");
        for u in 0..=0xFFFFu32 {
            t[u as usize] = char::from_u32(u)
                .and_then(|c| {
                    let mut lower = c.to_lowercase();
                    match (lower.next(), lower.next()) {
                        (Some(l), None) if (l as u32) <= 0xFFFF => Some(l as u16),
                        _ => None,
                    }
                })
                .unwrap_or(u as u16); // surrogates, multi-char lowercase
        }
        t[0x03C2] = 0x03C3; // final sigma
        for u in [0x2018, 0x2019] {
            t[u] = b'\'' as u16;
        }
        for u in [0x201C, 0x201D] {
            t[u] = b'"' as u16;
        }
        for u in [0x2010, 0x2011] {
            t[u] = b'-' as u16;
        }
        for u in [0xA0, 0x1680, 0x202F, 0x205F, 0x3000]
            .into_iter()
            .chain(0x2000..=0x200A)
        {
            t[u] = b' ' as u16;
        }
        for u in 0xFF01..=0xFF5E {
            t[u] = t[u - 0xFEE0];
        }
        t
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tok {
    /// A folded unit.
    Unit(u16),
    /// One or more whitespace characters.
    Space,
}

/// Fold the query; collapse whitespace runs; drop invisible units. A query
/// of only whitespace compiles to nothing (no search).
pub fn compile(q: &str) -> Vec<Tok> {
    let fold = fold_table();
    let mut out = Vec::new();
    for u in q.encode_utf16() {
        if is_space(u) {
            if out.last() != Some(&Tok::Space) {
                out.push(Tok::Space);
            }
        } else if !is_ignorable(u) {
            out.push(Tok::Unit(fold[u as usize]));
        }
    }
    if out.iter().all(|t| *t == Tok::Space) {
        out.clear();
    }
    out
}

/// A match on one page: `len` char indices from `start`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Match {
    pub start: u32,
    pub len: u32,
}

impl Match {
    pub fn end(&self) -> u32 {
        self.start + self.len
    }
}

/// Append the non-overlapping matches of `q` in `chars`. `soft` are the
/// ranges a match may skip ([`crate::text::soft_ranges`]). Returns false if
/// it stopped because `out` gained `cap` matches.
pub fn find_all(
    chars: &[u16],
    soft: &[(u32, u32)],
    q: &[Tok],
    out: &mut Vec<Match>,
    cap: usize,
) -> bool {
    let Some(&first) = q.first() else {
        return true;
    };
    let fold = fold_table();
    let n = chars.len();
    let mut found = 0;
    let mut i = 0;
    while i < n {
        // Scan for a place the first token can start.
        let skip = match first {
            Tok::Unit(u) => chars[i..].iter().position(|&c| fold[c as usize] == u),
            Tok::Space => chars[i..].iter().position(|&c| is_space(c)),
        };
        let Some(skip) = skip else { break };
        i += skip;
        match match_at(chars, soft, q, i, fold) {
            Some(end) => {
                if found >= cap {
                    return false;
                }
                out.push(Match {
                    start: i as u32,
                    len: (end - i) as u32,
                });
                found += 1;
                i = end.max(i + 1);
            }
            None => {
                i += 1;
                // A leading space token consumes the whole run wherever it
                // starts, so the rest of a failed run fails the same way.
                if first == Tok::Space {
                    while i < n && is_space(chars[i]) {
                        i += 1;
                    }
                }
            }
        }
    }
    true
}

/// End of the match of `q` starting exactly at `start`, if any.
fn match_at(
    chars: &[u16],
    soft: &[(u32, u32)],
    q: &[Tok],
    start: usize,
    fold: &[u16; 65536],
) -> Option<usize> {
    let mut p = start;
    let mut s = soft.partition_point(|r| (r.1 as usize) <= start);
    for (ti, tok) in q.iter().enumerate() {
        match *tok {
            Tok::Unit(u) => loop {
                let c = *chars.get(p)?;
                if fold[c as usize] == u {
                    p += 1;
                    break;
                }
                // Only the first token must match exactly where it starts.
                if ti == 0 {
                    return None;
                }
                while s < soft.len() && (soft[s].1 as usize) <= p {
                    s += 1;
                }
                if let Some(&(r0, _)) = soft.get(s).filter(|r| (r.0 as usize) <= p) {
                    // The line-end hyphen stands for a typed '-' too
                    // (PDFium may report it as 0x02 or 0xFFFE).
                    if u == b'-' as u16 && p == r0 as usize {
                        p += 1;
                        break;
                    }
                    p += 1;
                    continue;
                }
                if is_ignorable(c) {
                    p += 1;
                    continue;
                }
                return None;
            },
            Tok::Space => {
                let mut got = false;
                while let Some(&c) = chars.get(p) {
                    if is_space(c) {
                        got = true;
                    } else if !is_ignorable(c) {
                        break;
                    }
                    p += 1;
                }
                if !got {
                    return None;
                }
            }
        }
    }
    Some(p)
}

/// Results of the current query, filled page by page.
#[derive(Default)]
pub struct Search {
    query: Vec<Tok>,
    /// `None`: not searched yet (its text hasn't arrived).
    pages: Vec<Option<Vec<Match>>>,
    queue: VecDeque<u32>,
    queued: Vec<bool>,
    total: usize,
    truncated: bool,
    /// (page, index into that page's matches).
    current: Option<(u32, u32)>,
    /// Where "first match" is measured from.
    anchor: Option<TextPos>,
    /// Some pages will never arrive (text budget exhausted): stop waiting.
    partial: bool,
}

impl Search {
    /// New query. Pages are searched starting at the anchor's page, wrapping.
    pub fn restart(&mut self, q: &str, anchor: TextPos, n_pages: usize) {
        self.query = compile(q);
        self.pages = vec![None; n_pages];
        self.queue.clear();
        self.queued = vec![false; n_pages];
        self.total = 0;
        self.truncated = false;
        self.current = None;
        self.anchor = None;
        if self.query.is_empty() || n_pages == 0 {
            return;
        }
        let a = anchor.page.min(n_pages as u32 - 1);
        self.anchor = Some(TextPos {
            page: a,
            index: anchor.index,
        });
        for p in (a..n_pages as u32).chain(0..a) {
            self.queue.push_back(p);
            self.queued[p as usize] = true;
        }
    }

    pub fn is_active(&self) -> bool {
        !self.query.is_empty()
    }

    /// A page's text became available.
    pub fn text_arrived(&mut self, page: u32) {
        let p = page as usize;
        if self.is_active() && p < self.pages.len() && self.pages[p].is_none() && !self.queued[p] {
            self.queue.push_back(page);
            self.queued[p] = true;
        }
    }

    /// Pages that haven't arrived yet never will: don't wait for them.
    pub fn set_partial(&mut self, partial: bool) {
        self.partial = partial;
    }

    /// Search queued pages whose text is known until `budget` runs out.
    /// `text(page)` gives the page's units and soft ranges. True if work remains.
    pub fn run<'a>(
        &mut self,
        text: impl Fn(u32) -> Option<(&'a [u16], &'a [(u32, u32)])>,
        budget: Duration,
    ) -> bool {
        let t0 = Instant::now();
        while let Some(p) = self.queue.pop_front() {
            self.queued[p as usize] = false;
            if self.pages[p as usize].is_some() {
                continue;
            }
            // Unknown yet: `text_arrived` queues it again.
            let Some((chars, soft)) = text(p) else {
                continue;
            };
            let mut found = Vec::new();
            let room = MAX_MATCHES.saturating_sub(self.total);
            if !find_all(chars, soft, &self.query, &mut found, room) {
                self.truncated = true;
            }
            self.total += found.len();
            found.shrink_to_fit();
            self.pages[p as usize] = Some(found);
            if t0.elapsed() >= budget {
                break;
            }
        }
        !self.queue.is_empty()
    }

    /// Pick the first match once it's knowable: the first at or after the
    /// anchor, walking pages from the anchor's (wrapping). Waits at the first
    /// page not yet searched, so the pick never changes as results stream in.
    /// Returns the match to reveal, once.
    pub fn settle(&mut self) -> Option<(u32, Match)> {
        if self.current.is_some() || !self.is_active() {
            return None;
        }
        let a = self.anchor?;
        let n = self.pages.len() as u32;
        for step in 0..=n {
            let p = (a.page + step) % n;
            let Some(ms) = &self.pages[p as usize] else {
                if self.partial {
                    continue;
                }
                return None;
            };
            let pick = if step == 0 {
                ms.iter().position(|m| m.start >= a.index)
            } else {
                // Later pages, then the anchor page's earlier matches.
                (!ms.is_empty()).then_some(0)
            };
            if let Some(i) = pick {
                self.current = Some((p, i as u32));
                return Some((p, ms[i]));
            }
        }
        None
    }

    /// Next / previous match, wrapping; pages not searched yet are skipped.
    pub fn step(&mut self, forward: bool) -> Option<(u32, Match)> {
        if self.total == 0 {
            return None;
        }
        let n = self.pages.len() as u32;
        let len = |p: u32| self.pages[p as usize].as_ref().map_or(0, Vec::len) as u32;
        let next = match self.current {
            Some((p, i)) if forward && i + 1 < len(p) => Some((p, i + 1)),
            Some((p, i)) if !forward && i > 0 => Some((p, i - 1)),
            Some((p, _)) => (1..=n)
                .map(|s| {
                    if forward {
                        (p + s) % n
                    } else {
                        (p + n - s) % n
                    }
                })
                .find(|&q| len(q) > 0)
                .map(|q| (q, if forward { 0 } else { len(q) - 1 })),
            None => self.nearest(forward),
        };
        let (p, i) = next?;
        self.current = Some((p, i));
        Some((p, self.pages[p as usize].as_ref()?[i as usize]))
    }

    /// Nearest match after (or before) the anchor, ignoring unsearched pages.
    fn nearest(&self, forward: bool) -> Option<(u32, u32)> {
        let a = self.anchor?;
        let n = self.pages.len() as u32;
        for s in 0..=n {
            let p = if forward {
                (a.page + s) % n
            } else {
                (a.page + n - s % n) % n
            };
            let Some(ms) = &self.pages[p as usize] else {
                continue;
            };
            let i = match (s, forward) {
                (0, true) => ms.iter().position(|m| m.start >= a.index),
                (0, false) => ms.iter().rposition(|m| m.start < a.index),
                (_, true) => (!ms.is_empty()).then_some(0),
                (_, false) => ms.len().checked_sub(1),
            };
            if let Some(i) = i {
                return Some((p, i as u32));
            }
        }
        None
    }

    /// The current match.
    pub fn current(&self) -> Option<(u32, Match)> {
        let (p, i) = self.current?;
        Some((p, *self.pages.get(p as usize)?.as_ref()?.get(i as usize)?))
    }

    /// 0-based position of the current match among all found so far.
    pub fn ordinal(&self) -> Option<usize> {
        let (p, i) = self.current?;
        let before: usize = self.pages[..p as usize]
            .iter()
            .map(|m| m.as_ref().map_or(0, Vec::len))
            .sum();
        Some(before + i as usize)
    }

    pub fn total(&self) -> usize {
        self.total
    }

    /// Stopped counting at [`MAX_MATCHES`].
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// Known pages still waiting to be searched.
    pub fn pending(&self) -> bool {
        !self.queue.is_empty()
    }

    pub fn matches_on(&self, page: u32) -> &[Match] {
        self.pages
            .get(page as usize)
            .and_then(|m| m.as_deref())
            .unwrap_or(&[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::soft_ranges;

    fn u(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    fn find(text: &str, hyphens: &[u32], q: &str) -> Vec<(u32, u32)> {
        let c = u(text);
        let soft = soft_ranges(&c, hyphens);
        let mut out = Vec::new();
        assert!(find_all(&c, &soft, &compile(q), &mut out, usize::MAX));
        out.iter().map(|m| (m.start, m.len)).collect()
    }

    #[test]
    fn compiles() {
        assert_eq!(
            compile("Ab  c"),
            vec![
                Tok::Unit(b'a' as u16),
                Tok::Unit(b'b' as u16),
                Tok::Space,
                Tok::Unit(b'c' as u16)
            ]
        );
        assert_eq!(compile(" \t\u{3000} "), vec![]);
        assert_eq!(compile(""), vec![]);
        assert_eq!(compile("a\u{AD}b"), compile("ab"));
        assert_eq!(compile("ÉΣ"), compile("éσ"));
        assert_eq!(compile("ς"), compile("σ"));
        assert_eq!(compile("don’t"), compile("don't"));
        assert_eq!(compile("ＡＢＣ"), compile("abc"));
        assert_eq!(compile(" x"), vec![Tok::Space, Tok::Unit(b'x' as u16)]);
    }

    #[test]
    fn finds() {
        assert_eq!(
            find("Éclair and éclair", &[], "éCLAIR"),
            vec![(0, 6), (11, 6)]
        );
        // A query space matches any whitespace run, generated breaks included.
        assert_eq!(find("end \r\n next", &[], "end next"), vec![(0, 11)]);
        assert_eq!(find("a\u{A0}b", &[], "a b"), vec![(0, 3)]);
        // Hyphenated at a line end (PDFium's flag), with or without the hyphen typed.
        assert_eq!(find("hyphen-\r\nated", &[6], "hyphenated"), vec![(0, 13)]);
        assert_eq!(
            find("hyphen\u{2}\r\nated", &[6], "hyphenated"),
            vec![(0, 13)]
        );
        assert_eq!(
            find("hyphen\u{2}\r\nated", &[6], "hyphen-ated"),
            vec![(0, 13)]
        );
        assert_eq!(find("hyphen-\r\nated", &[6], "hyphen-ated"), vec![(0, 13)]);
        // …but an unflagged hyphen is a real one.
        assert_eq!(find("hyphen-\r\nated", &[], "hyphenated"), vec![]);
        // Soft hyphens and zero-width units inside a word are skipped.
        assert_eq!(find("co\u{AD}operate", &[], "cooperate"), vec![(0, 10)]);
        assert_eq!(find("co\u{AD}\r\noperate", &[], "cooperate"), vec![(0, 12)]);
        assert_eq!(find("zero\u{200B}width", &[], "zerowidth"), vec![(0, 10)]);
        // Non-overlapping.
        assert_eq!(find("aaaa", &[], "aa"), vec![(0, 2), (2, 2)]);
        assert_eq!(find("aaa", &[], "aa"), vec![(0, 2)]);
        // A leading space matches the whole run once.
        assert_eq!(find("a   b  b", &[], " b"), vec![(1, 4), (5, 3)]);
        assert_eq!(find("abc", &[], "abcd"), vec![]);
        assert_eq!(find("", &[], "a"), vec![]);
        // The match never starts on a skippable unit.
        assert_eq!(find("\u{AD}ab", &[], "ab"), vec![(1, 2)]);
    }

    #[test]
    fn caps() {
        let c = u("xxxxx");
        let mut out = Vec::new();
        assert!(!find_all(&c, &[], &compile("x"), &mut out, 3));
        assert_eq!(out.len(), 3);
        let mut out = Vec::new();
        assert!(find_all(&c, &[], &compile("x"), &mut out, 5));
    }

    struct Pages(Vec<Option<Vec<u16>>>);

    impl Pages {
        fn new(texts: &[Option<&str>]) -> Self {
            Self(texts.iter().map(|t| t.map(u)).collect())
        }

        fn run(&self, s: &mut Search) -> bool {
            s.run(
                |p| self.0[p as usize].as_deref().map(|c| (c, &[][..])),
                Duration::from_secs(60),
            )
        }
    }

    fn pos(page: u32, index: u32) -> TextPos {
        TextPos { page, index }
    }

    #[test]
    fn queue_order_from_anchor() {
        let mut s = Search::default();
        s.restart("x", pos(2, 0), 4);
        assert_eq!(
            s.queue.iter().copied().collect::<Vec<_>>(),
            vec![2, 3, 0, 1]
        );
        // Unknown pages drop out, and come back when their text arrives.
        let mut pages = Pages::new(&[Some("x"), None, Some("xx"), None]);
        assert!(!pages.run(&mut s));
        assert_eq!(s.total(), 3);
        pages.0[1] = Some(u("x"));
        s.text_arrived(1);
        s.text_arrived(1); // duplicate ignored
        s.text_arrived(0); // already searched
        assert_eq!(s.queue.len(), 1);
        pages.run(&mut s);
        assert_eq!(s.total(), 4);
        // Inactive searches ignore arrivals.
        let mut idle = Search::default();
        idle.restart("   ", pos(0, 0), 4);
        assert!(!idle.is_active());
        idle.text_arrived(1);
        assert!(!idle.pending());
    }

    #[test]
    fn settle_waits_then_picks_at_or_after_anchor() {
        let mut s = Search::default();
        // Anchor on page 1 at index 5.
        s.restart("ab", pos(1, 5), 3);
        let mut pages = Pages::new(&[Some("ab"), None, Some("ab")]);
        pages.run(&mut s);
        // Page 1 (the anchor's) isn't searched yet: wait.
        assert_eq!(s.settle(), None);
        assert_eq!(s.total(), 2);
        // Page 1 has a match before the anchor and one after it.
        pages.0[1] = Some(u("ab.....ab"));
        s.text_arrived(1);
        pages.run(&mut s);
        assert_eq!(s.settle(), Some((1, Match { start: 7, len: 2 })));
        assert_eq!(s.ordinal(), Some(2));
        assert_eq!(s.total(), 4);
        // Settled once only.
        assert_eq!(s.settle(), None);

        // Nothing after the anchor on its page: next page; wrap to earlier ones last.
        let mut s = Search::default();
        s.restart("ab", pos(2, 9), 3);
        let pages = Pages::new(&[None, Some("ab"), Some("ab")]);
        pages.run(&mut s);
        // Page 0 comes after page 2 when wrapping and isn't known yet.
        assert_eq!(s.settle(), None);
        let pages = Pages::new(&[Some("no"), Some("ab"), Some("ab")]);
        s.text_arrived(0);
        pages.run(&mut s);
        assert_eq!(s.settle(), Some((1, Match { start: 0, len: 2 })));

        // Only the anchor page's earlier matches exist.
        let mut s = Search::default();
        s.restart("ab", pos(0, 5), 2);
        Pages::new(&[Some("ab"), Some("")]).run(&mut s);
        assert_eq!(s.settle(), Some((0, Match { start: 0, len: 2 })));

        // Partial: missing pages are skipped instead of waited for.
        let mut s = Search::default();
        s.restart("ab", pos(0, 0), 3);
        Pages::new(&[Some(""), None, Some("ab")]).run(&mut s);
        assert_eq!(s.settle(), None);
        s.set_partial(true);
        assert_eq!(s.settle(), Some((2, Match { start: 0, len: 2 })));
    }

    #[test]
    fn current_stays_put_as_results_arrive() {
        let mut s = Search::default();
        s.restart("x", pos(1, 0), 3);
        let mut pages = Pages::new(&[None, Some("x"), None]);
        pages.run(&mut s);
        // Page 2 is unknown, but page 1 already has a match at/after the anchor.
        assert_eq!(s.settle(), Some((1, Match { start: 0, len: 1 })));
        assert_eq!(s.ordinal(), Some(0));
        pages.0[0] = Some(u("xxx"));
        s.text_arrived(0);
        pages.run(&mut s);
        assert_eq!(s.current(), Some((1, Match { start: 0, len: 1 })));
        // The ordinal accounts for earlier pages' new matches.
        assert_eq!(s.ordinal(), Some(3));
        assert_eq!(s.total(), 4);
    }

    #[test]
    fn steps_wrap_and_skip_unsearched() {
        let mut s = Search::default();
        s.restart("x", pos(0, 0), 4);
        Pages::new(&[Some("xx"), None, Some("-"), Some("x")]).run(&mut s);
        assert_eq!(s.settle(), Some((0, Match { start: 0, len: 1 })));
        assert_eq!(s.step(true).map(|m| m.0), Some(0));
        assert_eq!(s.current, Some((0, 1)));
        // Skips unsearched page 1 and empty page 2.
        assert_eq!(s.step(true).map(|m| m.0), Some(3));
        assert_eq!(s.step(true), Some((0, Match { start: 0, len: 1 })));
        assert_eq!(s.step(false).map(|m| m.0), Some(3));
        assert_eq!(s.step(false), Some((0, Match { start: 1, len: 1 })));
        assert_eq!(s.ordinal(), Some(1));

        // Stepping before anything settled starts from the anchor.
        let mut s = Search::default();
        s.restart("x", pos(1, 0), 3);
        Pages::new(&[Some("x"), None, Some("x")]).run(&mut s);
        assert_eq!(s.settle(), None);
        assert_eq!(s.step(true).map(|m| m.0), Some(2));
        let mut s = Search::default();
        s.restart("x", pos(1, 0), 3);
        Pages::new(&[Some("x"), None, Some("x")]).run(&mut s);
        assert_eq!(s.step(false).map(|m| m.0), Some(0));

        // No matches: nothing to step to.
        let mut s = Search::default();
        s.restart("q", pos(0, 0), 1);
        Pages::new(&[Some("x")]).run(&mut s);
        assert_eq!(s.step(true), None);
        assert_eq!(s.settle(), None);
    }

    #[test]
    fn caps_total() {
        let mut s = Search::default();
        s.restart("x", pos(0, 0), 2);
        let big = "x".repeat(MAX_MATCHES - 1);
        let pages = Pages(vec![Some(u(&big)), Some(u("xxx"))]);
        pages.run(&mut s);
        assert_eq!(s.total(), MAX_MATCHES);
        assert!(s.truncated());
    }
}
