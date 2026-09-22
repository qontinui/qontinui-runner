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
//! of it. The same per-entry rule applies inside every keyed map: a blank key
//! or a blank value is not an entry.
//!
//! ## The save is a PATCH, because a whole-struct replace erased maps
//!
//! [`save`] takes [`PathSettingsPatch`], not a `PathSettings`, and the
//! distinction is load-bearing. This module used to persist
//! `|paths| *paths = normalize(settings)` — a whole-struct **replace** — and
//! claimed "fields the caller does not show round-trip untouched **because the
//! whole struct is persisted**". That was true of the UI and of nothing else:
//! the preservation was implemented CLIENT-side, by
//! `pathsSettingsHelpers.ts`'s `buildPathSettingsPayload` spreading the
//! previously-fetched `configured` object. `repo_checkouts` carries
//! `#[serde(default, skip_serializing_if = "…is_empty")]`, so a
//! `PUT /settings/paths` body that simply omitted it deserialized to an empty
//! map and **erased the operator's repo mappings**. An agent or a script
//! composing a body by hand did that trivially, and keying three more maps by
//! tenant would have multiplied the same defect by four.
//!
//! So EVERY field on the patch reads the same way:
//!
//! - **absent** ⇒ leave the stored value alone;
//! - **an explicit value** (`"/x"`, `{"k":"v"}`, `true`) ⇒ set it;
//! - **`null`** (scalars) or **`{}`** (maps) ⇒ clear it.
//!
//! ⚠️ **The first cut of this applied absent-means-keep to the MAPS ONLY**, and
//! justified leaving the scalars on replace with "the panel shows all five and
//! sends all five, so a scalar a caller omits is a scalar it means to clear."
//! **The panel shows FOUR.** `plans_archive_dir` was re-sent verbatim from the
//! struct the panel loaded at mount, and `strict_mode` — a bare `bool` whose
//! serde default is `false` — was set OFF by any caller that omitted it. Both
//! are the same lost-update the map fix closed, on fields the panel never
//! displays, and for a replace-scalar an omission cannot be fixed client-side
//! because absent IS the clear.
//!
//! Hence one rule instead of two families: nothing changes by silence. A caller
//! states what it means to change — which is the shape every non-UI caller has,
//! and the reason `double_option` exists so `null` and absent stay distinct.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::config_facade;
use crate::settings::PathSettings;
use qontinui_runner_lib::plan_workunit_adapter::trigger::{
    adapter_metrics, resolve_plans_archive_dir, resolve_plans_dir, resolve_prompts_dir,
    MetricsSnapshot, ScanDivergence,
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

/// What the three keyed directories resolve to for ONE named tenant.
///
/// Present in [`ResolvedPaths`] only when the caller named a tenant, which is
/// what lets the panel show "this is where tenant X's sessions author" beside
/// the device-wide answer. Every field is `Option` for the same reason the
/// device-level ones are: an absent directory means the tier is off for that
/// tenant, which is a reading rather than a gap.
///
/// **`tenant_id` here is the LOOKUP KEY the caller passed, echoed back — never
/// an attribution claim.** It selects a stored string out of
/// `PathSettings::plans_dir_by_tenant` and its twins; nothing downstream may
/// read it as evidence of who owns a captured artifact, which comes from the
/// credential (plan
/// `2026-09-22-plans-dir-is-a-single-path-so-a-multi-bound-device-cannot-author-per-tenant`
/// §2 D1). It is echoed verbatim, including a key the device is not currently
/// bound to: resolution is honest about what it was asked, and the panel labels
/// an unbound key rather than hiding it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedForTenant {
    /// The `tenant_id` the caller named, verbatim.
    pub tenant_id: String,
    /// `plans_dir_by_tenant[tenant_id]`, else the device-wide `plans_dir`.
    pub plans_dir: Option<String>,
    /// `plans_archive_dir_by_tenant[tenant_id]`, else `plans_archive_dir`.
    pub plans_archive_dir: Option<String>,
    /// `prompts_dir_by_tenant[tenant_id]`, else `prompts_dir`.
    pub prompts_dir: Option<String>,
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
    /// The same three keyed directories resolved **for one named tenant**.
    ///
    /// Present only when the caller named a `tenant_id`; with no tenant named
    /// this whole field is absent and the view is byte-identical to the one
    /// served before the settings were keyed. A caller that wants the device
    /// answer asks for nothing, which is the only reading that cannot be
    /// mistaken for a tenant's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_for_tenant: Option<ResolvedForTenant>,
}

/// The whole `paths` section: what is configured, and what is in effect.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathSettingsView {
    pub configured: PathSettings,
    pub resolved: ResolvedPaths,
}

/// The SAVE wire shape: a **patch** over the stored `paths` section.
///
/// Why this exists rather than accepting a whole [`PathSettings`]: see the
/// module doc's "The save is a PATCH" section. In one line — a whole-struct
/// replace let a caller that had never heard of `repo_checkouts` erase it by
/// omission, and three tenant-keyed maps would have made that four ways to lose
/// an operator's configuration.
///
/// **Every field means the same thing: ABSENT ⇒ leave the stored value alone.**
/// Present-and-explicit is the only way to change anything —
/// `"/x"` / `{"k":"v"}` / `true` sets, and `null` / `{}` clears.
///
/// It reached that uniformity in two steps, and the second is worth stating
/// because the first looked finished. Round one made only the four MAPS
/// absent-means-keep, on the reasoning that "a map has no single field a panel
/// shows, so an omission is more likely ignorance of the field than intent to
/// empty it", while the scalars kept replace semantics justified as "the panel
/// shows all five and sends all five, so an omission there is a clear."
///
/// **That justification was false, and the field it was false about is exactly
/// the one that got hurt.** The panel shows FOUR (`plans_dir`, `prompts_dir`,
/// `workspace_root`, `dev_logs_dir`) and sent five: `plans_archive_dir` was
/// re-sent verbatim from the struct the panel loaded at mount, and `strict_mode`
/// the same. Under replace, that is a blind last-writer-wins — a peer that set
/// `plans_archive_dir` through `PUT /settings/paths` while an operator had the
/// panel open was silently reverted by that operator's next save. Identical to
/// the map hazard in every respect except that `delete` could not fix it, since
/// for a replace-scalar an omission IS the clear.
///
/// So the rule is now one rule rather than two families with a rationale each.
/// A caller states what it means to change and omits the rest; nothing is
/// changed by silence. That also makes the door safe for a caller that knows
/// about *some* fields — the shape every non-UI caller actually has.
///
/// `double_option` is what makes `null` distinguishable from absent for the
/// scalars: plain `Option<Option<T>>` folds both to `None`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PathSettingsPatch {
    /// Absent ⇒ the stored value survives; `null` ⇒ unset; a string ⇒ set.
    #[serde(default, deserialize_with = "double_option")]
    pub dev_logs_dir: Option<Option<String>>,
    /// Absent ⇒ the stored value survives; `null` ⇒ unset; a string ⇒ set.
    #[serde(default, deserialize_with = "double_option")]
    pub plans_dir: Option<Option<String>>,
    /// Absent ⇒ the stored value survives; `null` ⇒ unset; a string ⇒ set.
    /// The panel does not show this field, which is why absent must not clear it.
    #[serde(default, deserialize_with = "double_option")]
    pub plans_archive_dir: Option<Option<String>>,
    /// Absent ⇒ the stored value survives; `null` ⇒ unset; a string ⇒ set.
    #[serde(default, deserialize_with = "double_option")]
    pub prompts_dir: Option<Option<String>>,
    /// Absent ⇒ the stored value survives; `null` ⇒ unset; a string ⇒ set.
    #[serde(default, deserialize_with = "double_option")]
    pub workspace_root: Option<Option<String>>,
    /// Absent/`null` ⇒ the stored map survives; `{}` ⇒ a deliberate clear.
    #[serde(default)]
    pub plans_dir_by_tenant: Option<BTreeMap<String, String>>,
    /// Absent/`null` ⇒ the stored map survives; `{}` ⇒ a deliberate clear.
    #[serde(default)]
    pub plans_archive_dir_by_tenant: Option<BTreeMap<String, String>>,
    /// Absent/`null` ⇒ the stored map survives; `{}` ⇒ a deliberate clear.
    #[serde(default)]
    pub prompts_dir_by_tenant: Option<BTreeMap<String, String>>,
    /// Absent/`null` ⇒ the stored map survives; `{}` ⇒ a deliberate clear.
    /// Keyed by coord repo slug, not by tenant — this map is touched here only
    /// to delete the erasure hazard it shared, never to key it by tenant.
    #[serde(default)]
    pub repo_checkouts: Option<BTreeMap<String, String>>,
    /// Absent ⇒ the stored flag survives. It was a bare `bool` with
    /// `#[serde(default)]`, which is the same hazard in its most dangerous
    /// shape: `false` is the serde default, so a caller that omitted the field
    /// silently turned strict mode OFF. The panel does not show it either.
    #[serde(default)]
    pub strict_mode: Option<bool>,
}

/// Deserialize into `Option<Option<T>>` so an ABSENT field and an explicit
/// `null` are different values.
///
/// Serde folds both to `None` for a plain `Option<Option<T>>`, which is
/// precisely the distinction [`PathSettingsPatch`] is built on: absent must mean
/// *leave the stored value alone* and `null` must mean *clear it*. With
/// `#[serde(default, deserialize_with = "double_option")]` an absent field takes
/// the `Default` (`None`) and never reaches this function, while a present one —
/// `null` included — arrives here and is wrapped `Some(..)`.
fn double_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(deserializer).map(Some)
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
        plans_dir_by_tenant: normalize_entries(settings.plans_dir_by_tenant),
        plans_archive_dir_by_tenant: normalize_entries(settings.plans_archive_dir_by_tenant),
        prompts_dir_by_tenant: normalize_entries(settings.prompts_dir_by_tenant),
        // A blank slug or a blank path is not a mapping; both sides trimmed.
        repo_checkouts: normalize_entries(settings.repo_checkouts),
        strict_mode: settings.strict_mode,
    }
}

/// Per-entry trim-and-drop for a keyed path map: a blank key or a blank value
/// is not an entry, and both sides are trimmed.
///
/// ⚠️ **Dropping a blank entry is NOT the same as dropping an unparseable
/// key, and this function does only the first.** A tenant-keyed map whose key
/// is not a currently-bound tenant UUID — or not a UUID at all — is PRESERVED
/// (settings D2: the operator may be re-pairing, and losing their configured
/// directory silently is the failure to avoid). Such a key is inert for
/// resolution and renders in the UI as not-currently-bound. Only a key that is
/// *nothing* is removed, because there is nothing to preserve.
fn normalize_entries(map: BTreeMap<String, String>) -> BTreeMap<String, String> {
    map.into_iter()
        .filter_map(|(key, value)| {
            let (key, value) = (key.trim().to_string(), value.trim().to_string());
            (!key.is_empty() && !value.is_empty()).then_some((key, value))
        })
        .collect()
}

/// Apply `patch` to the `stored` section: **an absent field keeps the stored
/// value, for every field there is.**
///
/// The result is run through [`normalize`], so the persisted form is the
/// canonical one whichever door the patch arrived at. Pure, so the "a
/// pre-change body cannot erase anything" claim is asserted directly.
pub fn merge(stored: &PathSettings, patch: PathSettingsPatch) -> PathSettings {
    // One `unwrap_or_else` per field, and that uniformity IS the fix: an absent
    // field takes the STORED value, so a caller that has never heard of a field
    // cannot change it. An explicit value sets; an explicit `null` (scalars) or
    // `{}` (maps) clears. The scalars carry `Option<Option<_>>` so those two
    // cases are distinguishable at all — see `double_option`.
    normalize(PathSettings {
        dev_logs_dir: patch
            .dev_logs_dir
            .unwrap_or_else(|| stored.dev_logs_dir.clone()),
        plans_dir: patch.plans_dir.unwrap_or_else(|| stored.plans_dir.clone()),
        plans_archive_dir: patch
            .plans_archive_dir
            .unwrap_or_else(|| stored.plans_archive_dir.clone()),
        prompts_dir: patch
            .prompts_dir
            .unwrap_or_else(|| stored.prompts_dir.clone()),
        workspace_root: patch
            .workspace_root
            .unwrap_or_else(|| stored.workspace_root.clone()),
        plans_dir_by_tenant: patch
            .plans_dir_by_tenant
            .unwrap_or_else(|| stored.plans_dir_by_tenant.clone()),
        plans_archive_dir_by_tenant: patch
            .plans_archive_dir_by_tenant
            .unwrap_or_else(|| stored.plans_archive_dir_by_tenant.clone()),
        prompts_dir_by_tenant: patch
            .prompts_dir_by_tenant
            .unwrap_or_else(|| stored.prompts_dir_by_tenant.clone()),
        repo_checkouts: patch
            .repo_checkouts
            .unwrap_or_else(|| stored.repo_checkouts.clone()),
        strict_mode: patch.strict_mode.unwrap_or(stored.strict_mode),
    })
}

/// Build the view from inputs the caller already holds. Pure apart from the
/// workspace resolver's `current_exe()` probe, so the projection is testable
/// without a settings store or a running adapter.
///
/// `tenant` is an optional **lookup key**: when named, `resolved.resolved_for_tenant`
/// carries the three keyed directories resolved for it, so a caller reads a
/// *resolution* rather than having to re-implement the two-rung precedence over
/// the raw struct. When it is `None` the view is byte-identical to the one this
/// function produced before the settings were keyed — the device view.
pub fn view_from(
    configured: PathSettings,
    adapter: &MetricsSnapshot,
    dev_logs_dir: String,
    tenant: Option<&str>,
) -> PathSettingsView {
    // The device-wide answers: no tenant, so every resolution falls through the
    // map to the scalar. These are the directories the adapter's own reconcile
    // loop runs on, which is why they stay the headline of the view.
    let plans_dir = resolve_plans_dir(configured.plans_dir.clone(), &BTreeMap::new(), None);
    let resolved = ResolvedPaths {
        plan_tier_active: plans_dir.is_some(),
        plans_dir,
        prompts_dir: resolve_prompts_dir(configured.prompts_dir.clone(), &BTreeMap::new(), None),
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
        resolved_for_tenant: tenant.map(|tenant_id| ResolvedForTenant {
            tenant_id: tenant_id.to_string(),
            plans_dir: resolve_plans_dir(
                configured.plans_dir.clone(),
                &configured.plans_dir_by_tenant,
                Some(tenant_id),
            ),
            plans_archive_dir: resolve_plans_archive_dir(
                configured.plans_archive_dir.clone(),
                &configured.plans_archive_dir_by_tenant,
                Some(tenant_id),
            ),
            prompts_dir: resolve_prompts_dir(
                configured.prompts_dir.clone(),
                &configured.prompts_dir_by_tenant,
                Some(tenant_id),
            ),
        }),
    };
    PathSettingsView {
        configured,
        resolved,
    }
}

/// The live view: one settings read, one metrics snapshot. `tenant` as on
/// [`view_from`].
pub fn view(tenant: Option<&str>) -> PathSettingsView {
    view_from(
        config_facade::get_setting::<PathSettings>(),
        &adapter_metrics().snapshot(),
        crate::paths::get_dev_logs_dir_string(),
        tenant,
    )
}

/// Apply `patch` to the persisted section (blank-normalised, maps merged) and
/// return the fresh **device** view.
///
/// The echoed view names no tenant on purpose: a save is a write to the whole
/// section, and answering it with one tenant's resolution would invite a caller
/// to read that as "what I just saved". A caller that wants a tenant's
/// resolution asks for it by name on the read door.
pub fn save(patch: PathSettingsPatch) -> Result<PathSettingsView, String> {
    config_facade::update_setting::<PathSettings, _>(move |paths| *paths = merge(paths, patch))?;
    Ok(view(None))
}

/// Return the `paths` section: configured values plus what each resolves to.
///
/// `tenant_id` is optional and is a **lookup key, never an attribution claim**:
/// it selects one tenant's entry out of the keyed maps so `resolved.resolved_for_tenant`
/// can report where that tenant's sessions actually author. Omit it for the
/// device-wide view, which is exactly what this door served before the settings
/// were keyed. A `tenant_id` the device is not bound to is answered honestly
/// (the device default, with the key echoed) rather than refused — the operator
/// may be re-pairing.
#[tauri::command]
pub fn get_path_settings(tenant_id: Option<String>) -> Result<PathSettingsView, String> {
    Ok(view(tenant_id.as_deref()))
}

/// Persist the `paths` section and echo the fresh device view.
///
/// Blank strings are stored as unset. The five scalars are **replaced** —
/// absent means unset. The four keyed maps (`plans_dir_by_tenant`,
/// `plans_archive_dir_by_tenant`, `prompts_dir_by_tenant`, `repo_checkouts`) are
/// **merged**: absent or `null` leaves the stored map untouched, and `{}` is a
/// deliberate clear. That is a server-side guarantee, so a caller unaware of a
/// map cannot erase it — which a whole-struct replace allowed until this patch
/// type existed.
#[tauri::command]
pub fn save_path_settings(settings: PathSettingsPatch) -> Result<PathSettingsView, String> {
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
            retired_permanent_total: 0,
            deps_forbidden_total: 0,
            seeded_total: 0,
            seed_errors_total: 0,
            work_unit_writes_withheld_total: 0,
            work_unit_writes_withheld_unknown_total: 0,
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
            // Blank sides are dropped here too, per entry — and the
            // unparseable-but-non-blank key is PRESERVED, which is the
            // distinction asserted below.
            plans_dir_by_tenant: [
                (
                    " c231d9da-0ca8-4fe4-bd81-0e3d6c20339a ".to_string(),
                    " /a/plans ".to_string(),
                ),
                ("blank-value".to_string(), "  ".to_string()),
                ("   ".to_string(), "/no-key".to_string()),
                ("not-a-uuid".to_string(), "/still/kept".to_string()),
            ]
            .into_iter()
            .collect(),
            plans_archive_dir_by_tenant: Default::default(),
            prompts_dir_by_tenant: Default::default(),
            repo_checkouts: [
                (" acme/app ".to_string(), " /src/acme/app ".to_string()),
                ("acme/blank-path".to_string(), "   ".to_string()),
                ("  ".to_string(), "/src/no-slug".to_string()),
            ]
            .into_iter()
            .collect(),
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
        assert_eq!(
            stored.repo_checkouts.into_iter().collect::<Vec<_>>(),
            vec![("acme/app".to_string(), "/src/acme/app".to_string())],
            "a mapping with a blank side is dropped; the survivor is trimmed"
        );
        assert_eq!(
            stored.plans_dir_by_tenant.into_iter().collect::<Vec<_>>(),
            vec![
                (
                    "c231d9da-0ca8-4fe4-bd81-0e3d6c20339a".to_string(),
                    "/a/plans".to_string()
                ),
                ("not-a-uuid".to_string(), "/still/kept".to_string()),
            ],
            "a blank key or value is dropped and the survivors trimmed — but an              UNPARSEABLE key is preserved (D2: the operator may be re-pairing)"
        );
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
            plans_dir_by_tenant: [(
                "c231d9da-0ca8-4fe4-bd81-0e3d6c20339a".to_string(),
                "/a/plans".to_string(),
            )]
            .into_iter()
            .collect(),
            plans_archive_dir_by_tenant: Default::default(),
            prompts_dir_by_tenant: Default::default(),
            repo_checkouts: [("acme/app".to_string(), "/src/acme/app".to_string())]
                .into_iter()
                .collect(),
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
            None,
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
            None,
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
            None,
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
            None,
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

    // ---- the save is a PATCH: an omitted map cannot be erased ---------------

    const TENANT_A: &str = "c231d9da-0ca8-4fe4-bd81-0e3d6c20339a";
    const TENANT_B: &str = "7ac125b6-391b-4d64-8493-27305b25c5b9";

    fn stored_with_both_maps() -> PathSettings {
        PathSettings {
            plans_dir: Some("/device/plans".to_string()),
            plans_dir_by_tenant: [(TENANT_A.to_string(), "/a/plans".to_string())]
                .into_iter()
                .collect(),
            repo_checkouts: [(
                "portofino-pizzeria/mobile".to_string(),
                "/elsewhere/mobile".to_string(),
            )]
            .into_iter()
            .collect(),
            ..PathSettings::default()
        }
    }

    /// **THE claim of P3, and the one that FAILED before the fix.** A
    /// `PUT /settings/paths` body composed by a caller that has never heard of
    /// either map — a pre-change body, replayed verbatim — must leave BOTH
    /// stored maps exactly as they were.
    ///
    /// Before the patch type existed, `save` was `*paths = normalize(settings)`
    /// and both maps carried `skip_serializing_if = "…is_empty"`, so this body
    /// deserialized to two empty maps and erased the operator's configuration.
    /// The preservation the doc promised lived in the React helper, which an
    /// agent or a script composing a body by hand does not run.
    #[test]
    fn a_pre_change_body_omitting_both_maps_erases_neither() {
        // The shape the panel sent before this change: the five scalars and
        // `strict_mode`, and no map field of any kind.
        let pre_change_body = serde_json::json!({
            "dev_logs_dir": null,
            "plans_dir": "/device/plans",
            "plans_archive_dir": null,
            "prompts_dir": null,
            "workspace_root": null,
            "strict_mode": false
        });
        let patch: PathSettingsPatch =
            serde_json::from_value(pre_change_body).expect("a pre-change body must deserialize");
        assert_eq!(patch.plans_dir_by_tenant, None, "absent, not empty");
        assert_eq!(patch.repo_checkouts, None, "absent, not empty");

        let merged = merge(&stored_with_both_maps(), patch);
        assert_eq!(
            merged.plans_dir_by_tenant.get(TENANT_A).map(String::as_str),
            Some("/a/plans"),
            "a caller unaware of plans_dir_by_tenant must not erase it"
        );
        assert_eq!(
            merged
                .repo_checkouts
                .get("portofino-pizzeria/mobile")
                .map(String::as_str),
            Some("/elsewhere/mobile"),
            "the same latent defect repo_checkouts carried is fixed, not routed around"
        );
        assert_eq!(merged.plans_dir.as_deref(), Some("/device/plans"));
    }


    /// **The round-2 finding, as a test.** The first cut of the patch made only
    /// the MAPS absent-means-keep and left the scalars on replace, justified by
    /// "the panel shows all five and sends all five". It shows FOUR. So a
    /// caller that omits `plans_archive_dir` — which is every caller that does
    /// not display it, the panel included — used to CLEAR it, and a peer's
    /// concurrent write to that field was reverted by the next save.
    ///
    /// The fix is that absent means untouched for every field there is, so this
    /// pins the scalar half of it. It fails against a replace-scalar patch.
    #[test]
    fn an_absent_scalar_leaves_the_stored_one_alone() {
        let stored = PathSettings {
            plans_dir: Some("/device/plans".to_string()),
            plans_archive_dir: Some("/device/archive".to_string()),
            prompts_dir: Some("/device/prompts".to_string()),
            workspace_root: Some("/device/root".to_string()),
            dev_logs_dir: Some("/device/logs".to_string()),
            strict_mode: true,
            ..PathSettings::default()
        };
        // A caller that knows about ONE field and says nothing about the rest.
        let patch: PathSettingsPatch =
            serde_json::from_value(serde_json::json!({ "plans_dir": "/somewhere/else" }))
                .expect("must deserialize");
        assert_eq!(patch.plans_archive_dir, None, "absent, not Some(None)");
        assert_eq!(patch.strict_mode, None, "absent, not Some(false)");

        let merged = merge(&stored, patch);
        assert_eq!(merged.plans_dir.as_deref(), Some("/somewhere/else"));
        assert_eq!(
            merged.plans_archive_dir.as_deref(),
            Some("/device/archive"),
            "the field the panel does not show must survive a save that omits it"
        );
        assert_eq!(merged.prompts_dir.as_deref(), Some("/device/prompts"));
        assert_eq!(merged.workspace_root.as_deref(), Some("/device/root"));
        assert_eq!(merged.dev_logs_dir.as_deref(), Some("/device/logs"));
        assert!(
            merged.strict_mode,
            "an omitted strict_mode must not turn strict mode OFF — its serde \
             default is false, which is the most dangerous shape this had"
        );
    }

    /// The other half of the same rule: `null` is how a caller CLEARS a scalar,
    /// and it has to stay distinguishable from absent or there is no way to
    /// unset anything. This is what `double_option` buys — a plain
    /// `Option<Option<T>>` folds both to `None` and the two cases collapse.
    #[test]
    fn an_explicit_null_scalar_clears_it_while_absent_does_not() {
        let stored = PathSettings {
            plans_dir: Some("/device/plans".to_string()),
            plans_archive_dir: Some("/device/archive".to_string()),
            strict_mode: true,
            ..PathSettings::default()
        };
        let patch: PathSettingsPatch = serde_json::from_value(serde_json::json!({
            "plans_dir": null,
            "strict_mode": false
        }))
        .expect("must deserialize");
        assert_eq!(
            patch.plans_dir,
            Some(None),
            "an explicit null is Some(None) — present, and asking for unset"
        );

        let merged = merge(&stored, patch);
        assert_eq!(merged.plans_dir, None, "null clears");
        assert!(!merged.strict_mode, "an explicit false sets");
        assert_eq!(
            merged.plans_archive_dir.as_deref(),
            Some("/device/archive"),
            "and the one nobody mentioned is still untouched"
        );
    }

    /// A blank string is still unset — `normalize` owns that, and it must keep
    /// owning it after the scalars became three-state. Blank is how a text box
    /// says "clear", and it must not become a directory named `""`.
    #[test]
    fn a_blank_scalar_is_still_unset_not_a_directory_named_empty() {
        let stored = PathSettings {
            plans_dir: Some("/device/plans".to_string()),
            ..PathSettings::default()
        };
        let patch: PathSettingsPatch =
            serde_json::from_value(serde_json::json!({ "plans_dir": "   " }))
                .expect("must deserialize");
        assert_eq!(merge(&stored, patch).plans_dir, None);
    }

    /// An explicit JSON `null` reads the same as absent — untouched. A client
    /// that spells "I am not changing this" as `null` must not be punished for
    /// it, so this pins the behaviour rather than leaving it to be discovered.
    #[test]
    fn an_explicit_null_map_leaves_the_stored_map_untouched() {
        let patch: PathSettingsPatch = serde_json::from_value(serde_json::json!({
            "plans_dir": "/device/plans",
            "plans_dir_by_tenant": null,
            "repo_checkouts": null
        }))
        .expect("must deserialize");
        let merged = merge(&stored_with_both_maps(), patch);
        assert_eq!(
            merged.plans_dir_by_tenant.get(TENANT_A).map(String::as_str),
            Some("/a/plans")
        );
        assert_eq!(merged.repo_checkouts.len(), 1);
    }

    /// An empty object is the DELIBERATE clear, and it must still work —
    /// otherwise absent-means-untouched would leave no way to empty a map at
    /// all, which is what the frontend helper's `delete` used to express.
    #[test]
    fn an_empty_object_is_a_deliberate_clear() {
        let patch: PathSettingsPatch = serde_json::from_value(serde_json::json!({
            "plans_dir": "/device/plans",
            "plans_dir_by_tenant": {},
            "repo_checkouts": {}
        }))
        .expect("must deserialize");
        assert_eq!(patch.plans_dir_by_tenant, Some(BTreeMap::new()));

        let merged = merge(&stored_with_both_maps(), patch);
        assert!(
            merged.plans_dir_by_tenant.is_empty(),
            "an empty object clears"
        );
        assert!(merged.repo_checkouts.is_empty(), "an empty object clears");
    }

    /// A patch that SENDS a map replaces it wholesale rather than deep-merging
    /// its entries — the panel edits a tenant list as a unit, so a removed row
    /// has to be expressible, and a per-key merge would make removal impossible
    /// for the same reason an omitted map used to make preservation impossible.
    /// Entries are still trim-and-dropped per entry on the way in.
    #[test]
    fn a_sent_map_replaces_the_stored_one_entry_for_entry() {
        let body = format!(
            r#"{{"plans_dir":"/device/plans",
                 "plans_dir_by_tenant":{{"{TENANT_B}":" /b/plans ","blank":"  "}}}}"#
        );
        let patch: PathSettingsPatch = serde_json::from_str(&body).expect("must deserialize");
        let merged = merge(&stored_with_both_maps(), patch);
        assert_eq!(
            merged.plans_dir_by_tenant.into_iter().collect::<Vec<_>>(),
            vec![(TENANT_B.to_string(), "/b/plans".to_string())],
            "the sent map replaces the stored one; the blank entry is dropped and \
             the survivor trimmed"
        );
    }

    // ---- the per-tenant view -----------------------------------------------

    /// The view answers a RESOLUTION for the named tenant while `configured`
    /// still shows the raw struct — so a UI reads "where does tenant A actually
    /// author" without re-implementing the two-rung precedence, and can still
    /// show the operator what is stored.
    #[test]
    fn the_per_tenant_view_resolves_for_that_tenant_while_configured_stays_raw() {
        let configured = PathSettings {
            plans_dir: Some("/device/plans".to_string()),
            plans_archive_dir: Some("/device/archive".to_string()),
            prompts_dir: Some("/device/prompts".to_string()),
            plans_dir_by_tenant: [(TENANT_A.to_string(), "/a/plans".to_string())]
                .into_iter()
                .collect(),
            prompts_dir_by_tenant: [(TENANT_A.to_string(), "/a/prompts".to_string())]
                .into_iter()
                .collect(),
            ..PathSettings::default()
        };

        let for_a = view_from(
            configured.clone(),
            &snapshot(1, 1),
            "/logs".to_string(),
            Some(TENANT_A),
        );
        let r = for_a
            .resolved
            .resolved_for_tenant
            .as_ref()
            .expect("a named tenant gets a resolution");
        assert_eq!(r.tenant_id, TENANT_A);
        assert_eq!(r.plans_dir.as_deref(), Some("/a/plans"));
        assert_eq!(r.prompts_dir.as_deref(), Some("/a/prompts"));
        assert_eq!(
            r.plans_archive_dir.as_deref(),
            Some("/device/archive"),
            "a directory this tenant did not key falls back to the device default"
        );
        // The device-wide half of the view is unaffected by the named tenant.
        assert_eq!(for_a.resolved.plans_dir.as_deref(), Some("/device/plans"));
        // And `configured` is the raw struct, map and all — not a resolution.
        assert_eq!(
            for_a
                .configured
                .plans_dir_by_tenant
                .get(TENANT_A)
                .map(String::as_str),
            Some("/a/plans")
        );

        // A tenant with no entries gets the device default for all three.
        let for_b = view_from(
            configured,
            &snapshot(1, 1),
            "/logs".to_string(),
            Some(TENANT_B),
        );
        let r = for_b.resolved.resolved_for_tenant.expect("resolution");
        assert_eq!(r.tenant_id, TENANT_B);
        assert_eq!(r.plans_dir.as_deref(), Some("/device/plans"));
        assert_eq!(r.prompts_dir.as_deref(), Some("/device/prompts"));
    }

    /// No tenant named ⇒ the DEVICE view, and `resolved_for_tenant` is ABSENT
    /// from the wire rather than null. A reader that cannot tell "you did not
    /// ask" from "this tenant has nothing" would render one as the other.
    #[test]
    fn no_tenant_named_serializes_the_device_view_with_no_tenant_field_at_all() {
        let view = view_from(
            PathSettings {
                plans_dir: Some("/device/plans".to_string()),
                plans_dir_by_tenant: [(TENANT_A.to_string(), "/a/plans".to_string())]
                    .into_iter()
                    .collect(),
                ..PathSettings::default()
            },
            &snapshot(1, 1),
            "/logs".to_string(),
            None,
        );
        assert_eq!(view.resolved.resolved_for_tenant, None);
        let wire = serde_json::to_value(&view.resolved).expect("must serialize");
        assert!(
            !wire
                .as_object()
                .unwrap()
                .contains_key("resolved_for_tenant"),
            "the field must be absent, not null: {wire}"
        );
        assert_eq!(
            view.resolved.plans_dir.as_deref(),
            Some("/device/plans"),
            "the device view ignores every map entry"
        );
    }

    /// A tenant the device is not bound to — or a key that is not a UUID at all
    /// — is answered HONESTLY rather than refused: the key is echoed and the
    /// device default resolved. Refusing would leave the panel unable to show an
    /// operator mid-re-pairing what their stored entry is.
    #[test]
    fn an_unbound_or_unparseable_tenant_key_resolves_the_device_default() {
        for probe in ["not-a-uuid", TENANT_B, ""] {
            let view = view_from(
                PathSettings {
                    plans_dir: Some("/device/plans".to_string()),
                    plans_dir_by_tenant: [(TENANT_A.to_string(), "/a/plans".to_string())]
                        .into_iter()
                        .collect(),
                    ..PathSettings::default()
                },
                &snapshot(1, 1),
                "/logs".to_string(),
                Some(probe),
            );
            let r = view.resolved.resolved_for_tenant.expect("resolution");
            assert_eq!(r.tenant_id, probe, "the key is echoed verbatim");
            assert_eq!(r.plans_dir.as_deref(), Some("/device/plans"));
        }
    }
}
