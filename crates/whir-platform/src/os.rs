//! Small OS queries.

use whir_core::theme::Mode;
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::core::w;

/// Dark or light UI: `WHIR_THEME=dark|light` overrides the theme picked in
/// Settings, which overrides the system's.
pub fn prefers_dark() -> bool {
    dark_in(Mode::saved())
}

/// [`prefers_dark`] for the dock, as `settings.ini` has it now.
pub fn prefers_dark_on_disk() -> bool {
    dark_in(Mode::on_disk())
}

fn dark_in(mode: Mode) -> bool {
    match std::env::var("WHIR_THEME").as_deref() {
        Ok("dark") => return true,
        Ok("light") => return false,
        _ => {}
    }
    match mode {
        Mode::System => system_uses_dark_theme(),
        Mode::Light => false,
        Mode::Dark => true,
    }
}

/// Whether apps should use dark mode (Settings → Personalization → Colors).
pub fn system_uses_dark_theme() -> bool {
    let mut value = 1u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut _),
            Some(&mut size),
        )
    };
    r.is_ok() && value == 0
}
