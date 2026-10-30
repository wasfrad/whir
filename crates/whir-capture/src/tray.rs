//! The icon in the notification area. The dock runs all day; the icon says
//! so (its tooltip shows the running timer), and its menu is the one place to
//! quit whir for good.

use windows::Win32::Foundation::HWND;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_SETVERSION,
    NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyIcon, GetSystemMetrics, HICON, IMAGE_ICON, LR_DEFAULTCOLOR, LoadImageW,
    RegisterWindowMessageW, SM_CXSMICON, SM_CYSMICON, WM_APP,
};
use windows::core::{PCWSTR, w};

/// What the icon sends the dock's window. With version 4 the event is in
/// `lParam`'s low word, and the pointer's position in `wParam`.
pub const WM_TRAY: u32 = WM_APP + 4;
/// A left click on the icon.
pub use windows::Win32::UI::Shell::NIN_SELECT;
/// Enter or Space on the icon, when it has the keyboard focus.
pub const NIN_KEYSELECT: u32 = NIN_SELECT | windows::Win32::UI::Shell::NINF_KEY;

const ID: u32 = 1;

pub struct Tray {
    hwnd: HWND,
    icon: HICON,
    tip: String,
    /// Broadcast when Explorer (re)starts: the icon must be added again.
    taskbar_created: u32,
}

impl Tray {
    pub fn new(hwnd: HWND) -> Tray {
        let icon = unsafe {
            let instance = GetModuleHandleW(PCWSTR::null()).ok();
            // whir.exe's own icon (resource 1, see whir.rc), at tray size.
            LoadImageW(
                instance.map(Into::into),
                PCWSTR(1 as _),
                IMAGE_ICON,
                GetSystemMetrics(SM_CXSMICON),
                GetSystemMetrics(SM_CYSMICON),
                LR_DEFAULTCOLOR,
            )
            .map(|h| HICON(h.0))
            .unwrap_or_default()
        };
        let tray = Tray {
            hwnd,
            icon,
            tip: "whir".to_owned(),
            taskbar_created: unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) },
        };
        tray.add();
        tray
    }

    pub fn is_taskbar_created(&self, msg: u32) -> bool {
        msg != 0 && msg == self.taskbar_created
    }

    fn data(&self) -> NOTIFYICONDATAW {
        let mut data = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: ID,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP,
            uCallbackMessage: WM_TRAY,
            hIcon: self.icon,
            ..Default::default()
        };
        let tip: Vec<u16> = self.tip.encode_utf16().collect();
        let n = tip.len().min(data.szTip.len() - 1);
        data.szTip[..n].copy_from_slice(&tip[..n]);
        data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        data
    }

    /// Put the icon in the notification area (again, after Explorer restarts).
    pub fn add(&self) {
        let data = self.data();
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &data);
            if Shell_NotifyIconW(NIM_ADD, &data).as_bool() {
                let _ = Shell_NotifyIconW(NIM_SETVERSION, &data);
            }
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &self.data());
            if !self.icon.is_invalid() {
                let _ = DestroyIcon(self.icon);
            }
        }
    }
}
