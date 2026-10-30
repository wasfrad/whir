//! Win32 platform layer: the main window, input, clipboard, dialogs, fonts,
//! and single-instance IPC.

pub mod clipboard;
pub mod dialogs;
pub mod fonts;
pub mod input;
pub mod ipc;
pub mod os;
pub mod placement;
pub mod shell;
mod window;

pub use window::{
    FrameInfo, FrameOutput, Handler, HitMap, RectPx, SaveDialogSpec, WakeSlot, Window,
    WindowCommand, WindowOptions,
};
