//! PDF pillar (PDFium).

use std::path::PathBuf;

use whir_core::{Document, MessageDoc, OpenCx};

pub const EXTENSIONS: &[&str] = &["pdf"];

pub fn open(path: PathBuf, _cx: &OpenCx) -> Box<dyn Document> {
    MessageDoc::new(Some(path), "PDF viewer not implemented yet", "").boxed()
}
