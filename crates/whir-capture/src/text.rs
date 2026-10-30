//! The dock's text, through DirectWrite: the way modern Windows apps draw
//! it, smoothly anti-aliased with glyphs placed to fractions of a pixel, and
//! drawn in its own colour over what is already painted (so dark text on a
//! light panel keeps its weight). ClearType needs an opaque window and the
//! dock's has per-pixel alpha, so this is grayscale. Direct2D draws it, in
//! software: nothing of the GPU's is loaded into the always-running dock.

use std::cell::RefCell;
use std::mem::ManuallyDrop;

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_ALIASED, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_FEATURE_LEVEL_DEFAULT, D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_SOFTWARE,
    D2D1_RENDER_TARGET_USAGE_NONE, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, D2D1CreateFactory,
    ID2D1DCRenderTarget, ID2D1Factory, ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_FEATURE, DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_PIXEL_GEOMETRY_FLAT,
    DWRITE_RENDERING_MODE_NATURAL_SYMMETRIC, DWRITE_TEXT_ALIGNMENT, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_TEXT_METRICS,
    DWRITE_TEXT_RANGE, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER,
    DWRITE_WORD_WRAPPING_NO_WRAP, DWriteCreateFactory, IDWriteFactory, IDWriteFactory1,
    IDWriteFontCollection, IDWriteRenderingParams1, IDWriteTextFormat, IDWriteTextLayout,
    IDWriteTypography,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::core::{BOOL, HSTRING, Interface, w};
use windows_numerics::Vector2;

use crate::geom::Rect;
use crate::paint::Surface;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    /// Windows' own UI font.
    Ui,
    /// Windows' icon font.
    Icons,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Font {
    pub family: Family,
    /// Pixels (the em size).
    pub px: f32,
    pub semibold: bool,
    /// Digits all as wide as each other, so a ticking clock stays still.
    pub tabular: bool,
}

impl Font {
    pub fn ui(px: f32) -> Font {
        Font {
            family: Family::Ui,
            px,
            semibold: false,
            tabular: false,
        }
    }

    pub fn icons(px: f32) -> Font {
        Font {
            family: Family::Icons,
            ..Font::ui(px)
        }
    }

    pub fn semibold(self) -> Font {
        Font {
            semibold: true,
            ..self
        }
    }

    pub fn tabular(self) -> Font {
        Font {
            tabular: true,
            ..self
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// A line of text in a box: centred top to bottom, placed across by
/// `align`, cut short with "…" if it doesn't fit.
#[derive(Clone, Debug)]
pub struct Text {
    pub text: String,
    pub font: Font,
    pub color: [u8; 3],
    pub rect: Rect,
    pub align: Align,
}

impl Text {
    pub fn new(text: impl Into<String>, font: Font, color: [u8; 3], rect: Rect) -> Text {
        Text {
            text: text.into(),
            font,
            color,
            rect,
            align: Align::Left,
        }
    }

    pub fn align(self, align: Align) -> Text {
        Text { align, ..self }
    }
}

struct Writer {
    dw: IDWriteFactory,
    target: ID2D1DCRenderTarget,
    brush: ID2D1SolidColorBrush,
    tabular: IDWriteTypography,
    /// The UI font's family name, and the icon font's if there is one.
    ui: HSTRING,
    icons: Option<HSTRING>,
    /// Made so far: family, size (bits of the f32), semibold.
    formats: Vec<((Family, u32, bool), IDWriteTextFormat)>,
}

thread_local! {
    /// `None` until first used; `Some(None)` if DirectWrite isn't there.
    /// Never released: it lasts as long as the thread anyway, and releasing
    /// Direct2D's objects as a thread ends can hang it.
    static WRITER: RefCell<Option<Option<ManuallyDrop<Writer>>>> = const { RefCell::new(None) };
}

fn with_writer<R>(f: impl FnOnce(&mut Writer) -> Option<R>) -> Option<R> {
    WRITER.with_borrow_mut(|w| {
        let writer = w
            .get_or_insert_with(|| Writer::new().ok().map(ManuallyDrop::new))
            .as_mut()?;
        f(writer)
    })
}

/// The first of `names` the system has.
fn family(fonts: &IDWriteFontCollection, names: &[&str]) -> Option<HSTRING> {
    names.iter().map(|n| HSTRING::from(*n)).find(|name| {
        let (mut index, mut exists) = (0u32, BOOL(0));
        unsafe { fonts.FindFamilyName(name, &mut index, &mut exists) }.is_ok() && exists.as_bool()
    })
}

impl Writer {
    fn new() -> windows::core::Result<Writer> {
        unsafe {
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let target = d2d.CreateDCRenderTarget(&D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                // Sizes are in pixels.
                dpiX: 96.0,
                dpiY: 96.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            })?;
            target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            // The system's gamma and contrast, rendered symmetrically smooth
            // (what DirectWrite picks for UI text at its best).
            let defaults = dw.CreateRenderingParams()?;
            let contrast = defaults.GetEnhancedContrast();
            let grayscale = defaults
                .cast::<IDWriteRenderingParams1>()
                .map_or(contrast, |p| p.GetGrayscaleEnhancedContrast());
            if let Ok(params) = dw.cast::<IDWriteFactory1>().and_then(|f| {
                f.CreateCustomRenderingParams(
                    defaults.GetGamma(),
                    contrast,
                    grayscale,
                    0.0,
                    DWRITE_PIXEL_GEOMETRY_FLAT,
                    DWRITE_RENDERING_MODE_NATURAL_SYMMETRIC,
                )
            }) {
                target.SetTextRenderingParams(&params);
            }
            let brush = target.CreateSolidColorBrush(
                &D2D1_COLOR_F {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                },
                None,
            )?;
            let tabular = dw.CreateTypography()?;
            tabular.AddFontFeature(DWRITE_FONT_FEATURE {
                nameTag: DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES,
                parameter: 1,
            })?;
            let mut fonts = None;
            dw.GetSystemFontCollection(&mut fonts, false)?;
            let fonts = fonts.ok_or_else(windows::core::Error::empty)?;
            let ui = family(&fonts, &["Segoe UI Variable Text", "Segoe UI"])
                .unwrap_or_else(|| HSTRING::from("Segoe UI"));
            let icons = family(&fonts, &["Segoe Fluent Icons", "Segoe MDL2 Assets"]);
            Ok(Writer {
                dw,
                target,
                brush,
                tabular,
                ui,
                icons,
                formats: Vec::new(),
            })
        }
    }

    fn format(&mut self, font: Font) -> Option<IDWriteTextFormat> {
        let key = (font.family, font.px.to_bits(), font.semibold);
        if let Some((_, f)) = self.formats.iter().find(|(k, _)| *k == key) {
            return Some(f.clone());
        }
        let name = match font.family {
            Family::Ui => &self.ui,
            Family::Icons => self.icons.as_ref()?,
        };
        let weight = if font.semibold {
            DWRITE_FONT_WEIGHT_SEMI_BOLD
        } else {
            DWRITE_FONT_WEIGHT_NORMAL
        };
        let format = unsafe {
            let f = self
                .dw
                .CreateTextFormat(
                    name,
                    None,
                    weight,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    font.px,
                    w!(""),
                )
                .ok()?;
            let _ = f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
            let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
            if let Ok(sign) = self.dw.CreateEllipsisTrimmingSign(&f) {
                let trimming = DWRITE_TRIMMING {
                    granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                    delimiter: 0,
                    delimiterCount: 0,
                };
                let _ = f.SetTrimming(&trimming, &sign);
            }
            f
        };
        self.formats.push((key, format.clone()));
        Some(format)
    }

    /// `text` laid out in a box `width` × `height`.
    fn layout(
        &mut self,
        text: &str,
        font: Font,
        width: f32,
        height: f32,
        align: DWRITE_TEXT_ALIGNMENT,
    ) -> Option<IDWriteTextLayout> {
        let format = self.format(font)?;
        let units: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            let layout = self
                .dw
                .CreateTextLayout(&units, &format, width.max(0.0), height.max(0.0))
                .ok()?;
            let _ = layout.SetTextAlignment(align);
            if font.tabular {
                let range = DWRITE_TEXT_RANGE {
                    startPosition: 0,
                    length: units.len() as u32,
                };
                let _ = layout.SetTypography(&self.tabular, range);
            }
            Some(layout)
        }
    }
}

/// There is an icon font (Windows 10 and 11 have one).
pub fn has_icons() -> bool {
    with_writer(|w| w.icons.is_some().then_some(())).is_some()
}

/// How wide and tall `text` is, in whole pixels.
pub fn measure(text: &str, font: Font) -> Option<(i32, i32)> {
    with_writer(|w| {
        let layout = w.layout(text, font, 1e5, 1e5, DWRITE_TEXT_ALIGNMENT_LEADING)?;
        let mut m = DWRITE_TEXT_METRICS::default();
        unsafe { layout.GetMetrics(&mut m) }.ok()?;
        Some((
            m.widthIncludingTrailingWhitespace.ceil() as i32,
            m.height.ceil() as i32,
        ))
    })
}

impl Surface {
    /// Draw `texts` over what is painted already, inside `clip` (if given).
    /// False if there is no DirectWrite to draw with.
    pub fn draw_text(&mut self, texts: &[Text], clip: Option<Rect>) -> bool {
        if texts.is_empty() {
            return true;
        }
        // Whatever was painted reaches the bitmap first.
        let _ = self.pixels();
        let (dc, width, height) = (self.dc(), self.width, self.height);
        with_writer(|w| unsafe {
            let whole = RECT {
                left: 0,
                top: 0,
                right: width,
                bottom: height,
            };
            w.target.BindDC(dc, &whole).ok()?;
            w.target.BeginDraw();
            if let Some(c) = clip {
                let r = D2D_RECT_F {
                    left: c.left as f32,
                    top: c.top as f32,
                    right: c.right as f32,
                    bottom: c.bottom as f32,
                };
                w.target
                    .PushAxisAlignedClip(&r, D2D1_ANTIALIAS_MODE_ALIASED);
            }
            for t in texts {
                draw_one(w, t);
            }
            if clip.is_some() {
                w.target.PopAxisAlignedClip();
            }
            w.target.EndDraw(None, None).ok()
        })
        .is_some()
    }
}

fn draw_one(w: &mut Writer, t: &Text) {
    if t.text.is_empty() || t.rect.width() <= 0 || t.rect.height() <= 0 {
        return;
    }
    let align = match t.align {
        Align::Left => DWRITE_TEXT_ALIGNMENT_LEADING,
        Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
        Align::Right => DWRITE_TEXT_ALIGNMENT_TRAILING,
    };
    let (width, height) = (t.rect.width() as f32, t.rect.height() as f32);
    let Some(layout) = w.layout(&t.text, t.font, width, height, align) else {
        return;
    };
    let mut origin = Vector2 {
        X: t.rect.left as f32,
        Y: t.rect.top as f32,
    };
    // An icon goes in the middle by its ink, not by the font's line.
    if t.font.family == Family::Icons
        && let Ok(ink) = unsafe { layout.GetOverhangMetrics() }
    {
        origin.X += ((ink.left - ink.right) / 2.0).round();
        origin.Y += ((ink.top - ink.bottom) / 2.0).round();
    }
    let [r, g, b] = t.color.map(|c| c as f32 / 255.0);
    unsafe {
        w.brush.SetColor(&D2D1_COLOR_F { r, g, b, a: 1.0 });
        w.target
            .DrawTextLayout(origin, &layout, &w.brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_measures_as_expected() {
        let (short, h) = measure("Acme", Font::ui(14.0)).expect("DirectWrite");
        let (long, _) = measure("Acme Corporation", Font::ui(14.0)).unwrap();
        assert!(short > 0 && h > 0);
        assert!(long > short);
        let (bold, _) = measure("Acme Corporation", Font::ui(14.0).semibold()).unwrap();
        assert!(bold >= long);
        let (big, tall) = measure("Acme", Font::ui(28.0)).unwrap();
        assert!(big > short && tall > h);
        // Tabular digits: every time of day is as wide as any other.
        let clock = Font::ui(15.0).semibold().tabular();
        assert_eq!(measure("1:11:11", clock), measure("0:08:00", clock));
        assert!(has_icons(), "Windows ships an icon font");
    }

    /// A surface painted all over in `color`.
    fn opaque(width: i32, height: i32, color: [u8; 3]) -> Surface {
        let mut s = Surface::new(width, height).unwrap();
        let [r, g, b] = color.map(u32::from);
        s.pixels().fill(0xff00_0000 | r << 16 | g << 8 | b);
        s
    }

    #[test]
    fn text_is_drawn_over_the_paint_inside_its_box() {
        let (w, h) = (200, 40);
        let mut s = opaque(w, h, [250, 250, 250]);
        let rect = Rect::new(10, 5, 120, 35);
        let text = Text::new("Aardvark Oil", Font::ui(14.0).semibold(), [20, 20, 20], rect);
        assert!(s.draw_text(&[text], None));
        let px = s.pixels().to_vec();
        // The background survived: everything still opaque.
        assert!(px.iter().all(|&p| p >> 24 == 0xff), "background lost");
        // Dark ink inside the box, none outside it.
        let at = |x: i32, y: i32| px[(y * w + x) as usize];
        let dark = |p: u32| (p >> 16) & 0xff < 128;
        assert!(
            (5..35).any(|y| (10..120).any(|x| dark(at(x, y)))),
            "no text"
        );
        assert!((0..h).all(|y| (130..w).all(|x| !dark(at(x, y)))));
        // Some pixels are in between: anti-aliased, not stair-stepped.
        assert!(px.iter().any(|&p| (40..210).contains(&((p >> 16) & 0xff))));
    }

    #[test]
    fn long_text_is_cut_short_and_icons_are_centred() {
        let (w, h) = (120, 40);
        let mut s = opaque(w, h, [0, 0, 0]);
        let font = Font::ui(14.0);
        let full = "Acme Corporation · Onboarding and much more";
        assert!(measure(full, font).unwrap().0 > 60);
        let cut = Text::new(full, font, [255, 255, 255], Rect::new(0, 0, 60, 40));
        assert!(s.draw_text(&[cut], None));
        let px = s.pixels().to_vec();
        let lit = |x: i32| (0..h).any(|y| (px[(y * w + x) as usize] >> 16) & 0xff > 60);
        assert!((0..60).any(lit));
        assert!(!(62..w).any(lit), "spilled out of its box");

        // The camera, centred in a square.
        let mut s = opaque(40, 40, [0, 0, 0]);
        let icon = Text::new(
            '\u{E722}',
            Font::icons(18.0),
            [255, 255, 255],
            Rect::new(0, 0, 40, 40),
        )
        .align(Align::Center);
        assert!(s.draw_text(&[icon], None));
        let px = s.pixels().to_vec();
        let (mut sx, mut sy, mut n) = (0i64, 0i64, 0i64);
        for y in 0..40 {
            for x in 0..40 {
                let v = ((px[(y * 40 + x) as usize] >> 16) & 0xff) as i64;
                sx += v * x as i64;
                sy += v * y as i64;
                n += v;
            }
        }
        assert!(n > 0, "no icon");
        let (cx, cy) = (sx as f64 / n as f64, sy as f64 / n as f64);
        assert!(
            (cx - 20.0).abs() < 3.0 && (cy - 20.0).abs() < 3.0,
            "{cx} {cy}"
        );
    }
}
