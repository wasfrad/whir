//! Lock-free-for-others file access.
//!
//! Every user file is opened with `FILE_SHARE_READ | FILE_SHARE_WRITE |
//! FILE_SHARE_DELETE` and never memory-mapped, so other programs (Excel saving a
//! CSV, a report generator rewriting a PDF) are never blocked by whir. Handles are
//! additionally dropped whenever whir loses activation ([`release_all`]) and
//! reopened lazily; on reopen the file identity is re-validated and a change is
//! surfaced through [`FileSource::changed`].

use std::fs::File;
use std::io;
use std::os::windows::fs::{FileExt as _, OpenOptionsExt as _};
use std::os::windows::io::AsRawHandle as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    GetFileInformationByHandle,
};

/// Opens `path` for reading without denying anyone else read/write/delete access.
pub fn open_shared(path: &Path) -> io::Result<File> {
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE).0)
        .open(path)
}

/// Stable identity of a file's content: which file (volume + index) and which version (size + mtime).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileIdentity {
    pub volume: u32,
    pub index: u64,
    pub size: u64,
    pub mtime: u64,
}

impl FileIdentity {
    pub fn of(file: &File) -> io::Result<Self> {
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }
            .map_err(|e| io::Error::from_raw_os_error(e.code().0))?;
        Ok(Self {
            volume: info.dwVolumeSerialNumber,
            index: ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64,
            size: ((info.nFileSizeHigh as u64) << 32) | info.nFileSizeLow as u64,
            mtime: ((info.ftLastWriteTime.dwHighDateTime as u64) << 32)
                | info.ftLastWriteTime.dwLowDateTime as u64,
        })
    }
}

/// A file read by position from any thread, whose handle may come and go.
pub struct FileSource {
    path: PathBuf,
    identity: FileIdentity,
    file: Mutex<Option<File>>,
    changed: AtomicBool,
}

static REGISTRY: Mutex<Vec<Weak<FileSource>>> = Mutex::new(Vec::new());

impl FileSource {
    pub fn open(path: &Path) -> io::Result<Arc<Self>> {
        let file = open_shared(path)?;
        let identity = FileIdentity::of(&file)?;
        let src = Arc::new(Self {
            path: path.to_owned(),
            identity,
            file: Mutex::new(Some(file)),
            changed: AtomicBool::new(false),
        });
        if let Ok(mut reg) = REGISTRY.lock() {
            reg.retain(|w| w.strong_count() > 0);
            reg.push(Arc::downgrade(&src));
        }
        Ok(src)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Size at open time. All reads are relative to this version of the file.
    pub fn len(&self) -> u64 {
        self.identity.size
    }

    pub fn is_empty(&self) -> bool {
        self.identity.size == 0
    }

    pub fn identity(&self) -> FileIdentity {
        self.identity
    }

    /// The file on disk no longer matches what was opened.
    pub fn changed(&self) -> bool {
        self.changed.load(Ordering::Relaxed)
    }

    /// Drop the OS handle; the next read reopens (and re-validates) it.
    pub fn release(&self) {
        if let Ok(mut f) = self.file.lock() {
            *f = None;
        }
    }

    /// Run `f` with an open handle, reopening and re-validating if needed.
    pub fn with_file<R>(&self, f: impl FnOnce(&File) -> io::Result<R>) -> io::Result<R> {
        if self.changed() {
            return Err(changed_error());
        }
        let mut guard = self
            .file
            .lock()
            .map_err(|_| io::Error::other("file lock poisoned"))?;
        if guard.is_none() {
            let file = open_shared(&self.path)?;
            if FileIdentity::of(&file)? != self.identity {
                self.changed.store(true, Ordering::Relaxed);
                return Err(changed_error());
            }
            *guard = Some(file);
        }
        f(guard.as_ref().expect("handle just ensured"))
    }

    /// Read up to `buf.len()` bytes at `offset`. Returns bytes read (0 at EOF).
    pub fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        let end = self.identity.size;
        if offset >= end {
            return Ok(0);
        }
        let want = buf.len().min((end - offset) as usize);
        self.with_file(|file| {
            let mut done = 0;
            while done < want {
                match file.seek_read(&mut buf[done..want], offset + done as u64) {
                    Ok(0) => break,
                    Ok(n) => done += n,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(e),
                }
            }
            Ok(done)
        })
    }

    /// Read exactly `buf.len()` bytes at `offset`.
    pub fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let n = self.read_at(offset, buf)?;
        if n == buf.len() {
            Ok(())
        } else {
            Err(io::ErrorKind::UnexpectedEof.into())
        }
    }
}

fn changed_error() -> io::Error {
    io::Error::other("the file changed on disk")
}

/// Drop every open [`FileSource`] handle. Called when whir loses activation so
/// the user can save/replace the file from another app.
pub fn release_all() {
    let live: Vec<Arc<FileSource>> = match REGISTRY.lock() {
        Ok(mut reg) => {
            reg.retain(|w| w.strong_count() > 0);
            reg.iter().filter_map(Weak::upgrade).collect()
        }
        Err(_) => return,
    };
    for src in live {
        src.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_release_reopen_and_detect_change() {
        let dir = std::env::temp_dir().join(format!("whir-file-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.txt");
        std::fs::write(&path, b"hello world").unwrap();

        let src = FileSource::open(&path).unwrap();
        let mut buf = [0u8; 5];
        src.read_exact_at(6, &mut buf).unwrap();
        assert_eq!(&buf, b"world");

        // Others can overwrite the file while we hold it open.
        release_all();
        src.read_exact_at(0, &mut buf).unwrap();
        assert_eq!(&buf, b"hello");
        std::fs::write(&path, b"HELLO WORLD, longer now").unwrap();

        src.release();
        assert!(src.read_exact_at(0, &mut buf).is_err());
        assert!(src.changed());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
