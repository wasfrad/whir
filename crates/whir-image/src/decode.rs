//! WIC decoding: shared-handle open, EXIF orientation, fast DCT-scaled JPEG
//! decode, high-quality downscale, conversion to premultiplied BGRA.

use std::cell::RefCell;
use std::os::windows::io::AsRawHandle as _;
use std::path::Path;

use windows::Win32::Foundation::WINCODEC_ERR_COMPONENTNOTFOUND;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PropVariantClear};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::Variant::VT_UI2;
use windows::core::{GUID, Interface as _, w};

/// A decoded image in premultiplied BGRA, already rotated per EXIF.
pub struct Decoded {
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub pixels: Vec<u8>,
    /// Full-resolution size after orientation (may be larger than `width`×`height`).
    pub full_size: [u32; 2],
    /// Any pixel is not fully opaque.
    pub has_alpha: bool,
}

impl Decoded {
    pub fn is_full_resolution(&self) -> bool {
        [self.width, self.height] == self.full_size
    }
}

#[derive(Debug)]
pub enum DecodeError {
    /// No WIC codec for this format (e.g. WebP/HEIF extension not installed).
    NoCodec,
    Io(std::io::Error),
    Wic(windows::core::Error),
    TooLarge,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::NoCodec => f.write_str(
                "Windows has no decoder for this image format. For WebP/HEIC, install the \
                 image extension from the Microsoft Store.",
            ),
            DecodeError::Io(e) => write!(f, "Couldn't read the file: {e}"),
            DecodeError::Wic(e) => write!(f, "Couldn't decode the image: {}", e.message()),
            DecodeError::TooLarge => f.write_str("The image is too large to display."),
        }
    }
}

impl From<windows::core::Error> for DecodeError {
    fn from(e: windows::core::Error) -> Self {
        if e.code() == WINCODEC_ERR_COMPONENTNOTFOUND {
            DecodeError::NoCodec
        } else {
            DecodeError::Wic(e)
        }
    }
}

/// Refuse absurd allocations (≈ 1.5 GB of BGRA).
const MAX_PIXELS: u64 = 400_000_000;

thread_local! {
    static FACTORY: RefCell<Option<IWICImagingFactory>> = const { RefCell::new(None) };
}

fn factory() -> Result<IWICImagingFactory, DecodeError> {
    FACTORY.with(|f| {
        if let Some(fac) = &*f.borrow() {
            return Ok(fac.clone());
        }
        let fac: IWICImagingFactory =
            unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }?;
        *f.borrow_mut() = Some(fac.clone());
        Ok(fac)
    })
}

/// EXIF orientation (1–8) → WIC transform, and whether it swaps width/height.
fn orientation_transform(o: u16) -> (WICBitmapTransformOptions, bool) {
    match o {
        2 => (WICBitmapTransformFlipHorizontal, false),
        3 => (WICBitmapTransformRotate180, false),
        4 => (WICBitmapTransformFlipVertical, false),
        5 => (
            WICBitmapTransformOptions(
                WICBitmapTransformRotate90.0 | WICBitmapTransformFlipHorizontal.0,
            ),
            true,
        ),
        6 => (WICBitmapTransformRotate90, true),
        7 => (
            WICBitmapTransformOptions(
                WICBitmapTransformRotate270.0 | WICBitmapTransformFlipHorizontal.0,
            ),
            true,
        ),
        8 => (WICBitmapTransformRotate270, true),
        _ => (WICBitmapTransformRotate0, false),
    }
}

fn read_orientation(frame: &IWICBitmapFrameDecode) -> u16 {
    unsafe {
        let Ok(reader) = frame.GetMetadataQueryReader() else {
            return 1;
        };
        let mut value = PROPVARIANT::default();
        if reader
            .GetMetadataByName(w!("System.Photo.Orientation"), &mut value)
            .is_err()
        {
            return 1;
        }
        let inner = &value.Anonymous.Anonymous;
        let o = if inner.vt == VT_UI2 {
            inner.Anonymous.uiVal
        } else {
            1
        };
        let _ = PropVariantClear(&mut value);
        o
    }
}

/// Decode `path`. With `max_long_side`, the result is downscaled so its longer
/// side is at most that (JPEGs decode directly at a reduced DCT scale, which is
/// much faster than decoding in full).
pub fn decode(path: &Path, max_long_side: Option<u32>) -> Result<Decoded, DecodeError> {
    let factory = factory()?;
    let file = whir_core::file::open_shared(path).map_err(DecodeError::Io)?;
    unsafe {
        let decoder = factory.CreateDecoderFromFileHandle(
            file.as_raw_handle() as usize,
            std::ptr::null(),
            WICDecodeMetadataCacheOnDemand,
        )?;
        let frame = decoder.GetFrame(0)?;
        let (mut w, mut h) = (0u32, 0u32);
        frame.GetSize(&mut w, &mut h)?;
        if w == 0 || h == 0 {
            return Err(DecodeError::Wic(windows::core::Error::empty()));
        }
        let orientation = read_orientation(&frame);
        let (transform, swaps) = orientation_transform(orientation);
        let full_size = if swaps { [h, w] } else { [w, h] };

        let mut source: IWICBitmapSource = frame.cast()?;
        let (mut cur_w, mut cur_h) = (w, h);

        if let Some(max) = max_long_side
            && w.max(h) > max
        {
            let scale = max as f64 / w.max(h) as f64;
            let (tw, th) = (
                ((w as f64 * scale).round() as u32).max(1),
                ((h as f64 * scale).round() as u32).max(1),
            );
            // 1) Native (DCT) scaling for JPEG and friends.
            if let Ok(t) = frame.cast::<IWICBitmapSourceTransform>() {
                let (mut cw, mut ch) = (tw, th);
                if t.GetClosestSize(&mut cw, &mut ch).is_ok()
                    && cw < w
                    && ch < h
                    && cw >= tw
                    && ch >= th
                {
                    let mut fmt: GUID = GUID_WICPixelFormat32bppPBGRA;
                    let _ = t.GetClosestPixelFormat(&mut fmt);
                    let bpp = bits_per_pixel(&factory, &fmt).unwrap_or(32);
                    let stride = (cw as usize * bpp as usize).div_ceil(8).next_multiple_of(4);
                    let mut buf = vec![0u8; stride * ch as usize];
                    if t.CopyPixels(
                        std::ptr::null(),
                        cw,
                        ch,
                        &fmt,
                        WICBitmapTransformRotate0,
                        stride as u32,
                        &mut buf,
                    )
                    .is_ok()
                    {
                        // Copies `buf`, which is freed at the end of this block.
                        let bmp =
                            factory.CreateBitmapFromMemory(cw, ch, &fmt, stride as u32, &buf)?;
                        source = bmp.cast()?;
                        (cur_w, cur_h) = (cw, ch);
                    }
                }
            }
            // 2) Otherwise a high-quality resample (reads the full image once).
            if cur_w.max(cur_h) > max.saturating_mul(2) || (cur_w, cur_h) == (w, h) {
                let scaler = factory.CreateBitmapScaler()?;
                scaler
                    .Initialize(&source, tw, th, WICBitmapInterpolationModeHighQualityCubic)
                    .or_else(|_| {
                        scaler.Initialize(&source, tw, th, WICBitmapInterpolationModeFant)
                    })?;
                source = scaler.cast()?;
                (cur_w, cur_h) = (tw, th);
            }
        }

        if (cur_w as u64) * (cur_h as u64) > MAX_PIXELS {
            return Err(DecodeError::TooLarge);
        }

        if transform != WICBitmapTransformRotate0 {
            let rot = factory.CreateBitmapFlipRotator()?;
            rot.Initialize(&source, transform)?;
            source = rot.cast()?;
        }

        let conv = factory.CreateFormatConverter()?;
        conv.Initialize(
            &source,
            &GUID_WICPixelFormat32bppPBGRA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeMedianCut,
        )?;
        let (mut ow, mut oh) = (0u32, 0u32);
        conv.GetSize(&mut ow, &mut oh)?;
        let stride = ow as usize * 4;
        let mut pixels = vec![0u8; stride * oh as usize];
        conv.CopyPixels(std::ptr::null(), stride as u32, &mut pixels)?;
        drop(file);

        let has_alpha = pixels.chunks_exact(4).any(|p| p[3] != 255);
        Ok(Decoded {
            width: ow,
            height: oh,
            stride,
            pixels,
            full_size,
            has_alpha,
        })
    }
}

fn bits_per_pixel(factory: &IWICImagingFactory, fmt: &GUID) -> Option<u32> {
    unsafe {
        let info = factory.CreateComponentInfo(fmt).ok()?;
        let pf: IWICPixelFormatInfo = info.cast().ok()?;
        pf.GetBitsPerPixel().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orientation_swaps_axes_for_quarter_turns() {
        for o in 1..=4 {
            assert!(!orientation_transform(o).1, "{o}");
        }
        for o in 5..=8 {
            assert!(orientation_transform(o).1, "{o}");
        }
    }
}
