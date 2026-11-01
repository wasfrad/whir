//! The dock's menus (its right-click one, the tray icon's), drawn as the window
//! draws its own. A window of their own, with a message loop of their own until
//! something is picked or they're dismissed: they run at a click, not all day.
//! Like a Windows menu, one comes forward so the keyboard reaches it, and goes
//! when anything else is clicked.

use std::cell::RefCell;

use egui::containers::menu::menu_style;
use egui::{Align, Button, FontId, Frame, Layout, Rect as Area, Ui, UiBuilder, vec2};
use whir_core::icons::Icon;
use whir_core::kit;
use whir_core::theme::Theme;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_DOWN, VK_ESCAPE,
    VK_LBUTTON, VK_RBUTTON, VK_RETURN, VK_SPACE, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWLP_USERDATA, GetCursorPos,
    GetMessageW, GetWindowLongPtrW, KillTimer, MSG, PostQuitMessage, SW_SHOW, SetForegroundWindow,
    SetTimer, SetWindowLongPtrW, ShowWindow, TranslateMessage, WA_INACTIVE, WM_ACTIVATE,
    WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCDESTROY, WM_RBUTTONDOWN,
    WM_RBUTTONUP, WM_TIMER, WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{PCWSTR, w};

use crate::dock::{client_point, present_layered};
use crate::geom::{Point, Rect};
use crate::monitors;
use crate::place;
use crate::stage::Stage;

pub const CLASS: PCWSTR = w!("whir.dock.menu.v1");

const WM_MOUSELEAVE: u32 = 0x02A3;
/// Room around the menu for its shadow, in points.
const MARGIN: f32 = 24.0;
const MIN_WIDTH: f32 = 180.0;
/// How often to check for a click somewhere else, in milliseconds, should
/// Windows not have let the menu come forward to be told of it.
const WATCH_MS: u32 = 100;
const TIMER_WATCH: usize = 1;
const TIMER_FRAME: usize = 2;

pub enum Entry {
    Item {
        /// What [`show`] returns for it.
        id: usize,
        label: String,
        shortcut: String,
        icon: Option<Icon>,
        enabled: bool,
    },
    Rule,
}

impl Entry {
    pub fn item(id: usize, label: impl Into<String>) -> Entry {
        Entry::Item {
            id,
            label: label.into(),
            shortcut: String::new(),
            icon: None,
            enabled: true,
        }
    }

    pub fn icon(mut self, icon: Icon) -> Entry {
        if let Entry::Item { icon: i, .. } = &mut self {
            *i = Some(icon);
        }
        self
    }

    pub fn ticked(self, on: bool) -> Entry {
        if on { self.icon(Icon::Check) } else { self }
    }

    pub fn shortcut(mut self, keys: impl Into<String>) -> Entry {
        if let Entry::Item { shortcut, .. } = &mut self {
            *shortcut = keys.into();
        }
        self
    }

    pub fn disabled(mut self) -> Entry {
        if let Entry::Item { enabled, .. } = &mut self {
            *enabled = false;
        }
        self
    }

    fn can_pick(&self) -> bool {
        matches!(self, Entry::Item { enabled: true, .. })
    }
}

/// How wide the menu needs to be, in points.
fn width(ui: &Ui, entries: &[Entry], theme: &Theme) -> f32 {
    const GUTTER: f32 = 16.0 + 8.0;
    const BEFORE_SHORTCUT: f32 = 32.0;
    const EDGES: f32 = 24.0;
    let font = FontId::proportional(theme.metrics.body_text_size);
    let measure = |text: &str| {
        let color = egui::Color32::PLACEHOLDER;
        let galley = ui
            .painter()
            .layout_no_wrap(text.to_owned(), font.clone(), color);
        galley.size().x
    };
    let widest = entries.iter().filter_map(|e| match e {
        Entry::Item {
            label, shortcut, ..
        } => {
            let keys = match shortcut.is_empty() {
                true => 0.0,
                false => BEFORE_SHORTCUT + measure(shortcut),
            };
            Some(GUTTER + measure(label) + keys + EDGES)
        }
        Entry::Rule => None,
    });
    widest.fold(MIN_WIDTH, f32::max)
}

/// The menu, `MARGIN` in from the window's corner, as wide as it has to be.
/// Returns where it came out, and what was picked.
fn draw(
    ui: &mut Ui,
    entries: &[Entry],
    keyed: Option<usize>,
    theme: &Theme,
) -> (Area, Option<usize>) {
    let wide = width(ui, entries, theme);
    let room = Area::from_min_size(
        ui.max_rect().min + vec2(MARGIN, MARGIN),
        vec2(wide, f32::INFINITY),
    );
    let mut picked = None;
    let builder = UiBuilder::new()
        .max_rect(room)
        .layout(Layout::top_down_justified(Align::Min));
    let shown = ui.scope_builder(builder, |ui| {
        menu_style(ui.style_mut());
        Frame::menu(ui.style()).show(ui, |ui| {
            for (i, entry) in entries.iter().enumerate() {
                match entry {
                    Entry::Rule => {
                        ui.separator();
                    }
                    Entry::Item {
                        id,
                        label,
                        shortcut,
                        icon,
                        enabled,
                    } => {
                        let row = kit::menu_entry(*icon, label.as_str(), shortcut.as_str());
                        let clicked = ui
                            .scope(|ui| {
                                if keyed == Some(i) {
                                    // The way the keyboard has got to: as if hovered.
                                    let w = &mut ui.visuals_mut().widgets;
                                    w.inactive.weak_bg_fill = w.hovered.weak_bg_fill;
                                }
                                let row: Button = row;
                                ui.add_enabled(*enabled, row).clicked()
                            })
                            .inner;
                        if clicked {
                            picked = Some(*id);
                        }
                    }
                }
            }
        })
    });
    (shown.inner.response.rect, picked)
}

struct Menu {
    hwnd: HWND,
    stage: Stage,
    entries: Vec<Entry>,
    theme: Theme,
    /// Its window on screen.
    window: Rect,
    /// The menu itself in that window, in points.
    shape: Area,
    scale: f32,
    /// Where the arrow keys have got to.
    keyed: Option<usize>,
    tracking: bool,
    picked: Option<usize>,
    done: bool,
}

impl Menu {
    fn frame(&mut self) {
        let (entries, keyed, theme) = (&self.entries, self.keyed, &self.theme);
        let size = (self.window.width(), self.window.height());
        let (mut shape, mut picked) = (self.shape, None);
        let drew = self.stage.run(size, |ui| {
            let (rect, clicked) = draw(ui, entries, keyed, theme);
            shape = rect;
            picked = clicked.or(picked);
        });
        self.shape = shape;
        if drew.changed
            && let Some(surface) = self.stage.surface()
        {
            present_layered(self.hwnd, surface, self.window, (0, 0), 255);
        }
        if let Some(wait) = drew.again {
            let ms = wait.as_millis().clamp(10, 1000) as u32;
            unsafe {
                SetTimer(Some(self.hwnd), TIMER_FRAME, ms, None);
            }
        }
        if picked.is_some() {
            self.picked = picked;
            self.done = true;
        }
    }

    /// The arrow keys: the next entry that can be picked, `by` one way or the other.
    fn step(&mut self, by: isize) {
        let n = self.entries.len() as isize;
        let from = self
            .keyed
            .map_or(if by > 0 { -1 } else { n }, |k| k as isize);
        let next = (1..=n)
            .map(|i| (from + by * i).rem_euclid(n) as usize)
            .find(|&i| self.entries[i].can_pick());
        if next.is_some() {
            self.keyed = next;
            self.frame();
        }
    }

    fn key(&mut self, vk: u16) {
        match vk {
            v if v == VK_ESCAPE.0 => self.done = true,
            v if v == VK_DOWN.0 => self.step(1),
            v if v == VK_UP.0 => self.step(-1),
            v if v == VK_RETURN.0 || v == VK_SPACE.0 => {
                if let Some(Entry::Item { id, .. }) = self.keyed.map(|k| &self.entries[k]) {
                    self.picked = Some(*id);
                    self.done = true;
                }
            }
            _ => {}
        }
    }

    /// A click in the window but not on the menu (on its shadow) is a click away.
    fn on_menu(&self, at: Point) -> bool {
        let p = egui::pos2(at.0 as f32 / self.scale, at.1 as f32 / self.scale);
        self.shape.contains(p)
    }

    fn pressed(&mut self, at: Point) {
        if self.on_menu(at) {
            self.stage.button(at, true);
            self.frame();
        } else {
            self.done = true;
        }
    }

    fn released(&mut self, at: Point) {
        self.stage.button(at, false);
        self.frame();
    }

    /// Windows didn't let us forward: nobody will say another window was clicked.
    fn watch(&mut self) {
        let held = |vk: u16| unsafe { GetAsyncKeyState(vk as i32) } < 0;
        if held(VK_LBUTTON.0) || held(VK_RBUTTON.0) {
            let at = cursor();
            if !self.window.contains(at) {
                self.done = true;
            }
        }
    }
}

fn cursor() -> Point {
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    (p.x, p.y)
}

pub unsafe extern "system" fn wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        let menu = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const RefCell<Menu>;
        let Some(menu) = menu.as_ref() else {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        };
        if msg == WM_NCDESTROY {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        let Ok(mut m) = menu.try_borrow_mut() else {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        };
        match msg {
            WM_MOUSEMOVE => {
                if !m.tracking {
                    let mut track = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    m.tracking = TrackMouseEvent(&mut track).is_ok();
                }
                m.keyed = None;
                m.stage.pointer_moved(client_point(lparam));
                m.frame();
            }
            WM_MOUSELEAVE => {
                m.tracking = false;
                m.stage.pointer_gone();
                m.frame();
            }
            // A menu takes either button, as Windows' do.
            WM_LBUTTONDOWN | WM_RBUTTONDOWN => m.pressed(client_point(lparam)),
            WM_LBUTTONUP | WM_RBUTTONUP => m.released(client_point(lparam)),
            WM_KEYDOWN => m.key(wparam.0 as u16),
            WM_ACTIVATE if (wparam.0 & 0xffff) as u32 == WA_INACTIVE => m.done = true,
            WM_TIMER => match wparam.0 {
                TIMER_WATCH => m.watch(),
                TIMER_FRAME => {
                    let _ = KillTimer(Some(hwnd), TIMER_FRAME);
                    m.frame();
                }
                _ => {}
            },
            _ => {
                drop(m);
                return DefWindowProcW(hwnd, msg, wparam, lparam);
            }
        }
        LRESULT(0)
    }
}

/// Show `entries` at the pointer. Returns the `id` of what was picked. Runs a
/// message loop, so it must not be called under a borrow of the dock.
pub fn show(entries: Vec<Entry>, theme: &Theme) -> Option<usize> {
    let at = cursor();
    let list = monitors::list();
    let monitor = list.iter().find(|m| m.rect.contains(at));
    let (work, dpi) = monitor.map_or((Rect::new(at.0, at.1, at.0, at.1), 96), |m| (m.work, m.dpi));
    let scale = dpi as f32 / 96.0;

    // How big it comes out, drawn once where there's room.
    let mut stage = Stage::new(theme, scale);
    let mut shape = Area::ZERO;
    stage.run((2000, 2000), |ui| {
        shape = draw(ui, &entries, None, theme).0;
    });
    let margin = (MARGIN * scale).ceil() as i32;
    let size = (
        (shape.right() * scale).ceil() as i32 + margin,
        (shape.bottom() * scale).ceil() as i32 + margin,
    );
    let window = place::menu_rect(at, size, margin, work);

    unsafe {
        let instance: HINSTANCE = GetModuleHandleW(PCWSTR::null())
            .map(Into::into)
            .unwrap_or_default();
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            CLASS,
            w!("whir dock menu"),
            WS_POPUP,
            window.left,
            window.top,
            window.width(),
            window.height(),
            None,
            None,
            Some(instance),
            None,
        )
        .ok()?;
        let menu = RefCell::new(Menu {
            hwnd,
            stage,
            entries,
            theme: theme.clone(),
            window,
            shape,
            scale,
            keyed: None,
            tracking: false,
            picked: None,
            done: false,
        });
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, &menu as *const RefCell<Menu> as isize);
        menu.borrow_mut().frame();
        let _ = ShowWindow(hwnd, SW_SHOW);
        // So that the keyboard comes here, and a click elsewhere dismisses it.
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(hwnd));
        SetTimer(Some(hwnd), TIMER_WATCH, WATCH_MS, None);

        let mut msg = MSG::default();
        while !menu.borrow().done {
            match GetMessageW(&mut msg, None, 0, 0).0 {
                // The dock is closing: that's for the loop outside this one too.
                0 => {
                    PostQuitMessage(msg.wParam.0 as i32);
                    break;
                }
                -1 => break,
                _ => {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
        let _ = DestroyWindow(hwnd);
        menu.into_inner().picked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAPTURE: usize = 1;
    const OFF: usize = 2;
    const HIDE: usize = 3;

    fn entries() -> Vec<Entry> {
        vec![
            Entry::item(CAPTURE, "Capture")
                .icon(Icon::Camera)
                .shortcut("Ctrl+PrtSc"),
            Entry::item(OFF, "Ctrl+PrtSc is in use by another app").disabled(),
            Entry::Rule,
            Entry::item(HIDE, "Hide dock").ticked(true),
        ]
    }

    fn frame(stage: &mut Stage, entries: &[Entry], theme: &Theme) -> (Area, Option<usize>) {
        let mut out = (Area::ZERO, None);
        stage.run((400, 240), |ui| {
            let (rect, picked) = draw(ui, entries, None, theme);
            out = (rect, picked.or(out.1));
        });
        out
    }

    fn click(stage: &mut Stage, entries: &[Entry], at: Point, theme: &Theme) -> Option<usize> {
        stage.pointer_moved(at);
        frame(stage, entries, theme);
        stage.button(at, true);
        frame(stage, entries, theme);
        stage.button(at, false);
        frame(stage, entries, theme).1
    }

    #[test]
    fn a_click_picks_an_entry_unless_it_is_off() {
        let theme = Theme::dark();
        let list = entries();
        let mut stage = Stage::new(&theme, 1.0);
        let (shape, _) = frame(&mut stage, &list, &theme);
        assert!(shape.width() >= MIN_WIDTH);
        let x = shape.center().x as i32;
        let y = |row: f32| (shape.top() + 4.0 + row * shape.height() / 4.5) as i32;
        assert_eq!(click(&mut stage, &list, (x, y(0.5)), &theme), Some(CAPTURE));
        assert_eq!(click(&mut stage, &list, (x, y(1.5)), &theme), None);
        let bottom = shape.bottom() as i32 - 12;
        assert_eq!(click(&mut stage, &list, (x, bottom), &theme), Some(HIDE));
    }

    #[test]
    fn the_longest_entry_fits() {
        let theme = Theme::dark();
        let list = entries();
        let mut stage = Stage::new(&theme, 1.0);
        let (narrow, _) = frame(&mut stage, &[Entry::item(1, "Hi")], &theme);
        let (wide, _) = frame(&mut stage, &list, &theme);
        assert_eq!(narrow.width(), MIN_WIDTH);
        assert!(wide.width() > 200.0, "{}", wide.width());
    }

    /// `cargo test -p whir-capture -- --ignored preview` writes the menus, in
    /// both themes, to PNGs in `WHIR_PREVIEW_DIR` (to look at).
    #[test]
    #[ignore = "writes pictures to look at"]
    fn preview_menu() {
        let Some(dir) = std::env::var_os("WHIR_PREVIEW_DIR") else {
            return;
        };
        for (name, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
            let list = vec![
                Entry::item(1, "Capture")
                    .icon(Icon::Camera)
                    .shortcut("Ctrl+PrtSc"),
                Entry::Rule,
                Entry::item(2, "No delay").ticked(true),
                Entry::item(3, "3 second delay"),
                Entry::item(4, "5 second delay"),
                Entry::Rule,
                Entry::item(5, "Open whir"),
                Entry::item(6, "Timesheet…").icon(Icon::Stopwatch),
                Entry::item(7, "Open screenshots folder").icon(Icon::Folder),
                Entry::Rule,
                Entry::item(8, "Hide dock"),
            ];
            let mut stage = Stage::new(&theme, 1.0);
            let size = (300, 330);
            let mut shape = Area::ZERO;
            stage.run(size, |ui| shape = draw(ui, &list, None, &theme).0);
            stage.pointer_moved((100, 130));
            for _ in 0..3 {
                stage.run(size, |ui| {
                    draw(ui, &list, None, &theme);
                });
                std::thread::sleep(std::time::Duration::from_millis(60));
            }
            let path = std::path::Path::new(&dir).join(format!("menu-{name}.png"));
            stage.save_png(&path);
        }
    }
}
