//! Plan `2026-09-30-a-gate-continuation-is-claimed-before-the-resource-guard-and-the-claude-cli-check`
//! (qontinui-runner#1925): spawn readiness checked BEFORE the consume claim
//! (D1/D2), the `claude_cli_unavailable:` stamp (D3), and the unclaimed
//! deferral of a seam refusal after a claim that provably never reached coord
//! (D5). Every test drives the production fn — `claim_if_spawn_ready`,
//! `settle_failed_gate_spawn`, `send_continuation_claim` — not a copy of it.

#![cfg(test)]

use std::cell::{Cell, RefCell};

use super::{
    claim_gate_dispatch, claim_if_spawn_ready, classify_claim_send_error, claude_cli_readiness,
    defer_continuation_unclaimed, failed_spawn_disposition, headless_spawn_error,
    presentation_claude_bin, pty_spawn_error, send_continuation_claim, settle_failed_gate_spawn,
    spawn_refusal_of, ClaimSent, CliReadiness, ConsumeTarget, FailedSpawnDisposition, PreClaim,
    Presentation, SpawnDecision, SpawnReadiness, SpawnRefusal,
};
use crate::resource_guard::{GateObservation, LaneMetric, SpawnGate};
use crate::terminal::pane_io::{probe_launchable, ExecFault};
use crate::terminal::spawn_refusal::{exec_failure_cause, SpawnSeamCause};

fn threads_observation() -> GateObservation {
    GateObservation {
        lane: "threads".to_string(),
        metric: LaneMetric::ThreadCount,
        observed: 540,
        limit: 512,
    }
}

fn memory_observation() -> GateObservation {
    GateObservation {
        lane: "host".to_string(),
        metric: LaneMetric::FreeCommitBytes,
        observed: 1,
        limit: 1024,
    }
}

fn ready() -> SpawnReadiness {
    SpawnReadiness {
        resource: SpawnGate::Proceed,
        cli: CliReadiness::Ready,
    }
}

/// Drive `claim_if_spawn_ready` with a counting claim poster and a counting
/// deferral poster that forwards to the REAL `defer_continuation_unclaimed`
/// (so the local-claim release is the production one). Returns (result,
/// claims posted, deferral stamps posted).
async fn run_pre_claim(
    consume_target: ConsumeTarget,
    claude_bin: &str,
    readiness: SpawnReadiness,
) -> (PreClaim, usize, Vec<String>) {
    let claims = Cell::new(0usize);
    let stamps = RefCell::new(Vec::new());
    let device = uuid::Uuid::now_v7();
    let result = claim_if_spawn_ready(
        consume_target,
        claude_bin,
        readiness,
        |_gate_id| {
            claims.set(claims.get() + 1);
            async { SpawnDecision::Spawn }
        },
        |stamp| {
            stamps.borrow_mut().push(stamp.clone());
            defer_continuation_unclaimed(consume_target, device, stamp)
        },
    )
    .await;
    (result, claims.get(), stamps.into_inner())
}

#[cfg(unix)]
fn cli_file(dir: &std::path::Path, mode: u32) -> String {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("claude");
    std::fs::write(&path, "#!/bin/sh\nexit 0\n").expect("write fake cli");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("chmod");
    path.to_string_lossy().into_owned()
}

// ---------------------------------------------------------------------------
// The exec predicate and the seam cause
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn probe_launchable_names_each_fault_the_exec_would() {
    let dir = tempfile::tempdir().expect("tempdir");
    let exec = cli_file(dir.path(), 0o755);
    assert_eq!(probe_launchable(std::path::Path::new(&exec)), Ok(()));

    let plain = dir.path().join("plain");
    std::fs::write(&plain, "x").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    // root bypasses the mode bits in access(2); only assert when it applies.
    if unsafe { libc::geteuid() } != 0 {
        assert_eq!(probe_launchable(&plain), Err(ExecFault::PermissionDenied));
    }
    assert_eq!(
        probe_launchable(&dir.path().join("missing")),
        Err(ExecFault::NotFound)
    );
    let dangling = dir.path().join("dangling");
    std::os::unix::fs::symlink(dir.path().join("gone"), &dangling).unwrap();
    assert_eq!(probe_launchable(&dangling), Err(ExecFault::NotFound));
    assert_eq!(
        probe_launchable(dir.path()),
        Err(ExecFault::PermissionDenied),
        "a directory passes access(X_OK) but execve refuses it"
    );
}

#[test]
fn exec_fault_wire_words() {
    assert_eq!(ExecFault::NotFound.wire(), "enoent");
    assert_eq!(ExecFault::PermissionDenied.wire(), "eacces");
    assert_eq!(ExecFault::Errno(7).wire(), "errno_7");
}

#[test]
fn exec_failure_cause_classifies_only_an_absolute_program_that_still_fails() {
    let failing = |_: &std::path::Path| Err(ExecFault::NotFound);
    let passing = |_: &std::path::Path| Ok(());
    let abs = if cfg!(windows) {
        r"C:\x\claude.exe"
    } else {
        "/x/claude"
    };
    assert_eq!(
        exec_failure_cause(Some(abs), &failing),
        Some(SpawnSeamCause::ProgramUnlaunchable {
            program: abs.to_string(),
            fault: ExecFault::NotFound,
        })
    );
    assert_eq!(
        exec_failure_cause(Some("claude"), &failing),
        None,
        "a bare name is searched on the CHILD's PATH; probing it here names the wrong file"
    );
    assert_eq!(
        exec_failure_cause(Some(abs), &passing),
        None,
        "a file that came back is an unexplained failure, recorded as one"
    );
    assert_eq!(exec_failure_cause(None, &failing), None);
}

// ---------------------------------------------------------------------------
// Pre-claim readiness (D1/D2): a refusal defers UNCLAIMED, the claim is
// posted 0 times; a ready dispatch claims exactly once.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_tripped_resource_lane_defers_before_the_claim_and_releases_the_dispatch() {
    for (gate, want_head) in [
        (
            SpawnGate::Critical(threads_observation()),
            "thread_pressure:critical:",
        ),
        (
            SpawnGate::Warn(memory_observation()),
            "commit_pressure:warn:",
        ),
    ] {
        let gate_id = uuid::Uuid::now_v7();
        assert!(claim_gate_dispatch(gate_id), "the dispatcher claims the id");
        let (result, claims, stamps) = run_pre_claim(
            ConsumeTarget::Gate(gate_id, 0),
            "/x/claude",
            SpawnReadiness {
                resource: gate,
                cli: CliReadiness::Ready,
            },
        )
        .await;
        assert_eq!(claims, 0, "a deferral must never post the consume claim");
        assert_eq!(stamps.len(), 1);
        assert!(stamps[0].starts_with(want_head), "stamp {:?}", stamps[0]);
        assert_eq!(
            result,
            PreClaim::Deferred {
                stamp: stamps[0].clone()
            }
        );
        assert!(
            claim_gate_dispatch(gate_id),
            "the deferral must release the in-process claim so a re-delivery dispatches"
        );
    }
}

#[tokio::test]
async fn an_unlaunchable_cli_defers_before_the_claim_with_its_fault() {
    for (fault, want) in [
        (ExecFault::NotFound, "claude_cli_unavailable:enoent"),
        (ExecFault::PermissionDenied, "claude_cli_unavailable:eacces"),
    ] {
        let gate_id = uuid::Uuid::now_v7();
        let (result, claims, stamps) = run_pre_claim(
            ConsumeTarget::Gate(gate_id, 0),
            "/x/claude",
            SpawnReadiness {
                resource: SpawnGate::Proceed,
                cli: CliReadiness::Unlaunchable(fault),
            },
        )
        .await;
        assert_eq!(claims, 0);
        assert_eq!(stamps, vec![want.to_string()]);
        assert!(matches!(result, PreClaim::Deferred { .. }));
    }
}

#[cfg(unix)]
#[tokio::test]
async fn an_unresolved_cli_defers_on_unix() {
    let (result, claims, stamps) = run_pre_claim(
        ConsumeTarget::Gate(uuid::Uuid::now_v7(), 0),
        "claude",
        SpawnReadiness {
            resource: SpawnGate::Proceed,
            cli: claude_cli_readiness("claude", &probe_launchable),
        },
    )
    .await;
    assert_eq!(claims, 0);
    assert_eq!(
        stamps,
        vec!["claude_cli_unavailable:not_on_path".to_string()]
    );
    assert!(matches!(result, PreClaim::Deferred { .. }));
}

#[cfg(not(unix))]
#[tokio::test]
async fn an_unresolved_cli_proceeds_on_windows() {
    let (result, claims, stamps) = run_pre_claim(
        ConsumeTarget::Gate(uuid::Uuid::now_v7(), 0),
        "claude",
        SpawnReadiness {
            resource: SpawnGate::Proceed,
            cli: claude_cli_readiness("claude", &probe_launchable),
        },
    )
    .await;
    assert_eq!(claims, 1, "the npm-only install shape proceeds unchanged");
    assert!(stamps.is_empty());
    assert!(matches!(result, PreClaim::Ready { .. }));
}

#[cfg(unix)]
#[tokio::test]
async fn a_real_cli_file_is_probed_by_the_production_predicate() {
    let dir = tempfile::tempdir().expect("tempdir");
    let exec = cli_file(dir.path(), 0o755);
    let readiness = SpawnReadiness {
        resource: SpawnGate::Proceed,
        cli: claude_cli_readiness(&exec, &probe_launchable),
    };
    let (result, claims, _) = run_pre_claim(
        ConsumeTarget::Gate(uuid::Uuid::now_v7(), 0),
        &exec,
        readiness,
    )
    .await;
    assert_eq!(claims, 1);
    assert_eq!(
        result,
        PreClaim::Ready {
            decision: Some(SpawnDecision::Spawn)
        }
    );

    std::fs::remove_file(&exec).unwrap();
    let readiness = SpawnReadiness {
        resource: SpawnGate::Proceed,
        cli: claude_cli_readiness(&exec, &probe_launchable),
    };
    let (_, claims, stamps) = run_pre_claim(
        ConsumeTarget::Gate(uuid::Uuid::now_v7(), 0),
        &exec,
        readiness,
    )
    .await;
    assert_eq!(
        claims, 0,
        "a CLI removed mid-reinstall must not burn a claim"
    );
    assert_eq!(stamps, vec!["claude_cli_unavailable:enoent".to_string()]);
}

#[tokio::test]
async fn a_ready_gate_claims_exactly_once_and_the_legacy_path_is_never_checked() {
    let (result, claims, stamps) = run_pre_claim(
        ConsumeTarget::Gate(uuid::Uuid::now_v7(), 0),
        "/x/claude",
        ready(),
    )
    .await;
    assert_eq!((claims, stamps.len()), (1, 0));
    assert_eq!(
        result,
        PreClaim::Ready {
            decision: Some(SpawnDecision::Spawn)
        }
    );

    // The legacy no-gate_id path has no re-delivery: a deferral there would be
    // a silent drop, so it is never checked — and it posts no claim.
    let (result, claims, stamps) = run_pre_claim(
        ConsumeTarget::None,
        "/x/claude",
        SpawnReadiness {
            resource: SpawnGate::Critical(threads_observation()),
            cli: CliReadiness::Unlaunchable(ExecFault::NotFound),
        },
    )
    .await;
    assert_eq!((claims, stamps.len()), (0, 0));
    assert_eq!(result, PreClaim::Ready { decision: None });
}

// ---------------------------------------------------------------------------
// D5: a seam refusal after a claim that never reached coord
// ---------------------------------------------------------------------------

fn cli_refusal_error() -> anyhow::Error {
    pty_spawn_error(
        "Failed to spawn shell: Unable to spawn /x/claude".to_string(),
        Some(SpawnSeamCause::ProgramUnlaunchable {
            program: "/x/claude".to_string(),
            fault: ExecFault::NotFound,
        }),
        "/x/claude",
    )
}

#[test]
fn the_seam_error_keeps_its_text_verbatim_and_carries_the_typed_refusal() {
    let e = cli_refusal_error();
    assert_eq!(
        e.to_string(),
        "Failed to spawn shell: Unable to spawn /x/claude",
        "the spawn_failed detail is a wire value and must not change"
    );
    assert_eq!(
        format!("{e:#}"),
        "Failed to spawn shell: Unable to spawn /x/claude",
        "the alternate form (every `{e:#}` log and the scheduler's record) must not grow a tail"
    );
    assert_eq!(
        spawn_refusal_of(&e),
        Some(&SpawnRefusal::ClaudeCliUnlaunchable {
            path: "/x/claude".to_string(),
            fault: ExecFault::NotFound,
        })
    );
    // A program that is NOT this spawn's claude keeps no refusal.
    let other = pty_spawn_error(
        "boom".to_string(),
        Some(SpawnSeamCause::ProgramUnlaunchable {
            program: "/bin/other".to_string(),
            fault: ExecFault::NotFound,
        }),
        "/x/claude",
    );
    assert!(spawn_refusal_of(&other).is_none());
}

#[test]
fn only_an_unsent_claim_with_a_typed_refusal_defers() {
    let cli = cli_refusal_error();
    let resource = pty_spawn_error(
        "resource_guard:critical:threads: …".to_string(),
        Some(SpawnSeamCause::ResourceGuard {
            severity: "critical",
            observation: threads_observation(),
        }),
        "/x/claude",
    );
    let unrelated = anyhow::anyhow!("no authenticated Claude account");

    assert_eq!(
        failed_spawn_disposition(Some(ClaimSent::NotSent), &cli),
        FailedSpawnDisposition::DeferUnclaimed("claude_cli_unavailable:enoent".into())
    );
    match failed_spawn_disposition(Some(ClaimSent::NotSent), &resource) {
        FailedSpawnDisposition::DeferUnclaimed(stamp) => {
            assert!(stamp.starts_with("thread_pressure:critical:"), "{stamp}")
        }
        other => panic!("expected a deferral, got {other:?}"),
    }
    for (claim, err) in [
        (Some(ClaimSent::Unknown), &cli),
        (None, &cli),
        (Some(ClaimSent::NotSent), &unrelated),
    ] {
        assert_eq!(
            failed_spawn_disposition(claim, err),
            FailedSpawnDisposition::PostSpawnFailed,
            "claim={claim:?} err={err}"
        );
    }
}

#[tokio::test]
async fn settle_failed_gate_spawn_wires_each_disposition_to_its_poster() {
    async fn settle(claim: Option<ClaimSent>, e: &anyhow::Error) -> (usize, Vec<String>) {
        let failed = RefCell::new(Vec::new());
        let deferred = Cell::new(0usize);
        settle_failed_gate_spawn(
            uuid::Uuid::now_v7(),
            claim,
            e,
            |detail| {
                failed.borrow_mut().push(detail);
                async {}
            },
            |_stamp| {
                deferred.set(deferred.get() + 1);
                async {}
            },
        )
        .await;
        (deferred.get(), failed.into_inner())
    }
    let e = cli_refusal_error();
    assert_eq!(settle(Some(ClaimSent::NotSent), &e).await, (1, vec![]));
    assert_eq!(
        settle(Some(ClaimSent::Unknown), &e).await,
        (
            0,
            vec!["Failed to spawn shell: Unable to spawn /x/claude".to_string()]
        )
    );
}

#[cfg(unix)]
#[test]
fn a_headless_spawn_of_a_missing_cli_carries_the_refusal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = dir.path().join("claude").to_string_lossy().into_owned();
    let mut cmd = tokio::process::Command::new(&bin);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    // No retry delays: one real spawn attempt, the production classifier.
    let failure = rt
        .block_on(crate::claude_cli_spawn::retry_transient_spawn(
            &[],
            || cmd.spawn(),
            crate::claude_cli_spawn::transient_kind,
            |_| async {},
        ))
        .expect_err("a missing binary cannot spawn");
    let e = headless_spawn_error(&bin, "/w", &failure);
    assert_eq!(
        spawn_refusal_of(&e),
        Some(&SpawnRefusal::ClaudeCliUnlaunchable {
            path: bin.clone(),
            fault: ExecFault::NotFound,
        })
    );
    assert_eq!(e.to_string(), failure.describe(&bin, "/w"));
}

// ---------------------------------------------------------------------------
// ClaimSent classification
// ---------------------------------------------------------------------------

#[test]
fn claim_send_error_is_not_sent_only_for_a_connect_failure_on_the_requested_url() {
    let requested: reqwest::Url = "http://127.0.0.1:1/coord/gates/x/continuation-consumed"
        .parse()
        .unwrap();
    let redirect: reqwest::Url = "http://127.0.0.1:2/elsewhere".parse().unwrap();
    assert_eq!(
        classify_claim_send_error(true, Some(&requested), &requested),
        ClaimSent::NotSent
    );
    assert_eq!(
        classify_claim_send_error(true, Some(&redirect), &requested),
        ClaimSent::Unknown,
        "a connect failure on a redirect target means the original POST reached a server"
    );
    assert_eq!(
        classify_claim_send_error(true, None, &requested),
        ClaimSent::Unknown
    );
    assert_eq!(
        classify_claim_send_error(false, Some(&requested), &requested),
        ClaimSent::Unknown
    );
}

#[tokio::test]
async fn claim_send_classifies_connect_refused_vs_answered() {
    // A port nothing listens on: bind, read the port, drop the listener.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    // `no_proxy`: an ambient HTTP(S)_PROXY would carry the "refused" request
    // to the proxy, which answers — and that is `Unknown`, not this test.
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let refused = send_continuation_claim(
        client
            .post(format!("http://127.0.0.1:{port}/claim"))
            .timeout(std::time::Duration::from_secs(5)),
    )
    .await;
    match refused {
        SpawnDecision::SpawnDespiteClaimError { claim_sent, .. } => {
            assert_eq!(claim_sent, ClaimSent::NotSent)
        }
        other => panic!("expected SpawnDespiteClaimError, got {other:?}"),
    }

    // A server that answers 503: coord answered, so the claim may have landed.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let _ = sock.read(&mut buf).await;
        let _ = sock
            .write_all(b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
            .await;
    });
    let answered = send_continuation_claim(client.post(format!("http://{addr}/claim"))).await;
    assert_eq!(
        answered,
        SpawnDecision::SpawnDespiteClaimError {
            cause: "claim returned status 503".into(),
            claim_sent: ClaimSent::Unknown,
        }
    );
}

// ---------------------------------------------------------------------------
// Which binary each presentation probes (review A2)
// ---------------------------------------------------------------------------

#[test]
fn each_presentation_probes_the_file_it_will_exec() {
    let failing = |_: &std::path::Path| Err(ExecFault::PermissionDenied);
    let passing = |_: &std::path::Path| Ok(());
    let abs = if cfg!(windows) {
        r"C:\x\claude.exe"
    } else {
        "/x/claude"
    };

    // Terminal: the PTY execs the resolved path itself.
    assert_eq!(
        presentation_claude_bin(&Presentation::Terminal, abs, "claude", &failing),
        (
            abs.to_string(),
            CliReadiness::Unlaunchable(ExecFault::PermissionDenied)
        )
    );
    // Headless whose resolved file is not executable: `launchable_or_bare`
    // handed the exec the bare name, so `execvp` searches on — not judged.
    assert_eq!(
        presentation_claude_bin(&Presentation::Headless, abs, "claude", &failing),
        ("claude".to_string(), CliReadiness::Ready)
    );
    // Headless with a launchable resolved file: probed as the terminal is.
    assert_eq!(
        presentation_claude_bin(&Presentation::Headless, abs, abs, &passing),
        (abs.to_string(), CliReadiness::Ready)
    );
    // Nothing resolved at all: unresolved on both presentations.
    for p in [Presentation::Terminal, Presentation::Headless] {
        assert_eq!(
            presentation_claude_bin(&p, "claude", "claude", &passing).1,
            CliReadiness::Unresolved
        );
    }
}
