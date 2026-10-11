//! A frozen copy of the whole desktop (every monitor), taken with GDI.
//!
//! GDI needs no graphics device, which keeps the dock small. Its limits: HDR
//! content comes out as SDR, protected video is black, and the mouse pointer
//! is never in the picture.

use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleDC, CreateDIBSection,
    DIB_RGB_COLORS, DeleteDC, DeleteObject, GdiFlush, GetDC, HBITMAP, HDC, HGDIOBJ, ROP_CODE,
    ReleaseDC, SRCCOPY, SelectObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

use crate::geom::{Point, Rect};

/// The desktop's pixels (`0x00RRGGBB`, top row first) in a GDI bitmap, so
/// they can be both edited directly and blitted to a window.
pub struct Frame {
    /// Where the frame's top-left is on the desktop (negative with a monitor
    /// left of or above the primary one).
    pub origin: Point,
    pub width: i32,
    pub height: i32,
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    bits: *mut u32,
}

impl Frame {
    pub fn grab() -> Result<Frame, String> {
        unsafe {
            let origin = (
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
            );
            let (width, height) = (
                GetSystemMetrics(SM_CXVIRTUALSCREEN),
                GetSystemMetrics(SM_CYVIRTUALSCREEN),
            );
            if width <= 0 || height <= 0 {
                return Err("Windows reports no screen to capture.".to_owned());
            }
            let screen = GetDC(None);
            if screen.is_invalid() {
                return Err("The screen can't be read right now.".to_owned());
            }
            let dc = CreateCompatibleDC(Some(screen));
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    // Negative: the top row first.
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
                    ReleaseDC(None, screen);
                    return Err("There isn't enough memory to capture the screen.".to_owned());
                }
            };
            let previous = SelectObject(dc, HGDIOBJ(bitmap.0));
            let frame = Frame {
                origin,
                width,
                height,
                dc,
                bitmap,
                previous,
                bits: bits.cast(),
            };
            // CAPTUREBLT: layered (translucent) windows too.
            let copied = BitBlt(
                dc,
                0,
                0,
                width,
                height,
                Some(screen),
                origin.0,
                origin.1,
                ROP_CODE(SRCCOPY.0 | CAPTUREBLT.0),
            );
            let _ = GdiFlush();
            ReleaseDC(None, screen);
            match copied {
                Ok(()) => Ok(frame),
                // The secure desktop (a UAC prompt, the lock screen) can't be read.
                Err(e) => Err(format!("The screen couldn't be captured: {}", e.message())),
            }
        }
    }

    pub fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.width, self.height)
    }

    pub fn pixels(&mut self) -> &mut [u32] {
        // GDI may still be drawing into the bitmap (text for the size label).
        unsafe {
            let _ = GdiFlush();
            std::slice::from_raw_parts_mut(self.bits, self.width as usize * self.height as usize)
        }
    }

    /// For blitting to a window, and for GDI to draw into.
    pub fn dc(&self) -> HDC {
        self.dc
    }

    /// The pixels of `r` (clipped to the frame) as tightly packed rows of
    /// blue, green, red and a spare byte.
    pub fn crop(&mut self, r: Rect) -> (Vec<u8>, u32, u32) {
        let r = r.intersect(self.bounds());
        let (w, h) = (r.width() as usize, r.height() as usize);
        let stride = self.width as usize;
        let px = self.pixels();
        let mut out = Vec::with_capacity(w * h * 4);
        for y in r.top as usize..r.top as usize + h {
            let row = &px[y * stride + r.left as usize..][..w];
            for p in row {
                out.extend_from_slice(&p.to_le_bytes());
            }
        }
        (out, w as u32, h as u32)
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.previous);
            let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
            let _ = DeleteDC(self.dc);
        }
    }
}
