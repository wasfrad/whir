//! What the dock is drawn into: a small bitmap with per-pixel alpha
//! (premultiplied `0xAARRGGBB`), handed to Windows as a layered window.

use crate::geom::Rect;

use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS,
    DeleteDC, DeleteObject, GdiFlush, HBITMAP, HDC, HGDIOBJ, SelectObject,
};

/// A top-down 32-bit bitmap, also a device context (to hand to
/// `UpdateLayeredWindow`).
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
}

/// What casts a shadow, and how it falls.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shadow {
    /// Its shape in the picture, and how round that is.
    pub shape: Rect,
    pub radius: f32,
    /// How far it falls below the shape.
    pub drop: i32,
    /// How soft its edge is.
    pub blur: f32,
    /// How dark it is at its darkest, 0-1.
    pub strength: f32,
}

/// A soft dark shape on nothing, `size` pixels: what's behind something that
/// floats. Black, so premultiplied it's alpha alone.
pub fn shadow(size: (i32, i32), s: Shadow) -> Option<Surface> {
    let mut surface = Surface::new(size.0, size.1)?;
    let r = s.shape;
    let (cx, cy) = (
        (r.left + r.right) as f32 / 2.0,
        (r.top + r.bottom) as f32 / 2.0 + s.drop as f32,
    );
    let (hw, hh) = (r.width() as f32 / 2.0, r.height() as f32 / 2.0);
    let radius = s.radius.min(hw).min(hh);
    let width = size.0 as usize;
    for (i, p) in surface.pixels().iter_mut().enumerate() {
        let (x, y) = ((i % width) as f32 + 0.5, (i / width) as f32 + 0.5);
        // Distance to the rounded outline (negative inside).
        let (qx, qy) = ((x - cx).abs() - hw + radius, (y - cy).abs() - hh + radius);
        let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
        let distance = outside + qx.max(qy).min(0.0) - radius;
        // Fading out across the blur, centred on the outline.
        let t = (0.5 - distance / s.blur).clamp(0.0, 1.0);
        let soft = t * t * (3.0 - 2.0 * t);
        *p = ((soft * s.strength * 255.0).round() as u32) << 24;
    }
    Some(surface)
}

#[cfg(test)]
impl Surface {
    pub fn pixel(&self, x: i32, y: i32) -> u32 {
        assert!(x >= 0 && y >= 0 && x < self.width && y < self.height);
        unsafe { *self.bits.add((y * self.width + x) as usize) }
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shadow_is_darkest_under_its_shape_and_fades_to_nothing() {
        let shape = Rect::new(30, 30, 90, 70);
        let s = Shadow {
            shape,
            radius: 8.0,
            drop: 4,
            blur: 16.0,
            strength: 0.5,
        };
        let surface = shadow((120, 104), s).unwrap();
        let alpha = |x: i32, y: i32| surface.pixel(x, y) >> 24;
        assert_eq!(alpha(60, 50), 128, "under the shape");
        assert_eq!(alpha(1, 1), 0, "far away");
        assert_eq!(surface.pixel(60, 50) & 0xff_ffff, 0, "it's black");
        // It falls below the shape more than above it, and softly.
        assert!(alpha(60, 74) > alpha(60, 26));
        let edge = alpha(60, 74);
        assert!(edge > 0 && edge < 128, "{edge}");
    }
}
