use std::sync::Arc;

use super::*;
use crate::edit::{ColMeta, Edits};
use crate::index::IndexState;
use crate::testutil::{Rng, collect, input_for, parse_all, trim};

/// A CRLF file with quoted multi-line fields and short (ragged) rows.
fn file(n: usize) -> Vec<u8> {
    let mut d = b"id,name,note\r\n".to_vec();
    for i in 0..n {
        let row = if i % 7 == 0 {
            format!("{i},\"n,{i}\",\"multi\nline {i}\"\r\n")
        } else if i % 11 == 0 {
            format!("{i}\r\n")
        } else {
            format!("{i},n{i},x\r\n")
        };
        d.extend(row.as_bytes());
    }
    d
}

/// What the stream should produce, computed through the overlay's own API.
fn expected(e: &Edits, base: &[Vec<String>], width: usize) -> Vec<(RowKey, Vec<String>)> {
    let n = base.len() as u64 - 1;
    let value = |k: RowKey, c: usize| -> String {
        let ck = e.col_key(c);
        if let Some(v) = e.get(k, ck) {
            return v.to_owned();
        }
        let row = if k.is_header() {
            &base[0]
        } else if let Some(r) = k.as_orig() {
            &base[r as usize + 1]
        } else {
            return String::new();
        };
        ck.as_orig()
            .and_then(|f| row.get(f).cloned())
            .unwrap_or_default()
    };
    let mut out = vec![(
        RowKey::HEADER,
        trim((0..width).map(|c| value(RowKey::HEADER, c)).collect()),
    )];
    for i in 0..e.logical_len(n) {
        let k = e.key(i, n).unwrap();
        out.push((k, trim((0..width).map(|c| value(k, c)).collect())));
    }
    out
}

fn trimmed(rows: Vec<(RowKey, Vec<String>)>) -> Vec<(RowKey, Vec<String>)> {
    rows.into_iter().map(|(k, v)| (k, trim(v))).collect()
}

#[test]
fn identity_is_the_file() {
    let data = file(200);
    let base = parse_all(&data, Dialect::default(), 0);
    let got = collect(&input_for(&data, Default::default()));
    assert_eq!(got.len(), base.len());
    assert_eq!(got[0], (RowKey::HEADER, base[0].clone()));
    for (i, (k, v)) in got.iter().enumerate().skip(1) {
        assert_eq!(*k, RowKey::orig(i as u64 - 1));
        assert_eq!(*v, base[i]);
    }
}

#[test]
fn identity_needs_no_index() {
    let data = file(100);
    let mut input = input_for(&data, Default::default());
    input.index = Arc::new(std::sync::RwLock::new(IndexState::default()));
    assert_eq!(collect(&input).len(), 101);
}

#[test]
fn random_edits_match_the_overlay() {
    let data = file(300);
    let base = parse_all(&data, Dialect::default(), 0);
    for seed in 1..=25 {
        let mut rng = Rng::new(seed);
        let mut e = Edits::default();
        e.set_base_rows(base.len() as u64 - 1);
        let mut width = 3usize;
        for step in 0..40 {
            let n = e.logical_len(base.len() as u64 - 1);
            match rng.below(6) {
                0 | 1 => {
                    let mut tx = e.tx("set");
                    let mut ch = Vec::new();
                    for _ in 0..5 {
                        let r = if rng.below(8) == 0 || n == 0 {
                            RowKey::HEADER
                        } else {
                            tx_key(tx_e(&tx), rng.below(n), base.len() as u64 - 1)
                        };
                        let c = rng.below(width as u64 + 1) as usize;
                        ch.push((
                            r,
                            col_key(tx_e(&tx), c),
                            Some(Arc::from(format!("v{step}"))),
                        ));
                    }
                    tx.set_cells(ch);
                    let _ = tx.commit(None, None);
                    width = width.max(1);
                }
                2 => {
                    let mut tx = e.tx("ins");
                    tx.insert_rows(rng.below(n + 1), 1 + rng.below(3) as u32, None);
                    let _ = tx.commit(None, None);
                }
                3 => {
                    if n == 0 {
                        continue;
                    }
                    // Long runs force seeks across checkpoints.
                    let a = rng.below(n);
                    let span = if rng.below(2) == 0 { 150 } else { 3 };
                    let len = 1 + rng.below(span);
                    let pos: Vec<u64> = (a..(a + len).min(n)).collect();
                    let mut tx = e.tx("del");
                    tx.delete_rows(&pos, None);
                    let _ = tx.commit(None, None);
                }
                4 => {
                    let mut tx = e.tx("inscol");
                    tx.insert_cols(rng.below(width as u64 + 1) as usize, 1, width);
                    let _ = tx.commit(None, None);
                    width += 1;
                }
                _ => {
                    if width <= 1 {
                        continue;
                    }
                    let p = rng.below(width as u64) as usize;
                    let meta = vec![ColMeta {
                        width: 1.0,
                        numeric: false,
                    }];
                    let mut tx = e.tx("delcol");
                    tx.delete_cols(&[p], meta, width);
                    let _ = tx.commit(None, None);
                    width -= 1;
                }
            }
            let input = input_for(&data, e.snapshot());
            let got = trimmed(collect(&input));
            let want = expected(&e, &base, width + 4);
            assert_eq!(got.len(), want.len(), "seed {seed} step {step}");
            for (i, (g, w)) in got.iter().zip(&want).enumerate() {
                assert_eq!(g, w, "seed {seed} step {step} row {i}");
            }
        }
    }
}

// The model loop needs the overlay while a transaction is open.
fn tx_e<'a>(tx: &'a crate::edit::Tx<'_>) -> &'a Edits {
    tx.edits()
}

fn tx_key(e: &Edits, i: u64, base: u64) -> RowKey {
    e.key(i, base).unwrap()
}

fn col_key(e: &Edits, c: usize) -> crate::edit::ColKey {
    e.col_key(c)
}

#[test]
fn unchanged_rows_keep_their_raw_bytes() {
    let data = b"a,b\r\n1,\"x\ny\"\r\n2,3\n4,5".to_vec();
    let mut e = Edits::default();
    e.set_base_rows(3);
    let mut tx = e.tx("set");
    tx.set_cells([(
        RowKey::orig(1),
        crate::edit::ColKey::orig(0),
        Some("z".into()),
    )]);
    let _ = tx.commit(None, None);
    let input = input_for(&data, e.snapshot());
    let mut raws = Vec::new();
    for_each(&input, &AtomicBool::new(false), |row, _| {
        raws.push(row.unchanged_raw().map(<[u8]>::to_vec));
        ControlFlow::Continue(())
    })
    .unwrap();
    assert_eq!(
        raws,
        [
            Some(b"a,b\r\n".to_vec()),
            Some(b"1,\"x\ny\"\r\n".to_vec()),
            None,
            Some(b"4,5".to_vec())
        ]
    );
}

#[test]
fn header_is_synthesized_for_an_empty_file() {
    let mut e = Edits::default();
    let mut tx = e.tx("new column");
    let cols = tx.insert_cols(0, 1, 0);
    tx.set_cells([(RowKey::HEADER, cols[0], Some("Column 1".into()))]);
    let rows = tx.insert_rows(0, 1, None);
    tx.set_cells([(rows[0], cols[0], Some("v".into()))]);
    let _ = tx.commit(None, None);
    let got = collect(&input_for(b"", e.snapshot()));
    assert_eq!(
        got,
        [
            (RowKey::HEADER, vec!["Column 1".to_owned()]),
            (rows[0], vec!["v".to_owned()])
        ]
    );
    // Nothing edited: nothing at all.
    assert!(collect(&input_for(b"", Default::default())).is_empty());
}

#[test]
fn cancel_stops_the_pass() {
    let data = file(10_000);
    let input = input_for(&data, Default::default());
    let cancel = AtomicBool::new(true);
    let done = for_each(&input, &cancel, |_, _| ControlFlow::Continue(())).unwrap();
    assert!(!done);
}

#[test]
#[ignore]
fn bench_stream_overhead() {
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
    let input = input_for(&data, Default::default());
    let finder = memchr::memmem::Finder::new(b"city, 5");
    for _ in 0..3 {
        // (a) the bare reader
        let t = std::time::Instant::now();
        let mut reader = RowReader::new(&*input.source, input.dialect, 0);
        let mut f = FieldBuf::default();
        let mut n = 0usize;
        while reader.next_row(&mut f).unwrap().is_some() {
            for field in f.iter() {
                let lowered;
                let hay = if field.iter().any(u8::is_ascii_uppercase) {
                    lowered = field.to_ascii_lowercase();
                    &lowered[..]
                } else {
                    field
                };
                n += finder.find(hay).is_some() as usize;
            }
        }
        let a = t.elapsed();
        // (b) through the stream
        let t = std::time::Instant::now();
        let mut m = 0usize;
        for_each(&input, &AtomicBool::new(false), |row, _| {
            if let Some(f) = row.plain_fields() {
                for field in f.iter() {
                    let lowered;
                    let hay = if field.iter().any(u8::is_ascii_uppercase) {
                        lowered = field.to_ascii_lowercase();
                        &lowered[..]
                    } else {
                        field
                    };
                    m += finder.find(hay).is_some() as usize;
                }
            }
            ControlFlow::Continue(())
        })
        .unwrap();
        println!("reader {:?} ({n})  stream {:?} ({m})", a, t.elapsed());
    }
}
