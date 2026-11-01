//! Loads UI fonts from `%WINDIR%\Fonts` instead of embedding them in the binary.
//!
//! Font files are memory-mapped read-only and the mapping is kept for the life
//! of the process: the pages are file-backed (not private memory) and only the
//! parts actually used are ever paged in.

use std::os::windows::io::AsRawHandle as _;
use std::path::{Path, PathBuf};

use egui::epaint::text::{FontTweak, VariationCoords};
use egui::{FontData, FontDefinitions, FontFamily};
use whir_core::theme::{FontRole, FontSpec, NAMED_FAMILIES};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_READ, MapViewOfFile, PAGE_READONLY,
};
use windows::core::PCWSTR;

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
    let dir = fonts_dir();
    let mut tweak = FontTweak {
        hinting: Some(spec.hinted),
        y_offset_factor: spec.drop,
        ..Default::default()
    };
    let bytes = match map_static(&dir.join(spec.file)) {
        Some(bytes) => {
            tweak.coords = VariationCoords::new(spec.axes.iter().copied());
            bytes
        }
        None => map_static(&dir.join(spec.alternate?))?,
    };
    let mut data = FontData::from_static(bytes).tweak(tweak);
    data.index = spec.index;
    Some(data)
}

/// Build egui font definitions from system font specs. Missing files are skipped.
///
/// Every named family in [`NAMED_FAMILIES`] is registered, falling back to the
/// regular faces, so rich text can use them before (or without) their own files.
pub fn definitions(specs: &[FontSpec]) -> FontDefinitions {
    let mut defs = FontDefinitions::empty();
    let mut fallbacks = Vec::new();
    for spec in specs {
        let Some(data) = load_font(spec) else {
            continue;
        };
        defs.font_data.insert(spec.name.to_owned(), data.into());
        match spec.role {
            FontRole::Proportional => {
                fam(&mut defs, FontFamily::Proportional).push(spec.name.to_owned())
            }
            FontRole::Monospace => fam(&mut defs, FontFamily::Monospace).push(spec.name.to_owned()),
            FontRole::Fallback => fallbacks.push(spec.name.to_owned()),
            FontRole::Family(name) => {
                fam(&mut defs, FontFamily::Name(name.into())).push(spec.name.to_owned())
            }
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
    // Named families: their own face first (if loaded), then the regular chain.
    let prop = fam(&mut defs, FontFamily::Proportional).clone();
    let mono = fam(&mut defs, FontFamily::Monospace).clone();
    for &(name, is_mono) in NAMED_FAMILIES {
        let chain = if is_mono { &mono } else { &prop };
        let family = fam(&mut defs, FontFamily::Name(name.into()));
        for f in chain {
            if !family.contains(f) {
                family.push(f.clone());
            }
        }
    }
    defs
}

/// Add fallback fonts to an already-configured context (lazy CJK/emoji support).
pub fn add_fallbacks(ctx: &egui::Context, specs: &[FontSpec]) {
    use egui::epaint::text::{FontInsert, FontPriority, InsertFontFamily};
    for spec in specs {
        let Some(data) = load_font(spec) else {
            continue;
        };
        let mut families = vec![
            InsertFontFamily {
                family: FontFamily::Proportional,
                priority: FontPriority::Lowest,
            },
            InsertFontFamily {
                family: FontFamily::Monospace,
                priority: FontPriority::Lowest,
            },
        ];
        families.extend(NAMED_FAMILIES.iter().map(|&(name, _)| InsertFontFamily {
            family: FontFamily::Name(name.into()),
            priority: FontPriority::Lowest,
        }));
        ctx.add_font(FontInsert::new(spec.name, data, families));
    }
}

/// Load the real faces of named families (bold, italic, ...) on demand.
pub fn add_family_fonts(ctx: &egui::Context, specs: &[FontSpec]) {
    use egui::epaint::text::{FontInsert, FontPriority, InsertFontFamily};
    for spec in specs {
        let FontRole::Family(name) = spec.role else {
            continue;
        };
        let Some(data) = load_font(spec) else {
            continue;
        };
        ctx.add_font(FontInsert::new(
            spec.name,
            data,
            vec![InsertFontFamily {
                family: FontFamily::Name(name.into()),
                priority: FontPriority::Highest,
            }],
        ));
    }
}

fn fam(defs: &mut FontDefinitions, family: FontFamily) -> &mut Vec<String> {
    defs.families.entry(family).or_default()
}

#[cfg(test)]
mod tests {
    use egui::epaint::text::{Fonts, TextOptions};
    use whir_core::icons::{self, Icon};

    use super::*;

    /// On Windows 10 the fonts of Windows 11 aren't there.
    #[test]
    fn a_missing_font_gives_way_to_its_alternate() {
        let spec = FontSpec {
            name: "test",
            file: "no such font.ttf",
            alternate: Some("segoeui.ttf"),
            index: 0,
            role: FontRole::Proportional,
            axes: &[(b"wght", 450.0)],
            hinted: false,
            drop: 0.0,
        };
        let Some(data) = load_font(&spec) else {
            return; // No Segoe UI on this machine at all.
        };
        assert!(
            data.tweak.coords.as_ref().is_empty(),
            "the alternate isn't variable"
        );
    }

    /// Glyphs that aren't there draw as nothing, with no error at all.
    #[test]
    fn every_icon_is_in_both_icon_fonts() {
        for file in ["SegoeIcons.ttf", "segmdl2.ttf"] {
            let Some(bytes) = map_static(&fonts_dir().join(file)) else {
                continue;
            };
            let mut defs = FontDefinitions::empty();
            defs.font_data
                .insert(file.to_owned(), FontData::from_static(bytes).into());
            let family = FontFamily::Name(whir_core::theme::ICONS.into());
            defs.families.insert(family, vec![file.to_owned()]);
            let mut fonts = Fonts::new(TextOptions::default(), defs);
            let mut view = fonts.with_pixels_per_point(1.0);
            let missing: Vec<&Icon> = Icon::ALL
                .iter()
                .filter(|icon| !view.has_glyph(&icons::font(16.0), icon.glyph()))
                .collect();
            assert!(missing.is_empty(), "{file} lacks {missing:?}");
        }
    }
}
