//! WIC encoding: BGRA pixels to a PNG held in memory.

use windows::Win32::Foundation::{HGLOBAL, WINCODEC_ERR_UNSUPPORTEDPIXELFORMAT};
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::StructuredStorage::{CreateStreamOnHGlobal, IPropertyBag2};
use windows::Win32::System::Com::{IStream, STATFLAG_NONAME, STATSTG, STREAM_SEEK_SET};

use crate::decode::factory;

/// How the four bytes of each source pixel are read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelLayout {
    /// Blue, green, red and a byte to ignore (a GDI screen grab): an opaque
    /// 24-bit PNG.
    Bgrx,
    /// Premultiplied BGRA, as [`crate::decode`] produces: a 32-bit PNG with alpha.
    PremultipliedBgra,
}

#[derive(Debug)]
pub enum EncodeError {
    /// An empty image, or `pixels` is shorter than `stride` × `height`.
    BadInput,
    Wic(windows::core::Error),
}

impl std::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EncodeError::BadInput => f.write_str("There is no image to encode."),
            EncodeError::Wic(e) => write!(f, "Couldn't encode the image: {}", e.message()),
        }
    }
}

impl From<windows::core::Error> for EncodeError {
    fn from(e: windows::core::Error) -> Self {
        EncodeError::Wic(e)
    }
}

/// Rows converted and handed to the encoder at a time (so the source is never
/// copied whole).
const BAND_ROWS: usize = 64;

/// Encode `height` rows of `width` pixels, `stride` bytes apart, as a PNG.
/// The calling thread must have COM initialized.
pub fn encode_png(
    pixels: &[u8],
    width: u32,
    height: u32,
    stride: usize,
    layout: PixelLayout,
) -> Result<Vec<u8>, EncodeError> {
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 || stride < w * 4 || pixels.len() < stride * (h - 1) + w * 4 {
        return Err(EncodeError::BadInput);
    }
    let factory = factory()?;
    unsafe {
        let stream = CreateStreamOnHGlobal(HGLOBAL::default(), true)?;
        let encoder = factory.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
        encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
        let mut frame = None;
        encoder.CreateNewFrame(&mut frame, std::ptr::null_mut())?;
        let frame = frame.ok_or_else(windows::core::Error::empty)?;
        frame.Initialize(None::<&IPropertyBag2>)?;
        frame.SetSize(width, height)?;
        let (wanted, bytes) = match layout {
            PixelLayout::Bgrx => (GUID_WICPixelFormat24bppBGR, 3),
            PixelLayout::PremultipliedBgra => (GUID_WICPixelFormat32bppBGRA, 4),
        };
        let mut format = wanted;
        frame.SetPixelFormat(&mut format)?;
        if format != wanted {
            // The encoder would rather have a format we aren't feeding it.
            return Err(EncodeError::Wic(WINCODEC_ERR_UNSUPPORTEDPIXELFORMAT.into()));
        }

        let out_stride = w * bytes;
        let mut band = vec![0u8; out_stride * BAND_ROWS.min(h)];
        for first in (0..h).step_by(BAND_ROWS) {
            let rows = BAND_ROWS.min(h - first);
            for (i, out) in band.chunks_exact_mut(out_stride).take(rows).enumerate() {
                let at = (first + i) * stride;
                let src = &pixels[at..at + w * 4];
                match layout {
                    PixelLayout::Bgrx => opaque_row(src, out),
                    PixelLayout::PremultipliedBgra => straight_row(src, out),
                }
            }
            frame.WritePixels(rows as u32, out_stride as u32, &band[..rows * out_stride])?;
        }
        frame.Commit()?;
        encoder.Commit()?;
        Ok(read_all(&stream)?)
    }
}

/// BGRX → BGR.
fn opaque_row(src: &[u8], out: &mut [u8]) {
    for (s, d) in src.chunks_exact(4).zip(out.chunks_exact_mut(3)) {
        d.copy_from_slice(&s[..3]);
    }
}

/// Premultiplied BGRA → straight BGRA (what PNG stores).
fn straight_row(src: &[u8], out: &mut [u8]) {
    for (s, d) in src.chunks_exact(4).zip(out.chunks_exact_mut(4)) {
        let a = s[3] as u32;
        match a {
            255 => d.copy_from_slice(s),
            0 => d.fill(0),
            _ => {
                for c in 0..3 {
                    d[c] = ((s[c] as u32 * 255 + a / 2) / a).min(255) as u8;
                }
                d[3] = s[3];
            }
        }
    }
}

fn read_all(stream: &IStream) -> windows::core::Result<Vec<u8>> {
    unsafe {
        let mut stat = STATSTG::default();
        stream.Stat(&mut stat, STATFLAG_NONAME)?;
        let mut out = vec![0u8; stat.cbSize as usize];
        stream.Seek(0, STREAM_SEEK_SET, None)?;
        let mut done = 0;
        while done < out.len() {
            let want = (out.len() - done).min(1 << 30) as u32;
            let mut got = 0u32;
            stream
                .Read(out[done..].as_mut_ptr().cast(), want, Some(&mut got))
                .ok()?;
            if got == 0 {
                break;
            }
            done += got as usize;
        }
        out.truncate(done);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::decode;

    fn init_com() {
        unsafe {
            use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
    }

    /// Encode, write to a temp file and decode it again.
    fn round_trip(
        name: &str,
        pixels: &[u8],
        width: u32,
        height: u32,
        stride: usize,
        layout: PixelLayout,
    ) -> crate::Decoded {
        init_com();
        let png = encode_png(pixels, width, height, stride, layout).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        let dir = std::env::temp_dir().join(format!("whir-encode-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, &png).unwrap();
        let decoded = decode(&path, None).unwrap();
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
        decoded
    }

    #[test]
    fn opaque_round_trip_ignores_the_fourth_byte_and_padding() {
        // Odd width, more rows than one band, rows padded with junk.
        let (w, h) = (37u32, 150u32);
        let stride = w as usize * 4 + 8;
        let mut pixels = vec![0xEEu8; stride * h as usize];
        for y in 0..h as usize {
            for x in 0..w as usize {
                let p = &mut pixels[y * stride + x * 4..][..4];
                p.copy_from_slice(&[(x * 7) as u8, (y * 3) as u8, (x + y) as u8, (x ^ y) as u8]);
            }
        }
        let d = round_trip("opaque.png", &pixels, w, h, stride, PixelLayout::Bgrx);
        assert_eq!((d.width, d.height), (w, h));
        assert!(!d.has_alpha);
        for y in 0..h as usize {
            for x in 0..w as usize {
                let got = &d.pixels[y * d.stride + x * 4..][..4];
                let want = [(x * 7) as u8, (y * 3) as u8, (x + y) as u8, 255];
                assert_eq!(got, want, "at {x},{y}");
            }
        }
    }

    #[test]
    fn premultiplied_round_trip_keeps_alpha() {
        let (w, h) = (16u32, 16u32);
        let stride = w as usize * 4;
        let mut pixels = vec![0u8; stride * h as usize];
        for y in 0..h as usize {
            for x in 0..w as usize {
                let a = (x * 17) as u32;
                let straight = [200u32, (y * 16) as u32, 40];
                let p = &mut pixels[y * stride + x * 4..][..4];
                for c in 0..3 {
                    p[c] = ((straight[c] * a + 127) / 255) as u8;
                }
                p[3] = a as u8;
            }
        }
        let d = round_trip(
            "alpha.png",
            &pixels,
            w,
            h,
            stride,
            PixelLayout::PremultipliedBgra,
        );
        assert_eq!((d.width, d.height), (w, h));
        assert!(d.has_alpha);
        for (i, (got, want)) in d.pixels.iter().zip(&pixels).enumerate() {
            assert!(got.abs_diff(*want) <= 1, "byte {i}: {got} vs {want}");
        }
    }

    #[test]
    fn single_pixel() {
        let d = round_trip("one.png", &[10, 20, 30, 0], 1, 1, 4, PixelLayout::Bgrx);
        assert_eq!((d.width, d.height), (1, 1));
        assert_eq!(d.pixels, [10, 20, 30, 255]);
    }

    #[test]
    fn rejects_empty_and_short_input() {
        let bad = |pixels: &[u8], w, h, stride| {
            matches!(
                encode_png(pixels, w, h, stride, PixelLayout::Bgrx),
                Err(EncodeError::BadInput)
            )
        };
        assert!(bad(&[], 0, 0, 0));
        assert!(bad(&[0; 16], 2, 0, 8));
        // Two rows promised, one supplied.
        assert!(bad(&[0; 8], 2, 2, 8));
        // A stride narrower than a row.
        assert!(bad(&[0; 16], 2, 2, 4));
    }
}
