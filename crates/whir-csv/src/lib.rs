//! CSV pillar: files of any size, never locked, never fully loaded — and
//! editable like a spreadsheet.
//!
//! A background pass indexes every 64th row; the grid parses only the rows on
//! screen. Find, sort and save stream through the file on their own threads.
//! Edits live in a small overlay keyed by stable row/column identities
//! (`edit/`); a save streams file + overlay into a new file and swaps to it.

mod bulk;
mod dialect;
mod doc;
mod edit;
mod editor;
mod find;
mod grid;
mod index;
mod keys;
mod loaded;
mod menu;
mod ops;
mod order;
mod parse;
mod paste;
mod scan;
mod show;
mod sort;
mod source;
mod stream;
mod structure;
mod table;
#[cfg(test)]
mod testutil;
mod view;
mod write;

use std::path::PathBuf;

use whir_core::{Document, OpenCx};

pub const EXTENSIONS: &[&str] = &["csv", "tsv", "tab"];

pub fn open(path: PathBuf, cx: &OpenCx) -> Box<dyn Document> {
    Box::new(doc::CsvDoc::open(path, cx))
}

/// A new, empty table (Ctrl+Shift+N): three columns, nothing on disk until
/// the first save, which puts it in the notes library next to the notes.
pub fn new_blank(cx: &OpenCx) -> Box<dyn Document> {
    Box::new(doc::new_blank(cx))
}
