use windows::Win32::Foundation::{DXGI_STATUS_OCCLUDED, HANDLE, HWND, WAIT_OBJECT_0};
use windows::Win32::Graphics::Direct3D11::{ID3D11RenderTargetView, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    DXGI_ERROR_DEVICE_REMOVED, DXGI_ERROR_DEVICE_RESET, DXGI_MWA_NO_ALT_ENTER, DXGI_PRESENT,
    DXGI_RGBA, DXGI_SCALING_NONE, DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1,
    DXGI_SWAP_CHAIN_FLAG, DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT, DXGI_SWAP_EFFECT,
    DXGI_SWAP_EFFECT_DISCARD, DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
    DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIAdapter, IDXGIDevice, IDXGIFactory2, IDXGISwapChain1,
    IDXGISwapChain2,
};
use windows::Win32::System::Threading::WaitForSingleObjectEx;
use windows::core::Interface as _;

use crate::Device;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentResult {
    Ok,
    /// Window fully covered/minimized: stop rendering until something changes.
    Occluded,
    /// The GPU device was removed or reset; everything must be recreated.
    DeviceLost,
}

/// The window's swapchain.
pub struct Surface {
    swapchain: IDXGISwapChain1,
    rtv: Option<ID3D11RenderTargetView>,
    waitable: Option<HANDLE>,
    flags: DXGI_SWAP_CHAIN_FLAG,
    width: u32,
    height: u32,
}

impl Surface {
    pub fn new(
        device: &Device,
        hwnd: HWND,
        width: u32,
        height: u32,
        background: [f32; 3],
    ) -> windows::core::Result<Self> {
        unsafe {
            let dxgi_device: IDXGIDevice = device.device.cast()?;
            let adapter: IDXGIAdapter = dxgi_device.GetAdapter()?;
            let factory: IDXGIFactory2 = adapter.GetParent()?;

            // Flip model first (Win10+); fall back to blt model for odd remoting stacks.
            let attempts: [(DXGI_SWAP_EFFECT, bool); 3] = [
                (DXGI_SWAP_EFFECT_FLIP_DISCARD, true),
                (DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL, true),
                (DXGI_SWAP_EFFECT_DISCARD, false),
            ];
            let mut last_err = None;
            for (effect, flip) in attempts {
                let flags = if flip {
                    DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT
                } else {
                    DXGI_SWAP_CHAIN_FLAG(0)
                };
                let desc = DXGI_SWAP_CHAIN_DESC1 {
                    Width: width.max(1),
                    Height: height.max(1),
                    Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    Stereo: false.into(),
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                    BufferCount: if flip { 2 } else { 1 },
                    // NONE: during live resize DWM shows the old frame unscaled
                    // (plus our background color) instead of a stretched smear.
                    Scaling: if flip {
                        DXGI_SCALING_NONE
                    } else {
                        DXGI_SCALING_STRETCH
                    },
                    SwapEffect: effect,
                    AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                    Flags: flags.0 as u32,
                };
                match factory.CreateSwapChainForHwnd(&device.device, hwnd, &desc, None, None) {
                    Ok(swapchain) => {
                        let _ = factory.MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER);
                        let _ = swapchain.SetBackgroundColor(&DXGI_RGBA {
                            r: background[0],
                            g: background[1],
                            b: background[2],
                            a: 1.0,
                        });
                        let mut waitable = None;
                        if flip && let Ok(sc2) = swapchain.cast::<IDXGISwapChain2>() {
                            let _ = sc2.SetMaximumFrameLatency(1);
                            let h = sc2.GetFrameLatencyWaitableObject();
                            if !h.is_invalid() {
                                waitable = Some(h);
                            }
                        }
                        return Ok(Self {
                            swapchain,
                            rtv: None,
                            waitable,
                            flags,
                            width: width.max(1),
                            height: height.max(1),
                        });
                    }
                    Err(e) => last_err = Some(e),
                }
            }
            Err(last_err.expect("at least one attempt"))
        }
    }

    pub fn size(&self) -> [u32; 2] {
        [self.width, self.height]
    }

    pub fn resize(&mut self, width: u32, height: u32) -> windows::core::Result<()> {
        let (width, height) = (width.max(1), height.max(1));
        if (width, height) == (self.width, self.height) {
            return Ok(());
        }
        self.rtv = None;
        unsafe {
            self.swapchain
                .ResizeBuffers(0, width, height, DXGI_FORMAT_UNKNOWN, self.flags)?;
        }
        self.width = width;
        self.height = height;
        Ok(())
    }

    /// Block until the swapchain can accept another frame (latency 1).
    pub fn wait_for_frame(&self) {
        if let Some(h) = self.waitable {
            unsafe {
                let r = WaitForSingleObjectEx(h, 250, true);
                let _ = r == WAIT_OBJECT_0;
            }
        }
    }

    pub fn render_target(
        &mut self,
        device: &Device,
    ) -> windows::core::Result<ID3D11RenderTargetView> {
        if let Some(rtv) = &self.rtv {
            return Ok(rtv.clone());
        }
        unsafe {
            let back: ID3D11Texture2D = self.swapchain.GetBuffer(0)?;
            let mut rtv = None;
            device
                .device
                .CreateRenderTargetView(&back, None, Some(&mut rtv))?;
            let rtv = rtv.expect("CreateRenderTargetView returned nothing");
            self.rtv = Some(rtv.clone());
            Ok(rtv)
        }
    }

    pub fn present(&self, vsync: bool) -> PresentResult {
        let hr = unsafe { self.swapchain.Present(u32::from(vsync), DXGI_PRESENT(0)) };
        if hr == DXGI_ERROR_DEVICE_REMOVED || hr == DXGI_ERROR_DEVICE_RESET {
            PresentResult::DeviceLost
        } else if hr == DXGI_STATUS_OCCLUDED {
            PresentResult::Occluded
        } else {
            PresentResult::Ok
        }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        if let Some(h) = self.waitable.take() {
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(h);
            }
        }
    }
}
