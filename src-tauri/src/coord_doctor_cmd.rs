//! In-app `coord doctor` Tauri command (plan 2026-06-13 Phase 4).
//!
//! Thin runner-binary wrapper over the lib-crate self-check
//! (`qontinui_runner_lib::coord_doctor`). The reusable logic — types, the
//! first-red-stops driver, the report formatter, and the 7-check wiring — lives
//! in the lib so the standalone `coord_doctor` bin can share it. This module's
//! only job is to inject the one fact only the LIVE runtime knows: the
//! ACTUALLY-BOUND loopback API port (check 6), resolved via
//! [`crate::coord_mcp::resolve_bound_api_port`] (the same managed-`AppState`
//! port the proxy `.mcp.json` writer uses). With that injected, the Tauri
//! command and the headless bin produce an IDENTICAL report.
//!
//! Follow-up (out of scope here): a Settings-UI button wired to these commands
//! — that needs temp-runner visual verification and lands in a separate UI
//! phase.

use qontinui_runner_lib::coord_doctor::{diagnose, DoctorInputs, DoctorReport};

/// Run the full `coord doctor` self-check and return the structured report.
/// Reuses the live bound API port so check 6 (`.mcp.json` port == bound port)
/// is authoritative in-app (the headless bin can't know it and reports that
/// link as unverifiable). The frontend renders `DoctorReport`; the
/// `render()`-formatted text is available via [`coord_doctor_text`].
#[tauri::command]
pub fn coord_doctor_run() -> DoctorReport {
    let inputs = DoctorInputs {
        bound_api_port: crate::coord_mcp::resolve_bound_api_port(),
        claude_config_dir: None,
    };
    diagnose(&inputs)
}

/// Same self-check, returned as the copy-pasteable text report (identical to
/// the standalone bin's stdout). Convenient for a "copy report" button.
#[tauri::command]
pub fn coord_doctor_text() -> String {
    coord_doctor_run().render()
}
