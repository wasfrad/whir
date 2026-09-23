use std::sync::Arc;

use super::*;
use crate::dialect::{sniff, sniff_style};
use crate::edit::{ColKey, ColMeta, EditSnapshot, Edits, RowKey};
use crate::index;
use crate::source::MemSource;
use crate::testutil::{Rng, collect, input_for, parse_all, trim};

/// The format a save of `data` uses (sniffed style, same encoding).
fn fmt_for(data: &[u8], edits: &EditSnapshot) -> OutputFormat {
    let s = sniff(data, None);
    let body = &data[s.bom_len as usize..];
    let style = sniff_style(body, true, data.last().copied(), &s.dialect);
    let idx = index::build_sync(&MemSource::new(data.to_vec()), s.bom_len, s.dialect);
    OutputFormat {
        delimiter: s.dialect.delimiter,
        line_ending: style.line_ending,
        final_newline: style.final_newline,
        quote_all: style.quote_all,
        quoting: !idx.quoting_disabled,
        terminator: s.dialect.terminator,
        out: OutEncoding::keep(s.encoding, s.bom_len > 0),
        rewrite_all: edits.cols.is_some(),
    }
}

fn write(data: &[u8], edits: EditSnapshot, fmt: &OutputFormat) -> (Vec<u8>, Written) {
    let input = input_for(data, edits);
    let (w, out) = write_table(&input, fmt, Vec::new(), &AtomicBool::new(false), |_| {})
        .unwrap()
        .unwrap();
    assert_eq!(w.bytes, out.len() as u64);
    (out, w)
}

/// The writer's own checkpoints equal a fresh index of its output.
fn check_index(out: &[u8], w: &Written, fmt: &OutputFormat) {
    let bom = fmt.out.bom().len() as u64;
    let d = Dialect {
        delimiter: fmt.delimiter,
        terminator: if fmt.line_ending == b"\r" {
            b'\r'
        } else {
            b'\n'
        },
        quoting: fmt.quoting,
    };
    let fresh = index::build_sync(&MemSource::new(out.to_vec()), bom, d);
    assert_eq!(
        fresh.rows,
        w.rows,
        "row count of {:?}",
        String::from_utf8_lossy(out)
    );
    assert_eq!(fresh.checkpoints, w.checkpoints);
}

const CORPUS: &[&[u8]] = &[
    b"a,b\r\n1,2\r\n",
    b"a,b\n1,2",
    b"a,b\r1,2\r",
    b"\xEF\xBB\xBFa,b\r\n1,2\r\n",
    b"h1,h2\r\n\"multi\r\nline\",x\r\n5\"x,y\r\n,,\r\n\r\nragged\r\n1,2,3,4\r\n",
    b"a\r\nb\nc\r\nd",
    b"\"a\"\"b\",c\n\"\",\n",
    b"",
    b"only header",
    b"x;y\n1;2,5\n",
    b"t\tu\n\"q\tq\"\tv\n",
    b"caf\xE9,\x80\r\nna\xEFve,1\r\n",
];

#[test]
fn unedited_files_are_written_byte_for_byte() {
    for data in CORPUS {
        let fmt = fmt_for(data, &Default::default());
        let (out, w) = write(data, Default::default(), &fmt);
        assert_eq!(out, *data, "{:?}", String::from_utf8_lossy(data));
        check_index(&out, &w, &fmt);
    }
}

#[test]
fn only_edited_lines_change() {
    let data = b"id,name\r\n0,a\r\n1,\"b\nc\"\r\n2,c\r\n3,d\r\n".to_vec();
    let mut e = Edits::default();
    e.set_base_rows(4);
    let mut tx = e.tx("edit");
    tx.set_cells([(RowKey::orig(2), ColKey::orig(1), Some("z, y".into()))]);
    let _ = tx.commit(None, None);
    let fmt = fmt_for(&data, &e.snapshot());
    let (out, w) = write(&data, e.snapshot(), &fmt);
    assert_eq!(
        out,
        b"id,name\r\n0,a\r\n1,\"b\nc\"\r\n2,\"z, y\"\r\n3,d\r\n"
    );
    check_index(&out, &w, &fmt);
}

#[test]
fn quoting_style_is_kept() {
    // Quote-all file: edited and new values are quoted too.
    let data = b"\"a\",\"b\"\n\"1\",\"2\"\n".to_vec();
    let mut e = Edits::default();
    e.set_base_rows(1);
    let mut tx = e.tx("edit");
    tx.set_cells([(RowKey::orig(0), ColKey::orig(1), Some("x".into()))]);
    let added = tx.insert_rows(1, 1, None);
    tx.set_cells([(added[0], ColKey::orig(0), Some("n".into()))]);
    let _ = tx.commit(None, None);
    let fmt = fmt_for(&data, &e.snapshot());
    assert!(fmt.quote_all);
    let (out, _) = write(&data, e.snapshot(), &fmt);
    assert_eq!(out, b"\"a\",\"b\"\n\"1\",\"x\"\n\"n\",\"\"\n");

    // Mixed file: fields keep their own style; new values quote only if needed.
    let data = b"a,\"b\"\n1,\"2\"\n".to_vec();
    let mut e = Edits::default();
    e.set_base_rows(1);
    let mut tx = e.tx("edit");
    tx.set_cells([
        (RowKey::orig(0), ColKey::orig(0), Some("x".into())),
        (RowKey::orig(0), ColKey::orig(1), Some("y".into())),
    ]);
    let _ = tx.commit(None, None);
    let fmt = fmt_for(&data, &e.snapshot());
    assert!(!fmt.quote_all);
    let (out, _) = write(&data, e.snapshot(), &fmt);
    assert_eq!(out, b"a,\"b\"\nx,\"y\"\n");
}

#[test]
fn end_of_file_cases() {
    // Last row without a line ending, then an added row: it gets one.
    let data = b"a\n1".to_vec();
    let mut e = Edits::default();
    e.set_base_rows(1);
    let mut tx = e.tx("add");
    let k = tx.insert_rows(1, 1, None);
    tx.set_cells([(k[0], ColKey::orig(0), Some("2".into()))]);
    let _ = tx.commit(None, None);
    let fmt = fmt_for(&data, &e.snapshot());
    let (out, w) = write(&data, e.snapshot(), &fmt);
    assert_eq!(out, b"a\n1\n2");
    check_index(&out, &w, &fmt);

    // An empty added last row still exists (it needs its line ending).
    let data = b"a\n1".to_vec();
    let mut e = Edits::default();
    e.set_base_rows(1);
    let mut tx = e.tx("add");
    tx.insert_rows(1, 1, None);
    let _ = tx.commit(None, None);
    let fmt = fmt_for(&data, &e.snapshot());
    let (out, w) = write(&data, e.snapshot(), &fmt);
    assert_eq!(out, b"a\n1\n\n");
    check_index(&out, &w, &fmt);
    assert_eq!(parse_all(&out, Dialect::default(), 0).len(), 3);

    // An unclosed quote at the end is closed before rows follow it.
    let data = b"a\n\"open".to_vec();
    let mut e = Edits::default();
    e.set_base_rows(1);
    let mut tx = e.tx("add");
    let k = tx.insert_rows(1, 1, None);
    tx.set_cells([(k[0], ColKey::orig(0), Some("z".into()))]);
    let _ = tx.commit(None, None);
    let fmt = fmt_for(&data, &e.snapshot());
    let (out, w) = write(&data, e.snapshot(), &fmt);
    assert_eq!(out, b"a\n\"open\"\nz");
    check_index(&out, &w, &fmt);
}

#[test]
fn windows_1252_edits_and_unicode_fallback() {
    let data = b"name\r\ncaf\xE9\r\n".to_vec();
    let mut e = Edits::default();
    e.set_base_rows(1);
    let mut tx = e.tx("edit");
    let k = tx.insert_rows(1, 1, None);
    tx.set_cells([(k[0], ColKey::orig(0), Some("€ 5".into()))]);
    let _ = tx.commit(None, None);
    let input = input_for(&data, e.snapshot());
    assert!(!needs_unicode(&input));
    let fmt = fmt_for(&data, &e.snapshot());
    assert_eq!(fmt.out, OutEncoding::Cp1252);
    let (out, w) = write(&data, e.snapshot(), &fmt);
    assert_eq!(out, b"name\r\ncaf\xE9\r\n\x80 5\r\n");
    check_index(&out, &w, &fmt);

    // A character Windows-1252 lacks: the whole file becomes UTF-8 + BOM.
    let mut tx = e.tx("edit");
    tx.set_cells([(k[0], ColKey::orig(0), Some("東京".into()))]);
    let _ = tx.commit(None, None);
    let input = input_for(&data, e.snapshot());
    assert!(needs_unicode(&input));
    let fmt = OutputFormat {
        out: OutEncoding::Cp1252ToUtf8Bom,
        ..fmt_for(&data, &e.snapshot())
    };
    let (out, w) = write(&data, e.snapshot(), &fmt);
    assert_eq!(out, "\u{feff}name\r\ncafé\r\n東京\r\n".as_bytes());
    check_index(&out, &w, &fmt);
}

#[test]
fn utf16_output() {
    // A UTF-16 file is read from its UTF-8 copy and encoded back.
    let text = "a,b\r\né,1\r\n";
    for be in [false, true] {
        for bom in [false, true] {
            let mut e = Edits::default();
            e.set_base_rows(1);
            let mut tx = e.tx("edit");
            tx.set_cells([(RowKey::orig(0), ColKey::orig(1), Some("😀".into()))]);
            let _ = tx.commit(None, None);
            let fmt = OutputFormat {
                out: OutEncoding::Utf16 { be, bom },
                ..fmt_for(text.as_bytes(), &e.snapshot())
            };
            let (out, _) = write(text.as_bytes(), e.snapshot(), &fmt);
            let mut want: Vec<u8> = Vec::new();
            let expect = format!("{}a,b\r\né,😀\r\n", if bom { "\u{feff}" } else { "" });
            for u in expect.encode_utf16() {
                want.extend(if be { u.to_be_bytes() } else { u.to_le_bytes() });
            }
            assert_eq!(out, want, "be {be} bom {bom}");
        }
    }
}

#[test]
fn column_changes_write_rectangular_rows() {
    let data = b"a,b\r\n1\r\n2,3,4\r\n".to_vec();
    let mut e = Edits::default();
    e.set_base_rows(2);
    let mut tx = e.tx("cols");
    let k = tx.insert_cols(1, 1, 2);
    tx.set_cells([(RowKey::HEADER, k[0], Some("new".into()))]);
    let _ = tx.commit(None, None);
    let fmt = fmt_for(&data, &e.snapshot());
    assert!(fmt.rewrite_all);
    let (out, w) = write(&data, e.snapshot(), &fmt);
    // Ragged extras keep their place after the mapped columns.
    assert_eq!(out, b"a,new,b\r\n1,,\r\n2,,3,4\r\n");
    check_index(&out, &w, &fmt);

    let mut tx = e.tx("del");
    tx.delete_cols(
        &[0],
        vec![ColMeta {
            width: 1.0,
            numeric: false,
        }],
        3,
    );
    let _ = tx.commit(None, None);
    let fmt = fmt_for(&data, &e.snapshot());
    let (out, _) = write(&data, e.snapshot(), &fmt);
    assert_eq!(out, b"new,b\r\n,\r\n,3,4\r\n");
}

#[test]
fn save_as_tsv_switches_the_delimiter() {
    let data = b"a,b\r\n\"x\ty\",\"1,5\"\r\n".to_vec();
    let fmt = OutputFormat {
        delimiter: b'\t',
        rewrite_all: true,
        ..fmt_for(&data, &Default::default())
    };
    let (out, w) = write(&data, Default::default(), &fmt);
    assert_eq!(out, b"a\tb\r\n\"x\ty\"\t\"1,5\"\r\n");
    check_index(&out, &w, &fmt);
    let d = Dialect {
        delimiter: b'\t',
        ..Dialect::default()
    };
    assert_eq!(
        parse_all(&out, d, 0),
        parse_all(&data, Dialect::default(), 0)
    );
}

#[test]
fn quoting_disabled_writes_verbatim() {
    let data = b"a,b\n\"x,1\n".to_vec();
    let mut e = Edits::default();
    e.set_base_rows(1);
    let mut tx = e.tx("edit");
    tx.set_cells([(RowKey::orig(0), ColKey::orig(1), Some("two\nlines".into()))]);
    let _ = tx.commit(None, None);
    let fmt = OutputFormat {
        quoting: false,
        ..fmt_for(&data, &e.snapshot())
    };
    let mut input = input_for(&data, e.snapshot());
    input.dialect.quoting = false;
    let (_, out) = write_table(&input, &fmt, Vec::new(), &AtomicBool::new(false), |_| {})
        .unwrap()
        .unwrap();
    assert_eq!(out, b"a,b\n\"x,two lines\n");
}

/// Random edits: the output parses back to the logical table and the
/// writer's index matches a fresh one.
#[test]
fn random_edits_round_trip() {
    const VALUES: &[&str] = &[
        "",
        "x",
        "a,b",
        "say \"hi\"",
        "two\nlines",
        "tab\there",
        "é😀",
        " sp ",
        "\"",
        "\r\n",
        "cr\r",
    ];
    let bases: &[&[u8]] = &[
        b"id,name,note\r\n1,a,\"q,1\"\r\n2,b,\"multi\r\nline\"\r\n3\r\n4,d,e,f\r\n",
        b"\xEF\xBB\xBFx;y\n1;2\n3;4",
        b"\"a\",\"b\"\n\"1\",\"2\"\n\"3\",\"4\"\n",
    ];
    for (bi, base) in bases.iter().enumerate() {
        let n0 =
            parse_all(base, sniff(base, None).dialect, sniff(base, None).bom_len).len() as u64 - 1;
        for seed in 1..=60 {
            let mut rng = Rng::new(seed * 31 + bi as u64);
            let mut e = Edits::default();
            e.set_base_rows(n0);
            let mut width = 3usize;
            for _ in 0..8 {
                let n = e.logical_len(n0);
                let mut tx = e.tx("op");
                match rng.below(5) {
                    0 | 1 => {
                        let mut ch = Vec::new();
                        for _ in 0..3 {
                            let r = if n == 0 || rng.below(5) == 0 {
                                RowKey::HEADER
                            } else {
                                tx.edits().key(rng.below(n), n0).unwrap()
                            };
                            let c = tx.edits().col_key(rng.below(width as u64 + 1) as usize);
                            let v = VALUES[rng.below(VALUES.len() as u64) as usize];
                            ch.push((r, c, Some(Arc::from(v))));
                        }
                        tx.set_cells(ch);
                    }
                    2 => {
                        let k = tx.insert_rows(rng.below(n + 1), 1 + rng.below(2) as u32, None);
                        let c = tx.edits().col_key(rng.below(width as u64) as usize);
                        let v = VALUES[rng.below(VALUES.len() as u64) as usize];
                        tx.set_cells([(k[0], c, Some(Arc::from(v)))]);
                    }
                    3 if n > 0 => {
                        tx.delete_rows(&[rng.below(n)], None);
                    }
                    _ => {
                        if rng.below(2) == 0 || width < 2 {
                            tx.insert_cols(rng.below(width as u64 + 1) as usize, 1, width);
                            width += 1;
                        } else {
                            let meta = vec![ColMeta {
                                width: 1.0,
                                numeric: false,
                            }];
                            tx.delete_cols(&[rng.below(width as u64) as usize], meta, width);
                            width -= 1;
                        }
                    }
                }
                let _ = tx.commit(None, None);
            }
            let snap = e.snapshot();
            let fmt = fmt_for(base, &snap);
            let expected: Vec<Vec<String>> = collect(&input_for(base, snap.clone()))
                .into_iter()
                .map(|(_, v)| trim(v))
                .collect();
            let (out, w) = write(base, snap, &fmt);
            check_index(&out, &w, &fmt);
            let d = Dialect {
                delimiter: fmt.delimiter,
                terminator: b'\n',
                quoting: true,
            };
            let got: Vec<Vec<String>> = parse_all(&out, d, fmt.out.bom().len() as u64)
                .into_iter()
                .map(trim)
                .collect();
            assert_eq!(
                got,
                expected,
                "base {bi} seed {seed}: {:?}",
                String::from_utf8_lossy(&out)
            );
        }
    }
}

// --- the save job --------------------------------------------------------------------

fn test_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("whir-csv-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn leftovers(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(whir_core::file::TEMP_PREFIX))
        .collect()
}

fn file_input(path: &Path, edits: EditSnapshot) -> (StreamInput, OutputFormat, FileIdentity) {
    let data = std::fs::read(path).unwrap();
    let src = FileSource::open(path).unwrap();
    let id = src.identity();
    let fmt = fmt_for(&data, &edits);
    let mut input = input_for(&data, edits);
    input.source = src;
    (input, fmt, id)
}

fn job(
    input: StreamInput,
    format: OutputFormat,
    target: SaveTarget,
    expect: Option<FileIdentity>,
) -> SaveJob {
    SaveJob {
        input,
        format,
        target,
        expect,
        overwrite: false,
        cancel: Default::default(),
        progress: Default::default(),
        waker: Waker::noop(),
    }
}

#[test]
fn save_replaces_the_file_and_returns_its_index() {
    let dir = test_dir("save");
    let path = dir.join("t.csv");
    std::fs::write(&path, b"a,b\r\n1,2\r\n").unwrap();
    let mut e = Edits::default();
    e.set_base_rows(1);
    let mut tx = e.tx("edit");
    tx.set_cells([(RowKey::orig(0), ColKey::orig(0), Some("9".into()))]);
    let _ = tx.commit(None, None);
    let (input, fmt, id) = file_input(&path, e.snapshot());
    let j = job(input, fmt, SaveTarget::Path(path.clone()), Some(id));
    let SaveOutcome::Saved(saved) = run(j.format, &j) else {
        panic!("save failed");
    };
    drop(j);
    assert_eq!(std::fs::read(&path).unwrap(), b"a,b\r\n9,2\r\n");
    let fresh = index::build_sync(saved.source.as_deref().unwrap(), 0, Dialect::default());
    let idx = saved.index.unwrap();
    assert_eq!(idx.checkpoints, fresh.checkpoints);
    assert_eq!(idx.rows, fresh.rows);
    assert!(leftovers(&dir).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn changed_on_disk_and_locked_files_are_refused() {
    let dir = test_dir("refuse");
    let path = dir.join("t.csv");
    std::fs::write(&path, b"a\r\n1\r\n").unwrap();
    let (input, fmt, id) = file_input(&path, Default::default());
    // Another program rewrites the file.
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(&path, b"a\r\n1\r\n2\r\n").unwrap();
    let j = job(input.clone(), fmt, SaveTarget::Path(path.clone()), Some(id));
    assert!(matches!(
        run(fmt, &j),
        SaveOutcome::Failed(WriteError::ChangedOnDisk)
    ));
    // "Overwrite" goes ahead.
    let j = SaveJob {
        overwrite: true,
        ..job(input.clone(), fmt, SaveTarget::Path(path.clone()), Some(id))
    };
    assert!(matches!(run(fmt, &j), SaveOutcome::Saved(_)));
    drop(j);
    assert_eq!(std::fs::read(&path).unwrap(), b"a\r\n1\r\n");

    // Excel-style lock: open without sharing delete.
    let excel = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(windows::Win32::Storage::FileSystem::FILE_SHARE_READ.0)
        .open(&path)
        .unwrap();
    let (input, fmt, id) = file_input(&path, Default::default());
    let j = job(input, fmt, SaveTarget::Path(path.clone()), Some(id));
    assert!(matches!(
        run(fmt, &j),
        SaveOutcome::Failed(WriteError::Locked)
    ));
    drop(excel);
    assert!(leftovers(&dir).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cancel_leaves_no_temp_file() {
    let dir = test_dir("cancel");
    let path = dir.join("t.csv");
    std::fs::write(&path, b"a\r\n1\r\n").unwrap();
    let (input, fmt, id) = file_input(&path, Default::default());
    let j = job(input, fmt, SaveTarget::Path(path.clone()), Some(id));
    j.cancel.store(true, Ordering::Relaxed);
    assert!(matches!(run(fmt, &j), SaveOutcome::Cancelled));
    assert!(leftovers(&dir).is_empty());
    assert_eq!(std::fs::read(&path).unwrap(), b"a\r\n1\r\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn library_save_claims_a_free_name() {
    let dir = test_dir("library");
    std::fs::write(dir.join("Untitled table.csv"), b"x").unwrap();
    let data = b"Column 1,Column 2,Column 3\r\n".to_vec();
    let mut e = Edits::default();
    let mut tx = e.tx("edit");
    let k = tx.insert_rows(0, 1, None);
    tx.set_cells([(k[0], ColKey::orig(0), Some("hi".into()))]);
    let _ = tx.commit(None, None);
    let fmt = OutputFormat {
        out: OutEncoding::Utf8 { bom: true },
        ..fmt_for(&data, &e.snapshot())
    };
    let j = job(
        input_for(&data, e.snapshot()),
        fmt,
        SaveTarget::Library {
            dir: dir.clone(),
            stem: "Untitled table".into(),
        },
        None,
    );
    let SaveOutcome::Saved(saved) = run(fmt, &j) else {
        panic!("save failed")
    };
    assert_eq!(saved.path.file_name().unwrap(), "Untitled table (2).csv");
    assert_eq!(saved.header_offset, 3);
    assert_eq!(
        std::fs::read(&saved.path).unwrap(),
        "\u{feff}Column 1,Column 2,Column 3\r\nhi,,\r\n".as_bytes()
    );
    drop(saved);
    let _ = std::fs::remove_dir_all(&dir);
}

/// `cargo test -p whir-csv --release bench_save -- --ignored --nocapture`
#[test]
#[ignore]
fn bench_save() {
    let dir = test_dir("bench-save");
    let path = dir.join("big.csv");
    let mut data = b"id,name,city,amount,note\r\n".to_vec();
    for i in 0..1_500_000u64 {
        data.extend(
            format!(
                "{i},Name {},\"City, {}\",{}.{:02},\"some note text {i}\"\r\n",
                i * 7919 % 100_003,
                i % 977,
                i * 31 % 100_000,
                i % 100
            )
            .as_bytes(),
        );
    }
    std::fs::write(&path, &data).unwrap();
    for (label, rows_too, cols_too) in [
        ("cells", false, false),
        ("cells+rows", true, false),
        ("cells+cols (rewrite)", false, true),
    ] {
        let mut e = Edits::default();
        e.set_base_rows(1_500_000);
        let mut tx = e.tx("edit");
        tx.set_cells((0..2000u64).map(|i| {
            (
                RowKey::orig(i * 700),
                ColKey::orig(1),
                Some(Arc::from("edited")),
            )
        }));
        if rows_too {
            tx.delete_rows(&[10, 500_000, 1_000_000], None);
            tx.insert_rows(700_000, 5, None);
        }
        if cols_too {
            tx.insert_cols(2, 1, 5);
        }
        let _ = tx.commit(None, None);
        let (input, fmt, _) = file_input(&path, e.snapshot());
        let out = dir.join("out.csv");
        let j = job(input, fmt, SaveTarget::Path(out.clone()), None);
        let t = std::time::Instant::now();
        let SaveOutcome::Saved(saved) = run(fmt, &j) else {
            panic!("save failed")
        };
        let dt = t.elapsed();
        let fresh = index::build_sync(saved.source.as_deref().unwrap(), 0, Dialect::default());
        assert_eq!(saved.index.as_ref().unwrap().checkpoints, fresh.checkpoints);
        println!("save {label}: {:?} ({} MB)", dt, data.len() >> 20);
        drop(saved);
        let _ = std::fs::remove_file(&out);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn quoting_off_saves_are_indexed_again() {
    let dir = test_dir("quoting-off");
    let path = dir.join("t.csv");
    std::fs::write(&path, b"a,b\n\"x,1\n2,3\n").unwrap();
    let (mut input, fmt, id) = file_input(&path, Default::default());
    input.dialect.quoting = false;
    let fmt = OutputFormat {
        quoting: false,
        ..fmt
    };
    let j = job(input, fmt, SaveTarget::Path(path.clone()), Some(id));
    let SaveOutcome::Saved(saved) = run(fmt, &j) else {
        panic!("save failed")
    };
    // A fresh index may honour quotes again: ours isn't reused.
    assert!(saved.index.is_none());
    drop(saved);
    let _ = std::fs::remove_dir_all(&dir);
}
