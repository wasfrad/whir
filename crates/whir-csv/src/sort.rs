//! Background column sort: one pass extracts keys, then a stable sort builds a
//! permutation of data rows (4 bytes per row).

use std::cmp::Ordering;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::mpsc::Sender;

use whir_core::Waker;

use crate::dialect::Dialect;
use crate::parse::{FieldBuf, RowReader};
use crate::source::ByteSource;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Ascending,
    Descending,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SortSpec {
    pub column: usize,
    pub direction: Direction,
}

pub struct Sorted {
    pub spec: SortSpec,
    /// Display order → data row (0 = first row after the header).
    pub perm: Vec<u32>,
    /// Data row → display order.
    pub inverse: Vec<u32>,
    /// Compared as numbers (vs. natural text order).
    #[cfg_attr(not(test), allow(dead_code))]
    pub numeric: bool,
}

pub enum SortMsg {
    Done(Result<Sorted, String>),
}

/// Refuse to hold more than this much key text.
const MAX_KEY_BYTES: usize = 512 << 20;
/// Keys longer than this compare by prefix (then by row order).
const MAX_KEY_LEN: usize = 256;

pub fn spawn(
    source: Arc<dyn ByteSource>,
    dialect: Dialect,
    first_row_offset: u64,
    spec: SortSpec,
    cancel: Arc<AtomicBool>,
    tx: Sender<SortMsg>,
    waker: Waker,
) {
    std::thread::Builder::new()
        .name("csv-sort".into())
        .spawn(move || {
            let result = run(&*source, dialect, first_row_offset, spec, &cancel);
            if !cancel.load(AtomicOrdering::Relaxed) {
                let _ = tx.send(SortMsg::Done(result));
                waker.wake();
            }
        })
        .expect("spawn csv sort");
}

fn run(
    src: &dyn ByteSource,
    dialect: Dialect,
    offset: u64,
    spec: SortSpec,
    cancel: &AtomicBool,
) -> Result<Sorted, String> {
    let mut reader = RowReader::new(src, dialect, offset);
    let mut fields = FieldBuf::default();
    // Skip the header.
    reader.next_row(&mut fields).map_err(|e| e.to_string())?;

    let mut numbers: Vec<f64> = Vec::new();
    let mut arena: Vec<u8> = Vec::new();
    let mut spans: Vec<(u64, u32)> = Vec::new();
    let (mut numeric, mut non_empty) = (0u64, 0u64);
    let mut n: u64 = 0;
    while reader
        .next_row(&mut fields)
        .map_err(|e| e.to_string())?
        .is_some()
    {
        if n % 65536 == 0 && cancel.load(AtomicOrdering::Relaxed) {
            return Err("cancelled".into());
        }
        if n >= u32::MAX as u64 {
            return Err("Too many rows to sort.".into());
        }
        let field = fields.get(spec.column).unwrap_or(b"");
        let trimmed = field.trim_ascii();
        let num = parse_number(trimmed);
        if !trimmed.is_empty() {
            non_empty += 1;
            numeric += num.is_some() as u64;
        }
        numbers.push(num.unwrap_or(f64::NAN));
        let key = &trimmed[..trimmed.len().min(MAX_KEY_LEN)];
        spans.push((arena.len() as u64, key.len() as u32));
        arena.extend(key.iter().map(u8::to_ascii_lowercase));
        if arena.len() > MAX_KEY_BYTES {
            return Err("This column is too large to sort in memory.".into());
        }
        n += 1;
    }

    let numeric_sort = non_empty > 0 && numeric * 10 >= non_empty * 9;
    let mut perm: Vec<u32> = (0..n as u32).collect();
    let key = |i: u32| {
        let (s, l) = spans[i as usize];
        &arena[s as usize..s as usize + l as usize]
    };
    // Rows sort in two groups: values first, then "the rest" (empty cells, and
    // text in a numeric column). The rest stays last in both directions; only
    // the comparison within a group flips for descending.
    let group = |i: u32| -> u8 {
        if numeric_sort {
            numbers[i as usize].is_nan() as u8
        } else {
            key(i).is_empty() as u8
        }
    };
    let value_cmp = |a: u32, b: u32| -> Ordering {
        if numeric_sort {
            let (x, y) = (numbers[a as usize], numbers[b as usize]);
            if x.is_nan() || y.is_nan() {
                natural_cmp(key(a), key(b))
            } else {
                x.total_cmp(&y)
            }
        } else {
            natural_cmp(key(a), key(b))
        }
    };
    let descending = spec.direction == Direction::Descending;
    perm.sort_by(|&a, &b| {
        group(a)
            .cmp(&group(b))
            .then_with(|| {
                let v = value_cmp(a, b);
                if descending { v.reverse() } else { v }
            })
            .then(a.cmp(&b))
    });
    if cancel.load(AtomicOrdering::Relaxed) {
        return Err("cancelled".into());
    }
    let mut inverse = vec![0u32; perm.len()];
    for (display, &row) in perm.iter().enumerate() {
        inverse[row as usize] = display as u32;
    }
    Ok(Sorted {
        spec,
        perm,
        inverse,
        numeric: numeric_sort,
    })
}

/// "1,234.5", "$12", "-3e4", "45%" → numbers. Anything else → None.
pub fn parse_number(s: &[u8]) -> Option<f64> {
    let s = std::str::from_utf8(s).ok()?.trim();
    if s.is_empty() || s.len() > 64 {
        return None;
    }
    let s = s.strip_prefix(['$', '€', '£']).unwrap_or(s);
    let s = s.strip_suffix('%').unwrap_or(s);
    let (neg, s) = match s.strip_prefix('(').and_then(|x| x.strip_suffix(')')) {
        Some(inner) => (true, inner), // accounting negatives: (12.50)
        None => (false, s),
    };
    if !s.bytes().any(|b| b.is_ascii_digit()) {
        return None;
    }
    let cleaned: String = s
        .chars()
        .filter(|&c| c != ',' && c != '_' && c != ' ')
        .collect();
    let v: f64 = cleaned.parse().ok()?;
    v.is_finite().then_some(if neg { -v } else { v })
}

/// Case-folded bytes compared with digit runs as numbers ("a2" < "a10").
pub fn natural_cmp(a: &[u8], b: &[u8]) -> Ordering {
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i].is_ascii_digit() && b[j].is_ascii_digit() {
            let si = i;
            while i < a.len() && a[i].is_ascii_digit() {
                i += 1;
            }
            let sj = j;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let na = trim_zeros(&a[si..i]);
            let nb = trim_zeros(&b[sj..j]);
            let ord = na.len().cmp(&nb.len()).then_with(|| na.cmp(nb));
            if ord != Ordering::Equal {
                return ord;
            }
        } else {
            let ord = a[i].cmp(&b[j]);
            if ord != Ordering::Equal {
                return ord;
            }
            i += 1;
            j += 1;
        }
    }
    (a.len() - i).cmp(&(b.len() - j))
}

fn trim_zeros(d: &[u8]) -> &[u8] {
    let k = d.iter().position(|&c| c != b'0').unwrap_or(d.len());
    &d[k..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        assert_eq!(parse_number(b"1,234.5"), Some(1234.5));
        assert_eq!(parse_number(b" -3e2 "), Some(-300.0));
        assert_eq!(parse_number(b"$12"), Some(12.0));
        assert_eq!(parse_number(b"45%"), Some(45.0));
        assert_eq!(parse_number(b"(7.25)"), Some(-7.25));
        assert_eq!(parse_number(b"abc"), None);
        assert_eq!(parse_number(b""), None);
        assert_eq!(parse_number(b"2024-01-05"), None);
    }

    #[test]
    fn natural() {
        let mut v: Vec<&[u8]> = vec![b"item10", b"item2", b"item02b", b"alpha", b"item1"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(
            v,
            [&b"alpha"[..], b"item1", b"item2", b"item02b", b"item10"]
        );
    }

    struct Mem(Vec<u8>);
    impl ByteSource for Mem {
        fn len(&self) -> u64 {
            self.0.len() as u64
        }
        fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
            let o = (offset as usize).min(self.0.len());
            let n = buf.len().min(self.0.len() - o);
            buf[..n].copy_from_slice(&self.0[o..o + n]);
            Ok(n)
        }
    }

    #[test]
    fn sorts_numeric_and_text_columns() {
        let data = b"name,qty\nb,10\na,9\nc,\nd,100\n".to_vec();
        let src = Mem(data);
        let spec = SortSpec {
            column: 1,
            direction: Direction::Ascending,
        };
        let s = run(&src, Dialect::default(), 0, spec, &AtomicBool::new(false)).unwrap();
        assert!(s.numeric);
        assert_eq!(s.perm, [1, 0, 3, 2]); // 9, 10, 100, (empty)
        let spec = SortSpec {
            column: 0,
            direction: Direction::Descending,
        };
        let s = run(&src, Dialect::default(), 0, spec, &AtomicBool::new(false)).unwrap();
        assert!(!s.numeric);
        assert_eq!(s.perm, [3, 2, 0, 1]); // d c b a
        assert_eq!(s.inverse[3], 0);
        // Descending numbers: the empty cell still sorts last.
        let spec = SortSpec {
            column: 1,
            direction: Direction::Descending,
        };
        let s = run(&src, Dialect::default(), 0, spec, &AtomicBool::new(false)).unwrap();
        assert_eq!(s.perm, [3, 0, 1, 2]); // 100, 10, 9, (empty)
    }
}
