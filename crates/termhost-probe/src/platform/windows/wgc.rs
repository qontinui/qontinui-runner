//! Windows.Graphics.Capture of one window — the primary Windows source (D2).
//! `Direct3D11CaptureFrame.SystemRelativeTime` is the QPC time DWM composed
//! the frame; frames arrive only when the window changed, so their intervals
//! are the window's update cadence. The mouse cursor is excluded (it could sit
//! on a marker), and the capture border is turned off where Windows allows.

use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError};
use std::time::Duration;

use windows::core::{Interface, HRESULT};
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{
    Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Graphics::Direct3D11::{ID3D11Texture2D, D3D11_TEXTURE2D_DESC};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
use windows::Win32::UI::WindowsAndMessaging::IsWindow;

use super::d3d::{self, Device, Staging};
use super::hwnd;
use crate::clock::hundred_ns_to_ns;
use crate::platform::{CaptureSource, CaptureStatus, RawFrame, WindowHandle};
use crate::records::SourceId;

const BUFFERS: i32 = 3;

pub struct WgcSource {
    target: u64,
    dev: Device,
    d3d: IDirect3DDevice,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    size: SizeInt32,
    arrived: Receiver<()>,
    token: i64,
    staging: Staging,
    border: &'static str,
}

// SAFETY: the WinRT capture objects are agile (free-threaded pool), and the
// D3D11 device and context are used only by the one thread that owns this
// source at a time (the capture thread it is moved into).
unsafe impl Send for WgcSource {}

impl WgcSource {
    pub fn open(target: WindowHandle) -> Result<WgcSource, String> {
        // SAFETY: initialising the calling thread's apartment; an already
        // initialised thread returns a success code or a harmless error.
        let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
        if !GraphicsCaptureSession::IsSupported().unwrap_or(false) {
            return Err("Windows.Graphics.Capture is not supported here (GraphicsCaptureSession.IsSupported is false)".into());
        }
        let h = hwnd(target);
        // SAFETY: IsWindow accepts any value.
        if !unsafe { IsWindow(Some(h)) }.as_bool() {
            return Err(format!("{target} is not a window"));
        }
        let dev = d3d::create(None)?;
        let dxgi: IDXGIDevice = dev.device.cast().map_err(|e| format!("IDXGIDevice: {e}"))?;
        // SAFETY: a valid DXGI device in, a WinRT device out.
        let d3d: IDirect3DDevice = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }
            .and_then(|i| i.cast())
            .map_err(|e| format!("CreateDirect3D11DeviceFromDXGIDevice: {e}"))?;
        let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
            .map_err(|e| format!("IGraphicsCaptureItemInterop: {e}"))?;
        // SAFETY: a live HWND (checked above).
        let item: GraphicsCaptureItem =
            unsafe { interop.CreateForWindow(h) }.map_err(|e| format!("CreateForWindow: {e}"))?;
        let size = item
            .Size()
            .map_err(|e| format!("GraphicsCaptureItem.Size: {e}"))?;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &d3d,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            BUFFERS,
            size,
        )
        .map_err(|e| format!("Direct3D11CaptureFramePool.CreateFreeThreaded: {e}"))?;
        let (tx, arrived) = sync_channel::<()>(64);
        let token = pool
            .FrameArrived(&TypedEventHandler::new(move |_, _| {
                let _ = tx.try_send(());
                Ok(())
            }))
            .map_err(|e| format!("FrameArrived: {e}"))?;
        let session = pool
            .CreateCaptureSession(&item)
            .map_err(|e| format!("CreateCaptureSession: {e}"))?;
        // Windows 10 2004+; older builds keep capturing the cursor, which the
        // checksum then rejects wherever it covers a marker.
        let _ = session.SetIsCursorCaptureEnabled(false);
        let border = if session.SetIsBorderRequired(false).is_ok() {
            "border off"
        } else {
            "border on (this Windows requires it)"
        };
        session
            .StartCapture()
            .map_err(|e| format!("StartCapture: {e}"))?;
        Ok(WgcSource {
            target: target.0,
            dev,
            d3d,
            pool,
            session,
            size,
            arrived,
            token,
            staging: Staging::default(),
            border,
        })
    }

    /// Take one frame if one is ready.
    fn take(&mut self, f: &mut dyn FnMut(RawFrame<'_>)) -> Result<bool, String> {
        let frame = match self.pool.TryGetNextFrame() {
            Ok(fr) => fr,
            // A null frame comes back as an empty error: no frame ready.
            Err(e) if e.code() == HRESULT(0) => return Ok(false),
            Err(e) => return Err(format!("TryGetNextFrame: {e}")),
        };
        let t_ns = hundred_ns_to_ns(
            frame
                .SystemRelativeTime()
                .map_err(|e| format!("SystemRelativeTime: {e}"))?
                .Duration,
        );
        let content = frame
            .ContentSize()
            .map_err(|e| format!("ContentSize: {e}"))?;
        let tex: ID3D11Texture2D = frame
            .Surface()
            .and_then(|s| s.cast::<IDirect3DDxgiInterfaceAccess>())
            // SAFETY: GetInterface returns an owned COM reference.
            .and_then(|a| unsafe { a.GetInterface::<ID3D11Texture2D>() })
            .map_err(|e| format!("frame surface: {e}"))?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: writes one struct.
        unsafe { tex.GetDesc(&mut desc) };
        let w = (content.Width.max(0) as u32).min(desc.Width);
        let h = (content.Height.max(0) as u32).min(desc.Height);
        let result = if w > 0 && h > 0 {
            self.staging
                .read(&self.dev, &tex, (0, 0, w, h), |view| {
                    f(RawFrame {
                        t_ns,
                        view,
                        origin: (0, 0),
                        // WGC drops frames silently: it cannot say how many.
                        missed: None,
                        window_size: (w, h),
                    })
                })
                .map(|_| true)
        } else {
            Ok(false)
        };
        let _ = frame.Close();
        if content.Width > 0
            && content.Height > 0
            && (content.Width != self.size.Width || content.Height != self.size.Height)
        {
            self.pool
                .Recreate(
                    &self.d3d,
                    DirectXPixelFormat::B8G8R8A8UIntNormalized,
                    BUFFERS,
                    content,
                )
                .map_err(|e| format!("Recreate after a resize: {e}"))?;
            self.size = content;
        }
        result
    }
}

impl CaptureSource for WgcSource {
    fn id(&self) -> SourceId {
        SourceId::Wgc
    }

    fn describe(&self) -> String {
        format!(
            "Windows.Graphics.Capture of {} ({}x{}, {} D3D11 device, {})",
            WindowHandle(self.target),
            self.size.Width,
            self.size.Height,
            self.dev.kind,
            self.border
        )
    }

    fn next_frame(&mut self, timeout: Duration, f: &mut dyn FnMut(RawFrame<'_>)) -> CaptureStatus {
        match self.take(f) {
            Ok(true) => return CaptureStatus::Frame,
            Ok(false) => {}
            Err(e) => return CaptureStatus::Lost(e),
        }
        match self.arrived.recv_timeout(timeout) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => {}
            Err(RecvTimeoutError::Timeout) => {
                // SAFETY: IsWindow accepts any value.
                if !unsafe { IsWindow(Some(hwnd(WindowHandle(self.target)))) }.as_bool() {
                    return CaptureStatus::Ended("the window was closed".into());
                }
                return CaptureStatus::Timeout;
            }
        }
        match self.take(f) {
            Ok(true) => CaptureStatus::Frame,
            Ok(false) => CaptureStatus::Timeout,
            Err(e) => CaptureStatus::Lost(e),
        }
    }
}

impl Drop for WgcSource {
    fn drop(&mut self) {
        let _ = self.pool.RemoveFrameArrived(self.token);
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}
