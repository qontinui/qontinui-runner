//! Boot-time seeding of the `paths.plans_dir` setting from an
//! operator-exported plans-directory env var.
//!
//! Plan `2026-09-05-plans-dir-is-env-only-and-unreachable-in-the-product`,
//! Phase 1. The env var used to be read FIRST by the plan adapter's resolver
//! and silently outranked the setting; the setting was never written on any
//! fleet primary (the live provenance was 100% the env var, measured
//! 2026-09-03). Phase 2 deletes the env read, and `plans_dir` defaults to
//! **unset = the markdown-plan tier is OFF with no fallback** — so deleting the
//! read alone would have turned plan scanning off, silently, on every machine
//! that relied on the shim. This migration is what makes that deletion safe:
//! it records the env value into the setting, after which the setting is the
//! only source the resolver reads.
//!
//! **It is a seed, not a one-shot.** There is no persisted "migrated" marker:
//! it re-seeds on ANY primary boot where `paths.plans_dir` is blank and a
//! source below is exported. So turning the tier OFF durably means removing
//! the export as well as clearing the setting — clearing the setting alone
//! re-arms the tier on the next primary boot.
//!
//! ## Two sources, in precedence order
//!
//! Plan `2026-09-21-plans-dir-migration-seeds-from-a-retired-variable-nothing-sets`.
//! The migration originally read only the retired shim's name,
//! `$QONTINUI_PLAN_ADAPTER_DIR` — which nothing on the fleet set. The runners
//! carried `$QONTINUI_PLANS_DIR` instead, so the migration found nothing,
//! wrote nothing, and the tier stayed off fleet-wide. It now reads:
//!
//! 1. `$QONTINUI_PLAN_ADAPTER_DIR` — the retired shim, unconditionally. The
//!    runner never injects it, so its value can only be operator-exported.
//! 2. `$QONTINUI_PLANS_DIR` — the variable the fleet actually carries, **only
//!    when `$QONTINUI_RUNNER_CONTEXT` is empty or absent.** The runner itself
//!    exports `QONTINUI_PLANS_DIR` into every session it spawns
//!    ([`crate::agent_worktree::session_env`]), resolved PER TENANT; a primary
//!    launched from inside a runner-spawned terminal would inherit ONE
//!    tenant's directory and freeze it as the DEVICE default for every tenant.
//!    `QONTINUI_RUNNER_CONTEXT` is set on every one of those spawns (the PTY
//!    seam in `terminal::session` and the headless seam in `agent_runtime`),
//!    so its presence marks exactly the circular, runner-injected value and
//!    leaves an operator-exported one (systemd unit, `setx`, shell profile)
//!    usable.
//!
//! An existing non-blank setting outranks both — the operator's choice always
//! wins — and a blank env value is unset. The decision is one pure function,
//! [`migration_decision`], and every primary boot logs its outcome in one
//! `info` line ([`outcome_message`]) — except when the seeding write itself
//! fails, where the caller's `warn` is the one line instead — so "ran and
//! found nothing to seed" is distinguishable from "never ran".
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
//! **This module is the only RESOLVER-side reader of either variable.** No
//! resolver, CLI rung of the runner process, or log line elsewhere in the
//! runner reads `QONTINUI_PLAN_ADAPTER_DIR` or `QONTINUI_PLANS_DIR` to decide
//! a plans directory; the setting is the only source. Other occurrences are
//! not readers of this kind: `ambient.rs` names both in its env-key inventory
//! (`AMBIENT_ENV_KEYS`, the list a test fixture captures and restores), and
//! `agent_worktree::session_env` is the PRODUCER that exports
//! `QONTINUI_PLANS_DIR` into spawned sessions. (The separate `qontinui-pr`
//! CLI binary reads `QONTINUI_PLANS_DIR` as a flag fallback for its own
//! commands; it never touches the runner's setting.)

use crate::agent_worktree::session_env::PLANS_DIR_ENV;
use crate::config_facade::{get_setting, update_setting};
use crate::settings::PathSettings;
use tracing::info;

/// The retired per-machine override. Read here, once per primary boot, to
/// seed a blank setting — and nowhere else.
pub const PLAN_ADAPTER_DIR_ENV: &str = "QONTINUI_PLAN_ADAPTER_DIR";

/// The fleet's "am I inside the runner?" identity marker, injected into every
/// runner-spawned session (`terminal::session` PTY seam, `agent_runtime`
/// headless seam). Non-empty ⇒ this process's `QONTINUI_PLANS_DIR` was
/// injected by a runner and is tenant-scoped, so the migration ignores it.
pub const RUNNER_CONTEXT_ENV: &str = "QONTINUI_RUNNER_CONTEXT";

/// Which env var a seeded value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SeedSource {
    /// `$QONTINUI_PLAN_ADAPTER_DIR` — the retired shim.
    AdapterEnv,
    /// `$QONTINUI_PLANS_DIR` — read only outside runner context.
    PlansEnv,
}

impl SeedSource {
    fn env_var(self) -> &'static str {
        match self {
            SeedSource::AdapterEnv => PLAN_ADAPTER_DIR_ENV,
            SeedSource::PlansEnv => PLANS_DIR_ENV,
        }
    }
}

/// Every outcome of one boot's migration decision — all of them logged.
#[derive(Debug, Clone, PartialEq, Eq)]
enum MigrationOutcome {
    /// Write `value` into `paths.plans_dir`; it came from `source`.
    Seed { value: String, source: SeedSource },
    /// The setting already holds a non-blank value; nothing is written.
    KeptExisting,
    /// No usable source. `plans_env_ignored_in_runner_context` is true when
    /// `$QONTINUI_PLANS_DIR` held a non-blank value that the runner-context
    /// guard discarded — the one case an operator would otherwise misread.
    NothingToSeed {
        plans_env_ignored_in_runner_context: bool,
    },
}

/// Persist a plans directory from the environment into `paths.plans_dir` when
/// — and only when — the setting is unset and a source resolves (see
/// [`migration_decision`] for the precedence). Idempotent: a second boot finds
/// the setting present and writes nothing; an operator's own value is never
/// overwritten. A setting cleared later is re-seeded on the next primary boot
/// while the source stays exported. Every primary boot emits exactly one
/// `info` line naming the outcome, unless the seeding write fails — then the
/// `Err` returns first and the caller's `warn` is the one line.
///
/// Failures are non-fatal: a runner that cannot write its settings still
/// boots, and the tier is simply off until the operator sets the field.
pub fn persist_env_plans_dir() -> Result<(), String> {
    if crate::instance::is_secondary() {
        return Ok(());
    }

    let existing = get_setting::<PathSettings>().plans_dir;
    let (adapter_env, plans_env, in_runner_context) = read_env_inputs();

    let outcome = migration_decision(
        existing.as_deref(),
        adapter_env.as_deref(),
        plans_env.as_deref(),
        in_runner_context,
    );
    if let MigrationOutcome::Seed { value, .. } = &outcome {
        update_setting::<PathSettings, _>(|paths| paths.plans_dir = Some(value.clone()))?;
    }
    info!("{}", outcome_message(&outcome));
    Ok(())
}

/// The three process-env inputs of [`migration_decision`], read once each:
/// `$QONTINUI_PLAN_ADAPTER_DIR`, `$QONTINUI_PLANS_DIR`, and whether
/// `$QONTINUI_RUNNER_CONTEXT` is non-blank (i.e. this process is inside a
/// runner-spawned session).
fn read_env_inputs() -> (Option<String>, Option<String>, bool) {
    let adapter_env = std::env::var(PLAN_ADAPTER_DIR_ENV).ok();
    let plans_env = std::env::var(PLANS_DIR_ENV).ok();
    let in_runner_context = std::env::var(RUNNER_CONTEXT_ENV)
        .ok()
        .is_some_and(|v| !v.trim().is_empty());
    (adapter_env, plans_env, in_runner_context)
}

/// Trim, and treat blank as unset — never a directory named `""`.
fn nonblank(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The migration's decision rule, pure so it is asserted directly.
///
/// Precedence: an existing non-blank setting wins (write nothing) →
/// `$QONTINUI_PLAN_ADAPTER_DIR` → `$QONTINUI_PLANS_DIR`, the last ONLY when
/// `in_runner_context` is false (see the module docs for why).
fn migration_decision(
    existing: Option<&str>,
    adapter_env: Option<&str>,
    plans_env: Option<&str>,
    in_runner_context: bool,
) -> MigrationOutcome {
    if nonblank(existing).is_some() {
        return MigrationOutcome::KeptExisting;
    }
    if let Some(value) = nonblank(adapter_env) {
        return MigrationOutcome::Seed {
            value,
            source: SeedSource::AdapterEnv,
        };
    }
    match nonblank(plans_env) {
        Some(value) if !in_runner_context => MigrationOutcome::Seed {
            value,
            source: SeedSource::PlansEnv,
        },
        Some(_) => MigrationOutcome::NothingToSeed {
            plans_env_ignored_in_runner_context: true,
        },
        None => MigrationOutcome::NothingToSeed {
            plans_env_ignored_in_runner_context: false,
        },
    }
}

/// The one boot-time `info` line for an outcome. Pure, so the mapping is
/// asserted directly.
fn outcome_message(outcome: &MigrationOutcome) -> String {
    match outcome {
        MigrationOutcome::Seed { value, source } => format!(
            "plans_dir_migration: seeded from {var}: recorded paths.plans_dir = {value:?}. \
             The resolver reads only the setting; the export stays a seed, so clearing \
             the setting re-seeds it on the next primary boot unless {var} is also removed.",
            var = source.env_var(),
        ),
        MigrationOutcome::KeptExisting => {
            "plans_dir_migration: kept existing setting: paths.plans_dir is already set, \
             so the env vars were not used."
                .to_string()
        }
        MigrationOutcome::NothingToSeed {
            plans_env_ignored_in_runner_context,
        } => {
            let ignored = if *plans_env_ignored_in_runner_context {
                format!(
                    "; {PLANS_DIR_ENV} ignored inside runner context \
                     ({RUNNER_CONTEXT_ENV} is set, so its value may be runner-injected)"
                )
            } else {
                String::new()
            };
            format!(
                "plans_dir_migration: nothing to seed (checked: {PLAN_ADAPTER_DIR_ENV}, \
                 {PLANS_DIR_ENV}{ignored}). paths.plans_dir stays unset, so the \
                 markdown-plan tier is OFF until it is configured."
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env::{env_lock, EnvVarRestore};
    use qontinui_runner_lib::plan_workunit_adapter::resolve_plans_dir;

    /// Serialized against every other env-touching test in this binary, and
    /// restoring all three vars on the way out — the operator's machines have
    /// them `setx`/systemd-exported, and a test run from a runner terminal
    /// inherits `QONTINUI_RUNNER_CONTEXT`, so a leaked change would alter what
    /// sibling tests observe.
    fn with_env<T>(
        adapter: Option<&str>,
        plans: Option<&str>,
        runner_context: Option<&str>,
        f: impl FnOnce() -> T,
    ) -> T {
        let _guard = env_lock();
        let _restore =
            EnvVarRestore::capture(&[PLAN_ADAPTER_DIR_ENV, PLANS_DIR_ENV, RUNNER_CONTEXT_ENV]);
        for (key, value) in [
            (PLAN_ADAPTER_DIR_ENV, adapter),
            (PLANS_DIR_ENV, plans),
            (RUNNER_CONTEXT_ENV, runner_context),
        ] {
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
        f()
    }

    fn seed(value: &str, source: SeedSource) -> MigrationOutcome {
        MigrationOutcome::Seed {
            value: value.to_string(),
            source,
        }
    }

    const NOTHING: MigrationOutcome = MigrationOutcome::NothingToSeed {
        plans_env_ignored_in_runner_context: false,
    };

    /// Adapter env set + setting absent ⇒ written. The upgrade path, and the
    /// whole reason Phase 2's deletion is safe.
    #[test]
    fn adapter_env_set_and_setting_absent_writes_the_adapter_value() {
        assert_eq!(
            migration_decision(None, Some("D:/qontinui-root/plans"), None, false),
            seed("D:/qontinui-root/plans", SeedSource::AdapterEnv)
        );
        // A blank setting is unset, so it is replaceable rather than an
        // operator decision.
        assert_eq!(
            migration_decision(Some("   "), Some("/plans"), None, false),
            seed("/plans", SeedSource::AdapterEnv)
        );
    }

    /// Plans env only ⇒ the plans value. The variable the fleet actually
    /// carries — the defect this plan fixes was this case writing nothing.
    #[test]
    fn plans_env_only_writes_the_plans_value() {
        assert_eq!(
            migration_decision(None, None, Some("/fleet/plans"), false),
            seed("/fleet/plans", SeedSource::PlansEnv)
        );
    }

    /// A blank setting is unset, so the plans rung seeds it too — the
    /// settings-reset case (a cleared field on a box that still exports the
    /// fleet variable).
    #[test]
    fn blank_setting_with_plans_env_outside_context_seeds_from_plans_env() {
        assert_eq!(
            migration_decision(Some("  "), None, Some("/fleet/plans"), false),
            seed("/fleet/plans", SeedSource::PlansEnv)
        );
    }

    /// Both set ⇒ the retired adapter var wins (precedence order).
    #[test]
    fn both_envs_set_prefers_the_adapter_value() {
        assert_eq!(
            migration_decision(None, Some("/adapter"), Some("/plans"), false),
            seed("/adapter", SeedSource::AdapterEnv)
        );
    }

    /// Setting present ⇒ untouched, whichever env is set. The operator's
    /// configuration always wins; this is what makes the migration safe to
    /// run every boot.
    #[test]
    fn existing_setting_wins_over_either_env() {
        for (adapter, plans) in [
            (Some("/env/adapter"), None),
            (None, Some("/env/plans")),
            (Some("/env/adapter"), Some("/env/plans")),
            (None, None),
        ] {
            assert_eq!(
                migration_decision(Some("/operator/choice"), adapter, plans, false),
                MigrationOutcome::KeptExisting
            );
        }
    }

    /// Both absent ⇒ no write. Nothing to bridge, and the migration must never
    /// invent a path.
    #[test]
    fn both_envs_absent_writes_nothing() {
        assert_eq!(migration_decision(None, None, None, false), NOTHING);
        assert_eq!(migration_decision(Some(""), None, None, false), NOTHING);
    }

    /// The circularity guard: inside runner context `QONTINUI_PLANS_DIR` is a
    /// runner-injected, tenant-scoped value and must not become the device
    /// default — and the outcome says it was ignored rather than absent.
    #[test]
    fn plans_env_inside_runner_context_writes_nothing() {
        assert_eq!(
            migration_decision(None, None, Some("/tenant-a/plans"), true),
            MigrationOutcome::NothingToSeed {
                plans_env_ignored_in_runner_context: true
            }
        );
        // A blank plans env inside runner context is simply absent.
        assert_eq!(migration_decision(None, None, Some("  "), true), NOTHING);
    }

    /// The runner never injects the retired name, so the guard does not
    /// apply to it.
    #[test]
    fn adapter_env_inside_runner_context_still_writes() {
        assert_eq!(
            migration_decision(None, Some("/adapter"), None, true),
            seed("/adapter", SeedSource::AdapterEnv)
        );
        assert_eq!(
            migration_decision(None, Some("/adapter"), Some("/tenant-a/plans"), true),
            seed("/adapter", SeedSource::AdapterEnv)
        );
    }

    /// Blank env ⇒ treated as unset, never a directory named `""` — and a
    /// surrounding-whitespace value is trimmed rather than stored verbatim.
    /// A blank adapter value falls through to the plans rung.
    #[test]
    fn blank_env_is_unset_and_whitespace_is_trimmed() {
        assert_eq!(migration_decision(None, Some(""), None, false), NOTHING);
        assert_eq!(
            migration_decision(None, Some("   "), Some(""), false),
            NOTHING
        );
        assert_eq!(
            migration_decision(None, Some("  /plans \n"), None, false),
            seed("/plans", SeedSource::AdapterEnv)
        );
        assert_eq!(
            migration_decision(None, Some("  "), Some(" /fleet/plans\n"), false),
            seed("/fleet/plans", SeedSource::PlansEnv)
        );
    }

    /// The env readers are the process environment, read through the SAME
    /// [`read_env_inputs`] the boot-time call uses — so the decision rule
    /// above IS what the boot applies, including the runner-context guard.
    #[test]
    fn the_env_vars_are_read_from_the_process_environment() {
        let read = |adapter: Option<&str>, plans: Option<&str>, ctx: Option<&str>| {
            with_env(adapter, plans, ctx, || {
                let (adapter_env, plans_env, in_ctx) = read_env_inputs();
                migration_decision(None, adapter_env.as_deref(), plans_env.as_deref(), in_ctx)
            })
        };
        assert_eq!(
            read(Some("/from/env"), None, None),
            seed("/from/env", SeedSource::AdapterEnv)
        );
        assert_eq!(
            read(None, Some("/fleet/plans"), None),
            seed("/fleet/plans", SeedSource::PlansEnv)
        );
        assert_eq!(
            read(None, Some("/fleet/plans"), Some("")),
            seed("/fleet/plans", SeedSource::PlansEnv),
            "an empty runner-context marker is outside the runner"
        );
        assert_eq!(
            read(None, Some("/tenant-a/plans"), Some("BRIEFING")),
            MigrationOutcome::NothingToSeed {
                plans_env_ignored_in_runner_context: true
            }
        );
        assert_eq!(read(Some("  "), None, None), NOTHING);
        assert_eq!(read(None, None, None), NOTHING);
    }

    /// Phase 2: every outcome maps to one line naming it — the source var on a
    /// seed, both checked vars on nothing-to-seed, and the guard when it
    /// discarded a value.
    #[test]
    fn each_outcome_logs_a_line_naming_it() {
        let adapter = outcome_message(&seed("/a", SeedSource::AdapterEnv));
        assert!(
            adapter.contains("seeded from QONTINUI_PLAN_ADAPTER_DIR"),
            "{adapter}"
        );
        assert!(adapter.contains("\"/a\""), "{adapter}");

        let plans = outcome_message(&seed("/p", SeedSource::PlansEnv));
        assert!(plans.contains("seeded from QONTINUI_PLANS_DIR"), "{plans}");

        let kept = outcome_message(&MigrationOutcome::KeptExisting);
        assert!(kept.contains("kept existing setting"), "{kept}");
        // All three vars ARE read every boot; the line may only say they were
        // not USED, never that they were not read/consulted.
        for false_claim in ["not read", "not consulted", "no env var was"] {
            assert!(!kept.contains(false_claim), "{kept}");
        }

        let nothing = outcome_message(&NOTHING);
        assert!(
            nothing.contains(
                "nothing to seed (checked: QONTINUI_PLAN_ADAPTER_DIR, QONTINUI_PLANS_DIR)"
            ),
            "{nothing}"
        );
        assert!(!nothing.contains("runner context"), "{nothing}");

        let ignored = outcome_message(&MigrationOutcome::NothingToSeed {
            plans_env_ignored_in_runner_context: true,
        });
        assert!(ignored.contains("nothing to seed"), "{ignored}");
        assert!(
            ignored.contains("QONTINUI_PLANS_DIR ignored inside runner context"),
            "{ignored}"
        );
    }

    /// Phase 2 of the owning plan's contract, asserted from the module that
    /// reads the variables: a SET env var has NO effect on the resolved plans
    /// dir. The resolver returns the setting regardless of the environment,
    /// and an unset setting stays unset however the env is exported — only
    /// the one-shot migration reads it.
    ///
    /// The empty map and `None` tenant are the device-default rung — this
    /// migration is device-wide and deliberately keys nothing by tenant: it
    /// seeds the scalar that every tenant without its own entry falls back to.
    #[test]
    fn a_set_env_var_has_no_effect_on_the_resolved_plans_dir() {
        let no_tenant_overrides = std::collections::BTreeMap::new();
        with_env(Some("/env/plans"), Some("/env/fleet-plans"), None, || {
            assert_eq!(
                resolve_plans_dir(
                    Some("/settings/plans".to_string()),
                    &no_tenant_overrides,
                    None
                )
                .as_deref(),
                Some("/settings/plans"),
                "the setting is the only source"
            );
            assert_eq!(
                resolve_plans_dir(None, &no_tenant_overrides, None),
                None,
                "an exported env var must not arm a tier the setting leaves off"
            );
        });
    }
}
