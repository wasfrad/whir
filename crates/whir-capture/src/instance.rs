//! One dock per session. A second `whir.exe --dock…` hands its command to the
//! running dock and exits. (Its own mutex and window class: the dock never
//! touches the main window's single-instance hand-off.)

use std::time::{Duration, Instant};

use whir_platform::ipc;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, LPARAM, WAIT_ABANDONED, WAIT_OBJECT_0,
    WPARAM,
};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, GetWindowThreadProcessId, PostMessageW, WM_APP,
};
use windows::core::{PCWSTR, w};

use crate::Command;

/// [`ipc::DOCK_CLASS`], as `RegisterClassExW` wants it.
pub const CLASS: PCWSTR = w!("whir.dock.v1");
/// Posted to the dock by another launch; `wParam` is [`code`] of its command.
pub const WM_LAUNCHED: u32 = WM_APP + 1;
/// Posted to the dock by Settings; `wParam` and `lParam` are an
/// [`crate::config::Ask`].
pub const WM_ASKED: u32 = WM_APP + 5;

pub fn code(cmd: Command) -> usize {
    match cmd {
        Command::Reveal => 1,
        Command::Capture => 2,
        Command::Quit => 3,
        Command::Start => 4,
        Command::Hide => 5,
    }
}

pub fn from_code(code: usize) -> Option<Command> {
    Command::ALL.into_iter().find(|&c| self::code(c) == code)
}

pub enum Role {
    /// This process is the dock; keep the guard until it closes.
    Primary(Guard),
    /// A running dock has the command.
    Forwarded,
}

pub struct Guard(HANDLE);

impl Drop for Guard {
    fn drop(&mut self) {
        unsafe {
            let _ = ReleaseMutex(self.0);
            let _ = CloseHandle(self.0);
        }
    }
}

pub fn acquire(cmd: Command) -> Role {
    let mutex = match unsafe { CreateMutexW(None, true, w!("Local\\whir.dock.v1")) } {
        Ok(h) => h,
        // Can't coordinate: just run.
        Err(_) => return Role::Primary(Guard(HANDLE::default())),
    };
    if unsafe { GetLastError() } != ERROR_ALREADY_EXISTS {
        return Role::Primary(Guard(mutex));
    }
    // A dock is running, or just starting, or just closing.
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if let Some(hwnd) = ipc::find_dock_window() {
            unsafe {
                // We were started by the user: let the dock act for us.
                let mut process = 0u32;
                GetWindowThreadProcessId(hwnd, Some(&mut process));
                let _ = AllowSetForegroundWindow(process);
                if PostMessageW(Some(hwnd), WM_LAUNCHED, WPARAM(code(cmd)), LPARAM(0)).is_ok() {
                    let _ = CloseHandle(mutex);
                    return Role::Forwarded;
                }
            }
        }
        let r = unsafe { WaitForSingleObject(mutex, 50) };
        if r == WAIT_OBJECT_0 || r == WAIT_ABANDONED {
            return Role::Primary(Guard(mutex));
        }
    }
    // The other one is stuck: better a second dock than none.
    Role::Primary(Guard(mutex))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_class_is_the_one_others_look_for() {
        assert_eq!(unsafe { CLASS.to_string() }.unwrap(), ipc::DOCK_CLASS);
    }

    #[test]
    fn codes_round_trip() {
        for c in Command::ALL {
            assert_eq!(from_code(code(c)), Some(c));
        }
        assert_eq!(from_code(0), None);
    }
}
