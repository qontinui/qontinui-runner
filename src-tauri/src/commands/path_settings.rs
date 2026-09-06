//! Tauri commands for the `paths` settings section (`settings.paths`).
//!
//! Plan `2026-09-05-plans-dir-is-env-only-and-unreachable-in-the-product`,
//! Phase 3 (D5). `PathSettings` had the storage half — the
//! [`crate::config_facade::SettingsField`] impl — and no product surface at
//! all: no UI, no command, no HTTP route. A user configured the plans
//! directory by hand-editing `settings.json` at a path nothing in the product
//! names. These two commands are the missing half, in the shape of
//! [`crate::commands::cost_budget_settings`]: thin wrappers returning the
//! value directly.
//!
//! ## Configured vs. resolved
//!
//! The view carries both the **configured** struct (what is on disk) and the
//! **resolved** values (what is in effect), so the UI can show when the two
//! differ:
//!
//! - `workspace_root` genuinely can: `$QONTINUI_ROOT` / `$QONTINUI_WORKSPACE_ROOT`
//!   outrank the setting (D4 keeps that). It is resolved through
//!   [`crate::workspace_paths::workspace_root_from`], the READ-ONLY twin —
//!   never [`crate::workspace_paths::workspace_root`], which goes through
//!   `get_setting` and is a *write* on a fresh install (it can mint a
//!   `local_user_id`). The command reads the settings once and injects them.
//! - The plan-corpus dirs (`plans_dir`, `prompts_dir`) have no override, so
//!   they differ only by blank-normalisation — and, live, by at most one scan
//!   interval: the adapter re-reads them every tick. `plan_scan_roots` is read
//!   back from the adapter's metrics so "in effect" is measured, not inferred.
//! - `dev_logs_dir` always resolves to something (a platform default when
//!   unset); the process caches it at first use, so a change here is honest
//!   only as "the next runner start".
//!
//! ## Blank is unset
//!
//! Every `Option<String>` path field is normalised on save: a blank or
//! whitespace-only string becomes `None`. Blank means unset everywhere in this
//! codebase (the resolvers, the migration, the session-env injection), and
//! storing `Some("")` would make the on-disk file disagree with every reader
//! of it. `strict_mode` and any field the UI does not show round-trip
//! untouched because the whole struct is persisted.

use serde::{Deserialize, Serialize};

use crate::config_facade;
use crate::settings::PathSettings;
use qontinui_runner_lib::plan_workunit_adapter::trigger::{
    adapter_metrics, resolve_plans_dir, resolve_prompts_dir, MetricsSnapshot,
};

/// What each path setting resolves to **now**.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedPaths {
    /// The active plans dir the adapter resolves from the setting — after
    /// blank-normalisation, this is the setting itself.
    pub plans_dir: Option<String>,
    pub prompts_dir: Option<String>,
    /// Through the read-only workspace resolver; `$QONTINUI_ROOT` and
    /// `$QONTINUI_WORKSPACE_ROOT` outrank the setting here.
    pub workspace_root: Option<String>,
    /// Always resolves — the platform default when unset.
    pub dev_logs_dir: String,
    /// `plans_dir` resolved to something: the markdown-plan tier is armed.
    pub plan_tier_active: bool,
    /// The scan-root count the adapter's loop measured on its last path
    /// resolution. `None` when the loop is not running or has not resolved
    /// the settings yet — UNKNOWN, never `0`.
    pub plan_scan_roots: Option<u32>,
}

/// The whole `paths` section: what is configured, and what is in effect.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathSettingsView {
    pub configured: PathSettings,
    pub resolved: ResolvedPaths,
}

/// Blank → `None` for every `Option<String>` path field, and surrounding
/// whitespace trimmed — the same rule `plans_dir_migration` applies, so a
/// pasted path's stray spaces never become part of a directory name.
/// Everything else verbatim.
pub fn normalize(settings: PathSettings) -> PathSettings {
    let non_blank = |v: Option<String>| v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    PathSettings {
        dev_logs_dir: non_blank(settings.dev_logs_dir),
        plans_dir: non_blank(settings.plans_dir),
        plans_archive_dir: non_blank(settings.plans_archive_dir),
        prompts_dir: non_blank(settings.prompts_dir),
        workspace_root: non_blank(settings.workspace_root),
        strict_mode: settings.strict_mode,
    }
}

/// Build the view from inputs the caller already holds. Pure apart from the
/// workspace resolver's `current_exe()` probe, so the projection is testable
/// without a settings store or a running adapter.
pub fn view_from(
    configured: PathSettings,
    adapter: &MetricsSnapshot,
    dev_logs_dir: String,
) -> PathSettingsView {
    let plans_dir = resolve_plans_dir(configured.plans_dir.clone());
    let resolved = ResolvedPaths {
        plan_tier_active: plans_dir.is_some(),
        plans_dir,
        prompts_dir: resolve_prompts_dir(configured.prompts_dir.clone()),
        workspace_root: crate::workspace_paths::workspace_root_from(
            configured.workspace_root.as_deref(),
        )
        .map(|p| p.display().to_string()),
        dev_logs_dir,
        plan_scan_roots: (adapter.path_resolutions_total > 0)
            .then(|| u32::try_from(adapter.scan_roots).unwrap_or(u32::MAX)),
    };
    PathSettingsView {
        configured,
        resolved,
    }
}

/// The live view: one settings read, one metrics snapshot.
pub fn view() -> PathSettingsView {
    view_from(
        config_facade::get_setting::<PathSettings>(),
        &adapter_metrics().snapshot(),
        crate::paths::get_dev_logs_dir_string(),
    )
}

/// Persist the whole section (blank-normalised) and return the fresh view.
pub fn save(settings: PathSettings) -> Result<PathSettingsView, String> {
    let normalized = normalize(settings);
    config_facade::update_setting::<PathSettings, _>(|paths| *paths = normalized)?;
    Ok(view())
}

/// Return the `paths` section: configured values plus what each resolves to.
#[tauri::command]
pub fn get_path_settings() -> Result<PathSettingsView, String> {
    Ok(view())
}

/// Persist the `paths` section and echo the fresh view. Blank strings are
/// stored as unset; fields the UI does not show round-trip untouched.
#[tauri::command]
pub fn save_path_settings(settings: PathSettings) -> Result<PathSettingsView, String> {
    save(settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(path_resolutions_total: u64, scan_roots: u64) -> MetricsSnapshot {
        MetricsSnapshot {
            scanned: 0,
            transitions_total: 0,
            cycles_total: 0,
            conflicts_total: 0,
            errors_total: 0,
            deferrals_total: 0,
            deps_set_total: 0,
            deps_skipped_unmigrated_total: 0,
            deps_errors_total: 0,
            archive_stamped_total: 0,
            forbidden_total: 0,
            deps_forbidden_total: 0,
            scan_roots,
            path_resolutions_total,
            active_plans_dir: None,
        }
    }

    /// Blank means unset everywhere else, so it must mean unset on disk too —
    /// for every path field, while the non-path flag is carried verbatim.
    #[test]
    fn blank_strings_normalise_to_none() {
        let typed = PathSettings {
            dev_logs_dir: Some("".to_string()),
            plans_dir: Some("   ".to_string()),
            plans_archive_dir: Some("\t".to_string()),
            prompts_dir: Some(" /prompts ".to_string()),
            workspace_root: None,
            strict_mode: true,
        };
        let stored = normalize(typed);
        assert_eq!(stored.dev_logs_dir, None);
        assert_eq!(stored.plans_dir, None);
        assert_eq!(stored.plans_archive_dir, None);
        assert_eq!(
            stored.prompts_dir.as_deref(),
            Some("/prompts"),
            "surrounding whitespace is trimmed, as the migration trims it"
        );
        assert_eq!(stored.workspace_root, None);
        assert!(stored.strict_mode);
    }

    /// Loading and saving without editing must not change what is on disk:
    /// normalisation is idempotent, and a hidden field survives the trip.
    #[test]
    fn get_then_save_is_a_fixed_point() {
        let on_disk = PathSettings {
            dev_logs_dir: None,
            plans_dir: Some("/root/plans".to_string()),
            plans_archive_dir: Some("/root/archive".to_string()),
            prompts_dir: None,
            workspace_root: Some("/root".to_string()),
            strict_mode: true,
        };
        let once = normalize(on_disk.clone());
        assert_eq!(
            serde_json::to_value(&once).unwrap(),
            serde_json::to_value(&on_disk).unwrap()
        );
        let twice = normalize(once.clone());
        assert_eq!(
            serde_json::to_value(&twice).unwrap(),
            serde_json::to_value(&once).unwrap()
        );
    }

    /// The tier flag follows the RESOLVED plans dir, so a blank setting reads
    /// as off — and the scan-root count is UNKNOWN until the loop has resolved
    /// the settings at least once, never a defaulted zero.
    #[test]
    fn resolved_view_reports_tier_state_and_unknown_scan_roots_honestly() {
        let off = view_from(
            PathSettings {
                plans_dir: Some("  ".to_string()),
                ..PathSettings::default()
            },
            &snapshot(0, 0),
            "/logs".to_string(),
        );
        assert!(!off.resolved.plan_tier_active);
        assert_eq!(off.resolved.plans_dir, None);
        assert_eq!(
            off.resolved.plan_scan_roots, None,
            "no resolution yet is UNKNOWN"
        );
        assert_eq!(off.resolved.dev_logs_dir, "/logs");

        let on = view_from(
            PathSettings {
                plans_dir: Some("/root/plans".to_string()),
                prompts_dir: Some("/root/prompts".to_string()),
                ..PathSettings::default()
            },
            &snapshot(3, 2),
            "/logs".to_string(),
        );
        assert!(on.resolved.plan_tier_active);
        assert_eq!(on.resolved.plans_dir.as_deref(), Some("/root/plans"));
        assert_eq!(on.resolved.prompts_dir.as_deref(), Some("/root/prompts"));
        assert_eq!(on.resolved.plan_scan_roots, Some(2));
        // The configured half is echoed as given, not normalised on read.
        assert_eq!(on.configured.plans_dir.as_deref(), Some("/root/plans"));
    }
}
