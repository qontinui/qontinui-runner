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
    let live_terminal_ids: HashSet<String> = live.iter().map(|p| p.terminal_id.clone()).collect();
    let mut actions = Vec::new();

    // 1) Reconcile live PTYs lacking a confirmed record.
    for pty in live {
        let existing = store.find_open_by_terminal(&pty.terminal_id);
        let process_start_unix = pty
            .pid
            .and_then(|pid| snapshot.creation_times.get(&pid).copied())
            .unwrap_or(0);
        let candidates = index.candidates_for(&pty.working_dir);
        let action = decide_for_live_pty(pty, existing.as_ref(), process_start_unix, &candidates);
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
                    started_at_unix: s.started_at.as_deref().and_then(parse_iso_to_unix_secs),
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

/// Run ONE reconcile pass against an ALREADY-TAKEN process snapshot. Builds the
/// disk transcript index, runs the correlation, prunes phantoms, and logs the
/// counts. Shared by the boot driver ([`run_at_boot`]) and the recurring
/// session-lifecycle poll loop so a post-boot capture-miss is recovered within
/// one poll interval — while the PTY is still live — instead of only at the next
/// boot.
///
/// `context` tags the INFO log ("boot" / "periodic") so a reconcile-only
/// recovery is attributable to the pass that made it (the "no silent cap"
/// observability requirement). Fail-open: a scan failure degrades to a no-op.
pub fn run_reconcile_pass(
    store: &SessionLifecycleStore,
    live: &[LivePty],
    snapshot: &ProcessSnapshot,
    context: &str,
) {
    let index = DiskTranscriptIndex::discover();
    let actions = run_reconcile(store, live, snapshot, &index);
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
            context,
            "session reconcile: pass complete"
        );
    }
}

/// Boot entrypoint: take a fresh process snapshot, project the live PTY set, and
/// run the reconcile against the store. Fail-open — any failure to snapshot or
/// scan degrades to "did nothing", never worse than today's behavior.
pub async fn run_at_boot(store: &SessionLifecycleStore, live: Vec<LivePty>) {
    if live.is_empty() && store.open_records().is_empty() {
        return; // nothing to do
    }
    let snapshot = crate::process_capture::process_tree::snapshot_process_table_public().await;
    run_reconcile_pass(store, &live, &snapshot, "boot");
}

// ---------------------------------------------------------------------------
// Disk-only transcript-derived restore net (session-restore-redesign
// Phase 3 / G3).
//
// The boot-restore recovery layer (`terminal_session_list_open` →
// `restorable_records`) is a projection of the REGISTRY, so it inherits every
// registry gap. A session that was LIVE at crash but that the registry never
// captured — the spawn-record AND the provider hook both missed, AND the crash
// beat the next reconcile poll (which needs a LIVE PTY it no longer has) — has
// no restorable row and is silently lost. The process-start-anchored reconcile
// above cannot recover it either: with the runner dead there is no live PTY to
// correlate. This net recovers it from the DISK side instead: scan every config
// dir for transcripts recently active, and offer the registry-ABSENT ones as
// QUARANTINE-tier candidates (weak provenance — the frontend gates them behind
// the verified-resume handshake, never blind `--resume`). The account is the
// config dir that holds each transcript — enumerated DYNAMICALLY via
// `find_claude_config_dirs`, never a hardcoded account list (the defect
// `snapshot.py` has).
// ---------------------------------------------------------------------------

/// Liveness window for the disk-only restore net. A transcript whose LAST
/// ON-DISK ACTIVITY (file mtime) is within this window before `now` is treated
/// as a crash-recovery candidate — a session that was plausibly live when the
/// runner died but that the registry never captured. A transcript OLDER than
/// this is a FINISHED session the operator walked away from; resurrecting it on
/// every boot would be noise (and could re-offer stale foreign sessions), so it
/// is excluded — do NOT resurrect ancient sessions.
///
/// 6h is a defensible middle ground: comfortably longer than any plausible
/// crash→reboot gap (a genuine capture-miss during an active work session is
/// still caught after a lunch-break-length outage), yet short enough that
/// yesterday's finished sessions do not re-surface. The window only bounds how
/// much is OFFERED; it never auto-resumes anything. The backstop for a stale
/// candidate that slips through is the VERIFIED-RESUME handshake: a disk-only
/// candidate is `origin=reconciled`+unconfirmed, so the frontend quarantines it
/// behind a one-click operator confirm that TYPES the resume and verifies the
/// handshake (parking on failure), never a blind `--resume`.
pub const DISK_ONLY_RESTORE_WINDOW_MS: i64 = 6 * 60 * 60 * 1000;

/// Build the quarantine-tier [`TerminalSessionRecord`] for one disk-only
/// transcript candidate: `origin = reconciled`, `confirmed_at = None` (weak
/// provenance — the frontend never blind-`--resume`s it), `config_dir` = the
/// transcript's account, `working_dir` = the real cwd recovered from the
/// transcript, and DEFAULT page/zone (`page_id = "default"`, `zone_index = -1`)
/// since there is no recorded layout for a session the registry never saw —
/// boot-restore rebuilds it. `terminal_id` is empty (no live PTY hosts it; the
/// frontend creates a fresh terminal). `last_seen_at`/`opened_at` carry the
/// transcript's last-activity so the record is honest about its recency.
fn disk_only_record(t: &crate::terminal::transcript::RecentTranscript) -> TerminalSessionRecord {
    TerminalSessionRecord {
        claude_session_id: t.session_id.clone(),
        config_dir: Some(t.config_dir.clone()),
        working_dir: Some(t.working_dir.clone()),
        page_id: "default".to_string(),
        zone_index: -1,
        title: None,
        terminal_id: String::new(),
        opened_at: t.last_activity_ms,
        last_seen_at: t.last_activity_ms,
        state: "open".to_string(),
        closed_at: None,
        close_reason: None,
        provider: DEFAULT_PROVIDER.to_string(),
        origin: Some(ORIGIN_RECONCILED.to_string()),
        restore_pending_at: None,
        confirmed_at: None,
    }
}

/// Pure selection core for the disk-only restore net — unit-testable without
/// disk. Given the recently-active on-disk transcripts (each with its
/// last-activity ms), the set of session ids the registry ALREADY tracks, and
/// `now`, return the disk-only candidates to ADD.
///
/// Filters, in order:
/// - WINDOW: keep only transcripts within [`DISK_ONLY_RESTORE_WINDOW_MS`] of
///   `now` (older = finished session, excluded).
/// - REGISTRY DEDUP: drop any id in `registry_ids` — a restorable registry row
///   wins (real page/zone/layout), and a non-restorable registry row already
///   encodes a "do not restore" decision the net must honor (see
///   [`SessionLifecycleStore::all_ids`]). Only registry-ABSENT ids survive.
/// - CROSS-ACCOUNT DEDUP: the same session id under two config dirs is admitted
///   ONCE (first wins) so a transcript copied between accounts is not offered
///   twice.
pub fn select_disk_only_candidates(
    recent: &[crate::terminal::transcript::RecentTranscript],
    registry_ids: &HashSet<String>,
    now_ms: i64,
) -> Vec<TerminalSessionRecord> {
    let mut out = Vec::new();
    let mut emitted: HashSet<&str> = HashSet::new();
    for t in recent {
        if now_ms.saturating_sub(t.last_activity_ms) > DISK_ONLY_RESTORE_WINDOW_MS {
            continue; // older than the liveness window — a finished session
        }
        if registry_ids.contains(&t.session_id) {
            continue; // the registry already tracks it — registry wins
        }
        if !emitted.insert(t.session_id.as_str()) {
            continue; // same id under a second config dir — first wins
        }
        out.push(disk_only_record(t));
    }
    out
}

/// Build the disk-only restore candidates by SCANNING every Claude config dir
/// for transcripts recently active (within [`DISK_ONLY_RESTORE_WINDOW_MS`]) and
/// registry-ABSENT, returning them as quarantine-tier records to UNION into the
/// boot-restore set (`terminal_session_list_open`). This is the transcript-
/// DERIVED recovery net (G3): a session live at crash but never captured by the
/// registry is still restorable — under the correct account, derived
/// DYNAMICALLY from the config dir that holds its transcript.
///
/// Fail-open: any scan failure yields fewer (or zero) candidates, so the caller
/// degrades to exactly today's registry-only restorable set — never worse.
pub fn disk_only_restore_candidates(
    now_ms: i64,
    registry_ids: &HashSet<String>,
) -> Vec<TerminalSessionRecord> {
    let config_dirs = crate::terminal::transcript::find_claude_config_dirs();
    let mut recent = Vec::new();
    for dir in &config_dirs {
        recent.extend(
            crate::terminal::transcript::list_recent_sessions_all_projects(
                dir,
                now_ms,
                DISK_ONLY_RESTORE_WINDOW_MS,
            ),
        );
    }
    select_disk_only_candidates(&recent, registry_ids, now_ms)
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
        let candidates = vec![
            cand("a", "C:/cfg", Some(500)),
            cand("b", "C:/cfg", Some(900)),
        ];
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
        let index = FakeIndex {
            by_wd,
            existing_ids,
        };

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
        let index = FakeIndex {
            by_wd,
            existing_ids,
        };

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

    // ── Disk-only restore net (G3) — pure selection ─────────────────────────

    fn recent(id: &str, cfg: &str, wd: &str, last_ms: i64) -> crate::terminal::transcript::RecentTranscript {
        crate::terminal::transcript::RecentTranscript {
            session_id: id.to_string(),
            config_dir: cfg.to_string(),
            working_dir: wd.to_string(),
            last_activity_ms: last_ms,
        }
    }

    /// A registry-ABSENT, recently-active transcript is offered as a
    /// quarantine-tier candidate under the ACCOUNT (config dir) that holds it;
    /// a transcript OLDER than the window is excluded; an id ALREADY in the
    /// registry is not duplicated.
    #[test]
    fn select_disk_only_window_dedup_and_account() {
        let now = 10 * DISK_ONLY_RESTORE_WINDOW_MS; // large, avoids underflow
        let registry_ids: HashSet<String> = ["in-registry".to_string()].into_iter().collect();
        let recents = vec![
            // Recent + registry-absent under account A → INCLUDED.
            recent("fresh-a", "C:/cfg-A", "C:/repoA", now - 1_000),
            // Recent but ALREADY in the registry restorable/known set → excluded.
            recent("in-registry", "C:/cfg-A", "C:/repoA", now - 1_000),
            // Older than the window → excluded (finished session).
            recent(
                "ancient",
                "C:/cfg-B",
                "C:/repoB",
                now - DISK_ONLY_RESTORE_WINDOW_MS - 1,
            ),
            // Exactly at the window boundary is still IN-window → included.
            recent("edge", "C:/cfg-B", "C:/repoB", now - DISK_ONLY_RESTORE_WINDOW_MS),
        ];

        let out = select_disk_only_candidates(&recents, &registry_ids, now);
        let ids: HashSet<&str> = out.iter().map(|r| r.claude_session_id.as_str()).collect();
        assert!(ids.contains("fresh-a"), "recent registry-absent included");
        assert!(ids.contains("edge"), "boundary transcript included");
        assert!(!ids.contains("in-registry"), "registry id not duplicated");
        assert!(!ids.contains("ancient"), "older-than-window excluded");

        // The included candidate is quarantine-tier under the correct account.
        let a = out
            .iter()
            .find(|r| r.claude_session_id == "fresh-a")
            .unwrap();
        assert_eq!(a.origin.as_deref(), Some(ORIGIN_RECONCILED));
        assert!(a.confirmed_at.is_none(), "disk-only candidate is unconfirmed");
        assert_eq!(
            a.config_dir.as_deref(),
            Some("C:/cfg-A"),
            "account derived from the transcript's config dir"
        );
        assert_eq!(a.working_dir.as_deref(), Some("C:/repoA"));
        assert_eq!(a.zone_index, -1, "no recorded layout — default zone");
        assert_eq!(a.page_id, "default");
        assert!(a.terminal_id.is_empty(), "no live PTY hosts it");
    }

    /// The same session id under two config dirs is offered ONCE (first wins) —
    /// a transcript copied between accounts is not duplicated in the restore
    /// set.
    #[test]
    fn select_disk_only_dedups_same_id_across_accounts() {
        let now = 10 * DISK_ONLY_RESTORE_WINDOW_MS;
        let registry_ids: HashSet<String> = HashSet::new();
        let recents = vec![
            recent("dup", "C:/cfg-A", "C:/repo", now - 1_000),
            recent("dup", "C:/cfg-B", "C:/repo", now - 2_000),
        ];
        let out = select_disk_only_candidates(&recents, &registry_ids, now);
        assert_eq!(out.len(), 1, "same id across accounts offered once");
        assert_eq!(
            out[0].config_dir.as_deref(),
            Some("C:/cfg-A"),
            "first-seen account wins"
        );
    }
}
