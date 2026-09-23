//! The CSV tab: loading, grid, find bar, sort, copy, and a slim status footer.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, RwLock};

use egui::{FontId, Frame, Key, Rect, RichText, Stroke, UiBuilder, pos2, vec2};
use whir_core::find_bar::{FindBar, FindStatus, fmt_int};
use whir_core::theme::Theme;
use whir_core::{DocCx, DocKind, Document, FileSource, OpenCx, Waker};

use crate::dialect::{Encoding, Sniffed, sniff};
use crate::edit::{Edits, MAX_EDIT_ROWS, RowKey};
use crate::find::{self, FindMsg};
use crate::grid::{self, Action, CellPos, Editable, Grid, GridIo, Selection};
use crate::index::{self, IndexState, SharedIndex};
use crate::order::Order;
use crate::sort::{self, Direction, SortMsg, SortSpec};
use crate::source::{ByteSource, transcode_utf16};
use crate::stream::StreamInput;
use crate::table::Table;
use crate::view::SheetView;

/// Copy at most this many cells / bytes to the clipboard.
const MAX_COPY_CELLS: u64 = 2_000_000;
const MAX_COPY_BYTES: usize = 64 << 20;

enum LoadMsg {
    Ready {
        source: Arc<dyn ByteSource>,
        sniffed: Sniffed,
        original_encoding: Encoding,
        index: SharedIndex,
        index_cancel: Arc<AtomicBool>,
    },
    Failed(String),
}

pub(crate) struct FindState {
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
        }
    }
}

impl FindState {
    fn status(&self) -> FindStatus {
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
pub(crate) struct Sorting {
    pub spec: SortSpec,
    pub cancel: Arc<AtomicBool>,
    pub rx: Receiver<SortMsg>,
    /// `edits.rev` when it started: later cell edits make the result stale.
    pub rev: u64,
}

pub(crate) struct Loaded {
    pub table: Table,
    pub grid: Grid,
    pub measured_rows: u64,
    pub original_encoding: Encoding,
    /// Where the header row starts (after a BOM).
    pub header_offset: u64,
    pub edits: Edits,
    pub order: Option<Order>,
    /// Bumped whenever a sort result is installed or cleared.
    pub order_gen: u64,
    pub sorting: Option<Sorting>,
    pub find: FindState,
    /// Transient footer message and when it expires.
    pub status: Option<(String, f64)>,
    pub index_cancel: Arc<AtomicBool>,
}

enum Phase {
    Loading(Receiver<LoadMsg>),
    Ready(Box<Loaded>),
    Failed(String),
}

pub struct CsvDoc {
    path: PathBuf,
    title: String,
    waker: Waker,
    phase: Phase,
}

impl CsvDoc {
    pub fn open(path: PathBuf, cx: &OpenCx) -> Self {
        let title = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let phase = Phase::Loading(start_loading(path.clone(), cx.waker.clone()));
        Self {
            path,
            title,
            waker: cx.waker.clone(),
            phase,
        }
    }

    fn reload(&mut self) {
        if let Phase::Ready(l) = &self.phase {
            l.cancel_workers();
        }
        self.phase = Phase::Loading(start_loading(self.path.clone(), self.waker.clone()));
    }
}

fn start_loading(path: PathBuf, waker: Waker) -> Receiver<LoadMsg> {
    let (tx, rx) = channel();
    std::thread::Builder::new()
        .name("csv-open".into())
        .spawn(move || {
            let msg = match load(&path, &waker) {
                Ok(m) => m,
                Err(e) => LoadMsg::Failed(e),
            };
            let _ = tx.send(msg);
            waker.wake();
        })
        .expect("spawn csv loader");
    rx
}

fn load(path: &Path, waker: &Waker) -> Result<LoadMsg, String> {
    let file = FileSource::open(path).map_err(|e| format!("Couldn't open the file: {e}"))?;
    let mut sample = vec![0u8; 64 << 10];
    let n = file.read_at(0, &mut sample).map_err(|e| e.to_string())?;
    sample.truncate(n);
    let ext = path.extension().and_then(|e| e.to_str());
    let mut sniffed = sniff(&sample, ext);
    let original_encoding = sniffed.encoding;
    let source: Arc<dyn ByteSource> = match sniffed.encoding {
        Encoding::Utf16Le | Encoding::Utf16Be => {
            let big = sniffed.encoding == Encoding::Utf16Be;
            let temp = transcode_utf16(&*file, sniffed.bom_len, big, |_| {})
                .map_err(|e| format!("Couldn't convert the UTF-16 file: {e}"))?;
            let mut sample = vec![0u8; 64 << 10];
            let n = temp.read_at(0, &mut sample).map_err(|e| e.to_string())?;
            sample.truncate(n);
            sniffed = sniff(&sample, ext);
            temp
        }
        _ => file,
    };
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
    Ok(LoadMsg::Ready {
        source,
        sniffed,
        original_encoding,
        index,
        index_cancel,
    })
}

impl Loaded {
    fn new(
        source: Arc<dyn ByteSource>,
        sniffed: Sniffed,
        original_encoding: Encoding,
        index: SharedIndex,
        index_cancel: Arc<AtomicBool>,
    ) -> Self {
        Self {
            table: Table::new(source, sniffed.dialect, sniffed.encoding, index),
            grid: Grid::default(),
            measured_rows: 0,
            original_encoding,
            header_offset: sniffed.bom_len,
            edits: Edits::default(),
            order: None,
            order_gen: 0,
            sorting: None,
            find: FindState::default(),
            status: None,
            index_cancel,
        }
    }

    /// Data rows in the file (so far, while indexing).
    pub fn base_rows(&self) -> u64 {
        self.table.progress().rows.saturating_sub(1)
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

    fn cancel_workers(&self) {
        self.index_cancel.store(true, Ordering::Relaxed);
        if let Some(c) = &self.find.cancel {
            c.store(true, Ordering::Relaxed);
        }
        if let Some(s) = &self.sorting {
            s.cancel.store(true, Ordering::Relaxed);
        }
    }

    fn poll(&mut self) {
        // Sort results.
        if let Some(s) = &self.sorting
            && let Ok(SortMsg::Done(result)) = s.rx.try_recv()
        {
            let rev = s.rev;
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
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    FindMsg::Matches(m) => {
                        self.find.raw.extend(m);
                        got = true;
                    }
                    FindMsg::Done { truncated } => {
                        self.find.running = false;
                        self.find.truncated = truncated;
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
        let mut v: Vec<(u64, u32)> = self
            .find
            .raw
            .iter()
            .filter_map(|&(k, c)| self.display_of(k).map(|d| (d, c)))
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

    fn step_match(&mut self, forward: bool) {
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
    fn toggle_sort(&mut self, col: usize, waker: &Waker, now: f64) {
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

    /// Selection as tab-separated text (Excel-compatible quoting).
    fn copy_text(&mut self) -> Option<(String, u64, bool)> {
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
        let base_rows = self.base_rows();
        let mut view = SheetView {
            table: &mut self.table,
            edits: &self.edits,
            order: self.order.as_ref(),
            base_rows,
        };
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
}

impl Document for CsvDoc {
    fn title(&self) -> &str {
        &self.title
    }

    fn path(&self) -> Option<&Path> {
        Some(&self.path)
    }

    fn kind(&self) -> DocKind {
        DocKind::Csv
    }

    fn is_loading(&self) -> bool {
        match &self.phase {
            Phase::Loading(_) => true,
            Phase::Ready(l) => {
                let p = l.table.progress();
                p.rows == 0 && !p.done
            }
            Phase::Failed(_) => false,
        }
    }

    fn poll(&mut self) {
        if let Phase::Loading(rx) = &self.phase
            && let Ok(msg) = rx.try_recv()
        {
            self.phase = match msg {
                LoadMsg::Ready {
                    source,
                    sniffed,
                    original_encoding,
                    index,
                    index_cancel,
                } => Phase::Ready(Box::new(Loaded::new(
                    source,
                    sniffed,
                    original_encoding,
                    index,
                    index_cancel,
                ))),
                LoadMsg::Failed(e) => Phase::Failed(e),
            };
        }
        if let Phase::Ready(l) = &mut self.phase {
            l.poll();
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _cx: &mut DocCx<'_>) {
        self.poll();
        let theme = Theme::current();
        let area = ui.max_rect();
        let waker = self.waker.clone();
        let mut reload = false;
        match &mut self.phase {
            Phase::Loading(_) => {
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
                    Some(&self.path),
                );
            }
            Phase::Ready(l) => {
                reload = show_loaded(ui, l, area, &theme, &waker);
            }
        }
        if reload {
            self.reload();
        }
    }
}

fn show_loaded(
    ui: &mut egui::Ui,
    l: &mut Loaded,
    area: Rect,
    theme: &Theme,
    waker: &Waker,
) -> bool {
    let now = ui.input(|i| i.time);
    // Unbalanced quotes restarted indexing without quoting: forget everything
    // derived from the first parse.
    if l.table.sync() {
        l.measured_rows = 0;
        l.grid.sel = None;
        if !l.find.searched.is_empty() {
            l.start_find(l.find.searched.clone(), waker);
        }
    }
    let progress = l.table.progress();
    let base_rows = progress.rows.saturating_sub(1);

    // Column widths from a sample, once the first rows are in (again at 1000 rows).
    let available = base_rows;
    let target = available.min(1000);
    if progress.rows > 0
        && (l.measured_rows == 0
            || (l.measured_rows < 1000
                && target > l.measured_rows
                && (target == 1000 || progress.done)))
    {
        let mut view = SheetView {
            table: &mut l.table,
            edits: &l.edits,
            order: l.order.as_ref(),
            base_rows,
        };
        grid::measure_columns(ui, &mut l.grid, &mut view, target, theme);
        l.measured_rows = target.max(1);
    }

    let footer_h = 24.0;
    let grid_rect = Rect::from_min_max(area.min, pos2(area.right(), area.bottom() - footer_h));
    let footer = Rect::from_min_max(pos2(area.left(), grid_rect.bottom()), area.max);

    // Shortcuts owned by the document.
    let keys = FindBar::consume_keys(ui);
    if keys.open {
        l.find.bar.open_with(None);
    }
    if keys.next {
        l.step_match(true);
    }
    if keys.prev {
        l.step_match(false);
    }

    let find_has_focus = l.find.bar.has_focus(ui.ctx());
    let escape = !find_has_focus
        && !egui::Popup::is_any_open(ui.ctx())
        && ui.input(|i| i.key_pressed(Key::Escape));
    if escape {
        if l.find.bar.open {
            l.find.bar.close();
        } else {
            l.grid.sel = None;
        }
    }

    let sort_spec = l.sort_spec();
    let sort_dim = l.sorting.is_some() || l.order.as_ref().is_some_and(|o| o.stale);
    let sorted = l.order.is_some();
    let loaded = &mut *l;
    let mut view = SheetView {
        table: &mut loaded.table,
        edits: &loaded.edits,
        order: loaded.order.as_ref(),
        base_rows,
    };
    let rows = view.rows();
    let out = loaded.grid.show(
        ui,
        grid_rect,
        GridIo {
            view: &mut view,
            sort: sort_spec,
            sort_dim,
            sorted,
            matches: &loaded.find.display,
            current_match: loaded.find.current,
            keyboard: !find_has_focus,
            editable: Editable::READ_ONLY,
        },
        theme,
    );
    for action in out.actions {
        match action {
            Action::ToggleSort(col) => l.toggle_sort(col, waker, now),
            Action::Sort(col, dir) => l.sort(col, Some(dir), waker, now),
            Action::OriginalOrder => l.sort(0, None, waker, now),
            Action::Copy => {
                if let Some((text, cells, truncated)) = l.copy_text() {
                    ui.ctx().copy_text(text);
                    let msg = if truncated {
                        format!(
                            "Copied the first {} cells (selection too large)",
                            fmt_int(cells)
                        )
                    } else {
                        format!(
                            "Copied {} cell{}",
                            fmt_int(cells),
                            if cells == 1 { "" } else { "s" }
                        )
                    };
                    l.set_status(msg, now);
                }
            }
            _ => {}
        }
    }

    if l.find.bar.open {
        let status = l.find.status();
        let out = l.find.bar.show(ui, grid_rect, grid::HEADER_H, &status);
        if let Some(q) = out.search {
            l.start_find(q, waker);
        }
        if out.next {
            l.step_match(true);
        }
        if out.prev {
            l.step_match(false);
        }
    }
    if l.sorting.is_some() || l.find.running || !progress.done {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(150));
    }

    // Footer.
    let p = ui.painter();
    p.rect_filled(footer, 0.0, theme.strip_bg);
    p.line_segment(
        [footer.left_top(), footer.right_top()],
        Stroke::new(1.0, theme.separator),
    );
    let small = FontId::proportional(theme.metrics.small_text_size);
    let cols = l.grid.cols();
    let mut left = format!("{} rows × {} columns", fmt_int(rows), cols);
    if !progress.done && progress.total > 0 {
        let pct = progress.scanned as f64 / progress.total as f64 * 100.0;
        left.push_str(&format!("   ·   indexing {pct:.0}%"));
    }
    if let Some(s) = &l.sorting {
        let mut view = SheetView {
            table: &mut l.table,
            edits: &l.edits,
            order: l.order.as_ref(),
            base_rows,
        };
        let name = view.header().cell(s.spec.column).text.to_owned();
        left.push_str(&format!(
            "   ·   sorting by {}…",
            if name.is_empty() { "column" } else { &name }
        ));
    }
    if let Some(sel) = l.grid.sel
        && sel.cell_count() > 1
    {
        left.push_str(&format!("   ·   {} selected", fmt_int(sel.cell_count())));
    }
    p.text(
        pos2(footer.left() + 10.0, footer.center().y),
        egui::Align2::LEFT_CENTER,
        left,
        small.clone(),
        theme.text_dim,
    );

    let mut right = format!(
        "{}  ·  {}",
        l.table.dialect.delimiter_label(),
        l.original_encoding.label()
    );
    if progress.quoting_disabled {
        right = format!("Unbalanced quotes: showing raw lines  ·  {right}");
    }
    if let Some(e) = &progress.error {
        right = format!("Stopped reading: {e}  ·  {right}");
    }
    if let Some((msg, until)) = &l.status {
        if now < *until {
            right = format!("{msg}   ·   {right}");
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs_f64((until - now).min(3.0)));
        } else {
            l.status = None;
        }
    }
    p.text(
        pos2(footer.right() - 10.0, footer.center().y),
        egui::Align2::RIGHT_CENTER,
        right,
        small,
        theme.text_dim,
    );

    // The file changed underneath us: offer a reload.
    if l.table.source.changed() {
        let banner = Rect::from_min_size(
            pos2(grid_rect.center().x - 170.0, grid_rect.top() + 34.0),
            vec2(340.0, 34.0),
        );
        let mut reload = false;
        ui.scope_builder(UiBuilder::new().max_rect(banner), |ui| {
            Frame::NONE
                .fill(theme.overlay_bg)
                .stroke(Stroke::new(1.0, theme.separator))
                .corner_radius(8.0)
                .inner_margin(6.0)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("This file changed on disk.").color(theme.text));
                        reload = ui.button("Reload").clicked();
                    });
                });
        });
        return reload;
    }
    false
}

impl Drop for CsvDoc {
    fn drop(&mut self) {
        if let Phase::Ready(l) = &self.phase {
            l.cancel_workers();
        }
    }
}
