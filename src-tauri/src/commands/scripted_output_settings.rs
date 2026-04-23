//! Tauri commands for the scripted-output settings panel.
//!
//! Thin wrappers over [`crate::config_facade::get_setting`] /
//! [`crate::config_facade::save_setting`] for the
//! [`ScriptedOutputSettings`] struct. Kept in a dedicated file so the
//! provider-selection panel has a single, well-scoped surface to call into
//! — the existing `ai_settings` commands deliberately don't include
//! `scripted_output.*` because the emitter and chat provider are disjoint
//! subsystems.
//!
//! Both commands are idempotent and safe to call at any time: the setting
//! is re-read from disk on every emit call
//! (`resolve_provider_mode` in `script_emitter.rs`), so a save takes effect
//! on the next call with no runner restart required.

use crate::step_output::script_emitter::ScriptedOutputSettings;

/// Return the current [`ScriptedOutputSettings`] from the persisted
/// settings file. Used by the provider-selection UI to render the initial
/// state of the form controls.
#[tauri::command]
pub fn get_scripted_output_settings() -> Result<ScriptedOutputSettings, String> {
    Ok(crate::config_facade::get_setting())
}

/// Persist a new [`ScriptedOutputSettings`] and return the stored value
/// (canonicalized — missing optional fields default via `#[serde(default)]`
/// at load time).
///
/// Callers should treat this like an optimistic write: the setting is
/// picked up by the next emit call without a restart, so a successful
/// return means the change is live.
#[tauri::command]
pub fn save_scripted_output_settings(
    settings: ScriptedOutputSettings,
) -> Result<ScriptedOutputSettings, String> {
    crate::config_facade::save_setting(settings.clone())?;
    Ok(settings)
}
