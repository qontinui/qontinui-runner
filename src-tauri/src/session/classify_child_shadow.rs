//! `classify_child` in SHADOW: what the `finished_close` arm WOULD admit or
//! refuse if served `finished-criteria` condition 2's shim exemption were armed
//! (plan `2026-11-04-runner-finished-close-closes-nothing-on-any-box`, Phase 2a).
//!
//! **This module decides nothing.** It runs beside the arm's census and logs:
//! one `classify_child would admit|refuse (<classes>)` line per FINISHED
//! terminal-hosted session whose verdict changed, and ONE `classify_child pass —
//! N finished, A would admit, R would refuse (refuse classes: …)` tally per
//! pass, so a verdict that never changes still leaves evidence after the log
//! rotates. Eligibility, custody and `graceful_exit`'s any-child refusal are
//! untouched; arming the exemption is Phase 2b, after this log has shown correct
//! verdicts on a real runner.
//!
//! It looks at finished sessions whatever their eligibility verdict, because
//! the sessions the exemption exists for are exactly the ones eligibility
//! rejects today (`live_children`, the shim's `cmd.exe` on Windows).
//!
//! The reads follow the clause's binding rule: a fresh process table, then the
//! RAW argv of every member of every finished session's child subtrees, then a
//! SECOND fresh table every member is re-confirmed against (same pid, creation
//! time, parent and image). Windows reads each PEB's command line, never
//! `sysinfo`'s rejoined argv, which loses cmd's `^"` escapes and so could never
//! match the measured `cmd.exe /d /s /c` chain. Linux reads `/proc/<pid>/cmdline`
//! split on NUL with every element kept, never the space-joined line, in which
//! distinct argvs collapse into one; and both of its tables stamp a row's
//! creation with `/proc/<pid>/stat`'s clock ticks, not the seconds derived from
//! them. The pure judgement is [`session_lifecycle::classify_session`].
//!
//! **It touches no shared read state.** Its tables come from
//! [`process_tree::snapshot_process_table_unshared`]: a private in-process walk
//! on the blocking pool, bounded by [`WALK_BUDGET`], entirely outside the
//! process-table provider every other read goes through. It leads no walk
//! another caller could join, reads and writes no memo, never takes the single
//! external-probe (PowerShell/WMI) fallback slot and moves no counter or
//! circuit — so no census read, and no other caller's, can be served a shadow
//! walk's failure, delayed by it, or lose the fallback to it, and a failed
//! shadow walk leaves no trace. The argv read is in-process too, with no
//! fallback. With the in-process read switched off
//! (`QONTINUI_PROCESS_TABLE_IN_PROCESS=0`) every read the shadow could make is
//! UNKNOWN, so no pass runs.
//!
//! It runs on its own task, spawned from the census and never awaited by it, so
//! the arm's candidates, custody probes and closes act on the census exactly as
//! fresh as before. It runs only where the arm runs a census at all (`on` or
//! `shadow`); a box with the arm `off` produces no shadow evidence.
//!
//! **One pass at a time, and no pass while an earlier one's reads still run.**
//! A read's budget bounds the pass's wait, not the read: a walk or argv read
//! past its budget keeps its blocking-pool thread until the OS-level read
//! returns (a `/proc/<pid>/cmdline` read can block behind a process in
//! uninterruptible sleep). Each read therefore carries a [`LiveRead`] guard
//! INSIDE its blocking closure, dropped only when that read returns, and a
//! census starts no pass while any is still alive — so a hung read costs at
//! most the reads of the one pass that met it, never one more per census.
//! A pass whose first table is not a fresh read makes no further read at all:
//! every verdict would be UNKNOWN whatever the argv read and second walk found.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use session_lifecycle::{
    classify_session, Argv, ChildClass, Platform, ProcRow, ProcTable, SessionVerdict,
};
use tracing::{debug, info};

use crate::process_capture::process_tree::{self, ProcessSnapshot};
use crate::session::tracking_health::{LiveClaudeProcess, SessionWorkStatus};

/// Each of a pass's two private table walks is bounded on its own; a walk past
/// it reads stale (UNKNOWN) rather than spending the whole pass.
const WALK_BUDGET: Duration = Duration::from_secs(4);

/// The argv read's own bound; past it the argv read is UNKNOWN.
const ARGV_BUDGET: Duration = Duration::from_secs(2);

/// The whole shadow pass is bounded too — its three reads' budgets and a
/// second for the judgement — so nothing in it can pin the single-flight flag
/// and silence every later census.
const SHADOW_BUDGET: Duration = WALK_BUDGET
    .saturating_mul(2)
    .saturating_add(ARGV_BUDGET)
    .saturating_add(Duration::from_secs(1));

/// Set while a shadow pass runs; a census that finds it set starts none.
static IN_FLIGHT: AtomicBool = AtomicBool::new(false);

/// The shadow's blocking reads still running on the pool, by [`LiveRead`].
static LIVE_READS: LiveReads = LiveReads::new();

/// Set once a census has logged that live reads kept it from starting a pass;
/// cleared when a pass starts, so each stall is announced once.
static STALL_SAID: AtomicBool = AtomicBool::new(false);

/// A count of blocking reads still running, kept by the [`LiveRead`] guards
/// those reads hold.
struct LiveReads(AtomicUsize);

impl LiveReads {
    const fn new() -> Self {
        LiveReads(AtomicUsize::new(0))
    }

    /// Count one read in. The guard goes INTO the read's blocking closure (a
    /// read door's `hold`), so it is dropped — and the read counted out — only
    /// when the OS-level read returns, or when the closure is dropped unrun.
    fn enter(&'static self) -> LiveRead {
        self.0.fetch_add(1, Ordering::AcqRel);
        LiveRead(self)
    }

    fn count(&self) -> usize {
        self.0.load(Ordering::Acquire)
    }
}

/// One blocking read, counted in [`LiveReads`] until it is dropped.
struct LiveRead(&'static LiveReads);

impl Drop for LiveRead {
    fn drop(&mut self) {
        self.0 .0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Subtree members gathered per session; past this the session is skipped as
/// UNKNOWN (a real shim subtree has at most four members).
const MAX_MEMBERS_PER_SESSION: usize = 256;

/// PURE: the finished, terminal-hosted, top-level `claude`s worth a verdict:
/// `(claude_session_id, pid)`.
pub fn finished_sessions(processes: &[LiveClaudeProcess]) -> Vec<(String, u32)> {
    let finished = SessionWorkStatus::Finished.as_wire();
    processes
        .iter()
        .filter(|p| !p.nested_under_claude)
        .filter(|p| p.session_status.as_deref() == Some(finished.as_str()))
        .filter_map(|p| p.session_id.clone().map(|id| (id, p.pid)))
        .collect()
}

/// PURE: one [`ProcessSnapshot`] as the kernel's [`ProcTable`]. The creation
/// stamp is the kernel FILETIME on Windows and `/proc/<pid>/stat`'s start in
/// clock ticks elsewhere — the same unit in both of a pass's tables, since both
/// come from the same walk; a missing or zero stamp stays `None` (UNKNOWN,
/// binds nothing).
pub fn proc_table(snapshot: &ProcessSnapshot) -> ProcTable {
    let mut table = ProcTable::new(!snapshot.is_unknown());
    for (&ppid, kids) in &snapshot.parent_map {
        for &pid in kids {
            let start = if cfg!(windows) {
                snapshot.creation_filetimes.get(&pid).copied()
            } else {
                snapshot.start_ticks.get(&pid).copied()
            };
            table.insert(
                pid,
                ProcRow {
                    ppid,
                    image: snapshot.names.get(&pid).cloned(),
                    start: start.filter(|s| *s > 0),
                },
            );
        }
    }
    table
}

/// PURE: every pid in the child subtrees of `claudes`, the `claude`s excluded,
/// bounded per session. A session past the bound contributes nothing and is
/// judged from a table that then lacks those command lines (UNKNOWN).
fn subtree_members(table: &ProcTable, claudes: &[u32]) -> Vec<u32> {
    let mut out: HashSet<u32> = HashSet::new();
    for &claude in claudes {
        let mut seen: Vec<u32> = Vec::new();
        let mut queue = table.children_of(claude);
        while let Some(pid) = queue.pop() {
            if seen.contains(&pid) || pid == claude {
                continue;
            }
            seen.push(pid);
            if seen.len() > MAX_MEMBERS_PER_SESSION {
                seen.clear();
                break;
            }
            queue.extend(table.children_of(pid));
        }
        out.extend(seen);
    }
    let mut v: Vec<u32> = out.into_iter().collect();
    v.sort_unstable();
    v
}

/// PURE: the per-child detail a log line carries, e.g.
/// `123:shim_or_pump[windows-cmd-coord-mcp-shim]; 456:work (bash is a tool shell …)`.
pub fn detail(verdict: &SessionVerdict) -> String {
    if let Some(why) = &verdict.refusal {
        return why.clone();
    }
    verdict
        .children
        .iter()
        .map(|c| match c.chain {
            Some(chain) => format!("{}:{}[{chain}]", c.pid, c.class.word()),
            None => format!("{}:{} ({})", c.pid, c.class.word(), c.reason),
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// PURE: the per-pass tally, e.g. `7 finished, 3 would admit, 4 would refuse
/// (refuse classes: claude_unbound=1 unknown=1 work=5)`. The refuse classes
/// count, over the refused sessions only, every child that kept its session
/// open by class (`work`, `unknown`) and every session whose `claude` could not
/// be bound (`claude_unbound`); `none` when nothing was refused.
pub fn pass_tally(judged: &[(String, SessionVerdict)]) -> String {
    let admit = judged.iter().filter(|(_, v)| v.admit).count();
    let mut classes: BTreeMap<&'static str, usize> = BTreeMap::new();
    for (_, verdict) in judged.iter().filter(|(_, v)| !v.admit) {
        if verdict.refusal.is_some() {
            *classes.entry("claude_unbound").or_default() += 1;
            continue;
        }
        for child in verdict
            .children
            .iter()
            .filter(|c| c.class != ChildClass::ShimOrPump)
        {
            *classes.entry(child.class.word()).or_default() += 1;
        }
    }
    let classes = if classes.is_empty() {
        "none".to_string()
    } else {
        classes
            .iter()
            .map(|(class, n)| format!("{class}={n}"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    format!(
        "{} finished, {admit} would admit, {} would refuse (refuse classes: {classes})",
        judged.len(),
        judged.len() - admit
    )
}

/// The last logged signature per `claude_session_id`, so a verdict is logged on
/// its change and not on every census.
fn last_logged() -> &'static Mutex<HashMap<String, String>> {
    static MAP: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

/// PURE: which verdicts to log — those whose signature differs from `last` —
/// and the map to keep (only the sessions judged this pass).
fn changed(
    last: &HashMap<String, String>,
    judged: &[(String, SessionVerdict)],
) -> (Vec<usize>, HashMap<String, String>) {
    let mut to_log = Vec::new();
    let mut next = HashMap::new();
    for (i, (session_id, verdict)) in judged.iter().enumerate() {
        let sig = format!(
            "{} ({}) {}",
            verdict.word(),
            verdict.classes(),
            detail(verdict)
        );
        if last.get(session_id) != Some(&sig) {
            to_log.push(i);
        }
        next.insert(session_id.clone(), sig);
    }
    (to_log, next)
}

/// Start a shadow pass over the finished sessions in `processes` on its own
/// task and return at once — the caller (the arm's census) never waits on it.
/// No pass is started while one is still running or a blocking read an
/// earlier pass started is still on the pool ([`admit`]), nor at all while the
/// in-process process-table read is switched off. Never fails, never acts.
pub fn spawn_observe(processes: &[LiveClaudeProcess]) {
    if process_tree::in_process_table_read_disabled() {
        // Every read a pass makes is in-process only, so with the switch set
        // it could only log `unknown` — and must not reach for the shared
        // external-probe fallback the census's own reads depend on instead.
        static SAID: AtomicBool = AtomicBool::new(false);
        if !SAID.swap(true, Ordering::Relaxed) {
            info!(
                "finished_close shadow: the in-process process-table read is switched off \
                 (QONTINUI_PROCESS_TABLE_IN_PROCESS=0) — no classify_child pass runs on this runner"
            );
        }
        return;
    }
    let sessions = finished_sessions(processes);
    if sessions.is_empty() {
        last_logged()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        return;
    }
    let Some(sessions) = admit(sessions, &LIVE_READS, &IN_FLIGHT) else {
        return;
    };
    tokio::spawn(async move {
        // Cleared on drop, so a pass that panics does not silence every later
        // census.
        struct Clear;
        impl Drop for Clear {
            fn drop(&mut self) {
                IN_FLIGHT.store(false, Ordering::Release);
            }
        }
        let _clear = Clear;
        observe(sessions).await;
    });
}

/// The pass gate: `Some(sessions)` when a census may start a pass now, having
/// taken the single-flight slot `in_flight`; `None` while a pass is still
/// running, or while any blocking read counted in `live` — necessarily one an
/// earlier pass started and abandoned at its budget — is still on the pool. A
/// pass refused for live reads takes no slot, and the first refusal of each
/// stall is logged at info, the rest at debug.
fn admit(
    sessions: Vec<(String, u32)>,
    live: &LiveReads,
    in_flight: &AtomicBool,
) -> Option<Vec<(String, u32)>> {
    if in_flight.load(Ordering::Acquire) {
        debug!("finished_close shadow: a classify_child pass is still running — none started");
        return None;
    }
    let reads = live.count();
    if reads > 0 {
        if STALL_SAID.swap(true, Ordering::Relaxed) {
            debug!(
                live_reads = reads,
                "finished_close shadow: blocking reads from an earlier classify_child pass are still running — none started"
            );
        } else {
            info!(
                live_reads = reads,
                "finished_close shadow: {reads} blocking read(s) an earlier classify_child pass started are \
                 still running past their budget — no pass starts until they return"
            );
        }
        return None;
    }
    if in_flight.swap(true, Ordering::AcqRel) {
        debug!("finished_close shadow: a classify_child pass is still running — none started");
        return None;
    }
    STALL_SAID.store(false, Ordering::Relaxed);
    Some(sessions)
}

/// Judge `sessions`, log the verdicts that changed and the pass's tally.
/// Bounded by [`SHADOW_BUDGET`].
async fn observe(sessions: Vec<(String, u32)>) {
    let judged = match tokio::time::timeout(SHADOW_BUDGET, judge(&sessions)).await {
        Ok(judged) => judged,
        Err(_) => {
            info!(
                finished = sessions.len(),
                budget_s = SHADOW_BUDGET.as_secs(),
                "finished_close shadow: classify_child did not finish inside its budget — no verdict this census"
            );
            return;
        }
    };
    let mut last = last_logged().lock().unwrap_or_else(|e| e.into_inner());
    let (to_log, next) = changed(&last, &judged);
    for i in to_log {
        let (session_id, verdict) = &judged[i];
        info!(
            session_id = %session_id,
            claude_pid = verdict.claude_pid,
            children = %detail(verdict),
            "finished_close shadow: classify_child {} ({})",
            verdict.word(),
            verdict.classes()
        );
    }
    *last = next;
    drop(last);
    info!(
        finished = judged.len(),
        "finished_close shadow: classify_child pass — {}",
        pass_tally(&judged)
    );
}

/// The reads and the pure judgement: table, argv, fresh table — each one
/// in-process, private to the shadow, bounded, and counted in [`LIVE_READS`]
/// until its blocking read returns.
async fn judge(sessions: &[(String, u32)]) -> Vec<(String, SessionVerdict)> {
    judge_with(
        sessions,
        || process_tree::snapshot_process_table_unshared(WALK_BUDGET, LIVE_READS.enter()),
        |pids| async move {
            process_tree::raw_argvs_for_pids(&pids, ARGV_BUDGET, LIVE_READS.enter()).await
        },
    )
    .await
}

/// [`judge`] over injected reads, so a test drives the exact sequence — table,
/// argvs of the first table's members, table again — with reads of its own. A
/// first table that is not a fresh read ends the pass there: every verdict is
/// UNKNOWN whatever the argv read and a second walk would find, so neither is
/// made.
async fn judge_with<T, TF, A, AF>(
    sessions: &[(String, u32)],
    table: T,
    argvs: A,
) -> Vec<(String, SessionVerdict)>
where
    T: Fn() -> TF,
    TF: Future<Output = ProcessSnapshot>,
    A: FnOnce(Vec<u32>) -> AF,
    AF: Future<Output = Option<HashMap<u32, Argv>>>,
{
    let platform = Platform::current();
    let first = proc_table(&table().await);
    if !first.fresh {
        return sessions
            .iter()
            .map(|(id, pid)| {
                (
                    id.clone(),
                    classify_session(platform, *pid, &first, None, &first),
                )
            })
            .collect();
    }
    let claudes: Vec<u32> = sessions.iter().map(|(_, pid)| *pid).collect();
    let members = subtree_members(&first, &claudes);
    let argv = argvs(members).await;
    let fresh = proc_table(&table().await);
    sessions
        .iter()
        .map(|(id, pid)| {
            (
                id.clone(),
                classify_session(platform, *pid, &first, argv.as_ref(), &fresh),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use session_lifecycle::ChildVerdict;

    fn snap(rows: &[(u32, u32, &str, i64, u64, u64)]) -> ProcessSnapshot {
        let mut s = ProcessSnapshot {
            stale: false,
            observed_at_ms: 1,
            ..ProcessSnapshot::default()
        };
        for &(pid, ppid, name, secs, filetime, ticks) in rows {
            s.parent_map.entry(ppid).or_default().push(pid);
            s.names.insert(pid, name.to_string());
            s.creation_times.insert(pid, secs);
            s.creation_filetimes.insert(pid, filetime);
            s.start_ticks.insert(pid, ticks);
        }
        s
    }

    #[test]
    fn the_table_carries_parent_image_and_the_platforms_creation_stamp() {
        let t = proc_table(&snap(&[
            (10, 1, "claude", 100, 5_000, 70_000),
            (11, 10, "python3", 101, 5_001, 70_042),
        ]));
        assert!(t.fresh);
        let row = t.row(11).unwrap();
        assert_eq!(row.ppid, 10);
        assert_eq!(row.image.as_deref(), Some("python3"));
        // The exact stamp, never the seconds derived from it.
        let expected = if cfg!(windows) { 5_001 } else { 70_042 };
        assert_eq!(row.start, Some(expected));
        assert_eq!(t.children_of(10), vec![11]);
    }

    #[test]
    fn a_zero_creation_stamp_is_unknown_and_a_stale_snapshot_is_not_fresh() {
        let mut s = snap(&[(11, 10, "python3", 0, 0, 0)]);
        assert_eq!(proc_table(&s).row(11).unwrap().start, None);
        s.stale = true;
        assert!(!proc_table(&s).fresh);
        assert!(!proc_table(&ProcessSnapshot::default()).fresh);
    }

    #[test]
    fn members_are_every_descendant_of_every_claude_and_never_the_claude() {
        let t = proc_table(&snap(&[
            (10, 1, "claude", 1, 1, 1),
            (11, 10, "cmd.exe", 1, 1, 1),
            (12, 11, "python3.exe", 1, 1, 1),
            (13, 12, "python.exe", 1, 1, 1),
            (20, 1, "claude", 1, 1, 1),
            (21, 20, "bash", 1, 1, 1),
            (30, 1, "unrelated", 1, 1, 1),
        ]));
        assert_eq!(subtree_members(&t, &[10, 20]), vec![11, 12, 13, 21]);
    }

    #[test]
    fn only_a_changed_verdict_is_logged_and_gone_sessions_are_forgotten() {
        let v = |admit: bool| SessionVerdict {
            claude_pid: 1,
            admit,
            children: Vec::new(),
            refusal: None,
        };
        let judged = vec![("a".to_string(), v(true)), ("b".to_string(), v(false))];
        let (log, next) = changed(&HashMap::new(), &judged);
        assert_eq!(log, vec![0, 1]);
        let (log, next) = changed(&next, &judged);
        assert!(log.is_empty());
        let judged = vec![("a".to_string(), v(false))];
        let (log, next) = changed(&next, &judged);
        assert_eq!(log, vec![0]);
        assert!(!next.contains_key("b"));
    }

    #[test]
    fn the_pass_tally_counts_sessions_and_what_refused_them() {
        let child = |pid: u32, class: ChildClass| ChildVerdict {
            pid,
            class,
            chain: None,
            reason: String::new(),
        };
        let session = |children: Vec<ChildVerdict>, refusal: Option<&str>| {
            let admit =
                refusal.is_none() && children.iter().all(|c| c.class == ChildClass::ShimOrPump);
            SessionVerdict {
                claude_pid: 1,
                admit,
                children,
                refusal: refusal.map(str::to_string),
            }
        };
        let judged = vec![
            (
                "admitted".to_string(),
                session(vec![child(1, ChildClass::ShimOrPump)], None),
            ),
            ("childless".to_string(), session(Vec::new(), None)),
            (
                "busy".to_string(),
                session(
                    vec![
                        child(2, ChildClass::ShimOrPump),
                        child(3, ChildClass::Work),
                        child(4, ChildClass::Work),
                    ],
                    None,
                ),
            ),
            (
                "unreadable".to_string(),
                session(
                    vec![child(5, ChildClass::Unknown), child(6, ChildClass::Work)],
                    None,
                ),
            ),
            (
                "unbound".to_string(),
                session(Vec::new(), Some("claude 9 is missing from a table")),
            ),
        ];
        assert_eq!(
            pass_tally(&judged),
            "5 finished, 2 would admit, 3 would refuse \
             (refuse classes: claude_unbound=1 unknown=1 work=3)"
        );
        assert_eq!(
            pass_tally(&judged[..2]),
            "2 finished, 2 would admit, 0 would refuse (refuse classes: none)"
        );
    }

    /// Finding 1 of the Phase 2a review, pinned at the shadow: a census-side
    /// memoized read that asks WHILE a shadow pass is walking, and that walk is
    /// failing, is never handed the shadow's failure. The census leads its own
    /// walk on its own provider and, when that fails too, still gets the
    /// fallback; the shadow's verdicts are UNKNOWN (stale tables); and the one
    /// fallback attempt the provider counts is the census's own.
    #[tokio::test]
    async fn a_census_read_during_a_failing_shadow_pass_still_gets_its_fallback() {
        use crate::process_capture::process_table::{
            read_unshared, read_with, Config, Provider, MEMO_TTL,
        };
        let p: &'static Provider = Box::leak(Box::new(Provider::new(Config::default())));
        let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        // The pass's first walk signals, then stalls until released; every walk
        // the pass makes fails.
        let gate = std::sync::Arc::new(Mutex::new(Some((started_tx, release_rx))));
        let table = move || {
            let gate = gate.lock().unwrap_or_else(|e| e.into_inner()).take();
            read_unshared(
                move || {
                    if let Some((started, release)) = gate {
                        let _ = started.send(());
                        let _ = release.recv_timeout(Duration::from_secs(10));
                    }
                    Err("scripted shadow walk failure".to_string())
                },
                Duration::from_secs(30),
                (),
            )
        };
        let sessions = vec![("s1".to_string(), 10), ("s2".to_string(), 20)];
        let pass =
            tokio::spawn(async move { judge_with(&sessions, table, |_| async { None }).await });
        started_rx.await.expect("the shadow's walk started");

        let census = read_with(
            p,
            MEMO_TTL,
            || Err("scripted census walk failure".to_string()),
            || async { Some(snap(&[(10, 1, "claude", 1, 9, 9)])) },
        )
        .await;
        assert!(
            !census.stale && census.names.contains_key(&10),
            "the census read was served its own fallback's table"
        );

        release_tx.send(()).expect("release the shadow's walk");
        let judged = pass.await.expect("the shadow pass joined");
        assert_eq!(judged.len(), 2);
        for (id, v) in &judged {
            assert!(!v.admit, "{id} admitted from a stale table: {v:?}");
            assert_eq!(v.classes(), "claude unbound", "{id}: {v:?}");
        }
        let health = p.health_json();
        assert_eq!(
            health["powershellFallbacks"], 1,
            "only the census's own attempt is counted: {health}"
        );
        assert!(health["circuitOpenUntil"].is_null(), "{health}");
    }

    /// The same sequence on walks that succeed: the shadow judges from two
    /// fresh private tables and the argvs of the first one's members.
    #[tokio::test]
    async fn the_shadow_judges_from_two_fresh_tables_and_the_argvs() {
        use crate::process_capture::process_table::read_unshared;
        let table = || {
            read_unshared(
                || {
                    Ok(snap(&[
                        (10, 1, "claude", 1, 9, 9),
                        (11, 10, "bash", 1, 9, 9),
                        (20, 1, "claude", 1, 9, 9),
                    ]))
                },
                Duration::from_secs(30),
                (),
            )
        };
        let asked = std::sync::Arc::new(Mutex::new(Vec::new()));
        let seen = std::sync::Arc::clone(&asked);
        let judged = judge_with(
            &[("busy".to_string(), 10), ("idle".to_string(), 20)],
            table,
            |pids| async move {
                *seen.lock().unwrap() = pids;
                Some(HashMap::new())
            },
        )
        .await;
        assert_eq!(
            *asked.lock().unwrap(),
            vec![11],
            "argvs of the members only"
        );
        assert_eq!(judged[0].1.classes(), "work=1", "{:?}", judged[0]);
        assert!(judged[1].1.admit, "a childless claude: {:?}", judged[1]);
    }

    /// A pass's read that outlives its budget keeps its blocking-pool thread
    /// until the OS-level read returns, and while it does no census starts
    /// another pass — so a hung read can never pile up one more read per
    /// census. The guard each read holds INSIDE its blocking closure is what
    /// counts it: the pass that met the stall is long over by the time the next
    /// census asks.
    #[tokio::test]
    async fn a_stalled_blocking_read_keeps_the_next_pass_from_starting_until_it_returns() {
        use crate::process_capture::process_table::read_unshared;
        let live: &'static LiveReads = Box::leak(Box::new(LiveReads::new()));
        let in_flight: &'static AtomicBool = Box::leak(Box::new(AtomicBool::new(false)));
        let sessions = vec![("s".to_string(), 10)];
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let release_rx = std::sync::Arc::new(Mutex::new(Some(release_rx)));
        let table = move || {
            let stall = release_rx.lock().unwrap_or_else(|e| e.into_inner()).take();
            read_unshared(
                move || {
                    if let Some(release) = stall {
                        let _ = release.recv_timeout(Duration::from_secs(10));
                    }
                    Ok(snap(&[(10, 1, "claude", 1, 9, 9)]))
                },
                Duration::from_millis(50),
                live.enter(),
            )
        };

        // A pass whose first walk stalls past its budget: the pass gives up
        // and ends (stale first table: nothing else is read)...
        let first = admit(sessions.clone(), live, in_flight).expect("the first pass starts");
        let judged = judge_with(&first, table, |_| async { None }).await;
        in_flight.store(false, Ordering::Release);
        assert_eq!(judged[0].1.classes(), "claude unbound", "{judged:?}");
        // ...but its walk is still on the pool, and the next census starts none.
        assert_eq!(live.count(), 1, "the stalled walk is still counted");
        assert_eq!(admit(sessions.clone(), live, in_flight), None);
        assert_eq!(admit(sessions.clone(), live, in_flight), None);
        assert!(
            !in_flight.load(Ordering::Acquire),
            "a pass refused for live reads takes no slot"
        );

        // Once the OS-level read returns, the guard drops and a pass may start.
        release_tx.send(()).expect("release the stalled walk");
        let give_up = std::time::Instant::now() + Duration::from_secs(5);
        while live.count() > 0 && std::time::Instant::now() < give_up {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(live.count(), 0, "the guard dropped when the walk returned");
        assert_eq!(admit(sessions.clone(), live, in_flight), Some(sessions));
        assert!(
            in_flight.load(Ordering::Acquire),
            "the admitted pass took the slot"
        );
    }

    /// A first table that is not a fresh read decides every verdict (UNKNOWN),
    /// so the pass makes no argv read and no second walk — no blocking read it
    /// does not need.
    #[tokio::test]
    async fn a_first_table_that_is_not_fresh_makes_no_argv_read_and_no_second_walk() {
        let walks = std::sync::Arc::new(AtomicUsize::new(0));
        let argv_reads = std::sync::Arc::new(AtomicUsize::new(0));
        let w = std::sync::Arc::clone(&walks);
        let a = std::sync::Arc::clone(&argv_reads);
        let mut stale = snap(&[(10, 1, "claude", 1, 9, 9), (11, 10, "python3", 1, 9, 9)]);
        stale.stale = true;
        let judged = judge_with(
            &[("s1".to_string(), 10), ("s2".to_string(), 20)],
            move || {
                w.fetch_add(1, Ordering::SeqCst);
                let table = stale.clone();
                async move { table }
            },
            move |_| {
                a.fetch_add(1, Ordering::SeqCst);
                async { Some(HashMap::new()) }
            },
        )
        .await;
        assert_eq!(walks.load(Ordering::SeqCst), 1, "one walk, not two");
        assert_eq!(argv_reads.load(Ordering::SeqCst), 0, "no argv read");
        for (id, v) in &judged {
            assert!(!v.admit, "{id}: {v:?}");
            assert_eq!(v.classes(), "claude unbound", "{id}: {v:?}");
        }
    }

    /// The production binding of the two tests above: [`judge`] reads only
    /// through the private doors they exercise, and the shadow's code names no
    /// shared read anywhere. A behavioural test cannot see that binding, so its
    /// text is pinned instead.
    #[test]
    fn the_shadow_reads_only_through_private_doors() {
        let src = include_str!("classify_child_shadow.rs");
        let production = src.split("#[cfg(test)]").next().unwrap_or_default();
        for door in [
            "snapshot_process_table_unshared(WALK_BUDGET, LIVE_READS.enter())",
            "raw_argvs_for_pids(&pids, ARGV_BUDGET, LIVE_READS.enter())",
            "admit(sessions, &LIVE_READS, &IN_FLIGHT)",
            "process_tree::in_process_table_read_disabled()",
        ] {
            assert!(
                production.contains(door),
                "the shadow no longer reads through `{door}`"
            );
        }
        for shared in [
            "snapshot_process_table_fresh",
            "snapshot_process_table_public",
            "command_lines_for_pids",
            "snapshot_fresh",
            "snapshot(",
            "read_with(",
            "lead_or_join",
            "begin_fallback",
            "run_fallback",
            "provider()",
        ] {
            assert!(
                !production.contains(shared),
                "the shadow reads through `{shared}`, a door into the process-table \
                 provider's shared state (its single flight, memo or fallback slot)"
            );
        }
    }
}
