//! Shared contracts between the whir shell and its viewer pillars.
//!
//! Pillars (image, pdf, csv, ...) depend only on this crate: they never touch
//! D3D or Win32 windowing directly, which keeps viewer code pure egui.

pub mod document;
pub mod file;
pub mod message_doc;
pub mod texture;
pub mod theme;
pub mod trace;
pub mod waker;

pub use document::{DocCx, DocKind, Document, OpenCx};
pub use file::FileSource;
pub use message_doc::MessageDoc;
pub use texture::{Filter, PixelFormat, TextureDesc, TextureRegistry, UserTexture};
pub use waker::Waker;
