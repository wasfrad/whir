//! The clipboard: plain text (CF_UNICODETEXT) and images (CF_DIB).

use std::sync::atomic::{AtomicIsize, Ordering};

use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows::Win32::System::Ole::{CF_DIB, CF_UNICODETEXT};

static OWNER: AtomicIsize = AtomicIsize::new(0);

/// The window that owns clipboard data we set (the main window).
pub fn set_owner(hwnd: HWND) {
    OWNER.store(hwnd.0 as isize, Ordering::Relaxed);
}

/// OpenClipboard fails while another process holds it; retry briefly.
fn open() -> bool {
    let owner = HWND(OWNER.load(Ordering::Relaxed) as *mut _);
    for _ in 0..10 {
        if unsafe { OpenClipboard(Some(owner)) }.is_ok() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    false
}

pub fn get_text() -> Option<String> {
    if !open() {
        return None;
    }
    let text = unsafe {
        (|| {
            let h = GetClipboardData(CF_UNICODETEXT.0 as u32).ok()?;
            let hg = HGLOBAL(h.0);
            let ptr = GlobalLock(hg) as *const u16;
            if ptr.is_null() {
                return None;
            }
            // Whoever put this here may not have ended it with a NUL.
            let units = std::slice::from_raw_parts(ptr, GlobalSize(hg) / 2);
            let len = units.iter().position(|&c| c == 0).unwrap_or(units.len());
            let s = String::from_utf16_lossy(&units[..len]);
            let _ = GlobalUnlock(hg);
            Some(s.replace("\r\n", "\n"))
        })()
    };
    unsafe {
        let _ = CloseClipboard();
    }
    text
}

pub fn set_text(text: &str) -> bool {
    // Windows convention is CRLF.
    let wide: Vec<u16> = text
        .replace("\r\n", "\n")
        .replace('\n', "\r\n")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let bytes = unsafe { std::slice::from_raw_parts(wide.as_ptr().cast::<u8>(), wide.len() * 2) };
    put(CF_UNICODETEXT.0 as u32, bytes)
}

/// `dib` is a packed DIB: a BITMAPINFOHEADER followed by its pixels. Windows
/// offers it to other programs as a bitmap too.
pub fn set_image(dib: &[u8]) -> bool {
    put(CF_DIB.0 as u32, dib)
}

/// Replace the clipboard's contents with `bytes` in `format`.
fn put(format: u32, bytes: &[u8]) -> bool {
    if !open() {
        return false;
    }
    let ok = unsafe {
        (|| {
            EmptyClipboard().ok()?;
            let hg = GlobalAlloc(GMEM_MOVEABLE, bytes.len()).ok()?;
            let ptr = GlobalLock(hg) as *mut u8;
            if ptr.is_null() {
                let _ = GlobalFree(Some(hg));
                return None;
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
            let _ = GlobalUnlock(hg);
            if SetClipboardData(format, Some(HANDLE(hg.0))).is_err() {
                let _ = GlobalFree(Some(hg));
                return None;
            }
            Some(())
        })()
        .is_some()
    };
    unsafe {
        let _ = CloseClipboard();
    }
    ok
}
