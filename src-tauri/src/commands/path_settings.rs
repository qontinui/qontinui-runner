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
    adapter_metrics, resolve_plans_dir, resolve_prompts_dir, MetricsSnapshot, ScanDivergence,
};

/// The adapter's last scan-source divergence reading, projected across the
/// Tauri boundary.
///
/// A projection rather than a serde derive on
/// [`ScanDivergence`] itself: `plan_workunit_adapter::trigger` carries no
/// serde dependency, and the wire shape belongs with the other things this
/// module already serializes. `state` is the enum's own
/// [`qontinui_runner_lib::plan_workunit_adapter::ScanDivergenceState::as_str`]
/// tag, which is the contract between the two files.
///
/// Every count is `Option` for the same reason it is on the source type: an
/// absent number is UNKNOWN. Only `state == "measured"` carries
/// `behind`/`ahead`, so a UI can never render "0 behind" for a machine that
/// scanned nothing or failed to measure.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScanDivergenceView {
    /// `not_scanning` | `not_a_git_work_tree` | `measured` | `unknown`.
    pub state: String,
    pub plans_dir: Option<String>,
    pub repo_root: Option<String>,
    /// The repo's own default branch, resolved at scan time (e.g.
    /// `origin/main`) — never a hardcoded guess.
    pub default_ref: Option<String>,
    pub ref_sha: Option<String>,
    pub head_sha: Option<String>,
    /// Commits on `default_ref` the scanned tree lacks: how stale the scan
    /// source is. Measured as of that clone's last fetch — the adapter never
    /// fetches.
    pub behind: Option<u64>,
    /// Commits the scanned tree has that `default_ref` does not.
    pub ahead: Option<u64>,
    /// Seconds since `default_ref` was last known to be refreshed in that
    /// clone. `None` is UNKNOWN (no readable source, or not `measured`) —
    /// never "just now".
    pub ref_age_secs: Option<u64>,
    /// `true` when `behind`/`ahead` are LOWER BOUNDS rather than current
    /// numbers: the ref is older than the adapter's freshness window or of
    /// unknown age. A UI must render a floor as "at least N behind", and ANY
    /// floor with `behind == 0` (whatever `ahead` is) as "unknown", never as
    /// "in step" — the same reading the web read side gives it. Always `false`
    /// off the `measured` state, which has no counts to qualify.
    #[serde(default)]
    pub counts_are_floors: bool,
    /// Why the state is `unknown` or `not_a_git_work_tree`. Never empty on
    /// those two. On `measured`, names why `ref_age_secs` is absent when it is.
    pub detail: Option<String>,
}

impl From<&ScanDivergence> for ScanDivergenceView {
    fn from(d: &ScanDivergence) -> Self {
        Self {
            state: d.state.as_str().to_string(),
            plans_dir: d.plans_dir.clone(),
            repo_root: d.repo_root.clone(),
            default_ref: d.default_ref.clone(),
            ref_sha: d.ref_sha.clone(),
            head_sha: d.head_sha.clone(),
            behind: d.behind,
            ahead: d.ahead,
            ref_age_secs: d.ref_age_secs,
            counts_are_floors: d.counts_are_floors(),
            detail: d.detail.clone(),
        }
    }
}

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
    /// How far the directory the adapter actually scans has drifted from the
    /// ref it is supposed to represent — the reading that makes a plans dir
    /// parked on a peer's branch visible instead of silently authoritative.
    ///
    /// Same UNKNOWN posture as `plan_scan_roots`: `None` means the loop has
    /// not ticked yet, NOT "no divergence". A machine with the tier off ticks
    /// and reports `not_scanning`, so an off machine is a reading here, never
    /// an absence.
    pub plan_scan_divergence: Option<ScanDivergenceView>,
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
        plan_scan_divergence: adapter
            .scan_divergence
            .as_ref()
            .map(ScanDivergenceView::from),
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
    use qontinui_runner_lib::plan_workunit_adapter::ScanDivergenceState;

    fn snapshot(path_resolutions_total: u64, scan_roots: u64) -> MetricsSnapshot {
        snapshot_with_divergence(path_resolutions_total, scan_roots, None)
    }

    fn snapshot_with_divergence(
        path_resolutions_total: u64,
        scan_roots: u64,
        scan_divergence: Option<ScanDivergence>,
    ) -> MetricsSnapshot {
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
            seeded_total: 0,
            seed_errors_total: 0,
            scan_roots,
            path_resolutions_total,
            active_plans_dir: None,
            scan_divergence,
        }
    }

    /// A `Measured` reading with the operator box's own numbers, against a
    /// ref refreshed five minutes ago (the census's cadence) — so fresh.
    fn parked_reading() -> ScanDivergence {
        ScanDivergence {
            state: ScanDivergenceState::Measured,
            plans_dir: Some("/notes/plans".to_string()),
            repo_root: Some("/notes".to_string()),
            default_ref: Some("origin/main".to_string()),
            ref_sha: Some("a".repeat(40)),
            head_sha: Some("b".repeat(40)),
            behind: Some(2153),
            ahead: Some(11),
            ref_age_secs: Some(300),
            detail: None,
            source_repo: Some("notes/plans".to_string()),
            observed_at_unix: Some(1_789_000_000),
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
        assert_eq!(
            off.resolved.plan_scan_divergence, None,
            "before the loop's first tick the divergence is UNKNOWN, not 'none'"
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

    /// The divergence reading crosses the boundary WHOLE — every field, and
    /// the state as its stable snake_case tag. A read surface that dropped
    /// `behind` would leave the defect exactly as invisible as it was.
    #[test]
    fn resolved_view_projects_every_divergence_field() {
        let view = view_from(
            PathSettings {
                plans_dir: Some("/notes/plans".to_string()),
                ..PathSettings::default()
            },
            &snapshot_with_divergence(1, 1, Some(parked_reading())),
            "/logs".to_string(),
        );
        let d = view
            .resolved
            .plan_scan_divergence
            .expect("a ticked loop always has a reading");
        assert_eq!(d.state, "measured");
        assert_eq!(d.plans_dir.as_deref(), Some("/notes/plans"));
        assert_eq!(d.repo_root.as_deref(), Some("/notes"));
        assert_eq!(d.default_ref.as_deref(), Some("origin/main"));
        assert_eq!(d.ref_sha.as_deref(), Some("a".repeat(40).as_str()));
        assert_eq!(d.head_sha.as_deref(), Some("b".repeat(40).as_str()));
        assert_eq!(d.behind, Some(2153));
        assert_eq!(d.ahead, Some(11));
        assert_eq!(d.ref_age_secs, Some(300));
        assert!(!d.counts_are_floors, "a five-minute-old ref is current");
        assert_eq!(d.detail, None);
    }

    /// The floor rule crosses the boundary. A `0/0` taken against a ref seven
    /// hours old — or of unknown age — must reach the UI flagged as a lower
    /// bound, and must not serialize the same as a `0/0` against a fresh ref:
    /// otherwise the one reading that LOOKS like agreement is exactly the one
    /// a UI would show as "in step".
    #[test]
    fn a_zero_against_a_stale_or_unknown_age_ref_crosses_as_a_floor() {
        let fresh_zero = ScanDivergence {
            behind: Some(0),
            ahead: Some(0),
            ..parked_reading()
        };
        let fresh = ScanDivergenceView::from(&fresh_zero);
        assert!(!fresh.counts_are_floors);

        let stale = ScanDivergenceView::from(&ScanDivergence {
            ref_age_secs: Some(7 * 3600),
            ..fresh_zero.clone()
        });
        assert!(stale.counts_are_floors, "a 7h-old ref makes 0/0 a floor");
        assert_eq!((stale.behind, stale.ahead), (Some(0), Some(0)));
        assert_eq!(stale.ref_age_secs, Some(7 * 3600));

        let unknown_age = ScanDivergenceView::from(&ScanDivergence {
            ref_age_secs: None,
            ..fresh_zero
        });
        assert!(
            unknown_age.counts_are_floors,
            "an unknown age is never taken as fresh"
        );
        assert_eq!(unknown_age.ref_age_secs, None);

        let wire = |v: &ScanDivergenceView| serde_json::to_value(v).unwrap();
        assert_ne!(wire(&fresh), wire(&stale));
        assert_eq!(wire(&stale)["counts_are_floors"], serde_json::json!(true));
        assert_eq!(wire(&unknown_age)["ref_age_secs"], serde_json::Value::Null);
    }

    /// A tier-OFF machine is a READING, not a silence: `not_scanning` with no
    /// counts. This is the distinction the whole detector exists for — "we
    /// scan nothing" must not serialize the same as "we scanned and matched".
    #[test]
    fn a_tier_off_machine_reports_not_scanning_rather_than_zero_divergence() {
        let view = view_from(
            PathSettings::default(),
            &snapshot_with_divergence(1, 0, Some(ScanDivergence::not_scanning())),
            "/logs".to_string(),
        );
        assert!(!view.resolved.plan_tier_active);
        let d = view
            .resolved
            .plan_scan_divergence
            .expect("the idle tick records too");
        assert_eq!(d.state, "not_scanning");
        assert_eq!((d.behind, d.ahead), (None, None));
        assert_eq!(d.repo_root, None);
        assert_eq!(d.ref_age_secs, None);
        assert!(
            !d.counts_are_floors,
            "there are no counts to be floors of — the state already says so"
        );

        // And it is distinguishable on the wire from a measured zero.
        let measured_zero = ScanDivergenceView::from(&ScanDivergence {
            behind: Some(0),
            ahead: Some(0),
            ..parked_reading()
        });
        assert_ne!(
            serde_json::to_value(&d).unwrap(),
            serde_json::to_value(&measured_zero).unwrap()
        );
    }
}
