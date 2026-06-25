//! Process-start-anchored reconcile backstop (session-restore-redesign Phase 4).
//!
//! ## Why this exists
//!
//! The deterministic identity path — the runner pre-pins `--session-id` at spawn
//! and records authoritatively, the provider's SessionStart hook CONFIRMS it —
//! covers every session the runner launches AND every hand-typed `claude`/`clg`
//! that goes through the PATH shim. It does NOT cover the one documented
//! non-deterministic edge (plan §7): an **absolute-path** / shim-bypassing
//! `claude` invocation. Those launch a real provider whose session id the runner
//! never pinned and whose hook (delivered via the shim's `--settings`) never
//! attached — so the terminal hosts a live, unrecorded (or merely provisional)
//! session.
//!
//! This reconcile pass closes that gap WITHOUT the mtime-capture race that
//! [`crate::commands::terminal::poll_and_record_session`] suffers. For each live
//! PTY that lacks a CONFIRMED authoritative record, it correlates the PTY's child
//! AI process — its **start time + cwd + config dir** — to the freshest transcript
//! on disk whose **first event timestamp is AFTER the process started**. A
//! transcript that began after the process did is one this process authored; the
//! mtime race (newest-modified-wins) could instead bind a long-running foreign
//! session that merely got appended to in the same window.
//!
//! It also PRUNES the inverse: a **phantom** provisional record — an
//! authoritative-but-unconfirmed row written at spawn for a plain shell that
//! never ran a provider — with no live process AND no transcript. Pruning it
//! stops Phase 4's restore classifier from ever auto-`--resume`-ing a session
//! that never existed.
//!
//! ## Demotes, does not delete
//!
//! `poll_and_record_session` (the 45s/180s mtime race) is DEMOTED to a
//! last-resort backstop — still callable, no longer the primary identity path.
//! Per the plan's "no silent cap" note, every session recovered ONLY via this
//! reconcile is `log()`-ed at INFO so reconcile-only recoveries are observable.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::process_capture::process_tree::ProcessSnapshot;
use crate::session::session_lifecycle_store::{
    SessionLifecycleStore, TerminalSessionRecord, DEFAULT_PROVIDER, ORIGIN_RECONCILED,
};

/// One live PTY the reconcile considers, projected from
/// [`crate::terminal::manager::TerminalManager::list`].
#[derive(Debug, Clone)]
pub struct LivePty {
    /// Stable runner terminal id.
    pub terminal_id: String,
    /// The PTY child's pid (the shell, or `claude` itself for an agent pty).
    /// `None` when the manager couldn't resolve it — such a PTY can't be
    /// correlated and is skipped.
    pub pid: Option<u32>,
    /// Working directory the PTY was opened in (the transcript's project path).
    pub working_dir: String,
    /// Grid page this PTY belongs to.
    pub page_id: String,
    /// Tab title (best-effort label for a reconciled record).
    pub title: String,
}

/// A transcript candidate discovered on disk for a `(config_dir, project_path)`.
/// Mirrors the subset of [`crate::terminal::transcript::TranscriptSession`] the
/// correlation needs.
#[derive(Debug, Clone)]
pub struct TranscriptCandidate {
    pub session_id: String,
    pub config_dir: String,
    /// First-event timestamp as Unix epoch seconds (parsed from `started_at`).
    /// `None` when the transcript carried no parseable first timestamp — such a
    /// candidate can't be start-anchored and is rejected by the correlation.
    pub started_at_unix: Option<i64>,
}

/// What the reconcile decided for one live PTY / record (pure outcome, so the
/// correlation is unit-testable without a live process table or disk).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconcileAction {
    /// A confident post-start transcript matched this live PTY — write/refresh a
    /// `reconciled`+confirmed record under `session_id`. Carries the chosen
    /// transcript's `config_dir` so the record's resume env is correct.
    Reconcile {
        terminal_id: String,
        session_id: String,
        config_dir: String,
    },
    /// The PTY already has a confirmed authoritative (or reconciled) record, OR
    /// no confident transcript matched — leave it to the deterministic path /
    /// the mtime backstop. No write.
    LeaveAlone { terminal_id: String },
    /// A phantom provisional record (authoritative-but-unconfirmed, no live
    /// process, no transcript) — PRUNE it so it never auto-resumes.
    PrunePhantom { session_id: String },
}

/// Skew tolerance (seconds) for the "transcript started AFTER the process"
/// anchor. WMI/`/proc` creation times are second-granular and a provider can
/// write its first transcript record a beat before/after its own process-start
/// is observed, so accept a transcript whose first event is within this slack
/// before the process start, not strictly after.
const START_ANCHOR_SKEW_SECS: i64 = 5;

/// Pure correlation core: decide a [`ReconcileAction`] for one live PTY.
///
/// - If the PTY already has a CONFIRMED record (authoritative or reconciled),
///   leave it alone — the deterministic path owns it.
/// - Else pick the FRESHEST transcript candidate (latest `started_at`) whose
///   first event is at/after the PTY child's process start (minus skew) — that
///   is a session THIS process authored, not a foreign one merely appended to.
///   A match ⇒ `Reconcile`. No candidate qualifies ⇒ `LeaveAlone`.
///
/// `process_start_unix` is the PTY child's creation time (epoch seconds), looked
/// up from the [`ProcessSnapshot`]; `0`/unknown disables the start anchor (any
/// candidate qualifies — degrade to freshest, since we can't anchor).
pub fn decide_for_live_pty(
    pty: &LivePty,
    existing: Option<&TerminalSessionRecord>,
    process_start_unix: i64,
    candidates: &[TranscriptCandidate],
) -> ReconcileAction {
    // A confirmed record (the hook fired / a prior reconcile confirmed it) is
    // owned by the deterministic path — never re-bind it from transcripts.
    if let Some(rec) = existing {
        if rec.confirmed_at.is_some() {
            return ReconcileAction::LeaveAlone {
                terminal_id: pty.terminal_id.clone(),
            };
        }
    }

    // Freshest post-start candidate wins. The start anchor rejects a foreign
    // long-running session whose transcript merely got appended to in this
    // window (its first event predates this process) — the exact failure the
    // mtime race makes.
    let mut best: Option<&TranscriptCandidate> = None;
    for cand in candidates {
        let started = match cand.started_at_unix {
            Some(s) => s,
            None => continue, // can't anchor an undated transcript
        };
        // Anchor: the transcript must have begun at/after the process start
        // (minus skew). With an unknown process start (0), the anchor is a
        // no-op and we fall back to freshest.
        if process_start_unix > 0 && started + START_ANCHOR_SKEW_SECS < process_start_unix {
            continue;
        }
        if best
            .and_then(|b| b.started_at_unix)
            .map(|b| started > b)
            .unwrap_or(true)
        {
            best = Some(cand);
        }
    }

    match best {
        Some(cand) => ReconcileAction::Reconcile {
            terminal_id: pty.terminal_id.clone(),
            session_id: cand.session_id.clone(),
            config_dir: cand.config_dir.clone(),
        },
        None => ReconcileAction::LeaveAlone {
            terminal_id: pty.terminal_id.clone(),
        },
    }
}

/// Pure phantom detector: is `rec` a phantom provisional record that should be
/// pruned? True iff the record is authoritative-but-unconfirmed AND there is no
/// live PTY hosting it AND no transcript exists for it on disk.
///
/// `live_terminal_ids` is the set of currently-live PTY terminal ids;
/// `has_transcript` reports whether a transcript file exists for the record's
/// session id (injected so the check is testable without disk).
pub fn is_phantom_record(
    rec: &TerminalSessionRecord,
    live_terminal_ids: &HashSet<String>,
    has_transcript: bool,
) -> bool {
    // Only authoritative-but-PROVISIONAL rows are phantom candidates. A
    // confirmed row had a real hook; a reconciled row was already start-anchored
    // to a transcript; neither is a phantom.
    let authoritative = rec
        .origin
        .as_deref()
        .map(|o| o == crate::session::session_lifecycle_store::ORIGIN_AUTHORITATIVE)
        .unwrap_or(false);
    if !authoritative || rec.confirmed_at.is_some() {
        return false;
    }
    // A live PTY hosting it (by terminal id) means the session may still
    // confirm via a late hook — not a phantom yet.
    if live_terminal_ids.contains(&rec.terminal_id) {
        return false;
    }
    // The decisive signal: no transcript on disk ⇒ the pinned `--session-id`
    // was never used by a real provider ⇒ phantom shell.
    !has_transcript
}

/// Build the `reconciled`+confirmed record to write for a [`ReconcileAction::Reconcile`].
/// The id is the start-anchored transcript's session id; origin is `reconciled`
/// (recovered by a backstop, treated conservatively on the wire) but it IS
/// confirmed (a real transcript proves the session exists), so Phase 4's
/// classifier... treats reconciled rows as quarantine, not auto-resume — the
/// confirmation here is for liveness/observability + so a later identical reconcile
/// is idempotent, NOT to upgrade it to auto-resume. Zone defaults to the live
/// PTY's (unknown at reconcile time) — boot-restore rebuilds layout from the live
/// PTY set anyway.
fn reconciled_record(pty: &LivePty, session_id: &str, config_dir: &str) -> TerminalSessionRecord {
    TerminalSessionRecord {
        claude_session_id: session_id.to_string(),
        config_dir: Some(config_dir.to_string()),
        working_dir: Some(pty.working_dir.clone()),
        page_id: pty.page_id.clone(),
        zone_index: -1,
        title: Some(pty.title.clone()),
        terminal_id: pty.terminal_id.clone(),
        opened_at: 0,
        last_seen_at: 0,
        state: "open".to_string(),
        closed_at: None,
        close_reason: None,
        provider: DEFAULT_PROVIDER.to_string(),
        origin: Some(ORIGIN_RECONCILED.to_string()),
        restore_pending_at: None,
        confirmed_at: None,
    }
}

/// Lazily-evaluated transcript discovery for a `(working_dir)` — finds the
/// candidate transcripts the correlation ranks. Injected into [`run_reconcile`]
/// so the boot driver can wire the real disk scan while tests inject fixtures.
pub trait TranscriptIndex {
    /// Candidate transcripts for the given project working dir (across every
    /// known config dir). Empty when nothing on disk matches.
    fn candidates_for(&self, working_dir: &str) -> Vec<TranscriptCandidate>;
    /// Whether ANY transcript exists for `session_id` (used by the phantom
    /// detector). Cheap existence probe.
    fn transcript_exists(&self, session_id: &str, working_dir: Option<&str>) -> bool;
}

/// Run the reconcile pass against the store. Pure orchestration over the
/// injected live-PTY set + process snapshot + transcript index, so the whole
/// flow is unit-testable; the boot driver ([`run_at_boot`]) wires the real
/// sources.
///
/// Returns the actions taken (for logging/tests). Side effects: each
/// `Reconcile` writes+confirms a `reconciled` record; each `PrunePhantom`
/// removes the row. `LeaveAlone` is a no-op.
pub fn run_reconcile<I: TranscriptIndex>(
    store: &SessionLifecycleStore,
    live: &[LivePty],
    snapshot: &ProcessSnapshot,
    index: &I,
) -> Vec<ReconcileAction> {
    let live_terminal_ids: HashSet<String> =
        live.iter().map(|p| p.terminal_id.clone()).collect();
    let mut actions = Vec::new();

    // 1) Reconcile live PTYs lacking a confirmed record.
    for pty in live {
        let existing = store.find_open_by_terminal(&pty.terminal_id);
        let process_start_unix = pty
            .pid
            .and_then(|pid| snapshot.creation_times.get(&pid).copied())
            .unwrap_or(0);
        let candidates = index.candidates_for(&pty.working_dir);
        let action =
            decide_for_live_pty(pty, existing.as_ref(), process_start_unix, &candidates);
        match &action {
            ReconcileAction::Reconcile {
                session_id,
                config_dir,
                ..
            } => {
                store.record_open(reconciled_record(pty, session_id, config_dir));
                // A real transcript proves the session exists — confirm it so a
                // later identical reconcile is idempotent and the liveness poll
                // treats it as a real session.
                store.confirm_session(session_id);
                // "No silent cap": a session recovered ONLY via reconcile is
                // observable (the shim-bypass edge, plan §7).
                tracing::info!(
                    terminal_id = %pty.terminal_id,
                    claude_session = %session_id,
                    working_dir = %pty.working_dir,
                    "session reconcile: recovered a shim-bypassed session via process-start-anchored transcript correlation"
                );
            }
            ReconcileAction::LeaveAlone { .. } => {}
            ReconcileAction::PrunePhantom { .. } => {}
        }
        actions.push(action);
    }

    // 2) Prune phantom provisional records (no live PTY, no transcript).
    for rec in store.open_records() {
        let has_transcript =
            index.transcript_exists(&rec.claude_session_id, rec.working_dir.as_deref());
        if is_phantom_record(&rec, &live_terminal_ids, has_transcript) {
            store.remove_session(&rec.claude_session_id);
            tracing::info!(
                claude_session = %rec.claude_session_id,
                terminal_id = %rec.terminal_id,
                "session reconcile: pruned phantom provisional record (authoritative-but-unconfirmed, no live process, no transcript)"
            );
            actions.push(ReconcileAction::PrunePhantom {
                session_id: rec.claude_session_id,
            });
        }
    }

    actions
}

// ---------------------------------------------------------------------------
// Boot driver — wires the real live-PTY set, process snapshot, and disk scan.
// ---------------------------------------------------------------------------

/// Real [`TranscriptIndex`] backed by [`crate::terminal::transcript`] — scans
/// every known Claude config dir for transcripts of a project path.
pub struct DiskTranscriptIndex {
    config_dirs: Vec<PathBuf>,
}

impl DiskTranscriptIndex {
    pub fn discover() -> Self {
        Self {
            config_dirs: crate::terminal::transcript::find_claude_config_dirs(),
        }
    }
}

impl TranscriptIndex for DiskTranscriptIndex {
    fn candidates_for(&self, working_dir: &str) -> Vec<TranscriptCandidate> {
        let mut out = Vec::new();
        for dir in &self.config_dirs {
            let sessions = match crate::terminal::transcript::list_sessions(dir, working_dir) {
                Ok(s) => s,
                Err(_) => continue,
            };
            for s in sessions {
                out.push(TranscriptCandidate {
                    session_id: s.session_id,
                    config_dir: s.config_dir,
                    started_at_unix: s
                        .started_at
                        .as_deref()
                        .and_then(parse_iso_to_unix_secs),
                });
            }
        }
        out
    }

    fn transcript_exists(&self, session_id: &str, working_dir: Option<&str>) -> bool {
        let Some(working_dir) = working_dir else {
            return false;
        };
        self.config_dirs.iter().any(|dir| {
            crate::terminal::transcript::session_transcript_path(dir, working_dir, session_id)
                .exists()
        })
    }
}

/// Parse an ISO-8601 timestamp into Unix epoch seconds (best-effort).
fn parse_iso_to_unix_secs(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp())
}

/// Boot entrypoint: take a fresh process snapshot, project the live PTY set, and
/// run the reconcile against the store. Fail-open — any failure to snapshot or
/// scan degrades to "did nothing", never worse than today's behavior.
pub async fn run_at_boot(store: &SessionLifecycleStore, live: Vec<LivePty>) {
    if live.is_empty() && store.open_records().is_empty() {
        return; // nothing to do
    }
    let snapshot =
        crate::process_capture::process_tree::snapshot_process_table_public().await;
    let index = DiskTranscriptIndex::discover();
    let actions = run_reconcile(store, &live, &snapshot, &index);
    let reconciled = actions
        .iter()
        .filter(|a| matches!(a, ReconcileAction::Reconcile { .. }))
        .count();
    let pruned = actions
        .iter()
        .filter(|a| matches!(a, ReconcileAction::PrunePhantom { .. }))
        .count();
    if reconciled > 0 || pruned > 0 {
        tracing::info!(
            reconciled,
            pruned,
            live = live.len(),
            "session reconcile: boot pass complete"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::session_lifecycle_store::{ORIGIN_AUTHORITATIVE, ORIGIN_RECONCILED};
    use std::collections::HashMap;
    use tempfile::tempdir;

    fn pty(id: &str, pid: Option<u32>, wd: &str) -> LivePty {
        LivePty {
            terminal_id: id.to_string(),
            pid,
            working_dir: wd.to_string(),
            page_id: "default".to_string(),
            title: "T".to_string(),
        }
    }

    fn cand(id: &str, cfg: &str, started: Option<i64>) -> TranscriptCandidate {
        TranscriptCandidate {
            session_id: id.to_string(),
            config_dir: cfg.to_string(),
            started_at_unix: started,
        }
    }

    /// A fake transcript index over in-memory fixtures.
    struct FakeIndex {
        by_wd: HashMap<String, Vec<TranscriptCandidate>>,
        existing_ids: HashSet<String>,
    }
    impl TranscriptIndex for FakeIndex {
        fn candidates_for(&self, working_dir: &str) -> Vec<TranscriptCandidate> {
            self.by_wd.get(working_dir).cloned().unwrap_or_default()
        }
        fn transcript_exists(&self, session_id: &str, _wd: Option<&str>) -> bool {
            self.existing_ids.contains(session_id)
        }
    }

    /// The freshest POST-START transcript wins — a foreign session whose first
    /// event predates the process start is rejected (the mtime-race failure).
    #[test]
    fn decide_picks_freshest_post_start_and_rejects_pre_start() {
        let p = pty("term-1", Some(100), "C:/repo");
        let process_start = 1_000;
        let candidates = vec![
            // Foreign: started BEFORE the process — must be rejected even though
            // it could be the newest by mtime.
            cand("foreign", "C:/cfg", Some(500)),
            // Ours: started just after the process.
            cand("ours-old", "C:/cfg", Some(1_010)),
            cand("ours-new", "C:/cfg", Some(2_000)),
        ];
        let action = decide_for_live_pty(&p, None, process_start, &candidates);
        assert_eq!(
            action,
            ReconcileAction::Reconcile {
                terminal_id: "term-1".to_string(),
                session_id: "ours-new".to_string(),
                config_dir: "C:/cfg".to_string(),
            },
            "freshest transcript that began after the process start wins"
        );
    }

    /// No post-start candidate ⇒ LeaveAlone (don't bind a foreign session).
    #[test]
    fn decide_leaves_alone_when_only_pre_start_candidates() {
        let p = pty("term-1", Some(100), "C:/repo");
        let candidates = vec![cand("foreign", "C:/cfg", Some(500))];
        let action = decide_for_live_pty(&p, None, 1_000, &candidates);
        assert_eq!(
            action,
            ReconcileAction::LeaveAlone {
                terminal_id: "term-1".to_string()
            }
        );
    }

    /// A confirmed record is owned by the deterministic path — never re-bound.
    #[test]
    fn decide_leaves_confirmed_record_alone() {
        let p = pty("term-1", Some(100), "C:/repo");
        let mut rec = reconciled_record(&p, "already", "C:/cfg");
        rec.confirmed_at = Some(123);
        let candidates = vec![cand("ours", "C:/cfg", Some(2_000))];
        let action = decide_for_live_pty(&p, Some(&rec), 1_000, &candidates);
        assert_eq!(
            action,
            ReconcileAction::LeaveAlone {
                terminal_id: "term-1".to_string()
            },
            "a confirmed record is never re-bound from transcripts"
        );
    }

    /// Unknown process start (0) disables the anchor — degrade to freshest.
    #[test]
    fn decide_degrades_to_freshest_when_start_unknown() {
        let p = pty("term-1", None, "C:/repo");
        let candidates = vec![cand("a", "C:/cfg", Some(500)), cand("b", "C:/cfg", Some(900))];
        let action = decide_for_live_pty(&p, None, 0, &candidates);
        assert_eq!(
            action,
            ReconcileAction::Reconcile {
                terminal_id: "term-1".to_string(),
                session_id: "b".to_string(),
                config_dir: "C:/cfg".to_string(),
            }
        );
    }

    /// Phantom detector: authoritative + unconfirmed + no live PTY + no
    /// transcript ⇒ phantom.
    #[test]
    fn is_phantom_true_only_for_authoritative_provisional_orphan() {
        let p = pty("dead-term", None, "C:/repo");
        let mut rec = reconciled_record(&p, "phantom", "C:/cfg");
        rec.origin = Some(ORIGIN_AUTHORITATIVE.to_string());
        rec.confirmed_at = None;
        let no_live: HashSet<String> = HashSet::new();

        // No live PTY, no transcript ⇒ phantom.
        assert!(is_phantom_record(&rec, &no_live, false));
        // Transcript exists ⇒ NOT a phantom (real session).
        assert!(!is_phantom_record(&rec, &no_live, true));
        // Live PTY hosting it ⇒ NOT a phantom (may confirm via late hook).
        let live: HashSet<String> = [rec.terminal_id.clone()].into_iter().collect();
        assert!(!is_phantom_record(&rec, &live, false));
        // Confirmed ⇒ NOT a phantom.
        let mut confirmed = rec.clone();
        confirmed.confirmed_at = Some(1);
        assert!(!is_phantom_record(&confirmed, &no_live, false));
        // Reconciled origin ⇒ NOT a phantom (already start-anchored).
        let mut reconciled = rec.clone();
        reconciled.origin = Some(ORIGIN_RECONCILED.to_string());
        assert!(!is_phantom_record(&reconciled, &no_live, false));
    }

    /// End-to-end against a real store: a live shim-bypassed PTY gets a
    /// reconciled+confirmed record; a phantom provisional row is pruned.
    #[test]
    fn run_reconcile_recovers_live_and_prunes_phantom() {
        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();

        // A phantom provisional record (spawn-time pin for a plain shell that
        // never ran a provider): authoritative, unconfirmed, terminal long gone.
        let phantom = TerminalSessionRecord {
            claude_session_id: "phantom".to_string(),
            config_dir: None,
            working_dir: Some("C:/repo".to_string()),
            page_id: "default".to_string(),
            zone_index: -1,
            title: Some("shell".to_string()),
            terminal_id: "gone-term".to_string(),
            opened_at: 0,
            last_seen_at: 0,
            state: "open".to_string(),
            closed_at: None,
            close_reason: None,
            provider: DEFAULT_PROVIDER.to_string(),
            origin: Some(ORIGIN_AUTHORITATIVE.to_string()),
            restore_pending_at: None,
            confirmed_at: None,
        };
        store.record_open(phantom);

        // One live PTY with no record — a shim-bypassed claude. Its child
        // process start = 1000; a post-start transcript exists.
        let live = vec![pty("live-term", Some(4242), "C:/repo")];
        let mut snapshot = ProcessSnapshot::default();
        snapshot.creation_times.insert(4242, 1_000);

        let mut by_wd = HashMap::new();
        by_wd.insert(
            "C:/repo".to_string(),
            vec![cand("real-sess", "C:/cfg", Some(1_500))],
        );
        // The phantom has NO transcript; the reconciled one does.
        let existing_ids: HashSet<String> = ["real-sess".to_string()].into_iter().collect();
        let index = FakeIndex { by_wd, existing_ids };

        let actions = run_reconcile(&store, &live, &snapshot, &index);

        // The live PTY was reconciled.
        assert!(actions.iter().any(|a| matches!(
            a,
            ReconcileAction::Reconcile { session_id, .. } if session_id == "real-sess"
        )));
        let recovered = store.get("real-sess").expect("reconciled record written");
        assert_eq!(recovered.origin.as_deref(), Some(ORIGIN_RECONCILED));
        assert!(
            recovered.confirmed_at.is_some(),
            "a reconciled record with a real transcript is confirmed"
        );
        assert_eq!(recovered.terminal_id, "live-term");

        // The phantom was pruned.
        assert!(
            actions.iter().any(|a| matches!(
                a,
                ReconcileAction::PrunePhantom { session_id } if session_id == "phantom"
            )),
            "phantom pruned"
        );
        assert!(store.get("phantom").is_none(), "phantom removed from store");
    }

    /// A reconcile that finds the SAME transcript again is idempotent (the
    /// confirmed reconciled row is left alone the second time).
    #[test]
    fn run_reconcile_is_idempotent_on_second_pass() {
        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();
        let live = vec![pty("live-term", Some(4242), "C:/repo")];
        let mut snapshot = ProcessSnapshot::default();
        snapshot.creation_times.insert(4242, 1_000);
        let mut by_wd = HashMap::new();
        by_wd.insert(
            "C:/repo".to_string(),
            vec![cand("real-sess", "C:/cfg", Some(1_500))],
        );
        let existing_ids: HashSet<String> = ["real-sess".to_string()].into_iter().collect();
        let index = FakeIndex { by_wd, existing_ids };

        run_reconcile(&store, &live, &snapshot, &index);
        // The reconciled record now carries the LIVE terminal id, so the 2nd
        // pass finds it confirmed-by-terminal and leaves it alone.
        let store2_actions = {
            // Re-point the record's terminal_id to the live PTY (record_open did
            // that already) so find_open_by_terminal returns it.
            run_reconcile(&store, &live, &snapshot, &index)
        };
        assert!(
            store2_actions.iter().any(|a| matches!(
                a,
                ReconcileAction::LeaveAlone { terminal_id } if terminal_id == "live-term"
            )),
            "second pass leaves the now-confirmed reconciled record alone"
        );
        // Still exactly one row for the session.
        assert!(store.get("real-sess").is_some());
    }
}
