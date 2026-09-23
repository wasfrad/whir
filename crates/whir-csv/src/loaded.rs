//! A loaded table: the file, the edit overlay, the view state (grid, sort,
//! find) and background work (sort, find, save).

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, RwLock};

use whir_core::Waker;
use whir_core::file::FileIdentity;
use whir_core::find_bar::{FindBar, FindStatus};

use crate::dialect::{Encoding, Style};
use crate::edit::{Edits, MAX_EDIT_ROWS, RowKey};
use crate::editor::Editor;
use crate::find::{self, FindMsg};
use crate::grid::{CellPos, Grid, Selection};
use crate::index::SharedIndex;
use crate::order::Order;
use crate::sort::{self, Direction, SortMsg, SortSpec};
use crate::source::ByteSource;
use crate::stream::StreamInput;
use crate::table::Table;
use crate::view::SheetView;
use crate::write::{OutEncoding, OutputFormat, SaveOutcome, Saved};

/// Copy at most this many cells / bytes to the clipboard.
const MAX_COPY_CELLS: u64 = 2_000_000;
const MAX_COPY_BYTES: usize = 64 << 20;

pub struct FindState {
    pub bar: FindBar,
    /// Query the current results belong to.
    pub searched: String,
    /// Matches by row key (stable across edits and sorting).
    pub raw: Vec<(RowKey, u32)>,
    /// (display row, col), sorted: what the grid highlights.
    pub display: Vec<(u64, u32)>,
    pub current: Option<usize>,
    pub running: bool,
    pub truncated: bool,
    pub cancel: Option<Arc<AtomicBool>>,
    pub rx: Option<Receiver<FindMsg>>,
    /// Cells edited while the search runs: its (stale) hits there are dropped.
    pub ignore: std::collections::HashSet<(RowKey, u32)>,
}

impl Default for FindState {
    fn default() -> Self {
        Self {
            bar: FindBar::new("Find in file", 0.2),
            searched: String::new(),
            raw: Vec::new(),
            display: Vec::new(),
            current: None,
            running: false,
            truncated: false,
            cancel: None,
            rx: None,
            ignore: Default::default(),
        }
    }
}

impl FindState {
    pub fn status(&self) -> FindStatus {
        let n = self.display.len();
        if self.searched.is_empty() {
            FindStatus::Idle
        } else if n == 0 {
            if self.running {
                FindStatus::Searching { progress: None }
            } else {
                FindStatus::NoResults
            }
        } else {
            FindStatus::Results {
                current: self.current,
                total: n,
                more: self.running || self.truncated,
            }
        }
    }
}

/// A sort being computed on its thread.
pub struct Sorting {
    pub spec: SortSpec,
    pub cancel: Arc<AtomicBool>,
    pub rx: Receiver<SortMsg>,
    /// `edits.rev` when it started: later cell edits make the result stale.
    pub rev: u64,
}

/// What a save does once written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveKind {
    /// The document's own file (or a new table's first save): show the result.
    Own,
    /// Save As elsewhere: the document now is that file.
    Rebind,
    /// Save As from a library table: a copy; the document stays on its file.
    Export,
}

/// A save running on its thread.
pub struct SaveState {
    pub rx: Receiver<SaveOutcome>,
    pub cancel: Arc<AtomicBool>,
    /// Source bytes written so far, of `total`.
    pub progress: Arc<AtomicU64>,
    pub total: u64,
    pub kind: SaveKind,
}

/// A save that failed and how the banner offers to recover.
#[derive(Clone, Debug)]
pub struct SaveFailure {
    pub message: String,
    /// Another program changed the file: offer "Overwrite".
    pub changed_on_disk: bool,
}

/// View state carried over a reload (a save whose output must be re-read).
pub struct ViewCarry {
    pub grid: Grid,
    pub measured_rows: u64,
    pub order: Option<Order>,
    pub find_query: String,
    pub find_open: bool,
    /// Rows expected once indexed (keeps the scroll position meanwhile).
    pub expected_rows: u64,
}

pub struct Loaded {
    pub table: Table,
    pub grid: Grid,
    pub measured_rows: u64,
    /// Encoding of the file on disk (UTF-16 files are read via a UTF-8 copy).
    pub original_encoding: Encoding,
    /// The file on disk started with a byte-order mark.
    pub had_bom: bool,
    /// Where the header row starts in the source (after a BOM).
    pub header_offset: u64,
    /// How rows are written (line endings, quoting).
    pub style: Style,
    /// Identity of the file as loaded (a save refuses to replace a newer version).
    pub file_identity: Option<FileIdentity>,
    pub edits: Edits,
    pub editor: Option<Editor>,
    /// The editor's widget id (one per document).
    pub editor_id: egui::Id,
    /// An editor closed without a context: its text state is dropped next frame.
    pub stale_editor: Option<egui::Id>,
    pub order: Option<Order>,
    /// Bumped whenever a sort result is installed or cleared.
    pub order_gen: u64,
    pub sorting: Option<Sorting>,
    pub find: FindState,
    pub saving: Option<SaveState>,
    /// Transient footer message and when it expires.
    pub status: Option<(String, f64)>,
    /// Rows a reload after saving will have (until indexing catches up).
    pub expected_rows: Option<u64>,
    pub index_cancel: Arc<AtomicBool>,
    /// Time of the current frame (for status messages set outside `ui`).
    pub now: f64,
    pub waker: Waker,
}

/// What the loader found.
pub struct Opened {
    pub source: Arc<dyn ByteSource>,
    pub dialect: crate::dialect::Dialect,
    pub encoding: Encoding,
    pub header_offset: u64,
    pub original_encoding: Encoding,
    pub had_bom: bool,
    pub style: Style,
    pub identity: Option<FileIdentity>,
    pub index: SharedIndex,
    pub index_cancel: Arc<AtomicBool>,
    pub waker: Waker,
}

impl Loaded {
    pub fn new(o: Opened) -> Self {
        Self {
            table: Table::new(o.source, o.dialect, o.encoding, o.index),
            grid: Grid::default(),
            measured_rows: 0,
            original_encoding: o.original_encoding,
            had_bom: o.had_bom,
            header_offset: o.header_offset,
            style: o.style,
            file_identity: o.identity,
            edits: Edits::default(),
            editor: None,
            editor_id: {
                static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                egui::Id::new(("csv-cell-editor", NEXT.fetch_add(1, Ordering::Relaxed)))
            },
            stale_editor: None,
            order: None,
            order_gen: 0,
            sorting: None,
            find: FindState::default(),
            saving: None,
            status: None,
            expected_rows: None,
            index_cancel: o.index_cancel,
            now: 0.0,
            waker: o.waker,
        }
    }

    /// Restore the view after a reload that followed a save.
    pub fn apply_carry(&mut self, carry: ViewCarry, waker: &Waker) {
        self.grid = carry.grid;
        self.measured_rows = carry.measured_rows;
        self.expected_rows = Some(carry.expected_rows);
        if let Some(order) = carry.order {
            self.install_order(Some(order));
        }
        if carry.find_open {
            self.find.bar.open = true;
        }
        if !carry.find_query.is_empty() {
            self.find.bar.query = carry.find_query.clone();
            self.start_find(carry.find_query, waker);
        }
    }

    /// Data rows in the file (so far, while indexing).
    pub fn base_rows(&self) -> u64 {
        let p = self.table.progress();
        let n = p.rows.saturating_sub(1);
        match self.expected_rows {
            Some(e) if !p.done => n.max(e),
            _ => n,
        }
    }

    /// Rows shown (excluding the header and the "+" row).
    pub fn rows(&self) -> u64 {
        match &self.order {
            Some(o) => o.len(),
            None => self.edits.logical_len(self.base_rows()),
        }
    }

    pub fn display_of(&self, k: RowKey) -> Option<u64> {
        match &self.order {
            Some(o) => o.display_of(k),
            None => self.edits.index_of(k, self.base_rows()),
        }
    }

    /// Everything a worker needs to read the table as it is now.
    pub fn stream_input(&self) -> StreamInput {
        StreamInput {
            source: self.table.source.clone(),
            dialect: self.table.effective_dialect(),
            header_offset: self.header_offset,
            index: self.table.index.clone(),
            edits: self.edits.snapshot(),
        }
    }

    pub fn set_status(&mut self, text: impl Into<String>, now: f64) {
        self.status = Some((text.into(), now + 3.0));
    }

    pub fn cancel_workers(&self) {
        self.index_cancel.store(true, Ordering::Relaxed);
        if let Some(c) = &self.find.cancel {
            c.store(true, Ordering::Relaxed);
        }
        if let Some(s) = &self.sorting {
            s.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Absorb finished sort and find work. Returns a finished save, if any.
    pub fn poll(&mut self) -> Option<(SaveKind, SaveOutcome)> {
        use std::sync::mpsc::TryRecvError;
        // Sort results (a worker that died without answering counts as failed,
        // so row edits don't stay blocked).
        let sorted = self.sorting.as_ref().and_then(|s| match s.rx.try_recv() {
            Ok(SortMsg::Done(result)) => Some((s.rev, result)),
            Err(TryRecvError::Disconnected) => Some((s.rev, Err("Sorting stopped.".into()))),
            Err(TryRecvError::Empty) => None,
        });
        if let Some((rev, result)) = sorted {
            self.sorting = None;
            match result {
                Ok(mut order) => {
                    order.stale = rev != self.edits.rev;
                    self.install_order(Some(order));
                }
                Err(e) if e != "cancelled" => {
                    self.status = Some((e, f64::INFINITY));
                }
                Err(_) => {}
            }
        }
        // Find results.
        let mut got = false;
        if let Some(rx) = &self.find.rx {
            loop {
                let msg = match rx.try_recv() {
                    Ok(msg) => msg,
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.find.running = false;
                        break;
                    }
                };
                match msg {
                    FindMsg::Matches(mut m) => {
                        if !self.find.ignore.is_empty() {
                            m.retain(|hit| !self.find.ignore.contains(hit));
                        }
                        self.find.raw.extend(m);
                        got = true;
                    }
                    FindMsg::Done { truncated } => {
                        self.find.running = false;
                        self.find.truncated = truncated;
                        self.find.ignore.clear();
                    }
                }
            }
        }
        if got {
            self.remap_matches();
            if self.find.current.is_none() && !self.find.display.is_empty() {
                self.find.current = Some(0);
                self.reveal_match();
            }
        }
        // A finished save.
        let done = self.saving.as_ref().and_then(|s| match s.rx.try_recv() {
            Ok(outcome) => Some(outcome),
            Err(TryRecvError::Disconnected) => {
                Some(SaveOutcome::Failed(whir_core::file::WriteError::Io(
                    std::io::Error::other("the save stopped unexpectedly"),
                )))
            }
            Err(TryRecvError::Empty) => None,
        });
        done.map(|outcome| {
            let kind = self.saving.take().map_or(SaveKind::Own, |s| s.kind);
            (kind, outcome)
        })
    }

    /// Show a new display order (or the file order).
    pub fn install_order(&mut self, order: Option<Order>) {
        self.order_gen += 1;
        self.table
            .set_cache_capacity(if order.is_some() { 1024 } else { 96 });
        self.order = order.map(|mut o| {
            o.generation = self.order_gen;
            o
        });
        self.remap_matches();
    }

    /// Recompute display positions of find matches (after sorting or row changes).
    pub fn remap_matches(&mut self) {
        let current = self
            .find
            .current
            .and_then(|i| self.find.display.get(i).copied());
        let base = self.base_rows();
        let (order, edits) = (&self.order, &self.edits);
        let mut v: Vec<(u64, u32)> = self
            .find
            .raw
            .iter()
            .filter_map(|&(k, c)| {
                let d = match order {
                    Some(o) => o.display_of(k),
                    None => edits.index_of(k, base),
                };
                d.map(|d| (d, c))
            })
            .collect();
        v.sort_unstable();
        self.find.display = v;
        if let Some(cur) = current {
            self.find.current = self
                .find
                .display
                .iter()
                .position(|&m| m == cur)
                .or(self.find.current)
                .filter(|&i| i < self.find.display.len());
        }
    }

    pub fn start_find(&mut self, query: String, waker: &Waker) {
        if let Some(c) = self.find.cancel.take() {
            c.store(true, Ordering::Relaxed);
        }
        self.find.raw.clear();
        self.find.display.clear();
        self.find.ignore.clear();
        self.find.current = None;
        self.find.truncated = false;
        self.find.searched = query;
        if self.find.searched.is_empty() {
            self.find.running = false;
            self.find.rx = None;
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();
        find::spawn(
            self.stream_input(),
            self.table.encoding,
            self.find.searched.clone(),
            cancel.clone(),
            tx,
            waker.clone(),
        );
        self.find.cancel = Some(cancel);
        self.find.rx = Some(rx);
        self.find.running = true;
    }

    pub fn step_match(&mut self, forward: bool) {
        let n = self.find.display.len();
        if n == 0 {
            return;
        }
        self.find.current = Some(match self.find.current {
            None => 0,
            Some(i) if forward => (i + 1) % n,
            Some(i) => (i + n - 1) % n,
        });
        self.reveal_match();
    }

    fn reveal_match(&mut self) {
        if let Some(&(row, col)) = self.find.current.and_then(|i| self.find.display.get(i)) {
            let cell = CellPos {
                row,
                col: col as usize,
            };
            self.grid.sel = Some(Selection::single(cell));
            self.grid.reveal(cell);
        }
    }

    /// Current (or computing) sort.
    pub fn sort_spec(&self) -> Option<SortSpec> {
        self.sorting
            .as_ref()
            .map(|s| s.spec)
            .or(self.order.as_ref().and_then(|o| o.spec))
    }

    /// Header click: ascending → descending → original order.
    pub fn toggle_sort(&mut self, col: usize, waker: &Waker, now: f64) {
        let next = match self.sort_spec() {
            Some(s) if s.column == col && s.direction == Direction::Ascending => {
                Some(Direction::Descending)
            }
            Some(s) if s.column == col && s.direction == Direction::Descending => None,
            _ => Some(Direction::Ascending),
        };
        self.sort(col, next, waker, now);
    }

    /// Sort by `col` (`None`: back to file order).
    pub fn sort(&mut self, col: usize, direction: Option<Direction>, waker: &Waker, now: f64) {
        let progress = self.table.progress();
        if direction.is_some() && (!progress.done || progress.error.is_some()) {
            self.set_status(
                "Sorting is available once the whole file has been read.",
                now,
            );
            return;
        }
        if direction.is_some() && self.base_rows() >= MAX_EDIT_ROWS {
            self.set_status("This file has too many rows to sort.", now);
            return;
        }
        if let Some(s) = self.sorting.take() {
            s.cancel.store(true, Ordering::Relaxed);
        }
        let Some(direction) = direction else {
            if self.order.is_some() {
                self.install_order(None);
            }
            return;
        };
        let spec = SortSpec {
            column: col,
            direction,
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();
        sort::spawn(
            self.stream_input(),
            self.table.encoding,
            spec,
            cancel.clone(),
            tx,
            waker.clone(),
        );
        self.sorting = Some(Sorting {
            spec,
            cancel,
            rx,
            rev: self.edits.rev,
        });
    }

    pub fn view(&mut self) -> SheetView<'_> {
        let base_rows = self.base_rows();
        SheetView {
            table: &mut self.table,
            edits: &self.edits,
            order: self.order.as_ref(),
            base_rows,
        }
    }

    /// Selection as tab-separated text (Excel-compatible quoting).
    pub fn copy_text(&mut self) -> Option<(String, u64, bool)> {
        let rows = self.rows();
        let sel = self.grid.sel?.clamped(rows, self.grid.cols())?;
        let (r0, r1) = sel.rows();
        let (c0, c1) = sel.cols();
        let cols = (c1 - c0 + 1) as u64;
        let max_rows = (MAX_COPY_CELLS / cols).max(1);
        let r1_capped = r1.min(r0 + max_rows - 1);
        let mut out = String::new();
        let mut truncated = r1_capped < r1;
        let mut n = 0u64;
        let mut view = self.view();
        for r in r0..=r1_capped {
            let row = view.row(r);
            for c in c0..=c1 {
                if let Some(row) = &row {
                    let cell = row.cell(c).text;
                    if cell.contains(['\t', '\n', '\r', '"']) {
                        out.push('"');
                        out.push_str(&cell.replace('"', "\"\""));
                        out.push('"');
                    } else {
                        out.push_str(cell);
                    }
                }
                if c < c1 {
                    out.push('\t');
                }
            }
            out.push('\n');
            n += 1;
            if out.len() > MAX_COPY_BYTES {
                truncated = true;
                break;
            }
        }
        if out.ends_with('\n') && n == 1 && c0 == c1 {
            out.pop(); // a single cell pastes without a trailing newline
        }
        Some((out, n * cols, truncated))
    }

    // --- saving -----------------------------------------------------------------

    /// How a save writes the table; `target` (Save As) may change the delimiter.
    pub fn output_format(&self, target: Option<&Path>) -> OutputFormat {
        let d = self.table.effective_dialect();
        let ext = target
            .and_then(|p| p.extension())
            .map(|e| e.to_string_lossy().to_ascii_lowercase());
        let delimiter = match ext.as_deref() {
            Some("tsv" | "tab") => b'\t',
            Some("csv") if d.delimiter == b'\t' => b',',
            _ => d.delimiter,
        };
        OutputFormat {
            delimiter,
            line_ending: self.style.line_ending,
            final_newline: self.style.final_newline,
            quote_all: self.style.quote_all,
            quoting: d.quoting,
            terminator: d.terminator,
            out: OutEncoding::keep(self.original_encoding, self.had_bom),
            rewrite_all: self.edits.cols().is_some() || delimiter != d.delimiter,
        }
    }

    /// Show the file a save just wrote, keeping the view (selection, scroll,
    /// widths, sort, find). The edits are now part of the file: the overlay
    /// and its undo history are cleared. Returns the carry if the new file has
    /// to be read the normal way (UTF-16, or its size isn't what was written).
    pub fn finish_save(&mut self, saved: Saved, waker: &Waker) -> Option<ViewCarry> {
        // Rows got their new file positions: the new row `i` is logical row `i`.
        if let Some(rows) = &saved.rows {
            let remap = |k: RowKey| rows.index_of(k).map(RowKey::orig);
            if let Some(o) = &mut self.order {
                o.translate(|k| remap(k).unwrap_or(k));
            }
            self.find.raw = self
                .find
                .raw
                .iter()
                .filter_map(|&(k, c)| remap(k).map(|k| (k, c)))
                .collect();
        }
        let expected_rows = self.rows();
        if let Some(ed) = self.editor.take() {
            self.stale_editor = Some(ed.id);
        }
        self.edits = Edits::default();
        // Work that read the old file restarts on the new one.
        let resort = self.sorting.take().map(|s| {
            s.cancel.store(true, Ordering::Relaxed);
            s.spec
        });
        let refind = self.find.running.then(|| self.find.searched.clone());
        self.original_encoding = saved.encoding;
        if !matches!(saved.encoding, Encoding::Utf16Le | Encoding::Utf16Be) {
            self.had_bom = saved.header_offset > 0;
        }
        let (Some(index), Some(source)) = (saved.index, saved.source) else {
            return Some(ViewCarry {
                grid: std::mem::take(&mut self.grid),
                measured_rows: self.measured_rows,
                order: self.order.take(),
                find_query: self.find.searched.clone(),
                find_open: self.find.bar.open,
                expected_rows,
            });
        };
        self.file_identity = Some(source.identity());
        self.index_cancel = Arc::new(AtomicBool::new(false));
        self.table = Table::new(
            source,
            saved.dialect,
            saved.encoding,
            Arc::new(RwLock::new(index)),
        );
        self.header_offset = saved.header_offset;
        self.expected_rows = None;
        self.edits.set_base_rows(self.base_rows());
        if let Some(spec) = resort {
            self.sort(spec.column, Some(spec.direction), waker, 0.0);
        }
        if let Some(q) = refind {
            self.start_find(q, waker);
        } else {
            self.remap_matches();
        }
        None
    }
}
