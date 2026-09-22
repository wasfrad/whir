use std::sync::Arc;

/// Pixel layout of texture data handed to [`TextureRegistry::create`].
/// Both are 8 bits per channel, premultiplied alpha, gamma (sRGB) encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    /// WIC `32bppPBGRA` / PDFium `FPDFBitmap_BGRA`.
    Bgra8Premultiplied,
    /// egui `Color32` order.
    Rgba8Premultiplied,
}

/// Magnification filter. Minification is always linear (trilinear when mipmapped).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Filter {
    #[default]
    Linear,
    Nearest,
}

#[derive(Clone, Copy, Debug)]
pub struct TextureDesc {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    /// Generate a full mip chain on the GPU for good-looking downscaling.
    pub mipmaps: bool,
    pub filter: Filter,
}

/// Creates GPU textures that egui can draw as `TextureId::User`.
///
/// Implementations are free-threaded: workers call [`create`](Self::create)
/// directly after decoding, so uploads never block the UI thread. If the GPU
/// device is still initializing, `create` blocks until it is ready.
pub trait TextureRegistry: Send + Sync {
    /// `pixels` holds `height` rows of `stride` bytes each.
    fn create(
        self: Arc<Self>,
        desc: &TextureDesc,
        pixels: &[u8],
        stride: usize,
    ) -> Result<UserTexture, String>;

    /// Largest width/height a single texture may have.
    fn max_texture_side(&self) -> u32;

    /// Called when a [`UserTexture`] is dropped.
    fn release(&self, id: u64);
}

/// RAII handle to a registered GPU texture. Dropping it frees the texture.
pub struct UserTexture {
    id: u64,
    size: [u32; 2],
    registry: Arc<dyn TextureRegistry>,
}

impl UserTexture {
    /// For [`TextureRegistry`] implementations only.
    pub fn new(id: u64, size: [u32; 2], registry: Arc<dyn TextureRegistry>) -> Self {
        Self { id, size, registry }
    }

    #[inline]
    pub fn id(&self) -> egui::TextureId {
        egui::TextureId::User(self.id)
    }

    #[inline]
    pub fn size(&self) -> [u32; 2] {
        self.size
    }

    #[inline]
    pub fn size_vec2(&self) -> egui::Vec2 {
        egui::vec2(self.size[0] as f32, self.size[1] as f32)
    }
}

impl Drop for UserTexture {
    fn drop(&mut self) {
        self.registry.release(self.id);
    }
}

impl std::fmt::Debug for UserTexture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UserTexture")
            .field("id", &self.id)
            .field("size", &self.size)
            .finish()
    }
}
