//! Plan `2026-11-04-gate-continuation-defers-unclaimed-on-unlaunchable-claude-cli-and-unsent-claim`,
//! Phase 5. Wire strings are asserted as LITERALS: they are what coord groups
//! on (`continuation_deferred_reason`) and re-drives on (`spawn_failed`
//! detail heads), so a test that rebuilt them with the production `format!`
//! would pin nothing.

use std::cell::Cell;
use std::io;
use std::path::Path;

use super::*;
use crate::agent_runtime::ConsumeTarget;
use crate::resource_guard::{GateObservation, LaneMetric, SpawnGate};

fn gate_target() -> ConsumeTarget {
    ConsumeTarget::Gate(uuid::Uuid::now_v7(), 0)
}

fn threads_critical(observed: u64, limit: u64) -> SpawnGate {
    SpawnGate::Critical(GateObservation {
        lane: "threads".to_string(),
        metric: LaneMetric::ThreadCount,
        observed,
        limit,
    })
}

// ---------------------------------------------------------------------------
// Test 1 — claude_cli_readiness, with the PRODUCTION predicate
// ---------------------------------------------------------------------------

#[test]
fn an_executable_file_is_ready() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join(if cfg!(windows) {
        "claude.exe"
    } else {
        "claude"
    });
    std::fs::write(&bin, b"#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    assert_eq!(
        probe_claude_cli(&bin.to_string_lossy()),
        CliReadiness::Ready
    );
}

#[cfg(unix)]
#[test]
fn a_non_executable_file_is_unlaunchable_eacces() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("claude");
    std::fs::write(&bin, b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o644)).unwrap();
    // Root bypasses the mode check for `access(X_OK)` only when SOME execute
    // bit is set; 0o644 has none, so this holds for root too.
    assert_eq!(
        probe_claude_cli(&bin.to_string_lossy()),
        CliReadiness::Unlaunchable(CliKind::Known("eacces"))
    );
}

#[cfg(unix)]
#[test]
fn a_dangling_symlink_is_unlaunchable_enoent() {
    let tmp = tempfile::tempdir().unwrap();
    let link = tmp.path().join("claude");
    std::os::unix::fs::symlink(tmp.path().join("gone/claude.js"), &link).unwrap();
    assert_eq!(
        probe_claude_cli(&link.to_string_lossy()),
        CliReadiness::Unlaunchable(CliKind::Known("enoent"))
    );
}

/// "Never pick a directory as claude" (#1925 round 3), through the PRODUCTION
/// predicate rather than an injected one: `access(X_OK)` alone SUCCEEDS on a
/// searchable directory, so deleting the `is_dir()` arm of
/// [`exec_seam_access`] turns this red on unix.
#[test]
fn a_directory_is_unlaunchable() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join(if cfg!(windows) {
        "claude.exe"
    } else {
        "claude"
    });
    std::fs::create_dir(&dir).unwrap();
    let readiness = probe_claude_cli(&dir.to_string_lossy());
    assert!(
        matches!(readiness, CliReadiness::Unlaunchable(_)),
        "{readiness:?}"
    );
    #[cfg(unix)]
    assert_eq!(
        readiness,
        CliReadiness::Unlaunchable(CliKind::Known("eacces"))
    );
}

#[test]
fn a_bare_name_is_unresolved() {
    assert_eq!(probe_claude_cli("claude"), CliReadiness::Unresolved);
    // The injected predicate is never consulted for a bare name.
    let consulted = Cell::new(false);
    let access = |_: &Path| -> io::Result<()> {
        consulted.set(true);
        Ok(())
    };
    assert_eq!(
        claude_cli_readiness("claude", &access),
        CliReadiness::Unresolved
    );
    assert!(!consulted.get());
}

#[test]
fn the_kind_vocabulary_is_the_shared_one() {
    assert_eq!(
        CliKind::from_io(&io::Error::from(io::ErrorKind::NotFound)),
        CliKind::Known("enoent")
    );
    assert_eq!(CliKind::Known("enoent").token(), "enoent");
    assert_eq!(CliKind::NotOnPath.token(), "not_on_path");
    assert_eq!(CliKind::Errno(40).token(), "errno_40");
    assert_eq!(
        claude_cli_unavailable_stamp_reason(&CliKind::NotOnPath),
        "claude_cli_unavailable:not_on_path"
    );
    #[cfg(unix)]
    assert_eq!(
        CliKind::from_io(&io::Error::from_raw_os_error(libc::ELOOP)),
        CliKind::Errno(libc::ELOOP)
    );
}

// ---------------------------------------------------------------------------
// Test 2 — claim_if_spawn_ready, counting both posters
// ---------------------------------------------------------------------------

/// Run [`claim_if_spawn_ready`] with counting posters; returns
/// `(result, post_claim calls, deferral stamps)`.
async fn run_counted(
    target: ConsumeTarget,
    readiness: CliReadiness,
) -> (PreClaim<&'static str>, u32, Vec<String>) {
    let claims = Cell::new(0u32);
    let deferrals = std::cell::RefCell::new(Vec::new());
    let out = claim_if_spawn_ready(
        target,
        "/opt/claude",
        &readiness,
        |_gate| {
            claims.set(claims.get() + 1);
            async { "claimed" }
        },
        |stamp| {
            deferrals.borrow_mut().push(stamp);
            async {}
        },
    )
    .await;
    (out, claims.get(), deferrals.into_inner())
}

#[tokio::test]
async fn an_unlaunchable_cli_defers_a_gate_without_claiming() {
    let (out, claims, deferrals) = run_counted(
        gate_target(),
        CliReadiness::Unlaunchable(CliKind::Known("eacces")),
    )
    .await;
    assert_eq!(claims, 0, "no claim may be posted for an unlaunchable CLI");
    assert_eq!(deferrals, vec!["claude_cli_unavailable:eacces".to_string()]);
    assert_eq!(
        out,
        PreClaim::Deferred {
            stamp: "claude_cli_unavailable:eacces".to_string()
        }
    );
}

#[tokio::test]
async fn a_ready_cli_claims_a_gate_exactly_once() {
    let (out, claims, deferrals) = run_counted(gate_target(), CliReadiness::Ready).await;
    assert_eq!(claims, 1);
    assert!(deferrals.is_empty());
    assert_eq!(
        out,
        PreClaim::Ready {
            decision: Some("claimed")
        }
    );
}

#[cfg(unix)]
#[tokio::test]
async fn an_unresolved_cli_defers_on_unix() {
    let (out, claims, deferrals) = run_counted(gate_target(), CliReadiness::Unresolved).await;
    assert_eq!(claims, 0);
    assert_eq!(
        deferrals,
        vec!["claude_cli_unavailable:not_on_path".to_string()]
    );
    assert!(matches!(out, PreClaim::Deferred { .. }));
}

#[cfg(not(unix))]
#[tokio::test]
async fn an_unresolved_cli_proceeds_on_windows() {
    // The npm-only `.cmd` install shape: fail open on UNKNOWN.
    let (out, claims, deferrals) = run_counted(gate_target(), CliReadiness::Unresolved).await;
    assert_eq!(claims, 1);
    assert!(deferrals.is_empty());
    assert!(matches!(out, PreClaim::Ready { decision: Some(_) }));
}

#[tokio::test]
async fn the_legacy_path_is_never_deferred() {
    // No re-delivery on the legacy path, so a deferral would be a silent drop.
    let (out, claims, deferrals) = run_counted(
        ConsumeTarget::None,
        CliReadiness::Unlaunchable(CliKind::Known("enoent")),
    )
    .await;
    assert!(deferrals.is_empty(), "legacy must proceed unchanged");
    assert_eq!(claims, 0, "the legacy path posts no claim");
    assert_eq!(out, PreClaim::Ready { decision: None });
}

#[tokio::test]
async fn a_unit_dispatch_defers_on_an_unlaunchable_cli_and_never_claims() {
    let dispatch = ConsumeTarget::Dispatch(uuid::Uuid::now_v7());
    let (out, claims, deferrals) = run_counted(
        dispatch,
        CliReadiness::Unlaunchable(CliKind::Known("enoent")),
    )
    .await;
    assert_eq!(claims, 0);
    assert_eq!(deferrals, vec!["claude_cli_unavailable:enoent".to_string()]);
    assert!(matches!(out, PreClaim::Deferred { .. }));

    let (out, claims, deferrals) = run_counted(dispatch, CliReadiness::Ready).await;
    assert_eq!(claims, 0, "a unit dispatch has no consume claim");
    assert!(deferrals.is_empty());
    assert_eq!(out, PreClaim::Ready { decision: None });
}

// ---------------------------------------------------------------------------
// Test 5 — DeferredStampPost classification
// ---------------------------------------------------------------------------

#[test]
fn deferred_stamp_statuses_classify_by_what_coord_answered() {
    for unreached in [None, Some(500), Some(503), Some(302)] {
        assert_eq!(
            classify_deferred_stamp_status(unreached),
            DeferredStampPost::Unreached,
            "{unreached:?}"
        );
    }
    assert_eq!(
        classify_deferred_stamp_status(Some(200)),
        DeferredStampPost::Posted
    );
    for rejected in [404, 409] {
        assert_eq!(
            classify_deferred_stamp_status(Some(rejected)),
            DeferredStampPost::Rejected,
            "{rejected}"
        );
    }
}

// ---------------------------------------------------------------------------
// Test 6 — classify_claim_send_error
// ---------------------------------------------------------------------------

#[test]
fn only_a_connect_failure_on_the_requested_url_is_not_sent() {
    let requested: reqwest::Url = "http://coord.example/coord/gates/x/continuation-consumed"
        .parse()
        .unwrap();
    let redirected: reqwest::Url = "http://other.example/elsewhere".parse().unwrap();
    assert_eq!(
        classify_claim_send_error(true, Some(&requested), &requested),
        ClaimSent::NotSent
    );
    assert_eq!(
        classify_claim_send_error(true, Some(&redirected), &requested),
        ClaimSent::Unknown,
        "a connect failure on a redirect target means the POST reached a server"
    );
    assert_eq!(
        classify_claim_send_error(false, Some(&requested), &requested),
        ClaimSent::Unknown
    );
    assert_eq!(
        classify_claim_send_error(true, None, &requested),
        ClaimSent::Unknown
    );
}

// ---------------------------------------------------------------------------
// Test 7 — failed_spawn_disposition
// ---------------------------------------------------------------------------

#[test]
fn only_an_unsent_claim_with_a_seam_refusal_defers() {
    let resource = SeamRefusal::ResourceGuard {
        stamp: "thread_pressure:critical:900_over_800".to_string(),
    };
    let cli = SeamRefusal::ClaudeCli(CliKind::Known("enoent"));

    match failed_spawn_disposition(Some(ClaimSent::NotSent), Some(&resource)) {
        FailedSpawnDisposition::DeferUnclaimed(stamp) => {
            assert!(stamp.starts_with("thread_pressure:critical:"), "{stamp}")
        }
        other => panic!("expected a deferral, got {other:?}"),
    }
    assert_eq!(
        failed_spawn_disposition(Some(ClaimSent::NotSent), Some(&cli)),
        FailedSpawnDisposition::DeferUnclaimed("claude_cli_unavailable:enoent".to_string())
    );
    for refusal in [&resource, &cli] {
        assert_eq!(
            failed_spawn_disposition(Some(ClaimSent::Unknown), Some(refusal)),
            FailedSpawnDisposition::PostOutcome,
            "a claim that may have landed must never be deferred"
        );
        assert_eq!(
            failed_spawn_disposition(None, Some(refusal)),
            FailedSpawnDisposition::PostOutcome,
            "an accepted claim must never be deferred"
        );
    }
    assert_eq!(
        failed_spawn_disposition(Some(ClaimSent::NotSent), None),
        FailedSpawnDisposition::PostOutcome
    );
}

// ---------------------------------------------------------------------------
// classify_seam_refusal — the three recognisers
// ---------------------------------------------------------------------------

fn never_probed(_: &str) -> CliReadiness {
    panic!("the CLI must not be re-probed for this error")
}

fn calm() -> SpawnGate {
    SpawnGate::Proceed
}

#[test]
fn a_resource_guard_refusal_stamps_fresh_numbers_on_the_same_lane() {
    let err = anyhow::Error::new(TerminalSeamFailure {
        claude_bin: "/opt/claude".to_string(),
        message: "resource_guard:critical:thread_count: Not starting a new terminal session: \
                  whatever the human text says"
            .to_string(),
    });
    assert_eq!(
        classify_seam_refusal(&err, &never_probed, &|| threads_critical(900, 800)),
        Some(SeamRefusal::ResourceGuard {
            stamp: "thread_pressure:critical:900_over_800".to_string()
        })
    );
    // The fresh reading no longer trips that lane: the refusal's own token.
    assert_eq!(
        classify_seam_refusal(&err, &never_probed, &calm),
        Some(SeamRefusal::ResourceGuard {
            stamp: "resource_guard:critical:thread_count".to_string()
        })
    );
    // A Critical on a DIFFERENT lane is not this refusal's numbers.
    let memory_err = anyhow::anyhow!(
        "resource_guard:critical:free_commit_bytes: Not starting a new terminal session"
    );
    assert_eq!(
        classify_seam_refusal(&memory_err, &never_probed, &|| threads_critical(900, 800)),
        Some(SeamRefusal::ResourceGuard {
            stamp: "resource_guard:critical:free_commit_bytes".to_string()
        })
    );
}

#[test]
fn a_terminal_failure_is_a_cli_refusal_only_when_the_re_probe_says_so() {
    let err = anyhow::Error::new(TerminalSeamFailure {
        claude_bin: "/home/u/.nvm/versions/node/v24.19.0/bin/claude".to_string(),
        message: "PTY holder spawn failed: holder failed [exit_status: 5]".to_string(),
    });
    let probed = std::cell::RefCell::new(Vec::new());
    let gone = |bin: &str| {
        probed.borrow_mut().push(bin.to_string());
        CliReadiness::Unlaunchable(CliKind::Known("enoent"))
    };
    assert_eq!(
        classify_seam_refusal(&err, &gone, &calm),
        Some(SeamRefusal::ClaudeCli(CliKind::Known("enoent")))
    );
    assert_eq!(
        probed.into_inner(),
        vec!["/home/u/.nvm/versions/node/v24.19.0/bin/claude".to_string()],
        "the re-probe names the exact string the attempt exec'd"
    );
    // The CLI is back: classified non-CLI, nothing deferred on a guess.
    assert_eq!(
        classify_seam_refusal(&err, &|_| CliReadiness::Ready, &calm),
        None
    );
    // An untyped error is never re-probed into a CLI refusal.
    let untyped = anyhow::anyhow!("PTY holder spawn failed: something");
    assert_eq!(classify_seam_refusal(&untyped, &never_probed, &calm), None);
}

/// D-C: the headless failure stays TYPED under `spawn_claude_child`'s context,
/// and its text is byte-identical to the old `anyhow!("{}", describe)`.
#[test]
fn a_headless_cli_failure_downcasts_and_keeps_its_text() {
    let failure = crate::claude_cli_spawn::CliSpawnFailure {
        error: io::Error::from(io::ErrorKind::NotFound),
        attempts: 4,
        elapsed: std::time::Duration::from_secs(10),
        transient_kind: Some("enoent"),
    };
    let describe = failure.describe("/x/bin/claude", "/w");
    let err = anyhow::Error::new(crate::claude_cli_spawn::DescribedCliSpawnFailure {
        failure,
        detail: describe.clone(),
    });
    assert_eq!(err.to_string(), describe, "the detail text must not change");
    assert_eq!(
        format!("{err:#}"),
        describe,
        "no doubled io error under {{:#}}"
    );
    assert!(err
        .downcast_ref::<crate::claude_cli_spawn::DescribedCliSpawnFailure>()
        .is_some());
    assert_eq!(
        classify_seam_refusal(&err, &never_probed, &calm),
        Some(SeamRefusal::ClaudeCli(CliKind::Known("enoent")))
    );

    // A non-transient headless failure is not a CLI refusal.
    let other = crate::claude_cli_spawn::CliSpawnFailure {
        error: io::Error::other("boom"),
        attempts: 1,
        elapsed: std::time::Duration::ZERO,
        transient_kind: None,
    };
    let text = other.describe("/x/bin/claude", "/w");
    let err = anyhow::Error::new(other).context(text);
    assert_eq!(classify_seam_refusal(&err, &never_probed, &calm), None);
}

// ---------------------------------------------------------------------------
// Test 9 — Phase 4's detail head
// ---------------------------------------------------------------------------

/// A copy of coord's head check (qontinui-coord
/// `crates/coord/src/continuation_spawn_retry.rs:233`, the
/// `claude_cli_unavailable:` arm of `classify_spawn_failure`) — the consumer
/// this head exists for.
fn coord_reads_as_missing_cli(detail: &str) -> bool {
    detail.starts_with("claude_cli_unavailable:")
}

#[test]
fn a_cli_refusal_detail_leads_with_the_retriable_head() {
    let raw = "PTY holder spawn failed: holder failed [exit_status: 5]: holder_error=child child: \
               spawn: Unable to spawn /home/u/bin/claude because it doesn't exist";
    let cli = SeamRefusal::ClaudeCli(CliKind::Known("enoent"));
    let detail = outcome_detail(Some(&cli), raw);
    assert!(
        detail.starts_with("claude_cli_unavailable:enoent: PTY holder spawn failed: "),
        "{detail}"
    );
    assert!(coord_reads_as_missing_cli(&detail));
    assert!(
        !coord_reads_as_missing_cli(raw),
        "the holder's own text is what coord classified Deterministic"
    );

    // The resource guard's detail is byte-identical: coord already matches it.
    let resource = SeamRefusal::ResourceGuard {
        stamp: "thread_pressure:critical:900_over_800".to_string(),
    };
    let rg = "resource_guard:critical:thread_count: Not starting a new terminal session";
    assert_eq!(outcome_detail(Some(&resource), rg), rg);
    // No refusal: unchanged.
    assert_eq!(outcome_detail(None, raw), raw);
    // A detail that already carries the head (the headless retry's) is not
    // double-prefixed.
    let headless = "claude_cli_unavailable:enoent: spawn `/x/claude` in /w: not found";
    assert_eq!(outcome_detail(Some(&cli), headless), headless);
}

/// Review M1: the headless arm execs the BARE name after `launchable_or_bare`
/// rejects a non-executable resolution, and `execvp` searches PATH past it —
/// so a bare name with a launchable PATH entry is Ready, and without one it
/// is not.
#[cfg(unix)]
#[test]
fn a_bare_name_is_launchable_only_when_a_path_entry_holds_an_executable() {
    use std::os::unix::fs::PermissionsExt;
    let stub = tempfile::tempdir().unwrap();
    let real = tempfile::tempdir().unwrap();
    let s = stub.path().join("claude");
    std::fs::write(&s, b"#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&s, std::fs::Permissions::from_mode(0o644)).unwrap();
    let only_stub = std::env::join_paths([stub.path()]).unwrap();
    assert!(!bare_name_on_path("claude", Some(&only_stub)));
    let r = real.path().join("claude");
    std::fs::write(&r, b"#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&r, std::fs::Permissions::from_mode(0o755)).unwrap();
    let both = std::env::join_paths([stub.path(), real.path()]).unwrap();
    assert!(bare_name_on_path("claude", Some(&both)));
    // A directory named `claude` is skipped, as execve would refuse it.
    let dir_only = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir_only.path().join("claude")).unwrap();
    let dirs = std::env::join_paths([dir_only.path()]).unwrap();
    assert!(!bare_name_on_path("claude", Some(&dirs)));
    assert!(!bare_name_on_path("claude", None));
    assert!(!bare_name_on_path("a/claude", Some(&both)));
}
