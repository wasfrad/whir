//! Byte sources: the user's file (never locked), or a UTF-8 temp copy for UTF-16 files.

use std::fs::File;
use std::io::{self, Write as _};
use std::os::windows::fs::{FileExt as _, OpenOptionsExt as _};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use whir_core::FileSource;

pub trait ByteSource: Send + Sync {
    fn len(&self) -> u64;
    /// Read up to `buf.len()` bytes at `offset`; 0 at EOF.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize>;
    /// The underlying file changed since it was opened.
    fn changed(&self) -> bool {
        false
    }
}

impl ByteSource for FileSource {
    fn len(&self) -> u64 {
        FileSource::len(self)
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        FileSource::read_at(self, offset, buf)
    }

    fn changed(&self) -> bool {
        FileSource::changed(self)
    }
}

/// Bytes held in memory: a new blank table (a header line) and tests.
pub struct MemSource {
    data: Vec<u8>,
    /// Largest single read: tests use tiny reads to stress the refill paths.
    max_read: usize,
}

impl MemSource {
    pub fn new(data: impl Into<Vec<u8>>) -> Self {
        Self {
            data: data.into(),
            max_read: usize::MAX,
        }
    }

    /// Every `read_at` returns at most `max_read` bytes.
    #[cfg(test)]
    pub fn with_max_read(data: impl Into<Vec<u8>>, max_read: usize) -> Self {
        Self {
            data: data.into(),
            max_read: max_read.max(1),
        }
    }
}

impl ByteSource for MemSource {
    fn len(&self) -> u64 {
        self.data.len() as u64
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        let o = (offset as usize).min(self.data.len());
        let n = buf.len().min(self.data.len() - o).min(self.max_read);
        buf[..n].copy_from_slice(&self.data[o..o + n]);
        Ok(n)
    }
}

/// A temp file deleted automatically when closed (or when whir exits).
pub struct TempSource {
    file: File,
    len: u64,
}

impl ByteSource for TempSource {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        if offset >= self.len {
            return Ok(0);
        }
        let want = buf.len().min((self.len - offset) as usize);
        let mut done = 0;
        while done < want {
            match self
                .file
                .seek_read(&mut buf[done..want], offset + done as u64)?
            {
                0 => break,
                n => done += n,
            }
        }
        Ok(done)
    }
}

fn temp_path() -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "whir-{}-{}.csv",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ))
}

/// Stream-transcode a UTF-16 file into a UTF-8 temp file (memory stays flat).
pub fn transcode_utf16(
    src: &dyn ByteSource,
    skip: u64,
    big_endian: bool,
    mut progress: impl FnMut(u64),
) -> io::Result<Arc<TempSource>> {
    use windows::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_TEMPORARY, FILE_FLAG_DELETE_ON_CLOSE, FILE_SHARE_DELETE, FILE_SHARE_READ,
    };
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .share_mode((FILE_SHARE_READ | FILE_SHARE_DELETE).0)
        .custom_flags(FILE_FLAG_DELETE_ON_CLOSE.0)
        .attributes(FILE_ATTRIBUTE_TEMPORARY.0)
        .open(temp_path())?;

    let mut buf = vec![0u8; 1 << 20];
    let mut out = String::with_capacity(1 << 20);
    let mut decoder = Utf16Decoder::new(big_endian);
    let mut offset = skip;
    let mut written = 0u64;
    loop {
        let n = src.read_at(offset, &mut buf)?;
        if n == 0 {
            break;
        }
        offset += n as u64;
        decoder.feed(&buf[..n], &mut out);
        file.write_all(out.as_bytes())?;
        written += out.len() as u64;
        out.clear();
        progress(offset);
    }
    decoder.finish(&mut out);
    file.write_all(out.as_bytes())?;
    written += out.len() as u64;
    file.flush()?;
    Ok(Arc::new(TempSource { file, len: written }))
}

/// Incremental UTF-16 → UTF-8, tolerant of chunk boundaries anywhere.
pub struct Utf16Decoder {
    big_endian: bool,
    /// Odd byte left over from the previous chunk.
    carry: Option<u8>,
    /// High surrogate waiting for its pair.
    high: Option<u16>,
}

impl Utf16Decoder {
    pub fn new(big_endian: bool) -> Self {
        Self {
            big_endian,
            carry: None,
            high: None,
        }
    }

    fn unit(&mut self, b: [u8; 2], out: &mut String) {
        let u = if self.big_endian {
            u16::from_be_bytes(b)
        } else {
            u16::from_le_bytes(b)
        };
        if let Some(h) = self.high.take() {
            if (0xDC00..0xE000).contains(&u) {
                if let Some(Ok(c)) = char::decode_utf16([h, u]).next() {
                    out.push(c);
                    return;
                }
            }
            out.push(char::REPLACEMENT_CHARACTER);
        }
        if (0xD800..0xDC00).contains(&u) {
            self.high = Some(u);
        } else {
            out.push(char::from_u32(u as u32).unwrap_or(char::REPLACEMENT_CHARACTER));
        }
    }

    pub fn feed(&mut self, mut bytes: &[u8], out: &mut String) {
        if let Some(c) = self.carry.take() {
            match bytes.split_first() {
                Some((&b, rest)) => {
                    self.unit([c, b], out);
                    bytes = rest;
                }
                None => {
                    self.carry = Some(c);
                    return;
                }
            }
        }
        let mut pairs = bytes.chunks_exact(2);
        for p in &mut pairs {
            self.unit([p[0], p[1]], out);
        }
        if let [b] = pairs.remainder() {
            self.carry = Some(*b);
        }
    }

    pub fn finish(&mut self, out: &mut String) {
        if self.high.take().is_some() || self.carry.take().is_some() {
            out.push(char::REPLACEMENT_CHARACTER);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_decoder_handles_any_split() {
        let text = "a,b\n😀,é\n";
        for big_endian in [false, true] {
            let bytes: Vec<u8> = text
                .encode_utf16()
                .flat_map(|u| {
                    if big_endian {
                        u.to_be_bytes()
                    } else {
                        u.to_le_bytes()
                    }
                })
                .collect();
            for split in 0..bytes.len() {
                let mut d = Utf16Decoder::new(big_endian);
                let mut out = String::new();
                d.feed(&bytes[..split], &mut out);
                d.feed(&bytes[split..], &mut out);
                d.finish(&mut out);
                assert_eq!(out, text, "split at {split}");
            }
        }
    }
}
