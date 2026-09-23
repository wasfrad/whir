//! Saving: stream the logical table into a temp file next to the target, then
//! replace the target in one step (`whir_core::file::replace_file`).
//!
//! Rows nobody touched are copied byte for byte (their exact bytes, line
//! endings and quoting included); edited and added rows are serialized in the
//! file's style. A row's line ending is written only when the next row starts
//! (or at the end, following the original file's final-newline style), which
//! handles every end-of-file case with one rule.
//!
//! The writer records a checkpoint every 64 rows as it goes: for UTF-8 and
//! Windows-1252 output that *is* the new file's index, so reloading after a
//! save is instant.

pub mod encode;

use std::io::{self, BufWriter, Write};
use std::ops::ControlFlow;
use std::os::windows::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use whir_core::file::{FileIdentity, WriteError, is_read_only, replace_file, temp_in};
use whir_core::{FileSource, Waker};

pub use encode::OutEncoding;
use encode::Sink;

use crate::dialect::{Dialect, Encoding};
use crate::edit::RowOrder;
use crate::index::{BLOCK_ROWS, IndexState};
use crate::parse::cp1252_byte;
use crate::stream::{self, Field, LogicalRow, StreamInput};

/// How the saved file is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputFormat {
    pub delimiter: u8,
    /// Line ending of rewritten rows: `\r\n`, `\n` or `\r`.
    pub line_ending: &'static [u8],
    /// End the file with a line ending (the original's style).
    pub final_newline: bool,
    /// Quote every new value (the file quotes nearly everything).
    pub quote_all: bool,
    /// The source honours quotes. When it doesn't (unbalanced quotes), values
    /// are written verbatim with line breaks turned into spaces.
    pub quoting: bool,
    /// The source's row terminator (tells whether a copied row ended its line).
    pub terminator: u8,
    pub out: OutEncoding,
    /// Serialize every row (columns changed, or a different delimiter).
    pub rewrite_all: bool,
}

/// What a finished write produced.
#[derive(Debug)]
pub struct Written {
    pub bytes: u64,
    pub rows: u64,
    /// Offset of every 64th row in the output (row 0 = header).
    pub checkpoints: Vec<u64>,
}

struct RowWriter<W: Write> {
    sink: Sink<W>,
    rows: u64,
    at_line_start: bool,
    /// The last row produced no bytes (it needs its line ending to exist).
    last_empty: bool,
    checkpoints: Vec<u64>,
    fmt: OutputFormat,
}

impl<W: Write> RowWriter<W> {
    fn begin_row(&mut self) -> io::Result<u64> {
        if self.rows > 0 && !self.at_line_start {
            self.sink.ascii(self.fmt.line_ending)?;
        }
        let start = self.sink.position();
        if self.rows % BLOCK_ROWS == 0 {
            self.checkpoints.push(start);
        }
        self.rows += 1;
        Ok(start)
    }

    /// An untouched row, terminator included (if it had one).
    fn raw(&mut self, b: &[u8]) -> io::Result<()> {
        let start = self.begin_row()?;
        self.sink.raw(b)?;
        self.at_line_start = b.last() == Some(&self.fmt.terminator);
        self.last_empty = self.sink.position() == start;
        Ok(())
    }

    /// A rewritten row; its line ending is deferred.
    fn fields(&mut self, row: &LogicalRow<'_>, len: usize) -> io::Result<()> {
        let start = self.begin_row()?;
        for c in 0..len {
            if c > 0 {
                self.sink.ascii(&[self.fmt.delimiter])?;
            }
            match row.field(c) {
                Field::File(b) => self.file_field(b, row.was_quoted(c))?,
                Field::Text(s) => self.text_field(s, row.was_quoted(c))?,
            }
        }
        self.at_line_start = false;
        self.last_empty = self.sink.position() == start;
        Ok(())
    }

    /// A file value (already unescaped, in the source's encoding).
    fn file_field(&mut self, b: &[u8], was_quoted: bool) -> io::Result<()> {
        if !self.fmt.quoting {
            return self.sink.raw(b);
        }
        let d = self.fmt.delimiter;
        let quote = was_quoted
            || b.first() == Some(&b'"')
            || b.iter().any(|&c| c == d || c == b'\r' || c == b'\n');
        if !quote {
            return self.sink.raw(b);
        }
        self.sink.ascii(b"\"")?;
        let mut rest = b;
        while let Some(i) = memchr::memchr(b'"', rest) {
            self.sink.raw(&rest[..i])?;
            self.sink.ascii(b"\"\"")?;
            rest = &rest[i + 1..];
        }
        self.sink.raw(rest)?;
        self.sink.ascii(b"\"")
    }

    /// An edited value (UTF-8).
    fn text_field(&mut self, s: &str, was_quoted: bool) -> io::Result<()> {
        if !self.fmt.quoting {
            if s.contains(['\r', '\n']) {
                return self.sink.text(&s.replace(['\r', '\n'], " "));
            }
            return self.sink.text(s);
        }
        let d = self.fmt.delimiter as char;
        let quote = was_quoted
            || self.fmt.quote_all
            || s.contains(|c: char| c == d || c == '"' || c == '\r' || c == '\n');
        if !quote {
            return self.sink.text(s);
        }
        self.sink.ascii(b"\"")?;
        if s.contains('"') {
            self.sink.text(&s.replace('"', "\"\""))?;
        } else {
            self.sink.text(s)?;
        }
        self.sink.ascii(b"\"")
    }

    fn finish(&mut self) -> io::Result<()> {
        if self.rows > 0 && !self.at_line_start && (self.fmt.final_newline || self.last_empty) {
            self.sink.ascii(self.fmt.line_ending)?;
        }
        Ok(())
    }
}

/// Stream the logical table into `out`. `Ok(None)` if cancelled.
pub fn write_table<W: Write>(
    input: &StreamInput,
    fmt: &OutputFormat,
    out: W,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64),
) -> io::Result<Option<(Written, W)>> {
    let mut w = RowWriter {
        sink: Sink::new(out, fmt.out),
        rows: 0,
        at_line_start: true,
        last_empty: false,
        checkpoints: Vec::new(),
        fmt: *fmt,
    };
    w.sink.bom()?;
    let map_len = input.edits.cols.as_ref().map_or(0, |m| m.len());
    let mut header_len = 0usize;
    let mut error = None;
    let completed = stream::for_each(input, cancel, |row, pos| {
        let r = (|| {
            if !fmt.rewrite_all
                && let Some(raw) = row.unchanged_raw()
            {
                if row.key.is_header() {
                    header_len = row.len();
                }
                return w.raw(raw);
            }
            let len = if fmt.rewrite_all {
                row.len().max(map_len)
            } else if row.key.is_added() {
                header_len.max(row.edited_len())
            } else {
                row.len()
            };
            if row.key.is_header() {
                header_len = len;
            }
            w.fields(row, len)
        })();
        if let Err(e) = r {
            error = Some(e);
            return ControlFlow::Break(());
        }
        progress(pos);
        ControlFlow::Continue(())
    })?;
    if let Some(e) = error {
        return Err(e);
    }
    if !completed {
        return Ok(None);
    }
    w.finish()?;
    let written = Written {
        bytes: w.sink.position(),
        rows: w.rows,
        checkpoints: w.checkpoints,
    };
    Ok(Some((written, w.sink.into_inner())))
}

/// Any edited value needs a character Windows-1252 doesn't have.
pub fn needs_unicode(input: &StreamInput) -> bool {
    input.edits.cells.iter().any(|(_, row)| {
        row.iter()
            .any(|(_, v)| !v.is_ascii() && v.chars().any(|c| cp1252_byte(c).is_none()))
    })
}

// --- the save job ------------------------------------------------------------------

/// Where a save goes.
#[derive(Clone, Debug)]
pub enum SaveTarget {
    /// Replace (or create) this file.
    Path(PathBuf),
    /// A new table's first save: a free name in the notes library.
    Library { dir: PathBuf, stem: String },
}

pub struct SaveJob {
    pub input: StreamInput,
    pub format: OutputFormat,
    pub target: SaveTarget,
    /// The target is the document's own file, as loaded: refuse to replace it
    /// if another program changed it since (unless `overwrite`).
    pub expect: Option<FileIdentity>,
    pub overwrite: bool,
    pub cancel: Arc<AtomicBool>,
    /// Source bytes consumed so far.
    pub progress: Arc<AtomicU64>,
    pub waker: Waker,
}

/// A successful save: everything needed to show the new file.
pub struct Saved {
    pub path: PathBuf,
    pub source: Option<Arc<FileSource>>,
    /// The new file's index, when the writer's own checkpoints describe it.
    pub index: Option<IndexState>,
    /// How to read the new file.
    pub encoding: Encoding,
    pub header_offset: u64,
    pub dialect: Dialect,
    /// Rows were inserted/deleted: the new file's row `i` is the old logical row `i`.
    pub rows: Option<Arc<RowOrder>>,
    /// A Windows-1252 file was converted to UTF-8.
    pub converted: bool,
}

pub enum SaveOutcome {
    Saved(Box<Saved>),
    Cancelled,
    Failed(WriteError),
}

pub fn spawn(job: SaveJob) -> Receiver<SaveOutcome> {
    let (tx, rx) = channel();
    std::thread::Builder::new()
        .name("csv-save".into())
        .spawn(move || {
            let outcome = run(job.format, &job);
            let _ = tx.send(outcome);
            job.waker.wake();
        })
        .expect("spawn csv save");
    rx
}

/// The file a save really replaces (a symlink's target, so the link survives).
fn resolve(path: &Path) -> PathBuf {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => {
            std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
        }
        _ => path.to_path_buf(),
    }
}

fn preflight(
    job: &SaveJob,
    fmt: &OutputFormat,
    target: Option<&Path>,
    dir: &Path,
) -> Result<(), WriteError> {
    use windows::Win32::Storage::FileSystem::{
        DELETE, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        GetDiskFreeSpaceExW,
    };
    if let Some(p) = target {
        if let Some(expect) = job.expect
            && !job.overwrite
            && let Ok(now) = FileIdentity::of_path(p)
            && now != expect
        {
            return Err(WriteError::ChangedOnDisk);
        }
        if p.exists() {
            if is_read_only(p) {
                return Err(WriteError::ReadOnly);
            }
            // Excel holds open files without sharing delete: find out now, not
            // after writing the whole file.
            const ERROR_SHARING_VIOLATION: i32 = 32;
            let probe = std::fs::OpenOptions::new()
                .access_mode((DELETE | FILE_READ_ATTRIBUTES).0)
                .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE).0)
                .open(p);
            if let Err(e) = probe
                && e.raw_os_error() == Some(ERROR_SHARING_VIOLATION)
            {
                return Err(WriteError::Locked);
            }
        }
    }
    let need = job.input.source.len().saturating_mul(fmt.out.growth()) / 10 * 11;
    let mut free = 0u64;
    let dir_w = windows::core::HSTRING::from(dir.as_os_str());
    if unsafe { GetDiskFreeSpaceExW(&dir_w, Some(&mut free), None, None) }.is_ok() && free < need {
        return Err(WriteError::DiskFull);
    }
    Ok(())
}

fn run(mut fmt: OutputFormat, job: &SaveJob) -> SaveOutcome {
    if fmt.out == OutEncoding::Cp1252 && needs_unicode(&job.input) {
        fmt.out = OutEncoding::Cp1252ToUtf8Bom;
    }
    let (target, dir) = match &job.target {
        SaveTarget::Path(p) => {
            let real = resolve(p);
            let dir = real.parent().unwrap_or(Path::new(".")).to_path_buf();
            (Some(real), dir)
        }
        SaveTarget::Library { dir, .. } => (None, dir.clone()),
    };
    if let Err(e) = preflight(job, &fmt, target.as_deref(), &dir) {
        return SaveOutcome::Failed(e);
    }
    let (tmp, file) = match temp_in(&dir) {
        Ok(t) => t,
        Err(e) => return SaveOutcome::Failed(WriteError::from_io(e)),
    };
    let fail = |e: WriteError| {
        let _ = std::fs::remove_file(&tmp);
        SaveOutcome::Failed(e)
    };
    let mut last_wake = Instant::now();
    let written = write_table(
        &job.input,
        &fmt,
        BufWriter::with_capacity(1 << 20, file),
        &job.cancel,
        |pos| {
            job.progress.store(pos, Ordering::Relaxed);
            if last_wake.elapsed() > Duration::from_millis(100) {
                last_wake = Instant::now();
                job.waker.wake();
            }
        },
    );
    let (written, file) = match written {
        Ok(Some((w, buf))) => match buf.into_inner() {
            Ok(f) => (w, f),
            Err(e) => return fail(WriteError::from_io(e.into_error())),
        },
        Ok(None) => {
            let _ = std::fs::remove_file(&tmp);
            return SaveOutcome::Cancelled;
        }
        Err(e) => return fail(WriteError::from_io(e)),
    };
    if let Err(e) = file.sync_all() {
        return fail(WriteError::from_io(e));
    }
    drop(file);
    if job.cancel.load(Ordering::Relaxed) {
        let _ = std::fs::remove_file(&tmp);
        return SaveOutcome::Cancelled;
    }

    // From here on the save completes even if the tab closes.
    let path = match (&job.target, &target) {
        (SaveTarget::Path(p), Some(real)) => {
            let mut r = replace_file(real, &tmp);
            if matches!(r, Err(WriteError::Locked)) {
                // Our own reader shares delete, so this shouldn't happen; if a
                // handle of ours is in the way anyway, drop it and retry once.
                job.input.source.release();
                r = replace_file(real, &tmp);
            }
            if let Err(e) = r {
                return fail(e);
            }
            p.clone()
        }
        (SaveTarget::Library { dir, stem }, _) => {
            match whir_core::library::claim_name(&tmp, dir, stem, "csv") {
                Ok(p) => p,
                Err(e) => return fail(WriteError::from_io(e)),
            }
        }
        _ => unreachable!("a path target always resolves"),
    };

    let source = FileSource::open(&path).ok();
    let reusable = matches!(
        fmt.out,
        OutEncoding::Utf8 { .. } | OutEncoding::Cp1252 | OutEncoding::Cp1252ToUtf8Bom
    ) && source.as_ref().is_some_and(|s| s.len() == written.bytes);
    let terminator = if fmt.line_ending == b"\r" {
        b'\r'
    } else {
        b'\n'
    };
    let index = reusable.then(|| IndexState {
        checkpoints: written.checkpoints,
        rows: written.rows,
        scanned: written.bytes,
        total: written.bytes,
        done: true,
        quoting_disabled: !fmt.quoting,
        error: None,
    });
    SaveOutcome::Saved(Box::new(Saved {
        path,
        source,
        index,
        encoding: fmt.out.file_encoding(),
        header_offset: fmt.out.bom().len() as u64,
        dialect: Dialect {
            delimiter: fmt.delimiter,
            terminator,
            quoting: true,
        },
        rows: job.input.edits.rows.clone(),
        converted: fmt.out == OutEncoding::Cp1252ToUtf8Bom,
    }))
}

#[cfg(test)]
mod tests;
