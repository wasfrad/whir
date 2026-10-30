//! Talk between whir's two processes: a second `whir.exe` handing file paths
//! (or a Settings page) to the running window, and the window and the capture
//! dock telling each other that data they share has changed on disk.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, FindWindowW, GetWindowThreadProcessId, PostMessageW,
    RegisterWindowMessageW, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_COPYDATA,
};
use windows::core::{HSTRING, PCWSTR, w};

/// Window class of the main window; also how a second instance finds us.
pub const WINDOW_CLASS: &str = "whir.main.v1";

/// Window class of the capture dock (`whir.exe --dock`).
pub const DOCK_CLASS: &str = "whir.dock.v1";

/// `COPYDATASTRUCT::dwData` tag for "open these files".
pub const COPYDATA_OPEN: usize = 0x5748_4952; // "WHIR"

/// `COPYDATASTRUCT::dwData` tag for "show this Settings page" (UTF-16 name).
pub const COPYDATA_SHOW: usize = 0x5748_5253; // "WHRS"

pub fn find_main_window() -> Option<HWND> {
    find(WINDOW_CLASS)
}

pub fn find_dock_window() -> Option<HWND> {
    find(DOCK_CLASS)
}

fn find(class: &str) -> Option<HWND> {
    let class = HSTRING::from(class);
    unsafe { FindWindowW(&class, PCWSTR::null()) }
        .ok()
        .filter(|h| !h.is_invalid())
}

/// Data the window and the dock both read and write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Changed {
    /// Timers and time entries.
    Time,
    /// Which notes are flagged as todos.
    Todos,
}

impl Changed {
    fn code(self) -> usize {
        match self {
            Changed::Time => 1,
            Changed::Todos => 2,
        }
    }

    /// From a [`changed_message`]'s `wParam`.
    pub fn from_code(code: usize) -> Option<Changed> {
        [Changed::Time, Changed::Todos]
            .into_iter()
            .find(|c| c.code() == code)
    }
}

/// The message [`notify_changed`] posts; `wParam` says what changed. Registered,
/// so both processes agree on its number.
pub fn changed_message() -> u32 {
    static MESSAGE: AtomicU32 = AtomicU32::new(0);
    match MESSAGE.load(Ordering::Relaxed) {
        0 => {
            let m = unsafe { RegisterWindowMessageW(w!("whir.changed.v1")) };
            MESSAGE.store(m, Ordering::Relaxed);
            m
        }
        m => m,
    }
}

/// Tell the other process (whichever of the window and the dock is running)
/// that `what` was just written, so it reads it again.
pub fn notify_changed(what: Changed) {
    let me = unsafe { GetCurrentProcessId() };
    let message = changed_message();
    for hwnd in [find_main_window(), find_dock_window()]
        .into_iter()
        .flatten()
    {
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        if pid != me {
            let _ = unsafe { PostMessageW(Some(hwnd), message, WPARAM(what.code()), LPARAM(0)) };
        }
    }
}

/// NUL-separated UTF-16 paths.
pub fn encode_paths(paths: &[PathBuf]) -> Vec<u16> {
    let mut out = Vec::new();
    for p in paths {
        out.extend(p.as_os_str().to_string_lossy().encode_utf16());
        out.push(0);
    }
    out
}

pub fn decode_paths(units: &[u16]) -> Vec<PathBuf> {
    units
        .split(|&u| u == 0)
        .filter(|s| !s.is_empty())
        .map(|s| PathBuf::from(String::from_utf16_lossy(s)))
        .collect()
}

/// Send `paths` to the running instance. Lets it take the foreground (we were
/// launched by the user, so we hold foreground rights we can pass on).
pub fn send_paths(hwnd: HWND, paths: &[PathBuf]) -> bool {
    send(hwnd, COPYDATA_OPEN, &encode_paths(paths))
}

/// Ask the running instance to show a Settings page (`"timesheet"`, …).
pub fn send_show(hwnd: HWND, page: &str) -> bool {
    send(
        hwnd,
        COPYDATA_SHOW,
        &page.encode_utf16().collect::<Vec<_>>(),
    )
}

fn send(hwnd: HWND, tag: usize, payload: &[u16]) -> bool {
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid != 0 {
            let _ = AllowSetForegroundWindow(pid);
        }
        let cds = COPYDATASTRUCT {
            dwData: tag,
            cbData: (payload.len() * 2) as u32,
            lpData: payload.as_ptr() as *mut _,
        };
        let mut result = 0usize;
        let r = SendMessageTimeoutW(
            hwnd,
            WM_COPYDATA,
            WPARAM(0),
            LPARAM(&cds as *const _ as isize),
            SMTO_ABORTIFHUNG,
            3000,
            Some(&mut result),
        );
        r.0 != 0 && result != 0
    }
}

/// Absolute, normalized paths so the receiving process (different working
/// directory) can open them and dedupe tabs.
pub fn canonicalize(path: &Path) -> PathBuf {
    match std::path::absolute(path) {
        Ok(p) => strip_verbatim(&p),
        Err(_) => path.to_owned(),
    }
}

fn strip_verbatim(p: &Path) -> PathBuf {
    let s = p.as_os_str().to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        p.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_paths() {
        let paths = vec![
            PathBuf::from(r"C:\a b\c.png"),
            PathBuf::from(r"D:\ünï\x.csv"),
        ];
        assert_eq!(decode_paths(&encode_paths(&paths)), paths);
    }

    #[test]
    fn changes_round_trip() {
        for c in [Changed::Time, Changed::Todos] {
            assert_eq!(Changed::from_code(c.code()), Some(c));
        }
        assert_eq!(Changed::from_code(0), None);
    }
}
