//! The PDF thread. PDFium is not thread-safe, so exactly one thread owns the
//! library, every document and every page. Documents send it the set of tiles
//! they currently want (visible first); a newer request preempts an in-flight
//! render between page objects via `IFSDK_PAUSE`.

use std::collections::{HashMap, VecDeque};
use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, OnceLock};

use whir_core::texture::{Filter, PixelFormat, TextureDesc, TextureRegistry, UserTexture};
use whir_core::{FileSource, Waker};

use crate::ffi::{self, Api};

/// One rendered piece of a page at one scale.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TileKey {
    pub page: u32,
    /// Render scale in 1/1000 pixels per PDF point.
    pub scale: u32,
    pub tx: u16,
    pub ty: u16,
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
}

const OPEN_PAGES: usize = 6;

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
        // Block only when there's nothing to render.
        let idle = docs.values().all(|d| d.wanted.is_empty());
        if idle {
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
        let Some((&doc_id, _)) = docs.iter().find(|(_, d)| !d.wanted.is_empty()) else {
            continue;
        };
        let job = docs.get_mut(&doc_id).and_then(|d| d.wanted.pop_front());
        if let Some(job) = job {
            render(api, &rx, &interrupt, &mut docs, doc_id, job);
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
                    let _ = events.send(Event::Opened { pages });
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
    let pages = (0..count)
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
    let failed = |docs: &HashMap<u64, OpenDoc>| {
        if let Some(d) = docs.get(&doc_id) {
            let _ = d.events.send(Event::TileFailed { key: job.key });
            d.waker.wake();
        }
    };
    if w == 0 || h == 0 {
        failed(docs);
        return;
    }
    let Some(d) = docs.get_mut(&doc_id) else {
        return;
    };
    let Some(page) = page(api, d, job.key.page) else {
        failed(docs);
        return;
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
            if let Some(pos) = d.wanted.iter().position(|j| j.key == job.key) {
                d.wanted.remove(pos); // still wanted: keep going
            } else {
                abandoned = true;
                break;
            }
        }
        status = unsafe { (api.render_continue)(page, &mut pause) };
    }
    unsafe {
        (api.render_close)(page);
        (api.bitmap_destroy)(bitmap);
    }
    if abandoned {
        return;
    }
    if status != ffi::RENDER_DONE {
        failed(docs);
        return;
    }
    let Some(d) = docs.get(&doc_id) else { return };
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
            d.waker.wake();
        }
        Err(_) => failed(docs),
    }
}
