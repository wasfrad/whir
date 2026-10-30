//! The dock: a small tab on an edge of the screen. Only a grip shows until
//! the pointer rests on it; then it slides out with two buttons, Capture and
//! Delay. It can be dragged to any edge of any monitor, and remembers where.
//!
//! It never takes the keyboard or becomes the active window: clicking it must
//! not disturb whatever is about to be captured (an open menu, a tooltip).
//!
//! It stays above every other window, full-screen ones included. Being
//! topmost isn't enough for that: whichever topmost window came forward last
//! is above the rest, and a window that is never activated is never brought
//! forward. So it puts itself back on top whenever another window becomes
//! the active one, and every couple of seconds for those that appear without
//! being activated.

use std::cell::RefCell;
use std::sync::atomic::{AtomicIsize, Ordering};

use whir_core::theme::Theme;
use whir_platform::{ipc, shell};
use windows::Win32::Foundation::{
    COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM,
};
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::Graphics::Gdi::{AC_SRC_ALPHA, AC_SRC_OVER, BLENDFUNCTION};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_NOREPEAT, RegisterHotKey, ReleaseCapture, SetCapture, TME_HOVER,
    TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, UnregisterHotKey,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CheckMenuRadioItem, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
    DestroyWindow, DispatchMessageW, EVENT_SYSTEM_FOREGROUND, GWLP_USERDATA, GetCursorPos,
    GetMessageW, GetSystemMetrics, GetWindowLongPtrW, GetWindowRect, HWND_TOPMOST, IDC_ARROW,
    IsIconic, KillTimer, LoadCursorW, MF_BYCOMMAND, MF_CHECKED, MF_GRAYED, MF_SEPARATOR, MF_STRING,
    MSG, PM_REMOVE, PeekMessageW, PostMessageW, PostQuitMessage, RegisterClassExW, SM_CXDRAG,
    SPI_GETCLIENTAREAANIMATION, SW_HIDE, SW_RESTORE, SW_SHOWNOACTIVATE, SW_SHOWNORMAL,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    SetForegroundWindow, SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow, ShowWindowAsync,
    SystemParametersInfoW, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu,
    TranslateMessage, ULW_ALPHA, UpdateLayeredWindow, WINEVENT_OUTOFCONTEXT,
    WINEVENT_SKIPOWNPROCESS, WM_CLOSE, WM_CONTEXTMENU, WM_DESTROY, WM_DISPLAYCHANGE, WM_DPICHANGED,
    WM_HOTKEY, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_NCDESTROY, WM_NULL,
    WM_RBUTTONUP, WM_SETTINGCHANGE, WM_TIMER, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{BOOL, HSTRING, PCWSTR, w};

use crate::Command;
use crate::config::{DELAYS, DockConfig, Hotkey, Spot, format_hotkey};
use crate::geom::{Point, Rect};
use crate::instance::{self, Role, WM_LAUNCHED};
use crate::monitors::{self, Monitor};
use crate::paint::{Surface, icon_mask, text_mask};
use crate::place::{self, Edge, Layout};
use crate::tray::{self, Tray};

const WM_MOUSEHOVER: u32 = 0x02A1;
const WM_MOUSELEAVE: u32 = 0x02A3;
/// Posted to ourselves: take a capture (outside whatever handler asked).
const WM_CAPTURE: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 2;
/// Posted to ourselves: another window became the active one.
const WM_RAISE: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 3;
/// `WM_MOUSEACTIVATE`'s "don't activate me".
const MA_NOACTIVATE: isize = 3;

const TIMER_SLIDE: usize = 1;
const TIMER_LINGER: usize = 2;
const TIMER_COUNT: usize = 3;
const TIMER_RAISE: usize = 4;
const TIMER_KEEP_ON_TOP: usize = 5;
const HOTKEY: i32 = 1;

const CAPTURE: usize = 0;
const DELAY: usize = 1;

/// Milliseconds the pointer rests on the grip before the dock opens, and how
/// long it stays open after the pointer leaves (as the notes drawer does).
const HOVER_MS: u32 = 150;
const LINGER_MS: u32 = 400;
/// How long it shows itself when asked to (`whir.exe --dock`).
const REVEAL_MS: u32 = 1800;
/// After another window is activated, the dock goes back on top at once and
/// again this much later: programs often raise windows of their own (a call's
/// controls, a full-screen view) just after they come forward.
const RAISE_AGAIN_MS: u32 = 250;
/// And every so often, for windows that come forward without being activated
/// (notifications, floating toolbars).
const KEEP_ON_TOP_MS: u32 = 2000;

/// The dock's window, for the foreground hook (it has no other way to it).
static DOCK: AtomicIsize = AtomicIsize::new(0);

const CAMERA: char = '\u{E722}';
const STOPWATCH: char = '\u{E916}';

struct Dock {
    hwnd: HWND,
    config: DockConfig,
    monitors: Vec<Monitor>,
    monitor: usize,
    edge: Edge,
    offset: u16,
    layout: Layout,
    /// The whole panel, drawn; the window shows a slice of it.
    surface: Option<Surface>,
    /// Pixels of the panel's depth showing: from the grip to all of it.
    visible: i32,
    open: bool,
    hover: Option<usize>,
    /// Where the button went down (on screen), and on which of ours.
    pressed: Option<(Point, Option<usize>)>,
    dragging: bool,
    tracking: bool,
    /// Seconds left before a delayed capture.
    countdown: Option<u32>,
    capturing: bool,
    menu_open: bool,
    hotkey_registered: bool,
    tray: Tray,
}

fn rgb(c: [u8; 4]) -> [u8; 3] {
    [c[0], c[1], c[2]]
}

fn cursor() -> Point {
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    (p.x, p.y)
}

impl Dock {
    fn new(hwnd: HWND) -> Dock {
        let mut dock = Dock {
            hwnd,
            config: DockConfig::load(),
            monitors: Vec::new(),
            monitor: 0,
            edge: Edge::Left,
            offset: 500,
            layout: place::layout(96),
            surface: None,
            visible: 0,
            open: false,
            hover: None,
            pressed: None,
            dragging: false,
            tracking: false,
            countdown: None,
            capturing: false,
            menu_open: false,
            hotkey_registered: false,
            tray: Tray::new(hwnd),
        };
        dock.relayout();
        dock
    }

    /// Find our place on the desktop as it is now, and redraw for it.
    fn relayout(&mut self) {
        self.monitors = monitors::list();
        let remembered = self.config.spot.as_ref().and_then(|s| {
            let i = self.monitors.iter().position(|m| m.device == s.monitor)?;
            Some((i, s.edge, s.offset))
        });
        (self.monitor, self.edge, self.offset) =
            remembered.unwrap_or_else(|| place::default_spot(&self.monitors));
        self.layout = place::layout(self.monitors.get(self.monitor).map_or(96, |m| m.dpi));
        self.visible = if self.open {
            self.layout.depth
        } else {
            self.layout.grip
        };
        self.render();
    }

    fn work(&self) -> Rect {
        self.monitors
            .get(self.monitor)
            .map_or(Rect::new(0, 0, 1280, 720), |m| m.work)
    }

    fn anchor(&self) -> Rect {
        place::anchor(self.work(), self.edge, self.offset, &self.layout)
    }

    fn scale(&self) -> f32 {
        self.layout.button as f32 / 34.0
    }

    /// Draw the whole panel.
    fn render(&mut self) {
        let theme = if whir_platform::os::prefers_dark() {
            Theme::dark()
        } else {
            Theme::light()
        };
        let (bg, border) = (
            rgb(theme.drawer_bg.to_array()),
            rgb(theme.drawer_border.to_array()),
        );
        let (text, dim, accent) = (
            rgb(theme.text.to_array()),
            rgb(theme.text_dim.to_array()),
            rgb(theme.accent.to_array()),
        );
        let (edge, l, s) = (self.edge, self.layout, self.scale());
        let (w, h) = l.size(edge);
        let Some(mut surface) = Surface::new(w, h) else {
            return;
        };
        let mut c = surface.canvas();

        // Rounded on the side away from the screen edge; a hairline around it.
        let r = 9.0 * s;
        let (radii, inner) = match edge {
            Edge::Left => ([0.0, r, r, 0.0], Rect::new(0, 1, w - 1, h - 1)),
            Edge::Right => ([r, 0.0, 0.0, r], Rect::new(1, 1, w, h - 1)),
            Edge::Top => ([0.0, 0.0, r, r], Rect::new(1, 0, w - 1, h - 1)),
            Edge::Bottom => ([r, r, 0.0, 0.0], Rect::new(1, 1, w - 1, h)),
        };
        c.round_rect(Rect::new(0, 0, w, h), radii, border, 1.0);
        c.round_rect(inner, radii.map(|v| (v - 1.0).max(0.0)), bg, 1.0);

        // The grip: three dots, like the notes drawer's spine.
        let grip = l.grip_rect(edge);
        let middle = (
            (grip.left + grip.right) as f32 / 2.0,
            (grip.top + grip.bottom) as f32 / 2.0,
        );
        for step in [-1.0, 0.0, 1.0] {
            let d = step * 6.0 * s;
            let at = if edge.is_side() {
                (middle.0, middle.1 + d)
            } else {
                (middle.0 + d, middle.1)
            };
            c.dot(at, 1.5 * s, rgb(theme.drawer_grip.to_array()));
        }

        for i in 0..place::BUTTONS {
            let b = l.button_rect(edge, i);
            if self.hover == Some(i) {
                let hover = rgb(theme.drawer_item_hover.to_array());
                c.round_rect(b, [6.0 * s; 4], hover, 1.0);
            }
            let center = ((b.left + b.right) / 2, (b.top + b.bottom) / 2);
            let label = |text: &str| text_mask(text, "Segoe UI", (15.0 * s).round() as i32);
            let icon = |glyph: char| icon_mask(glyph, (17.0 * s).round() as i32);
            let (mask, color) = match (i, self.countdown, self.config.delay) {
                // Counting down: the seconds left, where the camera was.
                (CAPTURE, Some(n), _) => (label(&n.to_string()), accent),
                (CAPTURE, None, _) => (icon(CAMERA), text),
                (_, _, 0) => (icon(STOPWATCH).or_else(|| label("0s")), dim),
                (_, _, secs) => (label(&format!("{secs}s")), accent),
            };
            match mask {
                Some(mask) => c.mask(center, &mask, color),
                // No icon font at all: a plain dot still says "press me".
                None => c.dot((center.0 as f32, center.1 as f32), 6.0 * s, color),
            }
        }
        self.surface = Some(surface);
    }

    /// Put the right slice of the panel on screen.
    fn present(&self) {
        let Some(surface) = &self.surface else { return };
        let (on_screen, from) = place::viewport(self.edge, self.anchor(), self.visible);
        let at = POINT {
            x: on_screen.left,
            y: on_screen.top,
        };
        let size = SIZE {
            cx: on_screen.width(),
            cy: on_screen.height(),
        };
        let from = POINT {
            x: from.0,
            y: from.1,
        };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            // Closed, it is a little see-through.
            SourceConstantAlpha: if self.open { 255 } else { 205 },
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        unsafe {
            let _ = UpdateLayeredWindow(
                self.hwnd,
                None,
                Some(&at),
                Some(&size),
                Some(surface.dc()),
                Some(&from),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
        }
    }

    fn redraw(&mut self) {
        self.render();
        self.present();
    }

    /// On screen, unless hidden (then only the tray icon shows).
    fn show(&self) {
        let how = if self.config.hidden {
            SW_HIDE
        } else {
            SW_SHOWNOACTIVATE
        };
        unsafe {
            let _ = ShowWindow(self.hwnd, how);
        }
    }

    fn set_hidden(&mut self, hidden: bool) {
        if self.config.hidden != hidden {
            self.config.hidden = hidden;
            self.config.save();
        }
        self.show();
        self.raise();
    }

    /// The button under a point of the window.
    fn button_at(&self, client: Point) -> Option<usize> {
        let (_, from) = place::viewport(self.edge, self.anchor(), self.visible);
        let p = (client.0 + from.0, client.1 + from.1);
        (0..place::BUTTONS).find(|&i| self.layout.button_rect(self.edge, i).contains(p))
    }

    fn set_open(&mut self, open: bool) {
        if self.open == open {
            return;
        }
        self.open = open;
        if !open {
            self.hover = None;
            self.render();
        }
        let mut animate = BOOL(1);
        unsafe {
            let _ = SystemParametersInfoW(
                SPI_GETCLIENTAREAANIMATION,
                0,
                Some(&mut animate as *mut BOOL as *mut _),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            );
            if animate.as_bool() {
                SetTimer(Some(self.hwnd), TIMER_SLIDE, 15, None);
            } else {
                self.visible = self.goal();
            }
        }
        self.present();
    }

    fn goal(&self) -> i32 {
        if self.open {
            self.layout.depth
        } else {
            self.layout.grip
        }
    }

    /// One step of the slide (eased: fast, then settling).
    fn slide(&mut self) {
        let left = self.goal() - self.visible;
        if left == 0 {
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_SLIDE);
            }
            return;
        }
        let step = (left as f32 * 0.4).abs().ceil().max(2.0) as i32;
        self.visible += left.signum() * step.min(left.abs());
        self.present();
    }

    /// Close after a while, unless there is a reason to stay open by then.
    fn linger(&self, ms: u32) {
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_LINGER, ms, None);
        }
    }

    /// Back above every other window. Not while hidden for a capture, or
    /// while the right-click menu is up (it would cover it).
    fn raise(&self) {
        if self.capturing || self.menu_open {
            return;
        }
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }

    fn pointer_is_over(&self) -> bool {
        let mut r = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut r);
        }
        monitors::rect(r).contains(cursor())
    }

    fn lingered(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_LINGER);
        }
        let busy = self.menu_open || self.dragging || self.countdown.is_some();
        if !busy && !self.pointer_is_over() {
            self.set_open(false);
        }
    }

    fn mouse_move(&mut self, client: Point) {
        if !self.tracking {
            let mut track = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE | TME_HOVER,
                hwndTrack: self.hwnd,
                dwHoverTime: HOVER_MS,
            };
            self.tracking = unsafe { TrackMouseEvent(&mut track) }.is_ok();
        }
        if let Some((from, _)) = self.pressed
            && !self.dragging
        {
            let at = cursor();
            let far = 2 * unsafe { GetSystemMetrics(SM_CXDRAG) }.max(2);
            self.dragging = (at.0 - from.0).abs() > far || (at.1 - from.1).abs() > far;
        }
        if self.dragging {
            self.drag_to(cursor());
        } else if self.open {
            let hover = self.button_at(client);
            if hover != self.hover {
                self.hover = hover;
                self.redraw();
            }
        }
    }

    /// Dragged: go to the nearest edge of the monitor the pointer is on.
    fn drag_to(&mut self, at: Point) {
        let Some((monitor, edge, offset)) =
            place::snap(at, &self.monitors, |dpi| place::layout(dpi).length)
        else {
            return;
        };
        let turned = edge != self.edge || monitor != self.monitor;
        (self.monitor, self.edge, self.offset) = (monitor, edge, offset);
        self.hover = None;
        if turned {
            // Another edge is another shape; another monitor, maybe another scale.
            self.layout = place::layout(self.monitors[monitor].dpi);
            self.visible = self.layout.depth;
            self.open = true;
            self.render();
        }
        self.present();
    }

    fn released(&mut self, client: Point) {
        let Some((_, on)) = self.pressed.take() else {
            return;
        };
        if std::mem::take(&mut self.dragging) {
            self.config.spot = self.monitors.get(self.monitor).map(|m| Spot {
                monitor: m.device.clone(),
                edge: self.edge,
                offset: self.offset,
            });
            self.config.save();
            self.linger(LINGER_MS);
        } else if let Some(button) = on
            && self.button_at(client) == Some(button)
        {
            self.press(button);
        }
    }

    fn press(&mut self, button: usize) {
        match button {
            CAPTURE => self.capture_pressed(),
            DELAY => {
                // Step through the choices.
                let at = DELAYS.iter().position(|&d| d == self.config.delay);
                self.set_delay(DELAYS[(at.unwrap_or(0) + 1) % DELAYS.len()]);
            }
            _ => {}
        }
    }

    fn set_delay(&mut self, seconds: u32) {
        self.config.delay = seconds;
        self.config.save();
        self.redraw();
    }

    /// The Capture button: now, or after the delay; again to call it off.
    fn capture_pressed(&mut self) {
        unsafe {
            if self.countdown.take().is_some() {
                let _ = KillTimer(Some(self.hwnd), TIMER_COUNT);
                self.redraw();
                self.linger(LINGER_MS);
            } else if self.config.delay > 0 {
                self.countdown = Some(self.config.delay);
                SetTimer(Some(self.hwnd), TIMER_COUNT, 1000, None);
                self.set_open(true);
                self.redraw();
            } else {
                let _ = PostMessageW(Some(self.hwnd), WM_CAPTURE, WPARAM(0), LPARAM(0));
            }
        }
    }

    fn count(&mut self) {
        match self.countdown {
            Some(n) if n > 1 => {
                self.countdown = Some(n - 1);
                self.redraw();
            }
            _ => self.capture_now(),
        }
    }

    /// Skip (or cut short) the delay.
    fn capture_now(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_COUNT);
            self.countdown = None;
            let _ = PostMessageW(Some(self.hwnd), WM_CAPTURE, WPARAM(0), LPARAM(0));
        }
    }

    fn register_hotkey(&mut self) {
        let Some(Hotkey { mods, vk }) = self.config.hotkey else {
            return;
        };
        if !self.hotkey_registered {
            let mods = HOT_KEY_MODIFIERS(mods) | MOD_NOREPEAT;
            // Fails if another program has the combination: the menu says so.
            self.hotkey_registered =
                unsafe { RegisterHotKey(Some(self.hwnd), HOTKEY, mods, vk) }.is_ok();
        }
    }
}

/// One capture, with the dock out of the picture. Not under a borrow of the
/// dock: selecting runs its own message loop, which comes back in here.
fn capture(dock: &RefCell<Dock>, hwnd: HWND) {
    {
        let mut d = dock.borrow_mut();
        if d.capturing {
            return;
        }
        d.capturing = true;
        d.open = false;
        d.hover = None;
        d.visible = d.layout.grip;
        d.render();
    }
    let result = unsafe {
        let _ = KillTimer(Some(hwnd), TIMER_SLIDE);
        let _ = ShowWindow(hwnd, SW_HIDE);
        // Until the screen has been composed without us.
        let _ = DwmFlush();
        let _ = DwmFlush();
        let result = crate::capture();
        {
            let d = dock.borrow();
            d.present();
            d.show();
        }
        // A hotkey pressed while selecting isn't a request for another.
        let mut stale = MSG::default();
        while PeekMessageW(&mut stale, Some(hwnd), WM_HOTKEY, WM_HOTKEY, PM_REMOVE).as_bool() {}
        result
    };
    {
        let mut d = dock.borrow_mut();
        d.capturing = false;
        // The selection window was above everything; now we are again.
        d.raise();
    }
    // The frozen screen was tens of megabytes: give its pages back now.
    unsafe {
        use windows::Win32::System::Threading::{GetCurrentProcess, SetProcessWorkingSetSize};
        let _ = SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
    }
    if let Err(e) = result {
        crate::report(&e);
    }
}

const MENU_CAPTURE: usize = 1;
const MENU_DELAY: usize = 10;
const MENU_OPEN_WHIR: usize = 20;
const MENU_OPEN_FOLDER: usize = 21;
const MENU_HIDE: usize = 30;
const MENU_SHOW: usize = 31;
const MENU_AUTOSTART: usize = 32;
const MENU_QUIT: usize = 40;

/// The right-click menu. Also not under a borrow: a menu is a message loop.
fn menu(dock: &RefCell<Dock>, hwnd: HWND) {
    let (delay, hotkey, registered) = {
        let mut d = dock.borrow_mut();
        d.register_hotkey();
        d.menu_open = true;
        (d.config.delay, d.config.hotkey, d.hotkey_registered)
    };
    let picked = unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            dock.borrow_mut().menu_open = false;
            return;
        };
        let item = |flags, id: usize, text: &str| {
            let _ = AppendMenuW(menu, flags, id, &HSTRING::from(text));
        };
        match hotkey.map(format_hotkey) {
            Some(keys) if registered => item(MF_STRING, MENU_CAPTURE, &format!("Capture\t{keys}")),
            Some(keys) => {
                item(MF_STRING, MENU_CAPTURE, "Capture");
                item(
                    MF_STRING | MF_GRAYED,
                    0,
                    &format!("{keys} is in use by another app"),
                );
            }
            None => item(MF_STRING, MENU_CAPTURE, "Capture"),
        }
        item(MF_SEPARATOR, 0, "");
        for seconds in DELAYS {
            let text = match seconds {
                0 => "No delay".to_owned(),
                n => format!("{n} second delay"),
            };
            item(MF_STRING, MENU_DELAY + seconds as usize, &text);
        }
        let _ = CheckMenuRadioItem(
            menu,
            MENU_DELAY as u32,
            (MENU_DELAY + 5) as u32,
            MENU_DELAY as u32 + delay,
            MF_BYCOMMAND.0,
        );
        item(MF_SEPARATOR, 0, "");
        item(MF_STRING, MENU_OPEN_WHIR, "Open whir");
        item(MF_STRING, MENU_OPEN_FOLDER, "Open screenshots folder");
        item(MF_SEPARATOR, 0, "");
        item(MF_STRING, MENU_HIDE, "Hide dock");

        let at = cursor();
        // So that a click elsewhere dismisses the menu.
        let _ = SetForegroundWindow(hwnd);
        let picked = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
            at.0,
            at.1,
            None,
            hwnd,
            None,
        );
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        picked.0 as usize
    };
    let mut d = dock.borrow_mut();
    d.menu_open = false;
    d.linger(LINGER_MS);
    match picked {
        MENU_CAPTURE => d.capture_pressed(),
        MENU_OPEN_WHIR => open_whir(),
        MENU_OPEN_FOLDER => open_folder(),
        MENU_HIDE => d.set_hidden(true),
        id if DELAYS.iter().any(|&s| MENU_DELAY + s as usize == id) => {
            d.set_delay((id - MENU_DELAY) as u32);
        }
        _ => {}
    }
}

/// The tray icon's menu: the dock's way back when hidden, and whir's way out.
fn tray_menu(dock: &RefCell<Dock>, hwnd: HWND) {
    let (hidden, hotkey, registered) = {
        let mut d = dock.borrow_mut();
        d.register_hotkey();
        d.menu_open = true;
        (d.config.hidden, d.config.hotkey, d.hotkey_registered)
    };
    let autostart = shell::autostart().is_some();
    let picked = unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            dock.borrow_mut().menu_open = false;
            return;
        };
        let item = |flags, id: usize, text: &str| {
            let _ = AppendMenuW(menu, flags, id, &HSTRING::from(text));
        };
        match hotkey.map(format_hotkey) {
            Some(keys) if registered => item(MF_STRING, MENU_CAPTURE, &format!("Capture\t{keys}")),
            _ => item(MF_STRING, MENU_CAPTURE, "Capture"),
        }
        item(MF_STRING, MENU_OPEN_WHIR, "Open whir");
        item(MF_SEPARATOR, 0, "");
        if hidden {
            item(MF_STRING, MENU_SHOW, "Show dock");
        } else {
            item(MF_STRING, MENU_HIDE, "Hide dock");
        }
        let checked = if autostart { MF_CHECKED } else { MF_STRING };
        item(MF_STRING | checked, MENU_AUTOSTART, "Start with Windows");
        item(MF_SEPARATOR, 0, "");
        item(MF_STRING, MENU_QUIT, "Quit whir");

        let at = cursor();
        // So that a click elsewhere dismisses the menu.
        let _ = SetForegroundWindow(hwnd);
        let picked = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
            at.0,
            at.1,
            None,
            hwnd,
            None,
        );
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        picked.0 as usize
    };
    let mut d = dock.borrow_mut();
    d.menu_open = false;
    match picked {
        MENU_CAPTURE => d.capture_pressed(),
        MENU_OPEN_WHIR => open_whir(),
        MENU_SHOW => {
            d.set_hidden(false);
            d.set_open(true);
            d.linger(REVEAL_MS);
        }
        MENU_HIDE => d.set_hidden(true),
        MENU_AUTOSTART => {
            if let Err(e) = shell::set_autostart(!autostart) {
                drop(d);
                crate::report(&e);
            }
        }
        MENU_QUIT => {
            drop(d);
            quit(hwnd);
        }
        _ => {}
    }
}

/// Close whir altogether: its window (which still asks about unsaved work)
/// and the dock.
fn quit(hwnd: HWND) {
    unsafe {
        if let Some(main) = ipc::find_main_window() {
            let _ = PostMessageW(Some(main), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        let _ = DestroyWindow(hwnd);
    }
}

/// The first time the dock runs, whir starts with Windows from then on; after
/// that, the Run entry is the user's to switch off. whir is a folder that can
/// be moved, so an entry pointing at another copy is pointed at this one.
fn autostart_once(config: &mut DockConfig) {
    // A development build never claims the user's sign-in.
    if cfg!(debug_assertions) {
        return;
    }
    let wanted = shell::autostart_command();
    match shell::autostart() {
        None if !config.autostart_set => {
            let _ = shell::set_autostart(true);
        }
        Some(now) if wanted.is_some_and(|w| !w.eq_ignore_ascii_case(&now)) => {
            let _ = shell::set_autostart(true);
        }
        _ => {}
    }
    if !config.autostart_set {
        config.autostart_set = true;
        config.save();
    }
}

/// Bring whir's window forward, or start whir.
fn open_whir() {
    match ipc::find_main_window() {
        Some(hwnd) => unsafe {
            if IsIconic(hwnd).as_bool() {
                let _ = ShowWindowAsync(hwnd, SW_RESTORE);
            }
            let _ = SetForegroundWindow(hwnd);
        },
        None => {
            if let Ok(exe) = std::env::current_exe() {
                let _ = std::process::Command::new(exe).spawn();
            }
        }
    }
}

fn open_folder() {
    let Some(dir) = whir_core::captures::resolve_dir() else {
        return;
    };
    let _ = std::fs::create_dir_all(&dir);
    unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &HSTRING::from(dir.as_os_str()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
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
        let dock = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const RefCell<Dock>;
        let Some(dock) = dock.as_ref() else {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        };
        // These run message loops of their own, so they take the dock only
        // for the moments they need it.
        match msg {
            WM_CAPTURE => {
                capture(dock, hwnd);
                return LRESULT(0);
            }
            WM_RBUTTONUP => {
                menu(dock, hwnd);
                return LRESULT(0);
            }
            tray::WM_TRAY => {
                match (lparam.0 & 0xFFFF) as u32 {
                    WM_CONTEXTMENU => tray_menu(dock, hwnd),
                    tray::NIN_SELECT | tray::NIN_KEYSELECT => open_whir(),
                    _ => {}
                }
                return LRESULT(0);
            }
            WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE),
            WM_NCDESTROY => {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                return DefWindowProcW(hwnd, msg, wparam, lparam);
            }
            _ => {}
        }
        // Busy (one of the loops above is running): leave it be.
        let Ok(mut d) = dock.try_borrow_mut() else {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        };
        match msg {
            WM_MOUSEMOVE => d.mouse_move(point(lparam)),
            WM_MOUSEHOVER => {
                // Resting on the grip opens it; passing by with a button held doesn't.
                if wparam.0 & 0x1f == 0 {
                    d.set_open(true);
                }
            }
            WM_MOUSELEAVE => {
                d.tracking = false;
                if d.hover.take().is_some() {
                    d.redraw();
                }
                d.linger(LINGER_MS);
            }
            WM_LBUTTONDOWN => {
                d.pressed = Some((cursor(), d.button_at(point(lparam))));
                drop(d);
                SetCapture(hwnd);
            }
            WM_LBUTTONUP => {
                d.released(point(lparam));
                drop(d);
                let _ = ReleaseCapture();
            }
            WM_TIMER => match wparam.0 {
                TIMER_SLIDE => d.slide(),
                TIMER_LINGER => d.lingered(),
                TIMER_COUNT => d.count(),
                TIMER_RAISE => {
                    let _ = KillTimer(Some(hwnd), TIMER_RAISE);
                    d.raise();
                }
                TIMER_KEEP_ON_TOP => d.raise(),
                _ => {}
            },
            WM_HOTKEY => {
                if !d.capturing {
                    d.capture_now();
                }
            }
            WM_RAISE => {
                d.raise();
                SetTimer(Some(hwnd), TIMER_RAISE, RAISE_AGAIN_MS, None);
            }
            WM_LAUNCHED => match instance::from_code(wparam.0) {
                Some(Command::Reveal) => {
                    d.set_hidden(false);
                    d.set_open(true);
                    d.linger(REVEAL_MS);
                }
                // Already running: that was all.
                Some(Command::Start) => {}
                Some(Command::Capture) => d.capture_now(),
                Some(Command::Quit) => {
                    drop(d);
                    let _ = DestroyWindow(hwnd);
                }
                None => {}
            },
            // Monitors, scaling, the taskbar or the theme changed.
            WM_DISPLAYCHANGE | WM_DPICHANGED | WM_SETTINGCHANGE => {
                d.relayout();
                d.present();
                d.raise();
            }
            WM_DESTROY => {
                DOCK.store(0, Ordering::Relaxed);
                if d.hotkey_registered {
                    let _ = UnregisterHotKey(Some(hwnd), HOTKEY);
                }
                PostQuitMessage(0);
            }
            // Explorer restarted, with an empty notification area.
            _ if d.tray.is_taskbar_created(msg) => d.tray.add(),
            _ => {
                drop(d);
                return DefWindowProcW(hwnd, msg, wparam, lparam);
            }
        }
        LRESULT(0)
    }
}

/// Another program's window became the active one: it may be above us now.
/// Out of context, so this runs on our thread, inside `GetMessageW`.
unsafe extern "system" fn foreground_changed(
    _hook: HWINEVENTHOOK,
    _event: u32,
    _hwnd: HWND,
    _object: i32,
    _child: i32,
    _thread: u32,
    _time: u32,
) {
    let dock = DOCK.load(Ordering::Relaxed);
    if dock != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(dock as *mut _)), WM_RAISE, WPARAM(0), LPARAM(0));
        }
    }
}

/// Be the dock (or hand `cmd` to the one already running). Returns the
/// process's exit code.
pub fn run(cmd: Command) -> i32 {
    let _guard = match instance::acquire(cmd) {
        Role::Forwarded => return 0,
        Role::Primary(guard) => guard,
    };
    // Asked to close a dock that wasn't there.
    if cmd == Command::Quit {
        return 0;
    }
    unsafe {
        let instance: HINSTANCE = GetModuleHandleW(PCWSTR::null())
            .map(Into::into)
            .unwrap_or_default();
        RegisterClassExW(&WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: instance,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: instance::CLASS,
            ..Default::default()
        });
        // Layered: drawn with per-pixel alpha. A tool window that is never
        // activated: not in the taskbar or Alt+Tab, and it takes no focus.
        let created = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            instance::CLASS,
            w!("whir capture dock"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance),
            None,
        );
        let Ok(hwnd) = created else {
            crate::report("The capture dock couldn't be started.");
            return 1;
        };
        let dock = RefCell::new(Dock::new(hwnd));
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, &dock as *const RefCell<Dock> as isize);
        DOCK.store(hwnd.0 as isize, Ordering::Relaxed);
        let hook = SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            None,
            Some(foreground_changed),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        );
        SetTimer(Some(hwnd), TIMER_KEEP_ON_TOP, KEEP_ON_TOP_MS, None);
        {
            let mut d = dock.borrow_mut();
            autostart_once(&mut d.config);
            d.register_hotkey();
            d.present();
            d.show();
        }
        // Say hello, or get straight to it.
        let first = PostMessageW(
            Some(hwnd),
            WM_LAUNCHED,
            WPARAM(instance::code(cmd)),
            LPARAM(0),
        );
        let _ = first;

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        if !hook.is_invalid() {
            let _ = UnhookWinEvent(hook);
        }
        msg.wParam.0 as i32
    }
}
