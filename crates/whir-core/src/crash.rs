//! What a crash leaves behind. A release build aborts on a panic and has no
//! console, so without this whir just vanishes and nobody can say why.

use std::io::Write as _;
use std::panic::PanicHookInfo;

use crate::settings::app_data_file;
use crate::time::now_local;

/// The log starts over past this, so a crash loop can't fill a disk.
const MAX_LOG: u64 = 1 << 20;

/// Call first thing in `main`: panics go to `%LOCALAPPDATA%\whir\crash.log`.
pub fn install() {
    let next = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        record(info);
        next(info);
    }));
}

fn record(info: &PanicHookInfo<'_>) {
    let Some(path) = app_data_file("crash.log") else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut open = std::fs::OpenOptions::new();
    open.create(true);
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_LOG) {
        open.write(true).truncate(true);
    } else {
        open.append(true);
    }
    if let Ok(mut file) = open.open(&path) {
        let _ = file.write_all(entry(info).as_bytes());
    }
}

fn entry(info: &PanicHookInfo<'_>) -> String {
    let t = now_local();
    let thread = std::thread::current();
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}  whir {}  thread {}\n{info}\n\n",
        t.year,
        t.month,
        t.day,
        t.hour,
        t.minute,
        env!("CARGO_PKG_VERSION"),
        thread.name().unwrap_or("(unnamed)"),
    )
}
