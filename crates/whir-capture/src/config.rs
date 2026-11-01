//! What the dock remembers, in `%LOCALAPPDATA%\whir\dock.ini`: where it sits,
//! the delay, the capture hotkey, and whether it is hidden. (Its own file: the
//! main window owns `settings.ini`, and two processes must not write one file.)

use whir_core::settings::{IniFile, app_data_file};

use crate::place::Edge;

/// `RegisterHotKey` modifier bits.
pub const MOD_ALT: u32 = 1;
pub const MOD_CONTROL: u32 = 2;
pub const MOD_SHIFT: u32 = 4;
const VK_SNAPSHOT: u32 = 0x2C;
const VK_F1: u32 = 0x70;

/// A system-wide key combination.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hotkey {
    pub mods: u32,
    pub vk: u32,
}

impl Hotkey {
    /// Ctrl+PrtSc: leaves PrtSc, Alt+PrtSc and Win+Shift+S to Windows.
    pub const DEFAULT: Hotkey = Hotkey {
        mods: MOD_CONTROL,
        vk: VK_SNAPSHOT,
    };
}

/// "ctrl+shift+prtsc", "ctrl+alt+f9", "ctrl+alt+s". Needs Ctrl or Alt (a
/// bare key, or Shift with a letter, is somebody typing), and never the
/// Windows key, which belongs to Windows.
pub fn parse_hotkey(text: &str) -> Option<Hotkey> {
    let mut mods = 0;
    let mut vk = None;
    for word in text.split('+').map(|w| w.trim().to_ascii_lowercase()) {
        let key = match word.as_str() {
            "ctrl" | "control" => {
                mods |= MOD_CONTROL;
                continue;
            }
            "alt" => {
                mods |= MOD_ALT;
                continue;
            }
            "shift" => {
                mods |= MOD_SHIFT;
                continue;
            }
            "prtsc" | "prtscn" | "printscreen" => VK_SNAPSHOT,
            w if w.len() == 1 && w.as_bytes()[0].is_ascii_alphanumeric() => {
                w.as_bytes()[0].to_ascii_uppercase() as u32
            }
            w => {
                let n: u32 = w.strip_prefix('f')?.parse().ok()?;
                if !(1..=24).contains(&n) {
                    return None;
                }
                VK_F1 + n - 1
            }
        };
        // One key only.
        if vk.replace(key).is_some() {
            return None;
        }
    }
    let vk = vk?;
    (mods & (MOD_CONTROL | MOD_ALT) != 0).then_some(Hotkey { mods, vk })
}

/// "Ctrl+Shift+PrtSc", as menus show it (and [`parse_hotkey`] reads it).
pub fn format_hotkey(h: Hotkey) -> String {
    let mut out = String::new();
    for (bit, name) in [
        (MOD_CONTROL, "Ctrl"),
        (MOD_ALT, "Alt"),
        (MOD_SHIFT, "Shift"),
    ] {
        if h.mods & bit != 0 {
            out.push_str(name);
            out.push('+');
        }
    }
    match h.vk {
        VK_SNAPSHOT => out.push_str("PrtSc"),
        vk if (VK_F1..VK_F1 + 24).contains(&vk) => out.push_str(&format!("F{}", vk - VK_F1 + 1)),
        vk => out.push(char::from_u32(vk).unwrap_or('?')),
    }
    out
}

/// Seconds a delayed capture waits; the Delay button steps through these.
pub const DELAYS: [u32; 3] = [0, 3, 5];

/// What Settings can ask of the dock, which owns `dock.ini`. Small enough to go
/// in a posted message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    /// One of [`DELAYS`].
    Delay(u32),
    Hotkey(Option<Hotkey>),
}

impl Ask {
    /// As `wParam` and `lParam`.
    pub fn encode(self) -> (usize, isize) {
        match self {
            Ask::Delay(seconds) => (1, seconds as isize),
            Ask::Hotkey(None) => (2, 0),
            Ask::Hotkey(Some(h)) => (2, ((h.mods << 16) | h.vk) as isize),
        }
    }

    pub fn decode(kind: usize, value: isize) -> Option<Ask> {
        let value = value as u32;
        match (kind, value) {
            (1, seconds) => Some(Ask::Delay(seconds)),
            (2, 0) => Some(Ask::Hotkey(None)),
            (2, packed) => Some(Ask::Hotkey(Some(Hotkey {
                mods: packed >> 16,
                vk: packed & 0xffff,
            }))),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DockConfig {
    /// `None`: never placed, so the dock picks its default spot.
    pub spot: Option<Spot>,
    pub delay: u32,
    /// `None`: switched off (`hotkey = off`).
    pub hotkey: Option<Hotkey>,
    /// Windows gave it to us: another program can hold a combination.
    pub hotkey_ok: bool,
    /// Hidden (from its own menu or the tray's): only the tray icon shows.
    pub hidden: bool,
    /// Starting with Windows was set up once (after that it is the user's
    /// choice, and the Run entry itself says what it is).
    pub autostart_set: bool,
}

/// Where the dock was last put.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spot {
    /// The monitor's device name.
    pub monitor: String,
    pub edge: Edge,
    /// Thousandths of the way along the edge.
    pub offset: u16,
}

impl DockConfig {
    pub fn load() -> Self {
        Self::read(&IniFile::load(app_data_file("dock.ini")))
    }

    pub fn save(&self) {
        let mut ini = IniFile::load(app_data_file("dock.ini"));
        self.write(&mut ini);
        let _ = ini.save();
    }

    /// Anything missing or unreadable falls back to its default.
    fn read(ini: &IniFile) -> Self {
        let spot = (|| {
            Some(Spot {
                monitor: ini.get("monitor")?.to_owned(),
                edge: Edge::from_name(ini.get("edge")?)?,
                offset: ini.get("offset")?.parse::<u16>().ok()?.min(1000),
            })
        })();
        let delay = ini
            .get("delay")
            .and_then(|d| d.parse().ok())
            .filter(|d| DELAYS.contains(d))
            .unwrap_or(0);
        let hotkey = match ini.get("hotkey") {
            Some("off") => None,
            Some(text) => Some(parse_hotkey(text).unwrap_or(Hotkey::DEFAULT)),
            None => Some(Hotkey::DEFAULT),
        };
        let yes = |key| ini.get(key).is_some_and(|v| v == "yes");
        Self {
            spot,
            delay,
            hotkey,
            hotkey_ok: ini.get("hotkey-ok") != Some("no"),
            hidden: yes("hidden"),
            autostart_set: yes("autostart-set"),
        }
    }

    fn write(&self, ini: &mut IniFile) {
        if let Some(spot) = &self.spot {
            ini.set("monitor", &spot.monitor);
            ini.set("edge", spot.edge.name());
            ini.set("offset", &spot.offset.to_string());
        }
        ini.set("delay", &self.delay.to_string());
        let hotkey = self.hotkey.map(format_hotkey);
        ini.set("hotkey", hotkey.as_deref().unwrap_or("off"));
        let yes_no = |b| if b { "yes" } else { "no" };
        ini.set("hotkey-ok", yes_no(self.hotkey_ok));
        ini.set("hidden", yes_no(self.hidden));
        ini.set("autostart-set", yes_no(self.autostart_set));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_settings_asks_fits_in_a_message() {
        for ask in [
            Ask::Delay(0),
            Ask::Delay(5),
            Ask::Hotkey(None),
            Ask::Hotkey(Some(Hotkey::DEFAULT)),
            Ask::Hotkey(parse_hotkey("ctrl+alt+shift+f24")),
        ] {
            let (kind, value) = ask.encode();
            assert_eq!(Ask::decode(kind, value), Some(ask));
        }
        assert_eq!(Ask::decode(0, 0), None);
    }

    #[test]
    fn a_shortcut_windows_refused_is_remembered_and_starts_out_fine() {
        let mut ini = IniFile::load(None);
        assert!(DockConfig::read(&ini).hotkey_ok);
        let mut config = DockConfig::read(&ini);
        config.hotkey_ok = false;
        config.write(&mut ini);
        assert!(!DockConfig::read(&ini).hotkey_ok);
    }

    #[test]
    fn hotkeys_parse_and_print() {
        let h = parse_hotkey("ctrl+prtsc").unwrap();
        assert_eq!(h, Hotkey::DEFAULT);
        assert_eq!(format_hotkey(h), "Ctrl+PrtSc");
        for text in [
            "Ctrl+Shift+PrtSc",
            "Ctrl+Alt+F9",
            "Alt+S",
            "Ctrl+Alt+Shift+F24",
        ] {
            let h = parse_hotkey(text).unwrap_or_else(|| panic!("{text}"));
            assert_eq!(format_hotkey(h), text);
        }
        assert_eq!(
            parse_hotkey(" Control + PrintScreen "),
            Some(Hotkey::DEFAULT)
        );
    }

    #[test]
    fn combinations_that_would_get_in_the_way_are_refused() {
        for text in [
            "",
            "prtsc",       // Windows' own
            "s",           // typing
            "shift+s",     // typing
            "shift+prtsc", // no Ctrl or Alt
            "win+s",       // Windows' own
            "ctrl",        // no key
            "ctrl+a+b",    // two keys
            "ctrl+f25",
            "ctrl+f0",
            "ctrl+banana",
        ] {
            assert_eq!(parse_hotkey(text), None, "{text:?}");
        }
    }

    fn ini(text: &str) -> IniFile {
        let dir = std::env::temp_dir().join(format!("whir-dock-ini-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!(
            "{:x}.ini",
            text.len() * 31 + text.bytes().map(|b| b as usize).sum::<usize>()
        ));
        std::fs::write(&path, text).unwrap();
        let ini = IniFile::load(Some(path.clone()));
        let _ = std::fs::remove_file(path);
        ini
    }

    #[test]
    fn a_new_dock_has_no_spot_no_delay_and_the_default_hotkey() {
        let c = DockConfig::read(&ini(""));
        assert_eq!(
            c,
            DockConfig {
                spot: None,
                delay: 0,
                hotkey: Some(Hotkey::DEFAULT),
                hotkey_ok: true,
                hidden: false,
                autostart_set: false,
            }
        );
    }

    #[test]
    fn what_is_written_is_read_back() {
        let c = DockConfig {
            spot: Some(Spot {
                monitor: r"\\.\DISPLAY2".to_owned(),
                edge: Edge::Top,
                offset: 250,
            }),
            delay: 5,
            hotkey: parse_hotkey("ctrl+alt+f9"),
            hotkey_ok: false,
            hidden: true,
            autostart_set: true,
        };
        let mut file = ini("# kept\nfuture = thing\n");
        c.write(&mut file);
        assert_eq!(DockConfig::read(&file), c);
        assert_eq!(file.get("future"), Some("thing"));

        let off = DockConfig {
            hotkey: None,
            ..c.clone()
        };
        off.write(&mut file);
        assert_eq!(DockConfig::read(&file), off);
    }

    #[test]
    fn nonsense_falls_back_to_defaults() {
        let c = DockConfig::read(&ini(
            "monitor = x\nedge = middle\noffset = 12\ndelay = 4\nhotkey = banana\n",
        ));
        assert_eq!(c.spot, None);
        assert_eq!(c.delay, 0);
        assert_eq!(c.hotkey, Some(Hotkey::DEFAULT));
        // An offset past the end is the end.
        let c = DockConfig::read(&ini("monitor = x\nedge = left\noffset = 5000\n"));
        assert_eq!(c.spot.unwrap().offset, 1000);
    }
}
