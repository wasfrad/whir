//! Win32 input messages → egui events. Mirrors egui-winit's conventions.

use std::time::Instant;

use egui::{Event, Key, Modifiers, MouseWheelUnit, PointerButton, Pos2, TouchPhase, vec2};
use windows::Win32::UI::Input::KeyboardAndMouse::*;

/// Accumulates egui events between frames.
pub struct InputCollector {
    events: Vec<Event>,
    start: Instant,
    /// Physical pixels per egui point.
    pub pixels_per_point: f32,
    pub focused: bool,
    pointer_px: Option<Pos2>,
    high_surrogate: Option<u16>,
    buttons_down: u8,
    last_modifiers: Modifiers,
}

impl InputCollector {
    pub fn new(pixels_per_point: f32) -> Self {
        Self {
            events: Vec::with_capacity(32),
            start: Instant::now(),
            pixels_per_point,
            focused: true,
            pointer_px: None,
            high_surrogate: None,
            buttons_down: 0,
            last_modifiers: Modifiers::default(),
        }
    }

    /// egui learns modifier state only through `ModifiersChanged` events.
    fn sync_modifiers(&mut self) -> Modifiers {
        let mods = modifiers();
        if mods != self.last_modifiers {
            self.last_modifiers = mods;
            self.events.push(Event::ModifiersChanged(mods));
        }
        mods
    }

    pub fn has_events(&self) -> bool {
        !self.events.is_empty()
    }

    pub fn buttons_down(&self) -> bool {
        self.buttons_down != 0
    }

    /// Build the `RawInput` for the next frame, draining collected events.
    pub fn take_raw_input(&mut self, size_px: [u32; 2]) -> egui::RawInput {
        let ppp = self.pixels_per_point;
        let mut raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                Pos2::ZERO,
                vec2(size_px[0] as f32 / ppp, size_px[1] as f32 / ppp),
            )),
            time: Some(self.start.elapsed().as_secs_f64()),
            events: std::mem::take(&mut self.events),
            focused: self.focused,
            ..Default::default()
        };
        if let Some(vp) = raw.viewports.get_mut(&egui::ViewportId::ROOT) {
            vp.native_pixels_per_point = Some(ppp);
            vp.focused = Some(self.focused);
        }
        raw
    }

    fn to_points(&self, x_px: f32, y_px: f32) -> Pos2 {
        Pos2::new(x_px / self.pixels_per_point, y_px / self.pixels_per_point)
    }

    pub fn pointer_moved(&mut self, x_px: i32, y_px: i32) {
        let px = Pos2::new(x_px as f32, y_px as f32);
        if self.pointer_px == Some(px) {
            return;
        }
        self.pointer_px = Some(px);
        let pos = self.to_points(px.x, px.y);
        self.events.push(Event::PointerMoved(pos));
    }

    pub fn pointer_gone(&mut self) {
        if self.pointer_px.take().is_some() {
            self.events.push(Event::PointerGone);
        }
    }

    pub fn pointer_button(&mut self, x_px: i32, y_px: i32, button: PointerButton, pressed: bool) {
        self.pointer_moved(x_px, y_px);
        let bit = 1u8 << (button as u8);
        if pressed {
            self.buttons_down |= bit;
        } else {
            self.buttons_down &= !bit;
        }
        let pos = self.to_points(x_px as f32, y_px as f32);
        let modifiers = self.sync_modifiers();
        self.events.push(Event::PointerButton {
            pos,
            button,
            pressed,
            modifiers,
        });
    }

    /// `delta` in wheel notches (WHEEL_DELTA units), positive = away from the user / right.
    pub fn wheel(&mut self, dx_notches: f32, dy_notches: f32) {
        let modifiers = self.sync_modifiers();
        self.events.push(Event::MouseWheel {
            unit: MouseWheelUnit::Line,
            delta: vec2(dx_notches, dy_notches),
            phase: TouchPhase::Move,
            modifiers,
        });
    }

    pub fn focus(&mut self, focused: bool) {
        self.focused = focused;
        self.events.push(Event::WindowFocused(focused));
        if !focused {
            self.buttons_down = 0;
            self.high_surrogate = None;
            self.last_modifiers = Modifiers::default();
        }
    }

    /// We lost mouse capture mid-drag: release everything so egui doesn't stay stuck dragging.
    pub fn release_all_buttons(&mut self) {
        let Some(pos) = self.pointer_px else {
            self.buttons_down = 0;
            return;
        };
        for button in [
            PointerButton::Primary,
            PointerButton::Secondary,
            PointerButton::Middle,
            PointerButton::Extra1,
            PointerButton::Extra2,
        ] {
            if self.buttons_down & (1 << button as u8) != 0 {
                self.pointer_button(pos.x as i32, pos.y as i32, button, false);
            }
        }
        self.buttons_down = 0;
    }

    /// Modifier keys themselves produce no egui key event, but egui must still see the change.
    pub fn modifier_key(&mut self) {
        self.sync_modifiers();
    }

    /// A key went down or up. Clipboard shortcuts become `Copy`/`Cut`/`Paste`
    /// events *instead of* key events, matching egui-winit.
    pub fn key(&mut self, vk: u16, pressed: bool, repeat: bool) {
        let mods = self.sync_modifiers();
        let Some(key) = vk_to_key(VIRTUAL_KEY(vk)) else {
            return;
        };
        if pressed {
            let clip = if mods.ctrl && !mods.shift && !mods.alt {
                match key {
                    Key::C | Key::Insert => Some(Event::Copy),
                    Key::X => Some(Event::Cut),
                    Key::V => Some(Event::Paste(
                        crate::clipboard::get_text().unwrap_or_default(),
                    )),
                    _ => None,
                }
            } else if mods.shift && !mods.ctrl && !mods.alt {
                match key {
                    Key::Insert => Some(Event::Paste(
                        crate::clipboard::get_text().unwrap_or_default(),
                    )),
                    Key::Delete => Some(Event::Cut),
                    _ => None,
                }
            } else {
                None
            };
            if let Some(ev) = clip {
                if !matches!(&ev, Event::Paste(s) if s.is_empty()) {
                    self.events.push(ev);
                }
                return;
            }
        }
        self.events.push(Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat,
            modifiers: mods,
        });
    }

    /// One UTF-16 code unit from `WM_CHAR`.
    pub fn char(&mut self, unit: u16) {
        let mods = modifiers();
        // Ctrl+letter produces control characters; AltGr (= Ctrl+Alt) produces real text.
        if mods.ctrl && !mods.alt {
            self.high_surrogate = None;
            return;
        }
        let ch = match unit {
            0xD800..=0xDBFF => {
                self.high_surrogate = Some(unit);
                return;
            }
            0xDC00..=0xDFFF => match self.high_surrogate.take() {
                Some(hi) => char::decode_utf16([hi, unit]).next().and_then(Result::ok),
                None => None,
            },
            _ => char::from_u32(unit as u32),
        };
        let Some(ch) = ch else { return };
        if is_printable(ch) {
            self.events.push(Event::Text(ch.to_string()));
        }
    }
}

fn is_printable(ch: char) -> bool {
    let private_use = ('\u{e000}'..='\u{f8ff}').contains(&ch)
        || ('\u{f0000}'..='\u{ffffd}').contains(&ch)
        || ('\u{100000}'..='\u{10fffd}').contains(&ch);
    !private_use && !ch.is_ascii_control()
}

pub fn modifiers() -> Modifiers {
    let down = |vk: VIRTUAL_KEY| unsafe { GetKeyState(vk.0 as i32) } < 0;
    let ctrl = down(VK_CONTROL);
    Modifiers {
        alt: down(VK_MENU),
        ctrl,
        shift: down(VK_SHIFT),
        mac_cmd: false,
        command: ctrl,
    }
}

pub fn vk_to_key(vk: VIRTUAL_KEY) -> Option<Key> {
    Some(match vk {
        VK_LEFT => Key::ArrowLeft,
        VK_RIGHT => Key::ArrowRight,
        VK_UP => Key::ArrowUp,
        VK_DOWN => Key::ArrowDown,
        VK_ESCAPE => Key::Escape,
        VK_TAB => Key::Tab,
        VK_BACK => Key::Backspace,
        VK_RETURN => Key::Enter,
        VK_SPACE => Key::Space,
        VK_INSERT => Key::Insert,
        VK_DELETE => Key::Delete,
        VK_HOME => Key::Home,
        VK_END => Key::End,
        VK_PRIOR => Key::PageUp,
        VK_NEXT => Key::PageDown,
        VK_OEM_MINUS | VK_SUBTRACT => Key::Minus,
        VK_OEM_PLUS => Key::Equals,
        VK_ADD => Key::Plus,
        VK_OEM_COMMA => Key::Comma,
        VK_OEM_PERIOD | VK_DECIMAL => Key::Period,
        VK_OEM_1 => Key::Semicolon,
        VK_OEM_2 | VK_DIVIDE => Key::Slash,
        VK_OEM_3 => Key::Backtick,
        VK_OEM_4 => Key::OpenBracket,
        VK_OEM_5 => Key::Backslash,
        VK_OEM_6 => Key::CloseBracket,
        VK_OEM_7 => Key::Quote,
        VK_BROWSER_BACK => Key::BrowserBack,
        VK_F1 => Key::F1,
        VK_F2 => Key::F2,
        VK_F3 => Key::F3,
        VK_F4 => Key::F4,
        VK_F5 => Key::F5,
        VK_F6 => Key::F6,
        VK_F7 => Key::F7,
        VK_F8 => Key::F8,
        VK_F9 => Key::F9,
        VK_F10 => Key::F10,
        VK_F11 => Key::F11,
        VK_F12 => Key::F12,
        VIRTUAL_KEY(v @ 0x30..=0x39) => DIGITS[(v - 0x30) as usize],
        VIRTUAL_KEY(v @ 0x60..=0x69) => DIGITS[(v - 0x60) as usize],
        VIRTUAL_KEY(v @ 0x41..=0x5A) => LETTERS[(v - 0x41) as usize],
        _ => return None,
    })
}

const DIGITS: [Key; 10] = [
    Key::Num0,
    Key::Num1,
    Key::Num2,
    Key::Num3,
    Key::Num4,
    Key::Num5,
    Key::Num6,
    Key::Num7,
    Key::Num8,
    Key::Num9,
];

const LETTERS: [Key; 26] = [
    Key::A,
    Key::B,
    Key::C,
    Key::D,
    Key::E,
    Key::F,
    Key::G,
    Key::H,
    Key::I,
    Key::J,
    Key::K,
    Key::L,
    Key::M,
    Key::N,
    Key::O,
    Key::P,
    Key::Q,
    Key::R,
    Key::S,
    Key::T,
    Key::U,
    Key::V,
    Key::W,
    Key::X,
    Key::Y,
    Key::Z,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_common_keys() {
        assert_eq!(vk_to_key(VIRTUAL_KEY(0x41)), Some(Key::A));
        assert_eq!(vk_to_key(VIRTUAL_KEY(0x5A)), Some(Key::Z));
        assert_eq!(vk_to_key(VIRTUAL_KEY(0x30)), Some(Key::Num0));
        assert_eq!(vk_to_key(VIRTUAL_KEY(0x69)), Some(Key::Num9));
        assert_eq!(vk_to_key(VK_PRIOR), Some(Key::PageUp));
        assert_eq!(vk_to_key(VK_F3), Some(Key::F3));
        assert_eq!(vk_to_key(VK_LWIN), None);
    }

    #[test]
    fn surrogate_pairs_join() {
        let mut c = InputCollector::new(1.0);
        // U+1F600 😀 = D83D DE00
        c.char(0xD83D);
        c.char(0xDE00);
        c.char(b'a' as u16);
        c.char(0x08); // backspace control char: dropped
        let raw = c.take_raw_input([10, 10]);
        let texts: Vec<_> = raw
            .events
            .iter()
            .filter_map(|e| match e {
                Event::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, ["😀", "a"]);
    }
}
