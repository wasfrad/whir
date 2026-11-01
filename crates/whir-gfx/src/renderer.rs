use std::sync::Arc;

use egui::epaint::{ClippedPrimitive, textures::TexturesDelta};
use windows::Win32::Foundation::HWND;

use crate::Device;
use crate::painter::Painter;
use crate::surface::{Offscreen, PresentResult, Surface};
use crate::textures::GpuTextures;

/// Where a frame goes.
enum Target {
    Window(Surface),
    Offscreen(Offscreen),
}

/// Device + render target + painter for one window.
pub struct Renderer {
    device: Device,
    target: Target,
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
        Self::with_target(device, textures, Target::Window(surface))
    }

    /// A renderer with no window, for pictures of the UI: see [`Self::read_pixels`].
    pub fn offscreen(
        device: Device,
        textures: Arc<GpuTextures>,
        size_px: [u32; 2],
    ) -> windows::core::Result<Self> {
        let target = Offscreen::new(&device, size_px[0], size_px[1])?;
        Self::with_target(device, textures, Target::Offscreen(target))
    }

    fn with_target(
        device: Device,
        textures: Arc<GpuTextures>,
        target: Target,
    ) -> windows::core::Result<Self> {
        let painter = Painter::new(&device.device)?;
        textures.set_device(
            &device.device,
            device.max_texture_side(),
            device.npot_mipmaps(),
        );
        Ok(Self {
            device,
            target,
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
        match &self.target {
            Target::Window(surface) => surface.size(),
            Target::Offscreen(offscreen) => offscreen.size(),
        }
    }

    pub fn resize(&mut self, size_px: [u32; 2]) {
        match &mut self.target {
            Target::Window(surface) => {
                let _ = surface.resize(size_px[0], size_px[1]);
            }
            Target::Offscreen(offscreen) => {
                if let Ok(resized) = Offscreen::new(&self.device, size_px[0], size_px[1]) {
                    *offscreen = resized;
                }
            }
        }
    }

    /// What the last frame drew (top-down BGRA rows), if this renderer has no
    /// window.
    pub fn read_pixels(&self) -> Option<Vec<u8>> {
        match &self.target {
            Target::Window(_) => None,
            Target::Offscreen(offscreen) => offscreen.read_pixels(&self.device).ok(),
        }
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
        if let Target::Window(surface) = &self.target {
            surface.wait_for_frame();
        }
        let ctx = self.device.context.clone();
        self.set_textures(delta);

        let rtv = match &mut self.target {
            Target::Window(surface) => surface.render_target(&self.device),
            Target::Offscreen(offscreen) => Ok(offscreen.render_target()),
        };
        let Ok(rtv) = rtv else {
            self.free_textures(delta);
            return PresentResult::DeviceLost;
        };
        unsafe {
            ctx.ClearRenderTargetView(&rtv, &clear.to_normalized_gamma_f32());
        }
        let size = self.size();
        let _ = self.painter.paint(
            &ctx,
            &rtv,
            size,
            pixels_per_point,
            primitives,
            &self.textures,
        );
        self.free_textures(delta);
        match &self.target {
            Target::Window(surface) => surface.present(vsync),
            Target::Offscreen(_) => PresentResult::Ok,
        }
    }
}
