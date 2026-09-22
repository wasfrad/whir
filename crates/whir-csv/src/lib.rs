//! CSV pillar.

use std::path::PathBuf;

use whir_core::{Document, MessageDoc, OpenCx};

pub const EXTENSIONS: &[&str] = &["csv", "tsv", "tab"];

pub fn open(path: PathBuf, _cx: &OpenCx) -> Box<dyn Document> {
    MessageDoc::new(Some(path), "CSV viewer not implemented yet", "").boxed()
}
