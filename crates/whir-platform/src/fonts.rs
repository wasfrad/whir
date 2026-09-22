//! Loads UI fonts from `%WINDIR%\Fonts` instead of embedding them in the binary.
//!
//! Font files are memory-mapped read-only and the mapping is kept for the life
//! of the process: the pages are file-backed (not private memory) and only the
//! parts actually used are ever paged in.

use std::os::windows::io::AsRawHandle as _;
use std::path::{Path, PathBuf};

use egui::{FontData, FontDefinitions, FontFamily};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_READ, MapViewOfFile, PAGE_READONLY,
};
use windows::core::PCWSTR;
use whir_core::theme::{FontRole, FontSpec};

pub fn fonts_dir() -> PathBuf {
    let windir = std::env::var_os("WINDIR")
        .or_else(|| std::env::var_os("SystemRoot"))
        .unwrap_or_else(|| "C:\\Windows".into());
    PathBuf::from(windir).join("Fonts")
}

/// Map a (system font) file for the rest of the process lifetime.
fn map_static(path: &Path) -> Option<&'static [u8]> {
    let file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len() as usize;
    if len == 0 {
        return None;
    }
    unsafe {
        let mapping = CreateFileMappingW(
            HANDLE(file.as_raw_handle()),
            None,
            PAGE_READONLY,
            0,
            0,
            PCWSTR::null(),
        )
        .ok()?;
        let view = MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, 0);
        // The view keeps the mapping and file alive after the handles close.
        let _ = CloseHandle(mapping);
        if view.Value.is_null() {
            return None;
        }
        Some(std::slice::from_raw_parts(view.Value as *const u8, len))
    }
}

pub fn load_font(spec: &FontSpec) -> Option<FontData> {
    let bytes = map_static(&fonts_dir().join(spec.file))?;
    let mut data = FontData::from_static(bytes);
    data.index = spec.index;
    Some(data)
}

/// Build egui font definitions from system font specs. Missing files are skipped.
pub fn definitions(specs: &[FontSpec]) -> FontDefinitions {
    let mut defs = FontDefinitions::empty();
    let mut fallbacks = Vec::new();
    for spec in specs {
        let Some(data) = load_font(spec) else { continue };
        defs.font_data.insert(spec.name.to_owned(), data.into());
        match spec.role {
            FontRole::Proportional => fam(&mut defs, FontFamily::Proportional).push(spec.name.to_owned()),
            FontRole::Monospace => fam(&mut defs, FontFamily::Monospace).push(spec.name.to_owned()),
            FontRole::Fallback => fallbacks.push(spec.name.to_owned()),
        }
    }
    // Each family falls back to the other family's primary, then the symbol fonts.
    let prop = fam(&mut defs, FontFamily::Proportional).clone();
    let mono = fam(&mut defs, FontFamily::Monospace).clone();
    fam(&mut defs, FontFamily::Proportional).extend(mono.iter().cloned());
    fam(&mut defs, FontFamily::Monospace).extend(prop.iter().cloned());
    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        fam(&mut defs, family).extend(fallbacks.iter().cloned());
    }
    defs
}

/// Add fallback fonts to an already-configured context (lazy CJK/emoji support).
pub fn add_fallbacks(ctx: &egui::Context, specs: &[FontSpec]) {
    use egui::epaint::text::{FontInsert, FontPriority, InsertFontFamily};
    for spec in specs {
        let Some(data) = load_font(spec) else { continue };
        ctx.add_font(FontInsert::new(
            spec.name,
            data,
            vec![
                InsertFontFamily {
                    family: FontFamily::Proportional,
                    priority: FontPriority::Lowest,
                },
                InsertFontFamily {
                    family: FontFamily::Monospace,
                    priority: FontPriority::Lowest,
                },
            ],
        ));
    }
}

fn fam(defs: &mut FontDefinitions, family: FontFamily) -> &mut Vec<String> {
    defs.families.entry(family).or_default()
}
