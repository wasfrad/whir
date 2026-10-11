//! The windows a click can capture: their rectangles on the frozen screen,
//! collected before the overlay covers everything.

use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{
    DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GWL_EXSTYLE, GetClassNameW, GetShellWindow, GetWindowLongW, GetWindowRect,
    GetWindowThreadProcessId, IsIconic, IsWindowVisible, IsZoomed, WS_EX_TRANSPARENT,
};
use windows::core::BOOL;

use crate::geom::{Point, Rect};
use crate::monitors::rect;

/// Smaller than this (a side, in pixels) isn't worth offering.
const MIN_SIDE: i32 = 8;

struct Collector {
    origin: Point,
    bounds: Rect,
    own_process: u32,
    out: Vec<Rect>,
}

/// Top-level window rectangles, topmost first, in frame coordinates and
/// clipped to the frame. Top-level only: menus, tooltips and the taskbar are
/// windows of their own, so they can be picked; panes inside a window can't.
pub fn collect(origin: Point, bounds: Rect) -> Vec<Rect> {
    unsafe extern "system" fn each(hwnd: HWND, c: LPARAM) -> BOOL {
        let c = unsafe { &mut *(c.0 as *mut Collector) };
        if let Some(r) = unsafe { c.visible_rect(hwnd) } {
            c.out.push(r);
        }
        true.into()
    }

    let mut c = Collector {
        origin,
        bounds,
        own_process: std::process::id(),
        out: Vec::new(),
    };
    // EnumWindows walks the z-order from the top.
    unsafe {
        let _ = EnumWindows(Some(each), LPARAM(&mut c as *mut Collector as isize));
    }
    c.out
}

impl Collector {
    unsafe fn visible_rect(&self, hwnd: HWND) -> Option<Rect> {
        unsafe {
            if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
                return None;
            }
            // Suspended store apps and windows on other virtual desktops are
            // "visible" but cloaked.
            let mut cloaked = 0u32;
            let _ = DwmGetWindowAttribute(
                hwnd,
                DWMWA_CLOAKED,
                &mut cloaked as *mut u32 as *mut _,
                std::mem::size_of::<u32>() as u32,
            );
            if cloaked != 0 {
                return None;
            }
            let mut process = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut process));
            if process == self.own_process {
                return None;
            }
            // Click-through overlays would swallow every pick.
            if GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TRANSPARENT.0 != 0 {
                return None;
            }
            // The desktop itself: a click there takes the monitor instead.
            if hwnd == GetShellWindow() {
                return None;
            }
            let mut class = [0u16; 32];
            let len = GetClassNameW(hwnd, &mut class) as usize;
            if matches!(
                String::from_utf16_lossy(&class[..len]).as_str(),
                "Progman" | "WorkerW"
            ) {
                return None;
            }

            // Without the invisible resize border and the shadow.
            let mut r = RECT::default();
            let exact = DwmGetWindowAttribute(
                hwnd,
                DWMWA_EXTENDED_FRAME_BOUNDS,
                &mut r as *mut RECT as *mut _,
                std::mem::size_of::<RECT>() as u32,
            );
            if exact.is_err() && GetWindowRect(hwnd, &mut r).is_err() {
                return None;
            }
            let mut r = rect(r);
            // A maximized window hangs a few pixels over its monitor's edges.
            if IsZoomed(hwnd).as_bool() {
                let mut info = MONITORINFO {
                    cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                    ..Default::default()
                };
                let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
                if GetMonitorInfoW(monitor, &mut info).as_bool() {
                    r = r.intersect(rect(info.rcMonitor));
                }
            }
            let r = r
                .translate(-self.origin.0, -self.origin.1)
                .intersect(self.bounds);
            (r.width() >= MIN_SIDE && r.height() >= MIN_SIDE).then_some(r)
        }
    }
}

/// What a click at `at` would capture: the topmost window there, else the
/// monitor (a click on the bare desktop).
pub fn hit(targets: &[Rect], monitors: &[Rect], at: Point) -> Option<Rect> {
    targets
        .iter()
        .chain(monitors)
        .find(|r| r.contains(at))
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_topmost_window_wins_and_bare_desktop_is_the_monitor() {
        let front = Rect::new(100, 100, 300, 300);
        let back = Rect::new(50, 50, 400, 400);
        let targets = [front, back];
        let monitors = [Rect::new(0, 0, 1920, 1080), Rect::new(1920, 0, 3840, 1080)];
        assert_eq!(hit(&targets, &monitors, (150, 150)), Some(front));
        assert_eq!(hit(&targets, &monitors, (60, 60)), Some(back));
        assert_eq!(hit(&targets, &monitors, (1000, 900)), Some(monitors[0]));
        assert_eq!(hit(&targets, &monitors, (2000, 10)), Some(monitors[1]));
        assert_eq!(hit(&targets, &monitors, (-5, 10)), None);
    }
}

#[cfg(test)]
mod desktop {
    use super::*;

    /// Prints what a capture would offer right now (run by hand:
    /// `cargo test -p whir-capture offered -- --ignored --nocapture`).
    #[test]
    #[ignore = "reads the live desktop"]
    fn offered() {
        let monitors = crate::monitors::list();
        for m in &monitors {
            println!(
                "monitor {:?} work {:?} dpi {} {} primary={}",
                m.rect, m.work, m.dpi, m.device, m.primary
            );
        }
        let bounds = monitors
            .iter()
            .fold(Rect::default(), |all, m| all.union(m.rect));
        let origin = (bounds.left, bounds.top);
        let targets = collect(origin, bounds.translate(-origin.0, -origin.1));
        println!("origin {origin:?}, {} windows", targets.len());
        for t in targets.iter().take(12) {
            println!("  {t:?}");
        }
    }
}
