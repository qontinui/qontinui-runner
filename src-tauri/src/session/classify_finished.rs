//! `classify_child`, ARMED for the `finished_close` arm: which direct children of
//! a FINISHED session's `claude` are its own MCP shim, and so not background work
//! (plan `2026-11-04-runner-finished-close-closes-nothing-on-any-box`, Phase 2b;
//! served `finished-criteria` v11 condition 2).
//!
//! Phase 2a ran this beside the census in shadow, and its log on a real Linux and
//! a real Windows runner showed correct would-admit / would-refuse verdicts
//! (coord findings under the resource keys `phase-2a-shadow-evidence:linux` and
//! `phase-2a-shadow-evidence:windows`). Phase 2b arms
//! it in two places, and ONLY for the `finished_close` arm:
//!
//! 1. **The census** ([`judge_finished`], applied by
//!    `TrackingHealthReport::replace_boot_window_for_finished`): for a finished
//!    terminal-hosted session, `has_live_children` is no longer the 60 s
//!    boot-window rule ([`crate::session::tracking_health::child_signals_work`],
//!    which counts a late non-shell child as work and an early one as
//!    infrastructure by its image and start time alone). It reads every direct
//!    child: one is not work only when `classify_child` proves it the session's
//!    own shim, or it is a recorded stray wait loop. Every other session keeps
//!    the window rule.
//!    There is ONE census (`wind_down_observer::fresh_pass`, shared by the
//!    wind-down tick and `/restart-readiness` so the two cannot drift), so the
//!    replacement shows in both; it is bounded by [`CENSUS_BUDGET`], and a pass
//!    that does not answer in time reads every child as work.
//! 2. **The close** ([`shim_children`], through
//!    `ChildExemption::ShimOrPump`): `graceful_exit`'s any-child refusal, on the
//!    `finished_close` call only, leaves out a child this judges a shim NOW,
//!    from a fresh read. Every other `graceful_exit` caller counts every child.
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
//! **Anything unreadable keeps the session open.** No verdict (the reads are
//! switched off, a read stalled past its budget, the pass ran out of time) is
//! UNKNOWN, and an UNKNOWN child is counted as work.
//!
//! **It touches no shared read state.** Its tables come from
//! [`process_tree::snapshot_process_table_unshared`]: a private in-process walk
//! on the blocking pool, bounded by [`WALK_BUDGET`], entirely outside the
//! process-table provider every other read goes through. It leads no walk
//! another caller could join, reads and writes no memo, never takes the single
//! external-probe (PowerShell/WMI) fallback slot and moves no counter or
//! circuit — so no census read, and no other caller's, can be served a failure
//! of this one, delayed by it, or lose the fallback to it. The argv read is
//! in-process too, with no fallback. With the in-process read switched off
//! (`QONTINUI_PROCESS_TABLE_IN_PROCESS=0`) every read is UNKNOWN, so none is made.
//!
//! **No read while an earlier one still runs.** A read's budget bounds the
//! pass's wait, not the read: a walk or argv read past its budget keeps its
//! blocking-pool thread until the OS-level read returns (a `/proc/<pid>/cmdline`
//! read can block behind a process in uninterruptible sleep). Each read
//! therefore carries a [`LiveRead`] guard INSIDE its blocking closure, dropped
//! only when that read returns, and no pass starts while any is still alive
//! ([`admit`]) — so a hung read costs at most the reads of the one pass that met
//! it, never one more per census. A pass whose first table is not a fresh read
//! makes no further read at all: every verdict would be UNKNOWN whatever the
//! argv read and second walk found.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

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

/// What the CENSUS (`tracking_health::compute`, which `/restart-readiness`
/// shares with the wind-down tick) waits for the classification: a few walks'
/// worth, not the whole [`PASS_BUDGET`]. Past it every child of a finished
/// session counts as work for that census, and the next one tries again.
const CENSUS_BUDGET: Duration = Duration::from_secs(3);

/// The whole pass is bounded too — its three reads' budgets and a second for
/// the judgement — so a census never waits longer than this on it.
const PASS_BUDGET: Duration = WALK_BUDGET
    .saturating_mul(2)
    .saturating_add(ARGV_BUDGET)
    .saturating_add(Duration::from_secs(1));

/// The pass's blocking reads still running on the pool, by [`LiveRead`].
static LIVE_READS: LiveReads = LiveReads::new();

/// When the current stall was last announced at info ([`StallLog`]).
static STALL_LOG: StallLog = StallLog::new();

/// How often a stall that never ends is announced at info again, so a runner
/// whose log has rotated past the first announcement still says why it shows
/// no classify_child output.
const STALL_ANNOUNCE_EVERY: Duration = Duration::from_secs(3600);

/// When live reads last kept a census from starting a pass and that was said
/// at info; `None` once a pass has started (no stall).
struct StallLog(Mutex<Option<Instant>>);

impl StallLog {
    const fn new() -> Self {
        StallLog(Mutex::new(None))
    }

    /// Whether a refusal at `now` is announced at info: the first refusal of a
    /// stall, then at most one per [`STALL_ANNOUNCE_EVERY`] while it lasts.
    /// The clock is the caller's, so a test moves it by hand.
    fn announce(&self, now: Instant) -> bool {
        let mut last = self.0.lock().unwrap_or_else(|e| e.into_inner());
        match *last {
            Some(at) if now.saturating_duration_since(at) < STALL_ANNOUNCE_EVERY => false,
            _ => {
                *last = Some(now);
                true
            }
        }
    }

    /// A pass started: the stall, if there was one, is over.
    fn reset(&self) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

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

/// The finished terminal-hosted sessions in `processes` that hold a direct
/// child, judged by [`classify_session`] over fresh private reads, keyed by the
/// `claude` pid. `None` is UNKNOWN: nothing was judged (the in-process read is
/// switched off, an earlier read still runs past its budget, or the pass ran
/// out of time), and every child of every such session then counts as work. A
/// session with no direct child is not judged — it has nothing to classify.
///
/// A verdict that CHANGED since the last pass is logged, and every pass logs
/// one tally, so a verdict that never changes still leaves evidence after the
/// log rotates.
pub async fn judge_finished(
    processes: &[LiveClaudeProcess],
    snapshot: &ProcessSnapshot,
) -> Option<HashMap<u32, SessionVerdict>> {
    if process_tree::in_process_table_read_disabled() {
        static SAID: AtomicBool = AtomicBool::new(false);
        if !SAID.swap(true, Ordering::Relaxed) {
            info!(
                "finished_close: the in-process process-table read is switched off \
                 (QONTINUI_PROCESS_TABLE_IN_PROCESS=0) — classify_child cannot judge a child, \
                 so every child of a finished session counts as work on this runner"
            );
        }
        return None;
    }
    let sessions: Vec<(String, u32)> = finished_sessions(processes)
        .into_iter()
        .filter(|(_, pid)| snapshot.parent_map.contains_key(pid))
        .collect();
    if sessions.is_empty() {
        last_logged()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        return Some(HashMap::new());
    }
    let judged = judge_bounded(&sessions, CENSUS_BUDGET).await?;
    log_judged(&judged);
    Some(
        judged
            .into_iter()
            .map(|(_, verdict)| (verdict.claude_pid, verdict))
            .collect(),
    )
}

/// The children of `claude` (one of `children`, its direct children) that
/// `classify_child` judges the session's own shim NOW: a fresh judgement, made
/// at the close's own probe. Empty when nothing could be judged — fail closed.
pub async fn shim_children(claude: u32, children: &[u32]) -> BTreeSet<u32> {
    if process_tree::in_process_table_read_disabled() || children.is_empty() {
        return BTreeSet::new();
    }
    let Some(judged) = judge_bounded(&[(String::new(), claude)], PASS_BUDGET).await else {
        return BTreeSet::new();
    };
    judged
        .iter()
        .flat_map(|(_, verdict)| shim_pids(verdict, children))
        .collect()
}

/// PURE: the pids in `children` that `verdict` positively classifies
/// [`ChildClass::ShimOrPump`]. A verdict whose `claude` could not be bound
/// carries no child verdicts and so admits none.
pub fn shim_pids(verdict: &SessionVerdict, children: &[u32]) -> Vec<u32> {
    verdict
        .children
        .iter()
        .filter(|c| c.class == ChildClass::ShimOrPump && children.contains(&c.pid))
        .map(|c| c.pid)
        .collect()
}

/// The pass gate: may a pass start now? `false` while any blocking read counted
/// in `live` — necessarily one an earlier pass started and abandoned at its
/// budget — is still on the pool. A stall is logged at info on its first
/// refusal and again at most once per [`STALL_ANNOUNCE_EVERY`] while it lasts
/// (`stall`, read at `now`), and at debug otherwise. A pass that starts ends
/// the stall.
fn admit(live: &LiveReads, stall: &StallLog, now: Instant) -> bool {
    let reads = live.count();
    if reads > 0 {
        if stall.announce(now) {
            info!(
                live_reads = reads,
                "finished_close: {reads} blocking read(s) an earlier classify_child pass started are \
                 still running past their budget — no pass starts until they return (said again at most \
                 hourly while it lasts)"
            );
        } else {
            debug!(
                live_reads = reads,
                "finished_close: blocking reads from an earlier classify_child pass are still running — none started"
            );
        }
        return false;
    }
    stall.reset();
    true
}

/// One pass at a time: passes queue here, so two callers' reads never overlap
/// and a read one of them has running is never taken for a stalled one.
static PASS_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// [`judge`], once it is this caller's turn and [`admit`] allows a pass, all
/// inside `budget` (waiting for the turn included). `None` when it did not
/// finish or was not admitted — UNKNOWN.
async fn judge_bounded(
    sessions: &[(String, u32)],
    budget: Duration,
) -> Option<Vec<(String, SessionVerdict)>> {
    let pass = async {
        let _turn = PASS_LOCK.lock().await;
        if !admit(&LIVE_READS, &STALL_LOG, Instant::now()) {
            return None;
        }
        Some(judge(sessions).await)
    };
    match tokio::time::timeout(budget, pass).await {
        Ok(judged) => judged,
        Err(_) => {
            info!(
                finished = sessions.len(),
                budget_s = budget.as_secs(),
                "finished_close: classify_child did not finish inside its budget — no verdict this census"
            );
            None
        }
    }
}

/// Log the verdicts that changed and the pass's tally.
fn log_judged(judged: &[(String, SessionVerdict)]) {
    let mut last = last_logged().lock().unwrap_or_else(|e| e.into_inner());
    let (to_log, next) = changed(&last, judged);
    for i in to_log {
        let (session_id, verdict) = &judged[i];
        info!(
            session_id = %session_id,
            claude_pid = verdict.claude_pid,
            children = %detail(verdict),
            "finished_close: classify_child {} ({})",
            verdict.word(),
            verdict.classes()
        );
    }
    *last = next;
    drop(last);
    info!(
        finished = judged.len(),
        "finished_close: classify_child pass — {}",
        pass_tally(judged)
    );
}

/// The reads and the pure judgement: table, argv, fresh table — each one
/// in-process, private to this module, bounded, and counted in [`LIVE_READS`]
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

    fn child(pid: u32, class: ChildClass) -> ChildVerdict {
        ChildVerdict {
            pid,
            class,
            chain: None,
            reason: String::new(),
        }
    }

    /// The close's exemption admits exactly the children the verdict proves a
    /// shim AND that are direct children asked about; work, unknown, a child
    /// not asked about and an unbound `claude` admit nothing.
    #[test]
    fn only_a_proven_shim_among_the_asked_children_is_admitted() {
        let verdict = SessionVerdict {
            claude_pid: 10,
            admit: false,
            children: vec![
                child(11, ChildClass::ShimOrPump),
                child(12, ChildClass::Work),
                child(13, ChildClass::Unknown),
                child(14, ChildClass::ShimOrPump),
            ],
            refusal: None,
        };
        assert_eq!(shim_pids(&verdict, &[11, 12, 13]), vec![11]);
        assert!(shim_pids(&verdict, &[12, 13]).is_empty());
        assert!(shim_pids(&verdict, &[]).is_empty());
        let unbound = SessionVerdict {
            claude_pid: 10,
            admit: false,
            children: Vec::new(),
            refusal: Some("claude 10 is missing from a table".to_string()),
        };
        assert!(shim_pids(&unbound, &[11]).is_empty());
    }

    /// With nothing to ask about, `shim_children` reads nothing and admits
    /// nothing; so does a pid that is no process at all (no table row, so the
    /// `claude` cannot be bound).
    #[tokio::test]
    async fn shim_children_fails_closed() {
        assert!(shim_children(10, &[]).await.is_empty());
        assert!(shim_children(4_000_000_003, &[4_000_000_004])
            .await
            .is_empty());
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

    /// Finding 1 of the Phase 2a review, pinned at the classify pass: a census-side
    /// memoized read that asks WHILE a classify pass is walking, and that walk is
    /// failing, is never handed the pass's failure. The census leads its own
    /// walk on its own provider and, when that fails too, still gets the
    /// fallback; the pass's verdicts are UNKNOWN (stale tables); and the one
    /// fallback attempt the provider counts is the census's own.
    #[tokio::test]
    async fn a_census_read_during_a_failing_classify_pass_still_gets_its_fallback() {
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
        started_rx.await.expect("the pass's walk started");

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

        release_tx.send(()).expect("release the pass's walk");
        let judged = pass.await.expect("the pass joined");
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
    async fn the_pass_judges_from_two_fresh_tables_and_the_argvs() {
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
        let stall = StallLog::new();
        let t0 = Instant::now();
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
        assert!(admit(live, &stall, t0), "the first pass starts");
        let judged = judge_with(&sessions, table, |_| async { None }).await;
        assert_eq!(judged[0].1.classes(), "claude unbound", "{judged:?}");
        // ...but its walk is still on the pool, and the next census starts none.
        assert_eq!(live.count(), 1, "the stalled walk is still counted");
        let later = t0 + Duration::from_secs(30);
        assert!(!admit(live, &stall, later));
        assert!(!admit(live, &stall, later));
        assert!(
            !stall.announce(later),
            "the first refusal announced the stall; the next ones inside the interval do not"
        );

        // Once the OS-level read returns, the guard drops and a pass may start.
        release_tx.send(()).expect("release the stalled walk");
        let give_up = std::time::Instant::now() + Duration::from_secs(5);
        while live.count() > 0 && std::time::Instant::now() < give_up {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(live.count(), 0, "the guard dropped when the walk returned");
        assert!(admit(live, &stall, later));
        assert!(
            stall.announce(later),
            "the admitted pass ended the stall: the next one is announced at once"
        );
    }

    /// A stall that never ends is said again at info on a timer, never more
    /// than once per interval, so a rotated log still shows why no shadow
    /// output appears; a pass starting ends it. Driven by a hand-moved clock.
    #[test]
    fn a_stall_is_announced_at_once_then_at_most_once_per_interval() {
        let stall = StallLog::new();
        let t0 = Instant::now();
        let at = |d: Duration| t0 + d;
        assert!(stall.announce(t0), "the first refusal of a stall is said");
        assert!(!stall.announce(at(Duration::from_secs(30))));
        assert!(!stall.announce(at(STALL_ANNOUNCE_EVERY - Duration::from_millis(1))));
        assert!(stall.announce(at(STALL_ANNOUNCE_EVERY)), "one interval on");
        assert!(!stall.announce(at(STALL_ANNOUNCE_EVERY + Duration::from_secs(60))));
        assert!(!stall.announce(at(STALL_ANNOUNCE_EVERY * 2 - Duration::from_millis(1))));
        assert!(stall.announce(at(STALL_ANNOUNCE_EVERY * 2)), "and again");
        // A clock read out of order never announces early.
        assert!(!stall.announce(t0));
        stall.reset();
        assert!(
            stall.announce(at(STALL_ANNOUNCE_EVERY * 2 + Duration::from_secs(1))),
            "a stall after a pass is a new one, said at once"
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
    fn the_module_reads_only_through_private_doors() {
        let src = include_str!("classify_finished.rs");
        let production = src.split("#[cfg(test)]").next().unwrap_or_default();
        for door in [
            "snapshot_process_table_unshared(WALK_BUDGET, LIVE_READS.enter())",
            "raw_argvs_for_pids(&pids, ARGV_BUDGET, LIVE_READS.enter())",
            "admit(&LIVE_READS, &STALL_LOG, Instant::now())",
            "process_tree::in_process_table_read_disabled()",
        ] {
            assert!(
                production.contains(door),
                "the module no longer reads through `{door}`"
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
                "the module reads through `{shared}`, a door into the process-table \
                 provider's shared state (its single flight, memo or fallback slot)"
            );
        }
    }
}
