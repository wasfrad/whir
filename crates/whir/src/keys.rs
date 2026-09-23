//! Shortcut matching with exact modifiers.
//!
//! egui's `consume_key` ignores extra Shift/Alt, so `Ctrl+N` would also fire for
//! Ctrl+Shift+N — and for AltGr+N on layouts where that types a letter.

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

#[cfg(test)]
mod tests {
    use super::*;

    fn input_with(mods: Modifiers, key: Key) -> InputState {
        let raw = egui::RawInput {
            events: vec![Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: mods,
            }],
            ..Default::default()
        };
        let mut i = InputState::default();
        i = i.begin_pass(raw, false, 1.0, egui::InputOptions::default());
        i
    }

    #[test]
    fn exact_modifiers_only() {
        let ctrl_shift = Modifiers::CTRL | Modifiers::SHIFT;
        let mut i = input_with(ctrl_shift, Key::N);
        assert!(!consume_exact(&mut i, Modifiers::CTRL, Key::N));
        assert!(consume_exact(&mut i, ctrl_shift, Key::N));
        assert!(!consume_exact(&mut i, ctrl_shift, Key::N), "consumed once");

        let ctrl_alt = Modifiers::CTRL | Modifiers::ALT;
        let mut i = input_with(ctrl_alt, Key::N);
        assert!(!consume_exact(&mut i, Modifiers::CTRL, Key::N));

        let mut i = input_with(Modifiers::CTRL, Key::N);
        assert!(consume_exact(&mut i, Modifiers::CTRL, Key::N));
    }
}
