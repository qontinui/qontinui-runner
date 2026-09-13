//! Graceful exit of a terminal-hosted `claude` — type `/exit`, wait for the
//! process to leave, only then close the tab (plan
//! `2026-09-13-drained-runner-never-reaches-idle`, design decision D5).
//!
//! ## Why not `close`
//!
//! Every close in the runner ends in `TerminalSession::close_with_deadline` →
//! `PaneIo::kill`, which is `taskkill /F /T` on Windows and a `SIGTERM` to the
//! single shell pid on Unix — a kill of a live agent that can drop its
//! transcript tail, and on Unix can orphan the `claude` child instead of
//! ending it. Wind-down must never do that to a live `claude`.
//!
//! ## The protocol
//!
//! 1. Probe the pane's process subtree. No `claude` there → [`NoLiveClaude`],
//!    and nothing is written (typing `/exit` into a bare shell is not a no-op).
//!    An unreadable process table → [`ProbeUnavailable`], nothing written.
//! 2. Write [`EXIT_COMMAND`] through `TerminalSession::write`, the single
//!    input funnel — which keeps the primitive correct under the proposed
//!    out-of-process PTY owner.
//! 3. Poll the subtree every [`POLL_INTERVAL`] until no `claude` is left, up
//!    to the deadline ([`DEFAULT_DEADLINE`]).
//! 4. Gone → close the tab, which now holds a bare shell → [`Exited`].
//!    Still there at the deadline → [`ExitStuck`], and the process is LEFT
//!    RUNNING. There is no escalation to a kill anywhere in this module; the
//!    tab-closing callback is reachable only from the "gone" arm, and the
//!    tests below pin that.
//!
//! ## Evidence for the load-bearing assumption
//!
//! Measured 2026-09-13 on a Linux box against Claude Code 2.1.270 (a pty
//! harness spawning `claude --dangerously-skip-permissions`, no prompt sent):
//! `/exit\r` written as ONE chunk after the input prompt rendered ended the
//! process with exit code 0 in 3 of 3 trials, 0.85–0.93 s after the write; the
//! same bytes split as `/exit`, 300 ms, `\r` also exited 3 of 3. An idle
//! session's pty then carried no bytes at all for 55 s. The trials were fresh
//! sessions with no transcript, so the 60 s deadline is sized for a long
//! transcript flush, not for the measured latency.
//!
//! [`NoLiveClaude`]: GracefulExitOutcome::NoLiveClaude
//! [`ProbeUnavailable`]: GracefulExitOutcome::ProbeUnavailable
//! [`Exited`]: GracefulExitOutcome::Exited
//! [`ExitStuck`]: GracefulExitOutcome::ExitStuck

use std::future::Future;
use std::time::Duration;

use serde::Serialize;

/// The bytes typed into the pane. One chunk: the measured evidence (module
/// docs) shows Claude Code submits it without a paste-window split.
pub const EXIT_COMMAND: &[u8] = b"/exit\r";

/// How long to wait for `claude` to leave before reporting [`GracefulExitOutcome::ExitStuck`].
pub const DEFAULT_DEADLINE: Duration = Duration::from_secs(60);

/// Spacing between process-table probes while waiting.
pub const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// One look at the pane's process subtree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaudeProbe {
    /// These `claude` pids are live in the subtree.
    Live(Vec<u32>),
    /// The table was read and holds no `claude` in the subtree.
    Gone,
    /// The table could not be read; says nothing either way.
    Unreadable(String),
}

/// What a graceful exit did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum GracefulExitOutcome {
    /// `claude` left within the deadline and the tab was closed.
    Exited {
        waited_ms: u64,
        /// The `claude` pids present when `/exit` was written.
        claude_pids: Vec<u32>,
    },
    /// `claude` was still there at the deadline. NOTHING was killed and the
    /// tab was not closed.
    ExitStuck {
        waited_ms: u64,
        /// The `claude` pids from the last readable probe.
        claude_pids: Vec<u32>,
        /// The final probe could not read the process table.
        last_probe_unreadable: bool,
    },
    /// No `claude` in the pane's subtree; nothing was written or closed.
    NoLiveClaude,
    /// The process table could not be read before starting; nothing was
    /// written or closed.
    ProbeUnavailable { detail: String },
    /// Writing `/exit` failed; nothing was closed.
    WriteFailed {
        error: String,
        claude_pids: Vec<u32>,
    },
}

/// Drive the protocol over injected effects. `write` types into the pane,
/// `probe` looks at its subtree, `close_tab` closes it; the pane itself is
/// never touched any other way.
pub async fn drive<W, P, PF, C, CF>(
    write: W,
    mut probe: P,
    close_tab: C,
    deadline: Duration,
    poll: Duration,
) -> GracefulExitOutcome
where
    W: FnOnce(&[u8]) -> Result<(), String>,
    P: FnMut() -> PF,
    PF: Future<Output = ClaudeProbe>,
    C: FnOnce() -> CF,
    CF: Future<Output = ()>,
{
    let initial = match probe().await {
        ClaudeProbe::Live(pids) => pids,
        ClaudeProbe::Gone => return GracefulExitOutcome::NoLiveClaude,
        ClaudeProbe::Unreadable(detail) => return GracefulExitOutcome::ProbeUnavailable { detail },
    };

    if let Err(error) = write(EXIT_COMMAND) {
        return GracefulExitOutcome::WriteFailed {
            error,
            claude_pids: initial,
        };
    }

    let started = tokio::time::Instant::now();
    let mut last_seen = initial.clone();
    let mut last_probe_unreadable = false;
    loop {
        let elapsed = started.elapsed();
        if elapsed >= deadline {
            return GracefulExitOutcome::ExitStuck {
                waited_ms: millis(elapsed),
                claude_pids: last_seen,
                last_probe_unreadable,
            };
        }
        tokio::time::sleep(poll.min(deadline - elapsed)).await;
        match probe().await {
            ClaudeProbe::Live(pids) => {
                last_seen = pids;
                last_probe_unreadable = false;
            }
            ClaudeProbe::Gone => {
                close_tab().await;
                return GracefulExitOutcome::Exited {
                    waited_ms: millis(started.elapsed()),
                    claude_pids: initial,
                };
            }
            ClaudeProbe::Unreadable(_) => last_probe_unreadable = true,
        }
    }
}

fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// Probe the live `claude` processes in the inclusive subtree of `root_pid`
/// (the pane's child process), with the same counting rules the restart
/// census uses (`process_tree::claude_pids_in_inclusive_subtree`).
pub async fn probe_claude_under(root_pid: Option<u32>) -> ClaudeProbe {
    let Some(root) = root_pid else {
        return ClaudeProbe::Unreadable(
            "the pane has no local process id (a remote pane), so its subtree cannot be observed"
                .to_string(),
        );
    };
    let snap = crate::process_capture::process_tree::snapshot_process_table_public().await;
    if snap.parent_map.is_empty() {
        return ClaudeProbe::Unreadable(
            "the process table is unreadable (empty parent map)".to_string(),
        );
    }
    let pids = crate::process_capture::process_tree::claude_pids_in_inclusive_subtree(root, &snap);
    if pids.is_empty() {
        ClaudeProbe::Gone
    } else {
        ClaudeProbe::Live(pids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    type Log = Arc<Mutex<Vec<String>>>;

    /// A probe that replays `script`, repeating its last entry forever.
    fn scripted_probe(
        script: Vec<ClaudeProbe>,
        log: Log,
    ) -> impl FnMut() -> std::future::Ready<ClaudeProbe> {
        let mut i = 0usize;
        move || {
            let p = script[i.min(script.len() - 1)].clone();
            i += 1;
            log.lock().unwrap().push(format!("probe:{p:?}"));
            std::future::ready(p)
        }
    }

    fn recording_write(log: Log) -> impl FnOnce(&[u8]) -> Result<(), String> {
        move |bytes| {
            log.lock()
                .unwrap()
                .push(format!("write:{}", String::from_utf8_lossy(bytes)));
            Ok(())
        }
    }

    fn recording_close(log: Log) -> impl FnOnce() -> std::future::Ready<()> {
        move || {
            log.lock().unwrap().push("close".to_string());
            std::future::ready(())
        }
    }

    fn entries(log: &Log) -> Vec<String> {
        log.lock().unwrap().clone()
    }

    #[tokio::test(start_paused = true)]
    async fn exits_then_closes_only_after_claude_is_gone() {
        let log: Log = Arc::default();
        let outcome = drive(
            recording_write(log.clone()),
            scripted_probe(
                vec![
                    ClaudeProbe::Live(vec![42]),
                    ClaudeProbe::Live(vec![42]),
                    ClaudeProbe::Gone,
                ],
                log.clone(),
            ),
            recording_close(log.clone()),
            DEFAULT_DEADLINE,
            POLL_INTERVAL,
        )
        .await;
        assert_eq!(
            outcome,
            GracefulExitOutcome::Exited {
                waited_ms: 1_000,
                claude_pids: vec![42]
            }
        );
        let log = entries(&log);
        assert_eq!(
            log,
            vec![
                "probe:Live([42])",
                "write:/exit\r",
                "probe:Live([42])",
                "probe:Gone",
                "close",
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_claude_that_outlives_the_deadline_is_left_running_and_the_tab_is_not_closed() {
        let log: Log = Arc::default();
        let outcome = drive(
            recording_write(log.clone()),
            scripted_probe(vec![ClaudeProbe::Live(vec![7, 8])], log.clone()),
            recording_close(log.clone()),
            Duration::from_secs(60),
            POLL_INTERVAL,
        )
        .await;
        assert_eq!(
            outcome,
            GracefulExitOutcome::ExitStuck {
                waited_ms: 60_000,
                claude_pids: vec![7, 8],
                last_probe_unreadable: false,
            }
        );
        let log = entries(&log);
        assert!(!log.iter().any(|e| e == "close"), "closed a live claude");
        assert_eq!(log.iter().filter(|e| e.starts_with("write")).count(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn an_unreadable_table_while_waiting_never_reads_as_gone() {
        let log: Log = Arc::default();
        let outcome = drive(
            recording_write(log.clone()),
            scripted_probe(
                vec![
                    ClaudeProbe::Live(vec![9]),
                    ClaudeProbe::Unreadable("boom".into()),
                ],
                log.clone(),
            ),
            recording_close(log.clone()),
            Duration::from_secs(5),
            POLL_INTERVAL,
        )
        .await;
        assert_eq!(
            outcome,
            GracefulExitOutcome::ExitStuck {
                waited_ms: 5_000,
                claude_pids: vec![9],
                last_probe_unreadable: true,
            }
        );
        assert!(!entries(&log).iter().any(|e| e == "close"));
    }

    #[tokio::test(start_paused = true)]
    async fn no_claude_means_nothing_is_written_or_closed() {
        let log: Log = Arc::default();
        let outcome = drive(
            recording_write(log.clone()),
            scripted_probe(vec![ClaudeProbe::Gone], log.clone()),
            recording_close(log.clone()),
            DEFAULT_DEADLINE,
            POLL_INTERVAL,
        )
        .await;
        assert_eq!(outcome, GracefulExitOutcome::NoLiveClaude);
        assert_eq!(entries(&log), vec!["probe:Gone"]);
    }

    #[tokio::test(start_paused = true)]
    async fn an_unreadable_table_up_front_writes_nothing() {
        let log: Log = Arc::default();
        let outcome = drive(
            recording_write(log.clone()),
            scripted_probe(vec![ClaudeProbe::Unreadable("x".into())], log.clone()),
            recording_close(log.clone()),
            DEFAULT_DEADLINE,
            POLL_INTERVAL,
        )
        .await;
        assert_eq!(
            outcome,
            GracefulExitOutcome::ProbeUnavailable {
                detail: "x".to_string()
            }
        );
        assert_eq!(entries(&log).len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_write_closes_nothing_and_stops_probing() {
        let log: Log = Arc::default();
        let outcome = drive(
            |_bytes: &[u8]| Err("terminal exited".to_string()),
            scripted_probe(vec![ClaudeProbe::Live(vec![3])], log.clone()),
            recording_close(log.clone()),
            DEFAULT_DEADLINE,
            POLL_INTERVAL,
        )
        .await;
        assert_eq!(
            outcome,
            GracefulExitOutcome::WriteFailed {
                error: "terminal exited".to_string(),
                claude_pids: vec![3]
            }
        );
        assert_eq!(entries(&log), vec!["probe:Live([3])"]);
    }

    #[test]
    fn outcome_wire_shape() {
        let json = serde_json::to_value(GracefulExitOutcome::ExitStuck {
            waited_ms: 60_000,
            claude_pids: vec![1],
            last_probe_unreadable: false,
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "outcome": "exit_stuck", "waited_ms": 60000,
                "claude_pids": [1], "last_probe_unreadable": false
            })
        );
    }

    /// Structural pin on the invariant: this module never names a kill path.
    /// `drive` can only reach the pane through the three injected effects, and
    /// the production caller's `close_tab` is the ordinary tab close — so a
    /// kill of a live `claude` would have to be introduced HERE first.
    #[test]
    fn this_module_contains_no_kill_path() {
        let source = include_str!("graceful_exit.rs");
        // Code only: the module docs deliberately NAME the kill path they
        // explain the primitive avoids.
        let production: String = source
            .split("#[cfg(test)]")
            .next()
            .expect("module has a production section")
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        for forbidden in [
            ".kill(",
            "close_with_deadline",
            "taskkill",
            "SIGKILL",
            "SIGTERM",
        ] {
            assert!(
                !production.contains(forbidden),
                "graceful_exit production code must not contain `{forbidden}`"
            );
        }
    }
}
