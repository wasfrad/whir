//! Key matching with exact modifiers.
//!
//! egui's `consume_key` ignores extra Shift/Alt, so `consume_key(NONE, Enter)`
//! would also swallow Shift+Enter and Alt+Enter. The grid and the cell editor
//! give those different meanings, so they match exactly.

use egui::{Event, InputState, Key, Modifiers};

/// Remove the first press of `key` with exactly `mods`; true if there was one.
pub fn consume_exact(i: &mut InputState, mods: Modifiers, key: Key) -> bool {
    let pos = i.events.iter().position(|e| {
        matches!(e, Event::Key { key: k, pressed: true, modifiers, .. }
            if *k == key && modifiers.matches_exact(mods))
    });
    match pos {
        Some(p) => {
            i.events.remove(p);
            true
        }
        None => false,
    }
}

/// Remove and join this frame's typed text.
pub fn take_text(i: &mut InputState) -> String {
    let mut out = String::new();
    i.events.retain(|e| match e {
        Event::Text(t) => {
            out.push_str(t);
            false
        }
        _ => true,
    });
    out
}

pub const CTRL: Modifiers = Modifiers::CTRL;
pub const SHIFT: Modifiers = Modifiers::SHIFT;
pub const CTRL_SHIFT: Modifiers = Modifiers {
    ctrl: true,
    shift: true,
    ..Modifiers::NONE
};
pub const NONE: Modifiers = Modifiers::NONE;

#[cfg(test)]
mod tests {
    use super::*;

    fn input_with(events: Vec<Event>) -> InputState {
        let raw = egui::RawInput {
            events,
            ..Default::default()
        };
        InputState::default().begin_pass(raw, false, 1.0, egui::InputOptions::default())
    }

    fn key(key: Key, modifiers: Modifiers) -> Event {
        Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    #[test]
    fn exact_modifiers_only() {
        let mut i = input_with(vec![key(Key::Enter, Modifiers::ALT)]);
        assert!(!consume_exact(&mut i, NONE, Key::Enter));
        assert!(consume_exact(&mut i, Modifiers::ALT, Key::Enter));
        let mut i = input_with(vec![key(Key::Z, CTRL_SHIFT)]);
        assert!(!consume_exact(&mut i, CTRL, Key::Z));
        assert!(consume_exact(&mut i, CTRL_SHIFT, Key::Z));
        assert!(!consume_exact(&mut i, CTRL_SHIFT, Key::Z), "consumed once");
    }

    #[test]
    fn typed_text_is_taken() {
        let mut i = input_with(vec![
            Event::Text("a".into()),
            key(Key::B, NONE),
            Event::Text("b".into()),
        ]);
        assert_eq!(take_text(&mut i), "ab");
        assert_eq!(i.events.len(), 1);
    }
}
