//! Helpers shared by the unit tests.

use std::sync::{Arc, RwLock};

use crate::dialect::{Dialect, sniff};
use crate::edit::{EditSnapshot, RowKey};
use crate::index;
use crate::parse::{FieldBuf, RowReader};
use crate::source::{ByteSource, MemSource};
use crate::stream::{self, Field, StreamInput};

/// A small deterministic PRNG (xorshift64*).
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }
}

/// Every row of `data` as strings (UTF-8), header included.
pub fn parse_all(data: &[u8], d: Dialect, offset: u64) -> Vec<Vec<String>> {
    let src = MemSource::new(data.to_vec());
    let mut r = RowReader::new(&src, d, offset);
    let mut f = FieldBuf::default();
    let mut out = Vec::new();
    while r.next_row(&mut f).unwrap().is_some() {
        out.push(
            f.iter()
                .map(|b| String::from_utf8_lossy(b).into_owned())
                .collect(),
        );
    }
    out
}

/// A stream over `data` (sniffed dialect, indexed synchronously).
pub fn input_for(data: &[u8], edits: EditSnapshot) -> StreamInput {
    let s = sniff(data, None);
    let src = MemSource::new(data.to_vec());
    let idx = index::build_sync(&src, s.bom_len, s.dialect);
    let quoting = !idx.quoting_disabled;
    let source: Arc<dyn ByteSource> = Arc::new(src);
    StreamInput {
        source,
        dialect: Dialect {
            quoting,
            ..s.dialect
        },
        header_offset: s.bom_len,
        index: Arc::new(RwLock::new(idx)),
        edits,
    }
}

/// The logical table as strings: `(key, fields)` per row, header first.
pub fn collect(input: &StreamInput) -> Vec<(RowKey, Vec<String>)> {
    let mut out = Vec::new();
    let done = stream::for_each(input, &Default::default(), |row, _| {
        out.push((
            row.key,
            (0..row.len())
                .map(|c| match row.field(c) {
                    Field::File(b) => String::from_utf8_lossy(b).into_owned(),
                    Field::Text(s) => s.to_owned(),
                })
                .collect(),
        ));
        std::ops::ControlFlow::Continue(())
    })
    .unwrap();
    assert!(done);
    out
}

/// Drop trailing empty cells (ragged rows compare by content).
pub fn trim(mut v: Vec<String>) -> Vec<String> {
    while v.last().is_some_and(|s| s.is_empty()) {
        v.pop();
    }
    v
}

struct NoTextures;

impl whir_core::TextureRegistry for NoTextures {
    fn create(
        self: Arc<Self>,
        _desc: &whir_core::TextureDesc,
        _pixels: &[u8],
        _stride: usize,
    ) -> Result<whir_core::UserTexture, String> {
        Err("no GPU in tests".into())
    }

    fn max_texture_side(&self) -> u32 {
        4096
    }

    fn release(&self, _id: u64) {}
}

/// An `OpenCx` whose notes library is `dir`.
pub fn open_cx(dir: &std::path::Path) -> whir_core::OpenCx {
    whir_core::OpenCx {
        textures: Arc::new(NoTextures),
        waker: whir_core::Waker::noop(),
        library: whir_core::Library::at(dir.to_path_buf(), whir_core::Waker::noop()),
    }
}

/// A fresh, empty directory for a test.
pub fn test_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("whir-csv-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Poll until `done` (panics after 10 s).
pub fn wait_for(mut poll: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !poll() {
        assert!(std::time::Instant::now() < deadline, "timed out");
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}
