//! Upload a decoded image as one or more GPU textures (tiles), for images
//! larger than the GPU's maximum texture size.

use std::sync::Arc;

use whir_core::texture::{Filter, PixelFormat, TextureDesc, TextureRegistry, UserTexture};

use crate::decode::Decoded;

pub struct Tile {
    /// Top-left in level pixels.
    pub origin: [u32; 2],
    pub size: [u32; 2],
    pub texture: UserTexture,
}

/// One resolution level of an image on the GPU.
pub struct Level {
    /// Pixel size of this level.
    pub size: [u32; 2],
    /// Full-resolution size of the image (after orientation).
    pub full_size: [u32; 2],
    pub has_alpha: bool,
    pub tiles: Vec<Tile>,
}

impl Level {
    pub fn is_full_resolution(&self) -> bool {
        self.size == self.full_size
    }

    /// Level pixels per full-resolution pixel (≤ 1).
    pub fn scale(&self) -> f32 {
        self.size[0] as f32 / self.full_size[0].max(1) as f32
    }
}

pub fn upload(textures: &Arc<dyn TextureRegistry>, d: &Decoded) -> Result<Level, String> {
    let max = textures.max_texture_side().max(1024);
    let mut tiles = Vec::new();
    let mut y = 0;
    while y < d.height {
        let th = (d.height - y).min(max);
        let mut x = 0;
        while x < d.width {
            let tw = (d.width - x).min(max);
            let offset = y as usize * d.stride + x as usize * 4;
            let desc = TextureDesc {
                width: tw,
                height: th,
                format: PixelFormat::Bgra8Premultiplied,
                mipmaps: true,
                filter: Filter::Linear,
            };
            let texture = textures.clone().create(&desc, &d.pixels[offset..], d.stride)?;
            tiles.push(Tile {
                origin: [x, y],
                size: [tw, th],
                texture,
            });
            x += tw;
        }
        y += th;
    }
    Ok(Level {
        size: [d.width, d.height],
        full_size: d.full_size,
        has_alpha: d.has_alpha,
        tiles,
    })
}
