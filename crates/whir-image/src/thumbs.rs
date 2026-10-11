//! Small previews of image files, for lists (the drawer's Library). Decoded
//! on the image workers at a fraction of full size, asked for only by rows
//! actually on screen, and let go again when the list moves on.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::SystemTime;

use whir_core::{Filter, PixelFormat, TextureDesc, TextureRegistry, UserTexture, Waker};

use crate::decode;
use crate::pool::{self, JobHandle, Priority};

/// Longest side of a thumbnail, in pixels.
const SIDE: u32 = 256;
/// Texture bytes kept for rows that have scrolled away.
const BUDGET: usize = 8 << 20;

/// A file's modified time and size: a changed file gets a new thumbnail.
pub type Stamp = (SystemTime, u64);

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Thumb {
    Ready {
        texture: egui::TextureId,
        /// The thumbnail's own size.
        size: [u32; 2],
        /// The image's.
        full_size: [u32; 2],
    },
    Loading,
    Failed,
}

enum State {
    Loading(JobHandle),
    Ready {
        texture: UserTexture,
        full_size: [u32; 2],
    },
    Failed,
}

struct Entry {
    stamp: Stamp,
    state: State,
    /// The frame it was last asked for.
    used: u64,
}

type Loaded = (PathBuf, Stamp, Result<(UserTexture, [u32; 2]), String>);

pub struct Thumbs {
    textures: Arc<dyn TextureRegistry>,
    waker: Waker,
    tx: Sender<Loaded>,
    rx: Receiver<Loaded>,
    entries: HashMap<PathBuf, Entry>,
    frame: u64,
}

impl Thumbs {
    pub fn new(textures: Arc<dyn TextureRegistry>, waker: Waker) -> Self {
        let (tx, rx) = channel();
        Self {
            textures,
            waker,
            tx,
            rx,
            entries: HashMap::new(),
            frame: 0,
        }
    }

    /// Call once per frame, before any [`get`](Self::get).
    pub fn begin_frame(&mut self) {
        self.frame += 1;
        while let Ok((path, stamp, result)) = self.rx.try_recv() {
            // Still wanted, and still for this version of the file?
            if let Some(e) = self.entries.get_mut(&path)
                && e.stamp == stamp
            {
                e.state = match result {
                    Ok((texture, full_size)) => State::Ready { texture, full_size },
                    Err(_) => State::Failed,
                };
            }
        }
    }

    /// The thumbnail for `path`, starting to make it if there is none.
    pub fn get(&mut self, path: &Path, stamp: Stamp) -> Thumb {
        let frame = self.frame;
        let fresh = self.entries.get(path).is_some_and(|e| e.stamp == stamp);
        if !fresh {
            let state = State::Loading(self.load(path.to_owned(), stamp));
            let entry = Entry {
                stamp,
                state,
                used: frame,
            };
            self.entries.insert(path.to_owned(), entry);
        }
        let entry = self.entries.get_mut(path).expect("just ensured");
        entry.used = frame;
        match &entry.state {
            State::Loading(_) => Thumb::Loading,
            State::Failed => Thumb::Failed,
            State::Ready { texture, full_size } => Thumb::Ready {
                texture: texture.id(),
                size: texture.size(),
                full_size: *full_size,
            },
        }
    }

    fn load(&self, path: PathBuf, stamp: Stamp) -> JobHandle {
        let (tx, waker, textures) = (self.tx.clone(), self.waker.clone(), self.textures.clone());
        pool::submit(Priority::Background, move || {
            let result = decode::decode(&path, Some(SIDE))
                .map_err(|e| e.to_string())
                .and_then(|d| {
                    let desc = TextureDesc {
                        width: d.width,
                        height: d.height,
                        format: PixelFormat::Bgra8Premultiplied,
                        mipmaps: false,
                        filter: Filter::Linear,
                    };
                    let texture = textures.create(&desc, &d.pixels, d.stride)?;
                    Ok((texture, d.full_size))
                });
            let _ = tx.send((path, stamp, result));
            waker.wake();
        })
    }

    /// Call once per frame, after the last [`get`](Self::get): stops work
    /// nobody asked for this frame, and frees the least recently shown
    /// thumbnails once they add up to more than the budget.
    pub fn end_frame(&mut self) {
        let frame = self.frame;
        self.entries.retain(|_, e| match &e.state {
            State::Loading(job) if e.used != frame => {
                job.cancel();
                false
            }
            _ => true,
        });
        let bytes = |e: &Entry| match &e.state {
            State::Ready { texture, .. } => {
                let [w, h] = texture.size();
                w as usize * h as usize * 4
            }
            _ => 0,
        };
        let mut total: usize = self.entries.values().map(bytes).sum();
        if total <= BUDGET {
            return;
        }
        let mut idle: Vec<(u64, PathBuf, usize)> = self
            .entries
            .iter()
            .filter(|(_, e)| e.used != frame)
            .map(|(p, e)| (e.used, p.clone(), bytes(e)))
            .collect();
        idle.sort();
        for (_, path, size) in idle {
            if total <= BUDGET {
                break;
            }
            self.entries.remove(&path);
            total -= size;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    use super::*;
    use crate::encode::{PixelLayout, encode_png};

    /// Counts the textures alive.
    #[derive(Default)]
    struct Counting {
        next: AtomicU64,
        alive: AtomicU64,
    }

    impl TextureRegistry for Counting {
        fn create(
            self: Arc<Self>,
            desc: &TextureDesc,
            _pixels: &[u8],
            _stride: usize,
        ) -> Result<UserTexture, String> {
            self.alive.fetch_add(1, Ordering::SeqCst);
            let id = self.next.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(UserTexture::new(id, [desc.width, desc.height], self))
        }

        fn max_texture_side(&self) -> u32 {
            4096
        }

        fn release(&self, _id: u64) {
            self.alive.fetch_sub(1, Ordering::SeqCst);
        }
    }

    fn image(dir: &Path, name: &str, w: u32, h: u32) -> (PathBuf, Stamp) {
        unsafe {
            use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let pixels = vec![200u8; (w * h * 4) as usize];
        let png = encode_png(&pixels, w, h, w as usize * 4, PixelLayout::Bgrx).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, png).unwrap();
        let meta = std::fs::metadata(&path).unwrap();
        (path, (meta.modified().unwrap(), meta.len()))
    }

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("whir-thumbs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Ask for `path` every frame until it stops loading.
    fn settled(thumbs: &mut Thumbs, path: &Path, stamp: Stamp) -> Thumb {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            thumbs.begin_frame();
            let t = thumbs.get(path, stamp);
            thumbs.end_frame();
            if t != Thumb::Loading {
                return t;
            }
            assert!(Instant::now() < deadline, "timed out");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn a_thumbnail_is_made_small_and_remade_when_the_file_changes() {
        let dir = test_dir("make");
        let registry = Arc::new(Counting::default());
        let mut thumbs = Thumbs::new(registry.clone(), Waker::noop());
        let (path, stamp) = image(&dir, "wide.png", 1024, 512);
        let Thumb::Ready {
            size, full_size, ..
        } = settled(&mut thumbs, &path, stamp)
        else {
            panic!("not ready");
        };
        assert_eq!((size, full_size), ([256, 128], [1024, 512]));
        assert_eq!(registry.alive.load(Ordering::SeqCst), 1);

        // Replaced by another picture: a new thumbnail, the old one freed.
        let (_, changed) = image(&dir, "wide.png", 300, 600);
        let changed = (changed.0 + Duration::from_secs(1), changed.1);
        let Thumb::Ready { size, .. } = settled(&mut thumbs, &path, changed) else {
            panic!("not ready");
        };
        assert_eq!(size, [128, 256]);
        assert_eq!(registry.alive.load(Ordering::SeqCst), 1);

        // Not an image at all.
        let junk = dir.join("junk.png");
        std::fs::write(&junk, b"not a png").unwrap();
        assert_eq!(settled(&mut thumbs, &junk, stamp), Thumb::Failed);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn work_nobody_wants_any_more_is_dropped() {
        let dir = test_dir("cancel");
        let registry = Arc::new(Counting::default());
        let mut thumbs = Thumbs::new(registry.clone(), Waker::noop());
        let (path, stamp) = image(&dir, "a.png", 64, 64);
        thumbs.begin_frame();
        assert_eq!(thumbs.get(&path, stamp), Thumb::Loading);
        thumbs.end_frame();
        // The row scrolled away before it finished: next frame doesn't ask.
        thumbs.begin_frame();
        thumbs.end_frame();
        assert!(thumbs.entries.is_empty());
        // Whether or not the worker got to it, nothing is kept.
        std::thread::sleep(Duration::from_millis(100));
        thumbs.begin_frame();
        thumbs.end_frame();
        assert_eq!(registry.alive.load(Ordering::SeqCst), 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn thumbnails_off_screen_are_freed_past_the_budget() {
        let dir = test_dir("budget");
        let registry = Arc::new(Counting::default());
        let mut thumbs = Thumbs::new(registry.clone(), Waker::noop());
        // 256 × 256 × 4 bytes each: 32 of them fill the budget.
        let files: Vec<(PathBuf, Stamp)> = (0..40)
            .map(|i| image(&dir, &format!("{i}.png"), 256, 256))
            .collect();
        for (path, stamp) in &files {
            assert!(matches!(
                settled(&mut thumbs, path, *stamp),
                Thumb::Ready { .. }
            ));
        }
        // Each was last shown in its own frame; only the newest fit.
        assert_eq!(registry.alive.load(Ordering::SeqCst), 32);
        assert!(thumbs.entries.contains_key(&files[39].0));
        assert!(!thumbs.entries.contains_key(&files[0].0));
        let _ = std::fs::remove_dir_all(dir);
    }
}
