//! Notes pillar: Markdown notes with a title, styled as you type, that save
//! themselves into the notes library (`Documents\whir`). Other `.md`/`.txt`
//! files open in the same editor with explicit saving.

mod doc;
pub mod editing;
pub mod markdown;
pub mod note_file;
pub mod saver;

use std::path::PathBuf;

use whir_core::{Document, OpenCx};

pub const EXTENSIONS: &[&str] = &["md", "markdown", "txt"];

/// A file the notes editor opens (Markdown or plain text).
pub fn open(path: PathBuf, cx: &OpenCx) -> Box<dyn Document> {
    Box::new(doc::NoteDoc::open(path, cx))
}

/// A new, empty note (nothing is written until it has text).
pub fn new_note(cx: &OpenCx) -> Box<dyn Document> {
    Box::new(doc::NoteDoc::new_note(cx))
}
