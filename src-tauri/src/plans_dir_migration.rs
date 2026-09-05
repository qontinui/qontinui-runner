//! One-time migration of the retired `QONTINUI_PLAN_ADAPTER_DIR` env shim into
//! the `paths.plans_dir` setting.
//!
//! Plan `2026-09-05-plans-dir-is-env-only-and-unreachable-in-the-product`,
//! Phase 1. The env var used to be read FIRST by the plan adapter's resolver
//! and silently outranked the setting; the setting was never written on any
//! fleet primary (the live provenance was 100% the env var, measured
//! 2026-09-03). Phase 2 deletes the env read, and `plans_dir` defaults to
//! **unset = the markdown-plan tier is OFF with no fallback** — so deleting the
//! read alone would have turned plan scanning off, silently, on every machine
//! that relied on the shim. This migration is what makes that deletion safe:
//! it records the env value into the setting once, after which the setting is
//! the only source and the env var is inert.
//!
//! It is a copy of [`crate::workspace_paths::persist_resolved_workspace_root`]
//! in the three properties that are load-bearing there too:
//!
//! - **Primary only** — a secondary instance returns early. The migration is
//!   first-writer-wins, and a secondary must not freeze the value.
//! - **Through [`update_setting`]**, which refuses to write over a
//!   non-authoritative load: a corrupt `settings.json` yields `Err` here,
//!   never a clobber. Failures are non-fatal and logged by the caller.
//! - **Ordered before the adapter spawn** — called from `main.rs` beside the
//!   workspace-root migration, above the background thread that spawns the
//!   reconcile loop, so the loop's first tick already reads the persisted
//!   value. (The loop re-reads the setting every tick anyway, so the cost of
//!   getting this wrong is one interval, not one boot — but the ordering is
//!   free, so keep it.)
//!
//! **This module is the only place in the runner that names the env var.**
//! `git grep QONTINUI_PLAN_ADAPTER_DIR src-tauri/src` must list this file and
//! nothing else: no resolver, no CLI rung, no doc, no log line.

use crate::config_facade::{get_setting, update_setting};
use crate::settings::PathSettings;
use tracing::info;

/// The retired per-machine override. Read here, once, to seed the setting —
/// and nowhere else.
pub const PLAN_ADAPTER_DIR_ENV: &str = "QONTINUI_PLAN_ADAPTER_DIR";

/// Persist `$QONTINUI_PLAN_ADAPTER_DIR` into `paths.plans_dir` when — and
/// only when — the setting is unset and the env var is set to a non-blank
/// value. Idempotent: a second boot finds the setting present and writes
/// nothing; an operator's own value is never overwritten.
///
/// Failures are non-fatal: a runner that cannot write its settings still
/// boots, and the tier is simply off until the operator sets the field.
pub fn persist_env_plans_dir() -> Result<(), String> {
    if crate::instance::is_secondary() {
        return Ok(());
    }

    let existing = get_setting::<PathSettings>().plans_dir;
    let env_value = std::env::var(PLAN_ADAPTER_DIR_ENV).ok();
    let Some(value) = migration_write(existing.as_deref(), env_value.as_deref()) else {
        return Ok(());
    };

    update_setting::<PathSettings, _>(|paths| paths.plans_dir = Some(value.clone()))?;
    info!(
        env_var = PLAN_ADAPTER_DIR_ENV,
        "plans_dir_migration: recorded paths.plans_dir = {value:?} from the retired env \
         override. The setting is now the only source; the env var is no longer read."
    );
    Ok(())
}

/// The migration's decision rule, pure so it is asserted directly.
///
/// Returns the value to write, or `None` when nothing should be written: the
/// setting already holds a non-blank value (the operator's choice wins), or
/// the env var is absent or blank (blank is unset, never a directory named
/// `""`).
fn migration_write(existing: Option<&str>, env_value: Option<&str>) -> Option<String> {
    if existing.is_some_and(|s| !s.trim().is_empty()) {
        return None;
    }
    env_value
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env::{env_lock, EnvVarRestore};
    use qontinui_runner_lib::plan_workunit_adapter::resolve_plans_dir;

    /// Serialized against every other env-touching test in this binary, and
    /// restoring the var on the way out — the operator's machines have it
    /// `setx`-persisted, so a leaked removal would change what sibling tests
    /// observe.
    fn with_env<T>(value: Option<&str>, f: impl FnOnce() -> T) -> T {
        let _guard = env_lock();
        let _restore = EnvVarRestore::capture(&[PLAN_ADAPTER_DIR_ENV]);
        match value {
            Some(v) => std::env::set_var(PLAN_ADAPTER_DIR_ENV, v),
            None => std::env::remove_var(PLAN_ADAPTER_DIR_ENV),
        }
        f()
    }

    /// Env set + setting absent ⇒ written. The upgrade path, and the whole
    /// reason Phase 2's deletion is safe.
    #[test]
    fn env_set_and_setting_absent_writes_the_env_value() {
        assert_eq!(
            migration_write(None, Some("D:/qontinui-root/plans")),
            Some("D:/qontinui-root/plans".to_string())
        );
        // A blank setting is unset, so it is replaceable rather than an
        // operator decision.
        assert_eq!(
            migration_write(Some("   "), Some("/plans")),
            Some("/plans".to_string())
        );
    }

    /// Env set + setting present ⇒ untouched. The operator's configuration
    /// always wins; this is what makes the migration safe to run every boot.
    #[test]
    fn env_set_and_setting_present_leaves_the_setting_alone() {
        assert_eq!(
            migration_write(Some("/operator/choice"), Some("/env/plans")),
            None
        );
    }

    /// Env absent ⇒ no write. Nothing to bridge, and the migration must never
    /// invent a path.
    #[test]
    fn env_absent_writes_nothing() {
        assert_eq!(migration_write(None, None), None);
        assert_eq!(migration_write(Some(""), None), None);
    }

    /// Blank env ⇒ treated as unset, never a directory named `""` — and a
    /// surrounding-whitespace value is trimmed rather than stored verbatim.
    #[test]
    fn blank_env_is_unset_and_whitespace_is_trimmed() {
        assert_eq!(migration_write(None, Some("")), None);
        assert_eq!(migration_write(None, Some("   ")), None);
        assert_eq!(
            migration_write(None, Some("  /plans \n")),
            Some("/plans".to_string())
        );
    }

    /// The env reader is the process environment, read through the same
    /// predicate — so the decision rule above IS what the boot-time call
    /// applies.
    #[test]
    fn the_env_var_is_read_from_the_process_environment() {
        let read = |v: Option<&str>| {
            with_env(v, || {
                let env_value = std::env::var(PLAN_ADAPTER_DIR_ENV).ok();
                migration_write(None, env_value.as_deref())
            })
        };
        assert_eq!(read(Some("/from/env")), Some("/from/env".to_string()));
        assert_eq!(read(Some("  ")), None);
        assert_eq!(read(None), None);
    }

    /// Phase 2's contract, asserted from the one file allowed to spell the
    /// variable: a SET env var has NO effect on the resolved plans dir. The
    /// resolver returns the setting regardless of the environment, and an
    /// unset setting stays unset however the env is exported.
    #[test]
    fn a_set_env_var_has_no_effect_on_the_resolved_plans_dir() {
        with_env(Some("/env/plans"), || {
            assert_eq!(
                resolve_plans_dir(Some("/settings/plans".to_string())).as_deref(),
                Some("/settings/plans"),
                "the setting is the only source"
            );
            assert_eq!(
                resolve_plans_dir(None),
                None,
                "an exported env var must not arm a tier the setting leaves off"
            );
        });
    }
}
