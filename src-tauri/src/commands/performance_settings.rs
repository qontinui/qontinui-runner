//! Tauri commands for the performance caps panel (plan
//! `2026-07-28-runner-many-sessions-performance` Phase 8).
//!
//! Thin wrappers over [`crate::settings::get_performance_settings`] /
//! [`crate::settings::save_performance_settings`]. The getter serves the
//! mtime-validated process cache — the same one the terminal-spawn path
//! reads, so the UI cannot disagree with what a spawn will actually use.
//!
//! Deliberately NOT a validating surface beyond the floors: the caps are
//! advisory tuning knobs, and the floors
//! ([`crate::settings::MIN_GRID_SCAN_INTERVAL_MS`],
//! [`crate::settings::MIN_SCROLLBACK_CAPACITY`]) are applied at *use* by
//! `effective_*`, not at save, so an operator's typed value round-trips
//! unmangled and the floor stays a single source of truth.

use crate::settings::PerformanceSettings;

/// Return the current performance caps.
#[tauri::command]
pub fn get_performance_settings() -> Result<PerformanceSettings, String> {
    Ok(crate::settings::get_performance_settings())
}

/// Persist new performance caps.
///
/// Live immediately for anything read per-use (terminal spawn: scrollback
/// ring capacity, `share_output`/redaction; the frontend caps). The two
/// grid-scan loops read their interval once at startup, so that one knob
/// needs a runner restart — the settings UI states this.
///
/// Echoes the **accepted input**, not a re-read of disk. That is deliberate
/// (and matches `scripted_output_settings`): a successful return means the
/// value was written and the process cache refreshed, so a re-read would only
/// tell us what we just stored. Floors are applied at use, not at save, so
/// the echo is the value that will be persisted verbatim.
#[tauri::command]
pub fn save_performance_settings(
    settings: PerformanceSettings,
) -> Result<PerformanceSettings, String> {
    crate::settings::save_performance_settings(settings.clone())?;
    Ok(settings)
}
