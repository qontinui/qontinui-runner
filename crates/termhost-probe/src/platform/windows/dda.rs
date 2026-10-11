//! DXGI Desktop Duplication of the output holding the window — the cross-check
//! (D2). `LastPresentTime` is a QPC value; `AccumulatedFrames > 1` means the
//! probe missed frames (reported as `missed`, so the cross-check never compares
//! against a coalesced time). Only the window's rectangle is copied and read;
//! the desktop image is never kept. `ACCESS_LOST` (a desktop switch, a mode
//! change) is reported and the duplication re-established.

use std::time::Duration;

use windows::core::Interface;
use windows::Win32::Foundation::{E_ACCESSDENIED, POINT, RECT};
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_MODE_ROTATION_IDENTITY,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput1, IDXGIOutput5,
    IDXGIOutputDuplication, IDXGIResource, DXGI_ERROR_ACCESS_LOST,
    DXGI_ERROR_NOT_CURRENTLY_AVAILABLE, DXGI_ERROR_SESSION_DISCONNECTED, DXGI_ERROR_UNSUPPORTED,
    DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
};
use windows::Win32::UI::WindowsAndMessaging::IsWindow;

use super::d3d::{self, Device, Staging};
use super::{hwnd, window};
use crate::clock::qpc_ticks_to_ns;
use crate::platform::{CaptureSource, CaptureStatus, RawFrame, WindowHandle};
use crate::raster::Rect;
use crate::records::SourceId;

pub struct DdaSource {
    target: u64,
    adapter_name: String,
    output: IDXGIOutput1,
    output_name: String,
    desktop: Rect,
    dev: Device,
    dupl: Option<IDXGIOutputDuplication>,
    api: &'static str,
    staging: Staging,
}

// SAFETY: the DXGI and D3D11 objects are used only by the one thread that owns
// this source at a time (the capture thread it is moved into).
unsafe impl Send for DdaSource {}

/// The UI restrictions of a job object this process runs in, if any — a
/// concrete reason desktop duplication is refused while capture of our own
/// window works.
fn job_ui_limits() -> Option<String> {
    use windows::Win32::System::JobObjects::{
        IsProcessInJob, JobObjectBasicUIRestrictions, QueryInformationJobObject,
        JOBOBJECT_BASIC_UI_RESTRICTIONS,
    };
    use windows::Win32::System::Threading::GetCurrentProcess;
    let mut in_job = windows::core::BOOL(0);
    // SAFETY: queries on the current process and the job it belongs to.
    unsafe {
        IsProcessInJob(GetCurrentProcess(), None, &mut in_job).ok()?;
        if !in_job.as_bool() {
            return None;
        }
        let mut ui = JOBOBJECT_BASIC_UI_RESTRICTIONS::default();
        QueryInformationJobObject(
            None,
            JobObjectBasicUIRestrictions,
            &mut ui as *mut JOBOBJECT_BASIC_UI_RESTRICTIONS as *mut core::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_BASIC_UI_RESTRICTIONS>() as u32,
            None,
        )
        .ok()?;
        (ui.UIRestrictionsClass.0 != 0).then(|| {
            format!(
                "this process runs in a job object with UI restrictions 0x{:x}",
                ui.UIRestrictionsClass.0
            )
        })
    }
}

fn describe_hr(e: &windows::core::Error) -> String {
    let code = e.code();
    let what = if code == DXGI_ERROR_UNSUPPORTED {
        "DXGI_ERROR_UNSUPPORTED (no duplication in this session or on this adapter)"
    } else if code == E_ACCESSDENIED {
        "E_ACCESSDENIED (this process may not read the desktop image: a secure desktop, a non-interactive session, or a restricted process token or job)"
    } else if code == DXGI_ERROR_NOT_CURRENTLY_AVAILABLE {
        "DXGI_ERROR_NOT_CURRENTLY_AVAILABLE (too many duplications of this output)"
    } else if code == DXGI_ERROR_SESSION_DISCONNECTED {
        "DXGI_ERROR_SESSION_DISCONNECTED"
    } else {
        ""
    };
    let job = if code == E_ACCESSDENIED {
        job_ui_limits()
            .map(|j| format!(" — {j}"))
            .unwrap_or_default()
    } else {
        String::new()
    };
    if what.is_empty() {
        format!("{e}{job}")
    } else {
        format!("{what}: {e}{job}")
    }
}

/// Duplicate `output` with `dev`: `DuplicateOutput`, else (DXGI 1.5)
/// `DuplicateOutput1` asking for B8G8R8A8 — the route a per-monitor-DPI-aware
/// or HDR-desktop process may need.
fn duplicate(
    output: &IDXGIOutput1,
    dev: &Device,
) -> Result<(IDXGIOutputDuplication, &'static str), String> {
    // SAFETY: a device on the output's own adapter, as duplication requires.
    match unsafe { output.DuplicateOutput(&dev.device) } {
        Ok(d) => Ok((d, "DuplicateOutput")),
        Err(e1) => {
            let first = describe_hr(&e1);
            let Ok(out5) = output.cast::<IDXGIOutput5>() else {
                return Err(format!(
                    "DuplicateOutput: {first} (no IDXGIOutput5 to try DuplicateOutput1)"
                ));
            };
            // SAFETY: as above; one supported format.
            match unsafe { out5.DuplicateOutput1(&dev.device, 0, &[DXGI_FORMAT_B8G8R8A8_UNORM]) } {
                Ok(d) => Ok((d, "DuplicateOutput1")),
                Err(e2) => Err(format!(
                    "DuplicateOutput: {first}; DuplicateOutput1: {}",
                    describe_hr(&e2)
                )),
            }
        }
    }
}

fn adapter_name(a: &IDXGIAdapter1) -> String {
    // SAFETY: GetDesc1 fills one struct.
    match unsafe { a.GetDesc1() } {
        Ok(d) => String::from_utf16_lossy(&d.Description)
            .trim_end_matches('\0')
            .to_string(),
        Err(_) => "an unnamed adapter".into(),
    }
}

/// The output whose desktop rectangle holds `p`, with its adapter.
fn output_at(p: POINT) -> Result<(IDXGIAdapter1, IDXGIOutput1, Rect, String), String> {
    // SAFETY: factory and enumeration calls with out-values owned by Rust.
    unsafe {
        let factory: IDXGIFactory1 =
            CreateDXGIFactory1().map_err(|e| format!("CreateDXGIFactory1: {e}"))?;
        let mut a = 0;
        while let Ok(adapter) = factory.EnumAdapters1(a) {
            let mut o = 0;
            while let Ok(output) = adapter.EnumOutputs(o) {
                if let Ok(desc) = output.GetDesc() {
                    let r: RECT = desc.DesktopCoordinates;
                    if desc.AttachedToDesktop.as_bool()
                        && p.x >= r.left
                        && p.x < r.right
                        && p.y >= r.top
                        && p.y < r.bottom
                    {
                        if desc.Rotation != DXGI_MODE_ROTATION_IDENTITY && desc.Rotation.0 != 0 {
                            return Err(format!("the window's output is rotated ({:?}); rotated duplication is not supported", desc.Rotation));
                        }
                        let out1: IDXGIOutput1 = output
                            .cast()
                            .map_err(|e| format!("IDXGIOutput1 (DXGI 1.2): {e}"))?;
                        let name = String::from_utf16_lossy(&desc.DeviceName)
                            .trim_end_matches('\0')
                            .to_string();
                        let rect = Rect {
                            x: r.left,
                            y: r.top,
                            w: (r.right - r.left).max(0) as u32,
                            h: (r.bottom - r.top).max(0) as u32,
                        };
                        return Ok((adapter, out1, rect, name));
                    }
                }
                o += 1;
            }
            a += 1;
        }
    }
    Err(format!(
        "no desktop output contains the point ({}, {})",
        p.x, p.y
    ))
}

impl DdaSource {
    pub fn open(target: WindowHandle) -> Result<DdaSource, String> {
        let h = hwnd(target);
        let b = window::bounds(h).ok_or_else(|| format!("{target} has no bounds"))?;
        let centre = POINT {
            x: b.x + b.w as i32 / 2,
            y: b.y + b.h as i32 / 2,
        };
        let (adapter, output, desktop, output_name) = output_at(centre)?;
        let adapter_name = adapter_name(&adapter);
        let dev = d3d::create(Some(&adapter))?;
        let (dupl, api) = duplicate(&output, &dev)
            .map_err(|e| format!("{e} [output {output_name} on {adapter_name}]"))?;
        Ok(DdaSource {
            target: target.0,
            adapter_name,
            output,
            output_name,
            desktop,
            dev,
            dupl: Some(dupl),
            api,
            staging: Staging::default(),
        })
    }

    fn reestablish(&mut self) -> Result<(), String> {
        // Desktop coordinates may have changed with the mode.
        // SAFETY: GetDesc fills one struct.
        if let Ok(desc) = unsafe { self.output.GetDesc() } {
            let r = desc.DesktopCoordinates;
            self.desktop = Rect {
                x: r.left,
                y: r.top,
                w: (r.right - r.left).max(0) as u32,
                h: (r.bottom - r.top).max(0) as u32,
            };
        }
        let (dupl, api) = duplicate(&self.output, &self.dev)?;
        self.dupl = Some(dupl);
        self.api = api;
        Ok(())
    }

    fn window_alive(&self) -> bool {
        // SAFETY: IsWindow accepts any value.
        unsafe { IsWindow(Some(hwnd(WindowHandle(self.target)))) }.as_bool()
    }
}

impl CaptureSource for DdaSource {
    fn id(&self) -> SourceId {
        SourceId::Dda
    }

    fn describe(&self) -> String {
        format!(
            "DXGI Desktop Duplication ({}) of {} on {} ({}x{} at {},{}), cropped to {}",
            self.api,
            self.output_name,
            self.adapter_name,
            self.desktop.w,
            self.desktop.h,
            self.desktop.x,
            self.desktop.y,
            WindowHandle(self.target)
        )
    }

    fn next_frame(&mut self, timeout: Duration, f: &mut dyn FnMut(RawFrame<'_>)) -> CaptureStatus {
        if !self.window_alive() {
            return CaptureStatus::Ended("the window was closed".into());
        }
        if self.dupl.is_none() {
            if let Err(e) = self.reestablish() {
                std::thread::sleep(timeout);
                return CaptureStatus::Lost(format!("re-establishing duplication: {e}"));
            }
        }
        let Some(dupl) = self.dupl.clone() else {
            return CaptureStatus::Timeout;
        };
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        let ms = timeout.as_millis().min(u128::from(u32::MAX)) as u32;
        // SAFETY: out-pointers to locals; a successful acquire is always
        // released below.
        let acquired = unsafe { dupl.AcquireNextFrame(ms, &mut info, &mut resource) };
        if let Err(e) = acquired {
            let code = e.code();
            if code == DXGI_ERROR_WAIT_TIMEOUT {
                return CaptureStatus::Timeout;
            }
            self.dupl = None;
            if code == DXGI_ERROR_ACCESS_LOST {
                return CaptureStatus::Lost(
                    "DXGI_ERROR_ACCESS_LOST (desktop switch or mode change)".into(),
                );
            }
            return CaptureStatus::Lost(format!("AcquireNextFrame: {}", describe_hr(&e)));
        }
        let status = if info.LastPresentTime == 0 {
            // Only the pointer moved: no new desktop image.
            CaptureStatus::Timeout
        } else {
            let t_ns = qpc_ticks_to_ns(info.LastPresentTime);
            let missed = info.AccumulatedFrames.saturating_sub(1);
            match (
                resource
                    .as_ref()
                    .and_then(|r| r.cast::<ID3D11Texture2D>().ok()),
                window::bounds(hwnd(WindowHandle(self.target))),
            ) {
                (Some(tex), Some(win)) => match win.intersect(&self.desktop) {
                    Some(clip) => {
                        let region = (
                            (clip.x - self.desktop.x) as u32,
                            (clip.y - self.desktop.y) as u32,
                            clip.w,
                            clip.h,
                        );
                        let origin = (clip.x - win.x, clip.y - win.y);
                        match self.staging.read(&self.dev, &tex, region, |view| {
                            f(RawFrame {
                                t_ns,
                                view,
                                origin,
                                missed,
                                window_size: (win.w, win.h),
                            })
                        }) {
                            Ok(()) => CaptureStatus::Frame,
                            Err(e) => CaptureStatus::Lost(e),
                        }
                    }
                    // The window is off this output.
                    None => CaptureStatus::Timeout,
                },
                (None, _) => {
                    CaptureStatus::Lost("the duplicated frame is not a D3D11 texture".into())
                }
                (_, None) => CaptureStatus::Timeout,
            }
        };
        // SAFETY: releases the frame acquired above.
        let _ = unsafe { dupl.ReleaseFrame() };
        status
    }
}
