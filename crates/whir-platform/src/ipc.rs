//! Hand-off of file paths from a second `whir.exe` process to the running one.

use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, FindWindowW, GetWindowThreadProcessId, SMTO_ABORTIFHUNG,
    SendMessageTimeoutW, WM_COPYDATA,
};
use windows::core::{HSTRING, PCWSTR};

/// Window class of the main window; also how a second instance finds us.
pub const WINDOW_CLASS: &str = "whir.main.v1";

/// `COPYDATASTRUCT::dwData` tag for "open these files".
pub const COPYDATA_OPEN: usize = 0x5748_4952; // "WHIR"

pub fn find_main_window() -> Option<HWND> {
    let class = HSTRING::from(WINDOW_CLASS);
    unsafe { FindWindowW(&class, PCWSTR::null()) }
        .ok()
        .filter(|h| !h.is_invalid())
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
    let payload = encode_paths(paths);
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid != 0 {
            let _ = AllowSetForegroundWindow(pid);
        }
        let cds = COPYDATASTRUCT {
            dwData: COPYDATA_OPEN,
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
}
