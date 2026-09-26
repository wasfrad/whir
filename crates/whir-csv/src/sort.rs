//! Background column sort: one pass over the logical rows extracts keys, then
//! a stable sort produces the display order (a [`RowKey`] per row).

use std::cmp::Ordering;
use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::mpsc::Sender;

use whir_core::Waker;

use crate::dialect::Encoding;
use crate::edit::{MAX_EDIT_ROWS, RowKey};
use crate::order::Order;
use crate::parse::encode_cp1252_lossy;
use crate::stream::{self, Field, StreamInput};

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

pub enum SortMsg {
    Done(Result<Order, String>),
}

/// Refuse to hold more than this much key text.
const MAX_KEY_BYTES: usize = 512 << 20;
/// Keys longer than this compare by prefix (then by row order).
const MAX_KEY_LEN: usize = 256;

pub fn spawn(
    input: StreamInput,
    encoding: Encoding,
    spec: SortSpec,
    cancel: std::sync::Arc<AtomicBool>,
    tx: Sender<SortMsg>,
    waker: Waker,
) {
    std::thread::Builder::new()
        .name("csv-sort".into())
        .spawn(move || {
            let result = run(&input, encoding, spec, &cancel);
            if !cancel.load(AtomicOrdering::Relaxed) {
                let _ = tx.send(SortMsg::Done(result));
                waker.wake();
            }
        })
        .expect("spawn csv sort");
}

fn run(
    input: &StreamInput,
    encoding: Encoding,
    spec: SortSpec,
    cancel: &AtomicBool,
) -> Result<Order, String> {
    let mut rows: Vec<RowKey> = Vec::new();
    let mut numbers: Vec<f64> = Vec::new();
    let mut arena: Vec<u8> = Vec::new();
    let mut spans: Vec<(u64, u32)> = Vec::new();
    let (mut numeric, mut non_empty) = (0u64, 0u64);
    let mut text = Vec::new();
    let mut error = None;
    let completed = stream::for_each(input, cancel, |row, _| {
        if row.key.is_header() {
            return ControlFlow::Continue(());
        }
        if rows.len() as u64 >= MAX_EDIT_ROWS {
            error = Some("Too many rows to sort.");
            return ControlFlow::Break(());
        }
        let field: &[u8] = match row.field(spec.column) {
            Field::File(b) => b,
            // Edited text compares like the file's own bytes.
            Field::Text(s) => {
                text.clear();
                if encoding == Encoding::Windows1252 {
                    encode_cp1252_lossy(s, &mut text);
                } else {
                    text.extend_from_slice(s.as_bytes());
                }
                &text
            }
        };
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
            error = Some("This column is too large to sort in memory.");
            return ControlFlow::Break(());
        }
        rows.push(row.key);
        ControlFlow::Continue(())
    })
    .map_err(|e| e.to_string())?;
    if let Some(e) = error {
        return Err(e.into());
    }
    if !completed {
        return Err("cancelled".into());
    }

    let n = rows.len();
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
    drop((numbers, arena, spans));
    let ordered: Vec<RowKey> = perm.into_iter().map(|i| rows[i as usize]).collect();
    Ok(Order::from_keys(Some(spec), ordered, numeric_sort))
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

    fn keys(o: &Order) -> Vec<u64> {
        o.keys().iter().map(|k| k.as_orig().unwrap()).collect()
    }

    fn sort(data: &[u8], column: usize, direction: Direction) -> Order {
        let input = crate::testutil::input_for(data, Default::default());
        let spec = SortSpec { column, direction };
        run(&input, Encoding::Utf8, spec, &AtomicBool::new(false)).unwrap()
    }

    #[test]
    fn sorts_numeric_and_text_columns() {
        let data = b"name,qty
b,10
a,9
c,
d,100
";
        let s = sort(data, 1, Direction::Ascending);
        assert!(s.numeric);
        assert_eq!(keys(&s), [1, 0, 3, 2]); // 9, 10, 100, (empty)
        let s = sort(data, 0, Direction::Descending);
        assert!(!s.numeric);
        assert_eq!(keys(&s), [3, 2, 0, 1]); // d c b a
        assert_eq!(s.display_of(RowKey::orig(3)), Some(0));
        // Descending numbers: the empty cell still sorts last.
        let s = sort(data, 1, Direction::Descending);
        assert_eq!(keys(&s), [3, 0, 1, 2]); // 100, 10, 9, (empty)
    }

    #[test]
    fn sorts_edited_values_and_added_rows() {
        use crate::edit::{ColKey, Edits};
        let data = b"name,qty
b,10
a,9
";
        let mut e = Edits::default();
        e.set_base_rows(2);
        let mut tx = e.tx("edit");
        let added = tx.insert_rows(2, 1, None);
        tx.set_cells([
            (added[0], ColKey::orig(1), Some("5".into())),
            (RowKey::orig(0), ColKey::orig(1), Some("50".into())),
        ]);
        let _ = tx.commit(None, None);
        let input = crate::testutil::input_for(data, e.snapshot());
        let spec = SortSpec {
            column: 1,
            direction: Direction::Ascending,
        };
        let s = run(&input, Encoding::Utf8, spec, &AtomicBool::new(false)).unwrap();
        assert_eq!(s.keys(), [added[0], RowKey::orig(1), RowKey::orig(0)]);
    }
}

#[cfg(test)]
mod bench {
    use super::*;

    /// `cargo test -p whir-csv --release bench_ -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn bench_find_sort() {
        let mut data = b"id,name,city,amount,note\n".to_vec();
        for i in 0..1_500_000u64 {
            data.extend(
                format!(
                    "{i},Name {},\"City, {}\",{}.{:02},\"some note text {i}\"\n",
                    i * 7919 % 100_003,
                    i % 977,
                    i * 31 % 100_000,
                    i % 100
                )
                .as_bytes(),
            );
        }
        let input = crate::testutil::input_for(&data, Default::default());
        let t = std::time::Instant::now();
        let spec = SortSpec {
            column: 1,
            direction: Direction::Ascending,
        };
        let o = run(&input, Encoding::Utf8, spec, &AtomicBool::new(false)).unwrap();
        println!("sort {} rows: {:?}", o.len(), t.elapsed());
        let (tx, rx) = std::sync::mpsc::channel();
        let t = std::time::Instant::now();
        crate::find::spawn(
            input,
            Encoding::Utf8,
            "city, 5".into(),
            Default::default(),
            Default::default(),
            tx,
            Waker::noop(),
        );
        let mut n = 0;
        for m in rx {
            match m {
                crate::find::FindMsg::Matches(v) => n += v.len(),
                crate::find::FindMsg::Done { .. } => break,
            }
        }
        println!(
            "find {n} matches: {:?} ({} MB)",
            t.elapsed(),
            data.len() >> 20
        );
    }
}
