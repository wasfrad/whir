//! whir's screen capture: freeze the desktop, let the user pick a window or a
//! region on it, save that as a PNG in the screenshots folder and open it in
//! whir.
//!
//! This runs as its own process (`whir.exe --dock` and friends), separate
//! from the main window: plain Win32 and GDI, no renderer, so it can sit on
//! the desktop all day for a few megabytes.

mod config;
mod dock;
mod geom;
mod grab;
mod handoff;
mod instance;
mod monitors;
mod overlay;
mod paint;
mod place;
mod store;
mod targets;
mod veil;

use std::path::PathBuf;

use whir_image::{PixelLayout, encode_png};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::UI::WindowsAndMessaging::{
    MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MB_TOPMOST, MessageBoxW,
};
use windows::core::{HSTRING, w};

use crate::grab::Frame;
use crate::overlay::Outcome;

/// What a `whir.exe --dock…` launch asks of the dock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// Show it (starting it if needed).
    Reveal,
    /// Take a capture now (starting it if needed).
    Capture,
    /// Close it.
    Quit,
}

impl Command {
    pub fn from_arg(arg: &str) -> Option<Command> {
        [Command::Reveal, Command::Capture, Command::Quit]
            .into_iter()
            .find(|c| c.flag() == arg)
    }

    pub fn flag(self) -> &'static str {
        match self {
            Command::Reveal => "--dock",
            Command::Capture => "--dock-capture",
            Command::Quit => "--dock-quit",
        }
    }
}

/// Start `whir.exe` with `cmd`'s flag (from the main window's capture button).
pub fn launch(cmd: Command) -> std::io::Result<()> {
    std::process::Command::new(std::env::current_exe()?)
        .arg(cmd.flag())
        .spawn()
        .map(|_| ())
}

/// The whole of a `whir.exe --dock…` process. Returns its exit code.
pub fn run(cmd: Command) -> i32 {
    // Started from a document's folder, we'd keep that folder in use all day.
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let _ = std::env::set_current_dir(dir);
    }
    dock::run(cmd)
}

/// One capture, start to finish. `Ok(None)`: the user cancelled.
pub(crate) fn capture() -> Result<Option<PathBuf>, String> {
    let mut frame = Frame::grab()?;
    // The pixels first (they are the product), then what can be picked on them.
    let targets = targets::collect(frame.origin, frame.bounds());
    let monitors = monitors::list();
    let region = match overlay::select(&mut frame, targets, &monitors) {
        Outcome::Region(r) => r,
        Outcome::Cancelled => return Ok(None),
    };
    let (pixels, width, height) = frame.crop(region);
    drop(frame);

    // WIC wants COM; this thread pumps messages, so an apartment is fine.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    let png = encode_png(
        &pixels,
        width,
        height,
        width as usize * 4,
        PixelLayout::Bgrx,
    )
    .map_err(|e| e.to_string())?;
    drop(pixels);
    let path = store::save_png(&png).map_err(|e| {
        let folder = whir_core::captures::resolve_dir()
            .map(|d| d.display().to_string())
            .unwrap_or_default();
        format!("The capture couldn't be saved in {folder}: {e}")
    })?;
    handoff::open_in_whir(&path)?;
    Ok(Some(path))
}

/// There is no window of ours to say it in: a message box, on top.
pub(crate) fn report(error: &str) {
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(error),
            w!("whir"),
            MB_OK | MB_ICONERROR | MB_TOPMOST | MB_SETFOREGROUND,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_round_trip_and_nothing_else_is_a_command() {
        for c in [Command::Reveal, Command::Capture, Command::Quit] {
            assert_eq!(Command::from_arg(c.flag()), Some(c));
        }
        assert_eq!(Command::from_arg("--dock-"), None);
        assert_eq!(Command::from_arg("dock"), None);
        assert_eq!(Command::from_arg(r"C:\--dock.png"), None);
    }
}
