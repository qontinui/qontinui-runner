//! Resource-guard settings commands (plan
//! `2026-08-07-runner-resource-guard-and-session-protection.md`, Part B item 2).
//!
//! Backs the `Settings > Resource Guard` panel. Two settings groups, one
//! module, on purpose: the panel edits BOTH the live-session floors
//! ([`settings::SessionGuardSettings`]) and the CI-node floors
//! ([`settings::CiNodeSettings`]), because they are the two resource lanes on
//! this machine and an operator raising one almost always wants to see the
//! other. `ci_node` had no Settings UI at all before this panel — its own doc
//! comment described it as hand-edited JSON — and the plan is explicit that
//! shipping the first resource-lane panel while leaving a second hand-edited
//! surface behind is the wrong trade.
//!
//! Shape mirrors `commands/lock_yield_policy_settings.rs`: a `get_`/`save_`
//! pair per group, each `#[tauri::command]` delegating to an `_impl` that
//! returns [`AppError`], plus a `plugin()` for parity. See `settings.rs` for
//! the on-disk schema and for why both writers go through `update_settings`.
//!
//! **These commands are writers, not gates.** The one invariant that would be
//! silently corrupting — a critical floor above the warn floor, which would fire
//! the heavier verdict first — is rejected here rather than persisted, because
//! there is no later reader that could distinguish "operator meant it" from
//! "operator transposed two fields".

use crate::error::AppError;
use crate::settings::{self, CiNodeSettings, SessionGuardSettings};
use anyhow::Result;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
use tracing::info;

use super::CommandResponse;

// ---------------------------------------------------------------------------
// Validation (pure — the same shape `ci_node::settings_directive::validate`
// uses, so the rules can be tested without a settings file on disk)
// ---------------------------------------------------------------------------

/// `true` when the two session-guard floors are transposed.
///
/// A warn is the LIGHTER verdict and must therefore fire first, which means it
/// has to be the HIGHER floor. Equal floors are legal — "warn and block at the
/// same point" is a blunt but coherent operator choice.
fn session_floors_are_inverted(warn_bytes: u64, critical_bytes: u64) -> bool {
    critical_bytes > warn_bytes
}

/// `true` when an allowlist entry is a wildcard.
///
/// Refused locally for the same reason `ci_node::settings_directive` refuses it
/// remotely: it turns an opt-in list into "build anything this machine is
/// handed". A UI that can write what the remote door rejects is how a validated
/// field stops being validated.
fn allowlist_has_wildcard(repo_allowlist: &[String]) -> bool {
    repo_allowlist.iter().any(|r| r.trim() == "*")
}

// ---------------------------------------------------------------------------
// Session guard (live-session protection floors)
// ---------------------------------------------------------------------------

/// Internal implementation of [`get_session_guard_settings`].
fn get_session_guard_settings_impl() -> Result<CommandResponse, AppError> {
    info!("Getting session-guard settings");

    let guard = settings::get_session_guard_settings();
    let data = serde_json::to_value(&guard)?;

    Ok(CommandResponse {
        success: true,
        message: Some("Session-guard settings retrieved".to_string()),
        data: Some(data),
    })
}

/// Get the current live-session protection floors.
#[tauri::command]
pub fn get_session_guard_settings() -> Result<CommandResponse, String> {
    get_session_guard_settings_impl().map_err(String::from)
}

/// Internal implementation of [`save_session_guard_settings`].
fn save_session_guard_settings_impl(
    enabled: bool,
    warn_free_commit_bytes: u64,
    critical_free_commit_bytes: u64,
) -> Result<CommandResponse, AppError> {
    info!(
        "Saving session-guard settings: enabled={}, warn_free_commit_bytes={}, \
         critical_free_commit_bytes={}",
        enabled, warn_free_commit_bytes, critical_free_commit_bytes
    );

    // The ladder's ordering is the whole design (see `SessionGuardSettings`):
    // persisting the inverse would leave a machine whose spawns are blocked
    // before they are ever warned about.
    if session_floors_are_inverted(warn_free_commit_bytes, critical_free_commit_bytes) {
        return Err(AppError::ConfigError(format!(
            "critical floor ({critical_free_commit_bytes} bytes) must not exceed the warn floor \
             ({warn_free_commit_bytes} bytes) — a warn is the lighter verdict and must fire first"
        )));
    }

    let guard = SessionGuardSettings {
        warn_free_commit_bytes,
        critical_free_commit_bytes,
        enabled,
    };
    settings::save_session_guard_settings(guard).map_err(AppError::ConfigError)?;

    Ok(CommandResponse {
        success: true,
        message: Some("Session-guard settings saved".to_string()),
        data: None,
    })
}

/// Save the live-session protection floors.
#[tauri::command]
pub fn save_session_guard_settings(
    enabled: bool,
    warn_free_commit_bytes: u64,
    critical_free_commit_bytes: u64,
) -> Result<CommandResponse, String> {
    save_session_guard_settings_impl(enabled, warn_free_commit_bytes, critical_free_commit_bytes)
        .map_err(String::from)
}

// ---------------------------------------------------------------------------
// CI node (the other resource lane on this machine)
// ---------------------------------------------------------------------------

/// Internal implementation of [`get_ci_node_settings`].
fn get_ci_node_settings_impl() -> Result<CommandResponse, AppError> {
    info!("Getting CI-node settings");

    let ci_node = settings::get_ci_node_settings();
    let data = serde_json::to_value(&ci_node)?;

    Ok(CommandResponse {
        success: true,
        message: Some("CI-node settings retrieved".to_string()),
        data: Some(data),
    })
}

/// Get the current CI-node admission floors.
#[tauri::command]
pub fn get_ci_node_settings() -> Result<CommandResponse, String> {
    get_ci_node_settings_impl().map_err(String::from)
}

/// Internal implementation of [`save_ci_node_settings`].
fn save_ci_node_settings_impl(
    enabled: bool,
    max_concurrent_builds: u32,
    repo_allowlist: Vec<String>,
    min_free_disk_gb: u64,
) -> Result<CommandResponse, AppError> {
    info!(
        "Saving CI-node settings: enabled={}, max_concurrent_builds={}, \
         repo_allowlist={:?}, min_free_disk_gb={}",
        enabled, max_concurrent_builds, repo_allowlist, min_free_disk_gb
    );

    // The same two values coord's remote configuration door refuses (see
    // `ci_node::settings_directive`): a wildcard allowlist, and a zero disk
    // floor that disables the guard it names. The local door must not be the
    // looser of the two.
    if allowlist_has_wildcard(&repo_allowlist) {
        return Err(AppError::ConfigError(
            "repo allowlist must name repos explicitly — '*' is refused here for the same \
             reason ci_node::settings_directive refuses it remotely"
                .to_string(),
        ));
    }
    if min_free_disk_gb == 0 {
        return Err(AppError::ConfigError(
            "minimum free disk must be > 0 GiB — a 0 floor disables the guard rather than \
             relaxing it (this box has hit os error 112)"
                .to_string(),
        ));
    }

    let ci_node = CiNodeSettings {
        enabled,
        max_concurrent_builds,
        repo_allowlist: repo_allowlist
            .into_iter()
            .map(|r| r.trim().to_string())
            .filter(|r| !r.is_empty())
            .collect(),
        min_free_disk_gb,
        // This form has no field for `canonical_converge`, so it must not
        // DECIDE it. Struct-update carries the persisted value through, which
        // means an unrelated save here (toggling `enabled`, editing the
        // allowlist) cannot silently revoke a convergence grant the owner made
        // through the audited web/coord directive — nor, in the other
        // direction, invent one: the only writer that can set it true is that
        // directive. The same rule protects any field added here later.
        ..settings::get_ci_node_settings()
    };
    settings::save_ci_node_settings(ci_node).map_err(AppError::ConfigError)?;

    Ok(CommandResponse {
        success: true,
        message: Some("CI-node settings saved".to_string()),
        data: None,
    })
}

/// Save the CI-node admission floors.
#[tauri::command]
pub fn save_ci_node_settings(
    enabled: bool,
    max_concurrent_builds: u32,
    repo_allowlist: Vec<String>,
    min_free_disk_gb: u64,
) -> Result<CommandResponse, String> {
    save_ci_node_settings_impl(
        enabled,
        max_concurrent_builds,
        repo_allowlist,
        min_free_disk_gb,
    )
    .map_err(String::from)
}

/// Tauri plugin exposing the resource-guard settings commands.
///
/// Left in place for parity with the rest of `commands/*_settings.rs`;
/// the runtime registration goes through main.rs's central
/// `generate_handler!`.
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_resource_guard_settings")
        .invoke_handler(tauri::generate_handler![
            get_session_guard_settings,
            save_session_guard_settings,
            get_ci_node_settings,
            save_ci_node_settings,
        ])
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1024 * 1024 * 1024;

    /// The ordering rule, on the pure predicate: only a strictly higher
    /// critical floor is a transposition. Equal floors ("warn and block at the
    /// same point") are a blunt but coherent choice, and the shipped defaults
    /// must of course be legal.
    #[test]
    fn only_a_strictly_higher_critical_floor_is_inverted() {
        assert!(session_floors_are_inverted(GIB, 3 * GIB));
        assert!(!session_floors_are_inverted(2 * GIB, 2 * GIB));
        assert!(!session_floors_are_inverted(3 * GIB, 3 * GIB / 2));
        let d = SessionGuardSettings::default();
        assert!(!session_floors_are_inverted(
            d.warn_free_commit_bytes,
            d.critical_free_commit_bytes
        ));
    }

    /// An inverted ladder is refused rather than persisted: with
    /// `critical > warn` a machine would block a spawn it had never warned
    /// about. The refusal must name both numbers, or the operator cannot see
    /// which of the two fields they transposed.
    #[test]
    fn inverted_floors_are_refused_before_any_write() {
        let err = save_session_guard_settings_impl(true, GIB, 3 * GIB)
            .expect_err("critical above warn must not persist");
        let msg = String::from(err);
        assert!(msg.contains(&GIB.to_string()), "{msg}");
        assert!(msg.contains(&(3 * GIB).to_string()), "{msg}");
    }

    /// A wildcard allowlist is refused locally exactly as
    /// `ci_node::settings_directive` refuses it remotely — including when it is
    /// hidden among real entries or padded with whitespace.
    #[test]
    fn wildcard_allowlist_is_refused() {
        assert!(allowlist_has_wildcard(&["*".to_string()]));
        assert!(allowlist_has_wildcard(&[
            "qontinui/qontinui-runner".to_string(),
            " * ".to_string(),
        ]));
        assert!(!allowlist_has_wildcard(&[
            "qontinui/qontinui-runner".to_string()
        ]));

        let err = save_ci_node_settings_impl(true, 1, vec!["*".to_string()], 20)
            .expect_err("a wildcard allowlist must not persist");
        assert!(String::from(err).contains("allowlist"));
    }

    /// A zero disk floor disables the guard it names, so it is refused.
    #[test]
    fn zero_disk_floor_is_refused() {
        let err = save_ci_node_settings_impl(true, 1, Vec::new(), 0)
            .expect_err("a 0 GiB disk floor must not persist");
        assert!(String::from(err).contains("disk"));
    }
}
