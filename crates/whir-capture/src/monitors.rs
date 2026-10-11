//! The monitors that make up the desktop.

use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::core::BOOL;

use crate::geom::Rect;

/// `MONITORINFO::dwFlags`: this is the primary monitor.
const PRIMARY: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Monitor {
    /// The whole monitor, in screen coordinates.
    pub rect: Rect,
    /// Without the taskbar.
    pub work: Rect,
    pub dpi: u32,
    /// Its device name (`\\.\DISPLAY1`): how a remembered monitor is found again.
    pub device: String,
    pub primary: bool,
}

pub(crate) fn rect(r: RECT) -> Rect {
    Rect::new(r.left, r.top, r.right, r.bottom)
}

/// Every monitor, the primary one first.
pub fn list() -> Vec<Monitor> {
    unsafe extern "system" fn each(
        monitor: HMONITOR,
        _dc: HDC,
        _clip: *mut RECT,
        out: LPARAM,
    ) -> BOOL {
        let out = unsafe { &mut *(out.0 as *mut Vec<Monitor>) };
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        let known =
            unsafe { GetMonitorInfoW(monitor, &mut info as *mut _ as *mut MONITORINFO) }.as_bool();
        if known {
            let (mut dpi, mut dpi_y) = (96u32, 96u32);
            let _ = unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut dpi_y) };
            let name = &info.szDevice;
            let len = name.iter().position(|&c| c == 0).unwrap_or(name.len());
            out.push(Monitor {
                rect: rect(info.monitorInfo.rcMonitor),
                work: rect(info.monitorInfo.rcWork),
                dpi,
                device: String::from_utf16_lossy(&name[..len]),
                primary: info.monitorInfo.dwFlags & PRIMARY != 0,
            });
        }
        true.into()
    }

    let mut out: Vec<Monitor> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(each),
            LPARAM(&mut out as *mut Vec<Monitor> as isize),
        );
    }
    out.sort_by_key(|m| !m.primary);
    out
}
