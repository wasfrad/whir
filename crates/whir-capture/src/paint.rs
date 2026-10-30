//! Drawing the dock without a renderer: a small bitmap with per-pixel alpha
//! (premultiplied `0xAARRGGBB`), filled by hand and handed to Windows as a
//! layered window. Shapes get their soft edges from signed distances; text
//! goes over them through DirectWrite (`text`).

use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS,
    DeleteDC, DeleteObject, GdiFlush, HBITMAP, HDC, HGDIOBJ, SelectObject,
};

use crate::geom::Rect;

/// A top-down 32-bit bitmap, also a device context (to draw text into, and
/// to hand to `UpdateLayeredWindow`).
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

#[cfg(test)]
impl Surface {
    /// Write the picture to `path` as a PNG (to look at).
    pub fn save_png(&mut self, path: &std::path::Path) {
        use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
        // The PNG encoder is a COM object.
        let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        let (w, h) = (self.width, self.height);
        let bytes: Vec<u8> = self.pixels().iter().flat_map(|p| p.to_le_bytes()).collect();
        let png = whir_image::encode_png(
            &bytes,
            w as u32,
            h as u32,
            w as usize * 4,
            whir_image::PixelLayout::PremultipliedBgra,
        )
        .unwrap();
        std::fs::write(path, png).unwrap();
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
}
