//! Startup / latency tracing. Enabled with `WHIR_TRACE=1`; writes
//! `%LOCALAPPDATA%\whir\trace.log`. Near-zero cost when disabled.
//!
//! Times are milliseconds since *process creation* (not since `main`), so the
//! loader and CRT init are included in the numbers we optimize.

use std::io::Write as _;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::Instant;

struct Tracer {
    /// `Instant` at `init`, and how long the process had existed at that moment.
    base: Instant,
    base_ms: f64,
    marks: Mutex<Vec<(String, f64)>>,
}

static TRACER: OnceLock<Option<Tracer>> = OnceLock::new();

/// Call first thing in `main`.
pub fn init() {
    TRACER.get_or_init(|| {
        std::env::var_os("WHIR_TRACE")?;
        Some(Tracer {
            base: Instant::now(),
            base_ms: ms_since_process_creation(),
            marks: Mutex::new(Vec::with_capacity(64)),
        })
    });
}

#[inline]
pub fn enabled() -> bool {
    matches!(TRACER.get(), Some(Some(_)))
}

/// Milliseconds since process creation, if tracing is enabled.
pub fn now_ms() -> Option<f64> {
    let t = TRACER.get()?.as_ref()?;
    Some(t.base_ms + t.base.elapsed().as_secs_f64() * 1000.0)
}

/// Record a named point in time.
#[inline]
pub fn mark(label: &str) {
    if let Some(Some(t)) = TRACER.get() {
        let ms = t.base_ms + t.base.elapsed().as_secs_f64() * 1000.0;
        if let Ok(mut m) = t.marks.lock() {
            m.push((label.to_owned(), ms));
        }
    }
}

/// Append recorded marks to the trace log and clear them.
pub fn flush() {
    let Some(Some(t)) = TRACER.get() else { return };
    let marks = match t.marks.lock() {
        Ok(mut m) => std::mem::take(&mut *m),
        Err(_) => return,
    };
    if marks.is_empty() {
        return;
    }
    let Some(dir) = std::env::var_os("LOCALAPPDATA") else { return };
    let dir = std::path::Path::new(&dir).join("whir");
    let _ = std::fs::create_dir_all(&dir);
    let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("trace.log"))
    else {
        return;
    };
    let pid = std::process::id();
    let mut out = String::with_capacity(marks.len() * 48);
    for (label, ms) in marks {
        out.push_str(&format!("[{pid}] {ms:9.2} ms  {label}\n"));
    }
    let _ = f.write_all(out.as_bytes());
}

fn ms_since_process_creation() -> f64 {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

    let ft = |f: FILETIME| ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64;
    unsafe {
        let (mut created, mut exited, mut kernel, mut user) = Default::default();
        if GetProcessTimes(
            GetCurrentProcess(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )
        .is_err()
        {
            return 0.0;
        }
        let now = GetSystemTimePreciseAsFileTime();
        // FILETIME ticks are 100ns.
        ft(now).saturating_sub(ft(created)) as f64 / 10_000.0
    }
}
