//! Typing in the dock: a small window with a native edit box, put over the
//! panel's note line. The dock itself never takes the keyboard (a click on it
//! must not disturb what is about to be captured); this window does, for as
//! long as it is open. Enter keeps the text, Esc doesn't; clicking elsewhere
//! keeps it.

use std::cell::Cell;

use whir_core::theme::Theme;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    ANTIALIASED_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, CreateSolidBrush, DEFAULT_CHARSET,
    DeleteObject, FW_NORMAL, HBRUSH, HDC, HFONT, HGDIOBJ, OUT_DEFAULT_PRECIS, SetBkColor,
    SetTextColor,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{EM_LIMITTEXT, EM_SETMARGINS, EM_SETSEL};
use windows::Win32::UI::Input::KeyboardAndMouse::{SetFocus, VK_ESCAPE, VK_RETURN};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, EC_LEFTMARGIN, EC_RIGHTMARGIN, ES_AUTOHSCROLL,
    GWLP_USERDATA, GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW, MSG, PostMessageW,
    RegisterClassExW, SW_SHOW, SendMessageW, SetForegroundWindow, SetWindowLongPtrW, ShowWindow,
    WA_INACTIVE, WINDOW_STYLE, WM_ACTIVATE, WM_APP, WM_CTLCOLOREDIT, WM_KEYDOWN, WM_SETFOCUS,
    WM_SETFONT, WNDCLASSEXW, WS_CHILD, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
};
use windows::core::{HSTRING, PCWSTR, w};

use crate::geom::Rect;

/// Posted to the dock when typing is over: `wParam` 1 keeps the text, 0 not.
pub const WM_NOTE_DONE: u32 = WM_APP + 5;

const CLASS: PCWSTR = w!("whir.dock.note.v1");

thread_local! {
    /// The open edit box, for the keys the message loop hands us first.
    static EDIT: Cell<HWND> = const { Cell::new(HWND(std::ptr::null_mut())) };
    /// Its background, for `WM_CTLCOLOREDIT`.
    static BRUSH: Cell<HBRUSH> = const { Cell::new(HBRUSH(std::ptr::null_mut())) };
}

pub struct NoteEdit {
    popup: HWND,
    edit: HWND,
    font: HFONT,
    brush: HBRUSH,
}

fn colorref(c: whir_core::theme::Color32) -> COLORREF {
    let [r, g, b, _] = c.to_srgba_unmultiplied();
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

impl NoteEdit {
    /// Open over `row` (screen pixels) with `text` in it, all selected; the
    /// dock (`owner`) hears [`WM_NOTE_DONE`] when it is over.
    pub fn open(owner: HWND, row: Rect, text: &str, theme: &Theme, px: i32) -> Option<NoteEdit> {
        unsafe {
            let instance: HINSTANCE = GetModuleHandleW(PCWSTR::null()).ok()?.into();
            let brush = CreateSolidBrush(colorref(theme.drawer_item_hover));
            BRUSH.set(brush);
            RegisterClassExW(&WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wndproc),
                hInstance: instance,
                lpszClassName: CLASS,
                ..Default::default()
            });
            // A line of text and a little room, in the middle of the row.
            let height = (px * 3 / 2).min(row.height());
            let top = row.top + (row.height() - height) / 2;
            let rect = Rect::new(row.left, top, row.right, top + height);
            let popup = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                CLASS,
                w!("whir note"),
                WS_POPUP,
                rect.left,
                rect.top,
                rect.width(),
                rect.height(),
                Some(owner),
                None,
                Some(instance),
                None,
            )
            .ok()?;
            SetWindowLongPtrW(popup, GWLP_USERDATA, owner.0 as isize);
            let font = CreateFontW(
                -px,
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
                ANTIALIASED_QUALITY,
                0,
                w!("Segoe UI"),
            );
            let edit = CreateWindowExW(
                Default::default(),
                w!("EDIT"),
                &HSTRING::from(text),
                WS_CHILD | WS_VISIBLE | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
                0,
                0,
                rect.width(),
                rect.height(),
                Some(popup),
                None,
                Some(instance),
                None,
            )
            .ok()?;
            SendMessageW(
                edit,
                WM_SETFONT,
                Some(WPARAM(font.0 as usize)),
                Some(LPARAM(1)),
            );
            let margin = (px / 2) as usize;
            SendMessageW(
                edit,
                EM_SETMARGINS,
                Some(WPARAM((EC_LEFTMARGIN | EC_RIGHTMARGIN) as usize)),
                Some(LPARAM((margin | margin << 16) as isize)),
            );
            SendMessageW(edit, EM_LIMITTEXT, Some(WPARAM(500)), None);
            SendMessageW(edit, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
            EDIT.set(edit);
            let _ = ShowWindow(popup, SW_SHOW);
            let _ = SetForegroundWindow(popup);
            let _ = SetFocus(Some(edit));
            Some(NoteEdit {
                popup,
                edit,
                font,
                brush,
            })
        }
    }

    pub fn text(&self) -> String {
        unsafe {
            let n = GetWindowTextLengthW(self.edit).max(0) as usize;
            let mut buf = vec![0u16; n + 1];
            let got = GetWindowTextW(self.edit, &mut buf).max(0) as usize;
            String::from_utf16_lossy(&buf[..got])
        }
    }
}

impl Drop for NoteEdit {
    fn drop(&mut self) {
        EDIT.set(HWND(std::ptr::null_mut()));
        BRUSH.set(HBRUSH(std::ptr::null_mut()));
        unsafe {
            let _ = DestroyWindow(self.popup);
            let _ = DeleteObject(HGDIOBJ(self.font.0));
            let _ = DeleteObject(HGDIOBJ(self.brush.0));
        }
    }
}

/// Enter and Esc in the edit box, seen by the message loop before the box
/// gets them (a single-line edit box would just beep). True if handled.
pub fn intercept(msg: &MSG) -> bool {
    let edit = EDIT.get();
    if edit.is_invalid() || msg.hwnd != edit || msg.message != WM_KEYDOWN {
        return false;
    }
    let keep = match msg.wParam.0 as u16 {
        k if k == VK_RETURN.0 => 1,
        k if k == VK_ESCAPE.0 => 0,
        _ => return false,
    };
    done(msg.hwnd, keep);
    true
}

/// Tell the dock (the popup's owner) that typing is over.
fn done(any: HWND, keep: usize) {
    unsafe {
        let mut popup = any;
        let mut owner = GetWindowLongPtrW(popup, GWLP_USERDATA);
        if owner == 0 {
            // From the edit box: its popup knows the dock.
            popup = windows::Win32::UI::WindowsAndMessaging::GetParent(any).unwrap_or_default();
            owner = GetWindowLongPtrW(popup, GWLP_USERDATA);
        }
        if owner != 0 {
            let _ = PostMessageW(
                Some(HWND(owner as *mut _)),
                WM_NOTE_DONE,
                WPARAM(keep),
                LPARAM(0),
            );
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_CTLCOLOREDIT => {
                let dc = HDC(wparam.0 as *mut _);
                let theme = if whir_platform::os::prefers_dark() {
                    Theme::dark()
                } else {
                    Theme::light()
                };
                SetTextColor(dc, colorref(theme.text));
                SetBkColor(dc, colorref(theme.drawer_item_hover));
                LRESULT(BRUSH.get().0 as isize)
            }
            WM_SETFOCUS => {
                let _ = SetFocus(Some(EDIT.get()));
                LRESULT(0)
            }
            // Clicked away: keep what was typed.
            WM_ACTIVATE if (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE => {
                done(hwnd, 1);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}
