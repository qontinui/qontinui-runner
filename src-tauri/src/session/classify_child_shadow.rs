//! `classify_child` in SHADOW: what the `finished_close` arm WOULD admit or
//! refuse if served `finished-criteria` condition 2's shim exemption were armed
//! (plan `2026-11-04-runner-finished-close-closes-nothing-on-any-box`, Phase 2a).
//!
//! **This module decides nothing.** It runs beside the arm's census, logs one
//! `classify_child would admit|refuse (<classes>)` line per FINISHED
//! terminal-hosted session whose verdict changed, and returns. Eligibility,
//! custody and `graceful_exit`'s any-child refusal are untouched; arming the
//! exemption is Phase 2b, after this log has shown correct verdicts on a real
//! runner.
//!
//! It looks at finished sessions whatever their eligibility verdict, because
//! the sessions the exemption exists for are exactly the ones eligibility
//! rejects today (`live_children`, the shim's `cmd.exe` on Windows).
//!
//! The reads follow the clause's binding rule: a fresh process table, then the
//! RAW command lines of every member of every finished session's child subtrees
//! (Windows: each PEB, never `sysinfo`'s rejoined argv, which loses cmd's `^"`
//! escapes and so could never match the measured `cmd.exe /d /s /c` chain),
//! then a SECOND fresh table every member is re-confirmed against (same pid,
//! creation time, parent and image). The pure judgement is
//! [`session_lifecycle::classify_session`].
//!
//! It runs on its own task, spawned from the census and never awaited by it, so
//! the arm's candidates, custody probes and closes act on the census exactly as
//! fresh as before. One pass at a time: a census that finds a pass still running
//! starts none. It runs only where the arm runs a census at all (`on` or
//! `shadow`); a box with the arm `off` produces no shadow evidence.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use session_lifecycle::{classify_session, Platform, ProcRow, ProcTable, SessionVerdict};
use tracing::{debug, info};

use crate::process_capture::process_tree::{
    raw_command_lines_for_pids, snapshot_process_table_fresh, ProcessSnapshot,
};
use crate::session::tracking_health::{LiveClaudeProcess, SessionWorkStatus};

/// The whole shadow pass is bounded, so a stuck read cannot pin the
/// single-flight flag and silence every later census.
const SHADOW_BUDGET: Duration = Duration::from_secs(10);

/// Set while a shadow pass runs; a census that finds it set starts none.
static IN_FLIGHT: AtomicBool = AtomicBool::new(false);

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
/// stamp is the kernel FILETIME on Windows and `/proc`'s epoch seconds
/// elsewhere; a missing or zero stamp stays `None` (UNKNOWN, binds nothing).
pub fn proc_table(snapshot: &ProcessSnapshot) -> ProcTable {
    let mut table = ProcTable::new(!snapshot.is_unknown());
    for (&ppid, kids) in &snapshot.parent_map {
        for &pid in kids {
            let start = if cfg!(windows) {
                snapshot.creation_filetimes.get(&pid).copied()
            } else {
                snapshot
                    .creation_times
                    .get(&pid)
                    .copied()
                    .filter(|s| *s > 0)
                    .map(|s| s as u64)
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
/// No pass is started while one is still running. Never fails, never acts.
pub fn spawn_observe(processes: &[LiveClaudeProcess]) {
    let sessions = finished_sessions(processes);
    if sessions.is_empty() {
        last_logged()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        return;
    }
    if IN_FLIGHT.swap(true, Ordering::AcqRel) {
        debug!("finished_close shadow: a classify_child pass is still running — none started");
        return;
    }
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

/// Judge `sessions` and log the verdicts that changed. Bounded by
/// [`SHADOW_BUDGET`].
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
    let admit = judged.iter().filter(|(_, v)| v.admit).count();
    debug!(
        finished = judged.len(),
        would_admit = admit,
        would_refuse = judged.len() - admit,
        "finished_close shadow: classify_child pass"
    );
}

/// The reads and the pure judgement: table, command lines, fresh table.
async fn judge(sessions: &[(String, u32)]) -> Vec<(String, SessionVerdict)> {
    let first = proc_table(&snapshot_process_table_fresh().await);
    let claudes: Vec<u32> = sessions.iter().map(|(_, pid)| *pid).collect();
    let members = subtree_members(&first, &claudes);
    let argv = raw_command_lines_for_pids(&members).await;
    let fresh = proc_table(&snapshot_process_table_fresh().await);
    let platform = Platform::current();
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

    fn snap(rows: &[(u32, u32, &str, i64, u64)]) -> ProcessSnapshot {
        let mut s = ProcessSnapshot {
            stale: false,
            observed_at_ms: 1,
            ..ProcessSnapshot::default()
        };
        for &(pid, ppid, name, secs, filetime) in rows {
            s.parent_map.entry(ppid).or_default().push(pid);
            s.names.insert(pid, name.to_string());
            s.creation_times.insert(pid, secs);
            s.creation_filetimes.insert(pid, filetime);
        }
        s
    }

    #[test]
    fn the_table_carries_parent_image_and_the_platforms_creation_stamp() {
        let t = proc_table(&snap(&[
            (10, 1, "claude", 100, 5_000),
            (11, 10, "python3", 101, 5_001),
        ]));
        assert!(t.fresh);
        let row = t.row(11).unwrap();
        assert_eq!(row.ppid, 10);
        assert_eq!(row.image.as_deref(), Some("python3"));
        let expected = if cfg!(windows) { 5_001 } else { 101 };
        assert_eq!(row.start, Some(expected));
        assert_eq!(t.children_of(10), vec![11]);
    }

    #[test]
    fn a_zero_creation_stamp_is_unknown_and_a_stale_snapshot_is_not_fresh() {
        let mut s = snap(&[(11, 10, "python3", 0, 0)]);
        assert_eq!(proc_table(&s).row(11).unwrap().start, None);
        s.stale = true;
        assert!(!proc_table(&s).fresh);
        assert!(!proc_table(&ProcessSnapshot::default()).fresh);
    }

    #[test]
    fn members_are_every_descendant_of_every_claude_and_never_the_claude() {
        let t = proc_table(&snap(&[
            (10, 1, "claude", 1, 1),
            (11, 10, "cmd.exe", 1, 1),
            (12, 11, "python3.exe", 1, 1),
            (13, 12, "python.exe", 1, 1),
            (20, 1, "claude", 1, 1),
            (21, 20, "bash", 1, 1),
            (30, 1, "unrelated", 1, 1),
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
}
