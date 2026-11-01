//! Explorer integration: "Show in Explorer", opening links, and starting with
//! Windows.

use std::path::Path;

use windows::Win32::UI::Shell::{
    ILCreateFromPathW, ILFree, SHOpenFolderAndSelectItems, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, PCWSTR, w};

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

/// Opens a folder in Explorer (creating it first).
pub fn open_folder(dir: &Path) {
    let _ = std::fs::create_dir_all(dir);
    unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &HSTRING::from(dir.as_os_str()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const RUN_VALUE: PCWSTR = w!("whir");

/// The command that starts whir with Windows: this whir.exe, starting the
/// dock quietly (`"C:\…\whir.exe" --dock-start`).
pub fn autostart_command() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    Some(format!("\"{}\" --dock-start", exe.display()))
}

/// What the Run entry says now, if there is one.
pub fn autostart() -> Option<String> {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_SZ, RegGetValueW};
    let mut buf = [0u16; 1024];
    let mut bytes = (buf.len() * 2) as u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            RUN_VALUE,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut bytes),
        )
    };
    if r.is_err() {
        return None;
    }
    let n = (bytes as usize / 2).saturating_sub(1).min(buf.len());
    Some(String::from_utf16_lossy(&buf[..n]))
}

/// Start whir (its dock) when the user signs in, or stop doing so. Per user,
/// no admin rights.
pub fn set_autostart(on: bool) -> Result<(), String> {
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows::Win32::System::Registry::{
        HKEY_CURRENT_USER, REG_SZ, RegDeleteKeyValueW, RegSetKeyValueW,
    };
    unsafe {
        if !on {
            let r = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE);
            // Not there: already off.
            return if r == ERROR_SUCCESS || r == ERROR_FILE_NOT_FOUND {
                Ok(())
            } else {
                Err(format!("Couldn't change the startup setting ({r:?})."))
            };
        }
        let command = autostart_command().ok_or("whir couldn't find its own program file.")?;
        let data: Vec<u16> = command.encode_utf16().chain(Some(0)).collect();
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            RUN_VALUE,
            REG_SZ.0,
            Some(data.as_ptr() as *const _),
            (data.len() * 2) as u32,
        )
        .ok()
        .map_err(|e| format!("Couldn't change the startup setting: {}", e.message()))
    }
}
