//! Where the main window opens: where it was left (size, place, maximized),
//! kept in `settings.ini`. By default only if the mouse is on that monitor;
//! on another one it opens there instead, centred, at the same size. Or,
//! if asked, always exactly where it was. Never off-screen after monitors
//! come and go.

use std::fmt;
use std::str::FromStr;

use whir_core::settings;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, TRUE};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MonitorFromPoint, MonitorFromWindow,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, GetWindowRect, IsIconic, IsZoomed};
use windows::core::BOOL;

const PLACEMENT: &str = "window.placement";
const PLACE: &str = "window.place";

/// A window's place: its normal (not maximized) rect in screen pixels, the
/// DPI of the monitor it was on, and whether it was maximized.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    /// Left, top, right, bottom.
    pub rect: [i32; 4],
    pub dpi: u32,
    pub maximized: bool,
}

impl fmt::Display for Placement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [l, t, r, b] = self.rect;
        let state = if self.maximized {
            "maximized"
        } else {
            "normal"
        };
        write!(f, "{l} {t} {r} {b} {} {state}", self.dpi)
    }
}

impl FromStr for Placement {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, ()> {
        let parts: Vec<&str> = s.split_whitespace().collect();
        let [l, t, r, b, dpi, state] = parts[..] else {
            return Err(());
        };
        let n = |v: &str| v.parse::<i32>().map_err(|_| ());
        let rect = [n(l)?, n(t)?, n(r)?, n(b)?];
        let dpi: u32 = dpi.parse().map_err(|_| ())?;
        let maximized = match state {
            "maximized" => true,
            "normal" => false,
            _ => return Err(()),
        };
        if rect[2] <= rect[0] || rect[3] <= rect[1] || !(48..=960).contains(&dpi) {
            return Err(());
        }
        Ok(Self {
            rect,
            dpi,
            maximized,
        })
    }
}

/// Where whir was left, if anywhere.
pub fn saved() -> Option<Placement> {
    settings::get_str(PLACEMENT)?.parse().ok()
}

pub fn save(p: &Placement) {
    settings::set_str(PLACEMENT, &p.to_string());
}

/// Always exactly where it was left, wherever the mouse is.
pub fn exact() -> bool {
    settings::get_str(PLACE).as_deref().map(str::trim) == Some("exact")
}

pub fn set_exact(on: bool) {
    settings::set_str(PLACE, if on { "exact" } else { "mouse" });
}

/// A monitor: all of it and its work area (screen pixels), and its DPI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Monitor {
    pub area: [i32; 4],
    pub work: [i32; 4],
    pub dpi: u32,
}

fn overlap(a: [i32; 4], b: [i32; 4]) -> i64 {
    let w = (a[2].min(b[2]) - a[0].max(b[0])).max(0) as i64;
    let h = (a[3].min(b[3]) - a[1].max(b[1])).max(0) as i64;
    w * h
}

/// `rect` moved (and if need be shrunk) to lie wholly inside `area`.
fn pull_inside(rect: [i32; 4], area: [i32; 4]) -> [i32; 4] {
    let w = (rect[2] - rect[0]).min(area[2] - area[0]);
    let h = (rect[3] - rect[1]).min(area[3] - area[1]);
    let left = rect[0].clamp(area[0], area[2] - w);
    let top = rect[1].clamp(area[1], area[3] - h);
    [left, top, left + w, top + h]
}

/// A `w` × `h` rect in the middle of `area` (shrunk to it if need be).
fn centre_in(w: i32, h: i32, area: [i32; 4]) -> [i32; 4] {
    let w = w.min(area[2] - area[0]);
    let h = h.min(area[3] - area[1]);
    let left = area[0] + (area[2] - area[0] - w) / 2;
    let top = area[1] + (area[3] - area[1] - h) / 2;
    [left, top, left + w, top + h]
}

/// Where to put a window that was left at `saved` (its normal rect; it is
/// maximized again if it was), with these `monitors` and the mouse on
/// `monitors[cursor]`.
pub fn choose(saved: &Placement, exact: bool, monitors: &[Monitor], cursor: usize) -> [i32; 4] {
    let target = &monitors[cursor.min(monitors.len() - 1)];
    // The monitor it was on: the one holding most of it, if it's still there.
    let home = monitors
        .iter()
        .map(|m| (overlap(saved.rect, m.area), m))
        .filter(|(o, _)| *o > 0)
        .max_by_key(|(o, _)| *o)
        .map(|(_, m)| m);
    match home {
        Some(m) if exact || m == target => pull_inside(saved.rect, m.work),
        _ => {
            // On another monitor: the same size there (in its DPI), centred.
            let scale = |v: i32| (v as i64 * target.dpi as i64 / saved.dpi.max(1) as i64) as i32;
            let w = scale(saved.rect[2] - saved.rect[0]);
            let h = scale(saved.rect[3] - saved.rect[1]);
            centre_in(w, h, target.work)
        }
    }
}

fn rect_of(r: RECT) -> [i32; 4] {
    [r.left, r.top, r.right, r.bottom]
}

fn monitor_info(m: HMONITOR) -> Option<Monitor> {
    let mut mi = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe {
        if !GetMonitorInfoW(m, &mut mi).as_bool() {
            return None;
        }
        let (mut dpi, mut dpi_y) = (96u32, 96u32);
        let _ = GetDpiForMonitor(m, MDT_EFFECTIVE_DPI, &mut dpi, &mut dpi_y);
        Some(Monitor {
            area: rect_of(mi.rcMonitor),
            work: rect_of(mi.rcWork),
            dpi,
        })
    }
}

/// Every monitor, and which one the mouse is on.
pub(crate) fn monitors() -> Option<(Vec<Monitor>, usize)> {
    unsafe extern "system" fn add(m: HMONITOR, _: HDC, _: *mut RECT, list: LPARAM) -> BOOL {
        let list = unsafe { &mut *(list.0 as *mut Vec<Monitor>) };
        if let Some(info) = monitor_info(m) {
            list.push(info);
        }
        TRUE
    }
    let mut list: Vec<Monitor> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(add),
            LPARAM(&mut list as *mut Vec<Monitor> as isize),
        );
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let under = monitor_info(MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST))?;
        let cursor = list.iter().position(|m| m.area == under.area)?;
        Some((list, cursor))
    }
}

/// The window's normal rect right now, if it is neither minimized nor
/// maximized (a snapped window's is its snapped rect).
pub(crate) fn normal_rect(hwnd: HWND) -> Option<[i32; 4]> {
    unsafe {
        if IsIconic(hwnd).as_bool() || IsZoomed(hwnd).as_bool() {
            return None;
        }
        let mut r = RECT::default();
        GetWindowRect(hwnd, &mut r).ok()?;
        Some(rect_of(r))
    }
}

/// Where the window is, given its last normal rect: `None` while minimized.
/// Maximized on another monitor than that rect, the rect moves along.
pub(crate) fn capture(hwnd: HWND, normal: [i32; 4]) -> Option<Placement> {
    unsafe {
        if IsIconic(hwnd).as_bool() {
            return None;
        }
        let maximized = IsZoomed(hwnd).as_bool();
        let dpi = GetDpiForWindow(hwnd).max(96);
        let mut rect = normal_rect(hwnd).unwrap_or(normal);
        if maximized
            && let Some(m) = monitor_info(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST))
            && overlap(rect, m.area) == 0
        {
            rect = centre_in(rect[2] - rect[0], rect[3] - rect[1], m.work);
        }
        Some(Placement {
            rect,
            dpi,
            maximized,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1920 × 1080 monitor at 100%, and a 2560 × 1440 one at 150% to its right.
    fn two() -> Vec<Monitor> {
        vec![
            Monitor {
                area: [0, 0, 1920, 1080],
                work: [0, 0, 1920, 1032],
                dpi: 96,
            },
            Monitor {
                area: [1920, 0, 4480, 1440],
                work: [1920, 0, 4480, 1392],
                dpi: 144,
            },
        ]
    }

    fn at(rect: [i32; 4], dpi: u32) -> Placement {
        Placement {
            rect,
            dpi,
            maximized: false,
        }
    }

    #[test]
    fn placements_round_trip() {
        let p = Placement {
            rect: [-1700, 20, -100, 900],
            dpi: 120,
            maximized: true,
        };
        assert_eq!(p.to_string(), "-1700 20 -100 900 120 maximized");
        assert_eq!(p.to_string().parse(), Ok(p));
        let n = at([10, 20, 810, 620], 96);
        assert_eq!(n.to_string().parse(), Ok(n));
        for bad in [
            "",
            "1 2 3 4 96",
            "1 2 3 4 96 tiny",
            "10 10 5 20 96 normal",
            "1 2 3 4 0 normal",
            "a 2 3 4 96 normal",
        ] {
            assert_eq!(bad.parse::<Placement>(), Err(()), "{bad:?}");
        }
    }

    #[test]
    fn back_where_it_was_on_the_mouses_monitor() {
        let m = two();
        let p = at([100, 50, 1300, 950], 96);
        assert_eq!(choose(&p, false, &m, 0), p.rect);
        assert_eq!(choose(&p, true, &m, 0), p.rect);
        // Exactly where it was, even with the mouse elsewhere, if asked.
        assert_eq!(choose(&p, true, &m, 1), p.rect);
    }

    #[test]
    fn on_another_monitor_it_opens_there_centred_at_the_same_size() {
        let m = two();
        let p = at([100, 50, 1300, 950], 96);
        // 1200 × 900 at 96 DPI is 1800 × 1350 at 144, centred in the work area
        // (1350 fits in 1392).
        let r = choose(&p, false, &m, 1);
        assert_eq!([r[2] - r[0], r[3] - r[1]], [1800, 1350]);
        assert_eq!(r, [1920 + 380, 21, 1920 + 380 + 1800, 21 + 1350]);
        // Back the other way: 2400 × 1392 at 144 DPI is 1600 × 928 at 96.
        let big = at([2000, 0, 4400, 1392], 144);
        assert_eq!(choose(&big, false, &m, 0), [160, 52, 1760, 980]);
    }

    #[test]
    fn never_off_screen() {
        let m = two();
        // Its monitor is gone: on the mouse's monitor, even when exact.
        let gone = at([-1800, 100, -200, 900], 96);
        for exact in [false, true] {
            assert_eq!(choose(&gone, exact, &m, 0), [160, 116, 1760, 916]);
        }
        // Hanging off the edge and under the taskbar: pulled in whole.
        assert_eq!(
            choose(&at([1500, 600, 2000, 1200], 96), false, &m, 0),
            [1420, 432, 1920, 1032]
        );
        // Larger than the work area: shrunk to it.
        assert_eq!(
            choose(&at([-50, -50, 2100, 1200], 96), true, &m, 0),
            [0, 0, 1920, 1032]
        );
    }
}
