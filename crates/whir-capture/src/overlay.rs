//! Choosing what to capture, on the frozen screen: one borderless window over
//! every monitor shows the grab dimmed, with the window under the pointer (or
//! the dragged region) clear. A click takes that window; a drag, a region.
//!
//! The dimming, the outline and the size label are all drawn into the frame's
//! own pixels (see [`crate::veil`]), so painting is a single blit.

use std::cell::RefCell;

use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAK, DwmSetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, CreateSolidBrush,
    DEFAULT_CHARSET, DeleteObject, EndPaint, FW_NORMAL, FillRect, GdiFlush, GetTextExtentPoint32W,
    HGDIOBJ, InvalidateRect, OUT_DEFAULT_PRECIS, PAINTSTRUCT, SRCCOPY, SelectObject, SetBkMode,
    SetTextColor, TRANSPARENT, TextOutW, UpdateWindow,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    MOD_NOREPEAT, RegisterHotKey, ReleaseCapture, SetCapture, UnregisterHotKey, VK_ESCAPE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWLP_USERDATA,
    GetCursorPos, GetMessageW, GetSystemMetrics, GetWindowLongPtrW, IDC_CROSS, LoadCursorW, MSG,
    PostQuitMessage, RegisterClassExW, SM_CXDRAG, SW_SHOW, SetForegroundWindow, SetWindowLongPtrW,
    ShowWindow, TranslateMessage, WM_ACTIVATE, WM_CLOSE, WM_DISPLAYCHANGE, WM_ERASEBKGND,
    WM_HOTKEY, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCCREATE, WM_PAINT,
    WM_RBUTTONDOWN, WNDCLASSEXW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{BOOL, PCWSTR, w};

use crate::geom::{Point, Rect};
use crate::grab::Frame;
use crate::monitors::Monitor;
use crate::targets;
use crate::veil::{self, Outline, Saved};

/// A dragged region smaller than this (a side, in pixels) was a slip.
const MIN_REGION: i32 = 4;
/// whir's accent blue, as `0x00RRGGBB`.
const OUTLINE: u32 = 0x004c_8dff;
const ESCAPE_HOTKEY: i32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Cancelled,
    /// Capture this part of the frame.
    Region(Rect),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    Move(Point),
    Down(Point),
    Up(Point),
    Cancel,
}

/// Where a selection stands (all positions in frame pixels).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Select {
    /// Nothing held: the window under the pointer is on offer.
    Hover {
        target: Option<Rect>,
    },
    /// The button is down but hasn't moved far enough to be a drag.
    Pressed {
        anchor: Point,
        target: Option<Rect>,
    },
    Dragging {
        anchor: Point,
        to: Point,
    },
}

impl Select {
    /// What is shown clear of the dimming.
    pub fn clear(&self) -> Option<Rect> {
        match *self {
            Select::Hover { target } | Select::Pressed { target, .. } => target,
            Select::Dragging { anchor, to } => {
                Some(Rect::from_points(anchor, to)).filter(|r| !r.is_empty())
            }
        }
    }

    /// Take one input. `targets` are the windows (topmost first), `monitors`
    /// what a click on bare desktop takes, `bounds` the frame, `threshold`
    /// how far the pointer travels before a press becomes a drag.
    pub fn step(
        &mut self,
        input: Input,
        targets: &[Rect],
        monitors: &[Rect],
        bounds: Rect,
        threshold: i32,
    ) -> Option<Outcome> {
        let hover = |p: Point| Select::Hover {
            target: targets::hit(targets, monitors, p),
        };
        match (*self, input) {
            (_, Input::Cancel) => return Some(Outcome::Cancelled),
            (Select::Hover { .. }, Input::Move(p)) => *self = hover(p),
            (Select::Hover { .. }, Input::Down(p)) => {
                *self = Select::Pressed {
                    anchor: bounds.clamp(p),
                    target: targets::hit(targets, monitors, p),
                }
            }
            (Select::Pressed { anchor, .. }, Input::Move(p)) => {
                if (p.0 - anchor.0).abs() > threshold || (p.1 - anchor.1).abs() > threshold {
                    *self = Select::Dragging {
                        anchor,
                        to: bounds.clamp(p),
                    };
                }
            }
            (Select::Pressed { target, .. }, Input::Up(p)) => match target {
                Some(r) => return Some(Outcome::Region(r)),
                None => *self = hover(p),
            },
            (Select::Dragging { anchor, .. }, Input::Move(p)) => {
                *self = Select::Dragging {
                    anchor,
                    to: bounds.clamp(p),
                }
            }
            (Select::Dragging { anchor, .. }, Input::Up(p)) => {
                let r = Rect::from_points(anchor, bounds.clamp(p));
                if r.width() >= MIN_REGION && r.height() >= MIN_REGION {
                    return Some(Outcome::Region(r));
                }
                *self = hover(p);
            }
            (Select::Hover { .. }, Input::Up(_))
            | (Select::Pressed { .. } | Select::Dragging { .. }, Input::Down(_)) => {}
        }
        None
    }
}

struct State<'a> {
    frame: &'a mut Frame,
    targets: Vec<Rect>,
    /// Monitor rectangles and scales, in frame coordinates.
    monitors: Vec<Rect>,
    scales: Vec<f32>,
    select: Select,
    /// What the frame's pixels currently show clear, outlined and labelled.
    clear: Option<Rect>,
    outline: Option<Outline>,
    label: Option<Saved>,
    outcome: Option<Outcome>,
    /// It has been the active window (so losing that means the user left).
    was_active: bool,
    threshold: i32,
}

impl State<'_> {
    fn input(&mut self, hwnd: HWND, input: Input) {
        if self.outcome.is_some() {
            return;
        }
        let before = self.select;
        let bounds = self.frame.bounds();
        self.outcome =
            self.select
                .step(input, &self.targets, &self.monitors, bounds, self.threshold);
        if self.outcome.is_none() && self.select != before {
            let dirty = self.redraw();
            if !dirty.is_empty() {
                let r = RECT {
                    left: dirty.left,
                    top: dirty.top,
                    right: dirty.right,
                    bottom: dirty.bottom,
                };
                unsafe {
                    let _ = InvalidateRect(Some(hwnd), Some(&r), false);
                }
            }
        }
    }

    /// The display scale where `at` is.
    fn scale_at(&self, at: Point) -> f32 {
        self.monitors
            .iter()
            .position(|m| m.contains(at))
            .map_or(1.0, |i| self.scales[i])
    }

    /// Take the outline and label back off the frame's pixels.
    fn undecorate(&mut self) -> Rect {
        let stride = self.frame.width as usize;
        let mut dirty = Rect::default();
        if let Some(label) = self.label.take() {
            dirty = label.rect();
            label.restore(self.frame.pixels(), stride);
        }
        if let Some(outline) = self.outline.take() {
            outline.restore(self.frame.pixels(), stride);
        }
        dirty
    }

    /// Bring the frame's pixels in line with the selection. Returns what
    /// needs repainting.
    fn redraw(&mut self) -> Rect {
        let bounds = self.frame.bounds();
        let stride = self.frame.width as usize;
        let mut dirty = self.undecorate();
        let clear = self
            .select
            .clear()
            .map(|r| r.intersect(bounds))
            .filter(|r| !r.is_empty());
        veil::move_clear(self.frame.pixels(), stride, self.clear, clear);
        dirty = dirty
            .union(self.clear.unwrap_or_default())
            .union(clear.unwrap_or_default());
        self.clear = clear;
        if let Some(r) = clear {
            let thick = (2.0 * self.scale_at((r.left, r.top))).round().max(1.0) as i32;
            let px = self.frame.pixels();
            self.outline = Some(Outline::draw(px, stride, bounds, r, thick, OUTLINE));
            let label = self.draw_label(r);
            dirty = dirty.union(label.rect());
            self.label = Some(label);
        }
        dirty
    }

    /// "1280 × 720" under the selection (or just inside it at the bottom of
    /// the screen), drawn into the frame with what was there saved.
    fn draw_label(&mut self, r: Rect) -> Saved {
        let bounds = self.frame.bounds();
        let stride = self.frame.width as usize;
        let scale = self.scale_at((r.left, r.bottom - 1));
        let px = |v: f32| (v * scale).round() as i32;
        let text: Vec<u16> = format!("{} × {}", r.width(), r.height())
            .encode_utf16()
            .collect();
        unsafe {
            let dc = self.frame.dc();
            let font = CreateFontW(
                -px(13.0),
                0,
                0,
                0,
                FW_NORMAL.0 as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY,
                0,
                w!("Segoe UI"),
            );
            let previous = SelectObject(dc, HGDIOBJ(font.0));
            let mut size = Default::default();
            let _ = GetTextExtentPoint32W(dc, &text, &mut size);
            let (pad_x, pad_y, gap) = (px(7.0), px(3.0), px(6.0));
            let (w, h) = (size.cx + 2 * pad_x, size.cy + 2 * pad_y);
            // Below the selection while that is still on its monitor.
            let floor = self
                .monitors
                .iter()
                .find(|m| m.contains((r.left, r.bottom - 1)))
                .map_or(bounds.bottom, |m| m.bottom);
            let top = if r.bottom + gap + h <= floor {
                r.bottom + gap
            } else {
                r.bottom - gap - h
            };
            let left = r.left.min(bounds.right - w).max(bounds.left);
            let place = Rect::new(left, top, left + w, top + h);
            let saved = Saved::take(self.frame.pixels(), stride, bounds, place);

            let brush = CreateSolidBrush(COLORREF(0x0020_1c1c));
            let fill = RECT {
                left: place.left,
                top: place.top,
                right: place.right,
                bottom: place.bottom,
            };
            FillRect(dc, &fill, brush);
            SetBkMode(dc, TRANSPARENT);
            SetTextColor(dc, COLORREF(0x00ff_ffff));
            let _ = TextOutW(dc, place.left + pad_x, place.top + pad_y, &text);
            SelectObject(dc, previous);
            let _ = DeleteObject(HGDIOBJ(font.0));
            let _ = DeleteObject(HGDIOBJ(brush.0));
            let _ = GdiFlush();
            saved
        }
    }
}

fn point(lparam: LPARAM) -> Point {
    (
        (lparam.0 & 0xFFFF) as i16 as i32,
        ((lparam.0 >> 16) & 0xFFFF) as i16 as i32,
    )
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_NCCREATE {
            let create = &*(lparam.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const RefCell<State>;
        // Busy (a call below came back in here), or already gone: nothing of ours.
        let Some(Ok(mut s)) = state.as_ref().map(|s| s.try_borrow_mut()) else {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        };
        match msg {
            WM_ERASEBKGND => LRESULT(1),
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let dc = BeginPaint(hwnd, &mut ps);
                let r = ps.rcPaint;
                let _ = BitBlt(
                    dc,
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    Some(s.frame.dc()),
                    r.left,
                    r.top,
                    SRCCOPY,
                );
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                s.input(hwnd, Input::Move(point(lparam)));
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                s.input(hwnd, Input::Down(point(lparam)));
                drop(s);
                SetCapture(hwnd);
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                s.input(hwnd, Input::Up(point(lparam)));
                drop(s);
                let _ = ReleaseCapture();
                LRESULT(0)
            }
            WM_RBUTTONDOWN | WM_CLOSE | WM_DISPLAYCHANGE | WM_HOTKEY => {
                s.input(hwnd, Input::Cancel);
                LRESULT(0)
            }
            WM_KEYDOWN if wparam.0 == VK_ESCAPE.0 as usize => {
                s.input(hwnd, Input::Cancel);
                LRESULT(0)
            }
            WM_ACTIVATE => {
                // Low word 0: deactivated. Alt+Tab or the Win key ends it.
                if wparam.0 & 0xFFFF != 0 {
                    s.was_active = true;
                } else if s.was_active {
                    s.input(hwnd, Input::Cancel);
                }
                drop(s);
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            _ => {
                drop(s);
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
        }
    }
}

/// Dim `frame` and let the user choose on it; returns when they have. The
/// chosen part of the frame is left exactly as it was grabbed.
pub fn select(frame: &mut Frame, targets: Vec<Rect>, monitors: &[Monitor]) -> Outcome {
    let origin = frame.origin;
    let (width, height) = (frame.width, frame.height);
    let stride = width as usize;
    veil::dim_all(frame.pixels());
    let state = RefCell::new(State {
        frame,
        targets,
        monitors: monitors
            .iter()
            .map(|m| m.rect.translate(-origin.0, -origin.1))
            .collect(),
        scales: monitors.iter().map(|m| m.dpi as f32 / 96.0).collect(),
        select: Select::Hover { target: None },
        clear: None,
        outline: None,
        label: None,
        outcome: None,
        was_active: false,
        threshold: unsafe { GetSystemMetrics(SM_CXDRAG) }.max(2),
    });

    unsafe {
        let instance: HINSTANCE = GetModuleHandleW(PCWSTR::null())
            .map(Into::into)
            .unwrap_or_default();
        let class = w!("whir.capture.overlay.v1");
        // Registering twice (a second capture) fails harmlessly.
        RegisterClassExW(&WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: instance,
            hCursor: LoadCursorW(None, IDC_CROSS).unwrap_or_default(),
            lpszClassName: class,
            ..Default::default()
        });
        let created = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            class,
            w!("whir capture"),
            WS_POPUP,
            origin.0,
            origin.1,
            width,
            height,
            None,
            None,
            Some(instance),
            Some(&state as *const RefCell<State> as *const _),
        );
        let Ok(hwnd) = created else {
            return Outcome::Cancelled;
        };

        // The window under the pointer is offered from the first frame.
        let mut at = POINT::default();
        let _ = GetCursorPos(&mut at);
        let at = (at.x - origin.0, at.y - origin.1);
        state.borrow_mut().input(hwnd, Input::Move(at));

        // Painted while cloaked, then revealed: no flash of an empty window.
        let cloak = |on: bool| {
            let v = BOOL::from(on);
            let _ = DwmSetWindowAttribute(hwnd, DWMWA_CLOAK, &v as *const BOOL as *const _, 4);
        };
        cloak(true);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = UpdateWindow(hwnd);
        cloak(false);
        let _ = SetForegroundWindow(hwnd);
        // Esc must work even if Windows wouldn't give us the keyboard.
        let hotkey = RegisterHotKey(Some(hwnd), ESCAPE_HOTKEY, MOD_NOREPEAT, VK_ESCAPE.0 as u32);

        let mut msg = MSG::default();
        let outcome = loop {
            if let Some(outcome) = state.borrow().outcome {
                break outcome;
            }
            if GetMessageW(&mut msg, None, 0, 0).0 <= 0 {
                // The thread is quitting: pass that on to whoever runs its loop.
                PostQuitMessage(msg.wParam.0 as i32);
                break Outcome::Cancelled;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        };

        if hotkey.is_ok() {
            let _ = UnregisterHotKey(Some(hwnd), ESCAPE_HOTKEY);
        }
        {
            let mut s = state.borrow_mut();
            s.undecorate();
            // Whatever part was dimmed must not be in the capture.
            if let Outcome::Region(r) = outcome {
                let clear = s.clear;
                veil::move_clear(s.frame.pixels(), stride, clear, Some(r));
            }
        }
        let _ = DestroyWindow(hwnd);
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOUNDS: Rect = Rect::new(0, 0, 1920, 1080);
    const WINDOW: Rect = Rect::new(100, 100, 500, 400);

    fn run(inputs: &[Input]) -> (Select, Option<Outcome>) {
        let mut s = Select::Hover { target: None };
        let mut outcome = None;
        for &i in inputs {
            assert_eq!(outcome, None, "input after the end");
            outcome = s.step(i, &[WINDOW], &[BOUNDS], BOUNDS, 4);
        }
        (s, outcome)
    }

    #[test]
    fn hovering_offers_the_window_under_the_pointer() {
        let (s, done) = run(&[Input::Move((200, 200))]);
        assert_eq!(s.clear(), Some(WINDOW));
        assert_eq!(done, None);
        // Off the window: the whole monitor.
        let (s, _) = run(&[Input::Move((200, 200)), Input::Move((900, 900))]);
        assert_eq!(s.clear(), Some(BOUNDS));
    }

    #[test]
    fn a_click_takes_the_window_even_with_a_little_wobble() {
        let (_, done) = run(&[
            Input::Move((200, 200)),
            Input::Down((200, 200)),
            Input::Move((203, 198)),
            Input::Up((203, 198)),
        ]);
        assert_eq!(done, Some(Outcome::Region(WINDOW)));
        let (_, done) = run(&[Input::Down((900, 900)), Input::Up((900, 900))]);
        assert_eq!(done, Some(Outcome::Region(BOUNDS)));
    }

    #[test]
    fn a_drag_takes_a_region_in_any_direction() {
        let (s, done) = run(&[
            Input::Down((300, 300)),
            Input::Move((250, 250)),
            Input::Move((120, 180)),
        ]);
        assert_eq!(done, None);
        assert_eq!(s.clear(), Some(Rect::new(120, 180, 300, 300)));
        let (_, done) = run(&[
            Input::Down((300, 300)),
            Input::Move((120, 180)),
            Input::Up((120, 180)),
        ]);
        assert_eq!(done, Some(Outcome::Region(Rect::new(120, 180, 300, 300))));
    }

    #[test]
    fn a_drag_stops_at_the_edge_of_the_screen() {
        let (_, done) = run(&[
            Input::Down((1800, 1000)),
            Input::Move((2500, 1500)),
            Input::Up((2500, 1500)),
        ]);
        assert_eq!(
            done,
            Some(Outcome::Region(Rect::new(1800, 1000, 1920, 1080)))
        );
    }

    #[test]
    fn a_sliver_is_a_slip_and_selecting_goes_on() {
        let (s, done) = run(&[
            Input::Down((300, 300)),
            Input::Move((400, 302)),
            Input::Up((400, 302)),
        ]);
        assert_eq!(done, None);
        assert_eq!(
            s,
            Select::Hover {
                target: Some(WINDOW)
            }
        );
    }

    #[test]
    fn cancelling_works_from_anywhere() {
        assert_eq!(run(&[Input::Cancel]).1, Some(Outcome::Cancelled));
        assert_eq!(
            run(&[Input::Down((300, 300)), Input::Cancel]).1,
            Some(Outcome::Cancelled)
        );
        assert_eq!(
            run(&[
                Input::Down((300, 300)),
                Input::Move((600, 600)),
                Input::Cancel
            ])
            .1,
            Some(Outcome::Cancelled)
        );
    }
}
