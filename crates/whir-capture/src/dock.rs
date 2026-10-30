//! The dock: a small tab on an edge of the screen. Only a grip shows until
//! the pointer rests on it; then it slides out with its buttons: Capture,
//! Delay, the timer (pause/resume, showing the time so far) and a ▾ that
//! opens the time drawer. It can be dragged to any edge of any monitor, and
//! remembers where.
//!
//! Beside it, a panel (a second window) shows what must be seen: the todos,
//! the "were you away?" question, and the time drawer when it is open. While
//! there is something to show, or a timer runs, the dock stays out.
//!
//! It never takes the keyboard or becomes the active window: clicking it must
//! not disturb whatever is about to be captured (an open menu, a tooltip).
//! Typing a note opens a little window of its own that does.
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
use whir_core::time::Stamp;
use whir_platform::ipc::{self, Changed};
use whir_platform::shell;
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
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CheckMenuRadioItem, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
    DestroyWindow, DispatchMessageW, EVENT_SYSTEM_FOREGROUND, GWLP_USERDATA, GetCursorPos,
    GetMessageW, GetSystemMetrics, GetWindowLongPtrW, GetWindowRect, HWND_TOPMOST, IDC_ARROW,
    IsIconic, KillTimer, LoadCursorW, MF_BYCOMMAND, MF_CHECKED, MF_GRAYED, MF_SEPARATOR, MF_STRING,
    MSG, PM_REMOVE, PeekMessageW, PostMessageW, PostQuitMessage, RegisterClassExW, SM_CXDRAG,
    SPI_GETCLIENTAREAANIMATION, SW_HIDE, SW_RESTORE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOSIZE, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SetForegroundWindow, SetTimer,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, ShowWindowAsync, SystemParametersInfoW,
    TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, ULW_ALPHA,
    UpdateLayeredWindow, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_CLOSE, WM_CONTEXTMENU,
    WM_DESTROY, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_HOTKEY, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCDESTROY, WM_NULL, WM_POWERBROADCAST,
    WM_RBUTTONUP, WM_SETTINGCHANGE, WM_TIMER, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{BOOL, HSTRING, PCWSTR, w};

use crate::Command;
use crate::clock::Clock;
use crate::config::{DELAYS, DockConfig, Hotkey, Spot, format_hotkey};
use crate::geom::{Point, Rect};
use crate::instance::{self, Role, WM_LAUNCHED};
use crate::monitors::{self, Monitor};
use crate::note_edit::{self, NoteEdit, WM_NOTE_DONE};
use crate::paint::{Surface, icon_mask, label_mask, text_mask};
use crate::panel::{self, Hit};
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
/// `WM_POWERBROADCAST`: back from sleep.
const PBT_APMRESUMEAUTOMATIC: usize = 0x12;

const TIMER_SLIDE: usize = 1;
const TIMER_LINGER: usize = 2;
const TIMER_COUNT: usize = 3;
const TIMER_RAISE: usize = 4;
const TIMER_KEEP_ON_TOP: usize = 5;
/// While a timer runs: the time shown, and whether the user is away.
const TIMER_TICK: usize = 6;
const HOTKEY: i32 = 1;

const CAPTURE: usize = 0;
const DELAY: usize = 1;
const TIMER: usize = 2;
const MORE: usize = 3;

/// Milliseconds the pointer rests on the grip before the dock opens, and how
/// long it stays open after the pointer leaves (as the notes drawer does).
const HOVER_MS: u32 = 150;
const LINGER_MS: u32 = 400;
/// The time drawer stays a little longer: it has more to aim at.
const DRAWER_LINGER_MS: u32 = 1200;
/// How long it shows itself when asked to (`whir.exe --dock`).
const REVEAL_MS: u32 = 1800;
/// After another window is activated, the dock goes back on top at once and
/// again this much later: programs often raise windows of their own (a call's
/// controls, a full-screen view) just after they come forward.
const RAISE_AGAIN_MS: u32 = 250;
/// And every so often, for windows that come forward without being activated
/// (notifications, floating toolbars).
const KEEP_ON_TOP_MS: u32 = 2000;
const TICK_MS: u32 = 15_000;

/// The dock's window, for the foreground hook (it has no other way to it).
static DOCK: AtomicIsize = AtomicIsize::new(0);

const PANEL_CLASS: PCWSTR = w!("whir.dock.panel.v1");

const CAMERA: char = '\u{E722}';
const STOPWATCH: char = '\u{E916}';
const PLAY: char = '\u{E768}';
const PAUSE: char = '\u{E769}';
const CLOCK: char = '\u{E917}';
const CHEVRON_DOWN: char = '\u{E70D}';
const CHEVRON_UP: char = '\u{E70E}';

struct Dock {
    hwnd: HWND,
    config: DockConfig,
    monitors: Vec<Monitor>,
    monitor: usize,
    edge: Edge,
    offset: u16,
    layout: Layout,
    /// The whole strip, drawn; the window shows a slice of it.
    surface: Option<Surface>,
    /// Pixels of the strip's depth showing: from the grip to all of it.
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

    panel: Panel,
    /// The time drawer is open.
    expanded: bool,
    clock: Clock,
    /// Typing the running timer's note.
    note: Option<NoteEdit>,
}

/// The panel's window and what is on it.
struct Panel {
    hwnd: HWND,
    items: Vec<panel::Item>,
    /// On screen; empty while hidden.
    rect: Rect,
    /// It is below the strip (the ▾ points at it).
    below: bool,
    hover: Option<usize>,
    pressed: Option<usize>,
    tracking: bool,
    scroll: i32,
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

fn theme() -> Theme {
    if whir_platform::os::prefers_dark() {
        Theme::dark()
    } else {
        Theme::light()
    }
}

/// Put `surface` on `hwnd`'s screen at `rect`, from `from` in the picture.
fn present_layered(hwnd: HWND, surface: &Surface, rect: Rect, from: Point, alpha: u8) {
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: alpha,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    unsafe {
        let _ = UpdateLayeredWindow(
            hwnd,
            None,
            Some(&POINT {
                x: rect.left,
                y: rect.top,
            }),
            Some(&SIZE {
                cx: rect.width(),
                cy: rect.height(),
            }),
            Some(surface.dc()),
            Some(&POINT {
                x: from.0,
                y: from.1,
            }),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        );
    }
}

impl Dock {
    fn new(hwnd: HWND, panel: HWND) -> Dock {
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
            panel: Panel {
                hwnd: panel,
                items: Vec::new(),
                rect: Rect::default(),
                below: true,
                hover: None,
                pressed: None,
                tracking: false,
                scroll: 0,
            },
            expanded: false,
            clock: Clock::new(),
            note: None,
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
        self.panel.below = self.edge != Edge::Bottom;
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

    /// Draw the whole strip.
    fn render(&mut self) {
        let theme = theme();
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

        let elapsed = self.clock.elapsed(Stamp::now());
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
                (DELAY, _, 0) => (icon(STOPWATCH).or_else(|| label("0s")), dim),
                (DELAY, _, secs) => (label(&format!("{secs}s")), accent),
                (TIMER, ..) => match &elapsed {
                    // Running: the time so far, or (pointed at) pause.
                    Some(_) if self.hover == Some(TIMER) => (icon(PAUSE), accent),
                    Some(t) => (
                        label_mask(t, "Segoe UI", (12.5 * s).round() as i32, true, b.width()),
                        accent,
                    ),
                    None if self.clock.can_resume() => (icon(PLAY), text),
                    None => (icon(CLOCK), dim),
                },
                _ => {
                    // The ▾ points at the panel: down to open it below, up to close.
                    let down = self.panel.below != self.expanded;
                    let glyph = if down { CHEVRON_DOWN } else { CHEVRON_UP };
                    let small = icon_mask(glyph, (12.0 * s).round() as i32);
                    (small, if self.expanded { accent } else { dim })
                }
            };
            match mask {
                Some(mask) => c.mask(center, &mask, color),
                // No icon font at all: a plain dot still says "press me".
                None => c.dot((center.0 as f32, center.1 as f32), 6.0 * s, color),
            }
        }
        self.surface = Some(surface);
    }

    /// Put the right slice of the strip on screen.
    fn present(&self) {
        let Some(surface) = &self.surface else { return };
        let (on_screen, from) = place::viewport(self.edge, self.anchor(), self.visible);
        // Closed, it is a little see-through.
        let alpha = if self.open { 255 } else { 205 };
        present_layered(self.hwnd, surface, on_screen, from, alpha);
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
        self.settle();
    }

    /// Something must be seen (a todo, a question), or a timer runs and the
    /// dock is to stay out for it.
    fn pinned_out(&self) -> bool {
        !self.config.hidden
            && (self.expanded || self.clock.away.is_some() || self.clock.keeps_out())
    }

    /// After whatever changed what the dock shows: the timer ticking (or
    /// not), the tooltip, the strip, and out (or in) as needed.
    fn settle(&mut self) {
        let now = Stamp::now();
        unsafe {
            if self.clock.running().is_some() {
                SetTimer(Some(self.hwnd), TIMER_TICK, TICK_MS, None);
            } else {
                let _ = KillTimer(Some(self.hwnd), TIMER_TICK);
            }
        }
        self.tray.set_tip(&self.clock.tip(now));
        self.redraw();
        if self.pinned_out() && !self.open {
            self.set_open(true);
        } else if self.open {
            self.update_panel();
            if !self.pinned_out() && !self.pointer_is_over() {
                self.linger(LINGER_MS);
            }
        }
    }

    /// Lay out, draw and place the panel, or hide it if there's nothing on
    /// it (or the strip isn't out).
    fn update_panel(&mut self) {
        let model = self.clock.model(Stamp::now(), self.expanded);
        let fully_out = self.open && self.visible == self.layout.depth;
        let shown = fully_out
            && !model.is_empty()
            && !self.capturing
            && !self.dragging
            && !self.config.hidden;
        if !shown {
            self.hide_panel();
            return;
        }
        let s = self.scale();
        let width = self.layout.panel;
        let items = panel::layout(&model, width, s);
        let want = panel::height(&items, s);
        let (rect, below) = place::panel_rect(self.work(), self.edge, self.anchor(), (width, want));
        if rect.height() <= 0 {
            self.hide_panel();
            return;
        }
        if below != self.panel.below {
            self.panel.below = below;
            self.redraw();
        }
        let p = &mut self.panel;
        p.scroll = p.scroll.clamp(0, (want - rect.height()).max(0));
        if p.hover.is_some_and(|h| h >= items.len()) {
            p.hover = None;
        }
        // Square where it meets the screen's edge.
        let r = 9.0 * s;
        let radii = match self.edge {
            Edge::Left => [0.0, r, r, 0.0],
            Edge::Right => [r, 0.0, 0.0, r],
            Edge::Top | Edge::Bottom => [r; 4],
        };
        let size = (rect.width(), rect.height());
        if let Some(surface) = panel::draw(&items, size, p.scroll, p.hover, radii, &theme(), s) {
            present_layered(p.hwnd, &surface, rect, (0, 0), 255);
        }
        if p.rect.is_empty() {
            unsafe {
                let _ = ShowWindow(p.hwnd, SW_SHOWNOACTIVATE);
            }
        }
        p.items = items;
        p.rect = rect;
        self.raise();
    }

    fn hide_panel(&mut self) {
        if !self.panel.rect.is_empty() {
            unsafe {
                let _ = ShowWindow(self.panel.hwnd, SW_HIDE);
            }
        }
        self.panel.rect = Rect::default();
        self.panel.hover = None;
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
            self.expanded = false;
            self.hide_panel();
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
        if open && self.visible == self.goal() {
            self.update_panel();
        }
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
            // Out: the panel beside it, if it has anything to show.
            if self.open {
                self.update_panel();
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

    fn linger_ms(&self) -> u32 {
        if self.expanded {
            DRAWER_LINGER_MS
        } else {
            LINGER_MS
        }
    }

    /// Back above every other window. Not while hidden for a capture, or
    /// while the right-click menu is up (it would cover it).
    fn raise(&self) {
        if self.capturing || self.menu_open {
            return;
        }
        let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE;
        unsafe {
            let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, flags);
            if !self.panel.rect.is_empty() {
                let _ = SetWindowPos(self.panel.hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, flags);
            }
        }
    }

    fn pointer_is_over(&self) -> bool {
        let mut r = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut r);
        }
        let at = cursor();
        monitors::rect(r).contains(at) || self.panel.rect.contains(at)
    }

    fn lingered(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_LINGER);
        }
        let busy =
            self.menu_open || self.dragging || self.countdown.is_some() || self.note.is_some();
        if busy || self.pointer_is_over() {
            return;
        }
        if self.expanded {
            self.expanded = false;
            self.redraw();
            self.update_panel();
        }
        if !self.pinned_out() {
            self.set_open(false);
        }
    }

    fn track(hwnd: HWND, hover: bool) -> bool {
        let mut track = TRACKMOUSEEVENT {
            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: if hover {
                TME_LEAVE | TME_HOVER
            } else {
                TME_LEAVE
            },
            hwndTrack: hwnd,
            dwHoverTime: HOVER_MS,
        };
        unsafe { TrackMouseEvent(&mut track) }.is_ok()
    }

    fn mouse_move(&mut self, client: Point) {
        if !self.tracking {
            self.tracking = Dock::track(self.hwnd, true);
        }
        if let Some((from, _)) = self.pressed
            && !self.dragging
        {
            let at = cursor();
            let far = 2 * unsafe { GetSystemMetrics(SM_CXDRAG) }.max(2);
            self.dragging = (at.0 - from.0).abs() > far || (at.1 - from.1).abs() > far;
            if self.dragging {
                self.hide_panel();
            }
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
            self.panel.below = edge != Edge::Bottom;
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
            self.update_panel();
            self.linger(self.linger_ms());
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
            TIMER if self.clock.running().is_some() || self.clock.can_resume() => {
                self.clock.toggle();
                self.settle();
            }
            // Nothing to resume: the drawer, to pick a timer.
            TIMER | MORE => self.set_expanded(!self.expanded || button == TIMER),
            _ => {}
        }
    }

    fn set_expanded(&mut self, expanded: bool) {
        self.expanded = expanded;
        if expanded {
            // What the window may have changed since.
            self.clock.refresh();
            self.panel.scroll = 0;
        }
        self.settle();
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

    /// The time shown, and time away, every little while a timer runs.
    fn tick(&mut self) {
        let now = Stamp::now();
        let asked = self.clock.tick(now);
        self.tray.set_tip(&self.clock.tip(now));
        self.redraw();
        if asked {
            self.settle();
        } else if !self.panel.rect.is_empty() {
            self.update_panel();
        }
    }

    /// The window (or another dock) wrote the timesheet.
    fn shared_changed(&mut self, what: Option<Changed>) {
        if what == Some(Changed::Time) {
            self.clock.refresh();
            self.settle();
        }
    }

    // --- the panel ---

    fn panel_mouse_move(&mut self, client: Point) {
        if !self.panel.tracking {
            self.panel.tracking = Dock::track(self.panel.hwnd, false);
        }
        let hover = panel::hit_at(&self.panel.items, client, self.panel.scroll);
        if hover != self.panel.hover {
            self.panel.hover = hover;
            self.update_panel();
        }
    }

    fn panel_left(&mut self) {
        self.panel.tracking = false;
        if self.panel.hover.take().is_some() {
            self.update_panel();
        }
        self.linger(self.linger_ms());
    }

    fn panel_scrolled(&mut self, delta: i32) {
        let row = (30.0 * self.scale()).round() as i32;
        self.panel.scroll = (self.panel.scroll - delta * 3 * row / 120).max(0);
        self.update_panel();
    }

    fn panel_released(&mut self, client: Point) {
        let Some(pressed) = self.panel.pressed.take() else {
            return;
        };
        if panel::hit_at(&self.panel.items, client, self.panel.scroll) != Some(pressed) {
            return;
        }
        let Some(hit) = self.panel.items.get(pressed).and_then(|i| i.hit) else {
            return;
        };
        match hit {
            Hit::Timer(id) => self.clock.start(id),
            Hit::Stop => self.clock.stop(),
            Hit::Note => return self.edit_note(pressed),
            Hit::Keep => self.clock.keep_away(),
            Hit::Drop => self.clock.drop_away(),
            Hit::Timesheet => open_settings("timesheet"),
            Hit::AddTimer => open_settings("timers"),
            Hit::Todo(_) | Hit::TodoDone(_) | Hit::MoreTodos => open_whir(),
        }
        self.settle();
    }

    /// Type the running timer's note, in a box over its line.
    fn edit_note(&mut self, item: usize) {
        let Some(row) = self.panel.items.get(item).map(|i| i.rect) else {
            return;
        };
        let s = self.scale();
        let indent = (20.0 * s).round() as i32;
        let at = row.translate(
            self.panel.rect.left,
            self.panel.rect.top - self.panel.scroll,
        );
        let at = Rect::new(at.left + indent, at.top, at.right, at.bottom);
        let px = (12.5 * s).round() as i32;
        self.note = NoteEdit::open(self.hwnd, at, &self.clock.note(), &theme(), px);
    }

    fn note_done(&mut self, keep: bool) {
        let Some(edit) = self.note.take() else { return };
        if keep {
            let text = edit.text();
            if text.trim() != self.clock.note() {
                self.clock.set_note(&text);
            }
        }
        drop(edit);
        self.update_panel();
        self.linger(self.linger_ms());
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
        d.note_done(true);
        d.capturing = true;
        d.open = false;
        d.expanded = false;
        d.hover = None;
        d.visible = d.layout.grip;
        d.hide_panel();
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
        d.settle();
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
const MENU_TIMESHEET: usize = 22;
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
        item(MF_STRING, MENU_TIMESHEET, "Timesheet…");
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
    d.linger(d.linger_ms());
    match picked {
        MENU_CAPTURE => d.capture_pressed(),
        MENU_OPEN_WHIR => open_whir(),
        MENU_TIMESHEET => open_settings("timesheet"),
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
        item(MF_STRING, MENU_TIMESHEET, "Timesheet…");
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
        MENU_TIMESHEET => open_settings("timesheet"),
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

/// A page of whir's Settings (the timesheet, the timers), in the running
/// window or a new one.
fn open_settings(page: &str) {
    if let Some(hwnd) = ipc::find_main_window()
        && ipc::send_show(hwnd, page)
    {
        return;
    }
    if let Ok(exe) = std::env::current_exe()
        && let Ok(child) = std::process::Command::new(exe)
            .args(["--settings", page])
            .spawn()
    {
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(child.id());
        }
    }
}

fn open_folder() {
    if let Some(dir) = whir_core::captures::resolve_dir() {
        shell::open_folder(&dir);
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
                let ms = d.linger_ms();
                d.linger(ms);
            }
            WM_LBUTTONDOWN => {
                d.note_done(true);
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
                TIMER_TICK => d.tick(),
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
            WM_NOTE_DONE => d.note_done(wparam.0 == 1),
            WM_POWERBROADCAST => {
                if wparam.0 == PBT_APMRESUMEAUTOMATIC && d.clock.running().is_some() {
                    d.tick();
                }
                drop(d);
                return LRESULT(1);
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
                d.update_panel();
            }
            WM_DESTROY => {
                DOCK.store(0, Ordering::Relaxed);
                if d.hotkey_registered {
                    let _ = UnregisterHotKey(Some(hwnd), HOTKEY);
                }
                d.note = None;
                let _ = DestroyWindow(d.panel.hwnd);
                PostQuitMessage(0);
            }
            _ if msg == ipc::changed_message() => d.shared_changed(Changed::from_code(wparam.0)),
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

/// The panel's window: its own clicks, scrolling and hovering.
unsafe extern "system" fn panel_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        let dock = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const RefCell<Dock>;
        let Some(dock) = dock.as_ref() else {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        };
        match msg {
            WM_RBUTTONUP => {
                let strip = dock.borrow().hwnd;
                menu(dock, strip);
                return LRESULT(0);
            }
            WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE),
            WM_NCDESTROY => {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                return DefWindowProcW(hwnd, msg, wparam, lparam);
            }
            _ => {}
        }
        let Ok(mut d) = dock.try_borrow_mut() else {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        };
        match msg {
            WM_MOUSEMOVE => d.panel_mouse_move(point(lparam)),
            WM_MOUSELEAVE => d.panel_left(),
            WM_MOUSEWHEEL => d.panel_scrolled((wparam.0 >> 16) as u16 as i16 as i32),
            WM_LBUTTONDOWN => {
                d.note_done(true);
                d.panel.pressed = panel::hit_at(&d.panel.items, point(lparam), d.panel.scroll);
                drop(d);
                SetCapture(hwnd);
            }
            WM_LBUTTONUP => {
                d.panel_released(point(lparam));
                drop(d);
                let _ = ReleaseCapture();
            }
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
        let cursor = LoadCursorW(None, IDC_ARROW).unwrap_or_default();
        for (class, proc) in [
            (
                instance::CLASS,
                wndproc as unsafe extern "system" fn(_, _, _, _) -> _,
            ),
            (PANEL_CLASS, panel_wndproc),
        ] {
            RegisterClassExW(&WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(proc),
                hInstance: instance,
                hCursor: cursor,
                lpszClassName: class,
                ..Default::default()
            });
        }
        // Layered: drawn with per-pixel alpha. A tool window that is never
        // activated: not in the taskbar or Alt+Tab, and it takes no focus.
        let style = WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
        let window = |class, title| {
            CreateWindowExW(
                style,
                class,
                title,
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance),
                None,
            )
        };
        let (Ok(hwnd), Ok(panel)) = (
            window(instance::CLASS, w!("whir capture dock")),
            window(PANEL_CLASS, w!("whir dock panel")),
        ) else {
            crate::report("The capture dock couldn't be started.");
            return 1;
        };
        let dock = RefCell::new(Dock::new(hwnd, panel));
        for h in [hwnd, panel] {
            SetWindowLongPtrW(h, GWLP_USERDATA, &dock as *const RefCell<Dock> as isize);
        }
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
            d.settle();
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
            if note_edit::intercept(&msg) {
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        if !hook.is_invalid() {
            let _ = UnhookWinEvent(hook);
        }
        msg.wParam.0 as i32
    }
}
