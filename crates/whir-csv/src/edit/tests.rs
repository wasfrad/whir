//! Model test: random edits applied to both the overlay and a plain
//! `Vec<Vec<String>>`, compared after every step (undo/redo included).

use super::*;
use crate::testutil::Rng;

#[derive(Clone, PartialEq, Debug)]
struct Model {
    header: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl Model {
    fn width(&self) -> usize {
        self.header.len()
    }
}

/// The "file": a header and (possibly short) data rows.
struct Base {
    header: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl Base {
    fn value(&self, r: RowKey, c: ColKey) -> String {
        let row = if r.is_header() {
            &self.header
        } else if let Some(i) = r.as_orig() {
            &self.rows[i as usize]
        } else {
            return String::new();
        };
        c.as_orig()
            .and_then(|f| row.get(f))
            .cloned()
            .unwrap_or_default()
    }

    fn model(&self) -> Model {
        let w = self.header.len();
        Model {
            header: self.header.clone(),
            rows: self
                .rows
                .iter()
                .map(|r| {
                    let mut r = r.clone();
                    r.resize(w, String::new());
                    r
                })
                .collect(),
        }
    }
}

fn materialize(e: &Edits, base: &Base, width: usize) -> Model {
    let n = base.rows.len() as u64;
    let cell = |r: RowKey, c: usize| {
        let ck = e.col_key(c);
        e.get(r, ck)
            .map(str::to_owned)
            .unwrap_or_else(|| base.value(r, ck))
    };
    Model {
        header: (0..width).map(|c| cell(RowKey::HEADER, c)).collect(),
        rows: (0..e.logical_len(n))
            .map(|i| {
                let k = e.key(i, n).expect("key in range");
                assert_eq!(e.index_of(k, n), Some(i));
                (0..width).map(|c| cell(k, c)).collect()
            })
            .collect(),
    }
}

/// A cell change, normalized like the document does it.
fn change(base: &Base, r: RowKey, c: ColKey, v: &str) -> (RowKey, ColKey, Option<Arc<str>>) {
    let v = (base.value(r, c) != v).then(|| Arc::from(v));
    (r, c, v)
}

fn sample_base(rng: &mut Rng) -> Base {
    let w = 3;
    let n = 4 + rng.below(6) as usize;
    Base {
        header: (0..w).map(|c| format!("h{c}")).collect(),
        rows: (0..n)
            .map(|r| {
                // Some rows are short (ragged): missing cells read as "".
                let len = if rng.below(4) == 0 { 1 } else { w };
                (0..len).map(|c| format!("{r}.{c}")).collect()
            })
            .collect(),
    }
}

const VALUES: &[&str] = &["", "x", "y", "0.0", "1.1", "h0", "Z"];

#[test]
fn random_edits_match_a_plain_model() {
    for seed in 1..=60 {
        let mut rng = Rng::new(seed);
        let base = sample_base(&mut rng);
        let mut e = Edits::default();
        e.set_base_rows(base.rows.len() as u64);
        let mut model = base.model();
        let mut history: Vec<Model> = Vec::new();
        let mut future: Vec<Model> = Vec::new();
        let mut next_label = 0;

        for step in 0..150 {
            let before = model.clone();
            let n = model.rows.len() as u64;
            let base_n = base.rows.len() as u64;
            let w = model.width();
            let kind = rng.below(10);
            let mut changed = true;
            match kind {
                0..=2 => {
                    // Set a few cells (header included).
                    let mut tx = e.tx("set");
                    let mut changes = Vec::new();
                    for _ in 0..1 + rng.below(3) {
                        if w == 0 {
                            break;
                        }
                        let c = rng.below(w as u64) as usize;
                        let v = VALUES[rng.below(VALUES.len() as u64) as usize];
                        let (r, row): (RowKey, &mut Vec<String>) = if n == 0 || rng.below(6) == 0 {
                            (RowKey::HEADER, &mut model.header)
                        } else {
                            let i = rng.below(n);
                            (tx.e.key(i, base_n).unwrap(), &mut model.rows[i as usize])
                        };
                        row[c] = v.to_owned();
                        changes.push(change(&base, r, tx.e.col_key(c), v));
                    }
                    tx.set_cells(changes);
                    changed = tx.commit(None, None).effects.len() > 0;
                }
                3 => {
                    let at = rng.below(n + 1);
                    let k = 1 + rng.below(3) as u32;
                    let mut tx = e.tx("insert rows");
                    let keys = tx.insert_rows(at, k, None);
                    assert_eq!(keys.len(), k as usize);
                    // Sometimes fill the new rows in the same step (ghost row typing).
                    if w > 0 && rng.below(2) == 0 {
                        let c = rng.below(w as u64) as usize;
                        tx.set_cells([change(&base, keys[0], tx.e.col_key(c), "new")]);
                        let mut row = vec![String::new(); w];
                        row[c] = "new".into();
                        model.rows.insert(at as usize, row);
                        for _ in 1..k {
                            model.rows.insert(at as usize + 1, vec![String::new(); w]);
                        }
                    } else {
                        for _ in 0..k {
                            model.rows.insert(at as usize, vec![String::new(); w]);
                        }
                    }
                    let _ = tx.commit(None, None);
                }
                4 => {
                    if n == 0 {
                        continue;
                    }
                    let mut pos: Vec<u64> = (0..n).filter(|_| rng.below(3) == 0).collect();
                    if pos.is_empty() {
                        pos.push(rng.below(n));
                    }
                    let mut tx = e.tx("delete rows");
                    tx.delete_rows(&pos, None);
                    let _ = tx.commit(None, None);
                    for &p in pos.iter().rev() {
                        model.rows.remove(p as usize);
                    }
                }
                5 => {
                    let at = rng.below(w as u64 + 1) as usize;
                    let k = 1 + rng.below(2) as usize;
                    let mut tx = e.tx("insert cols");
                    let keys = tx.insert_cols(at, k, w);
                    let names: Vec<String> = (0..k)
                        .map(|_| {
                            next_label += 1;
                            format!("C{next_label}")
                        })
                        .collect();
                    tx.set_cells(
                        keys.iter()
                            .zip(&names)
                            .map(|(&ck, name)| change(&base, RowKey::HEADER, ck, name)),
                    );
                    let _ = tx.commit(None, None);
                    for (i, name) in names.iter().enumerate() {
                        model.header.insert(at + i, name.clone());
                        for row in &mut model.rows {
                            row.insert(at + i, String::new());
                        }
                    }
                }
                6 => {
                    if w <= 1 {
                        continue;
                    }
                    let mut pos: Vec<usize> = (0..w).filter(|_| rng.below(3) == 0).collect();
                    if pos.is_empty() || pos.len() == w {
                        pos = vec![rng.below(w as u64) as usize];
                    }
                    let meta = vec![
                        ColMeta {
                            width: 50.0,
                            numeric: false
                        };
                        pos.len()
                    ];
                    let mut tx = e.tx("delete cols");
                    tx.delete_cols(&pos, meta, w);
                    let _ = tx.commit(None, None);
                    for &p in pos.iter().rev() {
                        model.header.remove(p);
                        for row in &mut model.rows {
                            row.remove(p);
                        }
                    }
                }
                7 | 8 => {
                    if let Some(prev) = history.pop() {
                        assert!(e.undo().is_some(), "seed {seed} step {step}: undo");
                        future.push(model.clone());
                        model = prev;
                    } else {
                        assert!(e.undo().is_none());
                    }
                    let got = materialize(&e, &base, model.width());
                    assert_eq!(got, model, "seed {seed} step {step} after undo");
                    continue;
                }
                _ => {
                    if let Some(next) = future.pop() {
                        assert!(e.redo().is_some(), "seed {seed} step {step}: redo");
                        history.push(model.clone());
                        model = next;
                    } else {
                        assert!(e.redo().is_none());
                    }
                    let got = materialize(&e, &base, model.width());
                    assert_eq!(got, model, "seed {seed} step {step} after redo");
                    continue;
                }
            }
            if changed {
                history.push(before);
                future.clear();
            }
            let got = materialize(&e, &base, model.width());
            assert_eq!(got, model, "seed {seed} step {step} kind {kind}");
        }

        // Undo everything: back to the file, and clean.
        while e.undo().is_some() {}
        assert!(!e.is_dirty(), "seed {seed}: clean after undoing everything");
        assert_eq!(materialize(&e, &base, base.header.len()), base.model());
        // Redo everything: the final model again.
        while e.redo().is_some() {}
        while let Some(m) = future.pop() {
            model = m;
        }
        assert_eq!(
            materialize(&e, &base, model.width()),
            model,
            "seed {seed}: redo all"
        );
    }
}

#[test]
fn equal_to_file_value_is_not_an_override() {
    let base = Base {
        header: vec!["a".into()],
        rows: vec![vec!["1".into()]],
    };
    let mut e = Edits::default();
    e.set_base_rows(1);
    let (r, c) = (RowKey::orig(0), ColKey::orig(0));
    let mut tx = e.tx("set");
    tx.set_cells([change(&base, r, c, "2")]);
    let _ = tx.commit(None, None);
    assert!(e.is_dirty());
    let mut tx = e.tx("set back");
    tx.set_cells([change(&base, r, c, "1")]);
    let a = tx.commit(None, None);
    assert_eq!(a.effects, vec![Effect::Cells(vec![(r, c)])]);
    assert!(!e.is_dirty());
    // Setting the same value again records nothing.
    let mut tx = e.tx("noop");
    tx.set_cells([change(&base, r, c, "1")]);
    let a = tx.commit(None, None);
    assert!(a.effects.is_empty());
    assert!(e.can_undo());
    e.undo();
    e.undo();
    assert!(!e.can_undo());
}

#[test]
fn deleted_rows_take_their_overrides_along() {
    let mut e = Edits::default();
    e.set_base_rows(5);
    let mut tx = e.tx("set");
    tx.set_cells([(RowKey::orig(2), ColKey::orig(0), Some("x".into()))]);
    let _ = tx.commit(None, None);
    let sel = SelRange {
        anchor: (2, 0),
        cursor: (2, 1),
    };
    let mut tx = e.tx("delete");
    let removed = tx.delete_rows(&[1, 2], None);
    let a = tx.commit(Some(sel), None);
    assert_eq!(removed, [RowKey::orig(1), RowKey::orig(2)]);
    assert_eq!(
        a.effects,
        vec![Effect::RowsRemoved {
            keys: vec![RowKey::orig(1), RowKey::orig(2)]
        }]
    );
    // The orphaned override moved into the undo step.
    assert!(e.cells().is_empty());
    let u = e.undo().unwrap();
    assert_eq!(u.select, Some(sel));
    assert_eq!(u.label, "delete");
    assert_eq!(e.get(RowKey::orig(2), ColKey::orig(0)), Some("x"));
    assert!(e.rows().is_none(), "back to the identity order");
    e.redo();
    assert!(e.cells().is_empty());
    assert_eq!(e.logical_len(5), 3);
}

#[test]
fn deleted_columns_restore_widths_and_values() {
    let mut e = Edits::default();
    let mut tx = e.tx("set");
    tx.set_cells([(RowKey::orig(0), ColKey::orig(1), Some("v".into()))]);
    let _ = tx.commit(None, None);
    let meta = vec![ColMeta {
        width: 77.0,
        numeric: true,
    }];
    let mut tx = e.tx("delete col");
    tx.delete_cols(&[1], meta.clone(), 3);
    let _ = tx.commit(None, None);
    assert_eq!(e.col_key(1), ColKey::orig(2));
    assert!(e.cells().is_empty());
    let u = e.undo().unwrap();
    assert_eq!(
        u.effects,
        vec![Effect::ColsInserted {
            positions: vec![1],
            meta: Some(meta)
        }]
    );
    assert!(e.cols().is_none());
    assert_eq!(e.get(RowKey::orig(0), ColKey::orig(1)), Some("v"));
}

#[test]
fn snapshots_are_copy_on_write() {
    let mut e = Edits::default();
    let mut tx = e.tx("a");
    tx.set_cells([(RowKey::orig(0), ColKey::orig(0), Some("a".into()))]);
    let _ = tx.commit(None, None);
    let snap = e.snapshot();
    let mut tx = e.tx("b");
    tx.set_cells([(RowKey::orig(0), ColKey::orig(0), Some("b".into()))]);
    let _ = tx.commit(None, None);
    assert_eq!(
        snap.cells
            .get(RowKey::orig(0), ColKey::orig(0))
            .map(|v| &**v),
        Some("a")
    );
    assert_eq!(e.get(RowKey::orig(0), ColKey::orig(0)), Some("b"));
}

#[test]
fn history_budget_drops_the_oldest_steps() {
    let mut e = Edits {
        budget: 1000,
        ..Edits::default()
    };
    let per_step = 300;
    for s in 0..10u32 {
        let mut tx = e.tx("big");
        tx.set_cells((0..per_step).map(|i| {
            (
                RowKey::orig(i as u64),
                ColKey::orig(0),
                Some(Arc::from(s.to_string())),
            )
        }));
        let _ = tx.commit(None, None);
    }
    let mut n = 0;
    while e.undo().is_some() {
        n += 1;
    }
    assert!(n < 10 && n >= 1, "{n} steps kept");
}
