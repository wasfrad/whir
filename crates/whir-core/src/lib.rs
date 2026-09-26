//! Shared contracts between the whir shell and its pillars.
//!
//! Pillars (image, pdf, csv, notes, ...) depend only on this crate: they never
//! touch D3D or Win32 windowing directly, which keeps document code pure egui.

pub mod document;
pub mod file;
pub mod find_bar;
pub mod library;
pub mod message_doc;
pub mod scroll;
pub mod settings;
pub mod texture;
pub mod theme;
pub mod time;
pub mod trace;
pub mod waker;

pub use document::{
    DocCx, DocKind, DocRequest, Document, OpenCx, Persistence, SaveAsSpec, SaveResult, SearchHit,
    SearchState, needs_attention,
};
pub use file::FileSource;
pub use find_bar::FindQuery;
pub use library::Library;
pub use message_doc::MessageDoc;
pub use texture::{Filter, PixelFormat, TextureDesc, TextureRegistry, UserTexture};
pub use waker::Waker;
