//! Handing a capture to whir: the same road a double-clicked file takes.

use std::path::Path;

use whir_platform::ipc;
use windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow;

/// Open `path` in the running whir window, or start whir with it. Either way
/// whir may come to the front: the capture was just made by a click here.
pub fn open_in_whir(path: &Path) -> Result<(), String> {
    if let Some(hwnd) = ipc::find_main_window()
        && ipc::send_paths(hwnd, &[path.to_owned()])
    {
        return Ok(());
    }
    // No window (or it is busy closing): a new whir sorts that out itself,
    // forwarding to the old one or taking over from it.
    let started = std::env::current_exe()
        .and_then(|exe| std::process::Command::new(exe).arg(path).spawn())
        .map_err(|e| format!("The capture was saved, but whir couldn't be started: {e}"))?;
    unsafe {
        let _ = AllowSetForegroundWindow(started.id());
    }
    Ok(())
}
