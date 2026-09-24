//! The PDF thread. PDFium is not thread-safe, so exactly one thread owns the
//! library, every document and every page. Documents send it the set of tiles
//! they currently want (visible first); a newer request preempts an in-flight
//! render between page objects via `IFSDK_PAUSE`.
//!
//! Text extraction runs on the same thread, after screen tiles: one page at
//! a time (it can't be interrupted), urgent pages (visible, need geometry)
//! before the minimap's thumbnails, and those before the background sweep
//! that feeds search.

use std::collections::{HashMap, VecDeque};
use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use whir_core::texture::{Filter, PixelFormat, TextureDesc, TextureRegistry, UserTexture};
use whir_core::{FileSource, Waker};

use crate::ffi::{self, Api, TextApi};
use crate::geom::{Affine, NO_BOX, PageGeom, build_lines, is_empty};
use crate::text::PageText;

/// One rendered piece of a page at one scale.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TileKey {
    pub page: u32,
    /// Render scale in 1/1000 pixels per PDF point.
    pub scale: u32,
    pub tx: u16,
    pub ty: u16,
    /// A minimap thumbnail (always a whole page) rather than a screen tile.
    pub thumb: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Job {
    pub key: TileKey,
    /// Tile rect within the page raster: x, y, w, h (pixels).
    pub rect: [u32; 4],
    /// Size of the whole page raster at this scale.
    pub page_px: [u32; 2],
}

pub enum Event {
    Opened {
        pages: Vec<[f32; 2]>,
        /// pdfium.dll has the text API (selection and search work).
        text: bool,
    },
    Failed(String),
    Tile {
        key: TileKey,
        texture: UserTexture,
    },
    /// The page couldn't be loaded/rendered/uploaded; don't ask again.
    TileFailed {
        key: TileKey,
    },
    /// A page's text (with geometry if it was asked for urgently).
    Text {
        page: u32,
        /// Per-document sequence number; echo the highest seen in `want_text`.
        seq: u64,
        text: Box<PageText>,
    },
}

enum Cmd {
    Open {
        doc: u64,
        path: PathBuf,
        events: Sender<Event>,
        waker: Waker,
        textures: Arc<dyn TextureRegistry>,
    },
    Want {
        doc: u64,
        jobs: Vec<Job>,
    },
    /// Pages whose text and geometry are needed now (replaces the last list).
    WantText {
        doc: u64,
        pages: Vec<u32>,
        /// The highest `Event::Text::seq` the document had received.
        seen: u64,
    },
    /// `Some(p)`: extract every page not sent yet, starting at `p`, wrapping.
    /// `None`: stop.
    Sweep {
        doc: u64,
        from: Option<u32>,
    },
    Close {
        doc: u64,
    },
}

struct Engine {
    tx: Sender<Cmd>,
    /// Set whenever a command is sent: asks the current render to pause.
    interrupt: Arc<AtomicBool>,
}

fn engine() -> &'static Engine {
    static ENGINE: OnceLock<Engine> = OnceLock::new();
    ENGINE.get_or_init(|| {
        let (tx, rx) = channel();
        let interrupt = Arc::new(AtomicBool::new(false));
        let flag = interrupt.clone();
        std::thread::Builder::new()
            .name("pdfium".into())
            .spawn(move || run(rx, flag))
            .expect("spawn pdf thread");
        Engine { tx, interrupt }
    })
}

fn send(cmd: Cmd) {
    let e = engine();
    e.interrupt.store(true, Ordering::Release);
    let _ = e.tx.send(cmd);
}

/// Send without pausing the current render: text requests never preempt tiles.
fn send_quiet(cmd: Cmd) {
    let _ = engine().tx.send(cmd);
}

/// Handle a document holds; closing it releases everything on the PDF thread.
pub struct DocHandle {
    id: u64,
}

impl DocHandle {
    pub fn open(
        path: PathBuf,
        events: Sender<Event>,
        waker: Waker,
        textures: Arc<dyn TextureRegistry>,
    ) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        send(Cmd::Open {
            doc: id,
            path,
            events,
            waker,
            textures,
        });
        Self { id }
    }

    /// Replace the set of wanted tiles (in priority order).
    pub fn want(&self, jobs: Vec<Job>) {
        send(Cmd::Want { doc: self.id, jobs });
    }

    /// Replace the pages whose text and geometry are needed soon (in order).
    /// `seen` is the highest `Event::Text::seq` received so far: pages sent
    /// after it are already on their way and aren't extracted twice.
    pub fn want_text(&self, pages: Vec<u32>, seen: u64) {
        send_quiet(Cmd::WantText {
            doc: self.id,
            pages,
            seen,
        });
    }

    /// Start (from a page) or stop extracting every page's text in the background.
    pub fn sweep(&self, from: Option<u32>) {
        send_quiet(Cmd::Sweep { doc: self.id, from });
    }
}

impl Drop for DocHandle {
    fn drop(&mut self) {
        send(Cmd::Close { doc: self.id });
    }
}

// --- PDF thread ------------------------------------------------------------------

struct OpenDoc {
    handle: ffi::Document,
    /// `FPDF_FILEACCESS::m_Param`: must outlive the document.
    file: *const FileSource,
    access: Box<ffi::FileAccess>,
    events: Sender<Event>,
    waker: Waker,
    textures: Arc<dyn TextureRegistry>,
    pages: VecDeque<(u32, ffi::Page)>,
    wanted: VecDeque<Job>,
    /// Page sizes (display points), for the text transform.
    sizes: Vec<[f32; 2]>,
    text: TextQueue,
    /// Sweep results sent without waking the UI yet (wakes are rate-limited).
    wake_pending: bool,
    last_wake: Instant,
}

const OPEN_PAGES: usize = 6;
/// Sweep results wake the UI at most this often.
const SWEEP_WAKE: Duration = Duration::from_millis(100);
/// Characters extracted per page at most (a pathological page is truncated).
const MAX_PAGE_CHARS: i32 = 500_000;

impl OpenDoc {
    fn wake(&mut self) {
        self.waker.wake();
        self.wake_pending = false;
        self.last_wake = Instant::now();
    }
}

/// Which pages' text to extract next, per document.
#[derive(Default)]
struct TextQueue {
    /// Need geometry now; always first, re-extracted even if sent before
    /// (the UI evicted the geometry).
    urgent: VecDeque<u32>,
    /// Background walk over every page: (next page, pages left to visit).
    sweep: Option<(u32, u32)>,
    /// Text already sent to the UI (which keeps it): the sweep skips these.
    sent: Vec<bool>,
    /// Per page, the `seq` of the last send that carried geometry (0: none).
    geom_seq: Vec<u64>,
    /// Text events sent so far.
    seq: u64,
}

impl TextQueue {
    fn new(pages: usize) -> Self {
        Self {
            sent: vec![false; pages],
            geom_seq: vec![0; pages],
            ..Self::default()
        }
    }

    /// Replace the urgent list. The UI re-sends its list as pages arrive, so
    /// pages whose geometry went out after the last one it had received
    /// (`seen`) are in flight: asking again would extract them twice.
    fn set_urgent(&mut self, pages: Vec<u32>, seen: u64) {
        let n = self.sent.len() as u32;
        self.urgent = pages
            .into_iter()
            .filter(|&p| p < n && self.geom_seq[p as usize] <= seen)
            .collect();
    }

    fn set_sweep(&mut self, from: Option<u32>) {
        let n = self.sent.len() as u32;
        self.sweep = from.filter(|_| n > 0).map(|p| (p.min(n - 1), n));
    }

    fn has_work(&self) -> bool {
        !self.urgent.is_empty() || self.sweep.is_some()
    }

    fn next_urgent(&mut self) -> Option<u32> {
        self.urgent.pop_front()
    }

    /// The sweep's next unsent page; ends itself after one lap.
    fn next_sweep(&mut self) -> Option<u32> {
        let n = self.sent.len() as u32;
        while let Some((next, left)) = self.sweep {
            if left == 0 {
                self.sweep = None;
                break;
            }
            self.sweep = Some(((next + 1) % n, left - 1));
            if !self.sent[next as usize] {
                return Some(next);
            }
        }
        None
    }

    /// `(page, urgent)`.
    #[cfg(test)]
    fn next(&mut self) -> Option<(u32, bool)> {
        self.next_urgent()
            .map(|p| (p, true))
            .or_else(|| self.next_sweep().map(|p| (p, false)))
    }

    /// Record a send (`geometry`: it carried boxes and lines); returns its `seq`.
    fn mark_sent(&mut self, page: u32, geometry: bool) -> u64 {
        self.seq += 1;
        if let Some(s) = self.sent.get_mut(page as usize) {
            *s = true;
        }
        if geometry && let Some(g) = self.geom_seq.get_mut(page as usize) {
            *g = self.seq;
        }
        self.seq
    }
}

unsafe extern "C" fn get_block(param: *mut c_void, position: u32, buf: *mut u8, size: u32) -> i32 {
    let file = unsafe { &*(param as *const FileSource) };
    let out = unsafe { std::slice::from_raw_parts_mut(buf, size as usize) };
    file.read_exact_at(position as u64, out).is_ok() as i32
}

unsafe extern "C" fn need_to_pause(this: *mut ffi::Pause) -> i32 {
    let flag = unsafe { &*((*this).user as *const AtomicBool) };
    flag.load(Ordering::Acquire) as i32
}

fn run(rx: Receiver<Cmd>, interrupt: Arc<AtomicBool>) {
    let loaded = Api::load().inspect(|api| unsafe { (api.init_library)() });
    let api: Result<&Api, &str> = loaded.as_ref().map_err(String::as_str);
    let mut docs: HashMap<u64, OpenDoc> = HashMap::new();

    loop {
        // Block only when there's nothing to render or extract.
        let idle = docs
            .values()
            .all(|d| d.wanted.is_empty() && !d.text.has_work());
        if idle {
            // Rate-limited sweep wakes go out before we sleep.
            for d in docs.values_mut().filter(|d| d.wake_pending) {
                d.wake();
            }
            match rx.recv() {
                Ok(cmd) => handle(cmd, api, &mut docs),
                Err(_) => return,
            }
        }
        loop {
            match rx.try_recv() {
                Ok(cmd) => handle(cmd, api, &mut docs),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        interrupt.store(false, Ordering::Release);

        let Ok(api) = api else { continue };
        let render_next = |docs: &mut HashMap<u64, OpenDoc>, doc_id: u64| {
            let job = docs.get_mut(&doc_id).and_then(|d| d.wanted.pop_front());
            if let Some(job) = job {
                render(api, &rx, &interrupt, docs, doc_id, job);
            }
        };
        // Screen tiles first, always, in the order asked for (a visible page's
        // thumbnail may lead as its quick preview).
        let screen = docs
            .iter()
            .find(|(_, d)| d.wanted.iter().any(|j| !j.key.thumb))
            .map(|(&id, _)| id);
        if let Some(doc_id) = screen {
            render_next(&mut docs, doc_id);
            continue;
        }
        // Then one page of urgent text (what's on screen needs its geometry)…
        let text = api.text.as_ref();
        if let Some(t) = text
            && let Some((doc_id, page)) = docs
                .iter_mut()
                .find_map(|(&id, d)| d.text.next_urgent().map(|p| (id, p)))
        {
            extract_and_send(api, t, &mut docs, doc_id, page, true);
            continue;
        }
        // …then the minimap's thumbnails…
        if let Some((&doc_id, _)) = docs.iter().find(|(_, d)| !d.wanted.is_empty()) {
            render_next(&mut docs, doc_id);
            continue;
        }
        // …then one page of the background sweep.
        if let Some(t) = text
            && let Some((doc_id, page)) = docs
                .iter_mut()
                .find_map(|(&id, d)| d.text.next_sweep().map(|p| (id, p)))
        {
            extract_and_send(api, t, &mut docs, doc_id, page, false);
        }
    }
}

fn handle(cmd: Cmd, api: Result<&Api, &str>, docs: &mut HashMap<u64, OpenDoc>) {
    match cmd {
        Cmd::Open {
            doc,
            path,
            events,
            waker,
            textures,
        } => {
            let result = match api {
                Ok(api) => open_doc(api, &path, events.clone(), waker.clone(), textures),
                Err(e) => Err(format!(
                    "{e} PDF viewing needs pdfium.dll next to whir.exe."
                )),
            };
            match result {
                Ok((d, pages)) => {
                    docs.insert(doc, d);
                    let text = api.is_ok_and(|a| a.text.is_some());
                    let _ = events.send(Event::Opened { pages, text });
                }
                Err(e) => {
                    let _ = events.send(Event::Failed(e));
                }
            }
            waker.wake();
        }
        Cmd::Want { doc, jobs } => {
            if let Some(d) = docs.get_mut(&doc) {
                d.wanted = jobs.into();
            }
        }
        // Without the text API these stay empty, so nothing is ever extracted.
        Cmd::WantText { doc, pages, seen } => {
            if let Some(d) = docs.get_mut(&doc)
                && api.is_ok_and(|a| a.text.is_some())
            {
                d.text.set_urgent(pages, seen);
            }
        }
        Cmd::Sweep { doc, from } => {
            if let Some(d) = docs.get_mut(&doc)
                && api.is_ok_and(|a| a.text.is_some())
            {
                d.text.set_sweep(from);
            }
        }
        Cmd::Close { doc } => {
            if let (Some(d), Ok(api)) = (docs.remove(&doc), api) {
                close_doc(api, d);
            }
        }
    }
}

fn open_doc(
    api: &Api,
    path: &std::path::Path,
    events: Sender<Event>,
    waker: Waker,
    textures: Arc<dyn TextureRegistry>,
) -> Result<(OpenDoc, Vec<[f32; 2]>), String> {
    let file = FileSource::open(path).map_err(|e| format!("Couldn't open the file: {e}"))?;
    if file.len() > u32::MAX as u64 {
        return Err("PDFs larger than 4 GB aren't supported.".into());
    }
    let file = Arc::into_raw(file);
    let mut access = Box::new(ffi::FileAccess {
        file_len: unsafe { (*file).len() } as u32,
        get_block: Some(get_block),
        param: file as *mut c_void,
    });
    let handle = unsafe { (api.load_custom_document)(&mut *access, std::ptr::null()) };
    if handle.is_null() {
        let msg = api.error_message();
        drop(unsafe { Arc::from_raw(file) });
        return Err(msg);
    }
    let count = unsafe { (api.get_page_count)(handle) }.max(0);
    let pages: Vec<[f32; 2]> = (0..count)
        .map(|i| {
            let mut s = ffi::SizeF::default();
            unsafe { (api.get_page_size_by_index_f)(handle, i, &mut s) };
            [s.width.max(1.0), s.height.max(1.0)]
        })
        .collect();
    Ok((
        OpenDoc {
            handle,
            file,
            access,
            events,
            waker,
            textures,
            pages: VecDeque::new(),
            wanted: VecDeque::new(),
            sizes: pages.clone(),
            text: TextQueue::new(pages.len()),
            wake_pending: false,
            last_wake: Instant::now(),
        },
        pages,
    ))
}

fn close_doc(api: &Api, d: OpenDoc) {
    unsafe {
        for (_, p) in &d.pages {
            (api.close_page)(*p);
        }
        (api.close_document)(d.handle);
        drop(Arc::from_raw(d.file));
    }
    drop(d.access);
}

fn page(api: &Api, d: &mut OpenDoc, index: u32) -> Option<ffi::Page> {
    if let Some(pos) = d.pages.iter().position(|(i, _)| *i == index) {
        let entry = d.pages.remove(pos)?;
        d.pages.push_back(entry);
        return Some(entry.1);
    }
    let p = unsafe { (api.load_page)(d.handle, index as i32) };
    if p.is_null() {
        return None;
    }
    d.pages.push_back((index, p));
    while d.pages.len() > OPEN_PAGES {
        if let Some((_, old)) = d.pages.pop_front() {
            unsafe { (api.close_page)(old) };
        }
    }
    Some(p)
}

fn render(
    api: &Api,
    rx: &Receiver<Cmd>,
    interrupt: &Arc<AtomicBool>,
    docs: &mut HashMap<u64, OpenDoc>,
    doc_id: u64,
    job: Job,
) {
    let [x, y, w, h] = job.rect;
    let failed = |docs: &mut HashMap<u64, OpenDoc>| {
        if let Some(d) = docs.get_mut(&doc_id) {
            let _ = d.events.send(Event::TileFailed { key: job.key });
            d.wake();
        }
    };
    if w == 0 || h == 0 {
        failed(docs);
        return;
    }
    let Some(d) = docs.get_mut(&doc_id) else {
        return;
    };
    // Thumbnails borrow an open page or load a temporary one, so the minimap
    // never evicts pages the screen needs.
    let (page, temporary) = if job.key.thumb {
        match d.pages.iter().find(|(i, _)| *i == job.key.page) {
            Some(&(_, p)) => (Some(p), false),
            None => {
                let p = unsafe { (api.load_page)(d.handle, job.key.page as i32) };
                ((!p.is_null()).then_some(p), true)
            }
        }
    } else {
        (page(api, d, job.key.page), false)
    };
    let Some(page) = page else {
        failed(docs);
        return;
    };
    let release = || {
        if temporary {
            unsafe { (api.close_page)(page) };
        }
    };

    let stride = w as usize * 4;
    let mut buf = vec![0u8; stride * h as usize];
    let bitmap = unsafe {
        (api.bitmap_create_ex)(
            w as i32,
            h as i32,
            ffi::BITMAP_BGRA,
            buf.as_mut_ptr().cast(),
            stride as i32,
        )
    };
    if bitmap.is_null() {
        release();
        failed(docs);
        return;
    }
    unsafe { (api.bitmap_fill_rect)(bitmap, 0, 0, w as i32, h as i32, 0xFFFF_FFFF) };

    let mut pause = ffi::Pause {
        version: 1,
        need_to_pause_now: Some(need_to_pause),
        user: Arc::as_ptr(interrupt) as *mut c_void,
    };
    let mut status = unsafe {
        (api.render_start)(
            bitmap,
            page,
            -(x as i32),
            -(y as i32),
            job.page_px[0] as i32,
            job.page_px[1] as i32,
            0,
            ffi::FLAG_ANNOT,
            &mut pause,
        )
    };
    let mut abandoned = false;
    while status == ffi::RENDER_TOBECONTINUED {
        // Paused because new commands arrived: apply them, then decide whether
        // this tile is still wanted.
        let mut closing = false;
        let mut rewanted = false;
        while let Ok(cmd) = rx.try_recv() {
            match cmd {
                Cmd::Close { doc } if doc == doc_id => closing = true,
                other => {
                    rewanted |= matches!(&other, Cmd::Want { doc, .. } if *doc == doc_id);
                    handle(other, Ok(api), docs);
                }
            }
        }
        interrupt.store(false, Ordering::Release);
        if closing {
            unsafe {
                (api.render_close)(page);
                (api.bitmap_destroy)(bitmap);
            }
            release();
            if let Some(d) = docs.remove(&doc_id) {
                close_doc(api, d);
            }
            return;
        }
        let Some(d) = docs.get_mut(&doc_id) else {
            abandoned = true;
            break;
        };
        // Only this document's new wish list can make the tile unwanted; commands
        // for other documents (or a spurious wake) just resume the render.
        if rewanted {
            match d.wanted.iter().position(|j| j.key == job.key) {
                // A screen tile now comes first: the thumbnail stays queued for later.
                Some(pos) if job.key.thumb && d.wanted.iter().take(pos).any(|j| !j.key.thumb) => {
                    abandoned = true;
                    break;
                }
                Some(pos) => {
                    d.wanted.remove(pos); // still wanted: keep going
                }
                None => {
                    abandoned = true;
                    break;
                }
            }
        }
        status = unsafe { (api.render_continue)(page, &mut pause) };
    }
    unsafe {
        (api.render_close)(page);
        (api.bitmap_destroy)(bitmap);
    }
    release();
    if abandoned {
        return;
    }
    if status != ffi::RENDER_DONE {
        failed(docs);
        return;
    }
    let Some(d) = docs.get_mut(&doc_id) else {
        return;
    };
    let desc = TextureDesc {
        width: w,
        height: h,
        format: PixelFormat::Bgra8Premultiplied,
        mipmaps: false,
        filter: Filter::Linear,
    };
    match d.textures.clone().create(&desc, &buf, stride) {
        Ok(texture) => {
            let _ = d.events.send(Event::Tile {
                key: job.key,
                texture,
            });
            d.wake();
        }
        Err(_) => failed(docs),
    }
}

// --- text ----------------------------------------------------------------------

/// Extract one page's text and send it to its document. Urgent pages use the
/// render page cache (they're on screen, so usually a hit) and carry
/// geometry; sweep pages borrow a cached page if there is one, else load a
/// temporary page, so the sweep never evicts pages the renderer needs.
fn extract_and_send(
    api: &Api,
    t: &TextApi,
    docs: &mut HashMap<u64, OpenDoc>,
    doc_id: u64,
    index: u32,
    urgent: bool,
) {
    let Some(d) = docs.get_mut(&doc_id) else {
        return;
    };
    let Some(&size) = d.sizes.get(index as usize) else {
        return;
    };
    let (handle, temporary) = if urgent {
        (page(api, d, index), false)
    } else if let Some(&(_, p)) = d.pages.iter().find(|(i, _)| *i == index) {
        (Some(p), false)
    } else {
        let p = unsafe { (api.load_page)(d.handle, index as i32) };
        ((!p.is_null()).then_some(p), true)
    };
    let text = match handle {
        Some(p) => {
            let text = extract(t, p, size, urgent);
            if temporary {
                unsafe { (api.close_page)(p) };
            }
            text
        }
        None => PageText::failed(),
    };
    let seq = d.text.mark_sent(index, text.geom.is_some());
    let _ = d.events.send(Event::Text {
        page: index,
        seq,
        text: Box::new(text),
    });
    if urgent || d.last_wake.elapsed() >= SWEEP_WAKE {
        d.wake();
    } else {
        d.wake_pending = true;
    }
}

/// Page space → display space for `page`, whose display size is `size`.
fn page_affine(t: &TextApi, page: ffi::Page, size: [f32; 2]) -> Option<Affine> {
    // A virtual device the size of a large square: PDFium maps the page onto
    // it with its rotation and crop box; three corners pin the transform.
    const S: i32 = 1 << 16;
    let d2p = |dx: i32, dy: i32| {
        let (mut x, mut y) = (0.0, 0.0);
        let ok = unsafe { (t.device_to_page)(page, 0, 0, S, S, 0, dx, dy, &mut x, &mut y) };
        (ok != 0).then_some([x, y])
    };
    let (p00, p10, p01) = (d2p(0, 0)?, d2p(S, 0)?, d2p(0, S)?);
    Affine::from_corners(p00, p10, p01, size[0] as f64, size[1] as f64)
}

/// A glyph's box in page space: the loose (ascent/descent) box for upright
/// text so lines highlight evenly, the tight box otherwise.
fn char_box_page(t: &TextApi, tp: ffi::TextPage, i: i32) -> Option<[f64; 4]> {
    const EPS: f32 = 1e-3;
    let angle = unsafe { (t.char_angle)(tp, i) };
    let upright = (0.0..EPS).contains(&angle)
        || (std::f32::consts::TAU - EPS..=std::f32::consts::TAU).contains(&angle);
    if upright {
        let mut r = ffi::RectF::default();
        if unsafe { (t.loose_box)(tp, i, &mut r) } != 0 {
            let (w, h) = (r.right - r.left, r.top - r.bottom);
            if [r.left, r.top, r.right, r.bottom]
                .iter()
                .all(|v| v.is_finite())
                && w > 0.01
                && h > 0.01
            {
                return Some([r.left as f64, r.bottom as f64, r.right as f64, r.top as f64]);
            }
        }
    }
    let (mut l, mut r, mut b, mut top) = (0.0, 0.0, 0.0, 0.0);
    if unsafe { (t.char_box)(tp, i, &mut l, &mut r, &mut b, &mut top) } == 0 {
        return None;
    }
    let ok = [l, r, b, top].iter().all(|v| v.is_finite()) && r > l && top > b;
    ok.then_some([l, b, r, top])
}

/// Read a page's characters (and, if `geometry`, their boxes and lines).
fn extract(t: &TextApi, page: ffi::Page, size: [f32; 2], geometry: bool) -> PageText {
    let tp = unsafe { (t.load_page)(page) };
    if tp.is_null() {
        return PageText::failed();
    }
    let count = unsafe { (t.count_chars)(tp) };
    if count < 0 {
        unsafe { (t.close_page)(tp) };
        return PageText::failed();
    }
    let n = count.min(MAX_PAGE_CHARS);
    let xf = page_affine(t, page, size);
    let (w, h) = (size[0], size[1]);
    let mut chars = Vec::with_capacity(n as usize);
    let mut boxes = Vec::with_capacity(n as usize);
    let mut hyphens = Vec::new();
    for i in 0..n {
        let u = unsafe { (t.get_unicode)(tp, i) };
        let mut unit = if u > 0xFFFF { 0xFFFD } else { u as u16 };
        let generated = unsafe { (t.is_generated)(tp, i) } == 1;
        let mut b = NO_BOX;
        if let (false, Some(xf)) = (generated, xf)
            && let Some([l, bottom, r, top]) = char_box_page(t, tp, i)
        {
            b = xf.map_rect(l, bottom, r, top);
        }
        // Text entirely outside the crop box is invisible: don't let search
        // find it or selection land on it.
        let outside = b[2] < -1.0 || b[0] > w + 1.0 || b[3] < -1.0 || b[1] > h + 1.0;
        if !is_empty(&b) && outside {
            unit = 0;
            b = NO_BOX;
        } else if unsafe { (t.is_hyphen)(tp, i) } == 1 {
            hyphens.push(i as u32);
        }
        chars.push(unit);
        boxes.push(b);
    }
    // Boxes are needed for the crop-box filter either way; lines only when asked.
    let geom = (geometry && xf.is_some()).then(|| PageGeom {
        lines: build_lines(&chars, &boxes),
        boxes,
    });
    unsafe { (t.close_page)(tp) };
    PageText {
        chars,
        hyphens,
        geom,
        no_geom: geometry && xf.is_none(),
        failed: false,
    }
}

/// Shared by the tests that load the real pdfium.dll.
#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::{Arc, Mutex, MutexGuard};

    use whir_core::texture::{TextureDesc, TextureRegistry, UserTexture};

    /// PDFium isn't thread-safe: tests that call it hold this.
    pub fn pdfium_lock() -> MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub struct NoTextures;

    impl TextureRegistry for NoTextures {
        fn create(
            self: Arc<Self>,
            _: &TextureDesc,
            _: &[u8],
            _: usize,
        ) -> Result<UserTexture, String> {
            Err("no GPU in tests".into())
        }

        fn max_texture_side(&self) -> u32 {
            4096
        }

        fn release(&self, _: u64) {}
    }

    /// Two pages sharing one content stream: upright 612×792, and one with an
    /// offset MediaBox rotated 90°. "Zq" sits outside both crop boxes.
    pub fn test_pdf() -> Vec<u8> {
        let content = "BT /F1 24 Tf 72 700 Td (Hello world) Tj 0 -30 Td (second line) Tj ET\n\
                       BT /F1 24 Tf 700 700 Td (Zq) Tj ET\n";
        let res = "/Resources << /Font << /F1 5 0 R >> >>";
        let objs = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 2 >>".to_string(),
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R {res} >>"
            ),
            format!(
                "<< /Length {} >>\nstream\n{content}endstream",
                content.len()
            ),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [10 20 622 812] /Rotate 90 \
                 /Contents 4 0 R {res} >>"
            ),
        ];
        assemble(&objs)
    }

    /// `n` upright letter pages, each saying "Page k".
    pub fn pages_pdf(n: usize) -> Vec<u8> {
        let kids: Vec<String> = (0..n).map(|k| format!("{} 0 R", 4 + 2 * k)).collect();
        let mut objs = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            format!("<< /Type /Pages /Kids [{}] /Count {n} >>", kids.join(" ")),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        ];
        for k in 0..n {
            let content = format!("BT /F1 48 Tf 72 700 Td (Page {}) Tj ET\n", k + 1);
            objs.push(format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents {} 0 R \
                 /Resources << /Font << /F1 3 0 R >> >> >>",
                5 + 2 * k
            ));
            objs.push(format!(
                "<< /Length {} >>\nstream\n{content}endstream",
                content.len()
            ));
        }
        assemble(&objs)
    }

    /// Numbered objects → a PDF file with its cross-reference table.
    fn assemble(objs: &[String]) -> Vec<u8> {
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, o) in objs.iter().enumerate() {
            offsets.push(out.len());
            out.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).bytes());
        }
        let xref = out.len();
        out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).bytes());
        for o in offsets {
            out.extend(format!("{o:010} 00000 n \n").bytes());
        }
        out.extend(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objs.len() + 1
            )
            .bytes(),
        );
        out
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{NoTextures, pdfium_lock, test_pdf};
    use super::*;

    #[test]
    fn text_queue_order() {
        let mut q = TextQueue::new(5);
        assert!(!q.has_work());
        q.set_sweep(Some(3));
        // Out-of-range pages are dropped.
        q.set_urgent(vec![1, 9], 0);
        assert_eq!(q.next(), Some((1, true)));
        assert_eq!(q.mark_sent(1, true), 1);
        // The sweep starts at its page, wraps, and skips what was sent.
        let mut order = Vec::new();
        while let Some((p, urgent)) = q.next() {
            assert!(!urgent);
            q.mark_sent(p, false);
            order.push(p);
        }
        assert_eq!(order, vec![3, 4, 0, 2]);
        assert!(!q.has_work(), "one lap, then the sweep ends");
        // Urgent pages are extracted again even if sent without geometry…
        q.set_urgent(vec![3], 5);
        assert_eq!(q.next(), Some((3, true)));
        let seq = q.mark_sent(3, true);
        // …but not while that geometry is still on its way to the UI…
        q.set_urgent(vec![3, 1], seq - 1);
        assert_eq!(q.next(), Some((1, true)));
        assert_eq!(q.next(), None);
        // …only once the UI had it (and then evicted it).
        q.set_urgent(vec![3], seq);
        assert_eq!(q.next(), Some((3, true)));
        // A sweep over sent pages ends without yielding (no busy loop).
        q.set_sweep(Some(0));
        assert!(q.has_work());
        assert_eq!(q.next(), None);
        assert!(!q.has_work());
    }

    #[test]
    fn text_queue_urgent_first_and_stop() {
        let mut q = TextQueue::new(4);
        q.set_sweep(Some(0));
        assert_eq!(q.next(), Some((0, false)));
        // Urgent pages jump ahead of the sweep, and a new list replaces the old.
        q.set_urgent(vec![3, 2], 0);
        q.set_urgent(vec![2], 0);
        assert_eq!(q.next(), Some((2, true)));
        assert_eq!(q.next(), Some((1, false)));
        q.set_sweep(None);
        assert_eq!(q.next(), None);
        assert!(!q.has_work());
        // Empty documents never sweep.
        let mut q = TextQueue::new(0);
        q.set_sweep(Some(0));
        assert!(!q.has_work());
    }

    /// The real extraction path through vendor/pdfium/bin/pdfium.dll.
    /// `cargo test -p whir-pdf -- --ignored`
    #[test]
    #[ignore = "loads pdfium.dll"]
    fn extracts_real_text() {
        let _pdfium = pdfium_lock();
        let api = Api::load().expect("pdfium.dll");
        unsafe { (api.init_library)() };
        let t = api.text.as_ref().expect("text API");
        let path = std::env::temp_dir().join(format!("whir-pdf-text-{}.pdf", std::process::id()));
        std::fs::write(&path, test_pdf()).unwrap();
        let (tx, _rx) = channel();
        let (mut d, sizes) =
            open_doc(&api, &path, tx, Waker::noop(), Arc::new(NoTextures)).expect("open");
        assert_eq!(sizes, vec![[612.0, 792.0], [792.0, 612.0]]);

        // Upright page: two lines with a generated break between them.
        let p = page(&api, &mut d, 0).unwrap();
        let text = extract(t, p, sizes[0], true);
        let s = String::from_utf16_lossy(&text.chars);
        assert!(s.starts_with("Hello world"), "{s:?}");
        assert!(s.contains("second line"), "{s:?}");
        assert!(!s.contains('Z'), "off-page text must vanish: {s:?}");
        let g = text.geom.as_ref().unwrap();
        let lines: Vec<_> = g.lines.iter().filter(|l| !is_empty(&l.rect)).collect();
        assert_eq!(lines.len(), 2, "{:?}", g.lines);
        let h = g.boxes[0];
        // 'H' at x=72, baseline y=700 → display y 92.
        assert!(
            (h[0] - 72.0).abs() < 2.0 && h[1] < 92.0 && h[3] > 92.0,
            "{h:?}"
        );
        let hit = crate::geom::hit_test(g, &text.chars, [h[2] - 1.0, 88.0]).unwrap();
        assert_eq!(hit.caret, 1);
        assert!(hit.inside);

        // Rotated page with an offset MediaBox: the same text runs down the screen.
        let p = page(&api, &mut d, 1).unwrap();
        let text = extract(t, p, sizes[1], true);
        let g = text.geom.as_ref().unwrap();
        // (PDFium may order the two lines either way here.)
        let k = text.chars.iter().position(|&c| c == b'H' as u16).unwrap();
        let h = g.boxes[k];
        // u = y − B = 680 (baseline), v = x − L = 62.
        assert!(
            h[0] < 680.0 && h[2] > 680.0 && (h[1] - 62.0).abs() < 2.0,
            "{h:?}"
        );
        let line = g.lines[g.line_of(k as u32).unwrap()];
        assert!(line.vertical && line.dir == 1, "{line:?}");

        // Sweep extraction without geometry.
        let text = extract(t, p, sizes[1], false);
        assert!(text.geom.is_none() && !text.chars.is_empty());

        close_doc(&api, d);
        let _ = std::fs::remove_file(&path);
    }
}
