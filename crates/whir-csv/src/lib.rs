//! CSV pillar: files of any size, never locked, never fully loaded.
//!
//! A background pass indexes every 64th row; the grid parses only the rows on
//! screen. Find and sort stream through the file on their own threads.

mod dialect;
mod doc;
mod edit;
mod find;
mod grid;
mod index;
mod parse;
mod scan;
mod sort;
mod source;
mod table;

use std::path::PathBuf;

use whir_core::{Document, OpenCx};

pub const EXTENSIONS: &[&str] = &["csv", "tsv", "tab"];

pub fn open(path: PathBuf, cx: &OpenCx) -> Box<dyn Document> {
    Box::new(doc::CsvDoc::open(path, cx))
}
