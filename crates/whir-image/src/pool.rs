//! A tiny decode pool: two workers, priority-ordered, newest-first within a
//! priority, cancellable. Bounding concurrency bounds peak memory when a
//! multi-select opens many large images at once.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Priority {
    /// The image on screen right now.
    Visible = 0,
    /// Full resolution / neighbours of the visible image.
    Prefetch = 1,
    /// Tabs that aren't visible.
    Background = 2,
}

/// Lets the owner re-prioritize or cancel a queued job.
#[derive(Clone)]
pub struct JobHandle {
    priority: Arc<AtomicU8>,
    cancelled: Arc<AtomicBool>,
}

impl JobHandle {
    pub fn set_priority(&self, p: Priority) {
        self.priority.store(p as u8, Ordering::Relaxed);
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

struct Job {
    seq: u64,
    handle: JobHandle,
    run: Box<dyn FnOnce() + Send>,
}

struct Pool {
    queue: Mutex<Vec<Job>>,
    ready: Condvar,
    seq: AtomicU64,
}

const WORKERS: usize = 2;

fn pool() -> &'static Pool {
    static POOL: OnceLock<Pool> = OnceLock::new();
    POOL.get_or_init(|| {
        for i in 0..WORKERS {
            std::thread::Builder::new()
                .name(format!("image-decode-{i}"))
                .spawn(worker)
                .expect("spawn decode worker");
        }
        Pool {
            queue: Mutex::new(Vec::new()),
            ready: Condvar::new(),
            seq: AtomicU64::new(0),
        }
    })
}

pub fn submit(priority: Priority, run: impl FnOnce() + Send + 'static) -> JobHandle {
    let p = pool();
    let handle = JobHandle {
        priority: Arc::new(AtomicU8::new(priority as u8)),
        cancelled: Arc::new(AtomicBool::new(false)),
    };
    let job = Job {
        seq: p.seq.fetch_add(1, Ordering::Relaxed),
        handle: handle.clone(),
        run: Box::new(run),
    };
    p.queue.lock().unwrap_or_else(|e| e.into_inner()).push(job);
    p.ready.notify_one();
    handle
}

fn worker() {
    // WIC wants COM; MTA so decoders never need a message pump.
    unsafe {
        use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let p = pool();
    loop {
        let job = {
            let mut q = p.queue.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                q.retain(|j| !j.handle.is_cancelled());
                // Highest priority first, newest first within a priority.
                let best = q
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, j)| (j.handle.priority.load(Ordering::Relaxed), u64::MAX - j.seq))
                    .map(|(i, _)| i);
                if let Some(i) = best {
                    break q.swap_remove(i);
                }
                q = p.ready.wait(q).unwrap_or_else(|e| e.into_inner());
            }
        };
        if !job.handle.is_cancelled() {
            (job.run)();
        }
    }
}
