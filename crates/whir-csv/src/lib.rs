//! CSV pillar: files of any size, never locked, never fully loaded.
//!
//! A background pass indexes every 64th row; the grid parses only the rows on
//! screen. Find and sort stream through the file on their own threads.

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
mod scan;
mod show;
mod sort;
mod source;
mod stream;
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
