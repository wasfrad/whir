//! The image tab.
//!
//! Memory model: every image is shown from a *fit level* no larger than the
//! screen (JPEGs decode straight to that size via DCT scaling). Full
//! resolution is decoded only when the user zooms past what the fit level can
//! show, and dropped again when the tab is hidden.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use egui::epaint::text::LayoutJob;
use egui::epaint::{TextShape, Vertex};
use egui::{
    Align, Color32, ColorImage, CursorIcon, Event, FontDefinitions, FontId, Key, Layout, Mesh,
    Modifiers, PointerButton, Pos2, Rect, Sense, Shape, Stroke, TextBuffer, TextureOptions,
    UiBuilder, Vec2, pos2, vec2,
};
use whir_core::marks::{self, Ink, Kind, Mark, MarkSet, Pen, Text};
use whir_core::theme::Theme;
use whir_core::toolbar::{self, Glyph};
use whir_core::{
    Captures, ClipImage, DocCx, DocKind, DocRequest, Document, OpenCx, Persistence, SaveAsSpec,
    SaveResult, TextureRegistry, Waker,
};

use crate::decode::Decoded;
use crate::edit::{self, Editor};
use crate::encode::{PixelLayout, encode_png};
use crate::pool::{self, JobHandle, Priority};
use crate::prefs::{self, ZoomPref};
use crate::tiles::{self, Level};
use crate::view::{self, Mode, View};
use crate::{bake, decode, folder};

mod tools;

use tools::{Handle, MIN_DRAW, drawn, moved};

enum Msg {
    Loaded {
        generation: u64,
        full: bool,
        result: Result<Level, String>,
    },
    Prefetched {
        path: PathBuf,
        level: Level,
    },
    Folder(Vec<PathBuf>),
    /// The image as a clipboard DIB.
    Copied(Result<Vec<u8>, String>),
    /// "Save a copy" wrote this file.
    Exported(Result<PathBuf, String>),
    /// What the screenshots folder knows about an image about to be shown.
    Marks {
        path: PathBuf,
        /// It is in that folder (so its marks are kept).
        stored: bool,
        /// It was captured moments ago.
        fresh: bool,
        saved: Option<MarkSet>,
    },
    /// A marks file was written (or couldn't be).
    MarksSaved {
        path: PathBuf,
        result: Result<(), String>,
    },
}

/// A capture this recent is opening straight from the dock.
const FRESH: Duration = Duration::from_secs(15);

/// Look `path` up in the screenshots folder. Reads the disk (and resolves
/// the Documents folder the first time): for decode workers only.
fn stored_marks(captures: &Captures, path: &Path) -> Msg {
    let stored = captures.contains(path);
    let fresh = stored
        && std::fs::metadata(path)
            .and_then(|m| m.created())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age < FRESH);
    Msg::Marks {
        path: path.to_owned(),
        stored,
        fresh,
        saved: captures.load_marks(path),
    }
}

struct Shown {
    path: PathBuf,
    fit: Arc<Level>,
    full: Option<Arc<Level>>,
}

impl Shown {
    fn full_size(&self) -> egui::Vec2 {
        let [w, h] = self.fit.full_size;
        vec2(w as f32, h as f32)
    }

    fn best(&self) -> &Level {
        self.full.as_deref().unwrap_or(&self.fit)
    }
}

/// The tools that mark an image. Either one selects, moves, resizes and
/// removes any mark; they differ in what a drag or a click on bare image does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tool {
    /// A drag draws a highlight.
    Highlight,
    /// A click starts typing there.
    Text,
}

/// What a primary-button drag is doing while a tool is armed (positions in
/// image pixels).
#[derive(Clone, Copy)]
enum Drag {
    /// Drawing a new mark between two corners.
    Draw { from: Pos2, to: Pos2 },
    /// Moving mark `index` by `to - from`.
    Move { index: usize, from: Pos2, to: Pos2 },
    /// Resizing mark `index` by one of its handles, dragged `to - from`.
    Resize {
        index: usize,
        handle: Handle,
        from: Pos2,
        to: Pos2,
    },
}

/// Text being typed on the image: a new mark, or one being retyped.
struct Typing {
    /// The mark being retyped; `None` for a new one.
    index: Option<usize>,
    /// The image pixel at the text's top left as it reads on screen.
    anchor: Pos2,
    body: String,
    size: u32,
    pen: Pen,
    /// The editor on screen last frame: a press anywhere else finishes it.
    screen: Rect,
    /// It has had the keyboard: losing it finishes the text.
    focused: bool,
    /// The editor's, new each time: it starts with the caret at the end,
    /// not wherever the last one left it.
    id: egui::Id,
}

/// New text is about this many points tall on screen, whatever the zoom.
const TEXT_POINTS: f32 = 20.0;

/// Counts the times typing started, for [`Typing::id`].
static TYPED: AtomicU64 = AtomicU64::new(0);

/// Handles are this many points a side, and can be grabbed this far from
/// their middle.
const HANDLE: f32 = 7.0;
const HANDLE_REACH: f32 = 6.0;

/// Where the image is on screen this frame, and how its pixels map there.
#[derive(Clone, Copy)]
struct Placement {
    /// The image on screen, after its quarter turns.
    rect: Rect,
    /// Its unturned size in pixels.
    size: Vec2,
    turns: u8,
}

impl Placement {
    /// A screen position → a pixel of the unturned image.
    fn pixel(&self, p: Pos2) -> Pos2 {
        view::screen_to_image(p, self.rect, self.size, self.turns)
    }

    fn screen(&self, p: Pos2) -> Pos2 {
        view::image_to_screen(p, self.rect, self.size, self.turns)
    }

    /// Points per image pixel.
    fn zoom(&self) -> f32 {
        self.rect.width() / view::turned(self.size, self.turns).x
    }

    /// A mark's box on screen.
    fn mark_rect(&self, m: &Mark) -> Rect {
        let corner = |x: u32, y: u32| self.screen(pos2(x as f32, y as f32));
        Rect::from_two_pos(corner(m.x, m.y), corner(m.x + m.w, m.y + m.h))
    }

    /// The handles of `m`, where they are on screen. A box too small on
    /// screen for eight is given its corners only.
    fn handles(&self, m: &Mark) -> impl Iterator<Item = (Handle, Pos2)> {
        let r = self.mark_rect(m);
        let edges = r.width().min(r.height()) >= 3.0 * HANDLE;
        tools::handles(m, edges)
            .iter()
            .map(move |&h| (h, self.screen(h.on(m))))
    }
}

/// The resize cursor for `handle`, the way it points on screen.
fn resize_cursor(handle: Handle, turns: u8) -> CursorIcon {
    let d = marks::turn_vec(handle.pull(), turns);
    if d.x == 0.0 {
        CursorIcon::ResizeVertical
    } else if d.y == 0.0 {
        CursorIcon::ResizeHorizontal
    } else if d.x == d.y {
        CursorIcon::ResizeNwSe
    } else {
        CursorIcon::ResizeNeSw
    }
}

/// A text box's upright size as it lies on the image.
fn on_image([w, h]: [u32; 2], turn: u8) -> [u32; 2] {
    if turn % 2 == 1 { [h, w] } else { [w, h] }
}

/// Draw `m` in its box `r` on screen.
fn paint_mark(painter: &egui::Painter, r: Rect, m: &Mark, at: &Placement) {
    match &m.kind {
        Kind::Highlight(ink) => marks::paint(painter, r, *ink),
        Kind::Text(t) => {
            let turns = (at.turns + 4 - t.turn % 4) % 4;
            marks::paint_text(painter, r, t, turns, at.zoom());
        }
    }
}

/// `path` decoded in full as it is on screen: its marks baked in (text set
/// in `fonts`) and turned. Marks need paper to tint, so with marks (or
/// `opaque`) transparency becomes white.
fn flattened(
    path: &Path,
    marks: &[Mark],
    turns: u8,
    opaque: bool,
    fonts: Option<&FontDefinitions>,
) -> Result<Decoded, String> {
    let mut d = decode::decode(path, None).map_err(|e| e.to_string())?;
    if opaque || !marks.is_empty() {
        bake::flatten_on_white(&mut d);
        bake::bake(&mut d, marks, fonts);
    }
    Ok(bake::turn(d, turns))
}

pub struct ImageDoc {
    path: PathBuf,
    title: String,
    textures: Arc<dyn TextureRegistry>,
    waker: Waker,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    generation: u64,
    shown: Option<Shown>,
    loading: bool,
    error: Option<String>,
    fit_job: Option<JobHandle>,
    full_job: Option<JobHandle>,
    /// Full resolution couldn't be decoded/uploaded for the current image.
    full_failed: bool,
    visible: bool,
    view: View,
    /// The current image is still to be put at the zoom it opens at
    /// ([`prefs::initial`]), once it's on screen.
    opening: bool,
    folder: Option<Vec<PathBuf>>,
    folder_requested: bool,
    /// The user has navigated with ←/→ at least once.
    browsing: bool,
    /// Pre-decoded neighbours (fit level), for instant ←/→.
    cache: Vec<(PathBuf, Arc<Level>)>,
    prefetching: Vec<PathBuf>,
    /// Transient "1920 × 1080 · 50%" pill: text and when it hides.
    toast: Option<(String, f64)>,
    checker: Option<egui::TextureHandle>,
    /// Quarter turns clockwise per image (non-zero only), for as long as the
    /// tab is open. Only the screen turns: files are never changed.
    rotations: Vec<(PathBuf, u8)>,
    /// The tool in hand: the primary button marks instead of panning.
    tool: Option<Tool>,
    /// Marks per image, kept the same way.
    marks: Vec<(PathBuf, Editor)>,
    drag: Option<Drag>,
    typing: Option<Typing>,
    /// The press under way finished the typing: it does nothing else.
    swallow: bool,
    /// The size of the text typed or resized last, for the next.
    text_size: Option<u32>,
    /// For laying text out between frames (finishing typing, exporting).
    ctx: Option<egui::Context>,
    /// A copy / an export is being made on a worker.
    copying: bool,
    exporting: bool,
    /// A finished copy, for the shell to put on the clipboard.
    clip: Option<Vec<u8>>,
    /// Shown as the next toast (set where the clock isn't at hand).
    notice: Option<String>,
    captures: Arc<Captures>,
    /// Marks files being written.
    marks_in_flight: u32,
    /// Why the last one couldn't be.
    marks_error: Option<String>,
}

/// The zoom `view` is at, as a choice to open the next image at.
fn zoom_pref(view: &View, ppp: f32) -> ZoomPref {
    match view.mode {
        Mode::Fit => ZoomPref::Fit,
        Mode::Manual { zoom, .. } => ZoomPref::Percent(zoom * ppp * 100.0),
    }
}

/// Longest side of the fit level: the screen's longer side (physical px).
fn fit_cap() -> u32 {
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
    let (w, h) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
    (w.max(h).max(1280)) as u32
}

impl ImageDoc {
    pub fn open(path: PathBuf, cx: &OpenCx) -> Self {
        let (tx, rx) = channel();
        let mut doc = Self {
            title: file_title(&path),
            path,
            textures: cx.textures.clone(),
            waker: cx.waker.clone(),
            tx,
            rx,
            generation: 0,
            shown: None,
            loading: true,
            error: None,
            fit_job: None,
            full_job: None,
            full_failed: false,
            visible: false,
            view: View::default(),
            opening: true,
            folder: None,
            folder_requested: false,
            browsing: false,
            cache: Vec::new(),
            prefetching: Vec::new(),
            toast: None,
            checker: None,
            rotations: Vec::new(),
            tool: None,
            marks: Vec::new(),
            drag: None,
            typing: None,
            swallow: false,
            text_size: None,
            ctx: None,
            copying: false,
            exporting: false,
            clip: None,
            notice: None,
            captures: cx.captures.clone(),
            marks_in_flight: 0,
            marks_error: None,
        };
        // Newly opened documents are almost always the active tab: decode now.
        doc.start_fit_decode(Priority::Visible);
        doc
    }

    fn decode_job(&self, path: PathBuf, full: bool, priority: Priority) -> JobHandle {
        let (tx, waker, textures, generation) = (
            self.tx.clone(),
            self.waker.clone(),
            self.textures.clone(),
            self.generation,
        );
        let captures = self.captures.clone();
        pool::submit(priority, move || {
            edit::load_choices();
            if !full {
                // Ahead of the image, so its marks appear with it.
                let _ = tx.send(stored_marks(&captures, &path));
            }
            let max = if full { None } else { Some(fit_cap()) };
            let result = decode::decode(&path, max)
                .map_err(|e| e.to_string())
                .and_then(|d| tiles::upload(&textures, &d));
            let _ = tx.send(Msg::Loaded {
                generation,
                full,
                result,
            });
            waker.wake();
        })
    }

    fn start_fit_decode(&mut self, priority: Priority) {
        self.loading = true;
        self.fit_job = Some(self.decode_job(self.path.clone(), false, priority));
    }

    fn request_full(&mut self) {
        if self.full_job.is_some() || self.full_failed {
            return;
        }
        let Some(shown) = &self.shown else { return };
        // While ←/→ is still decoding the next image, `shown` is the previous one.
        if !folder::same_path(&shown.path, &self.path) {
            return;
        }
        if shown.fit.is_full_resolution() || shown.full.is_some() {
            return;
        }
        self.full_job = Some(self.decode_job(shown.path.clone(), true, Priority::Visible));
    }

    fn drain(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            self.handle(msg);
        }
        self.check_marks_fit();
    }

    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Loaded {
                generation,
                full,
                result,
            } => {
                if generation != self.generation {
                    return;
                }
                match result {
                    Ok(level) if full => {
                        self.full_job = None;
                        if let Some(s) = &mut self.shown
                            && folder::same_path(&s.path, &self.path)
                        {
                            s.full = Some(Arc::new(level));
                        }
                    }
                    Ok(level) => {
                        self.fit_job = None;
                        self.loading = false;
                        self.error = None;
                        let level = Arc::new(level);
                        let full = level.is_full_resolution().then(|| level.clone());
                        self.shown = Some(Shown {
                            path: self.path.clone(),
                            fit: level,
                            full,
                        });
                        self.prefetch_neighbours();
                    }
                    Err(e) => {
                        if full {
                            // Keep showing the fit level; don't retry every frame.
                            self.full_job = None;
                            self.full_failed = true;
                        } else {
                            self.fit_job = None;
                            self.loading = false;
                            self.shown = None;
                            self.error = Some(e);
                        }
                    }
                }
            }
            Msg::Prefetched { path, level } => {
                self.prefetching.retain(|p| !folder::same_path(p, &path));
                if self.visible && self.is_neighbour(&path) {
                    self.cache.push((path, Arc::new(level)));
                    self.trim_cache();
                }
            }
            Msg::Folder(list) => {
                self.folder = Some(list);
                self.prefetch_neighbours();
            }
            Msg::Copied(result) => {
                self.copying = false;
                match result {
                    Ok(dib) => {
                        self.clip = Some(dib);
                        self.notice = Some("Copied".to_owned());
                    }
                    Err(e) => self.notice = Some(format!("Couldn't copy the image. {e}")),
                }
            }
            Msg::Exported(result) => {
                self.exporting = false;
                self.notice = Some(match result {
                    Ok(path) => {
                        // Saved into the screenshots folder: the Library lists it.
                        self.captures.refresh(vec![path.clone()]);
                        format!("Saved {}", file_title(&path))
                    }
                    Err(e) => e,
                });
            }
            Msg::Marks {
                path,
                stored,
                fresh,
                saved,
            } => {
                // What this tab already has for the image (drawn before
                // it was reloaded) is newer than the file.
                let known = self.marks.iter().any(|(p, _)| folder::same_path(p, &path));
                if stored && !known {
                    self.marks.push((path.clone(), Editor::stored(saved)));
                }
                // Straight from the dock: ready to highlight.
                if fresh && folder::same_path(&path, &self.path) {
                    self.tool = Some(Tool::Highlight);
                }
            }
            Msg::MarksSaved { path, result } => {
                self.marks_in_flight = self.marks_in_flight.saturating_sub(1);
                match result {
                    Ok(()) => self.marks_error = None,
                    Err(e) => {
                        self.marks_error = Some(e);
                        let editor = self
                            .marks
                            .iter_mut()
                            .find(|(p, _)| folder::same_path(p, &path));
                        if let Some((_, e)) = editor {
                            e.failed = true;
                        }
                    }
                }
            }
        }
    }

    // --- folder navigation ------------------------------------------------------

    fn ensure_folder(&mut self) {
        if self.folder_requested {
            return;
        }
        self.folder_requested = true;
        let Some(dir) = self.path.parent().map(Path::to_owned) else {
            return;
        };
        let (tx, waker) = (self.tx.clone(), self.waker.clone());
        std::thread::Builder::new()
            .name("image-folder".into())
            .spawn(move || {
                let _ = tx.send(Msg::Folder(folder::list_images(&dir)));
                waker.wake();
            })
            .ok();
    }

    fn index_in_folder(&self) -> Option<usize> {
        self.folder
            .as_ref()?
            .iter()
            .position(|p| folder::same_path(p, &self.path))
    }

    fn neighbours(&self) -> Vec<PathBuf> {
        let (Some(list), Some(i)) = (&self.folder, self.index_in_folder()) else {
            return Vec::new();
        };
        [i.checked_add(1), i.checked_sub(1)]
            .into_iter()
            .flatten()
            .filter_map(|j| list.get(j).cloned())
            .collect()
    }

    fn is_neighbour(&self, path: &Path) -> bool {
        self.neighbours().iter().any(|p| folder::same_path(p, path))
    }

    fn trim_cache(&mut self) {
        let keep = self.neighbours();
        self.cache
            .retain(|(p, _)| keep.iter().any(|k| folder::same_path(k, p)));
    }

    fn prefetch_neighbours(&mut self) {
        // Only once the user is browsing (has pressed ←/→): opening a single
        // image shouldn't pay memory for its neighbours.
        if !self.browsing || !self.visible || self.shown.is_none() {
            return;
        }
        self.trim_cache();
        for path in self.neighbours() {
            let cached = self.cache.iter().any(|(p, _)| folder::same_path(p, &path));
            let pending = self.prefetching.iter().any(|p| folder::same_path(p, &path));
            if cached || pending {
                continue;
            }
            self.prefetching.push(path.clone());
            let (tx, waker, textures) =
                (self.tx.clone(), self.waker.clone(), self.textures.clone());
            let captures = self.captures.clone();
            pool::submit(Priority::Prefetch, move || {
                let _ = tx.send(stored_marks(&captures, &path));
                let level = decode::decode(&path, Some(fit_cap()))
                    .ok()
                    .and_then(|d| tiles::upload(&textures, &d).ok());
                if let Some(level) = level {
                    let _ = tx.send(Msg::Prefetched { path, level });
                    waker.wake();
                }
            });
        }
    }

    fn navigate(&mut self, delta: isize, time: f64) {
        self.browsing = true;
        let (Some(list), Some(i)) = (&self.folder, self.index_in_folder()) else {
            return;
        };
        let target = match delta {
            isize::MIN => 0,
            isize::MAX => list.len() - 1,
            d => (i as isize + d).clamp(0, list.len() as isize - 1) as usize,
        };
        if target == i {
            return;
        }
        let path = list[target].clone();
        let position = format!("{} / {}", target + 1, list.len());
        self.go_to(path);
        self.toast = Some((position, time + 1.2));
    }

    fn go_to(&mut self, path: PathBuf) {
        self.commit_typing();
        if let Some(j) = self.fit_job.take() {
            j.cancel();
        }
        if let Some(j) = self.full_job.take() {
            j.cancel();
        }
        self.generation += 1;
        self.full_failed = false;
        self.drag = None;
        for (_, e) in &mut self.marks {
            e.selected = None;
        }
        self.title = file_title(&path);
        self.path = path;
        self.view = View::default();
        self.opening = true;
        self.error = None;
        let cached = self
            .cache
            .iter()
            .position(|(p, _)| folder::same_path(p, &self.path))
            .map(|i| self.cache.swap_remove(i).1);
        match cached {
            Some(level) => {
                let full = level.is_full_resolution().then(|| level.clone());
                // The outgoing image becomes a neighbour: keep it for instant back-navigation.
                if let Some(old) = self.shown.take() {
                    self.cache.push((old.path, old.fit));
                }
                self.shown = Some(Shown {
                    path: self.path.clone(),
                    fit: level,
                    full,
                });
                self.loading = false;
                self.prefetch_neighbours();
            }
            // Keep showing the previous image until the new one is decoded.
            None => self.start_fit_decode(Priority::Visible),
        }
    }

    // --- rotation ----------------------------------------------------------------

    /// Quarter turns clockwise of the image on screen.
    fn turns(&self) -> u8 {
        let Some(shown) = &self.shown else { return 0 };
        self.rotations
            .iter()
            .find(|(p, _)| folder::same_path(p, &shown.path))
            .map_or(0, |r| r.1)
    }

    /// `image` is the on-screen size before the turn.
    fn rotate(&mut self, clockwise: bool, image: egui::Vec2, vp: egui::Vec2, ppp: f32, time: f64) {
        self.commit_typing();
        let Some(path) = self.shown.as_ref().map(|s| s.path.clone()) else {
            return;
        };
        let turns = (self.turns() + if clockwise { 1 } else { 3 }) % 4;
        self.rotations.retain(|(p, _)| !folder::same_path(p, &path));
        if turns != 0 {
            self.rotations.push((path, turns));
        }
        self.view.rotate(clockwise, image, vp, ppp);
        let label = if clockwise {
            "Rotated right"
        } else {
            "Rotated left"
        };
        self.toast = Some((label.to_owned(), time + 1.0));
    }

    // --- marking ------------------------------------------------------------------

    /// The marks on the image on screen.
    fn editor(&self) -> Option<&Editor> {
        let shown = self.shown.as_ref()?;
        self.marks
            .iter()
            .find(|(p, _)| folder::same_path(p, &shown.path))
            .map(|(_, e)| e)
    }

    fn editor_mut(&mut self) -> Option<&mut Editor> {
        let shown = self.shown.as_ref()?;
        self.marks
            .iter_mut()
            .find(|(p, _)| folder::same_path(p, &shown.path))
            .map(|(_, e)| e)
    }

    /// A new mark, on top. Returns its index.
    fn add_mark(&mut self, mark: Mark) -> Option<usize> {
        let path = self.shown.as_ref().map(|s| s.path.clone())?;
        if self.editor().is_none() {
            self.marks.push((path, Editor::default()));
        }
        let size = self.shown.as_ref().map_or([0, 0], |s| s.fit.full_size);
        let e = self.editor_mut()?;
        e.size = size;
        Some(e.add(mark))
    }

    /// Marks saved for an image of another size (the file was replaced) can't
    /// be placed on this one.
    fn check_marks_fit(&mut self) {
        let Some(size) = self.shown.as_ref().map(|s| s.fit.full_size) else {
            return;
        };
        if let Some(e) = self.editor_mut()
            && e.size != size
            && e.size != [0, 0]
        {
            e.drop_loaded();
            e.size = size;
        }
    }

    /// Hand every changed set of marks to the screenshots folder's writer.
    fn autosave(&mut self) {
        for (path, editor) in &mut self.marks {
            let Some(set) = editor.take_unsaved() else {
                continue;
            };
            self.marks_in_flight += 1;
            let (tx, waker, at) = (self.tx.clone(), self.waker.clone(), path.clone());
            self.captures.save_marks(path.clone(), set, move |result| {
                let _ = tx.send(Msg::MarksSaved { path: at, result });
                waker.wake();
            });
        }
    }

    fn set_tool(&mut self, tool: Option<Tool>) {
        self.commit_typing();
        self.tool = tool;
        self.drag = None;
        if tool.is_none()
            && let Some(e) = self.editor_mut()
        {
            e.selected = None;
        }
    }

    fn toggle_tool(&mut self, tool: Tool) {
        self.set_tool((self.tool != Some(tool)).then_some(tool));
    }

    /// Have `tool` in hand, as when one of its colours is picked.
    fn arm(&mut self, tool: Tool) {
        if self.tool != Some(tool) {
            self.set_tool(Some(tool));
        }
    }

    /// Recolour the selected mark (`paint` knows which kinds it applies to).
    fn recolour(&mut self, paint: impl FnOnce(&mut Kind)) {
        if let Some(e) = self.editor_mut()
            && let Some(i) = e.selected
            && let Some(m) = e.marks().get(i)
        {
            let mut after = m.clone();
            paint(&mut after.kind);
            e.change(i, after);
        }
    }

    /// Where the image is on screen in `viewport`, if one is shown.
    fn placement(&self, viewport: Rect, ppp: f32) -> Option<Placement> {
        let size = self.shown.as_ref().map(Shown::full_size)?;
        let turns = self.turns();
        let rect = self
            .view
            .image_rect(viewport, view::turned(size, turns), ppp);
        Some(Placement { rect, size, turns })
    }

    /// The handle of the selected mark at screen point `p`.
    fn handle_at(&self, at: &Placement, p: Pos2) -> Option<(usize, Handle)> {
        let e = self.editor()?;
        let index = e.selected?;
        let m = e.marks().get(index)?;
        at.handles(m)
            .find(|(_, c)| (*c - p).abs().max_elem() <= HANDLE_REACH)
            .map(|(h, _)| (index, h))
    }

    /// Mark `index` (`m`) as `drag` has it, if `drag` is moving or resizing it.
    fn dragged(&self, drag: Drag, index: usize, m: &Mark, at: &Placement) -> Option<Mark> {
        match drag {
            Drag::Move { index: i, from, to } if i == index => Some(moved(m, to - from, at.size)),
            Drag::Resize {
                index: i,
                handle,
                from,
                to,
            } if i == index => {
                let r = tools::resized(m, handle, to - from, at.size, MIN_DRAW / at.zoom())?;
                let (Some(ctx), Some(t)) = (&self.ctx, r.text()) else {
                    return Some(r);
                };
                // Text: the box is what the text measures at its new size.
                let [w, h] = on_image(marks::measure(ctx, t), t.turn);
                let (x, y) = tools::anchored(m, handle, w, h);
                Some(Mark { x, y, w, h, ..r })
            }
            _ => None,
        }
    }

    /// A drag that moved or resized a mark is over: keep what it did.
    fn drop_drag(&mut self, drag: Drag, at: &Placement) {
        let (Drag::Move { index, .. } | Drag::Resize { index, .. }) = drag else {
            return;
        };
        let after = self
            .editor()
            .and_then(|e| e.marks().get(index))
            .and_then(|m| self.dragged(drag, index, m, at));
        let Some(after) = after else { return };
        if let (Drag::Resize { .. }, Some(t)) = (drag, after.text()) {
            self.text_size = Some(t.size);
        }
        if let Some(e) = self.editor_mut() {
            e.change(index, after);
        }
    }

    /// Start typing with the text's top left at image pixel `anchor`: new
    /// text, or mark `index` retyped.
    fn start_typing(&mut self, index: Option<usize>, anchor: Pos2, zoom: f32) {
        self.commit_typing();
        self.drag = None;
        let retyped = index.and_then(|i| self.editor()?.marks().get(i)?.text().cloned());
        let (body, size, pen) = match retyped {
            Some(t) => (t.body, t.size, t.pen),
            None => {
                let readable = (TEXT_POINTS / zoom).round() as u32;
                let size = readable.clamp(Text::MIN_SIZE, Text::MAX_SIZE);
                (
                    String::new(),
                    self.text_size.unwrap_or(size),
                    edit::current_pen(),
                )
            }
        };
        if let Some(e) = self.editor_mut() {
            e.selected = None;
        }
        self.typing = Some(Typing {
            index,
            anchor,
            body,
            size,
            pen,
            screen: Rect::NOTHING,
            focused: false,
            id: egui::Id::new("whir.image.typing").with(TYPED.fetch_add(1, Ordering::Relaxed)),
        });
    }

    /// Finish the text being typed: it becomes a mark (or the mark it was),
    /// selected. With nothing typed it is dropped, and so is the mark it was.
    fn commit_typing(&mut self) {
        let Some(t) = self.typing.take() else { return };
        let (Some(ctx), Some(image)) =
            (self.ctx.clone(), self.shown.as_ref().map(Shown::full_size))
        else {
            return;
        };
        let body = t.body.replace('\r', "");
        if body.trim().is_empty() {
            if let Some(i) = t.index
                && let Some(e) = self.editor_mut()
            {
                e.remove(i);
            }
            return;
        }
        // Upright on screen as it is turned now.
        let turns = self.turns();
        let text = Text {
            body,
            size: t.size,
            pen: t.pen,
            turn: turns,
        };
        let [w, h] = marks::measure(&ctx, &text);
        // The box it fills on screen from the anchor, back on the image.
        let far = t.anchor + marks::turn_vec(vec2(w as f32, h as f32), (4 - turns) % 4);
        let r = Rect::from_two_pos(t.anchor, far);
        let (w, h) = (r.width().round() as u32, r.height().round() as u32);
        let (x, y) = tools::placed(r.min, w, h, image);
        let mark = Mark {
            x,
            y,
            w,
            h,
            kind: Kind::Text(text),
        };
        self.text_size = Some(t.size);
        let index = match t.index {
            Some(i) => {
                if let Some(e) = self.editor_mut() {
                    e.change(i, mark);
                }
                Some(i)
            }
            None => self.add_mark(mark),
        };
        if let Some(e) = self.editor_mut() {
            e.selected = index;
        }
    }

    /// Pointer input while a tool is armed: a drag resizes the selected mark
    /// by a handle, moves it, or (the highlighter) draws a new one; a click
    /// selects, or (the text tool, on bare image) starts typing; a double
    /// click on text retypes it.
    fn handle_tool(&mut self, ui: &egui::Ui, resp: &egui::Response, at: &Placement) {
        let Some(tool) = self.tool else { return };
        let size = at.size;
        let inside = |p: Pos2| pos2(p.x.clamp(0.0, size.x), p.y.clamp(0.0, size.y));
        let over_selected = |doc: &Self, p: Pos2| {
            doc.editor()
                .and_then(|e| e.marks().get(e.selected?))
                .is_some_and(|m| m.contains(p.x, p.y))
        };
        let swallowed = self.swallow;

        if !swallowed
            && resp.drag_started_by(PointerButton::Primary)
            && let Some(origin) = ui.input(|i| i.pointer.press_origin())
        {
            let p = at.pixel(origin);
            let selected = self.editor().and_then(|e| e.selected);
            self.drag = match (self.handle_at(at, origin), selected) {
                (Some((index, handle)), _) => Some(Drag::Resize {
                    index,
                    handle,
                    from: p,
                    to: p,
                }),
                (None, Some(index)) if over_selected(self, p) => Some(Drag::Move {
                    index,
                    from: p,
                    to: p,
                }),
                _ if tool == Tool::Highlight => Some(Drag::Draw {
                    from: inside(p),
                    to: inside(p),
                }),
                _ => None,
            };
        }
        if resp.dragged_by(PointerButton::Primary)
            && let Some(p) = resp.interact_pointer_pos()
        {
            match &mut self.drag {
                Some(Drag::Draw { to, .. }) => *to = inside(at.pixel(p)),
                Some(Drag::Move { to, .. } | Drag::Resize { to, .. }) => *to = at.pixel(p),
                None => {}
            }
        }
        if resp.drag_stopped_by(PointerButton::Primary) {
            match self.drag.take() {
                Some(Drag::Draw { from, to }) => {
                    let min = MIN_DRAW / at.zoom();
                    if let Some(m) = drawn(from, to, size, min, edit::current_ink()) {
                        self.add_mark(m);
                    }
                }
                Some(drag) => self.drop_drag(drag, at),
                None => {}
            }
        }
        if !swallowed
            && resp.clicked_by(PointerButton::Primary)
            && let Some(p) = resp.interact_pointer_pos()
        {
            let px = at.pixel(p);
            let hit = self.editor().and_then(|e| e.hit(px.x, px.y));
            let on_image = (0.0..size.x).contains(&px.x) && (0.0..size.y).contains(&px.y);
            if hit.is_none() && tool == Tool::Text && on_image {
                self.start_typing(None, px, at.zoom());
            } else if let Some(e) = self.editor_mut() {
                e.selected = hit;
            }
        }
        if !swallowed
            && resp.double_clicked_by(PointerButton::Primary)
            && let Some(p) = resp.interact_pointer_pos()
        {
            let px = at.pixel(p);
            let text = self.editor().and_then(|e| {
                let i = e.hit(px.x, px.y)?;
                let m = e.marks()[i].text().is_some().then(|| &e.marks()[i])?;
                Some((i, at.mark_rect(m).min))
            });
            if let Some((i, corner)) = text {
                self.start_typing(Some(i), at.pixel(corner), at.zoom());
            }
        }

        if resp.hovered() || resp.dragged() {
            let icon = match self.drag {
                Some(Drag::Move { .. }) => CursorIcon::Move,
                Some(Drag::Resize { handle, .. }) => resize_cursor(handle, at.turns),
                Some(Drag::Draw { .. }) => CursorIcon::Crosshair,
                None => match resp.hover_pos() {
                    Some(p) => match self.handle_at(at, p) {
                        Some((_, handle)) => resize_cursor(handle, at.turns),
                        None if over_selected(self, at.pixel(p)) => CursorIcon::Move,
                        None if tool == Tool::Text => CursorIcon::Text,
                        None => CursorIcon::Crosshair,
                    },
                    None => CursorIcon::Crosshair,
                },
            };
            ui.ctx().set_cursor_icon(icon);
        }
    }

    /// The marks, over the image: where they are, or where a drag has them;
    /// the selected one outlined, with its handles.
    fn paint_marks(&self, painter: &egui::Painter, at: &Placement, theme: &Theme) {
        if let Some(e) = self.editor() {
            for (i, m) in e.marks().iter().enumerate() {
                // Being retyped: the editor shows it.
                if self.typing.as_ref().is_some_and(|t| t.index == Some(i)) {
                    continue;
                }
                let dragged = self.drag.and_then(|d| self.dragged(d, i, m, at));
                let m = dragged.as_ref().unwrap_or(m);
                let r = at.mark_rect(m);
                paint_mark(painter, r, m, at);
                if e.selected == Some(i) {
                    painter.rect_stroke(
                        r,
                        0.0,
                        Stroke::new(1.5, theme.accent),
                        egui::StrokeKind::Outside,
                    );
                    for (_, c) in at.handles(m) {
                        let grip = Rect::from_center_size(c, Vec2::splat(HANDLE));
                        painter.rect_filled(grip, 1.0, Color32::WHITE);
                        painter.rect_stroke(
                            grip,
                            1.0,
                            Stroke::new(1.0, theme.accent),
                            egui::StrokeKind::Inside,
                        );
                    }
                }
            }
        }
        if let Some(Drag::Draw { from, to }) = self.drag
            && let Some(m) = drawn(from, to, at.size, 0.0, edit::current_ink())
        {
            paint_mark(painter, at.mark_rect(&m), &m, at);
        }
    }

    /// The editor for the text being typed, where its letters will be, with
    /// their halo under them.
    fn show_typing(&mut self, ui: &mut egui::Ui, at: &Placement, viewport: Rect) {
        let lost = {
            let Some(t) = &mut self.typing else { return };
            let zoom = at.zoom();
            let style = Text {
                body: String::new(),
                size: t.size,
                pen: t.pen,
                turn: 0,
            };
            let font = FontId::proportional((t.size as f32 * zoom).min(marks::SCREEN_TEXT_MAX));
            let color = t.pen.color();
            // Laid out as the mark will be: on one line per line, never wrapped.
            let mut layouter = |ui: &egui::Ui, text: &dyn TextBuffer, _wrap: f32| {
                let mut job =
                    LayoutJob::simple(text.as_str().to_owned(), font.clone(), color, f32::INFINITY);
                job.keep_trailing_whitespace = true;
                ui.fonts_mut(|f| f.layout_job(job))
            };
            let laid_out = layouter(ui, &t.body, f32::INFINITY);
            // Room for the caret and the next letter.
            let size = laid_out.size() + vec2(font.size, 0.0);
            let origin = at.screen(t.anchor) + Vec2::splat(style.halo() * zoom);
            let painter = ui.painter().with_clip_rect(viewport);
            let under = painter.add(Shape::Noop);
            let edit = egui::TextEdit::multiline(&mut t.body)
                .id(t.id)
                .frame(egui::Frame::NONE)
                .font(font.clone())
                .text_color(color)
                .desired_width(size.x)
                .desired_rows(1)
                .lock_focus(true)
                .layouter(&mut layouter);
            let place = UiBuilder::new()
                .max_rect(Rect::from_min_size(origin, size))
                .layout(Layout::top_down_justified(Align::Min));
            let out = ui.scope_builder(place, |ui| edit.show(ui)).inner;
            let halo = t.pen.halo();
            let shapes = style
                .halo_offsets()
                .into_iter()
                .map(|o| {
                    let pos = out.galley_pos + o * zoom;
                    let shape = TextShape::new(pos, out.galley.clone(), halo);
                    Shape::Text(shape.with_override_text_color(halo))
                })
                .collect();
            painter.set(under, Shape::Vec(shapes));
            if !t.focused {
                out.response.request_focus();
            }
            let lost = t.focused && !out.response.has_focus();
            t.focused |= out.response.has_focus();
            t.screen = out.response.rect;
            lost
        };
        if lost {
            self.commit_typing();
        }
    }

    // --- copying out --------------------------------------------------------------

    /// The image on screen: its file, marks and quarter turns, and the fonts
    /// its text is set in.
    fn on_screen(&self) -> Option<(PathBuf, Vec<Mark>, u8, Option<FontDefinitions>)> {
        let shown = self.shown.as_ref()?;
        let marks = self.editor().map_or(Vec::new(), |e| e.marks().to_vec());
        let fonts = self
            .ctx
            .as_ref()
            .filter(|_| marks.iter().any(|m| m.text().is_some()))
            .map(|ctx| ctx.fonts(|f| f.definitions().clone()));
        Some((shown.path.clone(), marks, self.turns(), fonts))
    }

    /// Put the image on the clipboard as it is on screen.
    fn copy_image(&mut self) {
        if self.copying {
            return;
        }
        self.commit_typing();
        let Some((path, marks, turns, fonts)) = self.on_screen() else {
            return;
        };
        self.copying = true;
        let (tx, waker) = (self.tx.clone(), self.waker.clone());
        pool::submit(Priority::Visible, move || {
            let result =
                flattened(&path, &marks, turns, true, fonts.as_ref()).map(|d| bake::pack_dib(&d));
            let _ = tx.send(Msg::Copied(result));
            waker.wake();
        });
    }

    /// Write the image as it is on screen to `dest`, as a PNG.
    fn export(&mut self, dest: PathBuf) {
        self.commit_typing();
        let Some((path, marks, turns, fonts)) = self.on_screen() else {
            return;
        };
        self.exporting = true;
        let (tx, waker) = (self.tx.clone(), self.waker.clone());
        pool::submit(Priority::Visible, move || {
            let result = flattened(&path, &marks, turns, false, fonts.as_ref()).and_then(|d| {
                let layout = if d.has_alpha {
                    PixelLayout::PremultipliedBgra
                } else {
                    PixelLayout::Bgrx
                };
                let png = encode_png(&d.pixels, d.width, d.height, d.stride, layout)
                    .map_err(|e| e.to_string())?;
                whir_core::file::write_atomic(&dest, |f| f.write_all(&png))
                    .map_err(|e| e.message(&file_title(&dest)))?;
                Ok(dest)
            });
            let _ = tx.send(Msg::Exported(result));
            waker.wake();
        });
    }

    // --- drawing -----------------------------------------------------------------

    fn checker(&mut self, ctx: &egui::Context, theme: &Theme) -> egui::TextureId {
        self.checker
            .get_or_insert_with(|| {
                let (a, b) = (theme.checker_a, theme.checker_b);
                ctx.load_texture(
                    "whir-checker",
                    ColorImage::new([2, 2], vec![a, b, b, a]),
                    TextureOptions::NEAREST_REPEAT,
                )
            })
            .id()
    }

    fn handle_input(
        &mut self,
        ui: &egui::Ui,
        resp: &egui::Response,
        viewport: Rect,
        image: egui::Vec2,
        requests: &mut Vec<DocRequest>,
    ) {
        let ppp = ui.ctx().pixels_per_point();
        let vp = viewport.size();
        let time = ui.input(|i| i.time);
        let before = self.view.zoom(image, vp, ppp);
        let picked_before = zoom_pref(&self.view, ppp);

        // A press anywhere but on the text being typed finishes it, and is
        // used up by that: it doesn't also select, draw or start more text.
        let press = ui.input(|i| i.pointer.any_pressed().then(|| i.pointer.interact_pos()));
        if let (Some(t), Some(Some(p))) = (&self.typing, press)
            && !t.screen.expand(4.0).contains(p)
        {
            self.commit_typing();
            self.swallow = true;
        }
        let typing = self.typing.is_some();

        // Wheel zooms around the cursor (smoothed by egui); Ctrl+wheel / pinch too.
        if resp.hovered() {
            let (scroll, zoom_delta, pointer) =
                ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.pointer.hover_pos()));
            let mut factor = zoom_delta;
            if scroll.y != 0.0 {
                factor *= (scroll.y / 200.0).exp();
            }
            if factor != 1.0 {
                let anchor = pointer.map_or(egui::Vec2::ZERO, |p| p - viewport.center());
                self.view.zoom_by(factor, anchor, image, vp, ppp);
            }
        }
        if let Some(at) = self.placement(viewport, ppp) {
            self.handle_tool(ui, resp, &at);
        }
        if self.swallow && !ui.input(|i| i.pointer.any_down()) {
            self.swallow = false;
        }
        // A tool takes the primary button; the middle one still pans.
        if (self.tool.is_none() && resp.dragged_by(PointerButton::Primary))
            || resp.dragged_by(PointerButton::Middle)
        {
            self.view.pan(resp.drag_delta(), image, vp, ppp);
        }
        if resp.double_clicked() && self.tool.is_none() {
            let anchor = resp
                .interact_pointer_pos()
                .map_or(egui::Vec2::ZERO, |p| p - viewport.center());
            match self.view.mode {
                Mode::Fit if view::fit_zoom(image, vp, ppp) < view::actual_size_zoom(ppp) => {
                    self.view
                        .set_zoom(view::actual_size_zoom(ppp), anchor, image, vp, ppp);
                }
                _ => self.view.mode = Mode::Fit,
            }
        }

        // Keyboard (this tab is active whenever ui() runs).
        let mut nav = 0isize;
        let mut turn = None;
        let mut tool = None;
        // Some(true): undo; Some(false): redo.
        let mut history = None;
        let (mut escape, mut delete) = (false, false);
        let (mut copy, mut finish) = (false, false);
        // From the context menu: fit (true) or actual size.
        let mut fit_pick = None;
        ui.input_mut(|i| {
            let none = Modifiers::NONE;
            if typing {
                // Every other key is the text's (Ctrl+C copies from it).
                finish =
                    i.consume_key(Modifiers::CTRL, Key::Enter) || i.consume_key(none, Key::Escape);
                return;
            }
            // Ctrl+C arrives as a copy event, not a key.
            if i.events.iter().any(|e| matches!(e, Event::Copy)) {
                i.events.retain(|e| !matches!(e, Event::Copy));
                copy = true;
            }
            if i.consume_key(none, Key::H) {
                tool = Some(Tool::Highlight);
            }
            if i.consume_key(none, Key::T) {
                tool = Some(Tool::Text);
            }
            // The Shift form first (see R below).
            if i.consume_key(Modifiers::CTRL | Modifiers::SHIFT, Key::Z)
                || i.consume_key(Modifiers::CTRL, Key::Y)
            {
                history = Some(false);
            } else if i.consume_key(Modifiers::CTRL, Key::Z) {
                history = Some(true);
            }
            if self.tool.is_some() {
                escape = i.consume_key(none, Key::Escape);
                delete = i.consume_key(none, Key::Delete) || i.consume_key(none, Key::Backspace);
            }
            if i.consume_key(none, Key::ArrowRight) || i.consume_key(none, Key::PageDown) {
                nav = 1;
            }
            if i.consume_key(none, Key::ArrowLeft) || i.consume_key(none, Key::PageUp) {
                nav = -1;
            }
            if i.consume_key(none, Key::Home) {
                nav = isize::MIN;
            }
            if i.consume_key(none, Key::End) {
                nav = isize::MAX;
            }
            if i.consume_key(none, Key::Num0) || i.consume_key(Modifiers::CTRL, Key::Num0) {
                self.view.mode = Mode::Fit;
            }
            if i.consume_key(none, Key::Num1) {
                self.view.set_zoom(
                    view::actual_size_zoom(ppp),
                    egui::Vec2::ZERO,
                    image,
                    vp,
                    ppp,
                );
            }
            let zoom_in = i.consume_key(none, Key::Plus)
                || i.consume_key(none, Key::Equals)
                || i.consume_key(Modifiers::CTRL, Key::Plus)
                || i.consume_key(Modifiers::CTRL, Key::Equals);
            let zoom_out =
                i.consume_key(none, Key::Minus) || i.consume_key(Modifiers::CTRL, Key::Minus);
            if zoom_in {
                self.view.zoom_by(1.25, egui::Vec2::ZERO, image, vp, ppp);
            }
            if zoom_out {
                self.view.zoom_by(0.8, egui::Vec2::ZERO, image, vp, ppp);
            }
            // Shift+R first: a pattern without Shift also matches with it.
            if i.consume_key(Modifiers::SHIFT, Key::R) {
                turn = Some(false);
            } else if i.consume_key(none, Key::R) {
                turn = Some(true);
            }
        });
        let fitted = self.view.mode == Mode::Fit;
        let actual = (self.view.zoom(image, vp, ppp) * ppp - 1.0).abs() < 1e-3;
        resp.context_menu(|ui| {
            ui.set_min_width(190.0);
            let item = egui::Button::new("Fit to window")
                .shortcut_text("0")
                .selected(fitted);
            if ui.add(item).clicked() {
                fit_pick = Some(true);
                ui.close();
            }
            let item = egui::Button::new("Actual size")
                .shortcut_text("1")
                .selected(actual);
            if ui.add(item).clicked() {
                fit_pick = Some(false);
                ui.close();
            }
            ui.separator();
            let item = egui::Button::new("Copy image").shortcut_text("Ctrl+C");
            if ui.add(item).clicked() {
                copy = true;
                ui.close();
            }
            let item = egui::Button::new("Save a copy…").shortcut_text("Ctrl+Shift+S");
            if ui.add(item).clicked() {
                requests.push(DocRequest::SaveAs);
                ui.close();
            }
            ui.separator();
            let right = egui::Button::new("Rotate right").shortcut_text("R");
            if ui.add(right).clicked() {
                turn = Some(true);
                ui.close();
            }
            let left = egui::Button::new("Rotate left").shortcut_text("Shift+R");
            if ui.add(left).clicked() {
                turn = Some(false);
                ui.close();
            }
        });
        match fit_pick {
            Some(true) => self.view.mode = Mode::Fit,
            Some(false) => {
                let actual = view::actual_size_zoom(ppp);
                self.view.set_zoom(actual, egui::Vec2::ZERO, image, vp, ppp);
            }
            None => {}
        }
        if finish {
            self.commit_typing();
        }
        if copy {
            self.copy_image();
        }
        if let Some(tool) = tool {
            self.toggle_tool(tool);
        }
        if let Some(undo) = history
            && let Some(e) = self.editor_mut()
        {
            let _ = if undo { e.undo() } else { e.redo() };
        }
        if delete
            && let Some(e) = self.editor_mut()
            && let Some(i) = e.selected
        {
            e.remove(i);
        }
        if escape {
            // One thing at a time: the drag, then the selection, then the tool.
            let selected = self.editor().and_then(|e| e.selected).is_some();
            if self.drag.take().is_some() {
                // Cancelled.
            } else if selected {
                if let Some(e) = self.editor_mut() {
                    e.selected = None;
                }
            } else {
                self.set_tool(None);
            }
        }
        if nav != 0 {
            self.ensure_folder();
            self.navigate(nav, time);
            return;
        }
        if let Some(clockwise) = turn {
            self.rotate(clockwise, image, vp, ppp, time);
            return;
        }

        let after = self.view.zoom(image, vp, ppp);
        if (after - before).abs() > f32::EPSILON {
            let pct = (after * ppp * 100.0).round();
            self.toast = Some((format!("{} × {}  ·  {pct}%", image.x, image.y), time + 1.0));
        }
        let picked = zoom_pref(&self.view, ppp);
        if picked != picked_before {
            // Someone zoomed: with "the zoom I used last", what opens next follows.
            prefs::used(picked);
        }
    }

    /// Put the current image at the zoom it opens at, once it is the one on
    /// screen (going ←/→, the last one stays up until the next is decoded).
    fn apply_opening(&mut self, image: egui::Vec2, vp: egui::Vec2, ppp: f32) {
        let on_screen = self
            .shown
            .as_ref()
            .is_some_and(|s| folder::same_path(&s.path, &self.path));
        if !self.opening || !on_screen {
            return;
        }
        self.opening = false;
        match prefs::initial() {
            ZoomPref::Fit => self.view.mode = Mode::Fit,
            ZoomPref::Percent(p) => {
                let zoom = p / 100.0 * view::actual_size_zoom(ppp);
                self.view = View::default();
                self.view.set_zoom(zoom, egui::Vec2::ZERO, image, vp, ppp);
            }
        }
    }

    fn paint(&mut self, ui: &egui::Ui, viewport: Rect, theme: &Theme) {
        let ppp = ui.ctx().pixels_per_point();
        let turns = self.turns();
        let Some(shown) = &self.shown else { return };
        let image = view::turned(shown.full_size(), turns);
        let rect = self.view.image_rect(viewport, image, ppp);
        let painter = ui.painter().with_clip_rect(viewport);

        // Need more pixels than the fit level holds? Fetch full resolution.
        let screen_px_per_level_px =
            self.view.zoom(image, viewport.size(), ppp) * ppp / shown.fit.scale();
        let wants_full = screen_px_per_level_px > 1.05;

        if shown.best().has_alpha {
            let cell = 8.0;
            let uv = Rect::from_min_max(
                pos2(0.0, 0.0),
                pos2(rect.width() / (2.0 * cell), rect.height() / (2.0 * cell)),
            );
            let visible = rect.intersect(viewport);
            let clip = painter.with_clip_rect(visible);
            let checker = self.checker(ui.ctx(), theme);
            clip.image(checker, rect, uv, Color32::WHITE);
        }

        let shown = self.shown.as_ref().expect("checked above");
        let level = shown.best();
        // A level pixel corner → screen, through the quarter turns.
        let [lw, lh] = level.size.map(|v| v as f32);
        let to_screen = |x: u32, y: u32| {
            let p = view::turn_point(pos2(x as f32 / lw, y as f32 / lh), turns);
            rect.min + p.to_vec2() * rect.size()
        };
        for tile in &level.tiles {
            let ([x0, y0], [w, h]) = (tile.origin, tile.size);
            let corners = [
                to_screen(x0, y0),
                to_screen(x0 + w, y0),
                to_screen(x0 + w, y0 + h),
                to_screen(x0, y0 + h),
            ];
            if !Rect::from_points(&corners).intersects(viewport) {
                continue;
            }
            let uvs = [
                pos2(0.0, 0.0),
                pos2(1.0, 0.0),
                pos2(1.0, 1.0),
                pos2(0.0, 1.0),
            ];
            let mut mesh = Mesh::with_texture(tile.texture.id());
            for (pos, uv) in corners.into_iter().zip(uvs) {
                mesh.vertices.push(Vertex {
                    pos,
                    uv,
                    color: Color32::WHITE,
                });
            }
            mesh.add_triangle(0, 1, 2);
            mesh.add_triangle(0, 2, 3);
            painter.add(mesh);
        }
        let at = Placement {
            rect,
            size: shown.full_size(),
            turns,
        };
        self.paint_marks(&painter, &at, theme);
        if wants_full && self.visible {
            self.request_full();
        }
    }

    fn paint_toast(&mut self, ui: &egui::Ui, viewport: Rect, theme: &Theme) {
        let Some((text, until)) = &self.toast else {
            return;
        };
        let now = ui.input(|i| i.time);
        if now >= *until {
            self.toast = None;
            return;
        }
        let alpha = ((until - now) / 0.25).clamp(0.0, 1.0) as f32;
        let painter = ui.painter();
        let galley = painter.layout_no_wrap(
            text.clone(),
            egui::FontId::proportional(12.5),
            theme.text.gamma_multiply(alpha),
        );
        let pad = vec2(12.0, 6.0);
        let size = galley.size() + pad * 2.0;
        let rect = Rect::from_center_size(
            pos2(viewport.center().x, viewport.bottom() - 28.0 - size.y / 2.0),
            size,
        );
        painter.rect_filled(rect, size.y / 2.0, theme.overlay_bg.gamma_multiply(alpha));
        painter.galley(rect.min + pad, galley, theme.text);
        ui.ctx().request_repaint_after(
            Duration::from_millis(30).min(Duration::from_secs_f64(until - now)),
        );
    }
}

fn file_title(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

impl Document for ImageDoc {
    fn title(&self) -> &str {
        &self.title
    }

    fn path(&self) -> Option<&Path> {
        Some(&self.path)
    }

    fn kind(&self) -> DocKind {
        DocKind::Image
    }

    fn is_loading(&self) -> bool {
        self.loading && self.error.is_none()
    }

    fn on_visibility(&mut self, visible: bool) {
        if !visible {
            self.commit_typing();
            prefs::save_last();
        }
        self.visible = visible;
        let priority = if visible {
            Priority::Visible
        } else {
            Priority::Background
        };
        if let Some(j) = &self.fit_job {
            j.set_priority(priority);
        }
        if visible {
            self.ensure_folder();
            self.prefetch_neighbours();
        } else {
            // Background tabs keep only their fit level.
            if let Some(j) = self.full_job.take() {
                j.cancel();
            }
            if let Some(s) = &mut self.shown
                && !s.fit.is_full_resolution()
            {
                s.full = None;
            }
            self.cache.clear();
        }
    }

    fn poll(&mut self) {
        self.drain();
    }

    /// A capture's highlights save themselves; any other image has nothing
    /// to save (its highlights last as long as the tab).
    fn persistence(&self) -> Persistence {
        if self.marks.iter().any(|(_, e)| e.stored) {
            Persistence::Auto
        } else {
            Persistence::None
        }
    }

    fn is_dirty(&self) -> bool {
        self.marks.iter().any(|(_, e)| e.unsaved())
    }

    fn is_saving(&self) -> bool {
        self.exporting || self.marks_in_flight > 0
    }

    fn save_error(&self) -> Option<&str> {
        self.marks_error.as_deref()
    }

    fn flush(&mut self) {
        self.commit_typing();
        self.autosave();
        prefs::save_last();
    }

    fn flush_blocking(&mut self, deadline: Instant) -> Result<(), String> {
        self.commit_typing();
        self.autosave();
        prefs::save_last();
        while self.marks_in_flight > 0 {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(msg) => self.handle(msg),
                Err(_) => return Err("The highlights are still being saved.".to_owned()),
            }
        }
        self.marks_error.clone().map_or(Ok(()), Err)
    }

    fn discard(&mut self) {
        for (_, e) in &mut self.marks {
            e.forget_unsaved();
        }
    }

    /// Nothing here is ever unsaved (the file is never changed), but Ctrl+S
    /// on a marked-up image means "keep this": offer a copy with the marks in.
    fn save(&mut self) -> SaveResult {
        self.commit_typing();
        let marked = self.editor().is_some_and(|e| !e.marks().is_empty());
        match self.save_as_spec() {
            Some(spec) if marked => SaveResult::NeedsPath(spec),
            _ => SaveResult::Unsupported,
        }
    }

    fn save_as_spec(&self) -> Option<SaveAsSpec> {
        let shown = self.shown.as_ref()?;
        let stem = shown.path.file_stem()?.to_string_lossy();
        Some(SaveAsSpec {
            title: "Save a copy".to_owned(),
            suggested_name: format!("{stem} copy.png"),
            filters: vec![("PNG image".to_owned(), "*.png".to_owned())],
            default_ext: "png".to_owned(),
            folder: shown.path.parent().map(Path::to_owned),
        })
    }

    /// Exports what is on screen; the tab stays on the original.
    fn save_as(&mut self, path: PathBuf) -> SaveResult {
        let Some(shown) = &self.shown else {
            return SaveResult::Unsupported;
        };
        if folder::same_path(&path, &shown.path) {
            return SaveResult::Failed(
                "Give the copy another name: the original is kept as it is.".to_owned(),
            );
        }
        self.export(path);
        SaveResult::Started
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, _cx: &mut DocCx<'_>) {
        let theme = Theme::current();

        // The highlighter, and its inks.
        let ink = edit::current_ink();
        let on = self.tool == Some(Tool::Highlight);
        let pressed =
            toolbar::tool_button(ui, Glyph::Highlighter, on, Some(ink.color()), true, &theme);
        if pressed.clicked {
            self.toggle_tool(Tool::Highlight);
        }
        let inks = egui::Id::new("whir.image.inks");
        if pressed.menu {
            toolbar::open_flyout(ui.ctx(), inks);
        }
        let button = pressed.response.rect;
        if !egui::Popup::is_id_open(ui.ctx(), inks) {
            pressed
                .response
                .on_hover_text("Highlight (H) · hold or right-click for colours");
        }
        toolbar::flyout(ui, inks, button, |ui| {
            let colours = Ink::ALL.map(Ink::color);
            let current = Ink::ALL.iter().position(|&i| i == ink).unwrap_or(0);
            let Some(picked) = toolbar::swatches(ui, &colours, current, &theme) else {
                return false;
            };
            let ink = Ink::ALL[picked];
            edit::set_ink(ink);
            // Picking a colour is reaching for the pen; a selected highlight takes it too.
            self.arm(Tool::Highlight);
            self.recolour(|kind| {
                if let Kind::Highlight(i) = kind {
                    *i = ink;
                }
            });
            true
        });

        // Text, and its pens.
        let pen = edit::current_pen();
        let on = self.tool == Some(Tool::Text);
        let pressed = toolbar::tool_button(ui, Glyph::Text, on, Some(pen.color()), true, &theme);
        if pressed.clicked {
            self.toggle_tool(Tool::Text);
        }
        let pens = egui::Id::new("whir.image.pens");
        if pressed.menu {
            toolbar::open_flyout(ui.ctx(), pens);
        }
        let button = pressed.response.rect;
        if !egui::Popup::is_id_open(ui.ctx(), pens) {
            pressed
                .response
                .on_hover_text("Text (T) · hold or right-click for colours");
        }
        toolbar::flyout(ui, pens, button, |ui| {
            let colours = Pen::ALL.map(Pen::color);
            let current = Pen::ALL.iter().position(|&p| p == pen).unwrap_or(0);
            let Some(picked) = toolbar::swatches(ui, &colours, current, &theme) else {
                return false;
            };
            let pen = Pen::ALL[picked];
            edit::set_pen(pen);
            self.arm(Tool::Text);
            if let Some(t) = &mut self.typing {
                t.pen = pen;
            }
            self.recolour(|kind| {
                if let Kind::Text(t) = kind {
                    t.pen = pen;
                }
            });
            true
        });
    }

    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut DocCx<'_>) {
        self.ctx = Some(ui.ctx().clone());
        self.drain();
        if let Some(dib) = self.clip.take() {
            cx.requests.push(DocRequest::CopyImage(ClipImage(dib)));
        }
        if let Some(text) = self.notice.take() {
            self.toast = Some((text, ui.input(|i| i.time) + 2.0));
        }
        let theme = Theme::current();
        let (viewport, resp) =
            ui.allocate_exact_size(ui.available_size(), Sense::CLICK | Sense::DRAG);
        ui.painter().rect_filled(viewport, 0.0, theme.canvas_bg);

        if let Some(size) = self.shown.as_ref().map(Shown::full_size) {
            let image = view::turned(size, self.turns());
            let ppp = ui.ctx().pixels_per_point();
            self.apply_opening(image, viewport.size(), ppp);
            self.handle_input(ui, &resp, viewport, image, cx.requests);
            self.paint(ui, viewport, &theme);
            if let Some(at) = self.placement(viewport, ppp) {
                self.show_typing(ui, &at, viewport);
            }
            if self.tool.is_none() && self.view.pannable(image, viewport.size(), ppp) {
                ui.ctx().set_cursor_icon(if resp.dragged() {
                    CursorIcon::Grabbing
                } else {
                    CursorIcon::Grab
                });
            }
        } else if let Some(err) = &self.error {
            whir_core::message_doc::show_message(
                ui,
                "Can't show this image",
                err,
                Some(&self.path),
            );
            // Still allow ←/→ past a broken file.
            let (mut nav, time) = (0isize, ui.input(|i| i.time));
            ui.input_mut(|i| {
                if i.consume_key(Modifiers::NONE, Key::ArrowRight) {
                    nav = 1;
                }
                if i.consume_key(Modifiers::NONE, Key::ArrowLeft) {
                    nav = -1;
                }
            });
            if nav != 0 {
                self.ensure_folder();
                self.navigate(nav, time);
            }
        }
        // Marks are saved as they are made: there is no Save for them.
        self.autosave();
        if self.loading && self.shown.is_none() && self.error.is_none() {
            ui.put(
                Rect::from_center_size(viewport.center(), vec2(24.0, 24.0)),
                egui::Spinner::new().size(20.0).color(theme.text_dim),
            );
        }
        self.paint_toast(ui, viewport, &theme);
    }
}

impl Drop for ImageDoc {
    fn drop(&mut self) {
        if let Some(j) = self.fit_job.take() {
            j.cancel();
        }
        if let Some(j) = self.full_job.take() {
            j.cancel();
        }
    }
}

#[cfg(test)]
mod ui_tests;
