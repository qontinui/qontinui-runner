//! Tests of [`spawn_account_ready`] and [`spawn_account_refusal`].
//!
//! Every test here holds the fleet pin and an isolated ambient for its whole body,
//! plus [`ResolvedConfigDirRestore`], because all three name process-global state
//! the predicate reads. On Windows `dirs` answers the REAL profile whatever HOME
//! and USERPROFILE say, which leaves two more doors to the box's own accounts, and
//! [`Arranged`] closes both: an instance root moves the machine-global roster into
//! the fixture, and an existing `$CLAUDE_CONFIG_DIR` credentials file is the default
//! location's first hit, so a dead one there reads as no login at all.
//!
//! macOS is out: its default-location check also reads the real login Keychain,
//! which nothing here can isolate.

use super::account_usage::pick_best_account;
use super::config::{
    get_effective_config_dir, get_resolved_config_dir, record_account_usage,
    set_resolved_config_dir, spawn_account_ready, spawn_account_refusal, ResolvedConfigDirRestore,
};
use super::oauth_refresh::default_location_has_valid_credentials;
use crate::claude_accounts::ClaudeAccountsFile;
use crate::settings::AccountSelectionMode;
use tempfile::TempDir;

const MODES: [AccountSelectionMode; 5] = [
    AccountSelectionMode::Manual,
    AccountSelectionMode::LeastUsage,
    AccountSelectionMode::HighestExpectedUsage,
    AccountSelectionMode::WeightedRandom,
    AccountSelectionMode::AlternatingHandoff,
];

const NO_ACCOUNT_DETAIL: &str =
    "no authenticated Claude account on this runner — run /login (instance=primary)";

/// A config dir whose credentials `has_valid_credentials` accepts (`live`) or
/// rejects. The live token is a day from expiry with no refresh token, and the dead
/// one is past expiry with no refresh token: neither queues a background refresh,
/// so the check stays a pure file read.
fn creds_dir(live: bool) -> (TempDir, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let expires_at_ms = if live {
        chrono::Utc::now().timestamp_millis() + 86_400_000
    } else {
        1_000
    };
    let body = serde_json::json!({
        "claudeAiOauth": {
            "accessToken": "sk-oauth-test",
            "refreshToken": "",
            "expiresAt": expires_at_ms,
            "scopes": ["user:inference"],
        }
    });
    std::fs::write(dir.path().join(".credentials.json"), body.to_string()).expect("write");
    let path = dir.path().to_string_lossy().to_string();
    (dir, path)
}

fn point_default_location_at(dir: &str) {
    std::env::set_var("CLAUDE_CONFIG_DIR", dir);
}

/// The fixture every test here shares. Fields drop in declaration order, so the
/// published pick is restored before any temp dir goes, and the env lock is
/// released last.
struct Arranged {
    _restore: ResolvedConfigDirRestore,
    dead_default: (TempDir, String),
    live_default: (TempDir, String),
    _guards: (
        crate::mcp::fleet_policy_poller::AccountSelectionPin,
        qontinui_runner_lib::ambient::test_support::IsolatedAmbient,
    ),
}

fn arrange() -> Arranged {
    let guards = crate::mcp::fleet_policy_poller::isolated_ambient_with_fleet_pin();
    std::env::set_var("QONTINUI_INSTANCE_ROOT", guards.1.dir().join("subject"));
    let dead_default = creds_dir(false);
    let live_default = creds_dir(true);
    point_default_location_at(&dead_default.1);
    Arranged {
        _restore: ResolvedConfigDirRestore::capture(),
        dead_default,
        live_default,
        _guards: guards,
    }
}

fn write_roster(mode: AccountSelectionMode, dirs: &[&str], config_dir: Option<&str>) {
    crate::claude_accounts::save(&ClaudeAccountsFile {
        claude_config_dirs: dirs.iter().map(|d| d.to_string()).collect(),
        account_selection_mode: mode,
        account_selection_pinned: true,
        config_dir: config_dir.map(str::to_string),
        ..Default::default()
    })
    .expect("write the fixture roster");
}

/// A resolver-only reading: what the spawn would decide if nothing picked first.
fn reads_ready_with_no_pick() -> bool {
    let ai = crate::settings::get_ai_settings();
    let (selected, _source) = get_effective_config_dir(&ai.claude_cli);
    selected.is_some() || default_location_has_valid_credentials()
}

/// The seams' own order, spelled as they spell it: pick, resolve, then refuse on
/// no dir and no default login.
fn the_spawn_would_proceed() -> bool {
    pick_best_account();
    reads_ready_with_no_pick()
}

/// One input's state in the parity table: no value, a dir with dead credentials,
/// or a dir with live ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    Absent,
    Invalid,
    Valid,
}

const SLOTS: [Slot; 3] = [Slot::Absent, Slot::Invalid, Slot::Valid];

/// A dead dir and a live dir for one input, kept distinct per input so a reading
/// from the wrong slot could not pass by coincidence.
struct Dirs {
    dead: (TempDir, String),
    live: (TempDir, String),
}

impl Dirs {
    fn new() -> Self {
        Self {
            dead: creds_dir(false),
            live: creds_dir(true),
        }
    }

    fn at(&self, slot: Slot) -> Option<&str> {
        match slot {
            Slot::Absent => None,
            Slot::Invalid => Some(&self.dead.1),
            Slot::Valid => Some(&self.live.1),
        }
    }
}

/// The table's answer, derived by hand rather than from the code under test.
fn expected_ready(
    gateway: bool,
    default_live: bool,
    mode: AccountSelectionMode,
    roster: Slot,
    resolved: Slot,
    config_dir: Slot,
) -> bool {
    if gateway || default_live {
        return true;
    }
    if mode == AccountSelectionMode::Manual {
        // Manual reads `config_dir` alone, and the picker no-ops.
        return config_dir == Slot::Valid;
    }
    if roster == Slot::Valid {
        // The picker publishes the roster's live dir, and the second read finds it.
        return true;
    }
    // Nothing to pick, so the resolver answers from what it already holds. A stale
    // resolved dir beats `config_dir` even when it is the dead one.
    match resolved {
        Slot::Valid => true,
        Slot::Invalid => false,
        Slot::Absent => config_dir == Slot::Valid,
    }
}

/// Every selection mode × roster × resolved dir × `config_dir` × default location
/// × gateway. `spawn_account_ready` must agree with the spawn's own reading and
/// with the hand-derived answer, and a reading already ready with no pick must
/// leave the published pick alone.
#[test]
#[cfg_attr(
    target_os = "macos",
    ignore = "the default-location check reads the real login Keychain on macOS"
)]
fn spawn_account_ready_agrees_with_the_spawn_across_the_whole_table() {
    let fx = arrange();
    let roster_dirs = Dirs::new();
    let resolved_dirs = Dirs::new();
    let config_dirs = Dirs::new();
    let gateway_on = crate::model_gateway::ModelGatewaySettings {
        base_url: Some("https://spawn-account-parity-gw.example.com".to_string()),
        api_key_helper: Some("echo parity-key".to_string()),
        ..Default::default()
    };

    let (mut rows, mut ready_rows) = (0usize, 0usize);
    for gateway in [false, true] {
        crate::settings::save_model_gateway(if gateway {
            gateway_on.clone()
        } else {
            Default::default()
        })
        .expect("declare or clear the fixture gateway");
        assert_eq!(crate::model_gateway::gateway_declared(), gateway);

        for default_live in [false, true] {
            point_default_location_at(if default_live {
                &fx.live_default.1
            } else {
                &fx.dead_default.1
            });
            assert_eq!(default_location_has_valid_credentials(), default_live);

            for mode in MODES {
                for roster in SLOTS {
                    for config_dir in SLOTS {
                        let roster_list: Vec<&str> = roster_dirs.at(roster).into_iter().collect();
                        write_roster(mode, &roster_list, config_dirs.at(config_dir));

                        for resolved in SLOTS {
                            let case = format!(
                                "gateway={gateway} default_live={default_live} mode={mode:?} \
                                 roster={roster:?} resolved={resolved:?} config_dir={config_dir:?}"
                            );
                            let start = resolved_dirs.at(resolved).map(str::to_string);
                            let expected = expected_ready(
                                gateway,
                                default_live,
                                mode,
                                roster,
                                resolved,
                                config_dir,
                            );

                            set_resolved_config_dir(start.clone());
                            let ready_before_any_pick = reads_ready_with_no_pick();
                            let ready = spawn_account_ready();
                            assert_eq!(ready, expected, "spawn_account_ready: {case}");
                            if ready_before_any_pick {
                                assert_eq!(
                                    get_resolved_config_dir(),
                                    start,
                                    "a reading already ready must not pick: {case}"
                                );
                            }

                            set_resolved_config_dir(start);
                            assert_eq!(
                                the_spawn_would_proceed(),
                                ready,
                                "the spawn's own order disagreed: {case}"
                            );

                            rows += 1;
                            ready_rows += usize::from(ready);
                        }
                    }
                }
            }
        }
    }
    assert_eq!(rows, 2 * 2 * 5 * 3 * 3 * 3, "the table ran in full");
    assert!(
        ready_rows > 0 && ready_rows < rows,
        "a table with one answer proves nothing: {ready_rows} of {rows} ready"
    );
}

/// `/login` lands in a roster dir, not `~/.claude`, and a box doing no work never
/// reaches a pick. The resolver alone stays blind to the login, which is the case
/// the second step of [`spawn_account_ready`] exists for.
#[test]
#[cfg_attr(
    target_os = "macos",
    ignore = "the default-location check reads the real login Keychain on macOS"
)]
fn the_first_login_into_a_roster_dir_reads_ready_with_nothing_resolved() {
    let _fx = arrange();
    let (_login, login_dir) = creds_dir(true);
    write_roster(
        AccountSelectionMode::AlternatingHandoff,
        &[&login_dir],
        None,
    );
    set_resolved_config_dir(None);

    assert!(
        !reads_ready_with_no_pick(),
        "the premise: a resolver-only read cannot see this login"
    );
    assert!(spawn_account_ready());
    assert_eq!(get_resolved_config_dir(), Some(login_dir));
}

/// A box with no login anywhere reads not ready, and the failed pick publishes
/// nothing.
#[test]
#[cfg_attr(
    target_os = "macos",
    ignore = "the default-location check reads the real login Keychain on macOS"
)]
fn a_roster_of_dead_accounts_reads_not_ready_and_publishes_nothing() {
    let _fx = arrange();
    let (_dead, dead_dir) = creds_dir(false);
    write_roster(AccountSelectionMode::AlternatingHandoff, &[&dead_dir], None);
    set_resolved_config_dir(None);

    assert!(!spawn_account_ready());
    assert_eq!(get_resolved_config_dir(), None);
}

/// Asking must not rotate. The control shows the picker WOULD move off the pin, so
/// a [`spawn_account_ready`] that picked first could not pass.
#[test]
#[cfg_attr(
    target_os = "macos",
    ignore = "the default-location check reads the real login Keychain on macOS"
)]
fn a_healthy_pin_is_not_rotated() {
    let _fx = arrange();
    let (_pin, pin_dir) = creds_dir(true);
    let (_better, better_dir) = creds_dir(true);
    write_roster(
        AccountSelectionMode::LeastUsage,
        &[&pin_dir, &better_dir],
        None,
    );
    // The pin is over its pace and the other is under, so `LeastUsage` prefers it.
    record_account_usage(&[
        (pin_dir.clone(), 0.90, Some(0.40), Some(0.50), false),
        (better_dir.clone(), 0.10, Some(-0.40), Some(0.50), false),
    ]);

    set_resolved_config_dir(Some(pin_dir.clone()));
    pick_best_account();
    assert_eq!(
        get_resolved_config_dir().as_deref(),
        Some(better_dir.as_str()),
        "the control: a pick here rotates off the pin"
    );

    set_resolved_config_dir(Some(pin_dir.clone()));
    assert!(spawn_account_ready());
    assert_eq!(get_resolved_config_dir(), Some(pin_dir));
}

/// The default location answers without a pick too: a live ambient login leaves a
/// roster's accounts unpublished.
#[test]
#[cfg_attr(
    target_os = "macos",
    ignore = "the default-location check reads the real login Keychain on macOS"
)]
fn a_live_default_location_answers_without_picking() {
    let fx = arrange();
    let (_roster, roster_dir) = creds_dir(true);
    write_roster(
        AccountSelectionMode::AlternatingHandoff,
        &[&roster_dir],
        None,
    );
    point_default_location_at(&fx.live_default.1);
    set_resolved_config_dir(None);

    assert!(spawn_account_ready());
    assert_eq!(get_resolved_config_dir(), None);
}

/// The detail is posted as `spawn_failed: <detail>` and read by coord's re-drive
/// classifier, so it is pinned to a LITERAL and not to its own `format!`.
#[test]
#[cfg_attr(
    target_os = "macos",
    ignore = "the default-location check reads the real login Keychain on macOS"
)]
fn the_refusal_detail_is_byte_identical_and_names_the_instance() {
    let fx = arrange();

    assert_eq!(
        spawn_account_refusal(None).as_deref(),
        Some(NO_ACCOUNT_DETAIL)
    );
    std::env::set_var("QONTINUI_INSTANCE_NAME", "test-9877");
    assert_eq!(
        spawn_account_refusal(None).as_deref(),
        Some("no authenticated Claude account on this runner — run /login (instance=test-9877)")
    );
    std::env::remove_var("QONTINUI_INSTANCE_NAME");

    assert_eq!(
        spawn_account_refusal(Some("/a/resolved/dir")),
        None,
        "a resolved dir is the seam's own validated pin"
    );

    point_default_location_at(&fx.live_default.1);
    assert_eq!(
        spawn_account_refusal(None),
        None,
        "an unset CLAUDE_CONFIG_DIR is safe over a live default login"
    );
}

/// The four seams have no copy of the detail left to drift: it exists once, in the
/// helper. A seam that refuses must therefore route through it.
#[test]
fn every_refusal_site_routes_through_the_one_helper() {
    const DETAIL: &str = "no authenticated Claude account on this runner — run /login";
    let production = |source: &'static str, marker: &str| -> &'static str {
        source
            .split_once(marker)
            .unwrap_or_else(|| panic!("the test-module marker {marker:?}"))
            .0
    };
    let config = production(include_str!("config.rs"), "#[cfg(test)]\nmod tests");
    let runtime = production(
        include_str!("../agent_runtime.rs"),
        "#[cfg(test)]\nmod no_account_tests;",
    );
    let looping = production(
        include_str!("../looping_agent_supervisor.rs"),
        "#[cfg(test)]\nmod nudge_and_exit_tests",
    );

    assert_eq!(config.matches(DETAIL).count(), 1, "the helper's one copy");
    assert_eq!(runtime.matches(DETAIL).count(), 0, "agent_runtime.rs");
    assert_eq!(
        looping.matches(DETAIL).count(),
        0,
        "looping_agent_supervisor.rs"
    );
}
