//! Image pillar: WIC decoding (built into Windows, no codecs shipped), GPU
//! textures with mipmaps, zoom/pan, and ←/→ through the folder.

mod bake;
mod decode;
mod doc;
mod edit;
mod encode;
mod folder;
mod pool;
mod thumbs;
mod tiles;
mod view;

use std::path::PathBuf;

use whir_core::{Document, OpenCx};

pub use decode::{DecodeError, Decoded, decode};
pub use encode::{EncodeError, PixelLayout, encode_png};
pub use thumbs::{Stamp, Thumb, Thumbs};

pub const EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "jpe", "jfif", "gif", "bmp", "dib", "tif", "tiff", "ico", "webp", "heic",
    "heif", "avif", "jxr", "wdp",
];

pub fn open(path: PathBuf, cx: &OpenCx) -> Box<dyn Document> {
    Box::new(doc::ImageDoc::open(path, cx))
}
