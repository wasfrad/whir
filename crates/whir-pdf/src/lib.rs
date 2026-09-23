//! PDF pillar: PDFium (bundled `pdfium.dll`, loaded on first use) on a
//! dedicated thread; pages render as GPU tiles at exact device resolution.

mod doc;
mod engine;
mod ffi;
mod layout;

use std::path::PathBuf;

use whir_core::{Document, OpenCx};

pub const EXTENSIONS: &[&str] = &["pdf"];

pub fn open(path: PathBuf, cx: &OpenCx) -> Box<dyn Document> {
    Box::new(doc::PdfDoc::open(path, cx))
}
