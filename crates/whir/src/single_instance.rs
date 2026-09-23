//! One whir window per user session. A second `whir.exe` (another double-click)
//! hands its paths to the running instance and exits within milliseconds.
//!
//! Explorer launches one process per file for a multi-select (up to 15 at
//! once), so this must be race-free: every process but one forwards, and if the
//! primary is shutting down, a waiting process takes over as the new primary.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use whir_platform::ipc;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0,
};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};
use windows::core::w;

pub enum Instance {
    /// We are the primary instance; keep this alive until the window is destroyed.
    Primary(PrimaryGuard),
    /// Paths were handed to the running instance.
    Forwarded,
}

pub struct PrimaryGuard(HANDLE);

impl Drop for PrimaryGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = ReleaseMutex(self.0);
            let _ = CloseHandle(self.0);
        }
    }
}

pub fn acquire(paths: &[PathBuf]) -> Instance {
    let mutex = match unsafe { CreateMutexW(None, true, w!("Local\\whir.instance.v1")) } {
        Ok(h) => h,
        // Can't coordinate: just run standalone.
        Err(_) => return Instance::Primary(PrimaryGuard(HANDLE::default())),
    };
    if unsafe { GetLastError() } != ERROR_ALREADY_EXISTS {
        return Instance::Primary(PrimaryGuard(mutex));
    }

    // Someone else is (or is becoming) the primary.
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Some(hwnd) = ipc::find_main_window()
            && ipc::send_paths(hwnd, paths)
        {
            unsafe {
                let _ = CloseHandle(mutex);
            }
            return Instance::Forwarded;
        }
        // Owning the mutex means the previous primary exited (or crashed).
        let r = unsafe { WaitForSingleObject(mutex, 50) };
        if r == WAIT_OBJECT_0 || r == WAIT_ABANDONED {
            return Instance::Primary(PrimaryGuard(mutex));
        }
    }
    // The primary is hung: better a second window than losing the user's file.
    Instance::Primary(PrimaryGuard(mutex))
}
