//! User textures (`egui::TextureId::User`) created from any thread.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_FORMAT_SUPPORT_MIP_AUTOGEN,
    D3D11_RESOURCE_MISC_GENERATE_MIPS, D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT, D3D11_USAGE_IMMUTABLE, ID3D11Device, ID3D11DeviceContext,
    ID3D11ShaderResourceView, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC,
};
use whir_core::texture::{PixelFormat, TextureDesc, TextureRegistry, UserTexture};

use crate::painter::SamplerKey;

pub(crate) struct Entry {
    pub srv: ID3D11ShaderResourceView,
    pub sampler: SamplerKey,
    /// False until the UI thread has filled the mip chain.
    pub ready: bool,
}

struct PendingMips {
    id: u64,
    src: ID3D11Texture2D,
    dst: ID3D11Texture2D,
}

struct State {
    device: Option<ID3D11Device>,
    /// Device creation failed: fail texture creation instead of waiting forever.
    failed: bool,
    max_side: u32,
    mip_autogen_bgra: bool,
    mip_autogen_rgba: bool,
    entries: HashMap<u64, Entry>,
    pending: Vec<PendingMips>,
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
                mip_autogen_bgra: false,
                mip_autogen_rgba: false,
                entries: HashMap::new(),
                pending: Vec::new(),
            }),
            device_ready: Condvar::new(),
            next_id: AtomicU64::new(1),
        })
    }

    pub fn set_device(&self, device: &ID3D11Device, max_side: u32) {
        let supports = |format: DXGI_FORMAT| unsafe {
            device
                .CheckFormatSupport(format)
                .map(|s| s & D3D11_FORMAT_SUPPORT_MIP_AUTOGEN.0 as u32 != 0)
                .unwrap_or(false)
        };
        let mut st = self.lock();
        st.device = Some(device.clone());
        st.failed = false;
        st.max_side = max_side;
        st.mip_autogen_bgra = supports(DXGI_FORMAT_B8G8R8A8_UNORM);
        st.mip_autogen_rgba = supports(DXGI_FORMAT_R8G8B8A8_UNORM);
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

    fn wait_device(&self) -> Result<(ID3D11Device, u32, bool, bool), String> {
        let mut st = self.lock();
        while st.device.is_none() && !st.failed {
            st = self
                .device_ready
                .wait(st)
                .unwrap_or_else(|e| e.into_inner());
        }
        match &st.device {
            Some(d) => Ok((d.clone(), st.max_side, st.mip_autogen_bgra, st.mip_autogen_rgba)),
            None => Err("no GPU device".to_owned()),
        }
    }

    /// UI thread, before drawing: copy freshly uploaded mip 0 into mipped
    /// textures and let the GPU build the chain.
    pub(crate) fn process_pending(&self, context: &ID3D11DeviceContext) {
        let pending = std::mem::take(&mut self.lock().pending);
        if pending.is_empty() {
            return;
        }
        let mut done = Vec::with_capacity(pending.len());
        for p in &pending {
            let srv = self.lock().entries.get(&p.id).map(|e| e.srv.clone());
            let Some(srv) = srv else { continue }; // released before we got to it
            unsafe {
                context.CopySubresourceRegion(&p.dst, 0, 0, 0, 0, &p.src, 0, None);
                context.GenerateMips(&srv);
            }
            done.push(p.id);
        }
        let mut st = self.lock();
        for id in done {
            if let Some(e) = st.entries.get_mut(&id) {
                e.ready = true;
            }
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
            if let Some(e) = st.entries.get(&id)
                && e.ready
            {
                out.insert(id, (e.srv.clone(), e.sampler));
            }
        }
    }

    /// Drop every texture (device lost). Documents must re-create theirs.
    pub fn clear(&self) {
        let mut st = self.lock();
        st.entries.clear();
        st.pending.clear();
        st.device = None;
    }
}

impl TextureRegistry for GpuTextures {
    fn create(
        self: Arc<Self>,
        desc: &TextureDesc,
        pixels: &[u8],
        stride: usize,
    ) -> Result<UserTexture, String> {
        let (device, max_side, autogen_bgra, autogen_rgba) = self.wait_device()?;
        let (w, h) = (desc.width, desc.height);
        if w == 0 || h == 0 {
            return Err("empty texture".to_owned());
        }
        if w > max_side || h > max_side {
            return Err(format!("texture {w}x{h} exceeds the GPU limit of {max_side}"));
        }
        if stride < w as usize * 4 || pixels.len() < stride * (h as usize - 1) + w as usize * 4 {
            return Err("pixel buffer too small".to_owned());
        }
        let (format, autogen) = match desc.format {
            PixelFormat::Bgra8Premultiplied => (DXGI_FORMAT_B8G8R8A8_UNORM, autogen_bgra),
            PixelFormat::Rgba8Premultiplied => (DXGI_FORMAT_R8G8B8A8_UNORM, autogen_rgba),
        };
        let mipmaps = desc.mipmaps && autogen && (w > 1 || h > 1);

        let base = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
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
        let init = D3D11_SUBRESOURCE_DATA {
            pSysMem: pixels.as_ptr().cast(),
            SysMemPitch: stride as u32,
            SysMemSlicePitch: 0,
        };
        let err = |e: windows::core::Error| format!("texture creation failed: {e}");

        // The upload (the expensive part) happens here, on the calling worker thread.
        let mut uploaded = None;
        unsafe { device.CreateTexture2D(&base, Some(&init), Some(&mut uploaded)) }.map_err(err)?;
        let uploaded = uploaded.ok_or("no texture")?;

        let (srv_resource, pending_dst) = if mipmaps {
            let mipped_desc = D3D11_TEXTURE2D_DESC {
                MipLevels: 0, // full chain
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
                MiscFlags: D3D11_RESOURCE_MISC_GENERATE_MIPS.0 as u32,
                ..base
            };
            let mut mipped = None;
            unsafe { device.CreateTexture2D(&mipped_desc, None, Some(&mut mipped)) }
                .map_err(err)?;
            let mipped = mipped.ok_or("no texture")?;
            (mipped.clone(), Some(mipped))
        } else {
            (uploaded.clone(), None)
        };

        let mut srv = None;
        unsafe { device.CreateShaderResourceView(&srv_resource, None, Some(&mut srv)) }
            .map_err(err)?;
        let srv = srv.ok_or("no view")?;

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        {
            let mut st = self.lock();
            st.entries.insert(
                id,
                Entry {
                    srv,
                    sampler: SamplerKey::user(desc.filter, mipmaps),
                    ready: pending_dst.is_none(),
                },
            );
            if let Some(dst) = pending_dst {
                st.pending.push(PendingMips {
                    id,
                    src: uploaded,
                    dst,
                });
            }
        }
        let registry: Arc<dyn TextureRegistry> = self;
        Ok(UserTexture::new(id, [w, h], registry))
    }

    fn max_texture_side(&self) -> u32 {
        self.wait_device().map(|d| d.1).unwrap_or(4096)
    }

    fn release(&self, id: u64) {
        let mut st = self.lock();
        st.entries.remove(&id);
        st.pending.retain(|p| p.id != id);
    }
}
