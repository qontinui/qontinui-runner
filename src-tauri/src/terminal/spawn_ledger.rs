//! Spawn ledger — forensic bookkeeping for terminal spawns (observability only).
//!
//! Plan `2026-10-28-runner-spawn-forensics-logs-name-the-session-pid-and-gate`.
//!
//! On 2026-10-27 a burst of gate-continuation terminals overloaded the box and
//! the runner log could not say WHICH session / pid / gate caused it. This
//! module is the one place those facts are kept:
//!
//! - every PTY spawn [`register`]s (terminal id, OS pid, parent pid, image,
//!   claude session id);
//! - the continuation dispatcher [`annotate_origin`]s with gate id, skill,
//!   anchor key and session name;
//! - a rate-based burst warning names the dominant source;
//! - [`log_roster`] dumps PID -> terminal -> claude session -> name, at a burst,
//!   at `close_all`, and from the force-exit watchdog.
//!
//! **Nothing here changes what is admitted or spawned.** Every entry point is
//! infallible and non-blocking on the spawn path; a poisoned lock is recovered,
//! never propagated. A reading that cannot be taken renders as `unknown`.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant};

use tracing::{info, warn};

/// Spawns within [`BURST_WINDOW`] that constitute a burst. A log trigger, not a
/// setting: tune by editing this line if the first measurement says so.
pub(crate) const BURST_THRESHOLD: usize = 8;
/// The window the burst threshold is counted over.
pub(crate) const BURST_WINDOW: Duration = Duration::from_secs(60);
/// After a burst warning, repeats are suppressed this long so the warning
/// cannot itself flood the log during the event it reports.
pub(crate) const BURST_SUPPRESS: Duration = Duration::from_secs(30);
/// Cap on the recent-spawn ring.
const RECENT_CAP: usize = 256;
/// Cap on roster lines in a burst dump (close_all / watchdog dump everything).
pub(crate) const BURST_ROSTER_CAP: usize = 64;

const UNKNOWN: &str = "unknown";
const UNATTRIBUTED: &str = "operator_or_unknown";

/// Where a terminal came from.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum Origin {
    /// Not (yet) annotated: an operator terminal, or a continuation whose
    /// dispatcher has not reached its annotation call.
    #[default]
    Unknown,
    GateContinuation {
        skill: String,
        gate_id: String,
        dispatch_id: String,
        anchor_key: String,
        session_name: String,
    },
}

impl Origin {
    /// The grouping key the burst warning uses: `<skill>:<name prefix before #>`.
    pub(crate) fn source_key(&self) -> String {
        match self {
            Origin::Unknown => UNATTRIBUTED.to_string(),
            Origin::GateContinuation {
                skill,
                session_name,
                ..
            } => {
                let prefix = session_name.split('#').next().unwrap_or(UNKNOWN);
                format!("{skill}:{prefix}")
            }
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Entry {
    pub terminal_id: String,
    pub os_pid: Option<u32>,
    pub parent_pid: Option<u32>,
    pub image: String,
    pub claude_session_id: String,
    pub spawned_at: Instant,
    pub origin: Origin,
}

#[derive(Debug, Clone)]
struct Recent {
    at: Instant,
    terminal_id: String,
    source: String,
}

#[derive(Debug, Default)]
pub(crate) struct Ledger {
    entries: HashMap<String, Entry>,
    recent: VecDeque<Recent>,
    last_burst_warn: Option<Instant>,
}

impl Ledger {
    /// Record a spawn. Returns a burst report when this registration trips the
    /// burst warning.
    pub(crate) fn register_at(&mut self, entry: Entry, now: Instant) -> Option<String> {
        self.recent.push_back(Recent {
            at: now,
            terminal_id: entry.terminal_id.clone(),
            source: entry.origin.source_key(),
        });
        while self.recent.len() > RECENT_CAP {
            self.recent.pop_front();
        }
        self.entries.insert(entry.terminal_id.clone(), entry);
        self.evaluate_burst(now)
    }

    /// Attach origin facts to an already-registered terminal. Counting is on
    /// registration only, so an annotated spawn is never counted twice; the
    /// burst is re-evaluated because the dominant source may only now be known.
    pub(crate) fn annotate_at(
        &mut self,
        terminal_id: &str,
        origin: Origin,
        now: Instant,
    ) -> Option<String> {
        let key = origin.source_key();
        if let Some(e) = self.entries.get_mut(terminal_id) {
            e.origin = origin;
        }
        if let Some(r) = self.recent.iter_mut().find(|r| r.terminal_id == terminal_id) {
            r.source = key;
        }
        self.evaluate_burst(now)
    }

    pub(crate) fn deregister(&mut self, terminal_id: &str) {
        self.entries.remove(terminal_id);
    }

    /// Pure burst evaluator over the ring (synthetic-clock testable).
    fn evaluate_burst(&mut self, now: Instant) -> Option<String> {
        let in_window: Vec<&Recent> = self
            .recent
            .iter()
            .filter(|r| now.saturating_duration_since(r.at) <= BURST_WINDOW)
            .collect();
        if in_window.len() < BURST_THRESHOLD {
            return None;
        }
        if let Some(last) = self.last_burst_warn {
            if now.saturating_duration_since(last) < BURST_SUPPRESS {
                return None;
            }
        }
        self.last_burst_warn = Some(now);

        let mut counts: HashMap<&str, usize> = HashMap::new();
        for r in &in_window {
            *counts.entry(r.source.as_str()).or_default() += 1;
        }
        let mut ranked: Vec<(&str, usize)> = counts.into_iter().collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        let oldest = in_window
            .first()
            .map(|r| now.saturating_duration_since(r.at).as_secs())
            .unwrap_or(0);
        let top: Vec<String> = ranked
            .iter()
            .take(3)
            .map(|(k, n)| format!("{k}:{n}"))
            .collect();
        let dominant = ranked
            .first()
            .map(|(k, n)| format!("{k}:{n}"))
            .unwrap_or_else(|| UNKNOWN.to_string());
        Some(format!(
            "spawn burst: {} terminal spawns in {}s, dominant source={}, top_sources=[{}], live={}",
            in_window.len(),
            oldest,
            dominant,
            top.join(", "),
            self.entries.len()
        ))
    }

    pub(crate) fn snapshot(&self) -> Vec<Entry> {
        let mut v: Vec<Entry> = self.entries.values().cloned().collect();
        v.sort_by_key(|e| e.spawned_at);
        v
    }
}

static LEDGER: Mutex<Option<Ledger>> = Mutex::new(None);

fn lock() -> MutexGuard<'static, Option<Ledger>> {
    match LEDGER.lock() {
        Ok(g) => g,
        // A panic elsewhere must not take the spawn path with it.
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn with_ledger<R>(f: impl FnOnce(&mut Ledger) -> R) -> R {
    let mut g = lock();
    f(g.get_or_insert_with(Ledger::default))
}

/// One-pid sysinfo read: `(parent pid, image name)`, `None`s when unreadable.
fn read_process_identity(pid: u32) -> (Option<u32>, String) {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
    let mut sys = System::new();
    let p = Pid::from_u32(pid);
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[p]),
        true,
        ProcessRefreshKind::new(),
    );
    match sys.process(p) {
        Some(proc_) => (
            proc_.parent().map(|pp| pp.as_u32()),
            proc_.name().to_string_lossy().into_owned(),
        ),
        None => (None, UNKNOWN.to_string()),
    }
}

/// Register a freshly spawned terminal and log a burst warning if it trips one.
pub(crate) fn register(terminal_id: &str, os_pid: Option<u32>, claude_session_id: &str) {
    let (parent_pid, image) = match os_pid {
        Some(pid) => read_process_identity(pid),
        None => (None, UNKNOWN.to_string()),
    };
    let entry = Entry {
        terminal_id: terminal_id.to_string(),
        os_pid,
        parent_pid,
        image,
        claude_session_id: claude_session_id.to_string(),
        spawned_at: Instant::now(),
        origin: Origin::Unknown,
    };
    let report = with_ledger(|l| l.register_at(entry, Instant::now()));
    emit_burst(report);
}

/// Attach gate-continuation origin facts to a registered terminal.
pub(crate) fn annotate_origin(terminal_id: &str, origin: Origin) {
    let report = with_ledger(|l| l.annotate_at(terminal_id, origin, Instant::now()));
    emit_burst(report);
}

pub(crate) fn deregister(terminal_id: &str) {
    with_ledger(|l| l.deregister(terminal_id));
}

/// `(live, spawned in the last 60 s)` for resource_guard's log line.
pub(crate) fn counts() -> (usize, usize) {
    with_ledger(|l| {
        let now = Instant::now();
        let recent = l
            .recent
            .iter()
            .filter(|r| now.saturating_duration_since(r.at) <= BURST_WINDOW)
            .count();
        (l.entries.len(), recent)
    })
}

fn emit_burst(report: Option<String>) {
    if let Some(msg) = report {
        warn!("{msg}");
        // Off the spawn path: the dump walks the process table and reads name files.
        let _ = std::thread::Builder::new()
            .name("spawn-burst-roster".into())
            .spawn(|| log_roster("burst", Some(BURST_ROSTER_CAP), true));
    }
}

/// The leading `/skill` token of a continuation prompt, `unknown` when absent.
pub(crate) fn skill_from_prompt(initial_prompt: &str) -> String {
    initial_prompt
        .split_whitespace()
        .next()
        .filter(|t| t.starts_with('/') && t.len() > 1)
        .map(|t| t.trim_start_matches('/').to_string())
        .unwrap_or_else(|| UNKNOWN.to_string())
}

/// Resolve a claude session id to its recorded name; `unknown` when absent.
fn resolve_name(claude_session_id: &str) -> String {
    crate::agent_worktree::custody::read_session_name(claude_session_id)
        .unwrap_or_else(|| UNKNOWN.to_string())
}

fn fmt_pid(p: Option<u32>) -> String {
    p.map(|v| v.to_string()).unwrap_or_else(|| UNKNOWN.to_string())
}

/// Format one roster line. Pure, so it is golden-testable.
pub(crate) fn format_roster_line(
    reason: &str,
    e: &Entry,
    name: &str,
    claude_pids: Option<&[u32]>,
    now: Instant,
) -> String {
    let origin = match &e.origin {
        Origin::Unknown => "operator_or_unknown".to_string(),
        Origin::GateContinuation {
            skill,
            gate_id,
            anchor_key,
            ..
        } => format!("gate_continuation skill={skill} gate={gate_id} anchor={anchor_key}"),
    };
    let claude = match claude_pids {
        Some(p) => format!(
            " claude_pids=[{}]",
            p.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(",")
        ),
        None => String::new(),
    };
    format!(
        "roster[{reason}] terminal={} pid={} parent={} image={} claude_session={} name={} origin={} age_s={}{}",
        e.terminal_id,
        fmt_pid(e.os_pid),
        fmt_pid(e.parent_pid),
        e.image,
        if e.claude_session_id.is_empty() { UNKNOWN } else { &e.claude_session_id },
        name,
        origin,
        now.saturating_duration_since(e.spawned_at).as_secs(),
        claude
    )
}

/// Descendant `claude*` pids per root pid, from ONE sysinfo process-table walk.
fn claude_descendants(roots: &[u32]) -> HashMap<u32, Vec<u32>> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
    let mut sys = System::new();
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::new());
    let mut out = HashMap::new();
    for &root in roots {
        let mut found = Vec::new();
        for (pid, p) in sys.processes() {
            let is_claude = p
                .name()
                .to_string_lossy()
                .to_ascii_lowercase()
                .starts_with("claude");
            if !is_claude {
                continue;
            }
            // Walk up from this claude process; bounded to guard cycles.
            let mut cur = Some(*pid);
            let mut hops = 0;
            while let Some(c) = cur {
                if c.as_u32() == root {
                    found.push(pid.as_u32());
                    break;
                }
                hops += 1;
                if hops > 32 {
                    break;
                }
                cur = sys.process(c).and_then(|x| x.parent());
            }
        }
        found.sort_unstable();
        out.insert(root, found);
    }
    out
}

/// Dump the live roster. `cap` bounds the lines (tail line says how many were
/// cut). `with_process_table = false` skips the sysinfo table walk — used by
/// the force-exit watchdog, which must never be slowed by the machine state it
/// is reporting. The ledger is read with `try_lock`: on contention the dump
/// says so and returns rather than blocking an exit path.
pub(crate) fn log_roster(reason: &str, cap: Option<usize>, with_process_table: bool) {
    let entries = match LEDGER.try_lock() {
        Ok(mut g) => g.get_or_insert_with(Ledger::default).snapshot(),
        Err(TryLockError::Poisoned(p)) => p.into_inner().get_or_insert_with(Ledger::default).snapshot(),
        Err(TryLockError::WouldBlock) => {
            warn!("roster[{reason}] unavailable: ledger busy");
            return;
        }
    };
    info!("roster[{reason}] live_terminals={}", entries.len());
    let shown = cap.map_or(entries.len(), |c| c.min(entries.len()));
    let table = if with_process_table && !entries.is_empty() {
        let roots: Vec<u32> = entries.iter().filter_map(|e| e.os_pid).collect();
        Some(claude_descendants(&roots))
    } else {
        None
    };
    let now = Instant::now();
    for e in entries.iter().take(shown) {
        // Name files are read only when the caller may do I/O (not the
        // watchdog / close_all paths): there the origin's own name is used.
        let name = if with_process_table {
            resolve_name(&e.claude_session_id)
        } else {
            match &e.origin {
                Origin::GateContinuation { session_name, .. } => session_name.clone(),
                Origin::Unknown => UNKNOWN.to_string(),
            }
        };
        let pids = table
            .as_ref()
            .and_then(|t| e.os_pid.and_then(|p| t.get(&p)))
            .map(|v| v.as_slice());
        info!("{}", format_roster_line(reason, e, &name, pids, now));
    }
    if shown < entries.len() {
        info!("roster[{reason}] ... and {} more", entries.len() - shown);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str) -> Entry {
        Entry {
            terminal_id: id.to_string(),
            os_pid: Some(100),
            parent_pid: Some(1),
            image: "claude.exe".into(),
            claude_session_id: "02fa8da1".into(),
            spawned_at: Instant::now(),
            origin: Origin::Unknown,
        }
    }

    fn cont(name: &str) -> Origin {
        Origin::GateContinuation {
            skill: "babysit-prs".into(),
            gate_id: "g1".into(),
            dispatch_id: "d1".into(),
            anchor_key: "pr:a/b#1".into(),
            session_name: name.into(),
        }
    }

    #[test]
    fn register_annotate_deregister_round_trip() {
        let mut l = Ledger::default();
        let now = Instant::now();
        l.register_at(entry("t1"), now);
        assert_eq!(l.snapshot().len(), 1);
        l.annotate_at("t1", cont("post-merge-runner#1"), now);
        let snap = l.snapshot();
        assert_eq!(snap[0].claude_session_id, "02fa8da1");
        assert_eq!(snap[0].origin.source_key(), "babysit-prs:post-merge-runner");
        l.deregister("t1");
        assert!(l.snapshot().is_empty());
    }

    #[test]
    fn below_threshold_emits_nothing_and_threshold_emits_once() {
        let mut l = Ledger::default();
        let t0 = Instant::now();
        for i in 0..BURST_THRESHOLD - 1 {
            assert!(l
                .register_at(entry(&format!("t{i}")), t0 + Duration::from_secs(i as u64))
                .is_none());
        }
        let r = l.register_at(entry("last"), t0 + Duration::from_secs(8));
        assert!(r.unwrap().starts_with("spawn burst: 8 terminal spawns"));
        // Inside the suppression window: nothing.
        assert!(l
            .register_at(entry("again"), t0 + Duration::from_secs(10))
            .is_none());
    }

    #[test]
    fn dominant_source_groups_by_skill_and_name_prefix() {
        let mut l = Ledger::default();
        let t0 = Instant::now();
        for i in 0..7 {
            let id = format!("t{i}");
            l.register_at(entry(&id), t0);
            l.annotate_at(&id, cont(&format!("post-merge-runner#{i}")), t0);
        }
        let r = l.register_at(entry("t7"), t0).unwrap();
        assert!(
            r.contains("dominant source=babysit-prs:post-merge-runner:7"),
            "{r}"
        );
        assert!(r.contains("operator_or_unknown:1"), "{r}");
    }

    /// Regression for 2026-10-27: 20 annotated spawns at 2 s spacing.
    #[test]
    fn regression_2026_10_27_burst_of_gate_continuations_names_dominant_source() {
        let mut l = Ledger::default();
        let t0 = Instant::now();
        let mut warnings = Vec::new();
        for i in 0..20u64 {
            let id = format!("t{i}");
            let at = t0 + Duration::from_secs(2 * i);
            warnings.extend(l.register_at(entry(&id), at));
            warnings.extend(l.annotate_at(&id, cont(&format!("post-merge-runner#{i}")), at));
        }
        // 40 s of spawns with a 30 s suppress: first fires at the 8th, then one more.
        assert!(!warnings.is_empty());
        assert!(warnings.len() <= 2, "{warnings:?}");
        assert!(
            warnings[0].contains("babysit-prs:post-merge-runner"),
            "{}",
            warnings[0]
        );
    }

    #[test]
    fn skill_is_the_leading_slash_token_or_unknown() {
        assert_eq!(skill_from_prompt("/babysit-prs  foo"), "babysit-prs");
        assert_eq!(skill_from_prompt("run it"), "unknown");
        assert_eq!(skill_from_prompt(""), "unknown");
    }

    #[test]
    fn roster_line_golden() {
        let mut e = entry("84c4fa68");
        e.os_pid = Some(44636);
        e.origin = cont("post-merge-runner#9");
        let line = format_roster_line("close_all", &e, "coord-error", Some(&[44636]), e.spawned_at);
        assert_eq!(
            line,
            "roster[close_all] terminal=84c4fa68 pid=44636 parent=1 image=claude.exe \
             claude_session=02fa8da1 name=coord-error \
             origin=gate_continuation skill=babysit-prs gate=g1 anchor=pr:a/b#1 age_s=0 \
             claude_pids=[44636]"
        );
    }

    #[test]
    fn unreadable_facts_render_as_unknown() {
        let mut e = entry("t");
        e.os_pid = None;
        e.parent_pid = None;
        e.claude_session_id = String::new();
        let line = format_roster_line("x", &e, "unknown", None, e.spawned_at);
        assert!(line.contains("pid=unknown parent=unknown"), "{line}");
        assert!(line.contains("claude_session=unknown"), "{line}");
    }

    #[test]
    fn roster_does_not_block_when_the_ledger_is_held() {
        let _g = LEDGER.lock().unwrap_or_else(|p| p.into_inner());
        // Same thread holds the lock: try_lock must return, not deadlock.
        log_roster("test", None, false);
    }
}
