//! D3D11 rendering for whir: device, swapchain, and a purpose-built egui painter.

mod device;
mod painter;
mod renderer;
mod surface;
mod textures;

pub use device::{Device, max_texture_side};
pub use renderer::Renderer;
pub use surface::PresentResult;
pub use textures::GpuTextures;
