//! `whir --register` / `--unregister`: per-user file associations (no admin).
//!
//! Registers ProgIDs, "Open with" entries and Default Apps capabilities under
//! HKCU. Windows doesn't allow programs to *set* the default app, so the user
//! finishes with "Open with → Choose another app → whir → Always".

use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey,
    RegCreateKeyExW, RegDeleteKeyValueW, RegDeleteTreeW, RegSetValueExW,
};
use windows::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};
use windows::core::{HSTRING, PCWSTR};

struct Group {
    prog_id: &'static str,
    description: &'static str,
    extensions: &'static [&'static str],
}

fn groups() -> [Group; 3] {
    [
        Group {
            prog_id: "whir.image",
            description: "Image (whir)",
            extensions: whir_image::EXTENSIONS,
        },
        Group {
            prog_id: "whir.pdf",
            description: "PDF document (whir)",
            extensions: whir_pdf::EXTENSIONS,
        },
        Group {
            prog_id: "whir.csv",
            description: "CSV file (whir)",
            extensions: whir_csv::EXTENSIONS,
        },
    ]
}

const CAPABILITIES: &str = r"Software\whir\Capabilities";

fn set_string(path: &str, name: Option<&str>, value: &str) -> Result<(), String> {
    unsafe {
        let mut key = HKEY::default();
        let r = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(path),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut key,
            None,
        );
        if r != ERROR_SUCCESS {
            return Err(format!("can't create HKCU\\{path}: {r:?}"));
        }
        let data: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes = std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 2);
        let name = name.map(HSTRING::from);
        let r = RegSetValueExW(
            key,
            name.as_ref().map_or(PCWSTR::null(), |n| PCWSTR(n.as_ptr())),
            None,
            REG_SZ,
            Some(bytes),
        );
        let _ = RegCloseKey(key);
        if r != ERROR_SUCCESS {
            return Err(format!("can't write HKCU\\{path}: {r:?}"));
        }
    }
    Ok(())
}

pub fn register() -> Result<usize, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = exe.display().to_string();
    let command = format!("\"{exe}\" \"%1\"");
    let icon = format!("\"{exe}\",0");
    let mut count = 0;

    set_string(CAPABILITIES, Some("ApplicationName"), "whir")?;
    set_string(
        CAPABILITIES,
        Some("ApplicationDescription"),
        "Fast viewer for images, PDFs and CSV files.",
    )?;
    let app = r"Software\Classes\Applications\whir.exe";
    set_string(app, Some("FriendlyAppName"), "whir")?;
    set_string(&format!(r"{app}\shell\open\command"), None, &command)?;

    for g in groups() {
        let base = format!(r"Software\Classes\{}", g.prog_id);
        set_string(&base, None, g.description)?;
        set_string(&format!(r"{base}\DefaultIcon"), None, &icon)?;
        set_string(&format!(r"{base}\shell\open\command"), None, &command)?;
        for ext in g.extensions {
            set_string(
                &format!(r"Software\Classes\.{ext}\OpenWithProgids"),
                Some(g.prog_id),
                "",
            )?;
            set_string(
                &format!(r"{app}\SupportedTypes"),
                Some(&format!(".{ext}")),
                "",
            )?;
            set_string(
                &format!(r"{CAPABILITIES}\FileAssociations"),
                Some(&format!(".{ext}")),
                g.prog_id,
            )?;
            count += 1;
        }
    }
    set_string(
        r"Software\RegisteredApplications",
        Some("whir"),
        CAPABILITIES,
    )?;
    notify();
    Ok(count)
}

pub fn unregister() {
    unsafe {
        let del_tree = |path: &str| {
            let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(path));
        };
        let del_value = |path: &str, name: &str| {
            let _ = RegDeleteKeyValueW(
                HKEY_CURRENT_USER,
                &HSTRING::from(path),
                &HSTRING::from(name),
            );
        };
        for g in groups() {
            del_tree(&format!(r"Software\Classes\{}", g.prog_id));
            for ext in g.extensions {
                del_value(
                    &format!(r"Software\Classes\.{ext}\OpenWithProgids"),
                    g.prog_id,
                );
            }
        }
        del_tree(r"Software\Classes\Applications\whir.exe");
        del_tree(r"Software\whir");
        del_value(r"Software\RegisteredApplications", "whir");
    }
    notify();
}

fn notify() {
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
}
