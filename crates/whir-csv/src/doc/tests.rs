//! Document-level save flow: own save (swap to the new file, overlay cleared),
//! Save As rebinding / exporting, the library first save.

use super::*;
use crate::edit::{ColKey, RowKey};
use crate::testutil::{open_cx, test_dir, wait_for};

fn opened(path: &Path, dir: &Path) -> CsvDoc {
    let mut doc = CsvDoc::open(path.to_path_buf(), &open_cx(dir));
    wait_for(|| {
        doc.poll();
        doc.loaded().is_some_and(|l| l.table.progress().done)
    });
    doc
}

fn set(doc: &mut CsvDoc, r: RowKey, c: usize, v: &str) {
    let l = doc.loaded().unwrap();
    let base = l.base_rows();
    l.edits.set_base_rows(base);
    let mut tx = l.edits.tx("set");
    tx.set_cells([(r, ColKey::orig(c), Some(v.into()))]);
    let _ = tx.commit(None, None);
}

fn saved(doc: &mut CsvDoc) {
    wait_for(|| {
        doc.poll();
        !doc.is_saving()
    });
    assert_eq!(doc.save_error(), None);
}

/// Text of display row `d`, column `c`, as the grid would show it.
fn cell(doc: &mut CsvDoc, d: u64, c: usize) -> String {
    let l = doc.loaded().unwrap();
    let row = l.view().row(d).unwrap();
    row.cell(c).text.to_owned()
}

#[test]
fn save_writes_and_shows_the_new_file() {
    let dir = test_dir("doc-save");
    let path = dir.join("t.csv");
    std::fs::write(&path, b"a,b\r\n1,2\r\n3,4\r\n").unwrap();
    let mut doc = opened(&path, &dir);
    assert!(
        matches!(doc.save(), SaveResult::Saved),
        "clean: nothing to do"
    );
    assert_eq!(doc.persistence(), Persistence::Explicit);

    set(&mut doc, RowKey::orig(1), 0, "x");
    assert!(doc.is_dirty());
    assert!(matches!(doc.save(), SaveResult::Started));
    assert!(doc.is_saving());
    saved(&mut doc);
    assert!(!doc.is_dirty());
    assert_eq!(std::fs::read(&path).unwrap(), b"a,b\r\n1,2\r\nx,4\r\n");
    // The new file is shown straight away, from the writer's own index.
    let l = doc.loaded().unwrap();
    assert!(l.table.progress().done);
    assert!(!l.edits.can_undo(), "history is relative to the old file");
    assert_eq!(cell(&mut doc, 1, 0), "x");

    // A second edit + save: the identity check uses the new file.
    set(&mut doc, RowKey::orig(0), 1, "y");
    doc.save();
    saved(&mut doc);
    assert_eq!(std::fs::read(&path).unwrap(), b"a,b\r\n1,y\r\nx,4\r\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn another_program_changing_the_file_blocks_the_save() {
    let dir = test_dir("doc-conflict");
    let path = dir.join("t.csv");
    std::fs::write(&path, b"a\r\n1\r\n").unwrap();
    let mut doc = opened(&path, &dir);
    set(&mut doc, RowKey::orig(0), 0, "2");
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(&path, b"a\r\n1\r\nexcel\r\n").unwrap();
    doc.save();
    wait_for(|| {
        doc.poll();
        !doc.is_saving()
    });
    assert!(
        doc.save_error()
            .unwrap()
            .contains("changed by another program")
    );
    assert!(doc.is_dirty(), "edits are kept");
    assert_eq!(std::fs::read(&path).unwrap(), b"a\r\n1\r\nexcel\r\n");
    // "Overwrite" from the banner.
    doc.failure = None;
    doc.save_own(true);
    saved(&mut doc);
    assert_eq!(std::fs::read(&path).unwrap(), b"a\r\n2\r\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn save_as_rebinds_or_exports() {
    let dir = test_dir("doc-saveas");
    let lib = dir.join("lib");
    std::fs::create_dir_all(&lib).unwrap();
    let path = dir.join("t.csv");
    std::fs::write(&path, b"a,b\r\n\"1\t5\",2\r\n").unwrap();
    let mut doc = CsvDoc::open(path.clone(), &open_cx(&lib));
    wait_for(|| {
        doc.poll();
        doc.loaded().is_some_and(|l| l.table.progress().done)
    });
    // Save As .tsv: the document now is the TSV (tab-delimited).
    let tsv = dir.join("t.tsv");
    assert!(matches!(doc.save_as(tsv.clone()), SaveResult::Started));
    saved(&mut doc);
    assert_eq!(doc.path(), Some(tsv.as_path()));
    assert_eq!(doc.title(), "t.tsv");
    assert_eq!(std::fs::read(&tsv).unwrap(), b"a\tb\r\n\"1\t5\"\t2\r\n");
    assert_eq!(doc.loaded().unwrap().table.dialect.delimiter, b'\t');
    assert_eq!(cell(&mut doc, 0, 0), "1\t5");

    // A table in the library exports a copy and stays bound.
    let in_lib = lib.join("mine.csv");
    std::fs::write(&in_lib, b"a\r\n1\r\n").unwrap();
    let mut doc = opened(&in_lib, &lib);
    set(&mut doc, RowKey::orig(0), 0, "2");
    let copy = dir.join("copy.csv");
    doc.save_as(copy.clone());
    saved(&mut doc);
    assert_eq!(doc.path(), Some(in_lib.as_path()));
    assert!(doc.is_dirty(), "still dirty relative to its own file");
    assert_eq!(std::fs::read(&copy).unwrap(), b"a\r\n2\r\n");
    assert_eq!(std::fs::read(&in_lib).unwrap(), b"a\r\n1\r\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn save_as_spec_suggests_the_file_name() {
    let dir = test_dir("doc-spec");
    let path = dir.join("people.tsv");
    std::fs::write(&path, b"a\tb\n").unwrap();
    let doc = opened(&path, &dir);
    let spec = doc.save_as_spec().unwrap();
    assert_eq!(spec.suggested_name, "people.tsv");
    assert_eq!(spec.default_ext, "csv");
    assert_eq!(spec.folder.as_deref(), Some(dir.as_path()));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn utf16_files_save_as_utf16_and_reload() {
    let dir = test_dir("doc-utf16");
    let path = dir.join("u.csv");
    let utf16 = |s: &str| -> Vec<u8> {
        let mut v = vec![0xFF, 0xFE];
        v.extend(s.encode_utf16().flat_map(|u| u.to_le_bytes()));
        v
    };
    std::fs::write(&path, utf16("name,city\r\nAnn,Oslo\r\n")).unwrap();
    let mut doc = opened(&path, &dir);
    doc.loaded().unwrap().grid.sel = Some(crate::grid::Selection::single(crate::grid::CellPos {
        row: 0,
        col: 1,
    }));
    set(&mut doc, RowKey::orig(0), 1, "Zürich");
    doc.save();
    saved(&mut doc);
    assert_eq!(
        std::fs::read(&path).unwrap(),
        utf16("name,city\r\nAnn,Zürich\r\n")
    );
    // Re-read through the normal path, keeping the view.
    wait_for(|| {
        doc.poll();
        doc.loaded().is_some_and(|l| l.table.progress().done)
    });
    assert!(!doc.is_dirty());
    assert_eq!(cell(&mut doc, 0, 1), "Zürich");
    let l = doc.loaded().unwrap();
    assert_eq!(l.original_encoding, Encoding::Utf16Le);
    assert!(l.grid.sel.is_some(), "selection survives the reload");
    let _ = std::fs::remove_dir_all(&dir);
}

fn searched(doc: &mut CsvDoc) -> whir_core::SearchState {
    wait_for(|| {
        doc.poll();
        doc.search_state().is_some_and(|s| !s.running)
    });
    doc.search_state().unwrap()
}

#[test]
fn find_in_all_tabs_lists_and_reveals_matches() {
    let dir = test_dir("doc-search");
    let path = dir.join("rack.csv");
    std::fs::write(
        &path,
        b"Terminal,Supplier,Price\r\nDallas,Citgo,2.4567\r\nTulsa,Shell,3.1\r\nTampa,\"BP\",2.45670\r\n",
    )
    .unwrap();
    let mut doc = opened(&path, &dir);
    assert!(doc.searchable());
    let q = FindQuery {
        text: "2.4567".into(),
        whole_cell: true,
    };
    doc.search(&q);
    let s = searched(&mut doc);
    assert_eq!((s.query, s.hits, s.truncated), (q.clone(), 2, false));
    // Nothing moves until a match is chosen.
    assert_eq!(s.current, None);
    assert_eq!(doc.loaded().unwrap().grid.sel, None);
    // The tab's own find shows the query.
    assert_eq!(doc.loaded().unwrap().find.bar.searched(), &q);

    let hit = doc.search_hit(1).unwrap();
    assert_eq!(hit.place, "Row 3");
    assert_eq!(hit.field, "Price");
    assert_eq!(hit.text, "2.45670");
    assert_eq!(hit.context, "Tampa  ·  BP");
    assert_eq!(doc.search_hit(2), None);

    doc.reveal_hit(1);
    assert_eq!(doc.search_state().unwrap().current, Some(1));
    let sel = doc.loaded().unwrap().grid.sel.unwrap();
    assert_eq!((sel.cursor.row, sel.cursor.col), (2, 2));
}

#[test]
fn a_search_while_loading_runs_once_loaded() {
    let dir = test_dir("doc-search-loading");
    let path = dir.join("t.csv");
    std::fs::write(&path, b"a\r\nzebra\r\nyak\r\nzebu\r\n").unwrap();
    let mut doc = CsvDoc::open(path.clone(), &open_cx(&dir));
    let q = FindQuery::new("ZE");
    // Straight after opening the file is still loading on its thread.
    doc.search(&q);
    assert_eq!(doc.search_state().map(|s| s.query), Some(q));
    let s = searched(&mut doc);
    assert_eq!(s.hits, 2);
}
