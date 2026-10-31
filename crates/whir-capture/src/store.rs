//! Where captures go: PNG files in the screenshots folder, named for the
//! moment they were taken.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use whir_core::{captures, library};
use windows::Win32::System::SystemInformation::GetLocalTime;

/// "Screenshot 2026-10-10 141207": sorts by time, and reads as one.
pub fn capture_stem(
    year: u16,
    month: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
) -> String {
    format!("Screenshot {year:04}-{month:02}-{day:02} {hour:02}{minute:02}{second:02}")
}

fn stem_now() -> String {
    let t = unsafe { GetLocalTime() };
    capture_stem(t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond)
}

/// Write `png` into the screenshots folder (created if needed) under a new
/// name. Never replaces a file: a second capture in the same second gets " (2)".
pub fn save_png(png: &[u8]) -> io::Result<PathBuf> {
    let dir = captures::resolve_dir()
        .ok_or_else(|| io::Error::other("the Documents folder isn't available"))?;
    save_png_in(&dir, &stem_now(), png)
}

fn save_png_in(dir: &Path, stem: &str, png: &[u8]) -> io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    library::place_new(dir, stem, "png", |f| f.write_all(png)).map(|(path, _)| path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_sort_by_time() {
        assert_eq!(
            capture_stem(2026, 10, 10, 14, 12, 7),
            "Screenshot 2026-10-10 141207"
        );
        assert_eq!(
            capture_stem(2026, 1, 2, 3, 4, 5),
            "Screenshot 2026-01-02 030405"
        );
        assert!(capture_stem(2026, 9, 30, 23, 59, 59) < capture_stem(2026, 10, 1, 0, 0, 0));
    }

    #[test]
    fn two_captures_in_one_second_both_survive() {
        let dir = std::env::temp_dir().join(format!("whir-capture-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // The folder doesn't exist yet.
        let dir = dir.join("Screenshots");
        let stem = capture_stem(2026, 10, 10, 14, 12, 7);
        let a = save_png_in(&dir, &stem, b"first").unwrap();
        let b = save_png_in(&dir, &stem, b"second").unwrap();
        assert_eq!(a.file_name().unwrap(), "Screenshot 2026-10-10 141207.png");
        assert_eq!(
            b.file_name().unwrap(),
            "Screenshot 2026-10-10 141207 (2).png"
        );
        assert_eq!(std::fs::read(&a).unwrap(), b"first");
        assert_eq!(std::fs::read(&b).unwrap(), b"second");
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }

    #[test]
    fn saved_captures_refresh_a_recently_scanned_library() {
        use std::sync::mpsc;
        use std::time::Duration;

        use whir_core::{Captures, Waker};
        use whir_platform::ipc;

        let root =
            std::env::temp_dir().join(format!("whir-capture-library-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("Screenshots");
        let (tx, rx) = mpsc::channel();
        let library = Captures::at(
            dir.clone(),
            Waker::new(move || {
                let _ = tx.send(());
            }),
        );
        library.scan();
        rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let before = library.snapshot();
        assert!(before.scanned && before.entries.is_empty());
        assert!(!dir.exists());

        let stem = capture_stem(2026, 10, 10, 14, 12, 7);
        let mut saved = Vec::new();
        for png in [b"first".as_slice(), b"second".as_slice()] {
            let path = save_png_in(&dir, &stem, png).unwrap();
            // The app normalizes the handed-off path before opening its tab.
            let opened = ipc::canonicalize(&path);
            assert!(library.contains(&opened));
            assert_eq!(std::fs::read(&opened).unwrap(), png);
            saved.push(opened.clone());
            // Opening must explicitly refresh, even if the drawer stayed visible.
            library.refresh(vec![opened.clone()]);
            rx.recv_timeout(Duration::from_secs(10)).unwrap();
            let after = library.snapshot();
            assert_eq!(after.entries.len(), saved.len());
            assert_eq!(after.entries[0].path, opened);
            assert_eq!(after.entries[0].size, png.len() as u64);
            assert_eq!(after.generation, before.generation + saved.len() as u64);
            assert_eq!(after.scanned_at, before.scanned_at);
            assert!(after.scanned && !after.scanning);
            assert_eq!(after.error, None);
        }
        let after = library.snapshot();
        assert_eq!(after.entries[1].path, saved[0]);
        assert_ne!(saved[0], saved[1]);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
        let _ = std::fs::remove_dir_all(root);
    }
}
