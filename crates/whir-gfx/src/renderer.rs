use std::sync::Arc;

use egui::epaint::{ClippedPrimitive, textures::TexturesDelta};
use windows::Win32::Foundation::HWND;

use crate::Device;
use crate::painter::Painter;
use crate::surface::{PresentResult, Surface};
use crate::textures::GpuTextures;

/// Device + swapchain + painter for one window.
pub struct Renderer {
    device: Device,
    surface: Surface,
    painter: Painter,
    textures: Arc<GpuTextures>,
}

impl Renderer {
    pub fn new(
        device: Device,
        textures: Arc<GpuTextures>,
        hwnd: HWND,
        size_px: [u32; 2],
        background: egui::Color32,
    ) -> windows::core::Result<Self> {
        let [r, g, b, _] = background.to_normalized_gamma_f32();
        let surface = Surface::new(&device, hwnd, size_px[0], size_px[1], [r, g, b])?;
        let painter = Painter::new(&device.device)?;
        textures.set_device(
            &device.device,
            device.max_texture_side(),
            device.npot_mipmaps(),
        );
        Ok(Self {
            device,
            surface,
            painter,
            textures,
        })
    }

    pub fn device(&self) -> &Device {
        &self.device
    }

    pub fn max_texture_side(&self) -> u32 {
        self.device.max_texture_side()
    }

    pub fn size(&self) -> [u32; 2] {
        self.surface.size()
    }

    pub fn resize(&mut self, size_px: [u32; 2]) {
        let _ = self.surface.resize(size_px[0], size_px[1]);
    }

    /// Apply texture uploads without drawing (minimized / occluded frames), so
    /// egui's font atlas never falls out of sync.
    pub fn apply_textures(&mut self, delta: &mut TexturesDelta) {
        self.set_textures(delta);
        self.free_textures(delta);
    }

    fn set_textures(&mut self, delta: &mut TexturesDelta) {
        let ctx = self.device.context.clone();
        for (id, image_delta) in delta.set.drain() {
            for d in &image_delta {
                let _ = self.painter.set_texture(&ctx, id, d);
            }
        }
    }

    fn free_textures(&mut self, delta: &mut TexturesDelta) {
        for id in delta.free.drain() {
            self.painter.free_texture(id);
        }
    }

    /// Draw one frame and present it. Consumes the texture delta.
    pub fn render(
        &mut self,
        primitives: &[ClippedPrimitive],
        delta: &mut TexturesDelta,
        pixels_per_point: f32,
        clear: egui::Color32,
        vsync: bool,
    ) -> PresentResult {
        self.surface.wait_for_frame();
        let ctx = self.device.context.clone();
        self.set_textures(delta);

        let Ok(rtv) = self.surface.render_target(&self.device) else {
            self.free_textures(delta);
            return PresentResult::DeviceLost;
        };
        unsafe {
            ctx.ClearRenderTargetView(&rtv, &clear.to_normalized_gamma_f32());
        }
        let size = self.surface.size();
        let _ = self.painter.paint(
            &ctx,
            &rtv,
            size,
            pixels_per_point,
            primitives,
            &self.textures,
        );
        self.free_textures(delta);
        self.surface.present(vsync)
    }
}
