//! The runner's trigger for an in-place PTY holder upgrade — OPERATOR-INITIATED
//! only.
//!
//! Plan `2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions`,
//! the runner-side follow-up to Phase 8 (D12a, D15). The holder half
//! (`qontinui_pty_holder::handoff`) shipped dormant: nothing sent
//! `prepare_upgrade`. This module is the one place that does, and only when
//! the Terminal page's "Upgrade holder" action asks for one pane — never on a
//! timer, at boot, or as a side effect of anything else.
//!
//! # When an upgrade is offered and allowed ([`gate`])
//!
//! 1. **The platform supports it** — [`upgrade_supported_here`]: Linux. The
//!    holder answers on every Unix (`server::upgrade_supported()`; D12: a
//!    ConPTY cannot be handed to another process), but its input drain works
//!    only with Linux's `SHUT_RD`, so elsewhere every upgrade of a pane this
//!    runner is attached to rolls back and is not offered. The runner and its
//!    holders run on one machine, so the runner's own build answers for them;
//!    a holder that disagrees still answers `unsupported`, reported as such.
//! 2. **The pane is holder-backed and its holder answers** — a census probe of
//!    that one pane, never a guess. A pane this runner is spawning right now is
//!    not probed (a probe can steal a starting holder's lock).
//! 3. **The holder needs it** — it speaks none of this runner's protocol
//!    versions (`Incompatible`, D15), or it answers with an OLDER build than
//!    the holder this runner ships ([`older_build`]). A holder of this
//!    runner's build or newer is refused: there is nothing to upgrade it to.
//!
//! # What the successor is
//!
//! `prepare_upgrade` is a frozen envelope verb with no arguments: the holder
//! starts its successor from ITS OWN executable path (`handoff::successor_exe`).
//! The upgrade therefore moves a pane onto a newer holder only when that path
//! now holds a newer binary (an installer replaced it in place). The outcome
//! reports the successor's build ([`HolderUpgradeOutcome::Upgraded`]), so a
//! swap onto the same build is visible rather than claimed as an upgrade.
//!
//! # Outcomes ([`HolderUpgradeOutcome`])
//!
//! `upgraded`, `rolled_back {reason}` (the holder kept serving — including a
//! declared-loss take-back, whose reason says what may be missing), `failed`
//! (the request never reached the holder: nothing changed), `unsupported`,
//! `unknown_on_timeout` (sent, unanswered within [`UPGRADE_DEADLINE`]: it may
//! have committed, rolled back, or still be running), and `refused` (a gate
//! said no; nothing was sent).
//!
//! # Windows, and "end and resume"
//!
//! Where an upgrade is unsupported the Terminal page offers "Leave running"
//! and, once plan Phase 6's resume path exists in this build, "End and
//! resume". [`END_AND_RESUME_AVAILABLE`] says whether it does.

use std::path::Path;
use std::time::{Duration, Instant};

use qontinui_pty_holder::client::{
    probe, upgrade_pane, ConnectError, PaneUpgrade, PaneUpgradeError, Probe, UpgradeOutcome,
};
use qontinui_pty_holder::pane::PaneId;
use serde::Serialize;

use crate::session::tracking_health::{HolderCensus, HolderPaneState};

/// The whole exchange's bound: the holder's own bounds sum to about 41 s
/// (`client::Client::prepare_upgrade`), so 45 s.
pub const UPGRADE_DEADLINE: Duration = Duration::from_secs(45);

/// Connect + `hello` before `prepare_upgrade` is sent.
pub const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(5);

/// One probe of the pane (the gate's, and the successor read-back's).
const PROBE_DEADLINE: Duration = Duration::from_secs(3);

/// How long the read-back waits for the successor's endpoint after
/// `upgraded` (the predecessor exits only after `serving`, but the successor's
/// build is read through a fresh probe that can meet the endpoint gap).
const READ_BACK_WINDOW: Duration = Duration::from_secs(3);

/// Whether this build has plan Phase 6's "end the pane and resume its session"
/// path. It does not (Phase 6 is a separate change): the page then offers
/// "Leave running" alone where an upgrade is unsupported, and says why.
pub const END_AND_RESUME_AVAILABLE: bool = false;

/// Whether a holder upgrade can WORK on this platform (review p8f r1, #5).
/// The holder answers `prepare_upgrade` on every Unix, but its input drain
/// needs Linux's `SHUT_RD` semantics: on other Unix it refuses while any input
/// connection is open, and a runner keeps one open for its pane's whole life —
/// so there every upgrade of a pane this runner is attached to rolls back.
/// The action is offered only where it can succeed.
pub fn upgrade_supported_here() -> bool {
    upgrade_offered_on(
        qontinui_pty_holder::server::upgrade_supported(),
        cfg!(target_os = "linux"),
    )
}

/// [`upgrade_supported_here`] as a pure rule.
fn upgrade_offered_on(holder_supports: bool, linux: bool) -> bool {
    holder_supports && linux
}

/// Why an upgrade is not offered here, for the page's note and the gate's
/// `unsupported` outcome. `None` where it is offered.
pub fn upgrade_unsupported_reason() -> Option<String> {
    unsupported_reason_on(
        qontinui_pty_holder::server::upgrade_supported(),
        cfg!(target_os = "linux"),
    )
}

/// [`upgrade_unsupported_reason`] as a pure rule.
fn unsupported_reason_on(holder_supports: bool, linux: bool) -> Option<String> {
    if !holder_supports {
        Some(qontinui_pty_holder::server::UPGRADE_UNSUPPORTED_ON_THIS_OS.to_string())
    } else if !linux {
        Some(UPGRADE_CANNOT_DRAIN_HERE.to_string())
    } else {
        None
    }
}

/// The non-Linux Unix reason ([`upgrade_supported_here`]).
pub const UPGRADE_CANNOT_DRAIN_HERE: &str = "holder upgrade cannot complete on this platform \
     while the runner is attached: the holder's input drain needs Linux's SHUT_RD, so with this \
     runner's input connection open every upgrade rolls back; leave this holder running";

/// The holder build this runner ships (its linked `qontinui-pty-holder`).
pub fn runner_holder_build() -> String {
    qontinui_pty_holder::protocol::holder_build()
}

/// The `x.y.z` at the end of a `holder_build` string
/// (`"qontinui-pty-holder 0.1.0"`), pre-release and build metadata dropped.
fn build_version(build: &str) -> Option<(u64, u64, u64)> {
    let token = build.split_whitespace().last()?;
    let core = token.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    let v = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(v)
}

/// `holder` is a strictly OLDER build than `runner`. Unparseable on either
/// side is `false`: an upgrade is offered only on proof.
pub fn older_build(holder: &str, runner: &str) -> bool {
    match (build_version(holder), build_version(runner)) {
        (Some(h), Some(r)) => h < r,
        _ => false,
    }
}

/// Why a census row needs the operator's attention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionReason {
    /// No common protocol version (D15): this runner cannot drive it.
    Incompatible,
    /// It answers, at an older holder build than this runner ships.
    OlderBuild,
}

/// One pane the Terminal page lists for an upgrade / leave-running decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttentionPane {
    pub pane_id: String,
    pub reason: AttentionReason,
    pub holder_pid: Option<u32>,
    pub child_pid: Option<u32>,
    pub holder_build: Option<String>,
    pub holder_versions: Vec<u32>,
    pub detail: Option<String>,
}

/// What the Terminal page needs to render holder actions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HolderAttention {
    /// "Upgrade holder" may be offered at all ([`upgrade_supported_here`]).
    pub upgrade_supported: bool,
    /// Why it is not ([`upgrade_unsupported_reason`]); `None` where offered.
    pub upgrade_unsupported_reason: Option<String>,
    /// "End and resume" may be offered ([`END_AND_RESUME_AVAILABLE`]).
    pub end_and_resume_available: bool,
    pub runner_holder_build: String,
    pub panes: Vec<AttentionPane>,
    /// The census could not enumerate the pane directory: the list is
    /// UNKNOWN, not empty.
    pub census_error: Option<String>,
}

/// The census rows that need a decision, and what may be offered for them.
pub fn attention_from_census(
    census: &HolderCensus,
    runner_build: &str,
    upgrade_supported: bool,
) -> HolderAttention {
    let panes = census
        .panes
        .iter()
        .filter_map(|p| {
            let reason = match p.state {
                HolderPaneState::Incompatible => AttentionReason::Incompatible,
                HolderPaneState::Live
                    if p.holder_build
                        .as_deref()
                        .is_some_and(|b| older_build(b, runner_build)) =>
                {
                    AttentionReason::OlderBuild
                }
                _ => return None,
            };
            Some(AttentionPane {
                pane_id: p.pane_id.clone(),
                reason,
                holder_pid: p.holder_pid,
                child_pid: p.child_pid,
                holder_build: p.holder_build.clone(),
                holder_versions: p.holder_versions.clone(),
                detail: p.detail.clone(),
            })
        })
        .collect();
    HolderAttention {
        upgrade_supported,
        upgrade_unsupported_reason: if upgrade_supported {
            None
        } else {
            upgrade_unsupported_reason().or_else(|| {
                Some(qontinui_pty_holder::server::UPGRADE_UNSUPPORTED_ON_THIS_OS.to_string())
            })
        },
        end_and_resume_available: END_AND_RESUME_AVAILABLE,
        runner_holder_build: runner_build.to_string(),
        panes,
        census_error: census.error.clone(),
    }
}

/// How one operator-initiated upgrade ended — what the Terminal page shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum HolderUpgradeOutcome {
    /// `successor_pid` serves the pane now; same child, same stream offsets.
    Upgraded {
        successor_pid: u32,
        handoff_offset: u64,
        /// The asked holder's build, when it answered a handshake.
        previous_build: Option<String>,
        /// The successor's build, read back by a probe — `None` when the
        /// read-back did not answer in time (UNKNOWN, not "same").
        successor_build: Option<String>,
    },
    /// The holder rolled the upgrade back and keeps serving the pane.
    RolledBack { reason: String },
    /// `prepare_upgrade` never reached the holder: nothing changed.
    Failed { reason: String },
    /// This holder (or platform) cannot be upgraded in place.
    Unsupported { reason: String },
    /// Sent, and not answered within [`UPGRADE_DEADLINE`]: UNKNOWN whether it
    /// committed. `holder_pid_now` is whoever answers for the pane after the
    /// wait, when anyone does.
    UnknownOnTimeout {
        reason: String,
        holder_pid_now: Option<u32>,
    },
    /// A gate refused before anything was sent ([`gate`]).
    Refused { reason: String },
}

/// What [`gate`] decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gate {
    /// Send `prepare_upgrade`. `previous_build` is the holder's build when it
    /// answered a handshake (an incompatible holder answers none);
    /// `expected_pid` is the holder the check saw (`None`: an incompatible
    /// one), so a pane that changed hands since is not asked
    /// (`client::upgrade_pane`).
    Proceed {
        previous_build: Option<String>,
        expected_pid: Option<u32>,
    },
    Stop(HolderUpgradeOutcome),
}

/// The half of [`gate`] that needs no probe: the platform, and a pane this
/// runner is spawning right now (which must not be probed at all).
fn pre_gate(supported: bool, in_flight: bool) -> Option<HolderUpgradeOutcome> {
    if !supported {
        return Some(HolderUpgradeOutcome::Unsupported {
            reason: upgrade_unsupported_reason().unwrap_or_else(|| {
                qontinui_pty_holder::server::UPGRADE_UNSUPPORTED_ON_THIS_OS.to_string()
            }),
        });
    }
    in_flight.then(|| HolderUpgradeOutcome::Refused {
        reason: "this runner is spawning that pane right now; it is not probed or upgraded".into(),
    })
}

/// Whether one pane's holder may be upgraded (module docs, "When").
pub fn gate(supported: bool, in_flight: bool, probe: &Probe, runner_build: &str) -> Gate {
    if let Some(stop) = pre_gate(supported, in_flight) {
        return Gate::Stop(stop);
    }
    let refuse = |reason: String| Gate::Stop(HolderUpgradeOutcome::Refused { reason });
    match probe {
        // TOCTOU, accepted (review p8f r1, #7): `no_common_version` carries
        // no pid, so `expected_pid: None` re-checks only that the pane is
        // STILL served by SOME incompatible holder when the request is sent
        // (`client::upgrade_pane`) — one incompatible holder swapped for
        // another in between is not detected. The request still reaches only
        // whichever holder serves this pane, and upgrading it is what was
        // asked. Closing it needs the pid in `no_common_version`, a frozen-
        // envelope change left to a later protocol revision.
        Probe::Incompatible { .. } => Gate::Proceed {
            previous_build: None,
            expected_pid: None,
        },
        Probe::Healthy { hello_ack } if older_build(&hello_ack.holder_build, runner_build) => {
            Gate::Proceed {
                previous_build: Some(hello_ack.holder_build.clone()),
                expected_pid: Some(hello_ack.holder_pid),
            }
        }
        Probe::Healthy { hello_ack } => refuse(format!(
            "its holder ({}) is neither incompatible nor older than this runner's ({runner_build}); \
             there is nothing to upgrade it to",
            hello_ack.holder_build
        )),
        Probe::Unknown { reason, .. } => refuse(format!(
            "its holder holds the pane's lock but did not answer ({reason}) — its state is \
             UNKNOWN, so it is not upgraded"
        )),
        Probe::Dead { .. } => refuse("its holder is dead (the pane's lock is free)".into()),
        Probe::Absent => refuse("the pane is not holder-backed (no holder lock for it)".into()),
    }
}

/// Map what the holder answered (or did not) to the page's outcome.
/// `read_back` probes the pane again; it is called at most once, and only
/// where the outcome needs to know who serves the pane now.
pub fn map_outcome(
    result: Result<PaneUpgrade, PaneUpgradeError>,
    previous_build: Option<String>,
    read_back: impl FnOnce(Option<u32>) -> Option<(u32, String)>,
) -> HolderUpgradeOutcome {
    match result {
        Ok(PaneUpgrade { outcome, .. }) => match outcome {
            UpgradeOutcome::Upgraded {
                successor_pid,
                handoff_offset,
            } => HolderUpgradeOutcome::Upgraded {
                successor_pid,
                handoff_offset,
                previous_build,
                successor_build: read_back(Some(successor_pid))
                    .filter(|(pid, _)| *pid == successor_pid)
                    .map(|(_, build)| build),
            },
            UpgradeOutcome::Failed(reason) => HolderUpgradeOutcome::RolledBack { reason },
            // Rejected before anything started: nothing to roll back.
            UpgradeOutcome::Rejected(reason) => HolderUpgradeOutcome::Failed {
                reason: format!("{reason}; nothing changed"),
            },
            UpgradeOutcome::Unsupported(reason) => HolderUpgradeOutcome::Unsupported { reason },
        },
        Err(PaneUpgradeError::NotSent(e)) => HolderUpgradeOutcome::Failed {
            reason: format!("the upgrade request did not reach the holder ({e}); nothing changed"),
        },
        Err(PaneUpgradeError::Unanswered(e)) => {
            let timed_out = matches!(
                &e,
                ConnectError::Io(io) if matches!(
                    io.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                )
            );
            let what = if timed_out {
                format!("the holder did not answer the upgrade within {UPGRADE_DEADLINE:?} ({e})")
            } else {
                format!("the holder's answer to the upgrade could not be read ({e})")
            };
            HolderUpgradeOutcome::UnknownOnTimeout {
                reason: format!("{what}; it may have committed, rolled back, or still be running"),
                holder_pid_now: read_back(None).map(|(pid, _)| pid),
            }
        }
    }
}

/// Probe until a holder answers (as `want` when given), for at most
/// [`READ_BACK_WINDOW`]. `(holder_pid, holder_build)`.
fn read_back(pane_dir: &Path, pane: &PaneId, want: Option<u32>) -> Option<(u32, String)> {
    let end = Instant::now() + READ_BACK_WINDOW;
    loop {
        if let Probe::Healthy { hello_ack } = probe(pane_dir, pane, Instant::now() + PROBE_DEADLINE)
        {
            if want.is_none_or(|w| w == hello_ack.holder_pid) {
                return Some((hello_ack.holder_pid, hello_ack.holder_build));
            }
        }
        if Instant::now() >= end {
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Upgrade one pane's holder, if the gate allows. BLOCKING (up to
/// [`UPGRADE_DEADLINE`] plus probes): run it off the async workers. The only
/// caller in production is the operator's "Upgrade holder" command.
pub fn upgrade_blocking(pane_dir: &Path, pane: &PaneId, in_flight: bool) -> HolderUpgradeOutcome {
    upgrade_blocking_with(
        pane_dir,
        pane,
        in_flight,
        upgrade_supported_here(),
        &runner_holder_build(),
    )
}

/// [`upgrade_blocking`] with the platform answer and the runner's build
/// injected — the seam the tests drive against a real holder.
pub fn upgrade_blocking_with(
    pane_dir: &Path,
    pane: &PaneId,
    in_flight: bool,
    supported: bool,
    runner_build: &str,
) -> HolderUpgradeOutcome {
    if let Some(stop) = pre_gate(supported, in_flight) {
        return stop;
    }
    let seen = probe(pane_dir, pane, Instant::now() + PROBE_DEADLINE);
    let (previous_build, expected_pid) = match gate(supported, in_flight, &seen, runner_build) {
        Gate::Proceed {
            previous_build,
            expected_pid,
        } => (previous_build, expected_pid),
        Gate::Stop(outcome) => return outcome,
    };
    tracing::info!(pane = %pane, ?previous_build, "pty holder: operator-initiated in-place upgrade requested");
    let now = Instant::now();
    let result = upgrade_pane(
        pane_dir,
        pane,
        expected_pid,
        now + HANDSHAKE_DEADLINE,
        now + HANDSHAKE_DEADLINE + UPGRADE_DEADLINE,
    );
    let outcome = map_outcome(result, previous_build, |want| {
        read_back(pane_dir, pane, want)
    });
    tracing::info!(pane = %pane, ?outcome, "pty holder: in-place upgrade finished");
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::tracking_health::HolderPane;
    use qontinui_pty_holder::protocol::HelloAck;

    const RUNNER: &str = "qontinui-pty-holder 0.2.0";

    fn healthy(build: &str) -> Probe {
        Probe::Healthy {
            hello_ack: HelloAck {
                version: 3,
                holder_build: build.into(),
                holder_pid: 10,
                child_pid: Some(11),
            },
        }
    }

    /// Review p8f r1, #5: offered only where it can succeed (Linux); other
    /// Unix gets the reason, Windows the holder's own.
    #[test]
    fn pty_holder_upgrade_offered_only_on_linux() {
        assert!(upgrade_offered_on(true, true));
        assert!(!upgrade_offered_on(true, false), "macOS/BSD: drain refuses");
        assert!(!upgrade_offered_on(false, false), "Windows");
        assert_eq!(unsupported_reason_on(true, true), None);
        assert!(unsupported_reason_on(true, false)
            .unwrap()
            .contains("SHUT_RD"));
        assert!(unsupported_reason_on(false, false)
            .unwrap()
            .contains("Unix-only"));
        assert_eq!(upgrade_supported_here(), cfg!(target_os = "linux"));
    }

    #[test]
    fn pty_holder_upgrade_build_comparison() {
        assert!(older_build("qontinui-pty-holder 0.1.0", RUNNER));
        assert!(older_build("qontinui-pty-holder 0.1.9", "x 0.10.0"));
        assert!(!older_build(RUNNER, RUNNER), "same build");
        assert!(!older_build("qontinui-pty-holder 0.3.0", RUNNER), "newer");
        assert!(older_build("h 0.1.0-rc1", "h 0.1.1"));
        assert!(!older_build("garbage", RUNNER), "unparseable is no proof");
        assert!(!older_build("h 0.1", RUNNER), "two parts is no proof");
        assert!(!older_build("h 0.1.0", "unknown"));
    }

    #[test]
    fn pty_holder_upgrade_gate_unsupported_platform_never_probes() {
        // Unsupported wins over everything, even an eligible holder.
        match gate(
            false,
            false,
            &Probe::Incompatible {
                holder_versions: vec![9],
            },
            RUNNER,
        ) {
            Gate::Stop(HolderUpgradeOutcome::Unsupported { reason }) => {
                assert!(reason.contains("Unix-only"), "{reason}")
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn pty_holder_upgrade_gate_allows_incompatible_and_older_only() {
        assert_eq!(
            gate(
                true,
                false,
                &Probe::Incompatible {
                    holder_versions: vec![9]
                },
                RUNNER
            ),
            Gate::Proceed {
                previous_build: None,
                expected_pid: None,
            }
        );
        assert_eq!(
            gate(true, false, &healthy("qontinui-pty-holder 0.1.0"), RUNNER),
            Gate::Proceed {
                previous_build: Some("qontinui-pty-holder 0.1.0".into()),
                expected_pid: Some(10),
            }
        );
        let refused = |g: Gate| matches!(g, Gate::Stop(HolderUpgradeOutcome::Refused { .. }));
        assert!(
            refused(gate(true, false, &healthy(RUNNER), RUNNER)),
            "same build"
        );
        assert!(refused(gate(
            true,
            false,
            &healthy("qontinui-pty-holder 0.9.0"),
            RUNNER
        )));
        assert!(refused(gate(
            true,
            false,
            &Probe::Unknown {
                reason: "timed out".into(),
                record: None
            },
            RUNNER
        )));
        assert!(refused(gate(
            true,
            false,
            &Probe::Dead { record: None },
            RUNNER
        )));
        assert!(refused(gate(true, false, &Probe::Absent, RUNNER)));
        // In flight: refused before any probe result is considered.
        assert!(refused(gate(
            true,
            true,
            &Probe::Incompatible {
                holder_versions: vec![9]
            },
            RUNNER
        )));
    }

    fn sent(outcome: UpgradeOutcome) -> Result<PaneUpgrade, PaneUpgradeError> {
        Ok(PaneUpgrade {
            asked_holder_pid: Some(10),
            outcome,
        })
    }

    #[test]
    fn pty_holder_upgrade_outcome_mapping() {
        let up = map_outcome(
            sent(UpgradeOutcome::Upgraded {
                successor_pid: 20,
                handoff_offset: 512,
            }),
            Some("h 0.1.0".into()),
            |want| {
                assert_eq!(want, Some(20));
                Some((20, "h 0.2.0".into()))
            },
        );
        assert_eq!(
            up,
            HolderUpgradeOutcome::Upgraded {
                successor_pid: 20,
                handoff_offset: 512,
                previous_build: Some("h 0.1.0".into()),
                successor_build: Some("h 0.2.0".into()),
            }
        );
        // A read-back that met another holder is not the successor's build.
        let up = map_outcome(
            sent(UpgradeOutcome::Upgraded {
                successor_pid: 20,
                handoff_offset: 0,
            }),
            None,
            |_| Some((99, "h 0.2.0".into())),
        );
        assert!(matches!(
            up,
            HolderUpgradeOutcome::Upgraded {
                successor_build: None,
                ..
            }
        ));
        let no_read = |_: Option<u32>| -> Option<(u32, String)> { panic!("no read-back owed") };
        match map_outcome(sent(UpgradeOutcome::Rejected("busy".into())), None, no_read) {
            HolderUpgradeOutcome::Failed { reason } => {
                assert!(reason.starts_with("busy"), "{reason}");
                assert!(reason.contains("nothing changed"), "{reason}");
            }
            other => panic!("a rejection is not a rollback: {other:?}"),
        }
        assert_eq!(
            map_outcome(sent(UpgradeOutcome::Failed("nak".into())), None, no_read),
            HolderUpgradeOutcome::RolledBack {
                reason: "nak".into()
            }
        );
        assert_eq!(
            map_outcome(
                sent(UpgradeOutcome::Unsupported("win".into())),
                None,
                no_read
            ),
            HolderUpgradeOutcome::Unsupported {
                reason: "win".into()
            }
        );
        let io = || ConnectError::Io(std::io::Error::from(std::io::ErrorKind::TimedOut));
        assert!(matches!(
            map_outcome(Err(PaneUpgradeError::NotSent(io())), None, no_read),
            HolderUpgradeOutcome::Failed { .. }
        ));
        match map_outcome(Err(PaneUpgradeError::Unanswered(io())), None, |want| {
            assert_eq!(want, None);
            Some((33, "h".into()))
        }) {
            HolderUpgradeOutcome::UnknownOnTimeout {
                holder_pid_now,
                reason,
            } => {
                assert_eq!(holder_pid_now, Some(33));
                assert!(reason.contains("did not answer"), "{reason}");
                assert!(reason.contains("may have committed"), "{reason}");
            }
            other => panic!("{other:?}"),
        }
        // Not a timeout: still UNKNOWN, but the reason does not claim one.
        let garbled = PaneUpgradeError::Unanswered(ConnectError::Protocol("garbled".into()));
        match map_outcome(Err(garbled), None, |_| None) {
            HolderUpgradeOutcome::UnknownOnTimeout {
                reason,
                holder_pid_now,
            } => {
                assert!(reason.contains("could not be read"), "{reason}");
                assert!(!reason.contains("within"), "{reason}");
                assert_eq!(holder_pid_now, None);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn pty_holder_upgrade_outcome_wire_shape() {
        let v = serde_json::to_value(HolderUpgradeOutcome::UnknownOnTimeout {
            reason: "r".into(),
            holder_pid_now: Some(5),
        })
        .unwrap();
        assert_eq!(
            v,
            serde_json::json!({"kind": "unknown_on_timeout", "reason": "r", "holderPidNow": 5})
        );
        let v =
            serde_json::to_value(HolderUpgradeOutcome::RolledBack { reason: "x".into() }).unwrap();
        assert_eq!(v, serde_json::json!({"kind": "rolled_back", "reason": "x"}));
    }

    fn row(id: &str, state: HolderPaneState, build: Option<&str>) -> HolderPane {
        HolderPane {
            pane_id: id.into(),
            state,
            holder_pid: Some(1),
            child_pid: Some(2),
            holder_build: build.map(str::to_string),
            negotiated_version: None,
            holder_versions: vec![],
            detail: None,
            claude_pids: vec![],
        }
    }

    #[test]
    fn pty_holder_upgrade_attention_lists_only_panes_needing_a_decision() {
        let census = HolderCensus {
            panes: vec![
                row("incompat", HolderPaneState::Incompatible, None),
                row(
                    "old",
                    HolderPaneState::Live,
                    Some("qontinui-pty-holder 0.1.0"),
                ),
                row("current", HolderPaneState::Live, Some(RUNNER)),
                row("unknown", HolderPaneState::Unknown, None),
                row("dead", HolderPaneState::Dead, None),
            ],
            ..HolderCensus::default()
        };
        let a = attention_from_census(&census, RUNNER, true);
        let ids: Vec<_> = a
            .panes
            .iter()
            .map(|p| (p.pane_id.as_str(), p.reason))
            .collect();
        assert_eq!(
            ids,
            vec![
                ("incompat", AttentionReason::Incompatible),
                ("old", AttentionReason::OlderBuild)
            ]
        );
        assert!(a.upgrade_supported);
        assert_eq!(a.end_and_resume_available, END_AND_RESUME_AVAILABLE);
        let a = attention_from_census(
            &HolderCensus {
                error: Some("unreadable".into()),
                ..HolderCensus::default()
            },
            RUNNER,
            false,
        );
        assert_eq!(a.census_error.as_deref(), Some("unreadable"));
        assert!(!a.upgrade_supported);
    }

    /// The trigger against a REAL holder (plan Phase 8): a holder this runner
    /// treats as older is upgraded in place — the pane keeps its child and the
    /// outcome names the successor and its build; the same holder at this
    /// runner's own build is refused without being asked; and an unsupported
    /// platform answer never reaches it.
    #[cfg(unix)]
    mod real_holder {
        use super::*;
        use crate::terminal::daemon_pane_io::DaemonPaneIo;
        use crate::terminal::pane_io::{PaneIo, ScrubbedCommand};
        use std::path::PathBuf;
        use std::sync::Arc;

        fn holder() -> PathBuf {
            let exe = std::env::current_exe().unwrap();
            let profile = exe.parent().and_then(Path::parent).unwrap();
            let p = profile.join("qontinui-pty-holder");
            assert!(
                p.is_file(),
                "{} is missing — build it first: `cargo build -p qontinui-pty-holder`",
                p.display()
            );
            p
        }

        fn pane_dir(tag: &str) -> PathBuf {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos();
            PathBuf::from("/tmp").join(format!("ptyu-{tag}-{}-{nanos:x}", std::process::id()))
        }

        #[test]
        fn pty_holder_upgrade_trigger_against_a_real_holder() {
            let dir = pane_dir("trig");
            let pane_id = PaneId::new("trig").unwrap();
            let mut cmd = portable_pty::CommandBuilder::new("/bin/sh");
            cmd.args(["-c", "stty -echo; printf UP; exec sleep 3600"]);
            cmd.cwd("/tmp");
            let pane = Arc::new(
                DaemonPaneIo::spawn(
                    &holder(),
                    &dir,
                    &pane_id,
                    &ScrubbedCommand::seal(cmd),
                    80,
                    24,
                )
                .expect("spawn + attach"),
            );
            let before = pane.holder_pid();
            let child = pane.child_pid();
            let own = runner_holder_build();

            // Unsupported: answered without touching the holder.
            assert!(matches!(
                upgrade_blocking_with(&dir, &pane_id, false, false, &own),
                HolderUpgradeOutcome::Unsupported { .. }
            ));
            // Same build as the runner: refused, nothing sent.
            assert!(matches!(
                upgrade_blocking_with(&dir, &pane_id, false, true, &own),
                HolderUpgradeOutcome::Refused { .. }
            ));
            // In flight: refused.
            assert!(matches!(
                upgrade_blocking_with(&dir, &pane_id, true, true, "h 999.0.0"),
                HolderUpgradeOutcome::Refused { .. }
            ));
            assert_eq!(pane.holder_pid(), before, "nothing was upgraded yet");

            // A runner that ships a NEWER holder build: this one is older,
            // so it is upgraded — for real.
            let outcome = upgrade_blocking_with(&dir, &pane_id, false, true, "h 999.0.0");
            let HolderUpgradeOutcome::Upgraded {
                successor_pid,
                previous_build,
                successor_build,
                ..
            } = outcome
            else {
                panic!("expected upgraded, got {outcome:?}");
            };
            assert_ne!(successor_pid, before);
            assert_eq!(previous_build.as_deref(), Some(own.as_str()));
            // Started from the same executable: the same build, and the
            // outcome says so rather than claiming a newer one.
            assert_eq!(successor_build.as_deref(), Some(own.as_str()));
            let end = Instant::now() + Duration::from_secs(20);
            while pane.holder_pid() != successor_pid {
                assert!(Instant::now() < end, "the pane did not follow the upgrade");
                std::thread::sleep(Duration::from_millis(50));
            }
            assert_eq!(pane.child_pid(), child, "same child");
            pane.kill(Duration::from_secs(5)).unwrap();
            pane.release(Duration::from_secs(1)).unwrap();
            let _ = std::fs::remove_dir_all(&dir);
        }

        /// No holder at all: refused as not holder-backed.
        #[test]
        fn pty_holder_upgrade_trigger_refuses_a_pane_without_a_holder() {
            let dir = pane_dir("none");
            std::fs::create_dir_all(&dir).unwrap();
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            let outcome =
                upgrade_blocking_with(&dir, &PaneId::new("nope").unwrap(), false, true, "h 9.0.0");
            match outcome {
                HolderUpgradeOutcome::Refused { reason } => {
                    assert!(reason.contains("not holder-backed"), "{reason}")
                }
                other => panic!("{other:?}"),
            }
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
