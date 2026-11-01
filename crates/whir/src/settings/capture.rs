//! The capture dock: its shortcut, its delay, and whether it shows. These are
//! the dock's own (`dock.ini`, which only it writes), so this page reads them
//! and asks it to change them.

use std::time::Duration;

use egui::{Event, Key, Modifiers, RichText};
use whir_capture::{Ask, DELAYS, DockConfig, Hotkey, format_hotkey, parse_hotkey};
use whir_core::DocCx;
use whir_core::kit;
use whir_core::theme::Theme;

use super::toast;

/// What's on the shortcut's ⋯: the combinations that can't be pressed here,
/// because Windows keeps PrtSc from the window.
const PRINT_SCREEN: [&str; 2] = ["Ctrl+PrtSc", "Ctrl+Shift+PrtSc"];
const CHANGE_WIDTH: f32 = 126.0;

#[derive(Default)]
pub struct View {
    /// `dock.ini`, as last read.
    config: Option<DockConfig>,
    /// The next keys pressed are the new shortcut.
    recording: bool,
    /// Why the keys pressed won't do.
    problem: Option<String>,
}

/// The combination `key` makes with `modifiers`, if it's one the dock can have.
fn combination(key: Key, modifiers: Modifiers) -> Option<Hotkey> {
    let mut text = String::new();
    for (held, name) in [
        (modifiers.ctrl, "ctrl+"),
        (modifiers.alt, "alt+"),
        (modifiers.shift, "shift+"),
    ] {
        if held {
            text.push_str(name);
        }
    }
    text.push_str(key.name());
    parse_hotkey(&text)
}

fn delay_label(seconds: u32) -> String {
    match seconds {
        0 => "None".to_owned(),
        s => format!("{s} seconds"),
    }
}

impl View {
    /// `dock.ini` changed: what was read from it goes.
    pub fn forget(&mut self) {
        self.config = None;
    }

    pub fn recording(&self) -> bool {
        self.recording
    }

    /// While recording, the keys pressed: Esc lets go, anything else is either
    /// the new shortcut or said to be no good.
    fn record(&mut self, ui: &mut egui::Ui, config: &mut DockConfig) {
        let pressed = ui.input_mut(|i| {
            let mut pressed = None;
            i.events.retain(|e| match e {
                Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    pressed.get_or_insert((*key, *modifiers));
                    false
                }
                Event::Key { .. } | Event::Text(_) => false,
                _ => true,
            });
            pressed
        });
        match pressed {
            None => {}
            Some((Key::Escape, _)) => self.recording = false,
            Some((key, modifiers)) => match combination(key, modifiers) {
                Some(hotkey) => {
                    self.recording = false;
                    self.problem = None;
                    set_hotkey(config, Some(hotkey));
                }
                None => {
                    self.problem = Some(
                        "Use Ctrl or Alt with a letter, a digit or a function key.".to_owned(),
                    );
                }
            },
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut DocCx<'_>, theme: &Theme) {
        kit::page_header(ui, "Capture & dock", "", theme, |_| {});
        let running = whir_capture::running();
        if !running {
            self.recording = false;
            let mut start = false;
            kit::banner(ui, theme.warning, theme, |ui| {
                ui.label(RichText::new("The capture dock isn't running.").color(theme.text));
                start = ui.button("Start it").clicked();
            });
            ui.add_space(12.0);
            if start && let Err(e) = whir_capture::launch(whir_capture::Command::Reveal) {
                toast(cx, format!("The capture dock couldn't be started: {e}"));
            }
            // Nothing says when it has started.
            ui.ctx().request_repaint_after(Duration::from_millis(500));
        }
        let mut config = self.config.take().unwrap_or_else(DockConfig::load);
        if self.recording {
            self.record(ui, &mut config);
        }

        ui.add_enabled_ui(running, |ui| {
            kit::group(ui, "Capture", theme, |g| {
                let about = self
                    .problem
                    .clone()
                    .unwrap_or_else(|| shortcut_says(&config));
                let mut change = false;
                let mut pick = None;
                g.setting("Capture shortcut", &about, CHANGE_WIDTH + 34.0, |ui| {
                    let words = if self.recording {
                        "Press the keys…"
                    } else {
                        "Change…"
                    };
                    let backwards = ui.layout().prefer_right_to_left();
                    if !backwards {
                        change = kit::action(ui, words, theme).clicked();
                    }
                    kit::more_button(ui, theme, |ui| {
                        for text in PRINT_SCREEN {
                            if kit::entry(ui, None, text, "", true) {
                                pick = Some(parse_hotkey(text));
                            }
                        }
                        ui.separator();
                        if kit::entry(ui, None, "Turn off", "", true) {
                            pick = Some(None);
                        }
                    });
                    if backwards {
                        change = kit::action(ui, words, theme).clicked();
                    }
                });
                if change {
                    self.recording = !self.recording;
                    self.problem = None;
                }
                if let Some(hotkey) = pick {
                    self.recording = false;
                    self.problem = None;
                    set_hotkey(&mut config, hotkey);
                }
                if let Some(seconds) = g.choice(
                    "Delay before a capture",
                    "Time to open a menu first. The dock's own Delay button changes this too.",
                    config.delay,
                    &DELAYS,
                    delay_label,
                ) {
                    config.delay = seconds;
                    whir_capture::ask(Ask::Delay(seconds));
                }
            });
            kit::group(ui, "Dock", theme, |g| {
                let mut shown = !config.hidden;
                if g.switch(
                    "Show the capture dock",
                    "Hidden, only its icon in the notification area is left. Screenshots, \
                     timers and todos carry on.",
                    &mut shown,
                ) {
                    config.hidden = !shown;
                    let command = if shown {
                        whir_capture::Command::Reveal
                    } else {
                        whir_capture::Command::Hide
                    };
                    if let Err(e) = whir_capture::launch(command) {
                        toast(cx, format!("The capture dock couldn't be changed: {e}"));
                    }
                }
            });
        });
        self.config = Some(config);
    }
}

/// Ask for `hotkey`, and show it meanwhile: it's the dock that finds out whether
/// Windows will give it to us.
fn set_hotkey(config: &mut DockConfig, hotkey: Option<Hotkey>) {
    config.hotkey = hotkey;
    config.hotkey_ok = true;
    whir_capture::ask(Ask::Hotkey(hotkey));
}

/// What the shortcut is, and whether it works.
fn shortcut_says(config: &DockConfig) -> String {
    match config.hotkey.map(format_hotkey) {
        None => "Off. Capture from the dock or its icon in the notification area.".to_owned(),
        Some(keys) if config.hotkey_ok => format!("{keys}, from anywhere in Windows."),
        Some(keys) => format!("Another app has {keys}, so it does nothing here. Pick another."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CTRL_ALT: Modifiers = Modifiers {
        alt: true,
        ctrl: true,
        shift: false,
        mac_cmd: false,
        command: true,
    };

    #[test]
    fn keys_pressed_make_the_combination_the_dock_reads() {
        assert_eq!(
            combination(Key::S, CTRL_ALT).map(format_hotkey).as_deref(),
            Some("Ctrl+Alt+S")
        );
        assert_eq!(
            combination(Key::F9, Modifiers::CTRL | Modifiers::SHIFT)
                .map(format_hotkey)
                .as_deref(),
            Some("Ctrl+Shift+F9")
        );
        assert_eq!(
            combination(Key::Num5, Modifiers::ALT)
                .map(format_hotkey)
                .as_deref(),
            Some("Alt+5")
        );
    }

    #[test]
    fn keys_that_would_get_in_the_way_are_refused() {
        assert_eq!(combination(Key::S, Modifiers::NONE), None);
        assert_eq!(combination(Key::S, Modifiers::SHIFT), None);
        assert_eq!(combination(Key::Enter, Modifiers::CTRL), None);
    }

    #[test]
    fn the_menu_offers_what_the_dock_can_have() {
        for text in PRINT_SCREEN {
            assert!(parse_hotkey(text).is_some(), "{text}");
        }
    }

    #[test]
    fn it_says_how_the_shortcut_stands() {
        let mut config = DockConfig::load();
        config.hotkey = Some(Hotkey::DEFAULT);
        config.hotkey_ok = true;
        assert!(shortcut_says(&config).starts_with("Ctrl+PrtSc,"));
        config.hotkey_ok = false;
        assert!(shortcut_says(&config).contains("Another app"));
        config.hotkey = None;
        assert!(shortcut_says(&config).starts_with("Off."));
    }
}
