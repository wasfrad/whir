//! Where a page's characters are, in display space: PDF points from the
//! page's top-left corner, y down — the frame `Layout::sizes` and the tiles
//! use, so a box maps to the screen with one scale and offset.
//!
//! PDFium reports boxes in PDF user space (y up, possibly rotated and
//! offset by the crop box); [`Affine`] converts. Characters are grouped into
//! [`Line`]s for hit-testing and for painting highlights at full line height.

use std::ops::Range;

use crate::text::{is_break, is_combining, is_high_surrogate, is_low_surrogate};

/// An empty box: neutral for [`union`], never hit.
pub const NO_BOX: [f32; 4] = [
    f32::INFINITY,
    f32::INFINITY,
    f32::NEG_INFINITY,
    f32::NEG_INFINITY,
];

#[inline]
pub fn is_empty(r: &[f32; 4]) -> bool {
    // Written so NaN counts as empty.
    !(r[0] <= r[2] && r[1] <= r[3])
}

#[inline]
pub fn union(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [
        a[0].min(b[0]),
        a[1].min(b[1]),
        a[2].max(b[2]),
        a[3].max(b[3]),
    ]
}

/// Page space → display space: `u = a·x + c·y + e`, `v = b·x + d·y + f`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub e: f64,
    pub f: f64,
}

impl Affine {
    /// From the page-space positions of the display's top-left (`p00`),
    /// top-right (`p10`) and bottom-left (`p01`) corners, for a display of
    /// `w`×`h` points. That's what `FPDF_DeviceToPage` tells us, so page
    /// rotation and crop-box offsets come out right without re-deriving
    /// PDFium's display matrix. `None` if the corners are degenerate.
    pub fn from_corners(
        p00: [f64; 2],
        p10: [f64; 2],
        p01: [f64; 2],
        w: f64,
        h: f64,
    ) -> Option<Self> {
        if !(w > 0.0 && h > 0.0) {
            return None;
        }
        // Display → page: P = p00 + u·U + v·V (U, V per display point).
        let (ux, uy) = ((p10[0] - p00[0]) / w, (p10[1] - p00[1]) / w);
        let (vx, vy) = ((p01[0] - p00[0]) / h, (p01[1] - p00[1]) / h);
        let det = ux * vy - vx * uy;
        if !det.is_finite() || det.abs() < 1e-9 {
            return None;
        }
        let (a, c) = (vy / det, -vx / det);
        let (b, d) = (-uy / det, ux / det);
        Some(Self {
            a,
            b,
            c,
            d,
            e: -(a * p00[0] + c * p00[1]),
            f: -(b * p00[0] + d * p00[1]),
        })
    }

    pub fn apply(&self, x: f64, y: f64) -> [f64; 2] {
        [
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        ]
    }

    /// A page-space rect (left, bottom, right, top) as a display box
    /// `[x0, y0, x1, y1]` (the bounds of all four corners).
    pub fn map_rect(&self, l: f64, b: f64, r: f64, t: f64) -> [f32; 4] {
        let mut out = NO_BOX;
        for (x, y) in [(l, b), (r, b), (l, t), (r, t)] {
            let [u, v] = self.apply(x, y);
            out = union(out, [u as f32, v as f32, u as f32, v as f32]);
        }
        out
    }
}

/// Character boxes and lines of one page.
#[derive(Clone, Debug, Default)]
pub struct PageGeom {
    /// One per char index, `[x0, y0, x1, y1]` display points; [`NO_BOX`]
    /// for generated or invisible characters.
    pub boxes: Vec<[f32; 4]>,
    pub lines: Vec<Line>,
}

impl PageGeom {
    /// Heap bytes, for the geometry cache budget.
    pub fn bytes(&self) -> usize {
        self.boxes.capacity() * size_of::<[f32; 4]>() + self.lines.capacity() * size_of::<Line>()
    }

    /// The line whose `[start, next start)` span holds `index`.
    pub fn line_of(&self, index: u32) -> Option<usize> {
        self.lines
            .partition_point(|l| l.start <= index)
            .checked_sub(1)
    }
}

/// A run of characters on one baseline. Lines partition the page's indices:
/// content is `[start, end)`, the line-break characters after it are
/// `[end, next line's start)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Line {
    pub start: u32,
    pub end: u32,
    /// Union of the content boxes (top/bottom trimmed where lines overlap).
    pub rect: [f32; 4],
    /// Characters advance down the page (vertical CJK, 90°-rotated text).
    pub vertical: bool,
    /// Reading direction along the line: +1 left→right / top→bottom, −1 reversed.
    pub dir: i8,
}

/// Extent along the line's reading axis.
#[inline]
fn along(r: &[f32; 4], vertical: bool) -> (f32, f32) {
    if vertical { (r[1], r[3]) } else { (r[0], r[2]) }
}

/// Extent across the line (its height for horizontal text).
#[inline]
fn cross(r: &[f32; 4], vertical: bool) -> (f32, f32) {
    if vertical { (r[0], r[2]) } else { (r[1], r[3]) }
}

/// Distance from `x` to `[a0, a1]` (0 inside).
#[inline]
fn gap(x: f32, a0: f32, a1: f32) -> f32 {
    if x < a0 {
        a0 - x
    } else if x > a1 {
        x - a1
    } else {
        0.0
    }
}

/// Overlap of two intervals relative to the shorter one.
fn overlap_ratio(a0: f32, a1: f32, b0: f32, b1: f32) -> f32 {
    let ov = a1.min(b1) - a0.max(b0);
    let m = (a1 - a0).min(b1 - b0);
    if m <= 1e-3 {
        if ov >= 0.0 { 1.0 } else { 0.0 }
    } else {
        ov / m
    }
}

/// Minimum overlap (relative to the smaller glyph) for two neighbouring
/// characters to share a line. Superscripts overlap their base by ~0.6.
const SAME_LINE: f32 = 0.3;

/// Group characters into lines. PDFium ends lines with generated `\r\n`;
/// we additionally split where consecutive glyphs stop overlapping across
/// the line (PDFium sometimes merges two lines without a break).
pub fn build_lines(chars: &[u16], boxes: &[[f32; 4]]) -> Vec<Line> {
    #[derive(Clone, Copy, PartialEq)]
    enum Axis {
        Unknown,
        H,
        V,
    }
    let n = chars.len().min(boxes.len());
    let mut lines = Vec::new();
    let mut start = 0;
    let mut axis = Axis::Unknown;
    // Compared with the previous glyph, not the line so far, so a drop cap
    // doesn't swallow the lines beside it.
    let mut prev: Option<[f32; 4]> = None;
    let mut i = 0;
    while i < n {
        if is_break(chars[i]) {
            let content_end = i;
            while i < n && is_break(chars[i]) {
                i += 1;
            }
            lines.push(make_line(boxes, start, content_end, axis == Axis::V));
            start = i;
            axis = Axis::Unknown;
            prev = None;
            continue;
        }
        let b = boxes[i];
        if !is_empty(&b) {
            if let Some(pb) = prev {
                let oy = overlap_ratio(pb[1], pb[3], b[1], b[3]);
                let ox = overlap_ratio(pb[0], pb[2], b[0], b[2]);
                let same = match axis {
                    // Overlapping both ways (ligature halves, accents): undecided.
                    Axis::Unknown if oy >= SAME_LINE && ox >= SAME_LINE => true,
                    Axis::Unknown if oy >= SAME_LINE => {
                        axis = Axis::H;
                        true
                    }
                    Axis::Unknown if ox >= SAME_LINE => {
                        axis = Axis::V;
                        true
                    }
                    Axis::Unknown => false,
                    Axis::H => oy >= SAME_LINE,
                    Axis::V => ox >= SAME_LINE,
                };
                if !same {
                    lines.push(make_line(boxes, start, i, axis == Axis::V));
                    start = i;
                    axis = Axis::Unknown;
                }
            }
            prev = Some(b);
        }
        i += 1;
    }
    if start < n {
        lines.push(make_line(boxes, start, n, axis == Axis::V));
    }
    trim_overlaps(&mut lines);
    lines
}

fn make_line(boxes: &[[f32; 4]], start: usize, end: usize, vertical: bool) -> Line {
    let mut rect = NO_BOX;
    let mut first = None;
    let mut last = None;
    for b in &boxes[start..end] {
        if !is_empty(b) {
            rect = union(rect, *b);
            first.get_or_insert(*b);
            last = Some(*b);
        }
    }
    let dir = match (first, last) {
        (Some(f), Some(l)) => {
            let (f0, f1) = along(&f, vertical);
            let (l0, l1) = along(&l, vertical);
            if l0 + l1 < f0 + f1 { -1 } else { 1 }
        }
        _ => 1,
    };
    Line {
        start: start as u32,
        end: end as u32,
        rect,
        vertical,
        dir,
    }
}

/// Consecutive lines whose boxes overlap a little (tall ascents, tight
/// leading) meet in the middle instead, so translucent highlights of a
/// multi-line selection don't darken where they overlap.
fn trim_overlaps(lines: &mut [Line]) {
    for k in 1..lines.len() {
        let (before, after) = lines.split_at_mut(k);
        let (a, b) = (&mut before[k - 1], &mut after[0]);
        if a.vertical || b.vertical || is_empty(&a.rect) || is_empty(&b.rect) {
            continue;
        }
        if a.rect[0] >= b.rect[2] || b.rect[0] >= a.rect[2] {
            continue; // side by side, not stacked
        }
        let (top, bottom) = if a.rect[1] <= b.rect[1] {
            (a, b)
        } else {
            (b, a)
        };
        let overlap = top.rect[3] - bottom.rect[1];
        if overlap <= 0.0 || top.rect[3] >= bottom.rect[3] {
            continue; // apart, or one inside the other's band
        }
        let smaller = (top.rect[3] - top.rect[1]).min(bottom.rect[3] - bottom.rect[1]);
        if overlap < 0.5 * smaller {
            let mid = (top.rect[3] + bottom.rect[1]) / 2.0;
            top.rect[3] = mid;
            bottom.rect[1] = mid;
        }
    }
}

// --- hit-testing ---------------------------------------------------------------

/// What's under a point on a page.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointHit {
    /// Insertion point: a char index in `[line.start, line.end]`.
    pub caret: u32,
    /// The character the point is on or nearest to (for word selection).
    pub char: Option<u32>,
    pub line: u32,
    /// Over the line itself (drives the I-beam cursor).
    pub inside: bool,
    /// Close enough to the line for double/triple-click to select there.
    pub near: bool,
}

/// Hit-test a page-local point (display points).
pub fn hit_test(g: &PageGeom, chars: &[u16], p: [f32; 2]) -> Option<PointHit> {
    let li = pick_line(&g.lines, p)?;
    let line = &g.lines[li];
    let (caret, ch) = caret_in_line(g, chars, line, p);
    let r = line.rect;
    let inside =
        p[0] >= r[0] - 1.0 && p[0] <= r[2] + 1.0 && p[1] >= r[1] - 1.0 && p[1] <= r[3] + 1.0;
    let (c0, c1) = cross(&r, line.vertical);
    let (dx, dy) = (gap(p[0], r[0], r[2]), gap(p[1], r[1], r[3]));
    let near = (dx * dx + dy * dy).sqrt() <= (0.5 * (c1 - c0)).max(4.0);
    Some(PointHit {
        caret,
        char: ch,
        line: li as u32,
        inside,
        near,
    })
}

/// (a) A line whose band (its height) contains the point, nearest along the
/// line — to the right of a short last line, or between two columns;
/// (b) else a line spanning the point horizontally, nearest across — the
/// leading between lines, the top/bottom margin; (c) else the nearest.
fn pick_line(lines: &[Line], p: [f32; 2]) -> Option<usize> {
    let mut band: Option<(f32, f32, usize)> = None;
    let mut span: Option<(f32, usize)> = None;
    let mut any: Option<(f32, usize)> = None;
    for (k, l) in lines.iter().enumerate() {
        if is_empty(&l.rect) {
            continue;
        }
        let (pa, pc) = if l.vertical {
            (p[1], p[0])
        } else {
            (p[0], p[1])
        };
        let (a0, a1) = along(&l.rect, l.vertical);
        let (c0, c1) = cross(&l.rect, l.vertical);
        let (da, dc) = (gap(pa, a0, a1), gap(pc, c0, c1));
        if dc == 0.0 {
            let centre = (pc - (c0 + c1) / 2.0).abs();
            if band.is_none_or(|(bd, bc, _)| da < bd || (da == bd && centre < bc)) {
                band = Some((da, centre, k));
            }
        } else if da == 0.0 && span.is_none_or(|(sd, _)| dc < sd) {
            span = Some((dc, k));
        }
        let d2 = da * da + dc * dc;
        if any.is_none_or(|(ad, _)| d2 < ad) {
            any = Some((d2, k));
        }
    }
    band.map(|b| b.2).or(span.map(|s| s.1)).or(any.map(|a| a.1))
}

/// Runs of consecutive content chars sharing one box (ligatures, surrogate
/// pairs, some CJK encodings): `(first index, count, box)`.
fn clusters(g: &PageGeom, line: &Line) -> Vec<(u32, u32, [f32; 4])> {
    let mut out: Vec<(u32, u32, [f32; 4])> = Vec::new();
    for k in line.start..line.end {
        let b = g.boxes[k as usize];
        if is_empty(&b) {
            continue;
        }
        match out.last_mut() {
            Some(c) if c.0 + c.1 == k && c.2 == b => c.1 += 1,
            _ => out.push((k, 1, b)),
        }
    }
    out
}

fn caret_in_line(g: &PageGeom, chars: &[u16], line: &Line, p: [f32; 2]) -> (u32, Option<u32>) {
    let v = line.vertical;
    let pa = if v { p[1] } else { p[0] };
    let cl = clusters(g, line);
    let (Some(first), Some(last)) = (cl.first(), cl.last()) else {
        return (line.start, None);
    };
    let (first, last) = (first.0, last.0 + last.1 - 1);
    let (la0, la1) = along(&line.rect, v);
    // Beyond the ends: the caret goes to the line's start or end.
    if pa < la0 || pa > la1 {
        let at_start = (pa < la0) == (line.dir >= 0);
        return if at_start {
            (line.start, Some(first))
        } else {
            (line.end, Some(last))
        };
    }
    let centre = |b: &[f32; 4]| {
        let (a0, a1) = along(b, v);
        (a0 + a1) / 2.0
    };
    let mut best = 0;
    let mut best_key = (f32::INFINITY, f32::INFINITY);
    for (ci, c) in cl.iter().enumerate() {
        let (a0, a1) = along(&c.2, v);
        let key = (gap(pa, a0, a1), (centre(&c.2) - pa).abs());
        if key.0 < best_key.0 || (key.0 == best_key.0 && key.1 < best_key.1) {
            best_key = key;
            best = ci;
        }
    }
    let (k0, m, b) = cl[best];
    // Local reading direction: an RTL run inside an LTR line reads backwards.
    let local = {
        let here = centre(&b);
        let d = if best + 1 < cl.len() {
            centre(&cl[best + 1].2) - here
        } else if best > 0 {
            here - centre(&cl[best - 1].2)
        } else {
            0.0
        };
        if d < 0.0 {
            -1
        } else if d > 0.0 {
            1
        } else {
            line.dir
        }
    };
    let (a0, a1) = along(&b, v);
    let w = (a1 - a0).max(1e-6);
    // Position within the cluster in reading order, 0..1; a cluster of m
    // chars splits its box evenly.
    let t = if local >= 0 {
        (pa - a0) / w
    } else {
        (a1 - pa) / w
    }
    .clamp(0.0, 1.0);
    let pos = t * m as f32;
    let j = (pos.floor() as u32).min(m - 1);
    let k = k0 + j;
    let after = pos - j as f32 >= 0.5;
    let caret = if after { k + 1 } else { k };
    (snap_caret(chars, caret, after), Some(k))
}

/// Move a caret off positions that would split a character: between a
/// surrogate pair, or before a combining mark. `forward` says which way.
pub fn snap_caret(chars: &[u16], mut caret: u32, forward: bool) -> u32 {
    let n = chars.len() as u32;
    let splits = |k: u32| {
        k > 0
            && k < n
            && ((is_low_surrogate(chars[k as usize]) && is_high_surrogate(chars[k as usize - 1]))
                || is_combining(chars[k as usize]))
    };
    while splits(caret) {
        if forward {
            caret += 1;
        } else {
            caret -= 1;
        }
    }
    caret
}

// --- highlight rects ---------------------------------------------------------------

/// Box of one char; a cluster's box split evenly in reading order.
fn char_rect(g: &PageGeom, line: &Line, k: u32) -> [f32; 4] {
    const MAX_CLUSTER: u32 = 64;
    let b = g.boxes[k as usize];
    if is_empty(&b) {
        return b;
    }
    let same = |j: u32| g.boxes[j as usize] == b;
    let mut k0 = k;
    while k0 > line.start && k - k0 < MAX_CLUSTER && same(k0 - 1) {
        k0 -= 1;
    }
    let mut k1 = k + 1;
    while k1 < line.end && k1 - k < MAX_CLUSTER && same(k1) {
        k1 += 1;
    }
    let m = (k1 - k0) as f32;
    if m <= 1.0 {
        return b;
    }
    let j = (k - k0) as f32;
    let (a0, a1) = along(&b, line.vertical);
    let w = (a1 - a0) / m;
    let (s0, s1) = if line.dir >= 0 {
        (a0 + j * w, a0 + (j + 1.0) * w)
    } else {
        (a1 - (j + 1.0) * w, a1 - j * w)
    };
    let mut r = b;
    if line.vertical {
        r[1] = s0;
        r[3] = s1;
    } else {
        r[0] = s0;
        r[2] = s1;
    }
    r
}

/// One rect per line covered by `range`, at the line's full height, so a
/// highlight reads as a band rather than ragged glyph boxes.
pub fn range_rects(g: &PageGeom, range: Range<u32>) -> Vec<[f32; 4]> {
    let mut out = Vec::new();
    if range.start >= range.end {
        return out;
    }
    let first = g.lines.partition_point(|l| l.end <= range.start);
    for line in &g.lines[first..] {
        if line.start >= range.end {
            break;
        }
        if is_empty(&line.rect) {
            continue;
        }
        let (s, e) = (range.start.max(line.start), range.end.min(line.end));
        let mut r = NO_BOX;
        for k in s..e {
            r = union(r, char_rect(g, line, k));
        }
        if is_empty(&r) {
            continue;
        }
        let (c0, c1) = cross(&line.rect, line.vertical);
        if line.vertical {
            r[0] = c0;
            r[2] = c1;
        } else {
            r[1] = c0;
            r[3] = c1;
        }
        out.push(r);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f64; 2], b: [f64; 2]) {
        assert!(
            (a[0] - b[0]).abs() < 1e-9 && (a[1] - b[1]).abs() < 1e-9,
            "{a:?} != {b:?}"
        );
    }

    /// Crop box L=10, B=20, R=622, T=812 (612×792 page, offset origin).
    const L: f64 = 10.0;
    const B: f64 = 20.0;
    const R: f64 = 622.0;
    const T: f64 = 812.0;

    #[test]
    fn affine_rotations() {
        let (pw, ph) = (R - L, T - B);
        let x = 100.0;
        let y = 300.0;
        // rot: (display TL, TR, BL in page space), (display w, h), expected (u, v).
        let cases = [
            ((L, T), (R, T), (L, B), (pw, ph), [x - L, T - y]),
            ((L, B), (L, T), (R, B), (ph, pw), [y - B, x - L]),
            ((R, B), (L, B), (R, T), (pw, ph), [R - x, y - B]),
            ((R, T), (R, B), (L, T), (ph, pw), [T - y, R - x]),
        ];
        for (p00, p10, p01, (w, h), want) in cases {
            let a =
                Affine::from_corners([p00.0, p00.1], [p10.0, p10.1], [p01.0, p01.1], w, h).unwrap();
            close(a.apply(x, y), want);
            // The corners land on the display corners.
            close(a.apply(p00.0, p00.1), [0.0, 0.0]);
            close(a.apply(p10.0, p10.1), [w, 0.0]);
            close(a.apply(p01.0, p01.1), [0.0, h]);
        }
    }

    #[test]
    fn map_rect_normalises() {
        // 90°: page rect (l,b,r,t) → display with swapped axes, still x0<x1, y0<y1.
        let a = Affine::from_corners([L, B], [L, T], [R, B], T - B, R - L).unwrap();
        let r = a.map_rect(100.0, 300.0, 110.0, 312.0);
        assert_eq!(r, [280.0, 90.0, 292.0, 100.0]);
        let up = Affine::from_corners([L, T], [R, T], [L, B], R - L, T - B).unwrap();
        assert_eq!(up.map_rect(10.0, 802.0, 16.0, 812.0), [0.0, 0.0, 6.0, 10.0]);
    }

    #[test]
    fn degenerate_affine() {
        assert!(Affine::from_corners([0.0, 0.0], [0.0, 0.0], [0.0, 1.0], 1.0, 1.0).is_none());
        assert!(Affine::from_corners([0.0, 0.0], [1.0, 1.0], [2.0, 2.0], 1.0, 1.0).is_none());
        assert!(Affine::from_corners([0.0, 0.0], [1.0, 0.0], [0.0, 1.0], 0.0, 1.0).is_none());
        assert!(Affine::from_corners([0.0, f64::NAN], [1.0, 0.0], [0.0, 1.0], 1.0, 1.0).is_none());
    }

    const CW: f32 = 6.0;
    const CH: f32 = 10.0;

    /// Lay out text lines: each `(text, x, y)` places chars 6×10 from (x, y);
    /// lines are joined by generated "\r\n" without boxes.
    fn page(lines: &[(&str, f32, f32)]) -> (Vec<u16>, PageGeom) {
        let mut chars = Vec::new();
        let mut boxes = Vec::new();
        for (li, (s, x, y)) in lines.iter().enumerate() {
            if li > 0 {
                chars.extend([0x0D, 0x0A]);
                boxes.extend([NO_BOX, NO_BOX]);
            }
            for (i, u) in s.encode_utf16().enumerate() {
                chars.push(u);
                let x0 = x + i as f32 * CW;
                boxes.push(if u == b' ' as u16 {
                    NO_BOX
                } else {
                    [x0, *y, x0 + CW, y + CH]
                });
            }
        }
        let lines = build_lines(&chars, &boxes);
        (chars, PageGeom { boxes, lines })
    }

    fn partition_ok(lines: &[Line], n: u32) {
        assert_eq!(lines[0].start, 0);
        for w in lines.windows(2) {
            assert!(w[0].start <= w[0].end && w[0].end <= w[1].start);
        }
        assert!(lines.last().unwrap().end <= n);
    }

    #[test]
    fn lines_from_breaks() {
        let (chars, g) = page(&[("abc", 0.0, 0.0), ("de", 0.0, 12.0)]);
        assert_eq!(g.lines.len(), 2);
        assert_eq!((g.lines[0].start, g.lines[0].end), (0, 3));
        assert_eq!((g.lines[1].start, g.lines[1].end), (5, 7));
        assert_eq!(g.lines[0].rect, [0.0, 0.0, 18.0, 10.0]);
        partition_ok(&g.lines, chars.len() as u32);

        // Blank lines (\r\n\r\n) are absorbed into the previous line's break.
        let chars: Vec<u16> = "ab\r\n\r\ncd".encode_utf16().collect();
        let mut boxes = vec![NO_BOX; chars.len()];
        for (k, x) in [(0, 0.0), (1, 6.0)] {
            boxes[k] = [x, 0.0, x + CW, CH];
        }
        for (k, x) in [(6, 0.0), (7, 6.0)] {
            boxes[k] = [x, 24.0, x + CW, 24.0 + CH];
        }
        let lines = build_lines(&chars, &boxes);
        assert_eq!(lines.len(), 2);
        assert_eq!((lines[0].start, lines[0].end, lines[1].start), (0, 2, 6));
        partition_ok(&lines, chars.len() as u32);

        // A page starting with a break keeps the partition from 0.
        let chars: Vec<u16> = "\r\nab".encode_utf16().collect();
        let boxes = [
            NO_BOX,
            NO_BOX,
            [0.0, 0.0, 6.0, 10.0],
            [6.0, 0.0, 12.0, 10.0],
        ];
        let lines = build_lines(&chars, &boxes);
        partition_ok(&lines, 4);
        assert!(is_empty(&lines[0].rect));
        assert_eq!((lines[1].start, lines[1].end), (2, 4));
    }

    #[test]
    fn geometric_split_and_superscript() {
        // Two lines merged by PDFium without a break: split where glyphs stop overlapping.
        let chars: Vec<u16> = "abcd".encode_utf16().collect();
        let boxes = [
            [0.0, 0.0, 6.0, 10.0],
            [6.0, 0.0, 12.0, 10.0],
            [0.0, 20.0, 6.0, 30.0],
            [6.0, 20.0, 12.0, 30.0],
        ];
        let lines = build_lines(&chars, &boxes);
        assert_eq!(lines.len(), 2);
        assert_eq!((lines[0].end, lines[1].start), (2, 2));

        // x² stays on its line: the small raised glyph overlaps by ~60% of its height.
        let chars: Vec<u16> = "x2y".encode_utf16().collect();
        let boxes = [
            [0.0, 10.0, 6.0, 20.0],
            [6.0, 7.0, 10.0, 12.0],
            [10.0, 10.0, 16.0, 20.0],
        ];
        assert_eq!(build_lines(&chars, &boxes).len(), 1);
    }

    #[test]
    fn vertical_lines() {
        let chars: Vec<u16> = "縦書き".encode_utf16().collect();
        let boxes = [
            [100.0, 0.0, 110.0, 10.0],
            [100.0, 10.0, 110.0, 20.0],
            [100.0, 20.0, 110.0, 30.0],
        ];
        let lines = build_lines(&chars, &boxes);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].vertical);
        assert_eq!(lines[0].dir, 1);
        // Hit-testing runs along y.
        let g = PageGeom {
            boxes: boxes.to_vec(),
            lines,
        };
        let h = hit_test(&g, &chars, [105.0, 12.0]).unwrap();
        assert_eq!((h.caret, h.char), (1, Some(1)));
        let h = hit_test(&g, &chars, [105.0, 18.0]).unwrap();
        assert_eq!(h.caret, 2);
        assert!(h.inside);
    }

    #[test]
    fn overlap_trim() {
        // Line boxes 0..12 and 10..22: 2pt overlap (< 50% of 12) → meet at 11.
        let chars: Vec<u16> = "ab\r\ncd".encode_utf16().collect();
        let boxes = [
            [0.0, 0.0, 6.0, 12.0],
            [6.0, 0.0, 12.0, 12.0],
            NO_BOX,
            NO_BOX,
            [0.0, 10.0, 6.0, 22.0],
            [6.0, 10.0, 12.0, 22.0],
        ];
        let lines = build_lines(&chars, &boxes);
        assert_eq!(lines[0].rect[3], 11.0);
        assert_eq!(lines[1].rect[1], 11.0);
        // Side-by-side lines are left alone.
        let boxes = [
            [0.0, 0.0, 6.0, 12.0],
            [6.0, 0.0, 12.0, 12.0],
            NO_BOX,
            NO_BOX,
            [50.0, 10.0, 56.0, 22.0],
            [56.0, 10.0, 62.0, 22.0],
        ];
        let lines = build_lines(&chars, &boxes);
        assert_eq!(lines[0].rect[3], 12.0);
    }

    #[test]
    fn hit_glyph_halves_and_ends() {
        let (chars, g) = page(&[("hello", 10.0, 0.0), ("hi", 10.0, 14.0)]);
        let hit = |x: f32, y: f32| hit_test(&g, &chars, [x, y]).unwrap();
        // 'e' spans 16..22: left half → before it, right half → after it.
        assert_eq!(hit(17.0, 5.0).caret, 1);
        assert_eq!(hit(21.0, 5.0).caret, 2);
        assert_eq!(hit(21.0, 5.0).char, Some(1));
        assert!(hit(21.0, 5.0).inside);
        // Before the start / after the end of a line.
        assert_eq!(hit(2.0, 5.0).caret, 0);
        assert_eq!(hit(200.0, 5.0).caret, 5);
        // Right of the short last line: stays on that line (band rule).
        let h = hit(100.0, 18.0);
        assert_eq!((h.line, h.caret), (1, 9));
        assert!(!h.inside);
        // In the leading between lines: the nearer line.
        assert_eq!(hit(12.0, 11.0).line, 0);
        assert_eq!(hit(12.0, 13.5).line, 1);
        // Far away: still a hit, but not near.
        let h = hit(10.0, 300.0);
        assert_eq!(h.line, 1);
        assert!(!h.near);
        assert!(hit(12.0, 11.0).near);
    }

    #[test]
    fn hit_two_columns() {
        let (chars, g) = page(&[("left", 0.0, 0.0), ("right", 100.0, 0.0)]);
        // Both are in the band: the nearer column wins.
        assert_eq!(hit_test(&g, &chars, [40.0, 5.0]).unwrap().line, 0);
        assert_eq!(hit_test(&g, &chars, [80.0, 5.0]).unwrap().line, 1);
    }

    #[test]
    fn hit_ligature_cluster() {
        // "ffi" as one glyph box 0..18: each char gets a third.
        let chars: Vec<u16> = "ffix".encode_utf16().collect();
        let lig = [0.0, 0.0, 18.0, 10.0];
        let boxes = vec![lig, lig, lig, [18.0, 0.0, 24.0, 10.0]];
        let lines = build_lines(&chars, &boxes);
        let g = PageGeom { boxes, lines };
        let caret = |x: f32| hit_test(&g, &chars, [x, 5.0]).unwrap().caret;
        assert_eq!(caret(1.0), 0);
        assert_eq!(caret(5.0), 1);
        assert_eq!(caret(7.0), 1);
        assert_eq!(caret(11.0), 2);
        assert_eq!(caret(17.0), 3);
        // Highlight rects split the cluster too.
        assert_eq!(range_rects(&g, 1..2), vec![[6.0, 0.0, 12.0, 10.0]]);
    }

    #[test]
    fn caret_never_splits_a_character() {
        // A surrogate pair shares one box; PDFium reports two indices.
        let chars = vec![b'a' as u16, 0xD83D, 0xDE00, b'b' as u16];
        let emoji = [6.0, 0.0, 18.0, 10.0];
        let boxes = vec![[0.0, 0.0, 6.0, 10.0], emoji, emoji, [18.0, 0.0, 24.0, 10.0]];
        let lines = build_lines(&chars, &boxes);
        let g = PageGeom { boxes, lines };
        for x in [7.0, 10.0, 11.0, 13.0, 16.0] {
            let c = hit_test(&g, &chars, [x, 5.0]).unwrap().caret;
            assert!(c == 1 || c == 3, "caret {c} at x={x}");
        }
        // A combining mark stays with its base.
        let chars = vec![b'e' as u16, 0x0301, b'x' as u16];
        assert_eq!(snap_caret(&chars, 1, true), 2);
        assert_eq!(snap_caret(&chars, 1, false), 0);
        assert_eq!(snap_caret(&chars, 3, true), 3);
    }

    #[test]
    fn rtl_run_flips_halves() {
        // Logical order ABC laid out right to left, inside an LTR line "x ABC".
        let chars: Vec<u16> = "xABC".encode_utf16().collect();
        let boxes = vec![
            [0.0, 0.0, 6.0, 10.0],
            [30.0, 0.0, 36.0, 10.0],
            [24.0, 0.0, 30.0, 10.0],
            [18.0, 0.0, 24.0, 10.0],
        ];
        let lines = vec![Line {
            start: 0,
            end: 4,
            rect: [0.0, 0.0, 36.0, 10.0],
            vertical: false,
            dir: 1,
        }];
        let g = PageGeom { boxes, lines };
        // 'B' spans 24..30; reading right to left its leading half is 27..30.
        assert_eq!(hit_test(&g, &chars, [29.0, 5.0]).unwrap().caret, 2);
        assert_eq!(hit_test(&g, &chars, [25.0, 5.0]).unwrap().caret, 3);
        // A wholly RTL line: dir −1, so its ends swap.
        let chars: Vec<u16> = "ABC".encode_utf16().collect();
        let boxes = vec![
            [12.0, 0.0, 18.0, 10.0],
            [6.0, 0.0, 12.0, 10.0],
            [0.0, 0.0, 6.0, 10.0],
        ];
        let lines = build_lines(&chars, &boxes);
        assert_eq!(lines[0].dir, -1);
        let g = PageGeom { boxes, lines };
        assert_eq!(hit_test(&g, &chars, [30.0, 5.0]).unwrap().caret, 0);
        assert_eq!(hit_test(&g, &chars, [-5.0, 5.0]).unwrap().caret, 3);
        assert_eq!(hit_test(&g, &chars, [16.0, 5.0]).unwrap().caret, 0);
        assert_eq!(hit_test(&g, &chars, [13.0, 5.0]).unwrap().caret, 1);
    }

    #[test]
    fn highlight_rects() {
        let (_, g) = page(&[("ab cd", 0.0, 0.0), ("ef", 0.0, 14.0)]);
        // Spans both lines, one rect each, full line height.
        let r = range_rects(&g, 3..9);
        assert_eq!(r, vec![[18.0, 0.0, 30.0, 10.0], [0.0, 14.0, 12.0, 24.0]]);
        // Only the (unboxed) space: nothing to draw.
        assert!(range_rects(&g, 2..3).is_empty());
        // Only the generated break: nothing either.
        assert!(range_rects(&g, 5..7).is_empty());
        assert!(range_rects(&g, 4..4).is_empty());
        // A short glyph still paints at full line height.
        let chars: Vec<u16> = "a.".encode_utf16().collect();
        let boxes = vec![[0.0, 0.0, 6.0, 10.0], [6.0, 8.0, 8.0, 10.0]];
        let lines = build_lines(&chars, &boxes);
        let g = PageGeom { boxes, lines };
        assert_eq!(range_rects(&g, 1..2), vec![[6.0, 0.0, 8.0, 10.0]]);
        assert_eq!(g.line_of(1), Some(0));
    }
}
