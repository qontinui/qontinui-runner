//! Is this an interactive, unlocked desktop session? (`session.interactive`)
//!
//! Interactive = the process's window station is the visible `WinSta0`, it is
//! not session 0 (services), and the current INPUT desktop opens and is named
//! `Default`. A secure desktop (lock screen, UAC, Ctrl+Alt+Del) refuses to
//! open: that is DEGRADED (interactive but unusable right now), not absent.

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::RemoteDesktop::{ProcessIdToSessionId, WTSGetActiveConsoleSessionId};
use windows::Win32::System::StationsAndDesktops::{
    GetProcessWindowStation, GetUserObjectInformationW, UOI_FLAGS, UOI_NAME, USEROBJECTFLAGS,
};
use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_REMOTESESSION, WSF_VISIBLE};

use super::input::{input_desktop, thread_desktop};
use crate::doctor::{SessionFacts, SessionProblem};

fn station() -> Result<(String, bool), String> {
    // SAFETY: queries on the process's own window station handle (not owned:
    // never closed).
    unsafe {
        let ws = GetProcessWindowStation().map_err(|e| format!("GetProcessWindowStation: {e}"))?;
        let mut name = [0u16; 128];
        let mut needed = 0u32;
        GetUserObjectInformationW(
            HANDLE(ws.0),
            UOI_NAME,
            Some(name.as_mut_ptr() as *mut core::ffi::c_void),
            (name.len() * 2) as u32,
            Some(&mut needed),
        )
        .map_err(|e| format!("window station name: {e}"))?;
        let mut flags = USEROBJECTFLAGS::default();
        GetUserObjectInformationW(
            HANDLE(ws.0),
            UOI_FLAGS,
            Some(&mut flags as *mut USEROBJECTFLAGS as *mut core::ffi::c_void),
            std::mem::size_of::<USEROBJECTFLAGS>() as u32,
            Some(&mut needed),
        )
        .map_err(|e| format!("window station flags: {e}"))?;
        let len = name.iter().position(|&c| c == 0).unwrap_or(name.len());
        Ok((
            String::from_utf16_lossy(&name[..len]),
            flags.dwFlags & WSF_VISIBLE as u32 != 0,
        ))
    }
}

pub fn session_facts() -> SessionFacts {
    let state = (|| {
        let (ws, visible) = station().map_err(SessionProblem::Unknown)?;
        if !ws.eq_ignore_ascii_case("WinSta0") || !visible {
            return Err(SessionProblem::NotInteractive(format!(
                "the process runs in window station {ws:?} (visible: {visible}), not the interactive WinSta0"
            )));
        }
        let mut sid = 0u32;
        // SAFETY: writes one u32 for our own pid.
        unsafe { ProcessIdToSessionId(std::process::id(), &mut sid) }
            .map_err(|e| SessionProblem::Unknown(format!("ProcessIdToSessionId: {e}")))?;
        if sid == 0 {
            return Err(SessionProblem::NotInteractive(
                "session 0 is the services session; it has no desktop".into(),
            ));
        }
        // SAFETY: no arguments.
        let console = unsafe { WTSGetActiveConsoleSessionId() };
        // SAFETY: a constant metric.
        let remote = unsafe { GetSystemMetrics(SM_REMOTESESSION) } != 0;
        let desk = input_desktop().map_err(SessionProblem::Unavailable)?;
        if !desk.eq_ignore_ascii_case("Default") {
            return Err(SessionProblem::Unavailable(format!(
                "the input desktop is {desk:?} (locked, a UAC prompt, or a secure desktop), not Default"
            )));
        }
        let own = thread_desktop().map_err(SessionProblem::Unknown)?;
        if !own.eq_ignore_ascii_case(&desk) {
            return Err(SessionProblem::NotInteractive(format!(
                "the probe runs on desktop {own:?}, not the input desktop {desk:?}: its windows are not shown and its input cannot reach the screen"
            )));
        }
        Ok(format!(
            "WinSta0\\Default is the input desktop of session {sid} ({}{})",
            if sid == console {
                "the console session"
            } else {
                "not the console session"
            },
            if remote { ", a remote session" } else { "" }
        ))
    })();
    SessionFacts {
        supported: Ok(()),
        state,
    }
}
