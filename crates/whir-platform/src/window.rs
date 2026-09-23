//! The main window: custom frame with tabs in the title bar, an event-driven
//! message loop, and Win32 → egui input translation.
//!
//! Re-entrancy: Windows calls the window procedure recursively (modal size/move
//! loops, `SetWindowPos` → `WM_SIZE`, dialogs pumping `WM_PAINT`). All state is
//! in `Cell`/`RefCell`s, the handler is only borrowed with `try_borrow_mut`, and
//! anything that can re-enter is executed *after* the handler borrow is released.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::time::{Duration, Instant};

use egui::{CursorIcon, PointerButton};
use whir_core::Waker;
use windows::Win32::Foundation::{
    FALSE, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, TRUE, WPARAM,
};
use windows::Win32::Graphics::Dwm::{
    DWMWA_CLOAK, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWINDOWATTRIBUTE, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
    PAINTSTRUCT, ScreenToClient,
};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{
    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CreateWaitableTimerExW, INFINITE, SetWaitableTimer,
    TIMER_ALL_ACCESS,
};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::{
    GetDpiForMonitor, GetDpiForWindow, GetSystemMetricsForDpi, MDT_EFFECTIVE_DPI,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetCapture, ReleaseCapture, SetCapture, TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT,
    TrackMouseEvent, VK_CONTROL, VK_F4, VK_F10, VK_MENU, VK_SHIFT, VK_SPACE,
};
use windows::Win32::UI::Shell::{DragAcceptFiles, DragFinish, DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, PCWSTR};

use crate::input::InputCollector;
use crate::ipc::{COPYDATA_OPEN, WINDOW_CLASS, decode_paths};

const WM_APP_WAKE: u32 = WM_APP + 1;
const WM_APP_OPEN: u32 = WM_APP + 2;
const SIZE_MOVE_TIMER: usize = 1;

/// Integer rectangle in physical client pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RectPx {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl RectPx {
    pub fn from_points(r: egui::Rect, ppp: f32) -> Self {
        Self {
            left: (r.min.x * ppp).floor() as i32,
            top: (r.min.y * ppp).floor() as i32,
            right: (r.max.x * ppp).ceil() as i32,
            bottom: (r.max.y * ppp).ceil() as i32,
        }
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }
}

/// Where the title bar lives, published by the app after every frame and used
/// by `WM_NCHITTEST` (which must answer instantly, without running egui).
#[derive(Clone, Debug, Default)]
pub struct HitMap {
    /// Height of the tab strip / caption area in client pixels.
    pub caption_height: i32,
    /// Widgets inside the caption area (tabs, buttons): these get normal client input.
    pub interactive: Vec<RectPx>,
    /// Reported as `HTMAXBUTTON` so Windows 11 shows the snap-layout flyout.
    pub maximize_button: Option<RectPx>,
}

pub enum WindowCommand {
    /// Uncloak the window (after the first frame is on screen).
    Show,
    SetTitle(String),
    Minimize,
    ToggleMaximize,
    Close,
    /// Show the native open dialog; results arrive via [`Handler::open_files`].
    OpenFileDialog {
        title: String,
        filters: Vec<(String, String)>,
    },
    /// Ask the OS to bring us to the foreground (e.g. after receiving files from another process).
    Focus,
    /// Dark or light system frame / border.
    SetDarkMode(bool),
}

pub struct FrameInfo {
    pub size_px: [u32; 2],
    pub pixels_per_point: f32,
    pub maximized: bool,
    pub minimized: bool,
    /// Inside a modal size/move loop: present without vsync to keep up with the mouse.
    pub in_size_move: bool,
}

#[derive(Default)]
pub struct FrameOutput {
    pub cursor: CursorIcon,
    pub copied_text: Option<String>,
    /// `None`: nothing to do until the next input or wake.
    pub repaint_after: Option<Duration>,
    pub hit_map: HitMap,
    pub commands: Vec<WindowCommand>,
}

/// The application side of the window.
pub trait Handler {
    fn frame(&mut self, input: egui::RawInput, info: &FrameInfo) -> FrameOutput;
    fn resized(&mut self, size_px: [u32; 2]);
    fn open_files(&mut self, paths: Vec<PathBuf>);
    fn activated(&mut self, _active: bool) {}
    fn close_requested(&mut self) -> bool {
        true
    }
    fn system_theme_changed(&mut self) {}
}

pub struct WindowOptions {
    pub title: String,
    pub dark: bool,
}

/// Wake target that exists *before* the window, so documents can start loading
/// (holding a [`Waker`]) while the window and GPU are still being created.
/// Wakes before the window exists are dropped; the first frame polls anyway.
#[derive(Clone, Default)]
pub struct WakeSlot {
    hwnd: Arc<AtomicIsize>,
    pending: Arc<AtomicBool>,
}

impl WakeSlot {
    pub fn new() -> Self {
        Self::default()
    }

    /// A thread-safe waker that schedules a repaint; bursts coalesce into one message.
    pub fn waker(&self) -> Waker {
        let slot = self.clone();
        Waker::new(move || {
            let h = slot.hwnd.load(Ordering::Acquire);
            if h == 0 || slot.pending.swap(true, Ordering::AcqRel) {
                return;
            }
            let posted =
                unsafe { PostMessageW(Some(HWND(h as *mut _)), WM_APP_WAKE, WPARAM(0), LPARAM(0)) };
            if posted.is_err() {
                slot.pending.store(false, Ordering::Release);
            }
        })
    }
}

struct State {
    hwnd: Cell<HWND>,
    handler: RefCell<Option<Box<dyn Handler>>>,
    input: RefCell<InputCollector>,
    hit: RefCell<HitMap>,
    pending_files: RefCell<Vec<PathBuf>>,
    repaint_at: Cell<Option<Instant>>,
    size_px: Cell<[u32; 2]>,
    delivered_size: Cell<[u32; 2]>,
    in_size_move: Cell<bool>,
    minimized: Cell<bool>,
    cursor: Cell<CursorIcon>,
    pointer_in_client: Cell<bool>,
    tracking_client_leave: Cell<bool>,
    tracking_nc_leave: Cell<bool>,
    destroyed: Cell<bool>,
    wake: WakeSlot,
}

pub struct Window {
    state: Rc<State>,
}

impl Window {
    /// Create the main window, cloaked (invisible but "shown"), so the first
    /// frame can be rendered before anything appears on screen.
    pub fn create(opts: &WindowOptions, wake: WakeSlot) -> windows::core::Result<Self> {
        let state = Rc::new(State {
            hwnd: Cell::new(HWND::default()),
            handler: RefCell::new(None),
            input: RefCell::new(InputCollector::new(1.0)),
            hit: RefCell::new(HitMap::default()),
            pending_files: RefCell::new(Vec::new()),
            repaint_at: Cell::new(Some(Instant::now())),
            size_px: Cell::new([1, 1]),
            delivered_size: Cell::new([0, 0]),
            in_size_move: Cell::new(false),
            minimized: Cell::new(false),
            cursor: Cell::new(CursorIcon::Default),
            pointer_in_client: Cell::new(false),
            tracking_client_leave: Cell::new(false),
            tracking_nc_leave: Cell::new(false),
            destroyed: Cell::new(false),
            wake,
        });

        unsafe {
            let instance: HINSTANCE = GetModuleHandleW(PCWSTR::null())?.into();
            let class_name = HSTRING::from(WINDOW_CLASS);
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                // No CS_DBLCLKS: egui detects double clicks itself. No background
                // brush: we paint everything, so there's never a white flash.
                style: WNDCLASS_STYLES(0),
                lpfnWndProc: Some(wndproc),
                hInstance: instance,
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                lpszClassName: PCWSTR(class_name.as_ptr()),
                hIcon: LoadIconW(Some(instance), PCWSTR(1 as _)).unwrap_or_default(),
                ..Default::default()
            };
            RegisterClassExW(&wc);
            whir_core::trace::mark("window: class registered");

            let (x, y, w, h) = initial_placement();
            let raw = Rc::into_raw(state.clone());
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                &class_name,
                &HSTRING::from(opts.title.as_str()),
                WS_OVERLAPPEDWINDOW,
                x,
                y,
                w,
                h,
                None,
                None,
                Some(instance),
                Some(raw as *const _),
            );
            whir_core::trace::mark("window: CreateWindowExW returned");
            let hwnd = match hwnd {
                Ok(h) => h,
                Err(e) => {
                    drop(Rc::from_raw(raw));
                    return Err(e);
                }
            };
            state.wake.hwnd.store(hwnd.0 as isize, Ordering::Release);
            crate::clipboard::set_owner(hwnd);

            set_bool_attribute(hwnd, DWMWA_CLOAK, true);
            set_bool_attribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, opts.dark);
            let ppp = GetDpiForWindow(hwnd) as f32 / 96.0;
            state.input.borrow_mut().pixels_per_point = ppp;
            // Re-run WM_NCCALCSIZE with our custom frame.
            let _ = SetWindowPos(
                hwnd,
                None,
                0,
                0,
                0,
                0,
                SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
            whir_core::trace::mark("window: frame changed");
            DragAcceptFiles(hwnd, true);
            whir_core::trace::mark("window: drag-drop registered");
            // Shown while cloaked: DXGI can present to it and it can take the
            // foreground, but nothing is visible until WindowCommand::Show.
            let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            state.size_px.set([
                (rc.right - rc.left).max(1) as u32,
                (rc.bottom - rc.top).max(1) as u32,
            ]);
        }
        Ok(Self { state })
    }

    pub fn hwnd(&self) -> HWND {
        self.state.hwnd.get()
    }

    pub fn size_px(&self) -> [u32; 2] {
        self.state.size_px.get()
    }

    pub fn pixels_per_point(&self) -> f32 {
        self.state.input.borrow().pixels_per_point
    }

    /// Reveal the window right away with a flat placeholder in the app's
    /// colours. Used when the GPU driver is slow to initialize, so the window
    /// still appears immediately; the first real frame replaces it.
    pub fn show_placeholder(
        &self,
        strip: egui::Color32,
        content: egui::Color32,
        strip_height_px: i32,
    ) {
        use windows::Win32::Foundation::COLORREF;
        use windows::Win32::Graphics::Gdi::{
            CreateSolidBrush, DeleteObject, FillRect, GetDC, ReleaseDC,
        };
        let hwnd = self.hwnd();
        let rgb =
            |c: egui::Color32| COLORREF(c.r() as u32 | (c.g() as u32) << 8 | (c.b() as u32) << 16);
        unsafe {
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let hdc = GetDC(Some(hwnd));
            let top = RECT {
                bottom: rc.top + strip_height_px,
                ..rc
            };
            let rest = RECT {
                top: rc.top + strip_height_px,
                ..rc
            };
            for (r, c) in [(top, strip), (rest, content)] {
                let brush = CreateSolidBrush(rgb(c));
                FillRect(hdc, &r, brush);
                let _ = DeleteObject(brush.into());
            }
            ReleaseDC(Some(hwnd), hdc);
        }
        set_bool_attribute(hwnd, DWMWA_CLOAK, false);
    }

    /// Run the event loop until the window closes. Returns the exit code.
    pub fn run(self, handler: Box<dyn Handler>) -> i32 {
        let state = self.state;
        *state.handler.borrow_mut() = Some(handler);
        state.deliver_pending_files();
        state.request_repaint();

        let timer = unsafe {
            CreateWaitableTimerExW(
                None,
                PCWSTR::null(),
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                TIMER_ALL_ACCESS.0,
            )
            .or_else(|_| CreateWaitableTimerExW(None, PCWSTR::null(), 0, TIMER_ALL_ACCESS.0))
            .ok()
        };

        let mut msg = MSG::default();
        loop {
            unsafe {
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    if msg.message == WM_QUIT {
                        if let Some(t) = timer {
                            let _ = windows::Win32::Foundation::CloseHandle(t);
                        }
                        return msg.wParam.0 as i32;
                    }
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            if state.destroyed.get() {
                continue; // WM_QUIT is on its way
            }
            let now = Instant::now();
            match state.repaint_at.get() {
                Some(at) if at <= now => {
                    state.do_frame();
                    continue;
                }
                Some(at) => unsafe {
                    let wait = at - now;
                    let handles: Vec<HANDLE> = match timer {
                        Some(t) => {
                            // Negative = relative, in 100ns units.
                            let due = -((wait.as_nanos() / 100).max(1) as i64);
                            let _ = SetWaitableTimer(t, &due, 0, None, None, false);
                            vec![t]
                        }
                        None => vec![],
                    };
                    let ms = if timer.is_some() {
                        INFINITE
                    } else {
                        wait.as_millis().max(1) as u32
                    };
                    MsgWaitForMultipleObjectsEx(
                        Some(&handles),
                        ms,
                        QS_ALLINPUT,
                        MWMO_INPUTAVAILABLE,
                    );
                },
                None => unsafe {
                    MsgWaitForMultipleObjectsEx(None, INFINITE, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
                },
            }
        }
    }
}

fn set_bool_attribute(hwnd: HWND, attr: DWMWINDOWATTRIBUTE, value: bool) {
    let v = if value { TRUE } else { FALSE };
    unsafe {
        let _ = DwmSetWindowAttribute(hwnd, attr, &v as *const _ as *const _, 4);
    }
}

/// Centered on the monitor under the cursor, sized to most of its work area.
fn initial_placement() -> (i32, i32, i32, i32) {
    unsafe {
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(mon, &mut mi).as_bool() {
            return (CW_USEDEFAULT, CW_USEDEFAULT, CW_USEDEFAULT, CW_USEDEFAULT);
        }
        let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
        let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        let scale = dpi_x as f32 / 96.0;
        let wa = mi.rcWork;
        let (ww, wh) = (wa.right - wa.left, wa.bottom - wa.top);
        let w = ((ww as f32 * 0.72) as i32)
            .max((900.0 * scale) as i32)
            .min(ww);
        let h = ((wh as f32 * 0.82) as i32)
            .max((620.0 * scale) as i32)
            .min(wh);
        (wa.left + (ww - w) / 2, wa.top + (wh - h) / 2, w, h)
    }
}

impl State {
    fn hwnd(&self) -> HWND {
        self.hwnd.get()
    }

    fn request_repaint(&self) {
        self.repaint_at.set(Some(Instant::now()));
    }

    fn schedule_repaint(&self, after: Duration) {
        let at = Instant::now() + after;
        match self.repaint_at.get() {
            Some(existing) if existing <= at => {}
            _ => self.repaint_at.set(Some(at)),
        }
    }

    fn is_maximized(&self) -> bool {
        unsafe { IsZoomed(self.hwnd()).as_bool() }
    }

    /// Run `f` on the handler unless it is already borrowed further up the stack.
    fn with_handler<R>(&self, f: impl FnOnce(&mut dyn Handler) -> R) -> Option<R> {
        let mut guard = self.handler.try_borrow_mut().ok()?;
        let handler = guard.as_mut()?;
        Some(f(handler.as_mut()))
    }

    fn queue_files(&self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        self.pending_files.borrow_mut().extend(paths);
        unsafe {
            let _ = PostMessageW(Some(self.hwnd()), WM_APP_OPEN, WPARAM(0), LPARAM(0));
        }
    }

    fn deliver_pending_files(&self) {
        if self.pending_files.borrow().is_empty() {
            return;
        }
        let paths = std::mem::take(&mut *self.pending_files.borrow_mut());
        let delivered = self.with_handler(|h| h.open_files(paths.clone())).is_some();
        if !delivered {
            self.pending_files.borrow_mut().extend(paths);
        }
        self.request_repaint();
    }

    fn do_frame(&self) {
        if self.destroyed.get() {
            return;
        }
        let size = self.size_px.get();
        let Ok(mut guard) = self.handler.try_borrow_mut() else {
            self.request_repaint();
            return;
        };
        let Some(handler) = guard.as_mut() else {
            return;
        };
        self.repaint_at.set(None);
        if self.delivered_size.get() != size {
            self.delivered_size.set(size);
            handler.resized(size);
        }
        let (raw, ppp) = {
            let mut input = self.input.borrow_mut();
            (input.take_raw_input(size), input.pixels_per_point)
        };
        self.wake.pending.store(false, Ordering::Release);
        let info = FrameInfo {
            size_px: size,
            pixels_per_point: ppp,
            maximized: self.is_maximized(),
            minimized: self.minimized.get(),
            in_size_move: self.in_size_move.get(),
        };
        let out = handler.frame(raw, &info);
        drop(guard);
        self.apply_output(out);
    }

    fn apply_output(&self, out: FrameOutput) {
        if out.cursor != self.cursor.get() {
            self.cursor.set(out.cursor);
            if self.pointer_in_client.get() || unsafe { GetCapture() } == self.hwnd() {
                set_cursor(out.cursor);
            }
        }
        if let Some(text) = out.copied_text {
            crate::clipboard::set_text(&text);
        }
        if let Some(after) = out.repaint_after {
            self.schedule_repaint(after);
        }
        *self.hit.borrow_mut() = out.hit_map;
        for cmd in out.commands {
            self.execute(cmd);
        }
    }

    fn execute(&self, cmd: WindowCommand) {
        let hwnd = self.hwnd();
        unsafe {
            match cmd {
                WindowCommand::Show => {
                    set_bool_attribute(hwnd, DWMWA_CLOAK, false);
                }
                WindowCommand::SetTitle(title) => {
                    let _ = SetWindowTextW(hwnd, &HSTRING::from(title));
                }
                WindowCommand::Minimize => {
                    let _ = ShowWindow(hwnd, SW_MINIMIZE);
                }
                WindowCommand::ToggleMaximize => {
                    let cmd = if self.is_maximized() {
                        SW_RESTORE
                    } else {
                        SW_MAXIMIZE
                    };
                    let _ = ShowWindow(hwnd, cmd);
                }
                WindowCommand::Close => {
                    let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
                WindowCommand::OpenFileDialog { title, filters } => {
                    let filters: Vec<(&str, &str)> = filters
                        .iter()
                        .map(|(a, b)| (a.as_str(), b.as_str()))
                        .collect();
                    let paths = crate::dialogs::open_files(hwnd, &title, &filters);
                    self.queue_files(paths);
                }
                WindowCommand::Focus => {
                    if IsIconic(hwnd).as_bool() {
                        let _ = ShowWindow(hwnd, SW_RESTORE);
                    }
                    let _ = SetForegroundWindow(hwnd);
                }
                WindowCommand::SetDarkMode(dark) => {
                    set_bool_attribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, dark);
                }
            }
        }
    }

    // --- custom frame ---------------------------------------------------------

    fn frame_thickness(&self) -> (i32, i32) {
        unsafe {
            let dpi = GetDpiForWindow(self.hwnd());
            let size = GetSystemMetricsForDpi(SM_CYSIZEFRAME, dpi);
            let pad = GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi);
            (size, size + pad)
        }
    }

    fn hit_test(&self, screen_x: i32, screen_y: i32) -> u32 {
        let mut pt = POINT {
            x: screen_x,
            y: screen_y,
        };
        unsafe {
            let _ = ScreenToClient(self.hwnd(), &mut pt);
        }
        let (resize, _) = self.frame_thickness();
        let maximized = self.is_maximized();
        if !maximized && pt.y < resize {
            let width = self.size_px.get()[0] as i32;
            let corner = resize * 3;
            return if pt.x < corner {
                HTTOPLEFT
            } else if pt.x >= width - corner {
                HTTOPRIGHT
            } else {
                HTTOP
            };
        }
        let Ok(hit) = self.hit.try_borrow() else {
            return HTCLIENT;
        };
        if pt.y < hit.caption_height {
            if hit.maximize_button.is_some_and(|r| r.contains(pt.x, pt.y)) {
                return HTMAXBUTTON;
            }
            if hit.interactive.iter().any(|r| r.contains(pt.x, pt.y)) {
                return HTCLIENT;
            }
            return HTCAPTION;
        }
        HTCLIENT
    }

    fn cursor_in_client_area(&self) -> Option<(i32, i32)> {
        unsafe {
            let mut pt = POINT::default();
            GetCursorPos(&mut pt).ok()?;
            let (sx, sy) = (pt.x, pt.y);
            let _ = ScreenToClient(self.hwnd(), &mut pt);
            let [w, h] = self.size_px.get();
            let inside = pt.x >= 0 && pt.y >= 0 && pt.x < w as i32 && pt.y < h as i32;
            (inside && WindowFromPoint(POINT { x: sx, y: sy }) == self.hwnd())
                .then_some((pt.x, pt.y))
        }
    }

    // --- mouse ------------------------------------------------------------------

    fn track_leave(&self, nonclient: bool) {
        let flag = if nonclient {
            &self.tracking_nc_leave
        } else {
            &self.tracking_client_leave
        };
        if flag.get() {
            return;
        }
        let mut tme = TRACKMOUSEEVENT {
            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: if nonclient {
                TME_LEAVE | TME_NONCLIENT
            } else {
                TME_LEAVE
            },
            hwndTrack: self.hwnd(),
            dwHoverTime: 0,
        };
        if unsafe { TrackMouseEvent(&mut tme) }.is_ok() {
            flag.set(true);
        }
    }

    fn mouse_move(&self, x: i32, y: i32) {
        self.track_leave(false);
        if !self.pointer_in_client.replace(true) {
            set_cursor(self.cursor.get());
        }
        self.input.borrow_mut().pointer_moved(x, y);
        self.request_repaint();
    }

    fn mouse_button(&self, x: i32, y: i32, button: PointerButton, pressed: bool) {
        let hwnd = self.hwnd();
        if pressed {
            unsafe { SetCapture(hwnd) };
        }
        let any_down = {
            let mut input = self.input.borrow_mut();
            input.pointer_button(x, y, button, pressed);
            input.buttons_down()
        };
        if !pressed && !any_down && unsafe { GetCapture() } == hwnd {
            unsafe {
                let _ = ReleaseCapture();
            }
        }
        self.request_repaint();
    }

    fn pointer_left(&self) {
        // Leaving the client area into our own caption (still inside the
        // window) is reported by the NC handlers; only report real exits.
        if let Some((x, y)) = self.cursor_in_client_area() {
            let _ = (x, y);
            let mut screen = POINT::default();
            unsafe {
                let _ = GetCursorPos(&mut screen);
            }
            if self.hit_test(screen.x, screen.y) != HTCLIENT {
                return;
            }
        }
        self.pointer_in_client.set(false);
        self.input.borrow_mut().pointer_gone();
        self.request_repaint();
    }
}

fn set_cursor(icon: CursorIcon) {
    let id = match icon {
        CursorIcon::None => {
            unsafe { SetCursor(None) };
            return;
        }
        CursorIcon::PointingHand => IDC_HAND,
        CursorIcon::Text | CursorIcon::VerticalText => IDC_IBEAM,
        CursorIcon::Crosshair | CursorIcon::Cell => IDC_CROSS,
        CursorIcon::Move | CursorIcon::AllScroll | CursorIcon::Grab | CursorIcon::Grabbing => {
            IDC_SIZEALL
        }
        CursorIcon::NotAllowed | CursorIcon::NoDrop => IDC_NO,
        CursorIcon::Wait => IDC_WAIT,
        CursorIcon::Progress => IDC_APPSTARTING,
        CursorIcon::Help => IDC_HELP,
        CursorIcon::ResizeHorizontal
        | CursorIcon::ResizeColumn
        | CursorIcon::ResizeEast
        | CursorIcon::ResizeWest => IDC_SIZEWE,
        CursorIcon::ResizeVertical
        | CursorIcon::ResizeRow
        | CursorIcon::ResizeNorth
        | CursorIcon::ResizeSouth => IDC_SIZENS,
        CursorIcon::ResizeNeSw | CursorIcon::ResizeNorthEast | CursorIcon::ResizeSouthWest => {
            IDC_SIZENESW
        }
        CursorIcon::ResizeNwSe | CursorIcon::ResizeNorthWest | CursorIcon::ResizeSouthEast => {
            IDC_SIZENWSE
        }
        _ => IDC_ARROW,
    };
    unsafe {
        if let Ok(c) = LoadCursorW(None, id) {
            SetCursor(Some(c));
        }
    }
}

#[inline]
fn loword(v: isize) -> i32 {
    (v & 0xFFFF) as u16 as i32
}

#[inline]
fn hiword(v: isize) -> i32 {
    ((v >> 16) & 0xFFFF) as u16 as i32
}

/// Signed coordinates packed in an LPARAM (GET_X_LPARAM / GET_Y_LPARAM).
#[inline]
fn xy(lparam: LPARAM) -> (i32, i32) {
    (
        (lparam.0 & 0xFFFF) as i16 as i32,
        ((lparam.0 >> 16) & 0xFFFF) as i16 as i32,
    )
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_NCCREATE {
            let cs = &*(lparam.0 as *const CREATESTRUCTW);
            let ptr = cs.lpCreateParams as *const State;
            (*ptr).hwnd.set(hwnd);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr as isize);
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const State;
        if ptr.is_null() {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        if msg == WM_NCDESTROY {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            let state = Rc::from_raw(ptr);
            state.destroyed.set(true);
            state.wake.hwnd.store(0, Ordering::Release);
            drop(state);
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        handle(&*ptr, hwnd, msg, wparam, lparam)
    }
}

unsafe fn handle(s: &State, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        let def = || DefWindowProcW(hwnd, msg, wparam, lparam);
        match msg {
            WM_NCCALCSIZE if wparam.0 != 0 => {
                // Remove the caption but keep the standard (invisible) resize
                // borders on the left, right and bottom.
                let params = &mut *(lparam.0 as *mut NCCALCSIZE_PARAMS);
                let top = params.rgrc[0].top;
                let r = def();
                if r.0 != 0 {
                    return r;
                }
                params.rgrc[0].top = top;
                if s.is_maximized() {
                    // Maximized windows overhang the monitor by the frame thickness.
                    params.rgrc[0].top += s.frame_thickness().1;
                }
                LRESULT(0)
            }
            WM_NCHITTEST => {
                let r = def();
                if r.0 as u32 != HTCLIENT {
                    return r;
                }
                let (x, y) = xy(lparam);
                LRESULT(s.hit_test(x, y) as isize)
            }
            WM_NCACTIVATE => {
                // lParam = -1 stops DefWindowProc from repainting the (hidden) caption.
                s.request_repaint();
                DefWindowProcW(hwnd, msg, wparam, LPARAM(-1))
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                BeginPaint(hwnd, &mut ps);
                let _ = EndPaint(hwnd, &ps);
                if s.in_size_move.get() {
                    s.do_frame();
                } else {
                    s.request_repaint();
                }
                LRESULT(0)
            }
            WM_SIZE => {
                if wparam.0 as u32 == SIZE_MINIMIZED {
                    s.minimized.set(true);
                    return LRESULT(0);
                }
                s.minimized.set(false);
                let (w, h) = (loword(lparam.0), hiword(lparam.0));
                s.size_px.set([w.max(1) as u32, h.max(1) as u32]);
                // Render right away: during a modal resize our loop isn't running.
                s.do_frame();
                LRESULT(0)
            }
            WM_ENTERSIZEMOVE => {
                s.in_size_move.set(true);
                SetTimer(Some(hwnd), SIZE_MOVE_TIMER, 16, None);
                LRESULT(0)
            }
            WM_EXITSIZEMOVE => {
                s.in_size_move.set(false);
                let _ = KillTimer(Some(hwnd), SIZE_MOVE_TIMER);
                s.request_repaint();
                LRESULT(0)
            }
            WM_TIMER if wparam.0 == SIZE_MOVE_TIMER => {
                // Animations and worker results while the modal loop owns the thread.
                if s.repaint_at.get().is_some_and(|t| t <= Instant::now()) {
                    s.do_frame();
                }
                LRESULT(0)
            }
            WM_DPICHANGED => {
                let dpi = hiword(wparam.0 as isize) as f32;
                s.input.borrow_mut().pixels_per_point = dpi / 96.0;
                let r = &*(lparam.0 as *const RECT);
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                s.request_repaint();
                LRESULT(0)
            }
            WM_GETMINMAXINFO => {
                let mmi = &mut *(lparam.0 as *mut MINMAXINFO);
                let scale = GetDpiForWindow(hwnd).max(96) as f32 / 96.0;
                mmi.ptMinTrackSize = POINT {
                    x: (420.0 * scale) as i32,
                    y: (280.0 * scale) as i32,
                };
                LRESULT(0)
            }
            WM_SETCURSOR => {
                if loword(lparam.0) as u32 == HTCLIENT {
                    set_cursor(s.cursor.get());
                    LRESULT(1)
                } else {
                    def()
                }
            }

            // --- mouse (client) ---
            WM_MOUSEMOVE => {
                let (x, y) = xy(lparam);
                s.mouse_move(x, y);
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                s.tracking_client_leave.set(false);
                s.pointer_left();
                LRESULT(0)
            }
            WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP | WM_MBUTTONDOWN
            | WM_MBUTTONUP | WM_XBUTTONDOWN | WM_XBUTTONUP => {
                let (x, y) = xy(lparam);
                let (button, pressed) = match msg {
                    WM_LBUTTONDOWN => (PointerButton::Primary, true),
                    WM_LBUTTONUP => (PointerButton::Primary, false),
                    WM_RBUTTONDOWN => (PointerButton::Secondary, true),
                    WM_RBUTTONUP => (PointerButton::Secondary, false),
                    WM_MBUTTONDOWN => (PointerButton::Middle, true),
                    WM_MBUTTONUP => (PointerButton::Middle, false),
                    _ => {
                        let which = hiword(wparam.0 as isize) as u16;
                        let b = if which == XBUTTON1 {
                            PointerButton::Extra1
                        } else {
                            PointerButton::Extra2
                        };
                        (b, msg == WM_XBUTTONDOWN)
                    }
                };
                s.mouse_button(x, y, button, pressed);
                if matches!(msg, WM_XBUTTONDOWN | WM_XBUTTONUP) {
                    LRESULT(1)
                } else {
                    LRESULT(0)
                }
            }
            WM_CAPTURECHANGED => {
                if HWND(lparam.0 as *mut _) != hwnd {
                    s.input.borrow_mut().release_all_buttons();
                    s.request_repaint();
                }
                LRESULT(0)
            }
            WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
                let delta = hiword(wparam.0 as isize) as i16 as f32 / WHEEL_DELTA as f32;
                if msg == WM_MOUSEWHEEL {
                    s.input.borrow_mut().wheel(0.0, delta);
                } else {
                    s.input.borrow_mut().wheel(-delta, 0.0);
                }
                s.request_repaint();
                LRESULT(0)
            }

            // --- mouse over our custom maximize button (non-client) ---
            WM_NCMOUSEMOVE => {
                let (x, y) = xy(lparam);
                let mut pt = POINT { x, y };
                let _ = ScreenToClient(hwnd, &mut pt);
                s.track_leave(true);
                s.pointer_in_client.set(false);
                s.input.borrow_mut().pointer_moved(pt.x, pt.y);
                s.request_repaint();
                def()
            }
            WM_NCMOUSELEAVE => {
                s.tracking_nc_leave.set(false);
                if s.cursor_in_client_area().is_none() {
                    s.input.borrow_mut().pointer_gone();
                    s.request_repaint();
                }
                LRESULT(0)
            }
            WM_NCLBUTTONDOWN | WM_NCLBUTTONDBLCLK | WM_NCLBUTTONUP
                if wparam.0 as u32 == HTMAXBUTTON =>
            {
                // Never let DefWindowProc see these: it would draw a classic
                // button and maximize on mouse-down. egui handles the click.
                let (x, y) = xy(lparam);
                let mut pt = POINT { x, y };
                let _ = ScreenToClient(hwnd, &mut pt);
                let pressed = msg != WM_NCLBUTTONUP;
                s.input
                    .borrow_mut()
                    .pointer_button(pt.x, pt.y, PointerButton::Primary, pressed);
                s.request_repaint();
                LRESULT(0)
            }
            WM_NCRBUTTONDOWN | WM_NCRBUTTONUP if wparam.0 as u32 == HTMAXBUTTON => LRESULT(0),

            // --- keyboard ---
            WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP => {
                let vk = wparam.0 as u16;
                let pressed = matches!(msg, WM_KEYDOWN | WM_SYSKEYDOWN);
                let repeat = pressed && (lparam.0 >> 30) & 1 != 0;
                {
                    let mut input = s.input.borrow_mut();
                    if matches!(vk, v if v == VK_SHIFT.0 || v == VK_CONTROL.0 || v == VK_MENU.0) {
                        input.modifier_key();
                    } else {
                        input.key(vk, pressed, repeat);
                    }
                }
                s.request_repaint();
                // Alt+F4 must still close; everything else (incl. F10 / lone Alt
                // entering the menu loop) is ours.
                if matches!(msg, WM_SYSKEYDOWN | WM_SYSKEYUP) && vk == VK_F4.0 {
                    return def();
                }
                if vk == VK_F10.0 || vk == VK_MENU.0 {
                    return LRESULT(0);
                }
                if matches!(msg, WM_SYSKEYDOWN | WM_SYSKEYUP) {
                    return LRESULT(0);
                }
                LRESULT(0)
            }
            WM_SYSCHAR => {
                // Alt+Space opens the system menu; swallow other Alt+key beeps.
                if wparam.0 as u16 == VK_SPACE.0 {
                    def()
                } else {
                    LRESULT(0)
                }
            }
            WM_CHAR => {
                s.input.borrow_mut().char(wparam.0 as u16);
                s.request_repaint();
                LRESULT(0)
            }
            WM_SETFOCUS | WM_KILLFOCUS => {
                s.input.borrow_mut().focus(msg == WM_SETFOCUS);
                s.request_repaint();
                LRESULT(0)
            }
            WM_ACTIVATEAPP => {
                let active = wparam.0 != 0;
                s.with_handler(|h| h.activated(active));
                s.request_repaint();
                LRESULT(0)
            }
            WM_SETTINGCHANGE => {
                let area = lparam.0 as *const u16;
                if !area.is_null()
                    && PCWSTR(area)
                        .to_string()
                        .is_ok_and(|a| a == "ImmersiveColorSet")
                {
                    s.with_handler(|h| h.system_theme_changed());
                    s.request_repaint();
                }
                def()
            }

            // --- files ---
            WM_COPYDATA => {
                let cds = &*(lparam.0 as *const COPYDATASTRUCT);
                if cds.dwData != COPYDATA_OPEN {
                    return LRESULT(0);
                }
                // Copy out and return quickly so the sender isn't held up.
                if !cds.lpData.is_null() && cds.cbData >= 2 {
                    let units = std::slice::from_raw_parts(
                        cds.lpData as *const u16,
                        cds.cbData as usize / 2,
                    );
                    s.queue_files(decode_paths(units));
                }
                // The sender granted us foreground rights (AllowSetForegroundWindow).
                s.execute(WindowCommand::Focus);
                LRESULT(1)
            }
            WM_DROPFILES => {
                let hdrop = HDROP(wparam.0 as *mut _);
                let count = DragQueryFileW(hdrop, u32::MAX, None);
                let mut paths = Vec::with_capacity(count as usize);
                for i in 0..count {
                    let len = DragQueryFileW(hdrop, i, None) as usize;
                    let mut buf = vec![0u16; len + 1];
                    let n = DragQueryFileW(hdrop, i, Some(&mut buf)) as usize;
                    paths.push(PathBuf::from(String::from_utf16_lossy(&buf[..n])));
                }
                DragFinish(hdrop);
                s.queue_files(paths);
                LRESULT(0)
            }
            WM_APP_OPEN => {
                s.deliver_pending_files();
                LRESULT(0)
            }
            WM_APP_WAKE => {
                s.wake.pending.store(false, Ordering::Release);
                if s.in_size_move.get() {
                    s.do_frame();
                } else {
                    s.request_repaint();
                }
                LRESULT(0)
            }

            // --- lifetime ---
            WM_CLOSE => {
                // Files handed over by another whir.exe after the close was queued
                // (e.g. the last tab was closed as a new file was double-clicked):
                // open them instead of closing, or they'd be lost.
                if !s.pending_files.borrow().is_empty() {
                    s.deliver_pending_files();
                    return LRESULT(0);
                }
                if s.with_handler(|h| h.close_requested()).unwrap_or(true) {
                    let _ = DestroyWindow(hwnd);
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => def(),
        }
    }
}
