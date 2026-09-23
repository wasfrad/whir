//! User textures (`egui::TextureId::User`) created from any thread.
//!
//! Textures are IMMUTABLE and created in one call on the calling (worker)
//! thread, mip chain included: the UI thread does no upload work, and there's
//! never a second, temporary copy of the texture in GPU memory.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use whir_core::texture::{PixelFormat, TextureDesc, TextureRegistry, UserTexture};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_SHADER_RESOURCE, D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_IMMUTABLE, ID3D11Device, ID3D11ShaderResourceView,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC,
};

use crate::painter::SamplerKey;

pub(crate) struct Entry {
    pub srv: ID3D11ShaderResourceView,
    pub sampler: SamplerKey,
}

struct State {
    device: Option<ID3D11Device>,
    /// Device creation failed: fail texture creation instead of waiting forever.
    failed: bool,
    max_side: u32,
    /// Mip chains allowed on non-power-of-two textures (feature level ≥ 10.0).
    npot_mipmaps: bool,
    entries: HashMap<u64, Entry>,
}

/// Free-threaded registry of GPU textures drawable by egui.
pub struct GpuTextures {
    state: Mutex<State>,
    device_ready: Condvar,
    next_id: AtomicU64,
}

impl GpuTextures {
    /// Created before the device exists, so documents can start decoding immediately.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                device: None,
                failed: false,
                max_side: 4096,
                npot_mipmaps: true,
                entries: HashMap::new(),
            }),
            device_ready: Condvar::new(),
            next_id: AtomicU64::new(1),
        })
    }

    pub fn set_device(&self, device: &ID3D11Device, max_side: u32, npot_mipmaps: bool) {
        let mut st = self.lock();
        st.device = Some(device.clone());
        st.failed = false;
        st.max_side = max_side;
        st.npot_mipmaps = npot_mipmaps;
        drop(st);
        self.device_ready.notify_all();
    }

    pub fn set_failed(&self) {
        self.lock().failed = true;
        self.device_ready.notify_all();
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn wait_device(&self) -> Result<(ID3D11Device, u32, bool), String> {
        let mut st = self.lock();
        while st.device.is_none() && !st.failed {
            st = self
                .device_ready
                .wait(st)
                .unwrap_or_else(|e| e.into_inner());
        }
        match &st.device {
            Some(d) => Ok((d.clone(), st.max_side, st.npot_mipmaps)),
            None => Err("no GPU device".to_owned()),
        }
    }

    /// Resolve user texture ids for one frame.
    pub(crate) fn lookup(
        &self,
        ids: impl Iterator<Item = u64>,
        out: &mut HashMap<u64, (ID3D11ShaderResourceView, SamplerKey)>,
    ) {
        let st = self.lock();
        for id in ids {
            if out.contains_key(&id) {
                continue;
            }
            if let Some(e) = st.entries.get(&id) {
                out.insert(id, (e.srv.clone(), e.sampler));
            }
        }
    }

    /// Drop every texture (device lost). Documents must re-create theirs.
    pub fn clear(&self) {
        let mut st = self.lock();
        st.entries.clear();
        st.device = None;
    }
}

/// Halve a premultiplied 8-bit RGBA/BGRA image with a 2×2 box filter.
fn downsample(src: &[u8], w: u32, h: u32, stride: usize) -> (Vec<u8>, u32, u32) {
    let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
    let mut out = vec![0u8; nw as usize * nh as usize * 4];
    let (w, h) = (w as usize, h as usize);
    for y in 0..nh as usize {
        let r0 = (2 * y).min(h - 1) * stride;
        let r1 = (2 * y + 1).min(h - 1) * stride;
        let dst = &mut out[y * nw as usize * 4..(y + 1) * nw as usize * 4];
        for x in 0..nw as usize {
            let c0 = (2 * x).min(w - 1) * 4;
            let c1 = (2 * x + 1).min(w - 1) * 4;
            for ch in 0..4 {
                let sum = src[r0 + c0 + ch] as u32
                    + src[r0 + c1 + ch] as u32
                    + src[r1 + c0 + ch] as u32
                    + src[r1 + c1 + ch] as u32;
                dst[x * 4 + ch] = ((sum + 2) / 4) as u8;
            }
        }
    }
    (out, nw, nh)
}

impl TextureRegistry for GpuTextures {
    fn create(
        self: Arc<Self>,
        desc: &TextureDesc,
        pixels: &[u8],
        stride: usize,
    ) -> Result<UserTexture, String> {
        let (device, max_side, npot_mipmaps) = self.wait_device()?;
        let (w, h) = (desc.width, desc.height);
        let mipmaps =
            desc.mipmaps && (npot_mipmaps || (w.is_power_of_two() && h.is_power_of_two()));
        if w == 0 || h == 0 {
            return Err("empty texture".to_owned());
        }
        if w > max_side || h > max_side {
            return Err(format!(
                "texture {w}x{h} exceeds the GPU limit of {max_side}"
            ));
        }
        if stride < w as usize * 4 || pixels.len() < stride * (h as usize - 1) + w as usize * 4 {
            return Err("pixel buffer too small".to_owned());
        }
        let format = match desc.format {
            PixelFormat::Bgra8Premultiplied => DXGI_FORMAT_B8G8R8A8_UNORM,
            PixelFormat::Rgba8Premultiplied => DXGI_FORMAT_R8G8B8A8_UNORM,
        };

        // Mip chain built here, on the worker thread.
        let mut levels: Vec<(Vec<u8>, u32, u32)> = Vec::new();
        if mipmaps {
            let (mut cw, mut ch) = (w, h);
            while cw > 1 || ch > 1 {
                let next = match levels.last() {
                    None => downsample(pixels, cw, ch, stride),
                    Some((prev, pw, ph)) => downsample(prev, *pw, *ph, *pw as usize * 4),
                };
                (cw, ch) = (next.1, next.2);
                levels.push(next);
            }
        }
        let mut init = Vec::with_capacity(levels.len() + 1);
        init.push(D3D11_SUBRESOURCE_DATA {
            pSysMem: pixels.as_ptr().cast(),
            SysMemPitch: stride as u32,
            SysMemSlicePitch: 0,
        });
        for (data, lw, _) in &levels {
            init.push(D3D11_SUBRESOURCE_DATA {
                pSysMem: data.as_ptr().cast(),
                SysMemPitch: lw * 4,
                SysMemSlicePitch: 0,
            });
        }

        let tex_desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: init.len() as u32,
            ArraySize: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_IMMUTABLE,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let err = |e: windows::core::Error| format!("texture creation failed: {e}");
        let mut texture = None;
        unsafe { device.CreateTexture2D(&tex_desc, Some(init.as_ptr()), Some(&mut texture)) }
            .map_err(err)?;
        let texture = texture.ok_or("no texture")?;
        drop(levels);
        let mut srv = None;
        unsafe { device.CreateShaderResourceView(&texture, None, Some(&mut srv)) }.map_err(err)?;
        let srv = srv.ok_or("no view")?;

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.lock().entries.insert(
            id,
            Entry {
                srv,
                sampler: SamplerKey::user(desc.filter, mipmaps),
            },
        );
        let registry: Arc<dyn TextureRegistry> = self;
        Ok(UserTexture::new(id, [w, h], registry))
    }

    fn max_texture_side(&self) -> u32 {
        self.wait_device().map(|d| d.1).unwrap_or(4096)
    }

    fn release(&self, id: u64) {
        self.lock().entries.remove(&id);
    }
}

#[cfg(test)]
mod tests {
    use super::downsample;

    #[test]
    fn box_filter_halves_and_averages() {
        // 3x2 image: odd width clamps the last column.
        let px = |v: u8| [v, v, v, 255];
        let src: Vec<u8> = [px(0), px(100), px(200), px(40), px(60), px(80)].concat();
        let (out, w, h) = downsample(&src, 3, 2, 12);
        assert_eq!((w, h), (1, 1));
        assert_eq!(out[0], 50); // (0 + 100 + 40 + 60) / 4
        assert_eq!(out[3], 255);
        let (out, w, h) = downsample(&out, 1, 1, 4);
        assert_eq!((w, h, out.len()), (1, 1, 4));
    }
}
