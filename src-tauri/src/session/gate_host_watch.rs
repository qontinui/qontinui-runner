//! The close observer's live-host check and pid-exit watcher (plan
//! `2026-10-07-a-resumed-gate-continuation-loses-its-reporter-on-…` D4).
//!
//! ## The defect it closes (Why §3, Gap A)
//!
//! A boot restore that finds a gate continuation's Claude process STILL
//! RUNNING skips it ("skip-alive"). The durable record keeps pointing at the
//! dead terminal id (or is rebound to a bare `terminal-only` shell), so the
//! liveness poll closes it non-restorably after ~2¼ min, and the D2 close
//! observer then posted `exit=crashed` for a session whose agent was alive.
//! The live process's own later `work_completed` was refused (coord admits one
//! `work_*` per continuation), and once coord#2729 lands `crashed` is
//! re-delivered — duplicate work beside the still-running original.
//!
//! ## What it does instead
//!
//! Before the close observer posts, [`report_on_close`] resolves the session's
//! live hosts: Claude Code's own registry rows for the session id
//! ([`claude_session_registry::read_live_sessions`], PID-filtered by
//! [`claude_session_registry::live_pids_from_snapshot`]) whose process still
//! passes the PID-reuse guard ([`host_liveness`]).
//!
//! - **No live host** — post the D2 report (`exit=crashed`) at once, exactly as
//!   runner#1984 does.
//! - **A live host** — post nothing; watch the host pid(s) every
//!   [`WATCH_INTERVAL`]. When every host has exited, post `exit=unknown …
//!   close=<close>+skip-alive-exit`. `unknown`, not `crashed`: the process
//!   outlived the runner crash and ended on its own, and a clean finish that
//!   forgot to report is as likely as a crash. If the process itself posted
//!   `work_completed` first, coord refuses this later post (one log line).
//!
//! [`claude_session_registry::find_live_session_by_id`] is deliberately NOT the
//! lookup: it skips the PID filter by design (it serves a startup path that
//! must not take a process snapshot), so it can return a stale file from a
//! crashed process — and when several files name one id it keeps one by NAME
//! precedence, which can hide the live row behind a dead one. The PID-filtered
//! listing filtered by session id is the same registry read with the filter
//! the plan asks for.
//!
//! ## PID reuse
//!
//! A host is identified by `(pid, LiveClaudeSession.started_at)`. A pid whose
//! OS creation time is LATER than the recorded `started_at` (beyond
//! [`PID_REUSE_SKEW_MS`]) is a different process that inherited the number:
//! - at close time, a registry file left by a crashed Claude on a reused pid
//!   is not a live host, so the report is `crashed` at once rather than a false
//!   `skip-alive-exit` posted whenever the unrelated process ends;
//! - while watching, a reused pid means the host is gone, so its exit is
//!   reported instead of being masked forever.
//!
//! ## Residue
//!
//! The watcher is in-memory. If the runner restarts again before the host
//! exits, the record is already closed and nothing re-arms it; coord's
//! backstop retires the gate. Every post claims
//! [`SessionLifecycleStore::claim_gate_report`] first, so a gate is reported
//! at most once per process generation.

use std::future::Future;
use std::sync::Weak;
use std::time::Duration;

use tracing::{info, warn};

use crate::process_capture::process_tree::{ProcessSnapshot, PID_REUSE_SKEW_MS};
use crate::session::claude_session_registry::LiveClaudeSession;
use crate::session::session_lifecycle_store::{
    runner_restart_detail, RunnerRestartUnreported, SessionLifecycleStore, TerminalSessionRecord,
};

/// How often a skip-alive watcher re-probes its host pid(s).
pub const WATCH_INTERVAL: Duration = Duration::from_secs(10);

/// How many consecutive indeterminate probes (an empty process table) the
/// close-time host lookup tolerates before falling back to runner#1984's
/// behaviour — posting the `crashed` report — rather than holding the report
/// on a read that cannot answer.
pub const INDETERMINATE_LOOKUP_ATTEMPTS: usize = 3;

/// Suffix appended to the close reason of a report posted when a skip-alive
/// host exits.
pub const SKIP_ALIVE_EXIT_SUFFIX: &str = "+skip-alive-exit";

/// One Claude process hosting the session, identified so a reused pid cannot
/// impersonate it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveHost {
    pub pid: u32,
    /// `LiveClaudeSession.started_at` (epoch millis, Claude's own stamp). `0`
    /// when the registry file carried none — the reuse guard is then skipped.
    pub started_at: i64,
}

/// One probe of the world: a process-table snapshot plus the PID-filtered
/// live-registry rows read against it.
#[derive(Debug, Default)]
pub struct HostProbe {
    pub snapshot: ProcessSnapshot,
    pub live_rows: Vec<LiveClaudeSession>,
}

/// Is this host still the process that registered it?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostLiveness {
    Alive,
    Exited,
    /// The snapshot failed (an empty process table — the runner itself is
    /// always live, so emptiness is a failed read). Never read as an exit.
    Unknown,
}

/// Pure liveness verdict for one host against one snapshot, with the PID-reuse
/// guard on `started_at`.
pub fn host_liveness(host: &LiveHost, snapshot: &ProcessSnapshot) -> HostLiveness {
    if snapshot.names.is_empty() {
        return HostLiveness::Unknown;
    }
    if !snapshot.names.contains_key(&host.pid) {
        return HostLiveness::Exited;
    }
    if let Some(&created_secs) = snapshot.creation_times.get(&host.pid) {
        // `creation_times` is epoch SECONDS (WMI granularity); Claude stamps
        // `startedAt` after its own process started, so a creation time later
        // than it is a different process holding the same pid.
        if created_secs > 0
            && host.started_at > 0
            && created_secs * 1000 > host.started_at + PID_REUSE_SKEW_MS
        {
            return HostLiveness::Exited;
        }
    }
    HostLiveness::Alive
}

/// The live hosts of `claude_session_id`: registry rows naming it whose
/// process passes [`host_liveness`]. `live_rows` must already be PID-filtered
/// ([`crate::session::claude_session_registry::read_live_sessions`]); the
/// liveness check additionally drops a stale file on a REUSED pid.
pub fn live_hosts(
    claude_session_id: &str,
    live_rows: &[LiveClaudeSession],
    snapshot: &ProcessSnapshot,
) -> Vec<LiveHost> {
    let mut out: Vec<LiveHost> = live_rows
        .iter()
        .filter(|r| r.session_id == claude_session_id)
        .map(|r| LiveHost {
            pid: r.pid,
            started_at: r.started_at,
        })
        .filter(|h| host_liveness(h, snapshot) == HostLiveness::Alive)
        .collect();
    out.sort_by_key(|h| h.pid);
    out.dedup();
    out
}

/// `Some(true)` when EVERY host has exited, `Some(false)` while any is alive,
/// `None` when the snapshot cannot say (never read as an exit).
pub fn hosts_exited(hosts: &[LiveHost], snapshot: &ProcessSnapshot) -> Option<bool> {
    let mut all_exited = true;
    for h in hosts {
        match host_liveness(h, snapshot) {
            HostLiveness::Unknown => return None,
            HostLiveness::Alive => all_exited = false,
            HostLiveness::Exited => {}
        }
    }
    Some(all_exited)
}

/// What a skip-alive watcher posts when its hosts exit: the gate identity of
/// the deferred D2 report, plus the record facts the new detail needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkipAliveWatch {
    pub hosts: Vec<LiveHost>,
    pub gate_id: uuid::Uuid,
    pub consuming_device_id: uuid::Uuid,
    pub terminal_id: String,
    pub opened_at: i64,
    /// The record's original (non-restorable) close reason.
    pub close: String,
}

impl SkipAliveWatch {
    /// The report posted once the hosts have exited, at `now_ms`.
    pub fn exit_report(&self, now_ms: i64) -> RunnerRestartUnreported {
        let after_s = (now_ms - self.opened_at).max(0) / 1000;
        RunnerRestartUnreported {
            gate_id: self.gate_id,
            consuming_device_id: self.consuming_device_id,
            detail: runner_restart_detail(
                "unknown",
                after_s,
                &self.terminal_id,
                &format!("{}{SKIP_ALIVE_EXIT_SUFFIX}", self.close),
            ),
        }
    }
}

/// The just-closed record's facts a deferred report needs, captured
/// SYNCHRONOUSLY by the close observer when it decides to report. A watch can
/// last hours; re-reading the row after it would pick up a later reopen or
/// re-close (a different `close=` tag), or lose the report when the row is
/// gone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClosedRecord {
    pub terminal_id: String,
    pub opened_at: i64,
    /// The record's (non-restorable) close reason.
    pub close: String,
}

impl ClosedRecord {
    pub fn of(rec: &TerminalSessionRecord) -> Self {
        Self {
            terminal_id: rec.terminal_id.clone(),
            opened_at: rec.opened_at,
            close: rec.close_reason.clone().unwrap_or_default(),
        }
    }
}

/// The production probe: one process-table snapshot and the live-registry rows
/// PID-filtered against it. An indeterminate snapshot yields no rows and an
/// empty table, which every consumer reads as UNKNOWN.
pub async fn production_probe() -> HostProbe {
    let snapshot = crate::process_capture::process_tree::snapshot_process_table_public().await;
    let live_rows =
        match crate::session::claude_session_registry::live_pids_from_snapshot(&snapshot) {
            Ok(pids) => crate::session::claude_session_registry::read_live_sessions(
                &crate::terminal::transcript::find_claude_config_dirs(),
                &pids,
            ),
            Err(_) => Vec::new(),
        };
    HostProbe {
        snapshot,
        live_rows,
    }
}

/// The close observer's D2 post, gated on D4's live-host check.
///
/// `report` is the already-decided D2 report for the just-closed record
/// ([`crate::session::session_lifecycle_store::runner_restart_report_on_close`]),
/// and `closed` that record's facts, captured at the same moment.
/// `probe` and `post` are injected so the whole path — lookup, watch, claim,
/// post — is testable without a real process table or coord; production
/// passes [`production_probe`] and `agent_runtime::post_runner_restart_unreported`.
///
/// Every post first claims [`SessionLifecycleStore::claim_gate_report`] for
/// `current_generation` and the report's gate, so a gate is posted at most
/// once per generation whichever producer gets there first, and never after
/// the row was re-adopted by this generation or re-bound to another gate
/// while the watch ran. A dropped store (the runner shutting
/// down) ends a watch without posting.
pub async fn report_on_close<P, PF, S, SF>(
    store: Weak<SessionLifecycleStore>,
    claude_session_id: String,
    current_generation: String,
    report: RunnerRestartUnreported,
    closed: ClosedRecord,
    mut probe: P,
    mut post: S,
    interval: Duration,
) where
    P: FnMut() -> PF,
    PF: Future<Output = HostProbe>,
    S: FnMut(RunnerRestartUnreported) -> SF,
    SF: Future<Output = ()>,
{
    let gate_id = report.gate_id;
    let claim = |store: &Weak<SessionLifecycleStore>| {
        store
            .upgrade()
            .is_some_and(|s| s.claim_gate_report(&claude_session_id, &current_generation, &gate_id))
    };

    // 1. Who, if anyone, still hosts the session?
    let mut hosts: Option<Vec<LiveHost>> = None;
    for attempt in 0..INDETERMINATE_LOOKUP_ATTEMPTS {
        if attempt > 0 {
            tokio::time::sleep(interval).await;
        }
        let p = probe().await;
        if p.snapshot.names.is_empty() {
            continue;
        }
        hosts = Some(live_hosts(&claude_session_id, &p.live_rows, &p.snapshot));
        break;
    }
    let hosts = hosts.unwrap_or_else(|| {
        warn!(
            claude_session = %claude_session_id,
            "continuation close: live-host lookup indeterminate — reporting as runner#1984 does"
        );
        Vec::new()
    });

    // 2. Nobody: the continuation really is gone. Post the D2 report now.
    if hosts.is_empty() {
        if claim(&store) {
            post(report).await;
        }
        return;
    }

    // 3. A live host: defer to its exit.
    let watch = SkipAliveWatch {
        hosts,
        gate_id: report.gate_id,
        consuming_device_id: report.consuming_device_id,
        terminal_id: closed.terminal_id,
        opened_at: closed.opened_at,
        close: closed.close,
    };
    info!(
        claude_session = %claude_session_id,
        gate_id = %watch.gate_id,
        pids = ?watch.hosts.iter().map(|h| h.pid).collect::<Vec<_>>(),
        "continuation close: a live process still hosts the session — work_unreported deferred until it exits"
    );
    loop {
        tokio::time::sleep(interval).await;
        if store.strong_count() == 0 {
            return;
        }
        let p = probe().await;
        if hosts_exited(&watch.hosts, &p.snapshot) == Some(true) {
            break;
        }
    }
    if claim(&store) {
        let exit = watch.exit_report(chrono::Utc::now().timestamp_millis());
        info!(
            claude_session = %claude_session_id,
            gate_id = %exit.gate_id,
            detail = %exit.detail,
            "continuation close: skip-alive host exited — posting work_unreported"
        );
        post(exit).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::past_sessions::PastSessionAccount;
    use crate::session::session_lifecycle_store::{
        runner_restart_report_on_close, TerminalSessionRecord,
    };
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    const GATE: &str = "7252781b-c49c-4a83-bdc2-4fec9c3dcd91";
    const DEVICE: &str = "eb2155ed-1111-4222-8333-444455556666";
    const CSID: &str = "44108327-98b7-4642-8f57-72e38fe8d723";
    const TERMINAL: &str = "3f08601c-8fc8-4b1d-835d-c65bab42b85c";
    const PRIOR: &str = "gen-prior";
    const CUR: &str = "gen-current";
    const PID: u32 = 4242;
    /// Claude's own `startedAt` for the host (epoch millis).
    const STARTED_AT: i64 = 1_784_712_055_852;
    const TICK: Duration = Duration::from_secs(10);

    fn gate_record() -> TerminalSessionRecord {
        serde_json::from_value(serde_json::json!({
            "claudeSessionId": CSID,
            "workingDir": "/repo",
            "pageId": "default",
            "zoneIndex": 0,
            "title": "Continuation",
            "terminalId": TERMINAL,
            "openedAt": 1000,
            "lastSeenAt": 61000,
            "state": "open",
            "gateId": GATE,
            "gateConsumingDeviceId": DEVICE,
            "gateBoundGeneration": PRIOR,
        }))
        .expect("gate record deserializes")
    }

    fn row(session_id: &str, pid: u32, started_at: i64) -> LiveClaudeSession {
        LiveClaudeSession {
            session_id: session_id.to_string(),
            name: "continuation".to_string(),
            name_source: None,
            pid,
            account: PastSessionAccount {
                label: "gmail".to_string(),
                wrapper: "clg".to_string(),
            },
            working_dir: "/repo".to_string(),
            status: "busy".to_string(),
            kind: "interactive".to_string(),
            started_at,
            updated_at: started_at,
            resume_command: String::new(),
        }
    }

    /// A healthy snapshot (the runner itself, pid 1) plus `pid` created at
    /// `created_secs` when given.
    fn snap(pid: Option<(u32, i64)>) -> ProcessSnapshot {
        let mut s = ProcessSnapshot::default();
        s.names.insert(1, "qontinui-runner".to_string());
        if let Some((p, created)) = pid {
            s.names.insert(p, "claude".to_string());
            s.creation_times.insert(p, created);
        }
        s
    }

    /// The host process, created a second before it stamped `startedAt`.
    fn host_alive() -> HostProbe {
        HostProbe {
            snapshot: snap(Some((PID, STARTED_AT / 1000 - 1))),
            live_rows: vec![row(CSID, PID, STARTED_AT)],
        }
    }

    fn host_gone() -> HostProbe {
        HostProbe {
            snapshot: snap(None),
            live_rows: vec![],
        }
    }

    /// The same pid now held by a process created a minute AFTER the host
    /// stamped `startedAt` — a reused pid. Its registry row (a stale file left
    /// by the crashed host) still names the session.
    fn pid_reused() -> HostProbe {
        HostProbe {
            snapshot: snap(Some((PID, STARTED_AT / 1000 + 60))),
            live_rows: vec![row(CSID, PID, STARTED_AT)],
        }
    }

    fn indeterminate() -> HostProbe {
        HostProbe::default()
    }

    struct Harness {
        _dir: tempfile::TempDir,
        store: Arc<SessionLifecycleStore>,
        report: RunnerRestartUnreported,
        closed: ClosedRecord,
    }

    /// A prior-generation gate record closed non-restorably, and its D2
    /// report decided the way `main.rs`'s close observer decides it.
    fn closed_gate(reason: &str) -> Harness {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(
            SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap(),
        );
        store.record_open(gate_record());
        store.record_close(CSID, reason);
        let report = runner_restart_report_on_close(&Arc::downgrade(&store), CSID, CUR)
            .expect("a prior-generation non-restorable close reports");
        let closed = ClosedRecord::of(&store.get(CSID).unwrap());
        Harness {
            _dir: dir,
            store,
            report,
            closed,
        }
    }

    /// Run [`report_on_close`] against a scripted probe sequence. Returns each
    /// post with the number of probes taken when it was posted.
    async fn run(h: &Harness, script: Vec<HostProbe>) -> Vec<(usize, RunnerRestartUnreported)> {
        let total = script.len();
        let script = Arc::new(Mutex::new(VecDeque::from(script)));
        let taken = Arc::new(Mutex::new(0usize));
        let posts = Arc::new(Mutex::new(Vec::new()));
        {
            let script = script.clone();
            let taken_p = taken.clone();
            let taken_s = taken.clone();
            let posts = posts.clone();
            report_on_close(
                Arc::downgrade(&h.store),
                CSID.to_string(),
                CUR.to_string(),
                h.report.clone(),
                h.closed.clone(),
                move || {
                    *taken_p.lock().unwrap() += 1;
                    let next = script
                        .lock()
                        .unwrap()
                        .pop_front()
                        .expect("probe script exhausted — the watcher kept probing");
                    async move { next }
                },
                move |r| {
                    posts.lock().unwrap().push((*taken_s.lock().unwrap(), r));
                    async {}
                },
                TICK,
            )
            .await;
        }
        assert!(
            script.lock().unwrap().is_empty(),
            "the watcher stopped before the script ran out ({} of {total} probes)",
            *taken.lock().unwrap()
        );
        let out = posts.lock().unwrap().clone();
        out
    }

    // (g) A live host defers the report until it exits, then posts once.
    #[tokio::test(start_paused = true)]
    async fn skip_alive_host_posts_nothing_while_alive_and_unknown_once_on_exit() {
        let h = closed_gate("no-terminal");
        let posts = run(
            &h,
            vec![host_alive(), host_alive(), host_alive(), host_gone()],
        )
        .await;
        assert_eq!(posts.len(), 1, "exactly one post: {posts:?}");
        let (probes_at_post, report) = &posts[0];
        assert_eq!(*probes_at_post, 4, "nothing posted while the pid lived");
        assert_eq!(report.gate_id.to_string(), GATE);
        assert_eq!(report.consuming_device_id.to_string(), DEVICE);
        assert!(
            report.detail.starts_with("exit=unknown code=none after="),
            "{}",
            report.detail
        );
        assert!(
            report.detail.ends_with(&format!(
                "terminal={TERMINAL} reason=runner_restart close=no-terminal+skip-alive-exit"
            )),
            "{}",
            report.detail
        );
        assert_eq!(
            h.store
                .get(CSID)
                .unwrap()
                .gate_reported_generation
                .as_deref(),
            Some(CUR),
            "the post is claimed for this generation"
        );
        // At most once per generation: a second close-path run (any producer)
        // finds the claim and posts nothing.
        let again = run(&h, vec![host_gone()]).await;
        assert!(again.is_empty(), "{again:?}");
    }

    // (g) An indeterminate snapshot while watching is never read as an exit.
    #[tokio::test(start_paused = true)]
    async fn an_indeterminate_tick_does_not_post_a_false_exit() {
        let h = closed_gate("terminal-only-idle");
        let posts = run(
            &h,
            vec![host_alive(), indeterminate(), indeterminate(), host_gone()],
        )
        .await;
        assert_eq!(posts.len(), 1, "{posts:?}");
        assert_eq!(posts[0].0, 4, "the indeterminate ticks posted nothing");
        assert!(posts[0]
            .1
            .detail
            .ends_with("close=terminal-only-idle+skip-alive-exit"));
    }

    // (g) PID reuse at close time: a stale registry file on a pid now held by
    // a DIFFERENT process (same pid, creation time after the host's
    // `startedAt`) is not a live host. The report is the D2 `crashed` one,
    // posted at once — never a false `skip-alive-exit` posted whenever the
    // unrelated process happens to end.
    #[tokio::test(start_paused = true)]
    async fn a_reused_pid_at_close_is_not_a_live_host() {
        let h = closed_gate("no-terminal");
        let posts = run(&h, vec![pid_reused()]).await;
        assert_eq!(posts.len(), 1, "{posts:?}");
        assert!(
            posts[0].1.detail.starts_with("exit=crashed "),
            "{}",
            posts[0].1.detail
        );
        assert!(!posts[0].1.detail.contains("skip-alive"));
    }

    // (g) PID reuse while watching: the host is gone even though its pid is
    // live again, so its exit is reported rather than masked forever.
    #[tokio::test(start_paused = true)]
    async fn a_pid_reused_while_watching_is_the_hosts_exit() {
        let h = closed_gate("no-terminal");
        let posts = run(&h, vec![host_alive(), host_alive(), pid_reused()]).await;
        assert_eq!(posts.len(), 1, "{posts:?}");
        assert_eq!(posts[0].0, 3);
        assert!(posts[0]
            .1
            .detail
            .ends_with("close=no-terminal+skip-alive-exit"));
    }

    // (g2) No live host: the D2 report is posted at once, unchanged from
    // runner#1984.
    #[tokio::test(start_paused = true)]
    async fn no_live_host_posts_crashed_exactly_as_before() {
        let h = closed_gate("no-terminal");
        let expected = h.report.clone();
        assert!(expected
            .detail
            .starts_with("exit=crashed code=none after=60s"));
        // A registry row for a DIFFERENT session on a live pid is not a host.
        let other = HostProbe {
            snapshot: snap(Some((PID, STARTED_AT / 1000 - 1))),
            live_rows: vec![row("some-other-session", PID, STARTED_AT)],
        };
        let posts = run(&h, vec![other]).await;
        assert_eq!(posts, vec![(1, expected)]);
        assert_eq!(
            h.store
                .get(CSID)
                .unwrap()
                .gate_reported_generation
                .as_deref(),
            Some(CUR)
        );
    }

    // (g2) A lookup that never becomes determinate falls back to #1984's post
    // rather than holding the report forever.
    #[tokio::test(start_paused = true)]
    async fn an_indeterminate_lookup_falls_back_to_the_crashed_post() {
        let h = closed_gate("no-terminal");
        let expected = h.report.clone();
        let script = (0..INDETERMINATE_LOOKUP_ATTEMPTS)
            .map(|_| indeterminate())
            .collect();
        let posts = run(&h, script).await;
        assert_eq!(posts, vec![(INDETERMINATE_LOOKUP_ATTEMPTS, expected)]);
    }

    #[test]
    fn host_liveness_reads_reuse_and_skew() {
        let host = LiveHost {
            pid: PID,
            started_at: STARTED_AT,
        };
        // Created within the WMI rounding skew AFTER startedAt: still the host.
        let rounded = snap(Some((PID, (STARTED_AT + PID_REUSE_SKEW_MS) / 1000)));
        assert_eq!(host_liveness(&host, &rounded), HostLiveness::Alive);
        // Past the skew: a reused pid.
        assert_eq!(
            host_liveness(&host, &pid_reused().snapshot),
            HostLiveness::Exited
        );
        // No creation time known: the pid's presence decides.
        let mut no_ctime = snap(None);
        no_ctime.names.insert(PID, "claude".to_string());
        assert_eq!(host_liveness(&host, &no_ctime), HostLiveness::Alive);
        assert_eq!(host_liveness(&host, &snap(None)), HostLiveness::Exited);
        assert_eq!(
            host_liveness(&host, &ProcessSnapshot::default()),
            HostLiveness::Unknown
        );
        assert_eq!(hosts_exited(&[host], &ProcessSnapshot::default()), None);
    }
}
