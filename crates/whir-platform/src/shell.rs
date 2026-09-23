//! Explorer integration: Recycle Bin, "Show in Explorer", opening links.

use std::path::Path;

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Shell::{
    FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_SILENT, FOF_WANTNUKEWARNING,
    ILCreateFromPathW, ILFree, SHFILEOPSTRUCTW, SHFileOperationW, SHOpenFolderAndSelectItems,
    ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, PCWSTR, w};

/// Move `path` to the Recycle Bin. If the bin can't be used (e.g. a
/// network-redirected Documents folder) Windows asks before deleting for good.
pub fn recycle(owner: HWND, path: &Path) -> Result<(), String> {
    // Double-NUL terminated list of one path.
    let from: Vec<u16> = path
        .as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .chain([0, 0])
        .collect();
    let mut op = SHFILEOPSTRUCTW {
        hwnd: owner,
        wFunc: FO_DELETE,
        pFrom: PCWSTR(from.as_ptr()),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_WANTNUKEWARNING | FOF_SILENT).0 as u16,
        ..Default::default()
    };
    let r = unsafe { SHFileOperationW(&mut op) };
    if r != 0 {
        return Err(format!("Couldn't delete the file (error {r})."));
    }
    if op.fAnyOperationsAborted.as_bool() {
        return Err("Delete was cancelled.".into());
    }
    Ok(())
}

/// Open Explorer on the folder containing `path`, with it selected.
pub fn reveal(path: &Path) {
    unsafe {
        let pidl = ILCreateFromPathW(&HSTRING::from(path.as_os_str()));
        if pidl.is_null() {
            // Fall back to opening the folder itself.
            if let Some(dir) = path.parent() {
                ShellExecuteW(
                    None,
                    w!("open"),
                    &HSTRING::from(dir.as_os_str()),
                    PCWSTR::null(),
                    PCWSTR::null(),
                    SW_SHOWNORMAL,
                );
            }
            return;
        }
        let _ = SHOpenFolderAndSelectItems(pidl, None, 0);
        ILFree(Some(pidl));
    }
}

/// Open a web or mail link in the default handler. Other schemes are ignored,
/// so a note can't launch programs.
pub fn open_url(url: &str) {
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("mailto:"))
    {
        return;
    }
    unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &HSTRING::from(url),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}
