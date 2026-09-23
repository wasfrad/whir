//! Win32 platform layer: the main window, input, clipboard, dialogs, fonts,
//! and single-instance IPC.

pub mod clipboard;
pub mod dialogs;
pub mod fonts;
pub mod input;
pub mod ipc;
pub mod os;
mod window;

pub use window::{
    FrameInfo, FrameOutput, Handler, HitMap, RectPx, WakeSlot, Window, WindowCommand, WindowOptions,
};
