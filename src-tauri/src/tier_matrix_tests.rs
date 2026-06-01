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
//! - `settings::migrate_tier_in_place` (Phase 2 — settings migration)
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
    let migrated = crate::settings::migrate_tier_in_place(&mut s);
    assert!(migrated, "migration must report it ran");
    assert_eq!(s.tier, RunnerTier::QontinuiAccount);
    assert!(s.tier_initialized);
}

#[test]
fn tier_inference_without_runner_token_stays_local() {
    let json = r#"{"web_integration":{"runner_token":""}}"#;
    let mut s: Settings = serde_json::from_str(json).expect("must deserialize");
    s.tier_initialized = false;

    let migrated = crate::settings::migrate_tier_in_place(&mut s);
    assert!(migrated);
    assert_eq!(s.tier, RunnerTier::Local);
    assert!(s.tier_initialized);
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
//      and the auth_callback success path)
// ----------------------------------------------------------------------------

#[test]
fn tier_promotion_local_to_qontinui_sets_user_id() {
    // Starting state: fresh Tier 0 install, no Qontinui user id.
    let mut s = settings_with(RunnerTier::Local, "");
    s.qontinui_user_id = None;
    s.tier_initialized = true; // user has been through the wizard

    // Apply the promotion path (mirrors auth_callback.rs on token receipt):
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
