//! The dock: a small bar on an edge of the screen. Only a grip shows until
//! the pointer rests on it; then it slides out: Capture, Delay, and a
//! stopwatch (▶ / ⏸, today's time on the milestone to the second, the
//! project over the milestone, which opens a picker) with a ⌄ that opens
//! today's times. It can be dragged to any edge of any monitor, and
//! remembers where.
//!
//! Under it, a panel (a second window) shows what must be seen: the todos
//! (notes flagged in whir), the "were you away?" question, "switch
//! milestones?", and today's times or the picker when open. While there is
//! something to show, or a timer runs, the dock stays out.
//!
//! It never takes the keyboard or becomes the active window: clicking it must
//! not disturb whatever is about to be captured (an open menu, a tooltip).
//! (Only its menus do, as Windows' own do.)
//!
//! What's on it is drawn with egui, in the window's look, by [`crate::stage`];
//! this is the windows around that: where they are, when they move, what's
//! pressed. Shadows have windows of their own, which clicks pass through.
//!
//! It stays above every other window, full-screen ones included. Being
//! topmost isn't enough for that: whichever topmost window came forward last
//! is above the rest, and a window that is never activated is never brought
//! forward. So it puts itself back on top whenever another window becomes
//! the active one, and every couple of seconds for those that appear without
//! being activated.

use std::cell::RefCell;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::time::Duration;

use std::path::PathBuf;

use egui::CornerRadius;
use whir_core::icons::Icon;
use whir_core::theme::Theme;
use whir_core::time::Stamp;
use whir_core::todos;
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
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, EVENT_SYSTEM_FOREGROUND,
    GWLP_USERDATA, GetCursorPos, GetMessageW, GetSystemMetrics, GetWindowLongPtrW, GetWindowRect,
    HWND_TOPMOST, IDC_ARROW, IsIconic, KillTimer, LoadCursorW, MSG, PM_REMOVE, PeekMessageW,
    PostMessageW, PostQuitMessage, RegisterClassExW, SM_CXDRAG, SPI_GETCLIENTAREAANIMATION,
    SW_HIDE, SW_RESTORE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SetForegroundWindow, SetTimer, SetWindowLongPtrW,
    SetWindowPos, ShowWindow, ShowWindowAsync, SystemParametersInfoW, TranslateMessage, ULW_ALPHA,
    UpdateLayeredWindow, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_CLOSE, WM_CONTEXTMENU,
    WM_DESTROY, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_HOTKEY, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCDESTROY, WM_POWERBROADCAST, WM_RBUTTONUP,
    WM_SETTINGCHANGE, WM_TIMER, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::{BOOL, PCWSTR, w};

use crate::Command;
use crate::bar::{self, Bar};
use crate::clock::Clock;
use crate::config::{Ask, DELAYS, DockConfig, Hotkey, Spot, format_hotkey};
use crate::geom::{Point, Rect};
use crate::instance::{self, Role, WM_LAUNCHED};
use crate::menu::Entry;
use crate::monitors::{self, Monitor};
use crate::paint::{self, Shadow, Surface};
use crate::panel::{self, Hit, View};
use crate::place::{self, Edge, Layout, Slot};
use crate::stage::{Drew, Stage};
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
/// While a timer runs: the stopwatch, and whether the user is away.
const TIMER_TICK: usize = 6;
/// The next frame of a fade.
const TIMER_BAR_FRAME: usize = 7;
const TIMER_PANEL_FRAME: usize = 8;
/// The panel coming in.
const TIMER_PANEL_FADE: usize = 9;
const HOTKEY: i32 = 1;

/// Milliseconds the pointer rests on the grip before the dock opens, and how
/// long it stays open after the pointer leaves (as the notes drawer does).
const HOVER_MS: u32 = 150;
const LINGER_MS: u32 = 400;
/// The panel stays a little longer: it has more to aim at.
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
/// The stopwatch shows seconds.
const TICK_MS: u32 = 1000;
/// How the panel fades in behind the slide: a step every so many milliseconds.
const PANEL_FADE_MS: u32 = 16;
const PANEL_FADE_STEP: u8 = 51;
/// Todos listed on the panel (the rest are counted).
const TODOS_SHOWN: usize = 3;

/// The dock's window, for the foreground hook (it has no other way to it).
static DOCK: AtomicIsize = AtomicIsize::new(0);

const PANEL_CLASS: PCWSTR = w!("whir.dock.panel.v1");
const SHADE_CLASS: PCWSTR = w!("whir.dock.shade.v1");

/// How far a shadow reaches, in points: its blur, and its drop.
const SHADOW_REACH: f32 = 22.0;

/// A shadow, on a window of its own that clicks pass through. A layered window
/// catches the mouse wherever it isn't fully clear, so a shadow drawn on the
/// dock's own windows would be a dead strip around them, all day.
struct Shade {
    hwnd: HWND,
    /// What it's a picture of, as last drawn.
    made: Option<((i32, i32), Shadow)>,
    picture: Option<Surface>,
    shown: bool,
}

impl Shade {
    fn new(hwnd: HWND) -> Shade {
        Shade {
            hwnd,
            made: None,
            picture: None,
            shown: false,
        }
    }

    /// `rect` of the picture, from `from`, at `alpha`.
    fn show(&mut self, size: (i32, i32), s: Shadow, rect: Rect, from: Point, alpha: u8) {
        if self.made != Some((size, s)) {
            self.picture = paint::shadow(size, s);
            self.made = Some((size, s));
        }
        if let Some(picture) = &self.picture {
            present_layered(self.hwnd, picture, rect, from, alpha);
        }
        if !self.shown {
            self.shown = true;
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            }
        }
    }

    fn hide(&mut self) {
        if std::mem::take(&mut self.shown) {
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
        }
    }
}

struct Dock {
    hwnd: HWND,
    config: DockConfig,
    monitors: Vec<Monitor>,
    monitor: usize,
    edge: Edge,
    offset: u16,
    layout: Layout,
    theme: Theme,
    /// The whole strip, drawn; the window shows a slice of it.
    stage: Stage,
    shade: Shade,
    /// Pixels of the strip's depth showing: from the grip to all of it.
    visible: i32,
    open: bool,
    /// Where the button went down, on screen.
    pressed: Option<Point>,
    /// Clicked in the last frame.
    clicked: Option<Slot>,
    dragging: bool,
    tracking: bool,
    /// Seconds left before a delayed capture.
    countdown: Option<u32>,
    capturing: bool,
    menu_open: bool,
    hotkey_registered: bool,
    tray: Tray,
    /// What the tray icon's tooltip says now.
    tip: String,

    panel: Panel,
    /// Today's times or the picker, open.
    view: Option<View>,
    /// A milestone picked while another runs: "switch?".
    confirm: Option<u32>,
    clock: Clock,
    /// The open todos: title, note, flagged since. Newest first.
    todos: Vec<(String, PathBuf, Stamp)>,
}

/// The panel's window and what is on it.
struct Panel {
    hwnd: HWND,
    stage: Stage,
    shade: Shade,
    items: Vec<panel::Item>,
    /// On screen; empty while hidden.
    rect: Rect,
    /// It is below the strip (the ▾ points at it).
    below: bool,
    corners: CornerRadius,
    /// Clicked in the last frame.
    clicked: Option<Hit>,
    tracking: bool,
    /// Start at the top again, in the next frame.
    from_top: bool,
    /// How far it has faded in.
    alpha: u8,
}

fn cursor() -> Point {
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    (p.x, p.y)
}

fn theme() -> Theme {
    if whir_platform::os::prefers_dark_on_disk() {
        Theme::dark()
    } else {
        Theme::light()
    }
}

/// Windows hasn't been told to do without animations.
fn animations_on() -> bool {
    let mut on = BOOL(1);
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            Some(&mut on as *mut BOOL as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
    }
    on.as_bool()
}

/// Put `surface` on `hwnd`'s screen at `rect`, from `from` in the picture.
pub fn present_layered(hwnd: HWND, surface: &Surface, rect: Rect, from: Point, alpha: u8) {
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
    fn new(hwnd: HWND, panel: HWND, shades: [HWND; 2]) -> Dock {
        let theme = theme();
        let mut dock = Dock {
            hwnd,
            config: DockConfig::load(),
            monitors: Vec::new(),
            monitor: 0,
            edge: Edge::Left,
            offset: 500,
            layout: place::layout(96),
            stage: Stage::new(&theme, 1.0),
            shade: Shade::new(shades[0]),
            visible: 0,
            open: false,
            pressed: None,
            clicked: None,
            dragging: false,
            tracking: false,
            countdown: None,
            capturing: false,
            menu_open: false,
            hotkey_registered: false,
            tray: Tray::new(hwnd),
            tip: String::new(),
            panel: Panel {
                hwnd: panel,
                stage: Stage::new(&theme, 1.0),
                shade: Shade::new(shades[1]),
                items: Vec::new(),
                rect: Rect::default(),
                below: true,
                corners: CornerRadius::ZERO,
                clicked: None,
                tracking: false,
                from_top: false,
                alpha: 255,
            },
            view: None,
            confirm: None,
            clock: Clock::new(),
            todos: Vec::new(),
            theme,
        };
        dock.load_todos();
        dock.relayout();
        dock
    }

    /// Which notes are todos (whir's window flags them).
    fn load_todos(&mut self) {
        let Some(dir) = whir_core::library::resolve_dir() else {
            self.todos.clear();
            return;
        };
        let all = todos::load(&dir);
        self.todos = all
            .open()
            .into_iter()
            .filter_map(|t| match t.state {
                todos::State::Open { since } => {
                    Some((t.title().to_owned(), dir.join(&t.name), since))
                }
                todos::State::Done { .. } => None,
            })
            .collect();
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
        self.visible = self.goal();
        self.panel.below = self.edge != Edge::Bottom;
        let theme = theme();
        if theme.dark != self.theme.dark {
            self.stage.set_theme(&theme);
            self.panel.stage.set_theme(&theme);
            self.theme = theme;
        }
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

    /// Pixels per point on our monitor.
    fn scale(&self) -> f32 {
        self.monitors.get(self.monitor).map_or(96, |m| m.dpi) as f32 / 96.0
    }

    /// Draw the whole dock: the bar and its grip. True if the picture changed.
    fn render(&mut self) -> bool {
        let bar = Bar {
            edge: self.edge,
            layout: self.layout,
            strip: self.clock.strip(Stamp::now()),
            countdown: self.countdown,
            delay: self.config.delay,
            today: self.view == Some(View::Today),
            below: self.panel.below,
        };
        self.stage.set_scale(self.scale());
        let (theme, mut clicked) = (&self.theme, None);
        let size = self.layout.size(self.edge);
        let drew = self.stage.run(size, |ui| {
            clicked = bar::show(ui, &bar, theme).or(clicked);
        });
        self.clicked = clicked.or(self.clicked);
        self.next_frame(TIMER_BAR_FRAME, &drew);
        drew.changed
    }

    /// A fade goes on: another frame soon.
    fn next_frame(&self, timer: usize, drew: &Drew) {
        if let Some(wait) = drew.again {
            let ms = wait.max(Duration::from_millis(10)).as_millis() as u32;
            unsafe {
                SetTimer(Some(self.hwnd), timer, ms, None);
            }
        }
    }

    /// Put the right slice of the strip on screen.
    fn present(&mut self) {
        let Some(surface) = self.stage.surface() else {
            return;
        };
        let (on_screen, from) = place::viewport(self.edge, self.anchor(), self.visible);
        // Closed, it is a little see-through.
        let alpha = if self.open { 255 } else { 205 };
        present_layered(self.hwnd, surface, on_screen, from, alpha);
        self.present_shade();
    }

    /// How a shadow falls around `shape`, in a picture of `full`, on this monitor.
    fn shadow_of(&self, shape: Rect, full: Rect) -> ((i32, i32), Shadow) {
        let s = self.scale();
        let look = self.theme.popup_shadow();
        let shadow = Shadow {
            shape: shape.translate(-full.left, -full.top),
            radius: self.theme.metrics.corner_radius * s,
            drop: (look.offset[1] as f32 * s).round() as i32,
            blur: look.blur as f32 * s,
            strength: look.color.a() as f32 / 255.0,
        };
        ((full.width(), full.height()), shadow)
    }

    /// The strip's shadow comes out with it, and isn't there when it's in.
    fn present_shade(&mut self) {
        let (depth, grip) = (self.layout.depth(self.edge), self.layout.grip);
        if self.config.hidden || self.capturing || self.visible <= grip {
            self.shade.hide();
            return;
        }
        let anchor = self.anchor();
        let reach = (SHADOW_REACH * self.scale()).round() as i32;
        let screen = self
            .monitors
            .get(self.monitor)
            .map_or(self.work(), |m| m.rect);
        let full = place::shadow_rect(anchor, reach, Some(self.edge)).intersect(screen);
        let (size, shadow) = self.shadow_of(anchor, full);
        let (rect, from) = place::shadow_viewport(self.edge, full, depth - self.visible);
        let alpha = 255 * (self.visible - grip) / (depth - grip).max(1);
        self.shade.show(size, shadow, rect, from, alpha as u8);
    }

    /// The panel's shadow, all around it but for a side against the screen.
    fn present_panel_shade(&mut self) {
        let shape = self.panel.rect;
        let reach = (SHADOW_REACH * self.scale()).round() as i32;
        let screen = self
            .monitors
            .get(self.monitor)
            .map_or(self.work(), |m| m.rect);
        let against = self.edge.is_side().then_some(self.edge);
        let full = place::shadow_rect(shape, reach, against).intersect(screen);
        let (size, shadow) = self.shadow_of(shape, full);
        let alpha = self.panel.alpha;
        self.panel.shade.show(size, shadow, full, (0, 0), alpha);
    }

    fn redraw(&mut self) {
        if self.render() {
            self.present();
        }
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

    /// Keep what changed, and tell Settings, which shows it.
    fn config_changed(&self) {
        self.config.save();
        ipc::notify_changed(Changed::Dock);
    }

    /// Settings would like something changed.
    fn asked(&mut self, ask: Ask) {
        match ask {
            Ask::Delay(seconds) if DELAYS.contains(&seconds) => self.set_delay(seconds),
            Ask::Delay(_) => {}
            Ask::Hotkey(hotkey) => {
                if std::mem::take(&mut self.hotkey_registered) {
                    unsafe {
                        let _ = UnregisterHotKey(Some(self.hwnd), HOTKEY);
                    }
                }
                self.config.hotkey = hotkey;
                self.config.hotkey_ok = true;
                self.register_hotkey();
                self.config_changed();
            }
        }
    }

    fn set_hidden(&mut self, hidden: bool) {
        if self.config.hidden != hidden {
            self.config.hidden = hidden;
            self.config_changed();
        }
        self.show();
        self.present_shade();
        self.raise();
        self.settle();
    }

    /// Something must be seen (a todo, a question), or a timer runs and the
    /// dock is to stay out for it.
    fn pinned_out(&self) -> bool {
        !self.config.hidden
            && (self.view.is_some()
                || self.confirm.is_some()
                || self.clock.away.is_some()
                || self.clock.keeps_out()
                || !self.todos.is_empty())
    }

    /// After whatever changed what the dock shows: the stopwatch ticking (or
    /// not), the tooltip, the bar, and out (or in) as needed.
    fn settle(&mut self) {
        unsafe {
            if self.clock.running().is_some() {
                SetTimer(Some(self.hwnd), TIMER_TICK, TICK_MS, None);
            } else {
                let _ = KillTimer(Some(self.hwnd), TIMER_TICK);
            }
        }
        // A milestone picked to switch to may be running now (or gone).
        if self.confirm.is_some()
            && !self
                .clock
                .running()
                .is_some_and(|r| Some(r.milestone) != self.confirm)
        {
            self.confirm = None;
        }
        self.set_tip(Stamp::now());
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
    /// The tray icon's tooltip, when it says something new.
    fn set_tip(&mut self, now: Stamp) {
        let tip = self.clock.tip(now);
        if tip != self.tip {
            self.tray.set_tip(&tip);
            self.tip = tip;
        }
    }

    fn update_panel(&mut self) {
        let mut model = self.clock.model(Stamp::now(), self.view, self.confirm);
        model.todos = self
            .todos
            .iter()
            .take(TODOS_SHOWN)
            .map(|(title, ..)| title.clone())
            .collect();
        model.more_todos = self.todos.len().saturating_sub(TODOS_SHOWN);
        let fully_out = self.open && self.visible == self.layout.depth(self.edge);
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
        let points = (width as f32 / s).round() as i32;
        let items = panel::layout(&model, points);
        let want = (panel::height(&items) as f32 * s).ceil() as i32;
        let gap = self.layout.panel_gap;
        let (rect, below) =
            place::panel_rect(self.work(), self.edge, self.anchor(), (width, want), gap);
        if rect.height() <= 0 {
            self.hide_panel();
            return;
        }
        if below != self.panel.below {
            self.panel.below = below;
            self.redraw();
        }
        // Square where it meets the screen's edge.
        let r = self.theme.metrics.corner_radius as u8;
        let p = &mut self.panel;
        p.corners = match self.edge {
            Edge::Left => CornerRadius {
                ne: r,
                se: r,
                ..CornerRadius::ZERO
            },
            Edge::Right => CornerRadius {
                nw: r,
                sw: r,
                ..CornerRadius::ZERO
            },
            Edge::Top | Edge::Bottom => CornerRadius::same(r),
        };
        p.items = items;
        let shown = !p.rect.is_empty();
        let moved = p.rect != rect;
        p.rect = rect;
        p.stage.set_scale(s);
        if !shown {
            p.alpha = if animations_on() {
                PANEL_FADE_STEP
            } else {
                255
            };
        }
        self.draw_panel(moved);
        self.present_panel_shade();
        if !shown {
            unsafe {
                let _ = ShowWindow(self.panel.hwnd, SW_SHOWNOACTIVATE);
                if self.panel.alpha < 255 {
                    SetTimer(Some(self.hwnd), TIMER_PANEL_FADE, PANEL_FADE_MS, None);
                }
            }
        }
        self.raise();
    }

    /// One step of the panel fading in.
    fn fade_panel(&mut self) {
        let p = &mut self.panel;
        p.alpha = p.alpha.saturating_add(PANEL_FADE_STEP);
        if p.alpha == 255 || p.rect.is_empty() {
            p.alpha = 255;
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_PANEL_FADE);
            }
        }
        if let Some(surface) = p.stage.surface()
            && !p.rect.is_empty()
        {
            present_layered(p.hwnd, surface, p.rect, (0, 0), p.alpha);
            self.present_panel_shade();
        }
    }

    /// A frame of the panel as it stands, shown if it's new or `moved`.
    fn draw_panel(&mut self, moved: bool) {
        let p = &mut self.panel;
        if p.rect.is_empty() {
            return;
        }
        let (items, corners, theme) = (&p.items, p.corners, &self.theme);
        let from_top = std::mem::take(&mut p.from_top);
        let size = (p.rect.width(), p.rect.height());
        let mut clicked = None;
        let drew = p.stage.run(size, |ui| {
            clicked = panel::show(ui, items, corners, from_top, theme).or(clicked);
        });
        p.clicked = clicked.or(p.clicked);
        if (drew.changed || moved)
            && let Some(surface) = p.stage.surface()
        {
            present_layered(p.hwnd, surface, p.rect, (0, 0), p.alpha);
        }
        self.next_frame(TIMER_PANEL_FRAME, &drew);
    }

    fn hide_panel(&mut self) {
        if !self.panel.rect.is_empty() {
            unsafe {
                let _ = ShowWindow(self.panel.hwnd, SW_HIDE);
            }
        }
        self.panel.shade.hide();
        self.panel.rect = Rect::default();
        self.panel.stage.pointer_gone();
    }

    /// A point of the window, in the picture of the whole strip it shows part of.
    fn on_strip(&self, client: Point) -> Point {
        let (_, from) = place::viewport(self.edge, self.anchor(), self.visible);
        (client.0 + from.0, client.1 + from.1)
    }

    fn set_open(&mut self, open: bool) {
        if self.open == open {
            return;
        }
        self.open = open;
        if !open {
            self.stage.pointer_gone();
            self.view = None;
            self.confirm = None;
            self.hide_panel();
        }
        // The stopwatch isn't drawn while the dock is in.
        self.render();
        if animations_on() {
            unsafe {
                SetTimer(Some(self.hwnd), TIMER_SLIDE, 15, None);
            }
        } else {
            self.visible = self.goal();
        }
        self.present();
        if open && self.visible == self.goal() {
            self.update_panel();
        }
    }

    fn goal(&self) -> i32 {
        if self.open {
            self.layout.depth(self.edge)
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
        if self.view.is_some() || self.confirm.is_some() {
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
        // Shadows first: each goes above the last.
        let panel = !self.panel.rect.is_empty();
        let order = [
            (self.shade.hwnd, self.shade.shown),
            (self.panel.shade.hwnd, self.panel.shade.shown),
            (self.hwnd, true),
            (self.panel.hwnd, panel),
        ];
        for (hwnd, shown) in order {
            if shown {
                unsafe {
                    let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, flags);
                }
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
        let busy = self.menu_open || self.dragging || self.countdown.is_some();
        if busy || self.pointer_is_over() {
            return;
        }
        // Left alone, a question is answered "keep it running".
        if self.view.is_some() || self.confirm.is_some() {
            self.view = None;
            self.confirm = None;
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
        if let Some(from) = self.pressed
            && !self.dragging
        {
            let at = cursor();
            let far = 2 * unsafe { GetSystemMetrics(SM_CXDRAG) }.max(2);
            self.dragging = (at.0 - from.0).abs() > far || (at.1 - from.1).abs() > far;
            if self.dragging {
                self.stage.call_off_press();
                self.hide_panel();
            }
        }
        if self.dragging {
            self.drag_to(cursor());
        } else if self.open {
            let at = self.on_strip(client);
            self.stage.pointer_moved(at);
            self.redraw();
        }
    }

    /// Dragged: go to the nearest edge of the monitor the pointer is on.
    fn drag_to(&mut self, at: Point) {
        let Some((monitor, edge, offset)) = place::snap(at, &self.monitors) else {
            return;
        };
        let turned = edge != self.edge || monitor != self.monitor;
        (self.monitor, self.edge, self.offset) = (monitor, edge, offset);
        if turned {
            // Another edge is another shape; another monitor, maybe another scale.
            self.layout = place::layout(self.monitors[monitor].dpi);
            self.visible = self.layout.depth(edge);
            self.open = true;
            self.panel.below = edge != Edge::Bottom;
            self.render();
        }
        self.present();
    }

    fn pressed_at(&mut self, client: Point) {
        self.pressed = Some(cursor());
        let at = self.on_strip(client);
        self.stage.button(at, true);
        self.redraw();
    }

    fn released(&mut self, client: Point) {
        if self.pressed.take().is_none() {
            return;
        }
        if std::mem::take(&mut self.dragging) {
            self.config.spot = self.monitors.get(self.monitor).map(|m| Spot {
                monitor: m.device.clone(),
                edge: self.edge,
                offset: self.offset,
            });
            self.config.save();
            self.update_panel();
            self.linger(self.linger_ms());
        } else {
            let at = self.on_strip(client);
            self.stage.button(at, false);
            self.redraw();
            if let Some(slot) = self.clicked.take() {
                self.press(slot);
            }
        }
    }

    fn press(&mut self, slot: Slot) {
        match slot {
            Slot::Capture => self.capture_pressed(),
            Slot::Delay => {
                // Step through the choices.
                let at = DELAYS.iter().position(|&d| d == self.config.delay);
                self.set_delay(DELAYS[(at.unwrap_or(0) + 1) % DELAYS.len()]);
            }
            Slot::Play if self.clock.selected().is_some() => {
                self.confirm = None;
                self.clock.toggle();
                self.settle();
            }
            // Nothing to play yet: choose a milestone.
            Slot::Play | Slot::Milestone => self.choose(),
            Slot::More => {
                let today = self.view != Some(View::Today);
                self.set_view(today.then_some(View::Today));
            }
            Slot::Time => {}
        }
    }

    /// The picker; or, with no milestones to pick from, where they're set up.
    fn choose(&mut self) {
        if self.clock.state().is_some() && !self.clock.has_milestones() {
            self.set_view(None);
            open_settings("milestones");
            return;
        }
        let picker = self.view != Some(View::Picker);
        self.set_view(picker.then_some(View::Picker));
    }

    fn set_view(&mut self, view: Option<View>) {
        self.view = view;
        self.confirm = None;
        if view.is_some() {
            // What the window may have changed since.
            self.clock.refresh();
            self.panel.from_top = true;
        }
        self.settle();
    }

    fn set_delay(&mut self, seconds: u32) {
        self.config.delay = seconds;
        self.config_changed();
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
            // Fails if another program has the combination: the menu and
            // Settings say so.
            self.hotkey_registered =
                unsafe { RegisterHotKey(Some(self.hwnd), HOTKEY, mods, vk) }.is_ok();
        }
        if self.config.hotkey_ok != self.hotkey_registered {
            self.config.hotkey_ok = self.hotkey_registered;
            self.config_changed();
        }
    }

    /// The stopwatch, and time away, every second a timer runs.
    fn tick(&mut self) {
        let now = Stamp::now();
        if self.clock.tick(now) {
            return self.settle();
        }
        self.set_tip(now);
        if self.open {
            self.redraw();
        }
        if !self.panel.rect.is_empty() {
            self.update_panel();
        }
    }

    /// Monitors, scaling, the taskbar or the theme changed.
    fn look_again(&mut self) {
        self.relayout();
        self.present();
        self.raise();
        self.update_panel();
    }

    /// The window (or another dock) wrote the timesheet.
    fn shared_changed(&mut self, what: Option<Changed>) {
        match what {
            Some(Changed::Time) => {
                self.clock.refresh();
            }
            Some(Changed::Todos) => self.load_todos(),
            Some(Changed::Theme) => {
                self.look_again();
                return;
            }
            // Ours: the dock is what writes it.
            Some(Changed::Dock) | None => return,
        }
        self.settle();
    }

    // --- the panel ---

    fn panel_mouse_move(&mut self, client: Point) {
        if !self.panel.tracking {
            self.panel.tracking = Dock::track(self.panel.hwnd, false);
        }
        self.panel.stage.pointer_moved(client);
        self.draw_panel(false);
    }

    fn panel_left(&mut self) {
        self.panel.tracking = false;
        self.panel.stage.pointer_gone();
        self.draw_panel(false);
        self.linger(self.linger_ms());
    }

    fn panel_scrolled(&mut self, delta: i32) {
        self.panel.stage.wheel(delta as f32 / 120.0);
        self.draw_panel(false);
    }

    fn panel_pressed(&mut self, client: Point) {
        self.panel.stage.button(client, true);
        self.draw_panel(false);
    }

    fn panel_released(&mut self, client: Point) {
        self.panel.stage.button(client, false);
        self.draw_panel(false);
        let Some(hit) = self.panel.clicked.take() else {
            return;
        };
        match hit {
            Hit::Pick(id) => {
                // Another runs: ask first, instead of the list.
                if self.clock.pick(id) {
                    self.confirm = Some(id);
                }
                self.view = None;
            }
            Hit::Switch(id) => {
                self.clock.start(id);
                self.confirm = None;
            }
            Hit::KeepRunning => self.confirm = None,
            Hit::Keep => self.clock.keep_away(),
            Hit::Drop => self.clock.drop_away(),
            Hit::Timesheet => open_settings("timesheet"),
            Hit::Milestones => open_settings("milestones"),
            // The note, in whir; the first not listed for "more".
            Hit::Todo(i) => self.open_todo(i),
            Hit::MoreTodos => self.open_todo(TODOS_SHOWN),
            Hit::TodoDone(i) => {
                if let Some((_, path, since)) = self.todos.get(i).cloned() {
                    let done = todos::State::Done {
                        since,
                        done: Stamp::now(),
                    };
                    if let Err(e) = todos::set(&path, Some(done)) {
                        crate::report(&e);
                    }
                    self.load_todos();
                }
            }
        }
        self.settle();
    }

    fn open_todo(&self, i: usize) {
        if let Some((_, path, _)) = self.todos.get(i)
            && let Err(e) = crate::handoff::open_in_whir(path)
        {
            crate::report(&e);
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
        d.view = None;
        d.confirm = None;
        d.stage.pointer_gone();
        d.visible = d.layout.grip;
        d.hide_panel();
        d.shade.hide();
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
            let mut d = dock.borrow_mut();
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
fn menu(dock: &RefCell<Dock>) {
    let (delay, hotkey, registered, theme) = {
        let mut d = dock.borrow_mut();
        d.register_hotkey();
        d.menu_open = true;
        (
            d.config.delay,
            d.config.hotkey,
            d.hotkey_registered,
            d.theme.clone(),
        )
    };
    let capture = Entry::item(MENU_CAPTURE, "Capture").icon(Icon::Camera);
    let mut entries = Vec::new();
    match hotkey.map(format_hotkey) {
        Some(keys) if registered => entries.push(capture.shortcut(keys)),
        Some(keys) => {
            entries.push(capture);
            entries.push(Entry::item(0, format!("{keys} is in use by another app")).disabled());
        }
        None => entries.push(capture),
    }
    entries.push(Entry::Rule);
    for seconds in DELAYS {
        let text = match seconds {
            0 => "No delay".to_owned(),
            n => format!("{n} second delay"),
        };
        entries.push(Entry::item(MENU_DELAY + seconds as usize, text).ticked(seconds == delay));
    }
    entries.extend([
        Entry::Rule,
        Entry::item(MENU_OPEN_WHIR, "Open whir"),
        Entry::item(MENU_TIMESHEET, "Timesheet…").icon(Icon::Stopwatch),
        Entry::item(MENU_OPEN_FOLDER, "Open screenshots folder").icon(Icon::Folder),
        Entry::Rule,
        Entry::item(MENU_HIDE, "Hide dock"),
    ]);
    let picked = crate::menu::show(entries, &theme);
    let mut d = dock.borrow_mut();
    d.menu_open = false;
    d.linger(d.linger_ms());
    match picked {
        Some(MENU_CAPTURE) => d.capture_pressed(),
        Some(MENU_OPEN_WHIR) => open_whir(),
        Some(MENU_TIMESHEET) => open_settings("timesheet"),
        Some(MENU_OPEN_FOLDER) => open_folder(),
        Some(MENU_HIDE) => d.set_hidden(true),
        Some(id) if DELAYS.iter().any(|&s| MENU_DELAY + s as usize == id) => {
            d.set_delay((id - MENU_DELAY) as u32);
        }
        _ => {}
    }
}

/// The tray icon's menu: the dock's way back when hidden, and whir's way out.
fn tray_menu(dock: &RefCell<Dock>, hwnd: HWND) {
    let (hidden, hotkey, registered, theme) = {
        let mut d = dock.borrow_mut();
        d.register_hotkey();
        d.menu_open = true;
        (
            d.config.hidden,
            d.config.hotkey,
            d.hotkey_registered,
            d.theme.clone(),
        )
    };
    let autostart = shell::autostart().is_some();
    let capture = Entry::item(MENU_CAPTURE, "Capture").icon(Icon::Camera);
    let entries = vec![
        match hotkey.map(format_hotkey) {
            Some(keys) if registered => capture.shortcut(keys),
            _ => capture,
        },
        Entry::item(MENU_OPEN_WHIR, "Open whir"),
        Entry::item(MENU_TIMESHEET, "Timesheet…").icon(Icon::Stopwatch),
        Entry::Rule,
        if hidden {
            Entry::item(MENU_SHOW, "Show dock")
        } else {
            Entry::item(MENU_HIDE, "Hide dock")
        },
        Entry::item(MENU_AUTOSTART, "Start with Windows").ticked(autostart),
        Entry::Rule,
        Entry::item(MENU_QUIT, "Quit whir"),
    ];
    let picked = crate::menu::show(entries, &theme);
    let mut d = dock.borrow_mut();
    d.menu_open = false;
    match picked {
        Some(MENU_CAPTURE) => d.capture_pressed(),
        Some(MENU_OPEN_WHIR) => open_whir(),
        Some(MENU_TIMESHEET) => open_settings("timesheet"),
        Some(MENU_SHOW) => {
            d.set_hidden(false);
            d.set_open(true);
            d.linger(REVEAL_MS);
        }
        Some(MENU_HIDE) => d.set_hidden(true),
        Some(MENU_AUTOSTART) => {
            if let Err(e) = shell::set_autostart(!autostart) {
                drop(d);
                crate::report(&e);
            }
        }
        Some(MENU_QUIT) => {
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

/// Starting with Windows is the user's to switch on. Once it's on, the entry
/// follows the folder: whir can be moved, so an entry pointing at another copy
/// is pointed at this one.
fn autostart_follows() {
    // A development build never claims the user's sign-in.
    if cfg!(debug_assertions) {
        return;
    }
    if let (Some(now), Some(wanted)) = (shell::autostart(), shell::autostart_command())
        && !wanted.eq_ignore_ascii_case(&now)
    {
        let _ = shell::set_autostart(true);
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

/// A page of whir's Settings (the timesheet, the milestones), in the running
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

pub fn client_point(lparam: LPARAM) -> Point {
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
                menu(dock);
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
            WM_MOUSEMOVE => d.mouse_move(client_point(lparam)),
            WM_MOUSEHOVER => {
                // Resting on the grip opens it; passing by with a button held doesn't.
                if wparam.0 & 0x1f == 0 {
                    d.set_open(true);
                }
            }
            WM_MOUSELEAVE => {
                d.tracking = false;
                d.stage.pointer_gone();
                d.redraw();
                let ms = d.linger_ms();
                d.linger(ms);
            }
            WM_LBUTTONDOWN => {
                d.pressed_at(client_point(lparam));
                drop(d);
                SetCapture(hwnd);
            }
            WM_LBUTTONUP => {
                d.released(client_point(lparam));
                // Holding the mouse may have cost us word of it leaving.
                d.tracking = Dock::track(hwnd, true);
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
                TIMER_BAR_FRAME => {
                    let _ = KillTimer(Some(hwnd), TIMER_BAR_FRAME);
                    d.redraw();
                }
                TIMER_PANEL_FRAME => {
                    let _ = KillTimer(Some(hwnd), TIMER_PANEL_FRAME);
                    d.draw_panel(false);
                }
                TIMER_PANEL_FADE => d.fade_panel(),
                _ => {}
            },
            WM_HOTKEY => {
                if !d.capturing {
                    d.capture_now();
                }
            }
            instance::WM_ASKED => {
                if let Some(ask) = Ask::decode(wparam.0, lparam.0) {
                    d.asked(ask);
                }
            }
            WM_RAISE => {
                d.raise();
                SetTimer(Some(hwnd), TIMER_RAISE, RAISE_AGAIN_MS, None);
            }
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
                Some(Command::Hide) => d.set_hidden(true),
                Some(Command::Start) => {}
                Some(Command::Capture) => d.capture_now(),
                Some(Command::Quit) => {
                    drop(d);
                    let _ = DestroyWindow(hwnd);
                }
                None => {}
            },
            WM_DISPLAYCHANGE | WM_DPICHANGED | WM_SETTINGCHANGE => d.look_again(),
            WM_DESTROY => {
                DOCK.store(0, Ordering::Relaxed);
                if d.hotkey_registered {
                    let _ = UnregisterHotKey(Some(hwnd), HOTKEY);
                }
                let _ = DestroyWindow(d.panel.hwnd);
                let _ = DestroyWindow(d.panel.shade.hwnd);
                let _ = DestroyWindow(d.shade.hwnd);
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
                menu(dock);
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
            WM_MOUSEMOVE => d.panel_mouse_move(client_point(lparam)),
            WM_MOUSELEAVE => d.panel_left(),
            WM_MOUSEWHEEL => d.panel_scrolled((wparam.0 >> 16) as u16 as i16 as i32),
            WM_LBUTTONDOWN => {
                d.panel_pressed(client_point(lparam));
                drop(d);
                SetCapture(hwnd);
            }
            WM_LBUTTONUP => {
                d.panel_released(client_point(lparam));
                // Holding the mouse may have cost us word of it leaving.
                d.panel.tracking = Dock::track(hwnd, false);
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

/// A shadow does nothing but be there.
unsafe extern "system" fn shade_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
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
    // Marking a todo done tells the window (it lists them).
    todos::on_change(|| ipc::notify_changed(Changed::Todos));
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
            (SHADE_CLASS, shade_wndproc),
            (crate::menu::CLASS, crate::menu::wndproc),
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
        // Clicks go straight through a shadow.
        let shade = || {
            CreateWindowExW(
                style | WS_EX_TRANSPARENT,
                SHADE_CLASS,
                w!("whir dock shadow"),
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
        let (Ok(hwnd), Ok(panel), Ok(shade_bar), Ok(shade_panel)) = (
            window(instance::CLASS, w!("whir capture dock")),
            window(PANEL_CLASS, w!("whir dock panel")),
            shade(),
            shade(),
        ) else {
            crate::report("The capture dock couldn't be started.");
            return 1;
        };
        let dock = RefCell::new(Dock::new(hwnd, panel, [shade_bar, shade_panel]));
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
            autostart_follows();
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
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        if !hook.is_invalid() {
            let _ = UnhookWinEvent(hook);
        }
        msg.wParam.0 as i32
    }
}
