//! egui → D3D11.
//!
//! One dynamic vertex/index buffer (grown by doubling) holds every mesh of a
//! frame; each `ClippedPrimitive` is a single `DrawIndexed` with its own scissor.

use std::collections::HashMap;

use egui::epaint::{ClippedPrimitive, ImageDelta, Primitive, Vertex};
use egui::{TextureFilter, TextureId, TextureOptions, TextureWrapMode};
use whir_core::texture::{Filter, MULTIPLY};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D::D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_R32_UINT, DXGI_FORMAT_R32G32_FLOAT, DXGI_SAMPLE_DESC,
};
use windows::core::{PCSTR, s};

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
    /// For [`MULTIPLY`] meshes; made the first time one is drawn.
    multiply: Option<ID3D11BlendState>,
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
                multiply: None,
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

    /// destination × source colour, weighted by source alpha (so feathered
    /// edges fade out); destination alpha is left alone.
    fn multiply_blend(&mut self) -> Option<ID3D11BlendState> {
        if self.multiply.is_none() {
            let mut desc = D3D11_BLEND_DESC::default();
            desc.RenderTarget[0] = D3D11_RENDER_TARGET_BLEND_DESC {
                BlendEnable: true.into(),
                SrcBlend: D3D11_BLEND_DEST_COLOR,
                DestBlend: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOp: D3D11_BLEND_OP_ADD,
                SrcBlendAlpha: D3D11_BLEND_ZERO,
                DestBlendAlpha: D3D11_BLEND_ONE,
                BlendOpAlpha: D3D11_BLEND_OP_ADD,
                RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
            };
            unsafe {
                self.device
                    .CreateBlendState(&desc, Some(&mut self.multiply))
            }
            .ok()?;
        }
        self.multiply.clone()
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
                    context.UpdateSubresource(
                        &t.texture,
                        0,
                        Some(&bx),
                        bytes.as_ptr().cast(),
                        pitch,
                        0,
                    );
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
        let mut multiplying = false;
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
                let multiply = mesh.texture_id == MULTIPLY;
                let (srv, key) = match mesh.texture_id {
                    TextureId::Managed(id) => {
                        let t = self.managed.get(&id)?;
                        (t.srv.clone(), t.sampler)
                    }
                    // Just its colour: sampled from the font atlas's white texel.
                    _ if multiply => {
                        let t = self.managed.get(&0)?;
                        (t.srv.clone(), t.sampler)
                    }
                    TextureId::User(id) => self.frame_user_textures.get(&id)?.clone(),
                };
                if multiply != multiplying {
                    let state = if multiply {
                        self.multiply_blend()?
                    } else {
                        self.blend.clone()
                    };
                    unsafe { context.OMSetBlendState(&state, Some(&[0.0; 4]), u32::MAX) };
                    multiplying = multiply;
                }
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
            self.vb = Some(make(
                cap * std::mem::size_of::<Vertex>(),
                D3D11_BIND_VERTEX_BUFFER,
            )?);
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

#[cfg(test)]
mod tests {
    use egui::epaint::{Mesh, WHITE_UV};
    use egui::{Color32, ColorImage, Rect, pos2};

    use super::*;
    use crate::Device;

    const SIDE: u32 = 8;

    /// Paint `meshes` over a cleared `SIDE` × `SIDE` target on the software
    /// rasterizer and read it back as rows of RGBA.
    fn render(clear: [u8; 3], meshes: Vec<Mesh>) -> Vec<[u8; 4]> {
        let Device {
            device, context, ..
        } = Device::create(true).expect("WARP device");
        let mut painter = Painter::new(&device).unwrap();
        // The font atlas: all that is used of it here is a white texel.
        let white = ColorImage::new([1, 1], vec![Color32::WHITE]);
        let atlas = ImageDelta::full(white, TextureOptions::LINEAR);
        painter
            .set_texture(&context, TextureId::Managed(0), &atlas)
            .unwrap();

        let desc = D3D11_TEXTURE2D_DESC {
            Width: SIDE,
            Height: SIDE,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let staging_desc = D3D11_TEXTURE2D_DESC {
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            ..desc
        };
        let primitives: Vec<ClippedPrimitive> = meshes
            .into_iter()
            .map(|m| ClippedPrimitive {
                clip_rect: Rect::EVERYTHING,
                primitive: Primitive::Mesh(m),
            })
            .collect();
        unsafe {
            let (mut target, mut staging, mut rtv) = (None, None, None);
            device
                .CreateTexture2D(&desc, None, Some(&mut target))
                .unwrap();
            device
                .CreateTexture2D(&staging_desc, None, Some(&mut staging))
                .unwrap();
            let (target, staging) = (target.unwrap(), staging.unwrap());
            device
                .CreateRenderTargetView(&target, None, Some(&mut rtv))
                .unwrap();
            let rtv = rtv.unwrap();
            let [r, g, b] = clear.map(|c| c as f32 / 255.0);
            context.ClearRenderTargetView(&rtv, &[r, g, b, 1.0]);
            painter
                .paint(
                    &context,
                    &rtv,
                    [SIDE, SIDE],
                    1.0,
                    &primitives,
                    &GpuTextures::new(),
                )
                .unwrap();

            context.CopyResource(&staging, &target);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .unwrap();
            let mut out = Vec::new();
            for y in 0..SIDE as usize {
                let row = (mapped.pData as *const u8).add(y * mapped.RowPitch as usize);
                for x in 0..SIDE as usize {
                    let p = std::slice::from_raw_parts(row.add(x * 4), 4);
                    out.push([p[0], p[1], p[2], p[3]]);
                }
            }
            context.Unmap(&staging, 0);
            out
        }
    }

    /// A solid quad over the left (`x` < 4) or right half.
    fn half(left: bool, texture: TextureId, color: Color32) -> Mesh {
        let x = if left { 0.0 } else { 4.0 };
        let mut mesh = Mesh::with_texture(texture);
        mesh.add_rect_with_uv(
            Rect::from_min_max(pos2(x, 0.0), pos2(x + 4.0, SIDE as f32)),
            Rect::from_min_max(WHITE_UV, WHITE_UV),
            color,
        );
        mesh
    }

    fn close(got: [u8; 4], want: [u8; 3]) -> bool {
        (0..3).all(|c| got[c].abs_diff(want[c]) <= 1)
    }

    #[test]
    fn multiply_meshes_tint_what_is_under_them() {
        let paper = [200u8, 100, 50];
        let ink = [0xffu8, 0xf0, 0x50];
        let px = render(
            paper,
            vec![half(
                true,
                MULTIPLY,
                Color32::from_rgb(ink[0], ink[1], ink[2]),
            )],
        );
        // What baking the same mark into the pixels gives: (p × c + 127) / 255.
        let baked = [0, 1, 2].map(|c| ((paper[c] as u32 * ink[c] as u32 + 127) / 255) as u8);
        for y in 0..SIDE as usize {
            for x in 0..SIDE as usize {
                let got = px[y * SIDE as usize + x];
                let want = if x < 4 { baked } else { paper };
                assert!(close(got, want), "at {x},{y}: {got:?}, expected {want:?}");
            }
        }
        // Black stays black, white takes the ink exactly.
        let px = render(
            [0, 0, 0],
            vec![half(true, MULTIPLY, Color32::from_rgb(255, 240, 80))],
        );
        assert!(close(px[0], [0, 0, 0]), "{:?}", px[0]);
        let px = render(
            [255, 255, 255],
            vec![half(true, MULTIPLY, Color32::from_rgb(255, 240, 80))],
        );
        assert!(close(px[0], [255, 240, 80]), "{:?}", px[0]);
    }

    #[test]
    fn ordinary_meshes_blend_normally_after_a_multiply_one() {
        // Multiply on the left, then an opaque and a half-transparent quad on
        // the right: the blend state must have switched back.
        let half_red = Color32::from_rgba_unmultiplied(255, 0, 0, 128);
        let px = render(
            [200, 100, 50],
            vec![
                half(true, MULTIPLY, Color32::from_rgb(128, 128, 128)),
                half(false, TextureId::Managed(0), Color32::from_rgb(10, 20, 30)),
                half(false, TextureId::Managed(0), half_red),
            ],
        );
        assert!(close(px[0], [100, 50, 25]), "{:?}", px[0]);
        // Premultiplied "over": src + dst × (1 - a).
        let [r, g, b, a] = half_red.to_array();
        let over = |src: u8, dst: u8| (src as u32 + dst as u32 * (255 - a as u32) / 255) as u8;
        let want = [over(r, 10), over(g, 20), over(b, 30)];
        assert!(close(px[7], want), "{:?}, expected {want:?}", px[7]);
    }
}
