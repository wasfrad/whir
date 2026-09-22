//! egui → D3D11.
//!
//! One dynamic vertex/index buffer (grown by doubling) holds every mesh of a
//! frame; each `ClippedPrimitive` is a single `DrawIndexed` with its own scissor.

use std::collections::HashMap;

use egui::epaint::{ClippedPrimitive, ImageDelta, Primitive, Vertex};
use egui::{TextureFilter, TextureId, TextureOptions, TextureWrapMode};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D::D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_R32_UINT, DXGI_FORMAT_R32G32_FLOAT, DXGI_SAMPLE_DESC,
};
use windows::core::{PCSTR, s};
use whir_core::texture::Filter;

use crate::textures::GpuTextures;

static VS_BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/egui_vs.dxbc"));
static PS_BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/egui_ps.dxbc"));

const _: () = assert!(std::mem::size_of::<Vertex>() == 20);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Lin {
    Point,
    Linear,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SamplerKey {
    min: Lin,
    mag: Lin,
    mip: Lin,
    wrap: TextureWrapMode,
}

impl SamplerKey {
    fn lin(f: TextureFilter) -> Lin {
        match f {
            TextureFilter::Nearest => Lin::Point,
            TextureFilter::Linear => Lin::Linear,
        }
    }

    fn managed(o: &TextureOptions) -> Self {
        Self {
            min: Self::lin(o.minification),
            mag: Self::lin(o.magnification),
            mip: Lin::Point,
            wrap: o.wrap_mode,
        }
    }

    pub(crate) fn user(filter: Filter, mipmaps: bool) -> Self {
        Self {
            min: Lin::Linear,
            mag: match filter {
                Filter::Linear => Lin::Linear,
                Filter::Nearest => Lin::Point,
            },
            mip: if mipmaps { Lin::Linear } else { Lin::Point },
            wrap: TextureWrapMode::ClampToEdge,
        }
    }

    fn d3d_filter(self) -> D3D11_FILTER {
        // D3D11_FILTER bit layout: mip = bit 0, mag = bit 2, min = bit 4.
        let bit = |l: Lin| matches!(l, Lin::Linear) as i32;
        D3D11_FILTER(bit(self.mip) | (bit(self.mag) << 2) | (bit(self.min) << 4))
    }

    fn d3d_address(self) -> D3D11_TEXTURE_ADDRESS_MODE {
        match self.wrap {
            TextureWrapMode::ClampToEdge => D3D11_TEXTURE_ADDRESS_CLAMP,
            TextureWrapMode::Repeat => D3D11_TEXTURE_ADDRESS_WRAP,
            TextureWrapMode::MirroredRepeat => D3D11_TEXTURE_ADDRESS_MIRROR,
        }
    }
}

struct ManagedTexture {
    texture: ID3D11Texture2D,
    srv: ID3D11ShaderResourceView,
    size: [usize; 2],
    sampler: SamplerKey,
}

pub struct Painter {
    device: ID3D11Device,
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    layout: ID3D11InputLayout,
    locals: ID3D11Buffer,
    blend: ID3D11BlendState,
    raster: ID3D11RasterizerState,
    vb: Option<ID3D11Buffer>,
    vb_capacity: usize,
    ib: Option<ID3D11Buffer>,
    ib_capacity: usize,
    samplers: HashMap<SamplerKey, ID3D11SamplerState>,
    managed: HashMap<u64, ManagedTexture>,
    /// Scratch, reused every frame.
    frame_user_textures: HashMap<u64, (ID3D11ShaderResourceView, SamplerKey)>,
}

impl Painter {
    pub fn new(device: &ID3D11Device) -> windows::core::Result<Self> {
        unsafe {
            let mut vs = None;
            device.CreateVertexShader(VS_BYTES, None, Some(&mut vs))?;
            let mut ps = None;
            device.CreatePixelShader(PS_BYTES, None, Some(&mut ps))?;

            let element = |name: PCSTR, format, offset| D3D11_INPUT_ELEMENT_DESC {
                SemanticName: name,
                SemanticIndex: 0,
                Format: format,
                InputSlot: 0,
                AlignedByteOffset: offset,
                InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
                InstanceDataStepRate: 0,
            };
            let elements = [
                element(s!("POSITION"), DXGI_FORMAT_R32G32_FLOAT, 0),
                element(s!("TEXCOORD"), DXGI_FORMAT_R32G32_FLOAT, 8),
                element(s!("COLOR"), DXGI_FORMAT_R8G8B8A8_UNORM, 16),
            ];
            let mut layout = None;
            device.CreateInputLayout(&elements, VS_BYTES, Some(&mut layout))?;

            let mut locals = None;
            device.CreateBuffer(
                &D3D11_BUFFER_DESC {
                    ByteWidth: 16,
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                    CPUAccessFlags: 0,
                    MiscFlags: 0,
                    StructureByteStride: 0,
                },
                None,
                Some(&mut locals),
            )?;

            let mut blend_desc = D3D11_BLEND_DESC::default();
            blend_desc.RenderTarget[0] = D3D11_RENDER_TARGET_BLEND_DESC {
                BlendEnable: true.into(),
                SrcBlend: D3D11_BLEND_ONE,
                DestBlend: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOp: D3D11_BLEND_OP_ADD,
                SrcBlendAlpha: D3D11_BLEND_INV_DEST_ALPHA,
                DestBlendAlpha: D3D11_BLEND_ONE,
                BlendOpAlpha: D3D11_BLEND_OP_ADD,
                RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
            };
            let mut blend = None;
            device.CreateBlendState(&blend_desc, Some(&mut blend))?;

            let mut raster = None;
            device.CreateRasterizerState(
                &D3D11_RASTERIZER_DESC {
                    FillMode: D3D11_FILL_SOLID,
                    CullMode: D3D11_CULL_NONE,
                    FrontCounterClockwise: false.into(),
                    DepthBias: 0,
                    DepthBiasClamp: 0.0,
                    SlopeScaledDepthBias: 0.0,
                    DepthClipEnable: true.into(),
                    ScissorEnable: true.into(),
                    MultisampleEnable: false.into(),
                    AntialiasedLineEnable: false.into(),
                },
                Some(&mut raster),
            )?;

            Ok(Self {
                device: device.clone(),
                vs: vs.unwrap(),
                ps: ps.unwrap(),
                layout: layout.unwrap(),
                locals: locals.unwrap(),
                blend: blend.unwrap(),
                raster: raster.unwrap(),
                vb: None,
                vb_capacity: 0,
                ib: None,
                ib_capacity: 0,
                samplers: HashMap::new(),
                managed: HashMap::new(),
                frame_user_textures: HashMap::new(),
            })
        }
    }

    fn sampler(&mut self, key: SamplerKey) -> Option<ID3D11SamplerState> {
        if let Some(s) = self.samplers.get(&key) {
            return Some(s.clone());
        }
        let address = key.d3d_address();
        let desc = D3D11_SAMPLER_DESC {
            Filter: key.d3d_filter(),
            AddressU: address,
            AddressV: address,
            AddressW: address,
            MipLODBias: 0.0,
            MaxAnisotropy: 1,
            ComparisonFunc: D3D11_COMPARISON_NEVER,
            BorderColor: [0.0; 4],
            MinLOD: 0.0,
            MaxLOD: f32::MAX,
        };
        let mut sampler = None;
        unsafe { self.device.CreateSamplerState(&desc, Some(&mut sampler)) }.ok()?;
        let sampler = sampler?;
        self.samplers.insert(key, sampler.clone());
        Some(sampler)
    }

    /// Apply egui's texture uploads (`TexturesDelta::set`). Call before [`Self::paint`].
    pub fn set_texture(
        &mut self,
        context: &ID3D11DeviceContext,
        id: TextureId,
        delta: &ImageDelta,
    ) -> windows::core::Result<()> {
        let TextureId::Managed(id) = id else {
            return Ok(());
        };
        let egui::ImageData::Color(image) = &delta.image;
        let size = image.size;
        if size[0] == 0 || size[1] == 0 {
            return Ok(());
        }
        let bytes = image.as_raw();
        let pitch = (size[0] * 4) as u32;
        let sampler = SamplerKey::managed(&delta.options);

        if let Some([x, y]) = delta.pos {
            // Partial update (font atlas growth).
            if let Some(t) = self.managed.get_mut(&id) {
                let bx = D3D11_BOX {
                    left: x as u32,
                    top: y as u32,
                    front: 0,
                    right: (x + size[0]) as u32,
                    bottom: (y + size[1]) as u32,
                    back: 1,
                };
                unsafe {
                    context.UpdateSubresource(&t.texture, 0, Some(&bx), bytes.as_ptr().cast(), pitch, 0);
                }
                t.sampler = sampler;
            }
            return Ok(());
        }

        if let Some(t) = self.managed.get_mut(&id)
            && t.size == size
        {
            unsafe {
                context.UpdateSubresource(&t.texture, 0, None, bytes.as_ptr().cast(), pitch, 0);
            }
            t.sampler = sampler;
            return Ok(());
        }

        let desc = D3D11_TEXTURE2D_DESC {
            Width: size[0] as u32,
            Height: size[1] as u32,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let init = D3D11_SUBRESOURCE_DATA {
            pSysMem: bytes.as_ptr().cast(),
            SysMemPitch: pitch,
            SysMemSlicePitch: 0,
        };
        unsafe {
            let mut texture = None;
            self.device
                .CreateTexture2D(&desc, Some(&init), Some(&mut texture))?;
            let texture = texture.unwrap();
            let mut srv = None;
            self.device
                .CreateShaderResourceView(&texture, None, Some(&mut srv))?;
            self.managed.insert(
                id,
                ManagedTexture {
                    texture,
                    srv: srv.unwrap(),
                    size,
                    sampler,
                },
            );
        }
        Ok(())
    }

    /// Apply `TexturesDelta::free`. Call after [`Self::paint`].
    pub fn free_texture(&mut self, id: TextureId) {
        if let TextureId::Managed(id) = id {
            self.managed.remove(&id);
        }
    }

    pub fn paint(
        &mut self,
        context: &ID3D11DeviceContext,
        rtv: &ID3D11RenderTargetView,
        size_px: [u32; 2],
        pixels_per_point: f32,
        primitives: &[ClippedPrimitive],
        user_textures: &GpuTextures,
    ) -> windows::core::Result<()> {
        let meshes = || {
            primitives.iter().filter_map(|p| match &p.primitive {
                Primitive::Mesh(m) if !m.indices.is_empty() => Some((p.clip_rect, m)),
                _ => None,
            })
        };
        let (vtx_count, idx_count) = meshes().fold((0, 0), |(v, i), (_, m)| {
            (v + m.vertices.len(), i + m.indices.len())
        });
        if vtx_count == 0 {
            return Ok(());
        }

        self.ensure_buffers(vtx_count, idx_count)?;
        let vb = self.vb.clone().unwrap();
        let ib = self.ib.clone().unwrap();

        unsafe {
            // Upload every mesh in one map per buffer.
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            context.Map(&vb, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut mapped))?;
            let mut dst = mapped.pData as *mut Vertex;
            for (_, m) in meshes() {
                std::ptr::copy_nonoverlapping(m.vertices.as_ptr(), dst, m.vertices.len());
                dst = dst.add(m.vertices.len());
            }
            context.Unmap(&vb, 0);

            context.Map(&ib, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut mapped))?;
            let mut dst = mapped.pData as *mut u32;
            for (_, m) in meshes() {
                std::ptr::copy_nonoverlapping(m.indices.as_ptr(), dst, m.indices.len());
                dst = dst.add(m.indices.len());
            }
            context.Unmap(&ib, 0);

            let screen_points = [
                size_px[0] as f32 / pixels_per_point,
                size_px[1] as f32 / pixels_per_point,
                0.0,
                0.0,
            ];
            context.UpdateSubresource(&self.locals, 0, None, screen_points.as_ptr().cast(), 0, 0);

            context.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
            context.OMSetBlendState(&self.blend, Some(&[0.0; 4]), u32::MAX);
            context.RSSetState(&self.raster);
            context.RSSetViewports(Some(&[D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: size_px[0] as f32,
                Height: size_px[1] as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            }]));
            context.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            context.IASetInputLayout(&self.layout);
            let stride = std::mem::size_of::<Vertex>() as u32;
            let offset = 0u32;
            context.IASetVertexBuffers(0, 1, Some(&Some(vb)), Some(&stride), Some(&offset));
            context.IASetIndexBuffer(&ib, DXGI_FORMAT_R32_UINT, 0);
            context.VSSetShader(&self.vs, None);
            context.VSSetConstantBuffers(0, Some(&[Some(self.locals.clone())]));
            context.PSSetShader(&self.ps, None);
        }

        // Resolve user textures once for the whole frame.
        self.frame_user_textures.clear();
        user_textures.lookup(
            meshes().filter_map(|(_, m)| match m.texture_id {
                TextureId::User(id) => Some(id),
                TextureId::Managed(_) => None,
            }),
            &mut self.frame_user_textures,
        );

        let (w, h) = (size_px[0] as f32, size_px[1] as f32);
        let mut vertex_base = 0i32;
        let mut index_base = 0u32;
        let mut bound: Option<(TextureId, SamplerKey)> = None;
        for (clip, mesh) in meshes() {
            let (vcount, icount) = (mesh.vertices.len() as i32, mesh.indices.len() as u32);
            let draw = (|| {
                // Scissor in physical pixels.
                let min_x = (clip.min.x * pixels_per_point).round().clamp(0.0, w);
                let min_y = (clip.min.y * pixels_per_point).round().clamp(0.0, h);
                let max_x = (clip.max.x * pixels_per_point).round().clamp(min_x, w);
                let max_y = (clip.max.y * pixels_per_point).round().clamp(min_y, h);
                if max_x <= min_x || max_y <= min_y {
                    return None;
                }
                let (srv, key) = match mesh.texture_id {
                    TextureId::Managed(id) => {
                        let t = self.managed.get(&id)?;
                        (t.srv.clone(), t.sampler)
                    }
                    TextureId::User(id) => self.frame_user_textures.get(&id)?.clone(),
                };
                if bound != Some((mesh.texture_id, key)) {
                    let sampler = self.sampler(key)?;
                    unsafe {
                        context.PSSetShaderResources(0, Some(&[Some(srv)]));
                        context.PSSetSamplers(0, Some(&[Some(sampler)]));
                    }
                    bound = Some((mesh.texture_id, key));
                }
                unsafe {
                    context.RSSetScissorRects(Some(&[RECT {
                        left: min_x as i32,
                        top: min_y as i32,
                        right: max_x as i32,
                        bottom: max_y as i32,
                    }]));
                    context.DrawIndexed(icount, index_base, vertex_base);
                }
                Some(())
            })();
            let _ = draw;
            vertex_base += vcount;
            index_base += icount;
        }
        self.frame_user_textures.clear();
        Ok(())
    }

    fn ensure_buffers(&mut self, vtx: usize, idx: usize) -> windows::core::Result<()> {
        let make = |bytes: usize, bind: D3D11_BIND_FLAG| -> windows::core::Result<ID3D11Buffer> {
            let mut buf = None;
            unsafe {
                self.device.CreateBuffer(
                    &D3D11_BUFFER_DESC {
                        ByteWidth: bytes as u32,
                        Usage: D3D11_USAGE_DYNAMIC,
                        BindFlags: bind.0 as u32,
                        CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
                        MiscFlags: 0,
                        StructureByteStride: 0,
                    },
                    None,
                    Some(&mut buf),
                )?;
            }
            Ok(buf.unwrap())
        };
        if self.vb.is_none() || vtx > self.vb_capacity {
            let cap = vtx.next_power_of_two().max(4096);
            self.vb = Some(make(cap * std::mem::size_of::<Vertex>(), D3D11_BIND_VERTEX_BUFFER)?);
            self.vb_capacity = cap;
        }
        if self.ib.is_none() || idx > self.ib_capacity {
            let cap = idx.next_power_of_two().max(8192);
            self.ib = Some(make(cap * 4, D3D11_BIND_INDEX_BUFFER)?);
            self.ib_capacity = cap;
        }
        Ok(())
    }
}
