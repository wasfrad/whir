//! The CSV tab: loading, saving, and the `Document` contract with the shell.
//!
//! Every CSV saves explicitly (Ctrl+S): the tab shows a dirty dot and closing
//! asks first. The undo history is cleared by a save — it describes changes
//! relative to the file as it was — so autosaving would make undo useless.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, RwLock};
use std::time::Instant;

use egui::{Rect, vec2};
use whir_core::file::WriteError;
use whir_core::library::paths_equal;
use whir_core::theme::Theme;
use whir_core::{
    DocCx, DocKind, Document, FileSource, Library, OpenCx, Persistence, SaveAsSpec, SaveResult,
    Waker,
};

use crate::dialect::{Encoding, Style, sniff, sniff_style};
use crate::index::{self, IndexState, SharedIndex};
use crate::loaded::{Loaded, Opened, PendingSave, SaveFailure, SaveKind, SaveState, ViewCarry};
use crate::source::{ByteSource, transcode_utf16};
use crate::write::{self, SaveJob, SaveOutcome, SaveTarget};

enum LoadMsg {
    Ready(Opened),
    Failed(String),
}

enum Phase {
    Loading {
        rx: Receiver<LoadMsg>,
        /// View state to restore (a reload after saving).
        carry: Option<Box<ViewCarry>>,
    },
    Ready(Box<Loaded>),
    Failed(String),
}

pub struct CsvDoc {
    /// `None` for a new table that was never saved.
    path: Option<PathBuf>,
    title: String,
    waker: Waker,
    library: Arc<Library>,
    phase: Phase,
    /// The last save failed (until the next successful one).
    failure: Option<SaveFailure>,
    /// The user dismissed the failure banner (the error still stands).
    failure_hidden: bool,
}

pub(crate) const UNTITLED: &str = "Untitled table";

fn title_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

impl CsvDoc {
    pub fn open(path: PathBuf, cx: &OpenCx) -> Self {
        let phase = Phase::Loading {
            rx: start_loading(path.clone(), cx.waker.clone()),
            carry: None,
        };
        Self {
            title: title_of(&path),
            path: Some(path),
            waker: cx.waker.clone(),
            library: cx.library.clone(),
            phase,
            failure: None,
            failure_hidden: false,
        }
    }

    fn reload(&mut self, carry: Option<Box<ViewCarry>>) {
        let Some(path) = self.path.clone() else {
            return;
        };
        if let Phase::Ready(l) = &self.phase {
            l.cancel_workers();
        }
        self.phase = Phase::Loading {
            rx: start_loading(path, self.waker.clone()),
            carry,
        };
    }

    fn file_name(&self) -> String {
        self.path
            .as_deref()
            .map(title_of)
            .unwrap_or_else(|| format!("{UNTITLED}.csv"))
    }

    fn loaded(&mut self) -> Option<&mut Loaded> {
        match &mut self.phase {
            Phase::Ready(l) => Some(l),
            _ => None,
        }
    }

    /// Start writing `target` in the background. Until the whole file has
    /// been indexed the save waits (queued): only then is the parse final (the
    /// indexer may still switch quoting off for an unbalanced quote).
    fn start_save(&mut self, target: SaveTarget, kind: SaveKind, overwrite: bool) -> SaveResult {
        let waker = self.waker.clone();
        let own = matches!(kind, SaveKind::Own);
        let Phase::Ready(l) = &mut self.phase else {
            return SaveResult::Failed("The table is still loading.".into());
        };
        if l.is_saving() {
            return SaveResult::Started; // one save at a time
        }
        if l.read_only_reason().is_some() {
            return SaveResult::Failed(
                "This file has too many rows (over 2,147,483,647) for whir to save.".into(),
            );
        }
        if !l.table.progress().done {
            l.pending_save = Some(PendingSave {
                target,
                kind,
                overwrite,
            });
            return SaveResult::Started;
        }
        let format = match &target {
            SaveTarget::Path(p) if !own => l.output_format(Some(p)),
            _ => l.output_format(None),
        };
        let input = l.stream_input();
        let total = input.source.len();
        let target_name = match &target {
            SaveTarget::Path(p) => title_of(p),
            SaveTarget::Library { stem, .. } => format!("{stem}.csv"),
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(AtomicU64::new(0));
        let job = SaveJob {
            input,
            format,
            target,
            // Replacing our own file: refuse if another program changed it.
            expect: if own { l.file_identity } else { None },
            overwrite,
            cancel: cancel.clone(),
            progress: progress.clone(),
            waker,
        };
        l.saving = Some(SaveState {
            rx: write::spawn(job),
            cancel,
            progress,
            total,
            kind,
            target_name,
        });
        SaveResult::Started
    }

    /// A save that couldn't even start is reported like one that failed.
    fn note_failure(&mut self, r: &SaveResult) {
        if let SaveResult::Failed(message) = r {
            self.failure = Some(SaveFailure {
                message: message.clone(),
                changed_on_disk: false,
            });
            self.failure_hidden = false;
        }
    }

    /// Save to the document's own file, or a new table's first save into the
    /// notes library (no dialog: new tables live next to the notes).
    fn save_own(&mut self, overwrite: bool) -> SaveResult {
        match self.path.clone() {
            Some(p) => self.start_save(SaveTarget::Path(p), SaveKind::Own, overwrite),
            None => match self.library.ensure_dir() {
                Ok(dir) => {
                    let stem =
                        whir_core::library::untitled_stem(UNTITLED, &whir_core::time::now_local());
                    let target = SaveTarget::Library {
                        dir: dir.to_path_buf(),
                        stem,
                    };
                    self.start_save(target, SaveKind::Own, false)
                }
                Err(_) => match self.save_as_spec() {
                    Some(spec) => SaveResult::NeedsPath(spec),
                    None => SaveResult::Unsupported,
                },
            },
        }
    }

    /// A finished save: show the new file (or report why it failed).
    fn finish_save(&mut self, kind: SaveKind, name: String, outcome: SaveOutcome) {
        let waker = self.waker.clone();
        match outcome {
            SaveOutcome::Saved(saved) => {
                self.failure = None;
                self.failure_hidden = false;
                let path = saved.path.clone();
                let converted = saved.converted;
                self.library.refresh(vec![path.clone()]);
                if kind == SaveKind::Export {
                    if let Some(l) = self.loaded() {
                        let msg = format!("Saved a copy as “{}”", title_of(&path));
                        l.status = Some((msg, f64::INFINITY));
                    }
                    return;
                }
                if kind == SaveKind::Rebind || self.path.is_none() {
                    self.title = title_of(&path);
                    self.path = Some(path);
                }
                let Some(l) = self.loaded() else { return };
                let carry = l.finish_save(*saved, &waker);
                let msg = if converted {
                    "Saved as UTF-8 (some characters aren't in Windows-1252)"
                } else {
                    "Saved"
                };
                l.status = Some((
                    msg.into(),
                    if converted {
                        f64::INFINITY
                    } else {
                        l.now + 3.0
                    },
                ));
                if let Some(carry) = carry {
                    self.reload(Some(Box::new(carry)));
                }
            }
            SaveOutcome::Cancelled => {
                if let Some(l) = self.loaded() {
                    let now = l.now;
                    l.set_status("Save cancelled", now);
                }
            }
            SaveOutcome::Failed(e) => {
                self.failure = Some(SaveFailure {
                    message: e.message(&name),
                    changed_on_disk: matches!(e, WriteError::ChangedOnDisk),
                });
                self.failure_hidden = false;
            }
        }
    }
}

fn start_loading(path: PathBuf, waker: Waker) -> Receiver<LoadMsg> {
    let (tx, rx) = channel();
    std::thread::Builder::new()
        .name("csv-open".into())
        .spawn(move || {
            let msg = match load(&path, &waker) {
                Ok(m) => LoadMsg::Ready(m),
                Err(e) => LoadMsg::Failed(e),
            };
            let _ = tx.send(msg);
            waker.wake();
        })
        .expect("spawn csv loader");
    rx
}

/// The first 64 KB after `offset`, whether that's the whole rest, and the last byte.
fn sample(src: &dyn ByteSource, offset: u64) -> Result<(Vec<u8>, bool, Option<u8>), String> {
    let mut buf = vec![0u8; 64 << 10];
    let n = src.read_at(offset, &mut buf).map_err(|e| e.to_string())?;
    buf.truncate(n);
    let whole = offset + (n as u64) >= src.len();
    let mut last = [0u8; 1];
    let last = match src.len() {
        0 => None,
        len => src
            .read_at(len - 1, &mut last)
            .ok()
            .filter(|&k| k == 1)
            .map(|_| last[0]),
    };
    Ok((buf, whole, last))
}

fn load(path: &Path, waker: &Waker) -> Result<Opened, String> {
    let file = FileSource::open(path).map_err(|e| format!("Couldn't open the file: {e}"))?;
    let identity = Some(file.identity());
    let (head, _, _) = sample(&*file, 0)?;
    let ext = path.extension().and_then(|e| e.to_str());
    let mut sniffed = sniff(&head, ext);
    let original_encoding = sniffed.encoding;
    let had_bom = sniffed.bom_len > 0;
    let source: Arc<dyn ByteSource> = match sniffed.encoding {
        Encoding::Utf16Le | Encoding::Utf16Be => {
            let big = sniffed.encoding == Encoding::Utf16Be;
            let temp = transcode_utf16(&*file, sniffed.bom_len, big, |_| {})
                .map_err(|e| format!("Couldn't convert the UTF-16 file: {e}"))?;
            let (head, _, _) = sample(&*temp, 0)?;
            sniffed = sniff(&head, ext);
            temp
        }
        _ => file,
    };
    let (body, whole, last) = sample(&*source, sniffed.bom_len)?;
    let style = sniff_style(&body, whole, last, &sniffed.dialect);
    let index: SharedIndex = Arc::new(RwLock::new(IndexState::default()));
    let index_cancel = Arc::new(AtomicBool::new(false));
    index::spawn(
        source.clone(),
        sniffed.bom_len,
        sniffed.dialect,
        index.clone(),
        waker.clone(),
        index_cancel.clone(),
    );
    Ok(Opened {
        source,
        dialect: sniffed.dialect,
        encoding: sniffed.encoding,
        header_offset: sniffed.bom_len,
        original_encoding,
        had_bom,
        style,
        identity,
        index,
        index_cancel,
        waker: waker.clone(),
    })
}

/// A new, never-saved table: three named columns, UTF-8 with BOM and CRLF
/// (what Excel expects), saved into the notes library on first save.
pub fn new_blank(cx: &OpenCx) -> CsvDoc {
    use crate::dialect::Dialect;
    use crate::source::MemSource;
    const HEADER: &[u8] = b"Column 1,Column 2,Column 3\r\n";
    let src = MemSource::new(HEADER.to_vec());
    let dialect = Dialect::default();
    let index = index::build_sync(&src, 0, dialect);
    let source: Arc<dyn ByteSource> = Arc::new(src);
    let mut l = Loaded::new(Opened {
        source,
        dialect,
        encoding: Encoding::Utf8,
        header_offset: 0,
        original_encoding: Encoding::Utf8,
        had_bom: true,
        style: Style::default(),
        identity: None,
        index: Arc::new(RwLock::new(index)),
        index_cancel: Arc::new(AtomicBool::new(false)),
        waker: cx.waker.clone(),
    });
    // Ready to type: the cursor sits on the "+" row.
    l.grid.sel = Some(crate::grid::Selection::single(crate::grid::CellPos {
        row: 0,
        col: 0,
    }));
    CsvDoc {
        path: None,
        title: UNTITLED.to_owned(),
        waker: cx.waker.clone(),
        library: cx.library.clone(),
        phase: Phase::Ready(Box::new(l)),
        failure: None,
        failure_hidden: false,
    }
}

impl Document for CsvDoc {
    fn title(&self) -> &str {
        &self.title
    }

    fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    fn kind(&self) -> DocKind {
        DocKind::Csv
    }

    fn is_loading(&self) -> bool {
        match &self.phase {
            Phase::Loading { .. } => true,
            Phase::Ready(l) => {
                let p = l.table.progress();
                p.rows == 0 && !p.done
            }
            Phase::Failed(_) => false,
        }
    }

    fn poll(&mut self) {
        if let Phase::Loading { rx, carry } = &mut self.phase
            && let Ok(msg) = rx.try_recv()
        {
            let carry = carry.take();
            self.phase = match msg {
                LoadMsg::Ready(opened) => {
                    let mut l = Loaded::new(opened);
                    if let Some(c) = carry {
                        l.apply_carry(*c, &self.waker);
                    }
                    Phase::Ready(Box::new(l))
                }
                LoadMsg::Failed(e) => Phase::Failed(e),
            };
        }
        let done = match &mut self.phase {
            Phase::Ready(l) => l.poll(),
            _ => None,
        };
        if let Some((kind, name, outcome)) = done {
            self.finish_save(kind, name, outcome);
        }
        // A save that waited for the index (dropped if a re-parse discarded
        // the edits it was for).
        let ready = match &mut self.phase {
            Phase::Ready(l) if l.pending_save.is_some() && l.table.progress().done => {
                let p = l.pending_save.take();
                p.filter(|p| p.kind != SaveKind::Own || l.is_dirty())
            }
            _ => None,
        };
        if let Some(p) = ready {
            let r = self.start_save(p.target, p.kind, p.overwrite);
            self.note_failure(&r);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut DocCx<'_>) {
        self.poll();
        let theme = Theme::current();
        let area = ui.max_rect();
        let waker = self.waker.clone();
        let mut request = None;
        match &mut self.phase {
            Phase::Loading { .. } => {
                ui.painter().rect_filled(area, 0.0, theme.content_bg);
                ui.put(
                    Rect::from_center_size(area.center(), vec2(24.0, 24.0)),
                    egui::Spinner::new().size(20.0),
                );
            }
            Phase::Failed(e) => {
                whir_core::message_doc::show_message(
                    ui,
                    "Can't open this CSV",
                    e,
                    self.path.as_deref(),
                );
            }
            Phase::Ready(l) => {
                let failure = (!self.failure_hidden)
                    .then_some(self.failure.as_ref())
                    .flatten();
                request = crate::show::show_loaded(ui, l, area, &theme, &waker, failure, cx);
            }
        }
        match request {
            Some(crate::show::Request::Reload) => self.reload(None),
            Some(crate::show::Request::Overwrite) => {
                self.failure = None;
                let r = self.save_own(true);
                self.note_failure(&r);
            }
            Some(crate::show::Request::DismissFailure) => self.failure_hidden = true,
            None => {}
        }
    }

    fn on_visibility(&mut self, visible: bool) {
        if !visible && let Some(l) = self.loaded() {
            l.commit_pending();
        }
    }

    // --- saving -------------------------------------------------------------

    fn persistence(&self) -> Persistence {
        Persistence::Explicit
    }

    fn is_dirty(&self) -> bool {
        match &self.phase {
            Phase::Ready(l) => l.is_dirty(),
            _ => false,
        }
    }

    fn is_saving(&self) -> bool {
        matches!(&self.phase, Phase::Ready(l) if l.is_saving())
    }

    fn save_error(&self) -> Option<&str> {
        self.failure.as_ref().map(|f| f.message.as_str())
    }

    fn is_pristine(&self) -> bool {
        self.path.is_none() && !self.is_dirty()
    }

    fn save(&mut self) -> SaveResult {
        let Some(l) = self.loaded() else {
            return match &self.phase {
                Phase::Failed(_) => SaveResult::Unsupported,
                _ => SaveResult::Saved,
            };
        };
        l.commit_pending();
        if l.is_saving() {
            return SaveResult::Started;
        }
        if !l.is_dirty() {
            return SaveResult::Saved;
        }
        let r = self.save_own(false);
        self.note_failure(&r);
        r
    }

    fn save_as_spec(&self) -> Option<SaveAsSpec> {
        if matches!(self.phase, Phase::Failed(_)) {
            return None;
        }
        let folder = self
            .path
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .or_else(|| self.library.dir().map(Path::to_path_buf));
        Some(SaveAsSpec {
            title: "Save table as".into(),
            suggested_name: self.file_name(),
            filters: vec![
                ("CSV (comma-separated)".into(), "*.csv".into()),
                ("TSV (tab-separated)".into(), "*.tsv;*.tab".into()),
                ("All files".into(), "*.*".into()),
            ],
            default_ext: "csv".into(),
            folder,
        })
    }

    fn save_as(&mut self, path: PathBuf) -> SaveResult {
        let Some(l) = self.loaded() else {
            return SaveResult::Failed("The table isn't loaded.".into());
        };
        l.commit_pending();
        if l.is_saving() {
            return SaveResult::Failed("A save is already running.".into());
        }
        let own = self.path.as_deref().is_some_and(|p| paths_equal(p, &path));
        let kind = if own {
            SaveKind::Own
        } else if self
            .path
            .as_deref()
            .is_some_and(|p| self.library.contains(p))
        {
            // A table in the library stays there; Save As exports a copy.
            SaveKind::Export
        } else {
            SaveKind::Rebind
        };
        let r = self.start_save(SaveTarget::Path(path), kind, false);
        self.note_failure(&r);
        r
    }

    fn flush_blocking(&mut self, _deadline: Instant) -> Result<(), String> {
        Ok(())
    }

    fn discard(&mut self) {
        if let Phase::Ready(l) = &mut self.phase {
            l.pending_save = None;
            if let Some(s) = &l.saving {
                s.cancel.store(true, Ordering::Relaxed);
            }
        }
    }
}

impl Drop for CsvDoc {
    fn drop(&mut self) {
        if let Phase::Ready(l) = &self.phase {
            l.cancel_workers();
            if let Some(s) = &l.saving {
                // A save still writing stops and deletes its temp file; one
                // already replacing the file finishes.
                s.cancel.store(true, Ordering::Relaxed);
            }
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod ui_tests;
