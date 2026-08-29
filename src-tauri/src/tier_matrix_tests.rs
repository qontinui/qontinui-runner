//! Phase 9 — E2E tier-matrix calibration suite for the runner
//! tier-decoupling rollout.
//!
//! See `plans/2026-05-20-runner-tier-decoupling.md` (Phase 9).
//!
//! ## What this is and what it is NOT
//!
//! This module is a **calibration test suite** in the sense of
//! [[feedback_build_verification_over_manual_observation]]: its only job is
//! to detect *regression of the tier-decoupling invariants* in CI. It does
//! NOT spawn a real runner, talk to a real backend, or exercise the full
//! WebSocket bridge end-to-end — that would re-invent the integration
//! plumbing the supervisor already owns and is way out of Phase 9 scope.
//!
//! Instead, this module exercises the **pure helpers** that gate each tier
//! boundary:
//!
//! - `settings::migrate_tier_in_place` (Phase 2 — settings migration; also the
//!   headless `QONTINUI_SERVER_MODE` tier default, and the device-pairing
//!   signal + unlatch)
//! - `qontinui_runner_lib::profiles::infer_tier` +
//!   `::tier_is_open_to_inference` (the ONE shared rule both tier readers now
//!   call — there were two hand-mirrored copies until they drifted)
//! - `settings::apply_tier_env_overlay` + `settings::apply_in_memory_tier_overlay`
//!   (the two overlays stacked over that inference, in `load_settings_full`'s
//!   order)
//! - `qontinui_runner_lib::profiles::read_runner_tier_at` (the SECOND, lib-side
//!   tier reader — the one `coord_doctor` consults; it sees no in-memory
//!   overlay and no process env, but it DOES see disk signals like pairing)
//! - `commands::auth::require_tier_2_for` (Phase 2 — auth-command gate)
//! - `mcp::backend_relay::should_relay_idle_with` (Phase 4 — relay gate;
//!   note: the live `should_relay_idle` wrapper reads `AuthManager` from
//!   disk, so calibration uses the pure inner instead — Phase 3 of the
//!   unified-devices migration plan)
//! - `api_config::PROD_API_BASE_URL` (Phase 6 — canonical prod URL)
//!
//! The relay loop and the live `load_settings` path are not invoked from
//! this file. Each predicate is fed a fixture `Settings`/`RunnerTier`
//! directly, so the tests are deterministic and need no env, no temp dir,
//! no network, and no module-local mutex (per
//! [[feedback_env_var_tests_serialize]]).
//!
//! ## Matrix
//!
//! | # | Test                                            | Asserts |
//! |---|--------------------------------------------------|---------|
//! | 1 | `tier_defaults_to_local_on_fresh_settings`       | `Settings::default()` is Tier 0 |
//! | 2 | `tier_inference_from_runner_token_promotes`      | non-empty `runner_token` → Tier 2 |
//! | 2b| `tier_inference_without_runner_token_stays_local`| empty `runner_token` → Tier 0 |
//! | 2c| `desktop_install_without_server_mode_stays_local` | fresh install, not headless → Tier 0 |
//! | 2d| `headless_server_mode_infers_qontinui_account`   | fresh install + `QONTINUI_SERVER_MODE` → Tier 2 |
//! | 2e| `runner_tier_env_overlay_beats_the_headless_default` | `QONTINUI_RUNNER_TIER=local` wins over 2d |
//! | 2f| `runtime_tier_override_beats_the_headless_default`   | `set_runner_tier` wins over 2d *and* 2e |
//! | 2g| `an_explicit_tier_choice_short_circuits_the_headless_default` | `tier_chosen_explicitly` closes the inference for good |
//! | 2h| `server_mode_default_is_invisible_to_the_disk_reader`  | process signals do not cross to the disk reader |
//! | 2i| `paired_box_infers_tier_2_in_both_readers`       | paired + empty `runner_token` → Tier 2 in BOTH readers |
//! | 2j| `inferred_local_is_re_inferred_but_an_explicit_one_is_not` | the unlatch, and the distinction it turns on |
//! | 2k| `re_inference_never_demotes`                     | unpairing a Tier 2 box does not demote it |
//! | 2l| `settings_without_tier_chosen_explicitly_reads_false` | the upgrade path: absent key ⇒ eligible |
//! | 2m| `sign_out_state_is_not_demoted_by_the_re_inference`  | signed-out-but-Tier-2 stays Tier 2 |
//! | 2n| `unpaired_tokenless_desktop_box_still_resolves_local` | the desktop regression guard |
//! | 3 | `require_tier_2_blocks_local_and_local_provider` | Tier 0/1 → `AuthError` |
//! | 3b| `require_tier_2_permits_qontinui_account`        | Tier 2 → `Ok` |
//! | 4 | `relay_idles_when_tier_local`                    | gate predicate idles in Tier 0/1 |
//! | 4b| `relay_idles_when_device_jwt_missing`            | gate predicate idles in Tier 2 w/o JWT |
//! | 4c| `relay_idles_when_disabled`                      | gate predicate idles when `enabled=false` |
//! | 4d| `relay_runs_when_tier_qontinui_and_jwt_present`  | gate predicate runs in Tier 2 + JWT |
//! | 4e| `relay_idles_when_device_jwt_empty`              | Phase 3: idle when JWT absent |
//! | 4f| `relay_idles_when_device_jwt_expired_or_absent`  | Phase 3: idle when slot empty |
//! | 4g| `relay_runs_when_device_jwt_present`             | Phase 3: run when JWT present |
//! | 5 | `tier_promotion_local_to_qontinui_sets_user_id`  | Local → Tier 2 transition shape |
//! | 5b| `tier_downgrade_qontinui_to_local_clears_state`  | Tier 2 → Local transition shape |
//! | 6 | `prod_api_base_url_is_canonical`                 | constant is `https://api.qontinui.io` |
//!
//! (Items 1, 2, 2b, 6 overlap with tests already living next to the helpers
//! they exercise. They are duplicated here intentionally so the matrix is a
//! standalone calibration surface — adding/removing a tier in the future
//! lands one diff in one file, and a single CI failure points at the whole
//! tier-decoupling invariant rather than a scattered set of asserts.)

use crate::commands::auth::require_tier_2_for;
use crate::mcp::backend_relay::should_relay_idle_with;
use crate::settings::{RunnerTier, Settings, WebIntegrationSettings};

// ----------------------------------------------------------------------------
// Fixture helpers
// ----------------------------------------------------------------------------

/// Fresh settings + tier override + optional runner_token. All other fields
/// stay at `Default::default()` (i.e. `web_integration.enabled = true`,
/// `runner_token = ""`).
fn settings_with(tier: RunnerTier, runner_token: &str) -> Settings {
    Settings {
        tier,
        web_integration: WebIntegrationSettings {
            runner_token: runner_token.to_string(),
            ..WebIntegrationSettings::default()
        },
        ..Settings::default()
    }
}

// ----------------------------------------------------------------------------
// #1 — Default tier
// ----------------------------------------------------------------------------

#[test]
fn tier_defaults_to_local_on_fresh_settings() {
    let s = Settings::default();
    assert_eq!(s.tier, RunnerTier::Local);
    assert!(s.qontinui_user_id.is_none());
    assert!(
        s.local_user_id.is_empty(),
        "default Settings must leave local_user_id empty; \
         load_settings fills it lazily, not Default"
    );
    assert!(
        !s.tier_initialized,
        "default Settings must NOT be tier_initialized so the first \
         load_settings pass runs migrate_tier_in_place"
    );
}

// ----------------------------------------------------------------------------
// #2 — Tier inference from runner_token (the upgrade-from-pre-tier shape)
// ----------------------------------------------------------------------------

#[test]
fn tier_inference_from_runner_token_promotes() {
    // Deserialize a settings.json fragment that has only a runner_token —
    // i.e. the shape on disk after upgrading from a pre-tier release.
    let json = r#"{"web_integration":{"runner_token":"qontinui_runner_test123"}}"#;
    let s: Settings = serde_json::from_str(json).expect("must deserialize");

    // Before migration: tier is the serde default (Local), sentinel is false.
    assert_eq!(s.tier, RunnerTier::Local);
    assert!(!s.tier_initialized);

    // Run the migration in-memory (no disk I/O).
    let mut s = s;
    let migrated = crate::settings::migrate_tier_in_place(
        &mut s, /* server_mode = */ false, /* paired = */ false,
    );
    assert!(migrated, "migration must report it ran");
    assert_eq!(s.tier, RunnerTier::QontinuiAccount);
    assert!(s.tier_initialized);
}

#[test]
fn tier_inference_without_runner_token_stays_local() {
    let json = r#"{"web_integration":{"runner_token":""}}"#;
    let mut s: Settings = serde_json::from_str(json).expect("must deserialize");
    s.tier_initialized = false;

    let migrated = crate::settings::migrate_tier_in_place(
        &mut s, /* server_mode = */ false, /* paired = */ false,
    );
    assert!(migrated);
    assert_eq!(s.tier, RunnerTier::Local);
    assert!(s.tier_initialized);
}

// ----------------------------------------------------------------------------
// #2c–#2h — The headless (`QONTINUI_SERVER_MODE`) tier default and its
//           precedence, per plan
//           `2026-08-29-headless-runner-tier-never-reaches-qontinui-account`
//           Phase 2.
//
// A headless runner exists to be driven over the network, and Tier 2 is the
// only tier allowed to talk to coord — so a headless box with no tier of its
// own defaults there instead of to `Local`. Tier 0 advertises "no cloud
// round-trips", so this is a deliberate product-posture default, not a bug
// fix; the tests below pin that it stays a DEFAULT and never becomes a trap.
//
// `server_mode` is threaded into `migrate_tier_in_place` as a parameter
// (parsed once, by `launch_env::server_mode_from_env`, and read by
// `load_settings_full`) precisely so this file can drive every combination
// with no process env, matching the module doc above.
// ----------------------------------------------------------------------------

/// The regression guard on every existing desktop install: not headless, no
/// `runner_token` ⇒ Tier 0, exactly as before this phase.
#[test]
fn desktop_install_without_server_mode_stays_local() {
    let mut s = Settings::default();
    assert!(!s.tier_initialized, "fixture must be a fresh install");

    let migrated = crate::settings::migrate_tier_in_place(
        &mut s, /* server_mode = */ false, /* paired = */ false,
    );
    assert!(migrated, "a fresh install must still be migrated");
    assert_eq!(
        s.tier,
        RunnerTier::Local,
        "a windowed install with no runner_token must keep landing in Tier 0 — \
         the headless default must not leak into desktop installs"
    );
    assert!(s.tier_initialized);
}

/// Fresh install + headless ⇒ Tier 2. This is the phase.
#[test]
fn headless_server_mode_infers_qontinui_account() {
    let mut s = Settings::default();
    assert!(s.web_integration.runner_token.is_empty(), "no legacy token");

    let migrated = crate::settings::migrate_tier_in_place(
        &mut s, /* server_mode = */ true, /* paired = */ false,
    );
    assert!(migrated, "migration must report it ran");
    assert_eq!(
        s.tier,
        RunnerTier::QontinuiAccount,
        "QONTINUI_SERVER_MODE with no explicit tier must default to the tier \
         that talks to coord"
    );
    assert!(s.tier_initialized);
}

/// Escape hatch 1: the spawn-time `QONTINUI_RUNNER_TIER` env overlay is applied
/// AFTER the inference in `load_settings_full`, so it wins. A headless deploy
/// that genuinely wants Tier 0 can still say so.
#[test]
fn runner_tier_env_overlay_beats_the_headless_default() {
    let mut s = Settings::default();
    crate::settings::migrate_tier_in_place(
        &mut s, /* server_mode = */ true, /* paired = */ false,
    );
    assert_eq!(s.tier, RunnerTier::QontinuiAccount, "default applied first");

    // `load_settings_full` step 2 — the parsed `QONTINUI_RUNNER_TIER` value.
    crate::settings::apply_tier_env_overlay(&mut s, "local");
    assert_eq!(
        s.tier,
        RunnerTier::Local,
        "QONTINUI_RUNNER_TIER=local must override the headless default — \
         the default may be opinionated, it may not be a trap"
    );
}

/// Escape hatch 2, and the top of the stack: the runtime override that
/// `commands::auth::set_runner_tier` writes (`settings::set_in_memory_tier` →
/// `TIER_OVERRIDE` → `in_memory_tier()`) is applied LAST, so an explicit
/// operator choice made after boot beats BOTH the env overlay and the headless
/// default.
///
/// Driven through the same three helpers `load_settings_full` calls, in its
/// order, with the override value supplied directly — `TIER_OVERRIDE` is a
/// process-wide global and this suite mutates no globals (see the module doc).
#[test]
fn runtime_tier_override_beats_the_headless_default() {
    let mut s = Settings::default();

    // 1. inference (headless default)
    crate::settings::migrate_tier_in_place(
        &mut s, /* server_mode = */ true, /* paired = */ false,
    );
    assert_eq!(s.tier, RunnerTier::QontinuiAccount);

    // 2. spawn-time env overlay — deliberately set to Tier 2 as well, so the
    //    only thing that can produce `Local` below is the runtime override.
    crate::settings::apply_tier_env_overlay(&mut s, "qontinui_account");
    assert_eq!(s.tier, RunnerTier::QontinuiAccount);

    // 3. runtime override — what `set_runner_tier("local")` leaves behind.
    crate::settings::apply_in_memory_tier_overlay(&mut s, Some(RunnerTier::Local));
    assert_eq!(
        s.tier,
        RunnerTier::Local,
        "a runtime set_runner_tier choice must beat both the env overlay and \
         the headless default"
    );
    assert!(s.tier_initialized);

    // And `None` (no runtime choice was ever made) must change nothing.
    let mut s2 = Settings::default();
    crate::settings::migrate_tier_in_place(
        &mut s2, /* server_mode = */ true, /* paired = */ false,
    );
    crate::settings::apply_in_memory_tier_overlay(&mut s2, None);
    assert_eq!(s2.tier, RunnerTier::QontinuiAccount);
}

/// The sentinel that closes the inference is `tier_chosen_explicitly` — the
/// operator's own choice, recorded by `commands::auth::set_runner_tier` and by
/// nothing else. Once it is set, NOTHING is re-inferred: not `runner_token`,
/// not server mode, not pairing.
///
/// **This test asserted `tier_initialized` until Phase 3.** That sentinel was a
/// one-shot latch, and the whole point of the phase is that it latched the
/// wrong thing: a box that first booted before it was paired was stuck at
/// `Local` forever, with the only exit a button in a WebView a headless box
/// does not have. `tier_initialized` could not stand in for a choice because
/// the *inference itself* writes it — so "never chosen" and "chose Local" were
/// the same document. See `inferred_local_is_re_inferred_but_an_explicit_one_is_not`
/// for the pair of cases that are now distinguished.
#[test]
fn an_explicit_tier_choice_short_circuits_the_headless_default() {
    let mut s = settings_with(RunnerTier::Local, "");
    s.tier_initialized = true;
    s.tier_chosen_explicitly = true;

    let migrated = crate::settings::migrate_tier_in_place(
        &mut s, /* server_mode = */ true, /* paired = */ true,
    );
    assert!(
        !migrated,
        "an explicitly-chosen tier must never be re-inferred — headless, paired, or both"
    );
    assert_eq!(
        s.tier,
        RunnerTier::Local,
        "server mode and pairing must not re-promote a box whose owner chose Tier 0"
    );

    // A Tier 1 install is closed even WITHOUT the field, because nothing but
    // `set_runner_tier` can produce `local_provider` — no inference has an arm
    // for it. That is a deduction from the writer set, not a guess at intent.
    let mut s = settings_with(RunnerTier::LocalProvider, "");
    s.tier_initialized = true;
    assert!(!s.tier_chosen_explicitly, "the pre-Phase-3 upgrade shape");
    assert!(!crate::settings::migrate_tier_in_place(
        &mut s, /* server_mode = */ true, /* paired = */ true
    ));
    assert_eq!(s.tier, RunnerTier::LocalProvider);

    // And Tier 2 is closed for the trivial reason: there is nothing above it.
    let mut s = settings_with(RunnerTier::QontinuiAccount, "");
    s.tier_initialized = true;
    assert!(!crate::settings::migrate_tier_in_place(
        &mut s, /* server_mode = */ false, /* paired = */ false
    ));
    assert_eq!(s.tier, RunnerTier::QontinuiAccount);
}

/// **The negative, and it is still deliberate — do NOT "fix" this test.**
///
/// `server_mode` is a property of the READING PROCESS's environment, not of the
/// settings document: `coord_doctor` runs in a process whose env says nothing
/// about how the runner was launched. So `profiles::read_runner_tier_at` —
/// the second tier reader, the one the doctor consults — always passes
/// `server_mode: false`, and the headless default stays invisible to it until
/// a load persists a tier.
///
/// **Phase 3 changed what this test is about, so read the new boundary.** Both
/// readers now share ONE inference (`profiles::infer_tier`), so the disk reader
/// DOES see pairing — pairing is a fact on disk, and
/// `paired_box_infers_tier_2_in_both_readers` pins that it is seen. The line
/// this test defends is narrower and sharper than "the disk reader is behind":
/// it is **disk signals cross; process signals do not**. Phase 4 owns the
/// doctor's reporting; it does not own this boundary, which is structural.
///
/// (One of the two temp files in this suite. The module doc's "no temp dir"
/// rule is about keeping *predicates* pure; the whole point here is that the
/// other reader is a disk read, and there is no way to assert that without one.)
#[test]
fn server_mode_default_is_invisible_to_the_disk_reader() {
    use qontinui_runner_lib::profiles::{read_runner_tier_at, TierRead};

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("settings.json");
    // A tier-less document with no legacy `runner_token` — a fresh install.
    std::fs::write(&path, r#"{"web_integration":{"runner_token":""}}"#).expect("write");

    // In memory, the headless default resolves this install to Tier 2 …
    let mut s: Settings = serde_json::from_str(&std::fs::read_to_string(&path).expect("read"))
        .expect("must deserialize");
    crate::settings::migrate_tier_in_place(
        &mut s, /* server_mode = */ true, /* paired = */ false,
    );
    assert_eq!(s.tier, RunnerTier::QontinuiAccount);

    // … and the on-disk document is unchanged, so the lib-side reader — the
    // one `coord_doctor` uses, and which cannot observe THIS process's
    // `QONTINUI_SERVER_MODE` — still reports Absent. NOT `Known("local")`
    // and NOT `Unknown`: the file parsed fine, it simply has no tier and
    // nothing on disk infers one.
    assert_eq!(
        read_runner_tier_at(&path, /* paired = */ false),
        TierRead::Absent,
        "the in-memory headless default must be invisible to the raw \
         settings.json reader — server mode is a property of the reading \
         process, not of the document"
    );
}

// ----------------------------------------------------------------------------
// #2i–#2n — Device pairing as a tier signal, and the unlatch, per plan
//           `2026-08-29-headless-runner-tier-never-reaches-qontinui-account`
//           Phase 3.
//
// Before this phase the inference consulted only `web_integration.runner_token`
// — a field `server_mode/mod.rs` records as LEGACY and "no longer consulted by
// the WS relay (it authenticates with the device JWT from `AuthManager`)". So
// the runner inferred its tier from a token nothing authenticates with any
// more, while the account bind the system actually runs on went unread.
// Pairing IS that bind: a paired device is bound to a Qontinui account, which
// is precisely what Tier 2 means.
//
// The signal is the `paired_user.json` binding entry, NOT the device JWT —
// see `pair::device_is_paired` for why (keychain cost, unreadable-vs-unpaired
// conflation, and a ~4h credential lifetime that would make a PRODUCT TIER
// flap).
// ----------------------------------------------------------------------------

/// **The test that proves the duplicate is gone.**
///
/// A paired box with an EMPTY `runner_token` infers Tier 2 — asserted against
/// BOTH readers, because they are different code paths for different consumers:
/// `settings::migrate_tier_in_place` is what `require_tier_2()` sees through
/// `load_settings`, and `profiles::read_runner_tier_at` is what `coord_doctor`
/// consults. They used to carry two hand-mirrored copies of the rule; they now
/// call one shared `profiles::infer_tier`, so teaching one and not the other is
/// no longer expressible.
#[test]
fn paired_box_infers_tier_2_in_both_readers() {
    use qontinui_runner_lib::profiles::{read_runner_tier_at, TierRead, QONTINUI_ACCOUNT_TIER};

    // Reader 1 — the runner bin's `Settings` path.
    let mut s = Settings::default();
    assert!(s.web_integration.runner_token.is_empty(), "no legacy token");
    let migrated = crate::settings::migrate_tier_in_place(
        &mut s, /* server_mode = */ false, /* paired = */ true,
    );
    assert!(migrated, "migration must report it ran");
    assert_eq!(
        s.tier,
        RunnerTier::QontinuiAccount,
        "a paired device is bound to a Qontinui account — that is what Tier 2 means"
    );

    // Reader 2 — the lib-side raw `settings.json` parse `coord_doctor` uses.
    // The document is the LATCHED box the headless defect actually produces:
    // `tier: "local"`, sentinel set, no explicit choice ever recorded.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("settings.json");
    std::fs::write(
        &path,
        r#"{"tier":"local","tier_initialized":true,"web_integration":{"runner_token":""}}"#,
    )
    .expect("write");

    assert_eq!(
        read_runner_tier_at(&path, /* paired = */ false),
        TierRead::Known("local".to_string()),
        "unpaired, this document reads exactly as it is written"
    );
    assert_eq!(
        read_runner_tier_at(&path, /* paired = */ true),
        TierRead::Known(QONTINUI_ACCOUNT_TIER.to_string()),
        "paired, the SAME document must read as Tier 2 in the doctor's reader \
         too — otherwise teaching only the settings-side inference leaves \
         `coord doctor` red on a correctly-paired box, which is the exact \
         symptom this plan was written from"
    );
}

/// The unlatch, and the distinction it turns on: an install latched at `Local`
/// by the INFERENCE is re-inferred when the signals change; one whose operator
/// chose `Local` is not. Same tier value, same sentinel — only
/// `tier_chosen_explicitly` separates them, which is why the field had to exist.
#[test]
fn inferred_local_is_re_inferred_but_an_explicit_one_is_not() {
    // (a) Booted unpaired: the inference latched it at Tier 0.
    let mut inferred = Settings::default();
    assert!(crate::settings::migrate_tier_in_place(
        &mut inferred,
        /* server_mode = */ false,
        /* paired = */ false
    ));
    assert_eq!(inferred.tier, RunnerTier::Local);
    assert!(inferred.tier_initialized, "the sentinel is set");
    assert!(
        !inferred.tier_chosen_explicitly,
        "an inference must never claim the operator chose"
    );

    // (b) The operator's explicit Tier 0, as `set_runner_tier` writes it.
    let mut chosen = Settings {
        tier: RunnerTier::Local,
        tier_initialized: true,
        tier_chosen_explicitly: true,
        ..Settings::default()
    };

    // The box is then paired. Only (a) moves.
    assert!(
        crate::settings::migrate_tier_in_place(
            &mut inferred,
            /* server_mode = */ false,
            /* paired = */ true
        ),
        "the one-shot latch is gone: a box that booted before it was paired \
         must not be stuck at Tier 0 forever"
    );
    assert_eq!(inferred.tier, RunnerTier::QontinuiAccount);

    assert!(
        !crate::settings::migrate_tier_in_place(
            &mut chosen,
            /* server_mode = */ false,
            /* paired = */ true
        ),
        "an explicit opt-out must survive pairing"
    );
    assert_eq!(chosen.tier, RunnerTier::Local);
}

/// The re-inference can only ever PROMOTE. Unpairing a box that reached Tier 2
/// does not demote it — silent demotion of a working primary is the top risk in
/// this area and a known historical failure mode, so it is ruled out by the
/// shape of the rule (`tier_is_open_to_inference` closes on any tier but
/// `local`), not by a guard that could be forgotten.
#[test]
fn re_inference_never_demotes() {
    let mut s = Settings::default();
    crate::settings::migrate_tier_in_place(
        &mut s, /* server_mode = */ false, /* paired = */ true,
    );
    assert_eq!(s.tier, RunnerTier::QontinuiAccount);

    // Every signal now gone — and the tier stands.
    assert!(
        !crate::settings::migrate_tier_in_place(
            &mut s, /* server_mode = */ false, /* paired = */ false
        ),
        "an unpaired Tier 2 box must not be re-inferred at all"
    );
    assert_eq!(s.tier, RunnerTier::QontinuiAccount);
}

/// **The upgrade path.** A settings document written before this phase carries
/// no `tier_chosen_explicitly` key at all. It must deserialize as `false`
/// ("never chosen") and therefore be ELIGIBLE for re-inference — that is what
/// rescues every already-latched box in the field, including the headless one
/// this plan was written from.
///
/// Asserted in both readers, because both must agree on what an absent key
/// means.
#[test]
fn settings_without_tier_chosen_explicitly_reads_false() {
    use qontinui_runner_lib::profiles::{
        read_runner_tier_at, tier_is_open_to_inference, TierRead, QONTINUI_ACCOUNT_TIER,
    };

    // Reader 1 — serde default on the typed struct.
    let json = r#"{"tier":"local","tier_initialized":true}"#;
    let mut s: Settings = serde_json::from_str(json).expect("must deserialize");
    assert!(
        !s.tier_chosen_explicitly,
        "an absent key must read as 'never chosen', not 'chosen'"
    );
    assert!(crate::settings::migrate_tier_in_place(
        &mut s, /* server_mode = */ false, /* paired = */ true
    ));
    assert_eq!(s.tier, RunnerTier::QontinuiAccount);

    // Reader 2 — the raw JSON parse, where the key is simply missing.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("settings.json");
    std::fs::write(&path, json).expect("write");
    assert_eq!(
        read_runner_tier_at(&path, /* paired = */ true),
        TierRead::Known(QONTINUI_ACCOUNT_TIER.to_string())
    );

    // And the shared predicate itself, stated directly.
    assert!(tier_is_open_to_inference(
        Some("local"),
        /* chosen_explicitly = */ false
    ));
    assert!(!tier_is_open_to_inference(
        Some("local"),
        /* chosen_explicitly = */ true
    ));
    assert!(tier_is_open_to_inference(
        None, /* chosen_explicitly = */ false
    ));
    assert!(!tier_is_open_to_inference(
        Some("local_provider"),
        /* chosen_explicitly = */ false
    ));
    assert!(!tier_is_open_to_inference(
        Some(QONTINUI_ACCOUNT_TIER),
        /* chosen_explicitly = */ false
    ));
}

/// **Sign-out must not be undone by the re-inference.** `qontinui_sign_out`
/// deliberately KEEPS `tier = QontinuiAccount` (`commands/auth.rs`: "so the App
/// gate renders LoginScreen for this Tier-2-unauthenticated state instead of
/// falling through to the synthesized local-guest app shell") while clearing
/// `runner_token` and `qontinui_user_id`.
///
/// That is a Tier 2 install with NO `runner_token`, which is exactly the shape
/// the inference would have resolved to `Local` — so a re-inference that could
/// demote would silently undo sign-out's deliberate choice on the next settings
/// load, on every box, paired or not. It cannot: Tier 2 is closed to inference.
#[test]
fn sign_out_state_is_not_demoted_by_the_re_inference() {
    // The exact post-sign-out document.
    let s = Settings {
        tier: RunnerTier::QontinuiAccount,
        tier_initialized: true,
        qontinui_user_id: None,
        ..Settings::default()
    };
    assert!(
        s.web_integration.runner_token.is_empty(),
        "sign-out clears the runner_token"
    );
    assert!(
        !s.tier_chosen_explicitly,
        "sign-out is not an explicit tier choice and must not claim to be one"
    );

    for paired in [true, false] {
        let mut s = s.clone();
        assert!(
            !crate::settings::migrate_tier_in_place(&mut s, /* server_mode = */ false, paired),
            "signed out, paired={paired}: nothing to re-infer"
        );
        assert_eq!(
            s.tier,
            RunnerTier::QontinuiAccount,
            "a signed-out-but-still-Tier-2 box must stay Tier 2 so the App gate \
             renders LoginScreen — paired={paired}"
        );
    }
}

/// The regression guard on every desktop install: unpaired, tokenless, not
/// headless ⇒ Tier 0, exactly as before any of this. Stated separately from
/// `desktop_install_without_server_mode_stays_local` because that one predates
/// pairing being a signal at all, and this is the combination that matters now.
#[test]
fn unpaired_tokenless_desktop_box_still_resolves_local() {
    use qontinui_runner_lib::profiles::{read_runner_tier_at, TierRead};

    let mut s = Settings::default();
    assert!(crate::settings::migrate_tier_in_place(
        &mut s, /* server_mode = */ false, /* paired = */ false
    ));
    assert_eq!(
        s.tier,
        RunnerTier::Local,
        "pairing must not leak Tier 2 into ordinary desktop installs"
    );

    // And the disk reader agrees — a tier-less, tokenless, unpaired document
    // is genuinely tier-less.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("settings.json");
    std::fs::write(&path, r#"{"web_integration":{"runner_token":""}}"#).expect("write");
    assert_eq!(
        read_runner_tier_at(&path, /* paired = */ false),
        TierRead::Absent
    );
}

// ----------------------------------------------------------------------------
// #3 — require_tier_2 gate (covers check_auth_status, login, refresh, ...)
// ----------------------------------------------------------------------------

#[test]
fn require_tier_2_blocks_local_and_local_provider() {
    let err = require_tier_2_for(RunnerTier::Local)
        .expect_err("Tier 0 (Local) must be blocked from cloud auth commands");
    let msg = err.to_string();
    assert!(
        msg.contains("Tier 0/1"),
        "blocked message must name the tier — got: {msg}"
    );

    let err = require_tier_2_for(RunnerTier::LocalProvider)
        .expect_err("Tier 1 (LocalProvider) must be blocked from cloud auth commands");
    let msg = err.to_string();
    assert!(
        msg.contains("Tier 0/1"),
        "blocked message must name the tier — got: {msg}"
    );
}

#[test]
fn require_tier_2_permits_qontinui_account() {
    require_tier_2_for(RunnerTier::QontinuiAccount)
        .expect("Tier 2 (QontinuiAccount) must be allowed through the auth gate");
}

// ----------------------------------------------------------------------------
// #4 — backend_relay::should_relay_idle_with (Phase 4 gate, Phase 3
//      unified-devices update)
//
// The pure inner predicate consults `(tier, enabled, has_device_jwt)`. The
// live wrapper reads the JWT slot from `AuthManager` on disk — these tests
// drive the inner directly to stay deterministic and free of disk I/O.
// ----------------------------------------------------------------------------

#[test]
fn relay_idles_when_tier_local() {
    // Tier 0 with a (hypothetical) JWT still idles — tier wins over JWT.
    assert!(
        should_relay_idle_with(RunnerTier::Local, true, true),
        "Tier 0 must idle the relay regardless of leftover device-JWT"
    );

    assert!(
        should_relay_idle_with(RunnerTier::LocalProvider, true, true),
        "Tier 1 must idle the relay regardless of leftover device-JWT"
    );
}

#[test]
fn relay_idles_when_device_jwt_missing() {
    // Tier 2 + enabled but no JWT — must idle (otherwise we 401-spam).
    assert!(
        should_relay_idle_with(RunnerTier::QontinuiAccount, true, false),
        "Tier 2 with no device-JWT must idle the relay"
    );
}

#[test]
fn relay_idles_when_disabled() {
    assert!(
        should_relay_idle_with(RunnerTier::QontinuiAccount, false, true),
        "web_integration.enabled=false must idle the relay even in Tier 2 \
         with a fresh device-JWT"
    );
}

#[test]
fn relay_runs_when_tier_qontinui_and_jwt_present() {
    assert!(
        !should_relay_idle_with(RunnerTier::QontinuiAccount, true, true),
        "Tier 2 + enabled + device-JWT present must let the relay run"
    );
}

// ----------------------------------------------------------------------------
// #4e/f/g — Phase 3 unified-devices: device-JWT slot replaces runner_token
//           as the relay-side credential gate.
// ----------------------------------------------------------------------------

#[test]
fn relay_idles_when_device_jwt_empty() {
    // Tier 2 + enabled=true + has_jwt=false → idle. Direct exercise of the
    // new Phase 3 conjunct (formerly: runner_token.is_empty()).
    assert!(
        should_relay_idle_with(RunnerTier::QontinuiAccount, true, false),
        "Tier 2 + enabled + has_device_jwt=false must idle the relay"
    );
}

#[test]
fn relay_idles_when_device_jwt_expired_or_absent() {
    // The relay's gate only cares whether the access_token slot is populated;
    // expiry logic lives in `AuthManager::device_jwt_needs_refresh`, which is
    // the refresher's concern. So "expired" + "absent" both surface here as
    // has_jwt=false → idle.
    assert!(
        should_relay_idle_with(RunnerTier::QontinuiAccount, true, false),
        "Tier 2 + enabled + (expired or absent JWT, i.e. has_jwt=false) \
         must idle the relay"
    );
}

#[test]
fn relay_runs_when_device_jwt_present() {
    // Tier 2 + enabled=true + has_jwt=true → not idle.
    assert!(
        !should_relay_idle_with(RunnerTier::QontinuiAccount, true, true),
        "Tier 2 + enabled + has_device_jwt=true must let the relay run"
    );
}

// ----------------------------------------------------------------------------
// #5 — Tier promotion / downgrade transitions (mirrors qontinui_sign_out
//      and the device-pairing success path)
// ----------------------------------------------------------------------------

#[test]
fn tier_promotion_local_to_qontinui_sets_user_id() {
    // Starting state: fresh Tier 0 install, no Qontinui user id.
    let mut s = settings_with(RunnerTier::Local, "");
    s.qontinui_user_id = None;
    s.tier_initialized = true; // user has been through the wizard

    // Apply the promotion path (mirrors device-pairing on token receipt):
    s.web_integration.runner_token = "qontinui_runner_freshly_paired".to_string();
    s.qontinui_user_id = Some("u123".to_string());
    s.tier = RunnerTier::QontinuiAccount;

    assert_eq!(s.tier, RunnerTier::QontinuiAccount);
    assert_eq!(s.qontinui_user_id.as_deref(), Some("u123"));
    assert_eq!(
        s.web_integration.runner_token, "qontinui_runner_freshly_paired",
        "promotion must persist the runner_token"
    );
    assert!(
        s.tier_initialized,
        "tier_initialized must stay sticky across a deliberate tier change"
    );
    // Post-promotion: refresher has populated the device-JWT slot, so the
    // pure inner sees has_jwt=true and runs.
    assert!(
        !should_relay_idle_with(s.tier, s.web_integration.enabled, true),
        "post-promotion settings (tier=Tier2 + enabled + JWT present) \
         must be relay-eligible"
    );
}

#[test]
fn sign_out_stays_tier2_unauthenticated_clears_token() {
    // Starting state: signed-in Tier 2 install.
    let mut s = settings_with(RunnerTier::QontinuiAccount, "qontinui_runner_signed_in");
    s.qontinui_user_id = Some("u456".to_string());
    s.local_user_id = "local-uuid-keeps".to_string();
    s.tier_initialized = true;

    // Apply the sign-out path (mirrors commands::auth::qontinui_sign_out):
    // clear the token + user id but KEEP Tier 2 so the App gate
    // `isTier2 && !authenticated → LoginScreen` renders the login screen
    // rather than silently dropping to the local-guest app shell. Local
    // guest is a separate, deliberate SetupWizard tier choice — not a
    // side effect of signing out of an account.
    s.web_integration.runner_token = String::new();
    s.qontinui_user_id = None;
    s.tier = RunnerTier::QontinuiAccount;

    assert_eq!(
        s.tier,
        RunnerTier::QontinuiAccount,
        "sign-out must stay Tier 2 (unauthenticated) so the LoginScreen shows"
    );
    assert!(s.qontinui_user_id.is_none());
    assert!(s.web_integration.runner_token.is_empty());
    assert_eq!(
        s.local_user_id, "local-uuid-keeps",
        "local_user_id must survive sign-out — local DB rows are keyed on it \
         per Phase 1 of the tier-decoupling plan"
    );
    // Post-sign-out the device-JWT slot is cleared, so the relay idles
    // because there is no JWT to authenticate the WS — even though tier is
    // still Tier 2.
    assert!(
        should_relay_idle_with(s.tier, s.web_integration.enabled, false),
        "post-sign-out (tier=Tier2 but cleared JWT) must idle the relay"
    );
    // The tier gate alone now passes (still Tier 2); actual cloud auth
    // fails downstream on the empty token / missing JWT, which surfaces as
    // `check_auth_status` → authenticated:false → LoginScreen.
    assert!(
        require_tier_2_for(s.tier).is_ok(),
        "sign-out keeps Tier 2, so the tier gate itself stays open; \
         authentication fails on the cleared token, not the tier"
    );
}

// ----------------------------------------------------------------------------
// #6 — Canonical production URL (Phase 6 unification)
// ----------------------------------------------------------------------------

#[test]
fn prod_api_base_url_is_canonical() {
    assert_eq!(
        crate::api_config::PROD_API_BASE_URL,
        "https://api.qontinui.io",
        "PROD_API_BASE_URL is the single source of truth for the prod \
         backend FQDN — both api_config::get_api_base_url AND \
         settings::default_web_integration_backend_url derive from it"
    );
}
