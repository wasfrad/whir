//! Plain-text clipboard (CF_UNICODETEXT).

use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::CF_UNICODETEXT;

/// OpenClipboard fails while another process holds it; retry briefly.
fn open() -> bool {
    for _ in 0..10 {
        if unsafe { OpenClipboard(Some(HWND::default())) }.is_ok() {
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
            let mut len = 0;
            while *ptr.add(len) != 0 {
                len += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len));
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
    if !open() {
        return false;
    }
    let ok = unsafe {
        (|| {
            EmptyClipboard().ok()?;
            let bytes = wide.len() * 2;
            let hg = GlobalAlloc(GMEM_MOVEABLE, bytes).ok()?;
            let ptr = GlobalLock(hg) as *mut u16;
            if ptr.is_null() {
                let _ = GlobalFree(Some(hg));
                return None;
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
            let _ = GlobalUnlock(hg);
            if SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(hg.0))).is_err() {
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
