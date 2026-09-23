//! The note tab: a title, a Markdown body styled as you type, and saving that
//! stays out of the way.
//!
//! Library notes (in `Documents\whir`) save themselves: about a second after
//! typing stops, when the tab is hidden, when whir loses focus, and on exit.
//! Other `.md`/`.txt` files are the user's documents: they save on Ctrl+S.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use egui::text::{CCursor, CCursorRange};
use egui::{
    Color32, CursorIcon, Event, EventFilter, FontId, Frame, Galley, Key, KeyboardShortcut, Margin,
    Modifiers, Pos2, Rect, RichText, Sense, Shape, Stroke, TextEdit, UiBuilder, pos2, vec2,
};
use whir_core::file::FileIdentity;
use whir_core::library::{self, Library};
use whir_core::scroll::{ScrollAxis, scrollbar};
use whir_core::theme::{self, Theme};
use whir_core::{
    DocCx, DocKind, DocRequest, Document, OpenCx, Persistence, SaveAsSpec, SaveResult, Waker,
};

use crate::editing::{self, Edit};
use crate::markdown::{LineKind, MdCache, MdStyle, Plain};
use crate::note_file::{self, Format, Header, TextEncoding};
use crate::saver::{self, Reply, SaveErr, Target};

/// Quiet time after the last keystroke before an autosave.
const AUTOSAVE_DELAY: Duration = Duration::from_millis(1000);
/// Longest an edit can stay unsaved while typing continuously.
const AUTOSAVE_MAX_DELAY: Duration = Duration::from_secs(10);
/// Larger files lose Markdown styling (TextEdit re-lays out and copies the
/// whole text as you type).
const STYLED_LIMIT: u64 = 256 << 10;
/// Larger files aren't opened as notes at all.
const OPEN_LIMIT: u64 = 8 << 20;
/// Retry delays after a failed autosave (then every 30 s).
const RETRY: [u64; 3] = [2, 5, 15];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    /// In the notes library: title field, autosave.
    Library,
    /// Any other text file: explicit save, the whole file is the body.
    External,
}

struct Loaded {
    title: String,
    body: String,
    header: Header,
    format: Format,
    identity: Option<FileIdentity>,
    mode: Mode,
    plain: Plain,
}

enum Phase {
    Loading(Receiver<Result<Loaded, String>>),
    Ready,
    Failed(String),
}

struct Inflight {
    rx: Receiver<Reply>,
    /// The header of the text being written (becomes ours on success).
    header: Header,
    user: bool,
}

#[derive(Clone, Copy)]
enum Focus {
    Title,
    /// Caret at this char index, or wherever it was.
    Body(Option<usize>),
}

pub struct NoteDoc {
    title_id: egui::Id,
    body_id: egui::Id,
    scroll_id: egui::Id,
    library: Arc<Library>,
    waker: Waker,
    mode: Mode,
    path: Option<PathBuf>,
    phase: Phase,
    /// A reload replacing a ready note (external edit accepted).
    reload: Option<Receiver<Result<Loaded, String>>>,
    title: String,
    body: String,
    header: Header,
    format: Format,
    plain: Plain,
    tab_title: String,

    content_hash: u64,
    saved_hash: u64,
    edited_at: Option<Instant>,
    first_unsaved: Option<Instant>,
    inflight: Option<Inflight>,
    /// Ctrl+S arrived during a write: save again right after it.
    resave: bool,
    error: Option<String>,
    retry_at: Option<Instant>,
    retries: usize,
    /// Changed on disk by another program while we have unsaved edits.
    conflict: bool,
    identity: Option<FileIdentity>,
    cancel: Arc<AtomicBool>,

    md: MdCache,
    toast: Option<(String, f64)>,
    queued_toast: Option<String>,
    focus: Option<Focus>,
    pending_scroll: Option<f32>,
    /// Chars on the body's first visual row (↑ from there goes to the title).
    first_row_chars: usize,
    /// Last frame's body galley and its screen position (margin clicks).
    last_body: Option<(Arc<Galley>, Pos2)>,
    body_height: f32,
    ctx: Option<egui::Context>,
}

fn next_uid() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn hash_of(title: &str, body: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    title.hash(&mut h);
    body.hash(&mut h);
    h.finish()
}

fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| library::NOTE_EXTS.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

fn load(path: &Path, library: &Library) -> Result<Loaded, String> {
    use std::io::Read as _;
    let mut f =
        whir_core::file::open_shared(path).map_err(|e| format!("Couldn't open the file: {e}"))?;
    let identity = FileIdentity::of(&f).ok();
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if len > OPEN_LIMIT {
        return Err(format!(
            "This file is too large to edit as a note ({} MB).",
            len >> 20
        ));
    }
    let mut bytes = Vec::with_capacity(len as usize);
    f.read_to_end(&mut bytes)
        .map_err(|e| format!("Couldn't read the file: {e}"))?;
    let (text, encoding) =
        note_file::decode(&bytes).map_err(|_| "This doesn't look like a text file.".to_owned())?;
    let mode = if library.contains(path) && is_markdown(path) {
        Mode::Library
    } else {
        Mode::External
    };
    let parsed = note_file::parse(&text, encoding, mode == Mode::Library);
    let plain = if !is_markdown(path) {
        Plain::Monospace
    } else if len > STYLED_LIMIT {
        Plain::Proportional
    } else {
        Plain::No
    };
    Ok(Loaded {
        title: parsed.title,
        body: parsed.body,
        header: parsed.header,
        format: parsed.format,
        identity,
        mode,
        plain,
    })
}

fn start_load(
    path: PathBuf,
    library: Arc<Library>,
    waker: Waker,
) -> Receiver<Result<Loaded, String>> {
    let (tx, rx) = channel();
    std::thread::Builder::new()
        .name("note-open".into())
        .spawn(move || {
            let _ = tx.send(load(&path, &library));
            waker.wake();
        })
        .expect("spawn note loader");
    rx
}

impl NoteDoc {
    fn blank(cx: &OpenCx) -> Self {
        let uid = next_uid();
        let id = egui::Id::new(("whir-note", uid));
        Self {
            title_id: id.with("title"),
            body_id: id.with("body"),
            scroll_id: id.with("scroll"),
            library: cx.library.clone(),
            waker: cx.waker.clone(),
            mode: Mode::Library,
            path: None,
            phase: Phase::Ready,
            reload: None,
            title: String::new(),
            body: String::new(),
            header: Header::NEW,
            format: Format::NEW,
            plain: Plain::No,
            tab_title: "New note".into(),
            content_hash: hash_of("", ""),
            saved_hash: hash_of("", ""),
            edited_at: None,
            first_unsaved: None,
            inflight: None,
            resave: false,
            error: None,
            retry_at: None,
            retries: 0,
            conflict: false,
            identity: None,
            cancel: Arc::new(AtomicBool::new(false)),
            md: MdCache::default(),
            toast: None,
            queued_toast: None,
            focus: Some(Focus::Body(None)),
            pending_scroll: None,
            first_row_chars: 0,
            last_body: None,
            body_height: 0.0,
            ctx: None,
        }
    }

    /// A new, empty library note. Touches nothing on disk until there's text.
    pub fn new_note(cx: &OpenCx) -> Self {
        Self::blank(cx)
    }

    pub fn open(path: PathBuf, cx: &OpenCx) -> Self {
        let mut doc = Self::blank(cx);
        doc.tab_title = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        doc.mode = if cx.library.contains(&path) && is_markdown(&path) {
            Mode::Library
        } else {
            Mode::External
        };
        doc.phase = Phase::Loading(start_load(
            path.clone(),
            cx.library.clone(),
            cx.waker.clone(),
        ));
        doc.path = Some(path);
        doc
    }

    fn apply_loaded(&mut self, l: Loaded) {
        self.title = l.title;
        self.body = l.body;
        self.header = l.header;
        self.format = l.format;
        self.identity = l.identity;
        self.mode = l.mode;
        self.plain = l.plain;
        self.content_hash = hash_of(&self.title, &self.body);
        self.saved_hash = self.content_hash;
        self.edited_at = None;
        self.first_unsaved = None;
        self.conflict = false;
        self.error = None;
        theme::note_text(&self.title);
        theme::note_text(&self.body);
        self.update_tab_title();
    }

    fn is_blank(&self) -> bool {
        self.title.trim().is_empty() && self.body.trim().is_empty()
    }

    fn file_stem(&self) -> Option<String> {
        self.path
            .as_deref()
            .and_then(Path::file_stem)
            .map(|s| s.to_string_lossy().into_owned())
    }

    fn update_tab_title(&mut self) {
        self.tab_title = match self.mode {
            Mode::External => self
                .path
                .as_deref()
                .and_then(Path::file_name)
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Untitled".into()),
            Mode::Library => {
                let t = self.title.trim();
                if !t.is_empty() {
                    t.to_owned()
                } else if let Some(stem) =
                    self.file_stem().filter(|s| !library::is_untitled_stem(s))
                {
                    stem
                } else {
                    self.body
                        .lines()
                        .map(library::strip_markdown)
                        .find(|l| !l.is_empty())
                        .map(|l| l.chars().take(60).collect())
                        .unwrap_or_else(|| "New note".into())
                }
            }
        };
    }

    /// Title or body changed.
    fn touched(&mut self) {
        let h = hash_of(&self.title, &self.body);
        if h == self.content_hash {
            return;
        }
        self.content_hash = h;
        let now = Instant::now();
        self.edited_at = Some(now);
        if self.content_hash != self.saved_hash {
            self.first_unsaved.get_or_insert(now);
        } else {
            self.first_unsaved = None;
        }
        self.update_tab_title();
    }

    fn toast(&mut self, text: impl Into<String>) {
        self.queued_toast = Some(text.into());
    }

    // --- saving ---------------------------------------------------------------

    /// Build the write for the current content. `None`: nothing to write.
    fn make_job(&mut self, user: bool) -> Option<(saver::Job, Receiver<Reply>, Header)> {
        let (reply, rx) = channel();
        let hash = self.content_hash;
        let blank = self.is_blank();
        let (target, bytes, header) = if blank {
            // An emptied library note leaves no empty file behind.
            match (self.mode, &self.path) {
                (Mode::Library, Some(p)) => {
                    (Target::DeleteIfEmpty(p.clone()), Vec::new(), Header::NEW)
                }
                (Mode::Library, None) => {
                    self.saved_hash = hash;
                    self.first_unsaved = None;
                    return None;
                }
                // Explicit saves of a blank external file do write it.
                (Mode::External, _) => (
                    Target::Path(self.path.clone()?),
                    self.encode("")?,
                    self.header.clone(),
                ),
            }
        } else {
            let text = note_file::compose(&self.title, &self.body, &self.header, &self.format);
            let written = note_file::parse(&text, self.format.encoding, self.mode == Mode::Library);
            let bytes = self.encode(&text)?;
            let target = match (self.mode, &self.path) {
                (Mode::Library, None) => {
                    let dir = match self.library.ensure_dir() {
                        Ok(d) => d.to_path_buf(),
                        Err(e) => {
                            self.error = Some(format!("Couldn't create the notes folder: {e}"));
                            return None;
                        }
                    };
                    let stem = library::sanitize_stem(&self.title).unwrap_or_else(|| {
                        library::untitled_stem("Untitled", &whir_core::time::now_local())
                    });
                    Target::LibraryNew { dir, stem }
                }
                (Mode::Library, Some(p)) => {
                    let current = self.file_stem().unwrap_or_default();
                    let rename_to = library::sanitize_stem(&self.title)
                        .filter(|want| !library::stem_matches(&current, want));
                    Target::Library {
                        path: p.clone(),
                        rename_to,
                    }
                }
                (Mode::External, Some(p)) => Target::Path(p.clone()),
                (Mode::External, None) => return None,
            };
            (target, bytes, written.header)
        };
        let _ = user;
        Some((
            saver::Job {
                target,
                bytes,
                expect: self.identity,
                cancel: self.cancel.clone(),
                hash,
                reply,
                waker: self.waker.clone(),
                library: self.library.clone(),
            },
            rx,
            header,
        ))
    }

    fn encode(&mut self, text: &str) -> Option<Vec<u8>> {
        match note_file::encode(text, &self.format) {
            Ok(b) => Some(b),
            Err(_) => {
                // A character the file's legacy encoding can't hold: switch to UTF-8.
                self.format.encoding = TextEncoding::Utf8 { bom: true };
                self.toast("Saved as UTF-8 (some characters aren't in the file's encoding)");
                note_file::encode(text, &self.format).ok()
            }
        }
    }

    fn submit(&mut self, user: bool) -> bool {
        if self.inflight.is_some() {
            self.resave |= user;
            return true;
        }
        if self.conflict {
            return false;
        }
        let Some((job, rx, header)) = self.make_job(user) else {
            return false;
        };
        saver::submit(job);
        self.inflight = Some(Inflight { rx, header, user });
        true
    }

    fn handle_reply(&mut self, r: Reply, header: Header, user: bool) {
        if r.export {
            match r.result {
                Ok(()) => self.toast("Exported"),
                Err(SaveErr::Io(e)) => self.error = Some(e),
                Err(SaveErr::Conflict) => {}
            }
            return;
        }
        match r.result {
            Ok(()) => {
                let renamed = r.path != self.path;
                self.path = r.path;
                self.identity = r.identity;
                self.saved_hash = r.hash;
                if self.path.is_none() {
                    self.header = Header::NEW;
                } else if self.mode == Mode::Library || header.title.is_some() {
                    self.header = header;
                }
                self.error = None;
                self.retries = 0;
                self.retry_at = None;
                if self.content_hash == self.saved_hash {
                    self.first_unsaved = None;
                }
                if user {
                    self.toast("Saved");
                }
                if renamed {
                    self.update_tab_title();
                }
            }
            Err(SaveErr::Conflict) => {
                if let Some(p) = r.path {
                    self.path = Some(p);
                }
                self.conflict = true;
            }
            Err(SaveErr::Io(e)) => {
                if let Some(p) = r.path {
                    self.path = Some(p);
                }
                if e != "cancelled" {
                    self.error = Some(e);
                    let delay = RETRY.get(self.retries).copied().unwrap_or(30);
                    self.retries += 1;
                    self.retry_at = Some(Instant::now() + Duration::from_secs(delay));
                }
            }
        }
    }

    fn drain(&mut self) {
        if let Phase::Loading(rx) = &self.phase
            && let Ok(result) = rx.try_recv()
        {
            self.phase = match result {
                Ok(l) => {
                    self.apply_loaded(l);
                    Phase::Ready
                }
                Err(e) => Phase::Failed(e),
            };
        }
        if let Some(rx) = &self.reload
            && let Ok(result) = rx.try_recv()
        {
            self.reload = None;
            match result {
                Ok(l) => self.apply_loaded(l),
                Err(e) => self.error = Some(e),
            }
        }
        if let Some(inf) = &self.inflight
            && let Ok(reply) = inf.rx.try_recv()
        {
            let inf = self.inflight.take().expect("checked");
            self.handle_reply(reply, inf.header, inf.user);
            if std::mem::take(&mut self.resave) && self.is_dirty() {
                self.submit(true);
            }
        }
    }

    /// When the next autosave is due, if one is pending.
    fn autosave_due(&self) -> Option<Instant> {
        if self.mode != Mode::Library
            || !matches!(self.phase, Phase::Ready)
            || self.conflict
            || self.inflight.is_some()
            || self.content_hash == self.saved_hash
        {
            return None;
        }
        if let Some(t) = self.retry_at {
            return Some(t);
        }
        let quiet = self.edited_at.map(|t| t + AUTOSAVE_DELAY);
        let forced = self.first_unsaved.map(|t| t + AUTOSAVE_MAX_DELAY);
        match (quiet, forced) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn reload(&mut self) {
        if let Some(path) = self.path.clone() {
            self.reload = Some(start_load(path, self.library.clone(), self.waker.clone()));
        }
    }

    // --- UI -------------------------------------------------------------------

    /// Keys the note handles itself, before the TextEdits see them.
    fn pre_edit_keys(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        // Title: Enter / ↓ / Tab go to the body.
        if ctx.memory(|m| m.has_focus(self.title_id)) {
            let go = ui.input_mut(|i| {
                i.consume_key(Modifiers::NONE, Key::Enter)
                    | i.consume_key(Modifiers::NONE, Key::ArrowDown)
                    | i.consume_key(Modifiers::NONE, Key::Tab)
            });
            if go {
                self.focus = Some(Focus::Body(Some(0)));
            }
            return;
        }
        if !ctx.memory(|m| m.has_focus(self.body_id)) {
            return;
        }
        let Some(mut state) = TextEdit::load_state(&ctx, self.body_id) else {
            return;
        };
        let Some(range) = state.cursor.char_range() else {
            return;
        };
        let (a, b) = (range.primary.index.0, range.secondary.index.0);
        let mut sel = a.min(b)..a.max(b);

        // ↑ on the first visual row moves up into the title.
        if self.mode == Mode::Library
            && sel.is_empty()
            && sel.start <= self.first_row_chars
            && ui.input_mut(|i| keys_exact(i, Modifiers::NONE, Key::ArrowUp))
        {
            self.focus = Some(Focus::Title);
            return;
        }

        let events = ui.input(|i| i.events.clone());
        let Some(k) = events.iter().position(|e| special(e).is_some()) else {
            return;
        };
        // Only plain typing may come first; it's applied here, in order.
        let preceding_ok = events[..k].iter().all(|e| {
            !matches!(
                e,
                Event::Key { pressed: true, .. } | Event::Paste(_) | Event::Cut | Event::Ime(_)
            )
        });
        if !preceding_ok {
            return;
        }
        let texts: Vec<usize> = (0..k)
            .filter(|&i| matches!(events[i], Event::Text(_)))
            .collect();
        let key = special(&events[k]).expect("found above");
        let mut body = self.body.clone();
        for &i in &texts {
            if let Event::Text(t) = &events[i] {
                let e = Edit {
                    replace: sel.clone(),
                    insert: t.clone(),
                    select: 0..0,
                };
                editing::apply(&mut body, &e);
                let c = sel.start + t.chars().count();
                sel = c..c;
            }
        }
        let edit = match key {
            Special::Enter => editing::on_enter(&body, sel.clone()),
            Special::Tab => editing::indent(&body, sel.clone(), false),
            Special::ShiftTab => editing::indent(&body, sel.clone(), true),
            Special::CtrlEnter => editing::toggle_task(&body, sel.end),
            Special::Bold => Some(editing::wrap(&body, sel.clone(), "**")),
            Special::Italic => Some(editing::wrap(&body, sel.clone(), "*")),
            Special::Link => Some(editing::link(&body, sel.clone())),
            Special::Swallow => None,
        };
        // Enter / Tab with nothing special to do: leave them to the TextEdit,
        // unless typing had to be applied first.
        if edit.is_none() && matches!(key, Special::Enter | Special::Tab) && texts.is_empty() {
            return;
        }
        let mut remove: Vec<usize> = texts;
        remove.push(k);
        let mut final_sel = sel.clone();
        match edit {
            Some(e) => {
                editing::apply(&mut body, &e);
                final_sel = e.select.clone();
            }
            None if matches!(key, Special::Enter) => {
                let e = Edit {
                    replace: sel.clone(),
                    insert: "\n".into(),
                    select: 0..0,
                };
                editing::apply(&mut body, &e);
                final_sel = sel.start + 1..sel.start + 1;
            }
            None if matches!(key, Special::Tab) => {
                let e = Edit {
                    replace: sel.clone(),
                    insert: "\t".into(),
                    select: 0..0,
                };
                editing::apply(&mut body, &e);
                final_sel = sel.start + 1..sel.start + 1;
            }
            None => {}
        }
        ui.input_mut(|i| {
            for &idx in remove.iter().rev() {
                if idx < i.events.len() {
                    i.events.remove(idx);
                }
            }
        });
        if body != self.body {
            self.body = body;
            self.touched();
        }
        state.cursor.set_char_range(Some(CCursorRange::two(
            CCursor::new(final_sel.start),
            CCursor::new(final_sel.end),
        )));
        state.store(&ctx, self.body_id);
    }

    fn apply_focus(&mut self, ctx: &egui::Context) {
        let Some(f) = self.focus.take() else { return };
        match f {
            Focus::Title => {
                let n = self.title.chars().count();
                let mut st = TextEdit::load_state(ctx, self.title_id).unwrap_or_default();
                st.cursor
                    .set_char_range(Some(CCursorRange::one(CCursor::new(n))));
                st.store(ctx, self.title_id);
                ctx.memory_mut(|m| m.request_focus(self.title_id));
            }
            Focus::Body(at) => {
                let mut st = TextEdit::load_state(ctx, self.body_id).unwrap_or_default();
                let pos = at.or_else(|| {
                    st.cursor
                        .char_range()
                        .is_none()
                        .then(|| self.body.chars().count())
                });
                if let Some(p) = pos {
                    st.cursor
                        .set_char_range(Some(CCursorRange::one(CCursor::new(p))));
                }
                st.store(ctx, self.body_id);
                ctx.memory_mut(|m| m.request_focus(self.body_id));
            }
        }
    }

    fn show(&mut self, ui: &mut egui::Ui, cx: &mut DocCx<'_>, theme: &Theme) {
        let ctx = ui.ctx().clone();
        let now = ui.input(|i| i.time);
        if let Some(t) = self.queued_toast.take() {
            self.toast = Some((t, now + 1.6));
        }
        if cx.just_activated && self.focus.is_none() {
            self.focus = Some(Focus::Body(None));
        }
        let m = theme.metrics;
        let area = ui.max_rect();
        let col_w = (area.width() - 2.0 * m.note_side_pad).clamp(120.0, m.note_column_max);
        let side = ((area.width() - col_w) / 2.0).floor().max(0.0);

        // Clicks in the side margins land in the body (registered first, so
        // the text fields win where they are).
        let bg = ui.interact(area, self.body_id.with("margin"), Sense::CLICK);
        if bg.clicked()
            && let (Some(p), Some((galley, origin))) = (bg.interact_pointer_pos(), &self.last_body)
        {
            let c = galley.cursor_from_pos(p - *origin);
            self.focus = Some(Focus::Body(Some(c.index.0)));
        }
        if bg.hovered() {
            ctx.set_cursor_icon(CursorIcon::Text);
        }

        self.pre_edit_keys(ui);
        self.apply_focus(&ctx);

        let style = MdStyle::from_theme(theme);
        let plain = self.plain;
        let vh = area.height();
        let title_h = if self.mode == Mode::Library {
            m.note_title_size * 1.4 + 12.0
        } else {
            0.0
        };
        let min_body = (vh - m.note_top_pad - title_h - 32.0).max(style.line_height);
        let bottom_pad = if self.body_height > min_body + 1.0 {
            (vh * 0.35).round()
        } else {
            24.0
        };

        let mut scroll = egui::ScrollArea::vertical()
            .id_salt(self.scroll_id)
            .auto_shrink([false, false])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden);
        if let Some(off) = self.pending_scroll.take() {
            scroll = scroll.vertical_scroll_offset(off);
        }

        let mut title_changed = false;
        let mut body_out = None;
        let deco_slot = ui.painter().add(Shape::Noop);
        let scroll_out = ui
            .scope_builder(UiBuilder::new().max_rect(area), |ui| {
                scroll.show(ui, |ui| {
                    ui.add_space(m.note_top_pad);
                    ui.horizontal(|ui| {
                        ui.add_space(side);
                        ui.vertical(|ui| {
                            ui.set_width(col_w);
                            if self.mode == Mode::Library {
                                let font =
                                    FontId::new(m.note_title_size, theme::family(theme::SEMIBOLD));
                                let resp = ui.add(
                                    TextEdit::singleline(&mut self.title)
                                        .id(self.title_id)
                                        .font(font.clone())
                                        .text_color(theme.text)
                                        .frame(Frame::NONE)
                                        .margin(Margin::ZERO)
                                        .desired_width(col_w)
                                        .char_limit(200)
                                        .return_key(None)
                                        .hint_text(
                                            RichText::new("Title")
                                                .font(font)
                                                .color(theme.text_dim.gamma_multiply(0.55)),
                                        ),
                                );
                                title_changed = resp.changed();
                                ui.add_space(12.0);
                            }
                            let md = &mut self.md;
                            let mut layouter =
                                |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap_width: f32| {
                                    let mut job = (*md.job(buf.as_str(), &style, plain)).clone();
                                    job.wrap.max_width = wrap_width;
                                    ui.fonts_mut(|f| f.layout_job(job))
                                };
                            let hint = if self.mode == Mode::Library {
                                "Start writing…"
                            } else {
                                ""
                            };
                            let out = TextEdit::multiline(&mut self.body)
                                .id(self.body_id)
                                .frame(Frame::NONE)
                                .margin(Margin::ZERO)
                                .desired_width(col_w)
                                .min_size(vec2(col_w, min_body))
                                .lock_focus(true)
                                .event_filter(EventFilter {
                                    tab: true,
                                    horizontal_arrows: true,
                                    vertical_arrows: true,
                                    escape: true,
                                })
                                .return_key(KeyboardShortcut::new(Modifiers::NONE, Key::Enter))
                                .hint_text(
                                    RichText::new(hint)
                                        .size(m.note_body_size)
                                        .color(theme.text_dim.gamma_multiply(0.55)),
                                )
                                .layouter(&mut layouter)
                                .show(ui);
                            ui.add_space(bottom_pad);
                            body_out = Some(out);
                        });
                    });
                })
            })
            .inner;

        if title_changed {
            theme::note_text(&self.title);
            self.touched();
        }
        let Some(out) = body_out else { return };
        if out.response.response.changed() {
            theme::note_text(&self.body);
            self.touched();
        }
        self.first_row_chars = out
            .galley
            .rows
            .first()
            .map_or(0, |r| r.row.char_count_excluding_newline().0);
        self.body_height = out.galley.size().y;
        self.last_body = Some((out.galley.clone(), out.galley_pos));

        // Decorations behind the text, and checkbox hit targets.
        let col = Rect::from_min_max(
            pos2(out.galley_pos.x, out.galley_pos.y),
            pos2(
                out.galley_pos.x + col_w,
                out.galley_pos.y + out.galley.size().y,
            ),
        );
        let clip = scroll_out.inner_rect;
        let (shapes, boxes) = if self.plain == Plain::No {
            decorations(&out.galley, out.galley_pos, &self.md, col, theme, clip)
        } else {
            (Vec::new(), Vec::new())
        };
        ui.painter()
            .with_clip_rect(clip)
            .set(deco_slot, Shape::Vec(shapes));
        let mut toggled = None;
        for (rect, box_at) in boxes {
            if !clip.intersects(rect) {
                continue;
            }
            let r = ui.interact(
                rect.expand(3.0).intersect(clip),
                self.body_id.with(("task", box_at)),
                Sense::CLICK,
            );
            if r.hovered() {
                ctx.set_cursor_icon(CursorIcon::PointingHand);
            }
            if r.clicked() {
                toggled = Some(box_at);
            }
        }
        if let Some(at) = toggled
            && let Some(e) = editing::toggle_task(&self.body, at + 1)
        {
            editing::apply(&mut self.body, &e);
            self.touched();
        }

        // Ctrl+click opens links.
        let resp = &out.response.response;
        let ctrl = ui.input(|i| i.modifiers.command);
        if ctrl && let Some(p) = resp.hover_pos() {
            let c = out.galley.cursor_from_pos(p - out.galley_pos);
            if let Some(url) = self.md.link_at(&self.body, c.index.0) {
                ctx.set_cursor_icon(CursorIcon::PointingHand);
                if resp.clicked() {
                    open_link(url, self.path.as_deref(), &ctx, cx);
                }
            }
        }

        // Our own overlay scrollbar (same look as PDF/CSV; the drawer knows it).
        let mut axis = ScrollAxis {
            offset: scroll_out.state.offset.y as f64,
            content: scroll_out.content_size.y as f64,
            viewport: scroll_out.inner_rect.height() as f64,
        };
        let track = Rect::from_min_max(pos2(area.right() - 12.0, area.top()), area.max);
        if scrollbar(ui, self.scroll_id.with("bar"), track, &mut axis, true) {
            self.pending_scroll = Some(axis.offset as f32);
            ctx.request_repaint();
        }

        self.banners(ui, area, theme, cx);
        paint_toast(ui, area, &mut self.toast, now, theme);

        // Wake up for the next autosave.
        if let Some(due) = self.autosave_due() {
            ctx.request_repaint_after(due.saturating_duration_since(Instant::now()));
        }
    }

    fn banners(&mut self, ui: &mut egui::Ui, area: Rect, theme: &Theme, cx: &mut DocCx<'_>) {
        let (text, buttons): (String, &[&str]) = if self.conflict {
            (
                "This note was changed outside whir.".into(),
                &["Load theirs", "Keep mine"],
            )
        } else if let Some(e) = &self.error {
            let text = format!("Couldn't save: {e}");
            if self.mode == Mode::Library {
                (text, &["Try again", "Save as…"])
            } else {
                (text, &["Save as…"])
            }
        } else {
            return;
        };
        let width = (area.width() - 40.0).min(560.0);
        let rect = Rect::from_min_size(
            pos2(area.center().x - width / 2.0, area.top() + 10.0),
            vec2(width, 36.0),
        );
        ui.interact(rect, self.body_id.with("banner"), Sense::CLICK);
        let mut clicked = None;
        ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
            Frame::NONE
                .fill(theme.overlay_bg)
                .stroke(Stroke::new(1.0, theme.separator))
                .corner_radius(8.0)
                .inner_margin(Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.add(egui::Label::new(RichText::new(text).color(theme.text)).truncate());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            for b in buttons.iter().rev() {
                                if ui.button(*b).clicked() {
                                    clicked = Some(*b);
                                }
                            }
                        });
                    });
                });
        });
        match clicked {
            Some("Load theirs") => {
                self.conflict = false;
                self.reload();
            }
            Some("Keep mine") => {
                // Take the disk version as the base and overwrite it.
                self.conflict = false;
                self.identity = self
                    .path
                    .as_deref()
                    .and_then(|p| FileIdentity::of_path(p).ok());
                self.submit(true);
            }
            Some("Try again") => {
                self.retry_at = None;
                self.error = None;
                self.submit(true);
            }
            Some("Save as…") => cx.requests.push(DocRequest::SaveAs),
            _ => {}
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Special {
    Enter,
    CtrlEnter,
    Tab,
    ShiftTab,
    Bold,
    Italic,
    Link,
    /// TextEdit's Emacs-style Ctrl+U / Ctrl+H: not what Windows users expect.
    Swallow,
}

fn special(e: &Event) -> Option<Special> {
    let Event::Key {
        key,
        pressed: true,
        modifiers,
        ..
    } = e
    else {
        return None;
    };
    let exact = |m: Modifiers| modifiers.matches_exact(m);
    Some(match key {
        Key::Enter if exact(Modifiers::NONE) => Special::Enter,
        Key::Enter if exact(Modifiers::CTRL) => Special::CtrlEnter,
        Key::Tab if exact(Modifiers::NONE) => Special::Tab,
        Key::Tab if exact(Modifiers::SHIFT) => Special::ShiftTab,
        Key::B if exact(Modifiers::CTRL) => Special::Bold,
        Key::I if exact(Modifiers::CTRL) => Special::Italic,
        Key::K if exact(Modifiers::CTRL) => Special::Link,
        Key::U | Key::H if exact(Modifiers::CTRL) => Special::Swallow,
        _ => return None,
    })
}

fn keys_exact(i: &mut egui::InputState, mods: Modifiers, key: Key) -> bool {
    let pos = i.events.iter().position(|e| {
        matches!(e, Event::Key { key: k, pressed: true, modifiers, .. }
            if *k == key && modifiers.matches_exact(mods))
    });
    match pos {
        Some(p) => {
            i.events.remove(p);
            true
        }
        None => false,
    }
}

fn open_link(url: &str, note: Option<&Path>, ctx: &egui::Context, cx: &mut DocCx<'_>) {
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:")
    {
        ctx.open_url(egui::OpenUrl::new_tab(url));
        return;
    }
    // A local file, relative to the note.
    let target = Path::new(url.trim_start_matches("file:///"));
    let target = if target.is_absolute() {
        target.to_path_buf()
    } else if let Some(dir) = note.and_then(Path::parent) {
        dir.join(target)
    } else {
        return;
    };
    if target.exists() {
        cx.requests.push(DocRequest::Open(vec![target]));
    }
}

/// Code block backgrounds, quote bars, rules and checkboxes, painted behind
/// the text. Returns the shapes and each checkbox's rect with its `[` index.
fn decorations(
    galley: &Galley,
    origin: Pos2,
    md: &MdCache,
    col: Rect,
    theme: &Theme,
    clip: Rect,
) -> (Vec<Shape>, Vec<(Rect, usize)>) {
    let mut shapes = Vec::new();
    let mut boxes = Vec::new();
    if galley.rows.is_empty() {
        return (shapes, boxes);
    }
    // Char index where each row starts.
    let mut starts = Vec::with_capacity(galley.rows.len());
    let mut at = 0usize;
    for r in &galley.rows {
        starts.push(at);
        at += r.char_count_including_newline().0;
    }
    let row_of = |c: usize| starts.partition_point(|&s| s <= c).saturating_sub(1);
    let span = |l: &crate::markdown::LineInfo| -> (f32, f32) {
        let r0 = row_of(l.start);
        let r1 = row_of(l.end.max(l.start + 1) - 1).max(r0);
        (
            origin.y + galley.rows[r0].min_y(),
            origin.y + galley.rows[r1.min(galley.rows.len() - 1)].max_y(),
        )
    };
    let visible = |y0: f32, y1: f32| y1 >= clip.top() - 50.0 && y0 <= clip.bottom() + 50.0;

    // Code blocks: one rounded background per run of fence/code lines.
    let mut block: Option<(f32, f32)> = None;
    let flush = |block: &mut Option<(f32, f32)>, shapes: &mut Vec<Shape>| {
        if let Some((y0, y1)) = block.take()
            && visible(y0, y1)
        {
            shapes.push(Shape::rect_filled(
                Rect::from_min_max(
                    pos2(col.left() - 10.0, y0 - 2.0),
                    pos2(col.right() + 10.0, y1 + 2.0),
                ),
                6.0,
                theme.md_code_block_bg,
            ));
        }
    };
    for l in &md.lines {
        let (y0, y1) = span(l);
        match l.kind {
            LineKind::Code | LineKind::Fence => {
                block = Some(match block {
                    Some((a, _)) => (a, y1),
                    None => (y0, y1),
                });
                continue;
            }
            _ => flush(&mut block, &mut shapes),
        }
        if !visible(y0, y1) {
            continue;
        }
        match l.kind {
            LineKind::Quote => shapes.push(Shape::rect_filled(
                Rect::from_min_max(
                    pos2(col.left() - 14.0, y0 + 2.0),
                    pos2(col.left() - 11.0, y1 - 2.0),
                ),
                1.5,
                theme.md_quote_bar,
            )),
            LineKind::Rule => {
                let y = ((y0 + y1) / 2.0).round() + 0.5;
                shapes.push(Shape::line_segment(
                    [pos2(col.left(), y), pos2(col.right(), y)],
                    Stroke::new(1.0, theme.md_rule),
                ));
            }
            LineKind::Task { checked, box_at } => {
                let a = galley.pos_from_cursor(CCursor::new(box_at));
                let b = galley.pos_from_cursor(CCursor::new(box_at + 3));
                let cx = origin.x + (a.left() + b.left()) / 2.0;
                let cy = origin.y + a.center().y;
                let rect = Rect::from_center_size(pos2(cx, cy), vec2(13.0, 13.0));
                if checked {
                    shapes.push(Shape::rect_filled(rect, 3.0, theme.accent));
                    let s = Stroke::new(1.6, Color32::WHITE);
                    shapes.push(Shape::line(
                        vec![
                            pos2(rect.left() + 3.0, rect.center().y),
                            pos2(rect.left() + 5.5, rect.bottom() - 3.5),
                            pos2(rect.right() - 3.0, rect.top() + 3.5),
                        ],
                        s,
                    ));
                } else {
                    shapes.push(Shape::rect_stroke(
                        rect,
                        3.0,
                        Stroke::new(1.3, theme.md_list_marker),
                        egui::StrokeKind::Inside,
                    ));
                }
                boxes.push((rect, box_at));
            }
            _ => {}
        }
    }
    flush(&mut block, &mut shapes);
    (shapes, boxes)
}

fn paint_toast(
    ui: &egui::Ui,
    area: Rect,
    toast: &mut Option<(String, f64)>,
    now: f64,
    theme: &Theme,
) {
    let Some((text, until)) = toast else { return };
    if now >= *until {
        *toast = None;
        return;
    }
    let alpha = ((*until - now) / 0.3).clamp(0.0, 1.0) as f32;
    let painter = ui.painter();
    let galley = painter.layout_no_wrap(
        text.clone(),
        FontId::proportional(12.0),
        theme.text_dim.gamma_multiply(alpha),
    );
    let pad = vec2(10.0, 5.0);
    let size = galley.size() + pad * 2.0;
    let rect = Rect::from_min_size(
        pos2(area.right() - size.x - 24.0, area.bottom() - size.y - 16.0),
        size,
    );
    painter.rect_filled(rect, size.y / 2.0, theme.overlay_bg.gamma_multiply(alpha));
    painter.galley(rect.min + pad, galley, theme.text_dim);
    ui.ctx().request_repaint_after(Duration::from_millis(40));
}

impl Document for NoteDoc {
    #[allow(clippy::misnamed_getters)] // the tab label, not the note's title field
    fn title(&self) -> &str {
        &self.tab_title
    }

    fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    fn kind(&self) -> DocKind {
        DocKind::Note
    }

    fn is_loading(&self) -> bool {
        matches!(self.phase, Phase::Loading(_))
    }

    fn poll(&mut self) {
        self.drain();
        if let Some(due) = self.autosave_due() {
            if Instant::now() >= due {
                self.retry_at = None;
                self.submit(false);
            } else if let Some(ctx) = &self.ctx {
                ctx.request_repaint_after(due - Instant::now());
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut DocCx<'_>) {
        if self.ctx.is_none() {
            self.ctx = Some(ui.ctx().clone());
        }
        self.drain();
        let theme = Theme::current();
        let area = ui.max_rect();
        ui.painter().rect_filled(area, 0.0, theme.content_bg);
        match &self.phase {
            Phase::Loading(_) => {
                ui.put(
                    Rect::from_center_size(area.center(), vec2(24.0, 24.0)),
                    egui::Spinner::new().size(20.0),
                );
            }
            Phase::Failed(e) => {
                let e = e.clone();
                whir_core::message_doc::show_message(
                    ui,
                    "Can't open this file",
                    &e,
                    self.path.as_deref(),
                );
            }
            Phase::Ready => self.show(ui, cx, &theme),
        }
    }

    fn on_visibility(&mut self, visible: bool) {
        if !visible {
            self.flush();
        }
    }

    fn persistence(&self) -> Persistence {
        match self.mode {
            Mode::Library => Persistence::Auto,
            Mode::External => Persistence::Explicit,
        }
    }

    fn is_dirty(&self) -> bool {
        matches!(self.phase, Phase::Ready) && self.content_hash != self.saved_hash
    }

    fn is_saving(&self) -> bool {
        self.inflight.is_some()
    }

    fn save_error(&self) -> Option<&str> {
        if self.conflict {
            Some("This note was changed outside whir.")
        } else {
            self.error.as_deref()
        }
    }

    fn is_pristine(&self) -> bool {
        self.mode == Mode::Library
            && self.path.is_none()
            && matches!(self.phase, Phase::Ready)
            && self.title.is_empty()
            && self.body.is_empty()
    }

    fn save(&mut self) -> SaveResult {
        if !matches!(self.phase, Phase::Ready) {
            return SaveResult::Saved;
        }
        if self.mode == Mode::Library && self.is_blank() && self.path.is_none() {
            self.toast("Empty notes aren't saved");
            return SaveResult::Saved;
        }
        if self.mode == Mode::Library && self.path.is_none() && self.library.dir().is_none() {
            return self
                .save_as_spec()
                .map_or(SaveResult::Unsupported, SaveResult::NeedsPath);
        }
        if self.conflict {
            return SaveResult::Failed("This note was changed outside whir.".into());
        }
        if !self.is_dirty() && self.inflight.is_none() {
            if self.error.is_none() {
                self.toast("Saved");
            }
            return SaveResult::Saved;
        }
        self.error = None;
        self.retry_at = None;
        if self.submit(true) {
            SaveResult::Started
        } else {
            SaveResult::Failed(
                self.error
                    .clone()
                    .unwrap_or_else(|| "Couldn't save.".into()),
            )
        }
    }

    fn save_as_spec(&self) -> Option<SaveAsSpec> {
        let name = match self.mode {
            Mode::Library => format!(
                "{}.md",
                library::sanitize_stem(&self.tab_title).unwrap_or_else(|| "Note".into())
            ),
            Mode::External => self
                .path
                .as_deref()
                .and_then(Path::file_name)
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Untitled.md".into()),
        };
        Some(SaveAsSpec {
            title: if self.mode == Mode::Library {
                "Export note".into()
            } else {
                "Save as".into()
            },
            suggested_name: name,
            filters: vec![
                ("Markdown".into(), "*.md;*.markdown".into()),
                ("Text".into(), "*.txt".into()),
                ("All files".into(), "*.*".into()),
            ],
            default_ext: "md".into(),
            folder: match self.mode {
                Mode::Library => None,
                Mode::External => self
                    .path
                    .as_deref()
                    .and_then(Path::parent)
                    .map(Path::to_path_buf),
            },
        })
    }

    fn save_as(&mut self, path: PathBuf) -> SaveResult {
        let text = note_file::compose(&self.title, &self.body, &self.header, &self.format);
        let Some(bytes) = self.encode(&text) else {
            return SaveResult::Failed("Couldn't encode the note.".into());
        };
        if self.mode == Mode::Library && self.path.is_some() {
            // A library note stays where it is; this writes a copy.
            let (reply, rx) = channel();
            saver::submit(saver::Job {
                target: Target::Export(path),
                bytes,
                expect: None,
                cancel: Arc::new(AtomicBool::new(false)),
                hash: self.content_hash,
                reply,
                waker: self.waker.clone(),
                library: self.library.clone(),
            });
            if self.inflight.is_none() {
                self.inflight = Some(Inflight {
                    rx,
                    header: self.header.clone(),
                    user: true,
                });
            }
            return SaveResult::Started;
        }
        // Rebind: from now on this document is that file.
        self.mode = if self.library.contains(&path) && is_markdown(&path) {
            Mode::Library
        } else {
            Mode::External
        };
        self.path = Some(path);
        self.identity = None;
        self.conflict = false;
        self.error = None;
        self.saved_hash = 0; // force the write
        self.update_tab_title();
        if self.submit(true) {
            SaveResult::Started
        } else {
            SaveResult::Failed(
                self.error
                    .clone()
                    .unwrap_or_else(|| "Couldn't save.".into()),
            )
        }
    }

    fn flush(&mut self) {
        if self.mode == Mode::Library && self.is_dirty() && !self.conflict {
            self.submit(false);
        }
    }

    fn flush_blocking(&mut self, deadline: Instant) -> Result<(), String> {
        if self.mode != Mode::Library {
            return if self.is_dirty() {
                Err("unsaved changes".into())
            } else {
                Ok(())
            };
        }
        // Let a write in flight finish first.
        if let Some(inf) = self.inflight.take() {
            let wait = deadline.saturating_duration_since(Instant::now());
            match inf.rx.recv_timeout(wait) {
                Ok(r) => self.handle_reply(r, inf.header, inf.user),
                Err(_) => {
                    self.inflight = Some(inf);
                    return Err("still saving".into());
                }
            }
        }
        if self.is_dirty()
            && !self.conflict
            && let Some((job, _rx, header)) = self.make_job(false)
        {
            let r = saver::run_now(job);
            self.handle_reply(r, header, false);
        }
        match (&self.error, self.is_dirty()) {
            (Some(e), _) => Err(e.clone()),
            (None, true) => Err("unsaved changes".into()),
            (None, false) => Ok(()),
        }
    }

    fn discard(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    fn app_activated(&mut self, active: bool) {
        if !active {
            self.flush();
            return;
        }
        // Edited by another program while we were away?
        if let (Some(path), Some(identity), true) = (
            self.path.as_deref(),
            self.identity,
            matches!(self.phase, Phase::Ready) && self.inflight.is_none() && self.reload.is_none(),
        ) && let Ok(now) = FileIdentity::of_path(path)
            && now != identity
        {
            if self.is_dirty() {
                self.conflict = true;
            } else {
                self.reload();
            }
        }
    }
}

impl Drop for NoteDoc {
    fn drop(&mut self) {
        // Free the TextEdit undo histories kept in egui's memory.
        if let Some(ctx) = &self.ctx {
            ctx.data_mut(|d| {
                d.remove::<egui::text_edit::TextEditState>(self.title_id);
                d.remove::<egui::text_edit::TextEditState>(self.body_id);
            });
        }
    }
}
