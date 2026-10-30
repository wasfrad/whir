//! Drawing the dock without a renderer: a small bitmap with per-pixel alpha
//! (premultiplied `0xAARRGGBB`), filled by hand and handed to Windows as a
//! layered window. Shapes get their soft edges from signed distances; glyphs
//! are rendered by GDI in white on black and used as coverage masks.

use std::cell::RefCell;

use windows::Win32::Foundation::{COLORREF, SIZE};
use windows::Win32::Graphics::Gdi::{
    ANTIALIASED_QUALITY, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CLIP_DEFAULT_PRECIS,
    CreateCompatibleDC, CreateDIBSection, CreateFontW, DEFAULT_CHARSET, DIB_RGB_COLORS, DeleteDC,
    DeleteObject, FW_NORMAL, FW_SEMIBOLD, GGI_MARK_NONEXISTING_GLYPHS, GdiFlush, GetGlyphIndicesW,
    GetTextExtentExPointW, GetTextExtentPoint32W, HBITMAP, HDC, HFONT, HGDIOBJ, OUT_DEFAULT_PRECIS,
    SelectObject, SetBkMode, SetTextColor, TRANSPARENT, TextOutW,
};
use windows::core::HSTRING;

use crate::geom::{Point, Rect};

/// A top-down 32-bit bitmap GDI can also draw into.
pub struct Surface {
    pub width: i32,
    pub height: i32,
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    bits: *mut u32,
}

impl Surface {
    /// All transparent (or black, read as a mask).
    pub fn new(width: i32, height: i32) -> Option<Surface> {
        if width <= 0 || height <= 0 {
            return None;
        }
        unsafe {
            let dc = CreateCompatibleDC(None);
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let bitmap = match CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)
            {
                Ok(b) if !bits.is_null() => b,
                _ => {
                    let _ = DeleteDC(dc);
                    return None;
                }
            };
            let previous = SelectObject(dc, HGDIOBJ(bitmap.0));
            Some(Surface {
                width,
                height,
                dc,
                bitmap,
                previous,
                bits: bits.cast(),
            })
        }
    }

    pub fn dc(&self) -> HDC {
        self.dc
    }

    pub fn pixels(&mut self) -> &mut [u32] {
        unsafe {
            let _ = GdiFlush();
            std::slice::from_raw_parts_mut(self.bits, self.width as usize * self.height as usize)
        }
    }

    pub fn canvas(&mut self) -> Canvas<'_> {
        let (width, height) = (self.width, self.height);
        Canvas {
            px: self.pixels(),
            width,
            height,
        }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.previous);
            let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
            let _ = DeleteDC(self.dc);
        }
    }
}

/// Premultiplied pixels to draw on.
pub struct Canvas<'a> {
    pub px: &'a mut [u32],
    pub width: i32,
    pub height: i32,
}

/// `color` (red, green, blue) at `coverage` (0–1), premultiplied.
fn premultiplied(color: [u8; 3], coverage: f32) -> u32 {
    let a = (coverage.clamp(0.0, 1.0) * 255.0).round() as u32;
    let c = |v: u8| (v as u32 * a + 127) / 255;
    a << 24 | c(color[0]) << 16 | c(color[1]) << 8 | c(color[2])
}

/// `src` over `dst`, both premultiplied.
fn over(dst: u32, src: u32) -> u32 {
    let keep = 255 - (src >> 24);
    let ch = |shift: u32| {
        let (d, s) = ((dst >> shift) & 0xff, (src >> shift) & 0xff);
        (s + (d * keep + 127) / 255).min(255) << shift
    };
    ch(24) | ch(16) | ch(8) | ch(0)
}

impl Canvas<'_> {
    fn blend(&mut self, x: i32, y: i32, color: [u8; 3], coverage: f32) {
        if coverage > 0.0 && x >= 0 && y >= 0 && x < self.width && y < self.height {
            let p = &mut self.px[(y * self.width + x) as usize];
            *p = over(*p, premultiplied(color, coverage));
        }
    }

    /// A filled rectangle with rounded corners (`radii`: top-left, top-right,
    /// bottom-right, bottom-left), its edge anti-aliased.
    pub fn round_rect(&mut self, r: Rect, radii: [f32; 4], color: [u8; 3], opacity: f32) {
        let (cx, cy) = (
            (r.left + r.right) as f32 / 2.0,
            (r.top + r.bottom) as f32 / 2.0,
        );
        let (hw, hh) = (r.width() as f32 / 2.0, r.height() as f32 / 2.0);
        for y in r.top..r.bottom {
            for x in r.left..r.right {
                let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
                let radius = match (dx < 0.0, dy < 0.0) {
                    (true, true) => radii[0],
                    (false, true) => radii[1],
                    (false, false) => radii[2],
                    (true, false) => radii[3],
                }
                .min(hw)
                .min(hh);
                // Distance to the rounded outline (negative inside).
                let (qx, qy) = (dx.abs() - hw + radius, dy.abs() - hh + radius);
                let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
                let distance = outside + qx.max(qy).min(0.0) - radius;
                self.blend(x, y, color, (0.5 - distance).clamp(0.0, 1.0) * opacity);
            }
        }
    }

    pub fn dot(&mut self, center: (f32, f32), radius: f32, color: [u8; 3]) {
        let reach = radius.ceil() as i32 + 1;
        let (cx, cy) = (center.0.floor() as i32, center.1.floor() as i32);
        for y in cy - reach..=cy + reach {
            for x in cx - reach..=cx + reach {
                let d = ((x as f32 + 0.5 - center.0).powi(2) + (y as f32 + 0.5 - center.1).powi(2))
                    .sqrt();
                self.blend(x, y, color, radius + 0.5 - d);
            }
        }
    }

    /// `mask` in `color`, centred on `center`.
    pub fn mask(&mut self, center: Point, mask: &Mask, color: [u8; 3]) {
        self.mask_at(
            (center.0 - mask.width / 2, center.1 - mask.height / 2),
            mask,
            color,
        );
    }

    /// `mask` in `color`, its top-left corner at `at`.
    pub fn mask_at(&mut self, at: Point, mask: &Mask, color: [u8; 3]) {
        for y in 0..mask.height {
            for x in 0..mask.width {
                let c = mask.coverage[(y * mask.width + x) as usize];
                self.blend(at.0 + x, at.1 + y, color, c as f32 / 255.0);
            }
        }
    }
}

/// How much of each pixel a piece of text covers.
pub struct Mask {
    pub width: i32,
    pub height: i32,
    pub coverage: Vec<u8>,
}

/// Fonts made so far, by face, size and weight (semibold or not).
type Fonts = Vec<((String, i32, bool), HFONT)>;

thread_local! {
    /// A dock uses a handful of fonts, and redraws often.
    static FONTS: RefCell<Fonts> = const { RefCell::new(Vec::new()) };
}

fn font(face: &str, px: i32, bold: bool) -> HFONT {
    FONTS.with_borrow_mut(|fonts| {
        if let Some((_, f)) = fonts
            .iter()
            .find(|((n, p, b), _)| n == face && *p == px && *b == bold)
        {
            return *f;
        }
        let weight = if bold { FW_SEMIBOLD } else { FW_NORMAL };
        let f = unsafe {
            CreateFontW(
                -px,
                0,
                0,
                0,
                weight.0 as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                ANTIALIASED_QUALITY,
                0,
                &HSTRING::from(face),
            )
        };
        fonts.push(((face.to_owned(), px, bold), f));
        f
    })
}

/// `text` in `face`, `px` pixels tall. `None` if the font has no glyph for it
/// (Windows would quietly substitute another font's, or a box).
pub fn text_mask(text: &str, face: &str, px: i32) -> Option<Mask> {
    render(text, font(face, px, false), None, true)
}

/// A line of text in `face` (semibold if `bold`), cut short with "…" if it
/// is wider than `max_width`. Characters the font lacks come from Windows'
/// fallback fonts (names and notes can be in any script).
pub fn label_mask(text: &str, face: &str, px: i32, bold: bool, max_width: i32) -> Option<Mask> {
    render(text, font(face, px, bold), Some(max_width), false)
}

fn render(text: &str, font: HFONT, max_width: Option<i32>, strict: bool) -> Option<Mask> {
    let mut units: Vec<u16> = text.encode_utf16().collect();
    unsafe {
        // Measured on a scratch surface, then drawn on one that fits.
        let probe = Surface::new(1, 1)?;
        let previous = SelectObject(probe.dc(), HGDIOBJ(font.0));
        let measure = |units: &[u16]| {
            let mut size = SIZE::default();
            GetTextExtentPoint32W(probe.dc(), units, &mut size)
                .as_bool()
                .then_some(size)
        };
        if strict {
            let mut glyphs = vec![0u16; units.len()];
            let found = GetGlyphIndicesW(
                probe.dc(),
                &HSTRING::from(text),
                units.len() as i32,
                glyphs.as_mut_ptr(),
                GGI_MARK_NONEXISTING_GLYPHS,
            );
            if found == u32::MAX || glyphs.contains(&0xFFFF) {
                SelectObject(probe.dc(), previous);
                return None;
            }
        }
        let mut size = measure(&units);
        if let (Some(max), Some(s)) = (max_width, size)
            && s.cx > max
        {
            // As much as fits before an ellipsis.
            let ellipsis = [0x2026u16];
            let room = max - measure(&ellipsis).map_or(0, |e| e.cx);
            let mut fit = 0i32;
            let mut whole = SIZE::default();
            let _ = GetTextExtentExPointW(
                probe.dc(),
                windows::core::PCWSTR(units.as_ptr()),
                units.len() as i32,
                room.max(0),
                Some(&mut fit),
                None,
                &mut whole,
            );
            units.truncate(fit.max(0) as usize);
            // Not half a surrogate pair, nor a trailing space.
            if units.last().is_some_and(|&u| (0xD800..0xDC00).contains(&u)) {
                units.pop();
            }
            while units.last() == Some(&(' ' as u16)) {
                units.pop();
            }
            units.push(ellipsis[0]);
            size = measure(&units);
        }
        SelectObject(probe.dc(), previous);
        drop(probe);
        let size = size.filter(|s| s.cx > 0 && s.cy > 0)?;

        let mut surface = Surface::new(size.cx, size.cy)?;
        let dc = surface.dc();
        let previous = SelectObject(dc, HGDIOBJ(font.0));
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, COLORREF(0x00ff_ffff));
        let _ = TextOutW(dc, 0, 0, &units);
        SelectObject(dc, previous);
        // White on black: any channel is the coverage.
        let coverage = surface.pixels().iter().map(|p| (p >> 8) as u8).collect();
        Some(Mask {
            width: size.cx,
            height: size.cy,
            coverage,
        })
    }
}

/// An icon from Windows' own icon font (Windows 11's, else Windows 10's).
pub fn icon_mask(glyph: char, px: i32) -> Option<Mask> {
    let text = glyph.to_string();
    text_mask(&text, "Segoe Fluent Icons", px).or_else(|| text_mask(&text, "Segoe MDL2 Assets", px))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas(px: &mut [u32], width: i32) -> Canvas<'_> {
        let height = px.len() as i32 / width;
        Canvas { px, width, height }
    }

    fn valid(p: u32) -> bool {
        let a = p >> 24;
        (0..3).all(|c| (p >> (c * 8)) & 0xff <= a)
    }

    #[test]
    fn a_round_rect_is_solid_inside_and_clear_at_its_rounded_corners() {
        let mut px = vec![0u32; 40 * 40];
        let mut c = canvas(&mut px, 40);
        // Rounded on the right only (the dock on the left edge).
        c.round_rect(
            Rect::new(4, 4, 36, 36),
            [0.0, 10.0, 10.0, 0.0],
            [200, 100, 50],
            1.0,
        );
        let at = |x: usize, y: usize| px[y * 40 + x];
        assert_eq!(at(20, 20), 0xffc8_6432);
        // A square corner is filled to the corner; a rounded one isn't.
        assert_eq!(at(4, 4), 0xffc8_6432);
        assert_eq!(at(35, 4), 0);
        assert_eq!(at(35, 35), 0);
        assert_eq!(at(4, 35), 0xffc8_6432);
        // Outside stays untouched, and every pixel is properly premultiplied.
        assert_eq!(at(3, 20), 0);
        assert_eq!(at(36, 20), 0);
        assert!(px.iter().all(|&p| valid(p)));
        // The rounded edge is soft: some pixels are partly covered.
        assert!(px.iter().any(|&p| (1..255).contains(&(p >> 24))));
    }

    #[test]
    fn drawing_over_blends_and_stays_premultiplied() {
        let mut px = vec![0u32; 20 * 20];
        let mut c = canvas(&mut px, 20);
        c.round_rect(Rect::new(0, 0, 20, 20), [0.0; 4], [10, 20, 30], 1.0);
        c.round_rect(Rect::new(5, 5, 15, 15), [0.0; 4], [255, 255, 255], 0.5);
        c.dot((10.0, 10.0), 2.0, [255, 0, 0]);
        assert_eq!(px[0], 0xff0a_141e);
        // Half white over the dark fill.
        let mid = px[6 * 20 + 6];
        assert_eq!(mid >> 24, 0xff);
        assert!(((mid >> 16) & 0xff).abs_diff(133) <= 1, "{mid:08x}");
        assert_eq!(px[10 * 20 + 10], 0xffff_0000);
        assert!(px.iter().all(|&p| valid(p)));
        // Clipped at the canvas, not wrapped or panicking.
        canvas(&mut px, 20).dot((0.0, 0.0), 6.0, [0, 255, 0]);
        canvas(&mut px, 20).round_rect(Rect::new(-5, -5, 3, 3), [2.0; 4], [0, 0, 255], 1.0);
    }

    #[test]
    fn text_becomes_a_mask_and_a_missing_glyph_is_reported() {
        let m = text_mask("3", "Segoe UI", 20).expect("Segoe UI has digits");
        assert_eq!(m.coverage.len(), (m.width * m.height) as usize);
        assert!(m.coverage.iter().any(|&c| c > 200), "nothing drawn");
        assert!(m.coverage.contains(&0), "everything drawn");
        // A private-use code point no text font has.
        assert!(text_mask("\u{F8FF}", "Segoe UI", 20).is_none());
        // Windows ships one of its icon fonts.
        let camera = icon_mask('\u{E722}', 20).expect("an icon font");
        assert!(camera.coverage.iter().any(|&c| c > 200));
    }

    #[test]
    fn long_labels_are_cut_short_to_fit() {
        let full = label_mask("Acme Corporation · Onboarding", "Segoe UI", 14, false, 1000)
            .expect("a label");
        let cut = label_mask("Acme Corporation · Onboarding", "Segoe UI", 14, false, 80)
            .expect("a label");
        assert!(cut.width <= 80, "{}", cut.width);
        assert!(cut.width < full.width);
        // Semibold is wider than regular.
        let bold = label_mask("Acme", "Segoe UI", 14, true, 1000).unwrap();
        let regular = label_mask("Acme", "Segoe UI", 14, false, 1000).unwrap();
        assert!(bold.width >= regular.width);
        // Unlike a strict mask, a label draws what it can.
        assert!(label_mask("\u{F8FF}x", "Segoe UI", 14, false, 100).is_some());
    }
}
