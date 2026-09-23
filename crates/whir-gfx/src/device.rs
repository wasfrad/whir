use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP, D3D_FEATURE_LEVEL,
    D3D_FEATURE_LEVEL_9_3, D3D_FEATURE_LEVEL_10_0, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_11_0,
    D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_FLAG, D3D11_SDK_VERSION,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext,
};

/// The D3D11 device. Created on a helper thread during startup, in parallel with
/// window creation. The device is free-threaded (no `SINGLETHREADED` flag) so
/// workers can create textures; the immediate context is UI-thread only.
pub struct Device {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    pub feature_level: D3D_FEATURE_LEVEL,
    /// Running on the WARP software rasterizer (no GPU, RDP/VDI, `--warp`).
    pub warp: bool,
}

impl Device {
    pub fn create(force_warp: bool) -> windows::core::Result<Self> {
        if !force_warp {
            match Self::create_with(D3D_DRIVER_TYPE_HARDWARE) {
                Ok(d) => return Ok(d),
                Err(_) => { /* fall through to WARP */ }
            }
        }
        Self::create_with(D3D_DRIVER_TYPE_WARP)
    }

    fn create_with(driver: D3D_DRIVER_TYPE) -> windows::core::Result<Self> {
        let base = D3D11_CREATE_DEVICE_BGRA_SUPPORT;
        if cfg!(debug_assertions) && std::env::var_os("WHIR_D3D_DEBUG").is_some() {
            use windows::Win32::Graphics::Direct3D11::D3D11_CREATE_DEVICE_DEBUG;
            if let Ok(d) = Self::create_raw(driver, base | D3D11_CREATE_DEVICE_DEBUG) {
                return Ok(d);
            }
        }
        Self::create_raw(driver, base)
    }

    fn create_raw(
        driver: D3D_DRIVER_TYPE,
        flags: D3D11_CREATE_DEVICE_FLAG,
    ) -> windows::core::Result<Self> {
        let levels = [
            D3D_FEATURE_LEVEL_11_1,
            D3D_FEATURE_LEVEL_11_0,
            D3D_FEATURE_LEVEL_10_1,
            D3D_FEATURE_LEVEL_10_0,
            D3D_FEATURE_LEVEL_9_3,
        ];
        let mut device = None;
        let mut context = None;
        let mut level = D3D_FEATURE_LEVEL::default();
        unsafe {
            D3D11CreateDevice(
                None,
                driver,
                HMODULE::default(),
                flags,
                Some(&levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                Some(&mut level),
                Some(&mut context),
            )?;
        }
        Ok(Self {
            device: device.expect("D3D11CreateDevice returned no device"),
            context: context.expect("D3D11CreateDevice returned no context"),
            feature_level: level,
            warp: driver == D3D_DRIVER_TYPE_WARP,
        })
    }

    /// Largest texture width/height guaranteed by the feature level.
    pub fn max_texture_side(&self) -> u32 {
        max_texture_side(self.feature_level)
    }

    /// Feature levels 9.x only allow mip chains on power-of-two textures.
    pub fn npot_mipmaps(&self) -> bool {
        self.feature_level.0 >= D3D_FEATURE_LEVEL_10_0.0
    }
}

pub fn max_texture_side(level: D3D_FEATURE_LEVEL) -> u32 {
    if level.0 >= D3D_FEATURE_LEVEL_11_0.0 {
        16384
    } else if level.0 >= D3D_FEATURE_LEVEL_10_0.0 {
        8192
    } else {
        4096
    }
}
