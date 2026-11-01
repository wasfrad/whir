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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use windows::Win32::Foundation::{HANDLE, HWND};
use windows::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_READONLY, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, GetFileAttributesW, GetFileInformationByHandle,
    INVALID_FILE_ATTRIBUTES, MOVE_FILE_FLAGS, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    MoveFileExW, REPLACEFILE_IGNORE_ACL_ERRORS, REPLACEFILE_IGNORE_MERGE_ERRORS, ReplaceFileW,
    SetFileAttributesW,
};
use windows::Win32::UI::Shell::{
    FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FOF_WANTNUKEWARNING,
    SHFILEOPSTRUCTW, SHFileOperationW,
};
use windows::core::{HSTRING, PCWSTR};

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
            .map_err(|e| win32_io_error(&e))?;
        Ok(Self {
            volume: info.dwVolumeSerialNumber,
            index: ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64,
            size: ((info.nFileSizeHigh as u64) << 32) | info.nFileSizeLow as u64,
            mtime: ((info.ftLastWriteTime.dwHighDateTime as u64) << 32)
                | info.ftLastWriteTime.dwLowDateTime as u64,
        })
    }

    /// Identity of the file currently at `path`.
    pub fn of_path(path: &Path) -> io::Result<Self> {
        Self::of(&open_shared(path)?)
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

// --- writing ------------------------------------------------------------------
//
// Saves never write the user's file in place: content goes to a temp file in the
// same folder (same volume, so the final step is an atomic rename), is flushed to
// disk, and then replaces the target. Temp names start with `~$`, which OneDrive
// and the library scan skip.

/// Win32 error code carried by a `windows` error (its HRESULT is `0x8007xxxx`).
pub fn win32_code(e: &windows::core::Error) -> u32 {
    let hr = e.code().0 as u32;
    if hr & 0xFFFF_0000 == 0x8007_0000 {
        hr & 0xFFFF
    } else {
        hr
    }
}

fn win32_io_error(e: &windows::core::Error) -> io::Error {
    io::Error::from_raw_os_error(win32_code(e) as i32)
}

/// Prefix of every temp/backup file whir creates next to user files.
pub const TEMP_PREFIX: &str = "~$whir-";

fn temp_name(ext: &str) -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    format!(
        "{TEMP_PREFIX}{}-{}.{ext}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

/// Create a new, empty temp file in `dir` (not hidden: if it becomes the final
/// file via a plain move, its attributes carry over).
pub fn temp_in(dir: &Path) -> io::Result<(PathBuf, File)> {
    loop {
        let path = dir.join(temp_name("tmp"));
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(f) => return Ok((path, f)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
}

/// Why a save couldn't replace its target.
#[derive(Debug)]
pub enum WriteError {
    /// Another program holds the file open without sharing (Excel does this).
    Locked,
    ReadOnly,
    AccessDenied,
    DiskFull,
    /// The file on disk is no longer the version we loaded.
    ChangedOnDisk,
    Io(io::Error),
}

impl WriteError {
    pub fn from_io(e: io::Error) -> Self {
        const ERROR_ACCESS_DENIED: i32 = 5;
        const ERROR_SHARING_VIOLATION: i32 = 32;
        const ERROR_LOCK_VIOLATION: i32 = 33;
        const ERROR_HANDLE_DISK_FULL: i32 = 39;
        const ERROR_DISK_FULL: i32 = 112;
        const ERROR_UNABLE_TO_REMOVE_REPLACED: i32 = 1175;
        match e.raw_os_error() {
            Some(
                ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION | ERROR_UNABLE_TO_REMOVE_REPLACED,
            ) => WriteError::Locked,
            Some(ERROR_ACCESS_DENIED) => WriteError::AccessDenied,
            Some(ERROR_DISK_FULL | ERROR_HANDLE_DISK_FULL) => WriteError::DiskFull,
            _ => WriteError::Io(e),
        }
    }

    /// User-facing explanation, naming the file.
    pub fn message(&self, name: &str) -> String {
        match self {
            WriteError::Locked => format!(
                "“{name}” is open in another program (Excel locks files while they're open). \
                 Close it there and save again, or use Save As."
            ),
            WriteError::ReadOnly => format!("“{name}” is read-only. Use Save As to save a copy."),
            WriteError::AccessDenied => {
                format!("You don't have permission to change “{name}”. Use Save As to save a copy.")
            }
            WriteError::DiskFull => "There isn't enough free disk space to save.".to_owned(),
            WriteError::ChangedOnDisk => {
                format!("“{name}” was changed by another program since whir opened it.")
            }
            WriteError::Io(e) => format!("Couldn't save “{name}”: {e}"),
        }
    }
}

impl From<io::Error> for WriteError {
    fn from(e: io::Error) -> Self {
        WriteError::from_io(e)
    }
}

/// A OneDrive (or other sync client's) placeholder: reading the file would
/// download it. `attributes` are its Win32 file attributes.
pub fn is_cloud_only(attributes: u32) -> bool {
    const FILE_ATTRIBUTE_OFFLINE: u32 = 0x1000;
    const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x4_0000;
    const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x40_0000;
    attributes
        & (FILE_ATTRIBUTE_OFFLINE
            | FILE_ATTRIBUTE_RECALL_ON_OPEN
            | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS)
        != 0
}

/// Mark a file or folder hidden in Explorer (if that fails, it just shows).
pub fn hide(path: &Path) {
    unsafe {
        let _ = SetFileAttributesW(&HSTRING::from(path.as_os_str()), FILE_ATTRIBUTE_HIDDEN);
    }
}

pub fn is_read_only(path: &Path) -> bool {
    let attrs = unsafe { GetFileAttributesW(&HSTRING::from(path.as_os_str())) };
    attrs != INVALID_FILE_ATTRIBUTES && attrs & FILE_ATTRIBUTE_READONLY.0 != 0
}

fn move_file(from: &Path, to: &Path, flags: MOVE_FILE_FLAGS) -> io::Result<()> {
    unsafe {
        MoveFileExW(
            &HSTRING::from(from.as_os_str()),
            &HSTRING::from(to.as_os_str()),
            flags,
        )
    }
    .map_err(|e| win32_io_error(&e))
}

/// Rename `from` to `to` unless `to` exists. `Ok(false)` if it does.
pub fn move_no_replace(from: &Path, to: &Path) -> io::Result<bool> {
    const ERROR_FILE_EXISTS: i32 = 80;
    const ERROR_ALREADY_EXISTS: i32 = 183;
    match move_file(from, to, MOVEFILE_WRITE_THROUGH) {
        Ok(()) => Ok(true),
        Err(e)
            if matches!(
                e.raw_os_error(),
                Some(ERROR_FILE_EXISTS | ERROR_ALREADY_EXISTS)
            ) =>
        {
            Ok(false)
        }
        Err(e) => Err(e),
    }
}

/// Replace `target` with the (flushed, closed) file at `temp`.
///
/// Uses `ReplaceFileW`, which keeps the target's ACL, attributes and creation
/// time, with a backup name so every failure leaves a recoverable state. A
/// missing target is a plain move. On error `temp` is left for the caller to delete.
pub fn replace_file(target: &Path, temp: &Path) -> Result<(), WriteError> {
    const ERROR_FILE_NOT_FOUND: u32 = 2;
    const ERROR_UNABLE_TO_MOVE_REPLACEMENT_2: u32 = 1177;
    if !target.exists() {
        return move_file(temp, target, MOVEFILE_WRITE_THROUGH).map_err(WriteError::from_io);
    }
    if is_read_only(target) {
        return Err(WriteError::ReadOnly);
    }
    let dir = target.parent().unwrap_or(Path::new("."));
    let backup = dir.join(temp_name("bak"));
    let r = unsafe {
        ReplaceFileW(
            &HSTRING::from(target.as_os_str()),
            &HSTRING::from(temp.as_os_str()),
            &HSTRING::from(backup.as_os_str()),
            REPLACEFILE_IGNORE_MERGE_ERRORS | REPLACEFILE_IGNORE_ACL_ERRORS,
            None,
            None,
        )
    };
    match r {
        Ok(()) => {
            // Our own reader may still hold the old file open (shared for delete):
            // the backup then disappears when that handle closes.
            let _ = std::fs::remove_file(&backup);
            Ok(())
        }
        Err(e) => {
            let code = win32_code(&e);
            if code == ERROR_UNABLE_TO_MOVE_REPLACEMENT_2 {
                // The original is now at `backup`, the new content still at `temp`.
                match move_file(
                    temp,
                    target,
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                ) {
                    Ok(()) => {
                        let _ = std::fs::remove_file(&backup);
                        Ok(())
                    }
                    Err(e2) => {
                        let _ = move_file(&backup, target, MOVEFILE_WRITE_THROUGH);
                        Err(WriteError::from_io(e2))
                    }
                }
            } else if code == ERROR_FILE_NOT_FOUND && !target.exists() {
                move_file(temp, target, MOVEFILE_WRITE_THROUGH).map_err(WriteError::from_io)
            } else {
                Err(WriteError::from_io(io::Error::from_raw_os_error(
                    code as i32,
                )))
            }
        }
    }
}

/// Write a small file atomically: temp file → flush to disk → replace `dest`.
/// Returns the identity of the file now at `dest`.
pub fn write_atomic(
    dest: &Path,
    write: impl FnOnce(&mut File) -> io::Result<()>,
) -> Result<FileIdentity, WriteError> {
    let dir = dest.parent().unwrap_or(Path::new("."));
    let (tmp, mut file) = temp_in(dir)?;
    let result = (|| {
        write(&mut file)?;
        file.sync_all()
    })();
    drop(file);
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(WriteError::from_io(e));
    }
    if let Err(e) = replace_file(dest, &tmp) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(FileIdentity::of_path(dest)?)
}

/// Move `path` to the Recycle Bin. If the bin can't be used (e.g. a
/// network-redirected Documents folder) Windows asks from `owner` before
/// deleting for good; with no window to ask from, it deletes without asking.
pub fn recycle(owner: Option<HWND>, path: &Path) -> Result<(), String> {
    // Double-NUL terminated list of one path.
    let from: Vec<u16> = path
        .as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .chain([0, 0])
        .collect();
    let quiet = FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT;
    let flags = if owner.is_some() {
        quiet | FOF_WANTNUKEWARNING
    } else {
        quiet | FOF_NOERRORUI
    };
    let mut op = SHFILEOPSTRUCTW {
        hwnd: owner.unwrap_or_default(),
        wFunc: FO_DELETE,
        pFrom: PCWSTR(from.as_ptr()),
        fFlags: flags.0 as u16,
        ..Default::default()
    };
    let r = unsafe { SHFileOperationW(&mut op) };
    if r != 0 {
        return Err(format!("Couldn't delete the file (error {r})."));
    }
    if op.fAnyOperationsAborted.as_bool() {
        return Err("Delete was cancelled.".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("whir-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The notes saver's case: a background thread with no window. Ignored
    /// because it puts a real file in the real Recycle Bin.
    #[test]
    #[ignore]
    fn recycle_from_a_thread_with_no_window() {
        let dir = test_dir("recycle");
        let path = dir.join("emptied note.md");
        std::fs::write(&path, b"old").unwrap();
        let on_thread = path.clone();
        let result = std::thread::spawn(move || recycle(None, &on_thread))
            .join()
            .unwrap();
        assert_eq!(result, Ok(()));
        assert!(!path.exists());
    }

    #[test]
    fn replace_while_our_reader_holds_the_file() {
        let dir = test_dir("replace");
        let path = dir.join("a.csv");
        std::fs::write(&path, b"old").unwrap();
        let src = FileSource::open(&path).unwrap();
        let mut buf = [0u8; 3];
        src.read_exact_at(0, &mut buf).unwrap();

        let id = write_atomic(&path, |f| {
            use std::io::Write as _;
            f.write_all(b"new content")
        })
        .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new content");
        assert_eq!(id.size, 11);
        drop(src);
        // No temp or backup files left behind.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(TEMP_PREFIX))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn locked_target_is_reported_and_left_alone() {
        let dir = test_dir("locked");
        let path = dir.join("b.csv");
        std::fs::write(&path, b"keep me").unwrap();
        // Simulate Excel: open without FILE_SHARE_DELETE / FILE_SHARE_WRITE.
        let excel = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ.0)
            .open(&path)
            .unwrap();
        let r = write_atomic(&path, |f| {
            use std::io::Write as _;
            f.write_all(b"clobber")
        });
        assert!(matches!(r, Err(WriteError::Locked)), "{r:?}");
        drop(excel);
        assert_eq!(std::fs::read(&path).unwrap(), b"keep me");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_target_moves_and_no_replace_refuses() {
        let dir = test_dir("move");
        let target = dir.join("new.md");
        let id = write_atomic(&target, |f| {
            use std::io::Write as _;
            f.write_all(b"hi")
        })
        .unwrap();
        assert_eq!(id.size, 2);
        let (tmp, file) = temp_in(&dir).unwrap();
        drop(file);
        assert!(!move_no_replace(&tmp, &target).unwrap());
        assert!(move_no_replace(&tmp, &dir.join("other.md")).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_only_target() {
        let dir = test_dir("ro");
        let path = dir.join("r.csv");
        std::fs::write(&path, b"x").unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&path, perms.clone()).unwrap();
        let r = write_atomic(&path, |_| Ok(()));
        assert!(matches!(r, Err(WriteError::ReadOnly)), "{r:?}");
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        std::fs::set_permissions(&path, perms).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

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
