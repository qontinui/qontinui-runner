//! A D3D11 device and a CPU-readable staging texture: GPU frames are copied
//! into it, mapped, read in place, and unmapped — never written anywhere.

use windows::core::Interface;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN, D3D_DRIVER_TYPE_WARP,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_BOX,
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE,
    D3D11_MAP_READ, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{IDXGIAdapter, IDXGIAdapter1};

use crate::raster::{FrameView, PixelFormat};

pub struct Device {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    pub kind: &'static str,
}

fn create_one(
    adapter: Option<&IDXGIAdapter>,
    driver: D3D_DRIVER_TYPE,
) -> windows::core::Result<(ID3D11Device, ID3D11DeviceContext)> {
    let mut device = None;
    let mut context = None;
    // SAFETY: out-pointers to locals; the call either fills both or fails.
    unsafe {
        D3D11CreateDevice(
            adapter,
            driver,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )?;
    }
    match (device, context) {
        (Some(d), Some(c)) => Ok((d, c)),
        _ => Err(windows::core::Error::empty()),
    }
}

/// A device on `adapter` (DXGI duplication needs the adapter that owns the
/// output), else a hardware device, else WARP (a GPU-less VM).
pub fn create(adapter: Option<&IDXGIAdapter1>) -> Result<Device, String> {
    if let Some(a) = adapter {
        let a: IDXGIAdapter = a.cast().map_err(|e| format!("IDXGIAdapter: {e}"))?;
        return create_one(Some(&a), D3D_DRIVER_TYPE_UNKNOWN)
            .map(|(device, context)| Device {
                device,
                context,
                kind: "adapter",
            })
            .map_err(|e| format!("D3D11CreateDevice on the output's adapter: {e}"));
    }
    match create_one(None, D3D_DRIVER_TYPE_HARDWARE) {
        Ok((device, context)) => Ok(Device {
            device,
            context,
            kind: "hardware",
        }),
        Err(hw) => create_one(None, D3D_DRIVER_TYPE_WARP)
            .map(|(device, context)| Device {
                device,
                context,
                kind: "warp",
            })
            .map_err(|warp| format!("D3D11CreateDevice: hardware {hw}; WARP {warp}")),
    }
}

/// A staging texture reused while the region size stays the same.
#[derive(Default)]
pub struct Staging {
    tex: Option<ID3D11Texture2D>,
    w: u32,
    h: u32,
}

impl Staging {
    /// Copy the `(x, y, w, h)` region of `src` into staging, map it, and call
    /// `f` with the pixels.
    pub fn read<R>(
        &mut self,
        dev: &Device,
        src: &ID3D11Texture2D,
        region: (u32, u32, u32, u32),
        f: impl FnOnce(FrameView<'_>) -> R,
    ) -> Result<R, String> {
        let (x, y, w, h) = region;
        if w == 0 || h == 0 {
            return Err("empty capture region".into());
        }
        let mut src_desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: GetDesc writes one struct.
        unsafe { src.GetDesc(&mut src_desc) };
        if src_desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
            return Err(format!(
                "unsupported frame format {:?} (need B8G8R8A8_UNORM)",
                src_desc.Format
            ));
        }
        if x + w > src_desc.Width || y + h > src_desc.Height {
            return Err(format!(
                "region {x},{y} {w}x{h} outside the {}x{} frame",
                src_desc.Width, src_desc.Height
            ));
        }
        if self.tex.is_none() || self.w != w || self.h != h {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            let mut tex = None;
            // SAFETY: a valid descriptor and an out-pointer to a local.
            unsafe { dev.device.CreateTexture2D(&desc, None, Some(&mut tex)) }
                .map_err(|e| format!("CreateTexture2D (staging): {e}"))?;
            self.tex = tex;
            self.w = w;
            self.h = h;
        }
        let staging = self.tex.as_ref().ok_or("no staging texture")?;
        let bx = D3D11_BOX {
            left: x,
            top: y,
            front: 0,
            right: x + w,
            bottom: y + h,
            back: 1,
        };
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: both textures belong to `dev`; the box lies inside `src`
        // (checked above) and matches the staging size; the mapping is read
        // only while mapped and unmapped before returning.
        unsafe {
            dev.context
                .CopySubresourceRegion(staging, 0, 0, 0, 0, src, 0, Some(&bx));
            dev.context
                .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .map_err(|e| format!("Map (staging): {e}"))?;
            let pitch = mapped.RowPitch as usize;
            let len = pitch * (h as usize - 1) + w as usize * 4;
            let bytes = std::slice::from_raw_parts(mapped.pData as *const u8, len);
            let out = FrameView::new(bytes, w as usize, h as usize, pitch, PixelFormat::Bgra8)
                .map(f)
                .map_err(|e| format!("mapped frame: {e}"));
            dev.context.Unmap(staging, 0);
            out
        }
    }
}
