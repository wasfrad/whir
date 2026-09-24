//! PDF pillar: PDFium (bundled `pdfium.dll`, loaded on first use) on a
//! dedicated thread; pages render as GPU tiles at exact device resolution.
//! Text is extracted on the same thread for selection, copy and find.

mod doc;
mod engine;
mod ffi;
mod geom;
mod layout;
mod mini_layout;
mod search;
mod select;
mod store;
mod text;
mod view;

use std::path::PathBuf;

use whir_core::{Document, OpenCx};

pub const EXTENSIONS: &[&str] = &["pdf"];

pub fn open(path: PathBuf, cx: &OpenCx) -> Box<dyn Document> {
    Box::new(doc::PdfDoc::open(path, cx))
}
