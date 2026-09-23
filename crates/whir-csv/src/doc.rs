//! The CSV tab: loading, grid, find bar, sort, copy, and a slim status footer.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, RwLock};

use egui::{
    Align, FontId, Frame, Key, Layout, Modifiers, Rect, RichText, Sense, Stroke, UiBuilder, pos2,
    vec2,
};
use whir_core::theme::Theme;
use whir_core::{DocCx, DocKind, Document, FileSource, OpenCx, Waker};

use crate::dialect::{Encoding, Sniffed, sniff};
use crate::find::{self, FindMsg};
use crate::grid::{self, CellPos, Grid, GridIo, Selection};
use crate::index::{self, IndexState, SharedIndex};
use crate::sort::{self, Direction, SortMsg, SortSpec, Sorted};
use crate::source::{ByteSource, transcode_utf16};
use crate::table::Table;

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

#[derive(Default)]
struct FindState {
    open: bool,
    focus: bool,
    query: String,
    /// Query the current results belong to.
    searched: String,
    /// Time of the last edit, for debouncing.
    edited_at: Option<f64>,
    raw: Vec<(u32, u32)>,
    /// (display row, col), sorted: what the grid highlights.
    display: Vec<(u64, u32)>,
    current: Option<usize>,
    running: bool,
    truncated: bool,
    cancel: Option<Arc<AtomicBool>>,
    rx: Option<Receiver<FindMsg>>,
}

struct Loaded {
    table: Table,
    grid: Grid,
    measured_rows: u64,
    original_encoding: Encoding,
    sorted: Option<Sorted>,
    sorting: Option<(SortSpec, Arc<AtomicBool>, Receiver<SortMsg>)>,
    find: FindState,
    /// Transient footer message and when it expires.
    status: Option<(String, f64)>,
    index_cancel: Arc<AtomicBool>,
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
            l.index_cancel.store(true, Ordering::Relaxed);
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
    fn data_rows(&self) -> u64 {
        match &self.sorted {
            Some(s) => s.perm.len() as u64,
            None => self.table.progress().rows.saturating_sub(1),
        }
    }

    fn perm(&self) -> Option<&[u32]> {
        self.sorted.as_ref().map(|s| &s.perm[..])
    }

    fn first_row_offset(&self) -> u64 {
        self.table
            .index
            .read()
            .ok()
            .and_then(|s| s.checkpoints.first().copied())
            .unwrap_or(0)
    }

    fn set_status(&mut self, text: impl Into<String>, now: f64) {
        self.status = Some((text.into(), now + 3.0));
    }

    fn poll(&mut self, waker: &Waker) {
        // Sort results.
        if let Some((spec, _, rx)) = &self.sorting
            && let Ok(SortMsg::Done(result)) = rx.try_recv()
        {
            let spec = *spec;
            self.sorting = None;
            match result {
                Ok(sorted) => {
                    self.table.set_cache_capacity(1024);
                    self.sorted = Some(sorted);
                    self.remap_matches();
                }
                Err(e) if e != "cancelled" => {
                    self.status = Some((e, f64::INFINITY));
                    let _ = spec;
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
        let _ = waker;
    }

    fn remap_matches(&mut self) {
        let current = self
            .find
            .current
            .and_then(|i| self.find.display.get(i).copied());
        self.find.display = match &self.sorted {
            Some(s) => {
                let mut v: Vec<(u64, u32)> = self
                    .find
                    .raw
                    .iter()
                    .filter_map(|&(r, c)| s.inverse.get(r as usize).map(|&d| (d as u64, c)))
                    .collect();
                v.sort_unstable();
                v
            }
            None => self.find.raw.iter().map(|&(r, c)| (r as u64, c)).collect(),
        };
        if let Some(cur) = current {
            self.find.current = self
                .find
                .display
                .iter()
                .position(|&m| m == cur)
                .or(self.find.current);
        }
    }

    fn start_find(&mut self, waker: &Waker) {
        if let Some(c) = self.find.cancel.take() {
            c.store(true, Ordering::Relaxed);
        }
        self.find.raw.clear();
        self.find.display.clear();
        self.find.current = None;
        self.find.truncated = false;
        self.find.searched = self.find.query.clone();
        if self.find.query.is_empty() {
            self.find.running = false;
            self.find.rx = None;
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();
        find::spawn(
            self.table.source.clone(),
            self.table.effective_dialect(),
            self.table.encoding,
            self.first_row_offset(),
            self.find.query.clone(),
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

    fn toggle_sort(&mut self, col: usize, waker: &Waker, now: f64) {
        let progress = self.table.progress();
        if !progress.done || progress.error.is_some() {
            self.set_status(
                "Sorting is available once the whole file has been read.",
                now,
            );
            return;
        }
        let current = self
            .sorting
            .as_ref()
            .map(|s| s.0)
            .or(self.sorted.as_ref().map(|s| s.spec));
        let next = match current {
            Some(s) if s.column == col && s.direction == Direction::Ascending => {
                Some(Direction::Descending)
            }
            Some(s) if s.column == col && s.direction == Direction::Descending => None,
            _ => Some(Direction::Ascending),
        };
        if let Some((_, cancel, _)) = self.sorting.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        let Some(direction) = next else {
            self.sorted = None;
            self.table.set_cache_capacity(96);
            self.remap_matches();
            return;
        };
        let spec = SortSpec {
            column: col,
            direction,
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();
        sort::spawn(
            self.table.source.clone(),
            self.table.effective_dialect(),
            self.first_row_offset(),
            spec,
            cancel.clone(),
            tx,
            waker.clone(),
        );
        self.sorting = Some((spec, cancel, rx));
    }

    /// Selection as tab-separated text (Excel-compatible quoting).
    fn copy_text(&mut self) -> Option<(String, u64, bool)> {
        let sel = self.grid.sel?;
        let (r0, r1) = sel.rows();
        let (c0, c1) = sel.cols();
        let cols = (c1 - c0 + 1) as u64;
        let max_rows = (MAX_COPY_CELLS / cols).max(1);
        let r1_capped = r1.min(r0 + max_rows - 1);
        let mut out = String::new();
        let mut truncated = r1_capped < r1;
        let mut rows = 0u64;
        for r in r0..=r1_capped {
            let data_row = self
                .perm()
                .map_or(r, |p| p.get(r as usize).copied().unwrap_or(0) as u64);
            let row = self.table.row(data_row + 1);
            for c in c0..=c1 {
                if let Some((b, i)) = &row
                    && let Some(view) = b.row(*i)
                {
                    let cell = view.cell(c);
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
            rows += 1;
            if out.len() > MAX_COPY_BYTES {
                truncated = true;
                break;
            }
        }
        if out.ends_with('\n') && rows == 1 && c0 == c1 {
            out.pop(); // a single cell pastes without a trailing newline
        }
        Some((out, rows * cols, truncated))
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
                } => Phase::Ready(Box::new(Loaded {
                    table: Table::new(source, sniffed.dialect, sniffed.encoding, index),
                    grid: Grid::default(),
                    measured_rows: 0,
                    original_encoding,
                    sorted: None,
                    sorting: None,
                    find: FindState::default(),
                    status: None,
                    index_cancel,
                })),
                LoadMsg::Failed(e) => Phase::Failed(e),
            };
        }
        if let Phase::Ready(l) = &mut self.phase {
            l.poll(&self.waker);
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
            l.start_find(waker);
        }
    }
    let progress = l.table.progress();

    // Column widths from a sample, once the first rows are in (again at 1000 rows).
    let available = progress.rows.saturating_sub(1);
    let target = available.min(1000);
    if progress.rows > 0
        && (l.measured_rows == 0
            || (l.measured_rows < 1000
                && target > l.measured_rows
                && (target == 1000 || progress.done)))
    {
        grid::measure_columns(ui, &mut l.grid, &mut l.table, target, theme);
        l.measured_rows = target.max(1);
    }

    let footer_h = 24.0;
    let grid_rect = Rect::from_min_max(area.min, pos2(area.right(), area.bottom() - footer_h));
    let footer = Rect::from_min_max(pos2(area.left(), grid_rect.bottom()), area.max);

    // Shortcuts owned by the document.
    let (open_find, f3, shift_f3, escape) = ui.input_mut(|i| {
        (
            i.consume_key(Modifiers::CTRL, Key::F),
            i.consume_key(Modifiers::NONE, Key::F3),
            i.consume_key(Modifiers::SHIFT, Key::F3),
            i.key_pressed(Key::Escape),
        )
    });
    if open_find {
        l.find.open = true;
        l.find.focus = true;
    }
    if f3 {
        l.step_match(true);
    }
    if shift_f3 {
        l.step_match(false);
    }

    let find_has_focus = l.find.open && ui.memory(|m| m.has_focus(egui::Id::new("csv-find-box")));
    if escape && !find_has_focus {
        if l.find.open {
            l.find.open = false;
        } else {
            l.grid.sel = None;
        }
    }

    let rows = l.data_rows();
    let sort_spec = l
        .sorting
        .as_ref()
        .map(|s| s.0)
        .or(l.sorted.as_ref().map(|s| s.spec));
    let sorting = l.sorting.is_some();
    let loaded = &mut *l;
    let out = loaded.grid.show(
        ui,
        grid_rect,
        GridIo {
            table: &mut loaded.table,
            rows,
            perm: loaded.sorted.as_ref().map(|s| s.perm.as_slice()),
            sort: sort_spec,
            sorting,
            matches: &loaded.find.display,
            current_match: loaded.find.current,
            keyboard: !find_has_focus,
        },
        theme,
    );
    if let Some(col) = out.sort_clicked {
        l.toggle_sort(col, waker, now);
    }
    if out.copy
        && let Some((text, cells, truncated)) = l.copy_text()
    {
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

    if l.find.open {
        find_bar(ui, l, grid_rect, theme, waker, now);
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
    if let Some((spec, _, _)) = &l.sorting {
        left.push_str(&format!(
            "   ·   sorting by {}…",
            l.grid
                .header
                .get(spec.column)
                .map_or("column", |s| s.as_str())
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

fn find_bar(
    ui: &mut egui::Ui,
    l: &mut Loaded,
    grid_rect: Rect,
    theme: &Theme,
    waker: &Waker,
    now: f64,
) {
    let size = vec2(390.0, 36.0);
    let rect = Rect::from_min_size(
        pos2(
            grid_rect.right() - size.x - 20.0,
            grid_rect.top() + grid::HEADER_H + 8.0,
        ),
        size,
    );
    // Swallow clicks on the bar's background so they don't reach the grid.
    ui.interact(rect, egui::Id::new("csv-find-bg"), Sense::click_and_drag());
    let mut close = false;
    let (mut next, mut prev) = (false, false);
    ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
        Frame::NONE
            .fill(theme.overlay_bg)
            .stroke(Stroke::new(1.0, theme.separator))
            .corner_radius(8.0)
            .inner_margin(egui::Margin::symmetric(8, 4))
            .show(ui, |ui| {
                ui.set_min_size(size - vec2(16.0, 8.0));
                ui.horizontal_centered(|ui| {
                    let edit = egui::TextEdit::singleline(&mut l.find.query)
                        .id(egui::Id::new("csv-find-box"))
                        .hint_text("Find in file")
                        // Enter steps through matches instead of dropping focus.
                        .return_key(None)
                        .desired_width(190.0);
                    let resp = ui.add(edit);
                    if l.find.focus {
                        resp.request_focus();
                        l.find.focus = false;
                    }
                    if resp.changed() {
                        l.find.edited_at = Some(now);
                    }
                    if resp.has_focus() {
                        let (shift_enter, enter, esc) = ui.input_mut(|i| {
                            (
                                i.consume_key(Modifiers::SHIFT, Key::Enter),
                                i.consume_key(Modifiers::NONE, Key::Enter),
                                i.consume_key(Modifiers::NONE, Key::Escape),
                            )
                        });
                        next |= enter;
                        prev |= shift_enter;
                        close |= esc;
                    }
                    let n = l.find.display.len();
                    let count = if l.find.query.is_empty() {
                        String::new()
                    } else if n == 0 {
                        if l.find.running {
                            "Searching…".into()
                        } else {
                            "No results".into()
                        }
                    } else {
                        let cur = l.find.current.map_or(0, |i| i + 1);
                        let more = if l.find.running || l.find.truncated {
                            "+"
                        } else {
                            ""
                        };
                        format!("{} of {}{more}", fmt_int(cur as u64), fmt_int(n as u64))
                    };
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        close |= ui
                            .add(egui::Button::new("✕").frame(false))
                            .on_hover_text("Close (Esc)")
                            .clicked();
                        next |= ui
                            .add(egui::Button::new("↓").frame(false))
                            .on_hover_text("Next (Enter, F3)")
                            .clicked();
                        prev |= ui
                            .add(egui::Button::new("↑").frame(false))
                            .on_hover_text("Previous (Shift+Enter)")
                            .clicked();
                        ui.label(RichText::new(count).small().color(theme.text_dim));
                    });
                });
            });
    });
    // Debounced search as you type.
    if let Some(t) = l.find.edited_at
        && now - t > 0.2
    {
        l.find.edited_at = None;
        if l.find.query != l.find.searched {
            l.start_find(waker);
        }
    } else if l.find.edited_at.is_some() {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(60));
    }
    if next || prev {
        if l.find.query != l.find.searched {
            l.start_find(waker);
        } else {
            l.step_match(next);
        }
    }
    if close {
        l.find.open = false;
    }
}

fn fmt_int(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

impl Drop for CsvDoc {
    fn drop(&mut self) {
        if let Phase::Ready(l) = &self.phase {
            l.index_cancel.store(true, Ordering::Relaxed);
            if let Some(c) = &l.find.cancel {
                c.store(true, Ordering::Relaxed);
            }
            if let Some((_, c, _)) = &l.sorting {
                c.store(true, Ordering::Relaxed);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn thousands() {
        assert_eq!(super::fmt_int(0), "0");
        assert_eq!(super::fmt_int(999), "999");
        assert_eq!(super::fmt_int(1000), "1,000");
        assert_eq!(super::fmt_int(1234567), "1,234,567");
    }
}
