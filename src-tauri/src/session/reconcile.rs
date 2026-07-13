//! Continuous, claude-process-anchored, evidence-graded session binder
//! (session-restore-redesign Phase 4 + launch-agnostic session binding).
//!
//! ## Why this exists
//!
//! The deterministic identity path — the runner pre-pins `--session-id` at spawn
//! and records authoritatively, the provider's SessionStart hook CONFIRMS it —
//! covers every session the runner launches AND every hand-typed `claude`/`clg`
//! that goes through the PATH shim. It does NOT cover the documented
//! non-deterministic edge: an **absolute-path** / shim-bypassing `claude`
//! invocation. Those launch a real provider whose session id the runner never
//! pinned and whose hook (delivered via the shim's `--settings`) never attached
//! — so the terminal hosts a live, unrecorded (or merely provisional) session.
//!
//! This pass closes that gap WITHOUT the mtime-capture race that
//! [`crate::commands::terminal::poll_and_record_session`] suffers. It runs from
//! the 45s liveness poll ([`run_reconcile_pass`] with `context = "periodic"`,
//! reusing that tick's process snapshot) and once at boot ([`run_at_boot`]), so
//! ANY session — however launched — converges on a recorded, restore-eligible
//! identity within one poll tick of writing its first transcript line.
//!
//! ## Evidence ladder (strongest first)
//!
//! 1. **Pinned / typed / hooked** — the deterministic path. A CONFIRMED record is
//!    owned by it and never re-bound here.
//! 2. **Cmdline-extracted** (rung 2): the live claude process's own argv carries
//!    `--session-id <uuid>` → that IS the identity. Bound `authoritative`,
//!    confirmed iff a transcript for that exact id already exists (else
//!    provisional until it does). No guessing. The cmdline query is TARGETED at
//!    exactly the anchor pids of unbound terminals — never a table-wide fetch —
//!    and fails open (unavailable ⇒ fall through to rung 3).
//! 3. **Observed** (rung 3): the claude descendant's **process start** anchors a
//!    transcript correlation — a candidate qualifies only when its FIRST event is
//!    at/after that start (minus skew). A UNIQUENESS gate is the safety: bind only
//!    when EXACTLY ONE candidate passes; >1 ⇒ skip this tick (a cross-terminal
//!    claimed-id set removes ids already bound to other live terminals, so two
//!    same-second launches converge across ticks). The transcript proves the
//!    session exists ⇒ `origin:"observed"` + confirmed ⇒ auto-resume eligible.
//! 4. **Reconciled** (rung 4): the mtime guess, the disk-only recovery net below,
//!    AND the degraded rung-3 case where NO start anchor could be resolved (the
//!    process table yielded neither a claude nor a shell creation time). Without
//!    the anchor the post-start filter is a no-op and "unique in this cwd" is the
//!    only evidence left — which is exactly the guess this rung exists to
//!    quarantine. Never auto-resumed. The grade follows the anchor, not the
//!    uniqueness gate.
//!
//! The anchor is the **claude descendant**, not the shell: anchoring on shell
//! start would admit a foreign same-cwd session started after the shell but
//! before claude. The shell pid stays the FALLBACK anchor when no claude image
//! resolves in the subtree.
//!
//! It also PRUNES the inverse: a **phantom** provisional record — an
//! authoritative-but-unconfirmed row written at spawn for a plain shell that
//! never ran a provider — with no live process AND no transcript. Pruning it
//! stops the restore classifier from ever auto-`--resume`-ing a session that
//! never existed.
//!
//! ## Demotes, does not delete
//!
//! `poll_and_record_session` (the 45s/180s mtime race) is DEMOTED to a
//! last-resort backstop — still callable, no longer the primary identity path.
//! Per the plan's "no silent cap" note, every session recovered ONLY via this
//! pass is `log()`-ed at INFO so binder-only recoveries are observable.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::process_capture::process_tree::ProcessSnapshot;
use crate::session::session_lifecycle_store::{
    SessionLifecycleStore, TerminalSessionRecord, DEFAULT_PROVIDER, ORIGIN_AUTHORITATIVE,
    ORIGIN_OBSERVED, ORIGIN_RECONCILED,
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
    /// The claude-image descendant pid that anchors this PTY's correlation (its
    /// `--session-id` cmdline + its process start). `None` when no claude image
    /// is present in the subtree — the shell `pid` above stays the fallback
    /// start anchor. Callers may leave this `None`: [`run_reconcile_pass`]
    /// resolves it from that pass's process snapshot.
    pub ai_pid: Option<u32>,
    /// The anchor process's creation time (epoch seconds). `None` (or `<= 0`)
    /// disables the start filter — the correlation then degrades to the shell
    /// anchor, and failing that to the uniqueness gate alone.
    pub ai_start_unix: Option<i64>,
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

/// Which origin a [`ReconcileAction::Bind`] records the session under — maps
/// directly to the store's `ORIGIN_*` consts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindOrigin {
    /// The runner KNOWS the id exactly — lifted from the anchor process's typed
    /// `--session-id` cmdline (rung 2). Auto-resume-eligible.
    Authoritative,
    /// A process-start-anchored, uniquely-correlated transcript bind (rung 3) —
    /// the transcript proves the session exists. A CONFIRMED observed row is
    /// auto-resume-eligible (unlike the conservative `reconciled` mtime guess).
    Observed,
    /// A uniquely-correlated transcript bind whose START ANCHOR WAS UNAVAILABLE
    /// (rung 4) — the process table gave us neither the claude nor the shell
    /// creation time, so the post-start filter could not run and the only
    /// evidence left is "exactly one transcript lives in this cwd". That is the
    /// mtime-guess class: it can name a foreign session, so it is graded
    /// `reconciled` and the frontend quarantines it behind the one-click
    /// confirm. Auto-resume MUST follow the anchor, not the uniqueness gate
    /// alone.
    Reconciled,
}

impl BindOrigin {
    /// The store `origin` const this maps to.
    pub fn as_origin_const(self) -> &'static str {
        match self {
            BindOrigin::Authoritative => ORIGIN_AUTHORITATIVE,
            BindOrigin::Observed => ORIGIN_OBSERVED,
            BindOrigin::Reconciled => ORIGIN_RECONCILED,
        }
    }
}

/// What the reconcile decided for one live PTY / record (pure outcome, so the
/// correlation is unit-testable without a live process table or disk).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconcileAction {
    /// Correlated this live PTY to a session identity — write/refresh a record
    /// under `session_id` with `origin`. `confirmed` = the identity is proven (a
    /// transcript exists for it), so the record is written CONFIRMED. An
    /// unconfirmed Bind (a typed `--session-id` whose transcript hasn't appeared
    /// yet) is written provisional. Carries the chosen transcript's `config_dir`
    /// (empty when not yet known).
    Bind {
        terminal_id: String,
        session_id: String,
        config_dir: String,
        origin: BindOrigin,
        confirmed: bool,
    },
    /// Rung 3 had MORE THAN ONE passing candidate — the observed bind can't be
    /// uniquely resolved this tick. Skip it (no write); a later tick, with a
    /// sibling claimed or a candidate aged out, may disambiguate.
    SkipAmbiguous { terminal_id: String },
    /// The PTY already has a confirmed record, OR no candidate qualified — leave
    /// it to the deterministic path / a later tick. No write.
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

/// Pure correlation core: decide a [`ReconcileAction`] for one live PTY, graded
/// by evidence strength.
///
/// - **Confirmed record** ⇒ `LeaveAlone` (the deterministic path owns it).
/// - **Rung 2 (cmdline):** `cmdline_session_id = Some(id)` ⇒ the anchor process
///   typed its own `--session-id`, so that IS the identity — `Bind`
///   `Authoritative`, `confirmed` iff a transcript for `id` already exists (else
///   provisional). No guessing.
/// - **Rung 3 (observed):** else correlate to `candidates` that are BOTH
///   post-start (`started + skew >= anchor_start_unix`) AND not already
///   `claimed_ids`. UNIQUENESS gate: exactly one passes ⇒ `Bind` `confirmed`;
///   >1 passes ⇒ `SkipAmbiguous` (retry next tick); 0 pass ⇒ `LeaveAlone`.
/// - **Rung 4 (degraded):** `anchor_start_unix <= 0` disables the start filter
///   (the uniqueness gate still applies), so the bind rests on "unique in this
///   cwd" alone. It is therefore graded `Reconciled` — QUARANTINED, not
///   auto-resumed. The grade tracks the anchor; only an anchored bind earns
///   `Observed`.
///
/// `anchor_start_unix` is the claude anchor's creation time (epoch seconds);
/// `claimed_ids` are session ids already bound to OTHER live terminals — removing
/// them lets two same-second launches converge across ticks (each terminal claims
/// one, disambiguating the other).
pub fn decide_bind_for_live_pty(
    pty: &LivePty,
    existing: Option<&TerminalSessionRecord>,
    anchor_start_unix: i64,
    cmdline_session_id: Option<&str>,
    candidates: &[TranscriptCandidate],
    claimed_ids: &HashSet<String>,
) -> ReconcileAction {
    // A confirmed record (the hook fired / a prior bind confirmed it) is owned
    // by the deterministic path — never re-bind it.
    if let Some(rec) = existing {
        if rec.confirmed_at.is_some() {
            return ReconcileAction::LeaveAlone {
                terminal_id: pty.terminal_id.clone(),
            };
        }
    }

    // RUNG 2 — the typed `--session-id` IS the identity. No correlation guess:
    // bind it authoritative. It's confirmed only if a real transcript for that
    // exact id is already on disk; otherwise it's provisional (the transcript
    // will appear a tick or two later and a hook / the next tick confirms it).
    if let Some(id) = cmdline_session_id {
        let matching = candidates.iter().find(|c| c.session_id == id);
        return ReconcileAction::Bind {
            terminal_id: pty.terminal_id.clone(),
            session_id: id.to_string(),
            config_dir: matching.map(|c| c.config_dir.clone()).unwrap_or_default(),
            origin: BindOrigin::Authoritative,
            confirmed: matching.is_some(),
        };
    }

    // RUNG 3 — observed. Keep only post-start, unclaimed candidates; the start
    // filter rejects a foreign long-running session whose transcript merely got
    // appended to in this window (its first event predates the anchor) — the
    // exact failure the mtime race makes. The uniqueness gate is the safety:
    // bind ONLY when exactly one survives.
    let passing: Vec<&TranscriptCandidate> = candidates
        .iter()
        .filter(|cand| {
            let post_start = anchor_start_unix <= 0
                || cand
                    .started_at_unix
                    .map(|s| s + START_ANCHOR_SKEW_SECS >= anchor_start_unix)
                    .unwrap_or(false);
            post_start && !claimed_ids.contains(&cand.session_id)
        })
        .collect();

    // The grade follows the EVIDENCE, not the uniqueness gate. With a live
    // anchor the bind is start-filtered and earns `observed` (auto-resume). With
    // no anchor the filter above was a no-op, so "unique in this cwd" is all we
    // have — the same guess `reconciled` exists to quarantine. Grading it
    // `observed` would let the weakest evidence take the strongest path.
    let correlated_origin = if anchor_start_unix > 0 {
        BindOrigin::Observed
    } else {
        BindOrigin::Reconciled
    };

    match passing.as_slice() {
        [cand] => ReconcileAction::Bind {
            terminal_id: pty.terminal_id.clone(),
            session_id: cand.session_id.clone(),
            config_dir: cand.config_dir.clone(),
            origin: correlated_origin,
            confirmed: true,
        },
        [] => ReconcileAction::LeaveAlone {
            terminal_id: pty.terminal_id.clone(),
        },
        _ => ReconcileAction::SkipAmbiguous {
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
    // confirmed row had a real hook; a reconciled/observed row was already
    // start-anchored to a transcript; neither is a phantom.
    let authoritative = rec
        .origin
        .as_deref()
        .map(|o| o == ORIGIN_AUTHORITATIVE)
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

/// Build the open record to write for a [`ReconcileAction::Bind`], stamped with
/// `origin` (`authoritative` for a rung-2 cmdline bind, `observed` for a rung-3
/// unique correlation) and, when `confirmed`, with `confirmed_at` ALREADY SET.
///
/// The confirmation is stamped HERE (rather than via a follow-up
/// [`SessionLifecycleStore::confirm_session`]) on purpose: `record_open`'s
/// single-tenant-terminal supersede fires for an AUTHORITATIVE **or CONFIRMED**
/// incoming bind, so an `observed` bind only retires the identity seam's
/// unconfirmed sibling if it arrives at `record_open` ALREADY confirmed. A
/// separate post-hoc `confirm_session` would flip the flag after the supersede
/// scan had already declined to run — leaving the orphan open (the exact class
/// this closes). Zone defaults to `-1` (unknown at bind time) — boot-restore
/// rebuilds layout from the live PTY set anyway.
fn bind_record(
    pty: &LivePty,
    session_id: &str,
    config_dir: &str,
    origin: &str,
    confirmed: bool,
) -> TerminalSessionRecord {
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
        origin: Some(origin.to_string()),
        restore_pending_at: None,
        confirmed_at: confirmed.then(|| chrono::Utc::now().timestamp_millis()),
    }
}

/// Lazily-evaluated transcript discovery for a `(working_dir)` — finds the
/// candidate transcripts the correlation ranks. Injected into [`run_reconcile`]
/// so the drivers can wire the real disk scan while tests inject fixtures.
pub trait TranscriptIndex {
    /// Candidate transcripts for the given project working dir (across every
    /// known config dir). Empty when nothing on disk matches.
    fn candidates_for(&self, working_dir: &str) -> Vec<TranscriptCandidate>;
    /// Whether ANY transcript exists for `session_id` (used by the phantom
    /// detector). Cheap existence probe.
    fn transcript_exists(&self, session_id: &str, working_dir: Option<&str>) -> bool;
}

/// Run the reconcile/bind pass against the store. Pure orchestration over the
/// injected live-PTY set + process snapshot + transcript index + per-anchor-pid
/// cmdline ids, so the whole flow is unit-testable; [`run_reconcile_pass`] wires
/// the real sources.
///
/// `cmdline_ids` maps an anchor pid ([`LivePty::ai_pid`]) to the `--session-id`
/// UUID parsed from that process's argv — the rung-2 evidence. An empty map (the
/// cmdline query failed, or found nothing) simply degrades every terminal to the
/// rung-3 transcript correlation: fail-open by construction.
///
/// Returns the actions taken (for logging/tests). Side effects: each `Bind`
/// writes a record (CONFIRMED when the bind is confirmed); each `PrunePhantom`
/// removes the row. `LeaveAlone` / `SkipAmbiguous` are no-ops.
pub fn run_reconcile<I: TranscriptIndex>(
    store: &SessionLifecycleStore,
    live: &[LivePty],
    snapshot: &ProcessSnapshot,
    index: &I,
    cmdline_ids: &HashMap<u32, String>,
) -> Vec<ReconcileAction> {
    let live_terminal_ids: HashSet<String> = live.iter().map(|p| p.terminal_id.clone()).collect();
    // Session ids already owned by a CONFIRMED open row can't be re-bound to a
    // different live terminal — seed the claim set so the uniqueness gate sees
    // them, and grow it as this pass binds more (two same-second launches then
    // converge: each terminal claims one, disambiguating the other).
    let mut claimed_ids: HashSet<String> = store
        .open_records()
        .into_iter()
        .filter(|r| r.confirmed_at.is_some())
        .map(|r| r.claude_session_id)
        .collect();
    let mut actions = Vec::new();

    // 1) Bind live PTYs lacking a confirmed record.
    for pty in live {
        let existing = store.find_open_by_terminal(&pty.terminal_id);
        // Steady state: a CONFIRMED terminal is owned by the deterministic path
        // — short-circuit BEFORE the disk scan, so a fully-bound fleet does zero
        // transcript work per tick.
        if existing
            .as_ref()
            .map(|r| r.confirmed_at.is_some())
            .unwrap_or(false)
        {
            actions.push(ReconcileAction::LeaveAlone {
                terminal_id: pty.terminal_id.clone(),
            });
            continue;
        }

        // Anchor on the CLAUDE descendant's start when resolved; fall back to
        // the shell pid's start (weaker, but better than no anchor at all).
        let anchor_start = pty.ai_start_unix.filter(|s| *s > 0).unwrap_or_else(|| {
            pty.pid
                .and_then(|pid| snapshot.creation_times.get(&pid).copied())
                .unwrap_or(0)
        });
        let cmdline_sid: Option<&str> = pty
            .ai_pid
            .and_then(|pid| cmdline_ids.get(&pid))
            .map(|s| s.as_str());
        let candidates = index.candidates_for(&pty.working_dir);
        let action = decide_bind_for_live_pty(
            pty,
            existing.as_ref(),
            anchor_start,
            cmdline_sid,
            &candidates,
            &claimed_ids,
        );
        match &action {
            ReconcileAction::Bind {
                session_id,
                config_dir,
                origin,
                confirmed,
                ..
            } => {
                // A confirmed bind is written CONFIRMED (see `bind_record`) so
                // `record_open`'s supersede retires the identity seam's
                // unconfirmed sibling on this terminal in the same write.
                store.record_open(bind_record(
                    pty,
                    session_id,
                    config_dir,
                    origin.as_origin_const(),
                    *confirmed,
                ));
                if *confirmed {
                    // Claim the id so no other live terminal in this pass binds
                    // it too.
                    claimed_ids.insert(session_id.clone());
                }
                match origin {
                    BindOrigin::Observed => {
                        // "No silent cap": a session recovered ONLY via the
                        // observed correlation is observable (the shim-bypass
                        // edge).
                        tracing::info!(
                            terminal_id = %pty.terminal_id,
                            claude_session = %session_id,
                            working_dir = %pty.working_dir,
                            confirmed = *confirmed,
                            "session binder: bound a launch-agnostic session via process-start-anchored transcript correlation"
                        );
                    }
                    BindOrigin::Authoritative => {
                        tracing::debug!(
                            terminal_id = %pty.terminal_id,
                            claude_session = %session_id,
                            confirmed = *confirmed,
                            "session binder: bound authoritative session from typed --session-id cmdline"
                        );
                    }
                    BindOrigin::Reconciled => {
                        // Degraded rung 4: no start anchor was resolvable, so the
                        // bind rests on the uniqueness gate alone and is written
                        // QUARANTINED. Logged at info (not debug) because a
                        // recurring one means the process table is failing to
                        // yield creation times — the anchor, not the bind, is the
                        // thing to fix.
                        tracing::info!(
                            terminal_id = %pty.terminal_id,
                            claude_session = %session_id,
                            working_dir = %pty.working_dir,
                            "session binder: no start anchor available — bound the unique cwd transcript as RECONCILED (quarantined, not auto-resumed)"
                        );
                    }
                }
            }
            ReconcileAction::SkipAmbiguous { terminal_id } => {
                tracing::debug!(
                    terminal_id = %terminal_id,
                    working_dir = %pty.working_dir,
                    "session binder: >1 candidate transcript — skipping ambiguous bind this pass"
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
// Drivers — wire the real live-PTY set, process snapshot, cmdline query, and
// disk scan.
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

/// Resolve each live PTY's CLAUDE anchor — the claude-image descendant pid + its
/// process start — from an ALREADY-TAKEN snapshot (no extra sweep). The shell pid
/// is only a fallback: the correlation must anchor on when CLAUDE started, not on
/// when its shell did (a foreign same-cwd session started after the shell but
/// before claude would otherwise qualify). A PTY whose subtree hosts no claude
/// image keeps `ai_pid: None` and degrades to the shell anchor.
fn anchor_live_ptys(live: &[LivePty], snapshot: &ProcessSnapshot) -> Vec<LivePty> {
    let mut out = live.to_vec();
    for pty in out.iter_mut() {
        if pty.ai_pid.is_some() {
            continue; // the caller already resolved it
        }
        let Some(root) = pty.pid else { continue };
        if let Some((ai_pid, ai_start)) =
            crate::process_capture::process_tree::claude_anchor_in_subtree(root, snapshot)
        {
            pty.ai_pid = Some(ai_pid);
            // An unknown (0) creation time is NOT an anchor — leave it None so
            // the shell-start fallback applies.
            pty.ai_start_unix = (ai_start > 0).then_some(ai_start);
        }
    }
    out
}

/// Run ONE reconcile/bind pass against an ALREADY-TAKEN process snapshot: resolve
/// the claude anchors, issue the TARGETED cmdline query for the unbound
/// terminals' anchor pids only, build the disk transcript index, run the graded
/// correlation, prune phantoms, and log the counts.
///
/// This is the SINGLE continuous entrypoint — shared by the boot driver
/// ([`run_at_boot`]) and the recurring 45s session-lifecycle poll (which passes
/// its own per-tick snapshot + live list, so the binder adds NO extra process
/// sweep). A post-boot capture-miss is therefore recovered within one poll
/// interval — while the PTY is still live — instead of only at the next boot.
///
/// Steady-state cheap: a terminal with a CONFIRMED record is skipped before any
/// disk work, and the cmdline query is skipped entirely when no unbound terminal
/// has a claude anchor (an empty pid list spawns no subprocess).
///
/// `context` tags the INFO log ("boot" / "periodic") so a binder-only recovery is
/// attributable to the pass that made it (the "no silent cap" observability
/// requirement). Fail-open throughout: a snapshot / cmdline / scan failure
/// degrades to "bound nothing this pass", never worse than today's behavior and
/// never a panic in the poll loop.
///
/// Returns the actions taken so the caller can propagate binds to the frontend
/// (`session-bound` tab-stamp events — see [`session_bound_payloads`]); the
/// registry writes have already happened by the time this returns.
pub async fn run_reconcile_pass(
    store: &SessionLifecycleStore,
    live: &[LivePty],
    snapshot: &ProcessSnapshot,
    context: &str,
) -> Vec<ReconcileAction> {
    let live = anchor_live_ptys(live, snapshot);

    // Rung 2 — ONE targeted cmdline query, for exactly the anchor pids of the
    // terminals that still need binding. Never a table-wide fetch; an empty list
    // means no query at all. Fail-open: an unavailable cmdline just leaves the
    // map empty and every terminal falls through to the rung-3 transcript anchor.
    let anchor_pids: Vec<u32> = live
        .iter()
        .filter(|pty| {
            !store
                .find_open_by_terminal(&pty.terminal_id)
                .map(|r| r.confirmed_at.is_some())
                .unwrap_or(false)
        })
        .filter_map(|pty| pty.ai_pid)
        .collect();
    let cmdline_ids: HashMap<u32, String> = if anchor_pids.is_empty() {
        HashMap::new()
    } else {
        crate::process_capture::process_tree::command_lines_for_pids(&anchor_pids)
            .await
            .iter()
            .filter_map(|(pid, cl)| {
                crate::process_capture::process_tree::parse_session_id_from_cmdline(cl)
                    .map(|sid| (*pid, sid))
            })
            .collect()
    };

    let index = DiskTranscriptIndex::discover();
    let actions = run_reconcile(store, &live, snapshot, &index, &cmdline_ids);

    let bound = actions
        .iter()
        .filter(|a| matches!(a, ReconcileAction::Bind { .. }))
        .count();
    let observed = actions
        .iter()
        .filter(|a| {
            matches!(
                a,
                ReconcileAction::Bind {
                    origin: BindOrigin::Observed,
                    ..
                }
            )
        })
        .count();
    let ambiguous = actions
        .iter()
        .filter(|a| matches!(a, ReconcileAction::SkipAmbiguous { .. }))
        .count();
    let pruned = actions
        .iter()
        .filter(|a| matches!(a, ReconcileAction::PrunePhantom { .. }))
        .count();
    if bound > 0 || pruned > 0 || ambiguous > 0 {
        tracing::info!(
            bound,
            observed,
            ambiguous,
            pruned,
            live = live.len(),
            context,
            "session reconcile: pass complete"
        );
    }
    actions
}

/// Boot entrypoint: take a fresh process snapshot, project the live PTY set, and
/// run the reconcile against the store. Fail-open — any failure to snapshot or
/// scan degrades to "did nothing", never worse than today's behavior.
///
/// Returns the pass's actions (empty on the nothing-to-do fast path) so the boot
/// driver can emit `session-bound` tab-stamp events like the periodic poll does.
pub async fn run_at_boot(
    store: &SessionLifecycleStore,
    live: Vec<LivePty>,
) -> Vec<ReconcileAction> {
    if live.is_empty() && store.open_records().is_empty() {
        return Vec::new(); // nothing to do
    }
    let snapshot = crate::process_capture::process_tree::snapshot_process_table_public().await;
    run_reconcile_pass(store, &live, &snapshot, "boot").await
}

/// Wire payload for the `session-bound` Tauri event — the binder→frontend
/// channel that stamps a bound `claudeSessionId` onto the hosting tab.
///
/// Why this exists: the tab durability marker (`sessionDurability.ts` — the
/// "ephemeral" tag) classifies from the FRONTEND tab object, which only learns
/// a session id on the launch-menu / resume / mtime-capture paths. A session
/// bound by this module (a hand-typed or absolute-path launch) was durable in
/// the REGISTRY but invisible to the tab — so the tag dishonestly read
/// "ephemeral" for a session that restores fine. Emitting one event per Bind
/// lets the frontend stamp the tab within the same poll tick.
///
/// camelCase field names are the wire contract with
/// `src/components/terminal/sessionBoundEvent.ts` — keep them in sync.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionBoundPayload {
    pub terminal_id: String,
    pub session_id: String,
    /// Empty string when the bind didn't resolve a config dir (rung-2 cmdline
    /// bind whose transcript hasn't appeared yet) — the frontend treats empty
    /// as "unknown", never as a path.
    pub config_dir: String,
    /// The evidence grade (`authoritative` / `observed` / `reconciled`) — the
    /// store normalizes on write; this mirrors what was written.
    pub origin: String,
    pub confirmed: bool,
}

/// Event name for the binder→frontend tab-stamp channel.
pub const SESSION_BOUND_EVENT: &str = "session-bound";

/// Project the [`ReconcileAction::Bind`]s of a pass into `session-bound` wire
/// payloads. Pure — the caller (main.rs poll / boot driver) does the actual
/// Tauri emit, so this is unit-testable without an AppHandle.
pub fn session_bound_payloads(actions: &[ReconcileAction]) -> Vec<SessionBoundPayload> {
    actions
        .iter()
        .filter_map(|a| match a {
            ReconcileAction::Bind {
                terminal_id,
                session_id,
                config_dir,
                origin,
                confirmed,
            } => Some(SessionBoundPayload {
                terminal_id: terminal_id.clone(),
                session_id: session_id.clone(),
                config_dir: config_dir.clone(),
                origin: origin.as_origin_const().to_string(),
                confirmed: *confirmed,
            }),
            _ => None,
        })
        .collect()
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
    use tempfile::tempdir;

    /// `session_bound_payloads` projects exactly the Bind actions — camelCase
    /// wire shape, origin as its registry string, non-Bind actions dropped.
    #[test]
    fn session_bound_payloads_projects_binds_only() {
        let actions = vec![
            ReconcileAction::Bind {
                terminal_id: "term-1".to_string(),
                session_id: "sess-1".to_string(),
                config_dir: "C:/cfg".to_string(),
                origin: BindOrigin::Observed,
                confirmed: true,
            },
            ReconcileAction::LeaveAlone {
                terminal_id: "term-2".to_string(),
            },
            ReconcileAction::SkipAmbiguous {
                terminal_id: "term-3".to_string(),
            },
            ReconcileAction::PrunePhantom {
                session_id: "sess-x".to_string(),
            },
            ReconcileAction::Bind {
                terminal_id: "term-4".to_string(),
                session_id: "sess-4".to_string(),
                config_dir: String::new(),
                origin: BindOrigin::Authoritative,
                confirmed: false,
            },
        ];
        let payloads = session_bound_payloads(&actions);
        assert_eq!(payloads.len(), 2, "only Bind actions project");
        assert_eq!(payloads[0].terminal_id, "term-1");
        assert_eq!(payloads[0].origin, "observed");
        assert!(payloads[0].confirmed);
        assert_eq!(payloads[1].origin, "authoritative");
        assert!(!payloads[1].confirmed);
        // The wire contract with sessionBoundEvent.ts is camelCase.
        let wire = serde_json::to_value(&payloads[0]).unwrap();
        assert!(wire.get("terminalId").is_some(), "camelCase terminalId");
        assert!(wire.get("sessionId").is_some(), "camelCase sessionId");
        assert!(wire.get("configDir").is_some(), "camelCase configDir");
    }

    fn pty(id: &str, pid: Option<u32>, wd: &str) -> LivePty {
        LivePty {
            terminal_id: id.to_string(),
            pid,
            working_dir: wd.to_string(),
            page_id: "default".to_string(),
            title: "T".to_string(),
            ai_pid: None,
            ai_start_unix: None,
        }
    }

    /// A PTY whose claude anchor is already resolved (anchor pid + its start).
    fn pty_anchored(id: &str, ai_pid: u32, ai_start: i64, wd: &str) -> LivePty {
        LivePty {
            ai_pid: Some(ai_pid),
            ai_start_unix: Some(ai_start),
            ..pty(id, Some(1), wd)
        }
    }

    /// An empty claim set (the common test case).
    fn no_claims() -> HashSet<String> {
        HashSet::new()
    }

    /// No cmdline evidence (the common test case — rung 3 only).
    fn no_cmdlines() -> HashMap<u32, String> {
        HashMap::new()
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

    /// RUNG 2: a typed `--session-id` IS the identity — bind authoritative.
    /// Confirmed iff a transcript for that exact id is already on disk; else
    /// provisional (empty config, unconfirmed).
    #[test]
    fn decide_cmdline_binds_authoritative_confirmed_iff_transcript() {
        let p = pty("term-1", Some(100), "C:/repo");
        // Transcript present for the typed id → confirmed, config carried over.
        let candidates = vec![cand("typed-id", "C:/cfg", Some(1_500))];
        let action =
            decide_bind_for_live_pty(&p, None, 1_000, Some("typed-id"), &candidates, &no_claims());
        assert_eq!(
            action,
            ReconcileAction::Bind {
                terminal_id: "term-1".to_string(),
                session_id: "typed-id".to_string(),
                config_dir: "C:/cfg".to_string(),
                origin: BindOrigin::Authoritative,
                confirmed: true,
            }
        );
        // No transcript yet → provisional (unconfirmed, empty config).
        let action2 =
            decide_bind_for_live_pty(&p, None, 1_000, Some("typed-id"), &[], &no_claims());
        assert_eq!(
            action2,
            ReconcileAction::Bind {
                terminal_id: "term-1".to_string(),
                session_id: "typed-id".to_string(),
                config_dir: String::new(),
                origin: BindOrigin::Authoritative,
                confirmed: false,
            },
            "typed --session-id with no transcript yet binds provisional"
        );
    }

    /// RUNG 3: exactly ONE post-start, unclaimed candidate ⇒ observed bind; a
    /// pre-start foreign transcript is rejected by the start filter.
    #[test]
    fn decide_observed_binds_unique_post_start_and_rejects_pre_start() {
        let p = pty("term-1", Some(100), "C:/repo");
        let candidates = vec![
            // Foreign: started BEFORE the anchor — rejected.
            cand("foreign", "C:/cfg", Some(500)),
            // Ours: started just after the anchor — the unique survivor.
            cand("ours", "C:/cfg", Some(1_010)),
        ];
        let action = decide_bind_for_live_pty(&p, None, 1_000, None, &candidates, &no_claims());
        assert_eq!(
            action,
            ReconcileAction::Bind {
                terminal_id: "term-1".to_string(),
                session_id: "ours".to_string(),
                config_dir: "C:/cfg".to_string(),
                origin: BindOrigin::Observed,
                confirmed: true,
            }
        );
    }

    /// RUNG 3: MORE THAN ONE passing candidate ⇒ SkipAmbiguous (no guess).
    #[test]
    fn decide_observed_two_passing_is_ambiguous() {
        let p = pty("term-1", Some(100), "C:/repo");
        let candidates = vec![
            cand("a", "C:/cfg", Some(1_010)),
            cand("b", "C:/cfg", Some(2_000)),
        ];
        let action = decide_bind_for_live_pty(&p, None, 1_000, None, &candidates, &no_claims());
        assert_eq!(
            action,
            ReconcileAction::SkipAmbiguous {
                terminal_id: "term-1".to_string()
            },
            "two post-start candidates can't be uniquely resolved this tick"
        );
    }

    /// RUNG 3: a `claimed_ids` entry removes a sibling, so the 2nd candidate
    /// becomes the unique survivor (two same-second launches converge).
    #[test]
    fn decide_observed_claimed_ids_disambiguates() {
        let p = pty("term-1", Some(100), "C:/repo");
        let candidates = vec![
            cand("a", "C:/cfg", Some(1_010)),
            cand("b", "C:/cfg", Some(2_000)),
        ];
        let claimed: HashSet<String> = ["a".to_string()].into_iter().collect();
        let action = decide_bind_for_live_pty(&p, None, 1_000, None, &candidates, &claimed);
        assert_eq!(
            action,
            ReconcileAction::Bind {
                terminal_id: "term-1".to_string(),
                session_id: "b".to_string(),
                config_dir: "C:/cfg".to_string(),
                origin: BindOrigin::Observed,
                confirmed: true,
            },
            "claiming the sibling makes the other candidate unique"
        );
    }

    /// RUNG 3: no post-start candidate ⇒ LeaveAlone (don't bind a foreign one).
    #[test]
    fn decide_leaves_alone_when_only_pre_start_candidates() {
        let p = pty("term-1", Some(100), "C:/repo");
        let candidates = vec![cand("foreign", "C:/cfg", Some(500))];
        let action = decide_bind_for_live_pty(&p, None, 1_000, None, &candidates, &no_claims());
        assert_eq!(
            action,
            ReconcileAction::LeaveAlone {
                terminal_id: "term-1".to_string()
            }
        );
    }

    /// A confirmed record is owned by the deterministic path — never re-bound
    /// (even when a typed cmdline id would otherwise bind rung 2).
    #[test]
    fn decide_leaves_confirmed_record_alone() {
        let p = pty("term-1", Some(100), "C:/repo");
        let mut rec = bind_record(&p, "already", "C:/cfg", ORIGIN_OBSERVED, false);
        rec.confirmed_at = Some(123);
        let candidates = vec![cand("ours", "C:/cfg", Some(2_000))];
        let action = decide_bind_for_live_pty(
            &p,
            Some(&rec),
            1_000,
            Some("typed-id"),
            &candidates,
            &no_claims(),
        );
        assert_eq!(
            action,
            ReconcileAction::LeaveAlone {
                terminal_id: "term-1".to_string()
            },
            "a confirmed record is never re-bound"
        );
    }

    /// Unknown anchor start (0) disables the start filter, but the uniqueness
    /// gate still applies: one candidate binds, two are ambiguous.
    #[test]
    fn decide_anchor_unknown_binds_reconciled_not_observed() {
        let p = pty("term-1", None, "C:/repo");
        // Single candidate, anchor unknown → the start filter could not run, so
        // the only evidence is "unique in this cwd" — the mtime-guess class.
        // It binds, but graded `reconciled` so the frontend QUARANTINES it.
        // Grading it `observed` here would auto-resume a possibly-foreign
        // session on the weakest evidence in the ladder.
        let one = vec![cand("a", "C:/cfg", Some(500))];
        let action = decide_bind_for_live_pty(&p, None, 0, None, &one, &no_claims());
        assert_eq!(
            action,
            ReconcileAction::Bind {
                terminal_id: "term-1".to_string(),
                session_id: "a".to_string(),
                config_dir: "C:/cfg".to_string(),
                origin: BindOrigin::Reconciled,
                confirmed: true,
            }
        );
        // Two candidates, anchor unknown → still ambiguous (uniqueness holds).
        let two = vec![
            cand("a", "C:/cfg", Some(500)),
            cand("b", "C:/cfg", Some(900)),
        ];
        let action2 = decide_bind_for_live_pty(&p, None, 0, None, &two, &no_claims());
        assert_eq!(
            action2,
            ReconcileAction::SkipAmbiguous {
                terminal_id: "term-1".to_string()
            }
        );
    }

    /// Phantom detector: authoritative + unconfirmed + no live PTY + no
    /// transcript ⇒ phantom.
    #[test]
    fn is_phantom_true_only_for_authoritative_provisional_orphan() {
        let p = pty("dead-term", None, "C:/repo");
        let mut rec = bind_record(&p, "phantom", "C:/cfg", ORIGIN_OBSERVED, false);
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

    /// End-to-end against a real store: a live shim-bypassed PTY gets an
    /// `observed`+confirmed record (process-anchored unique bind); a phantom
    /// provisional row is pruned.
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
        // The phantom has NO transcript; the bound one does.
        let existing_ids: HashSet<String> = ["real-sess".to_string()].into_iter().collect();
        let index = FakeIndex {
            by_wd,
            existing_ids,
        };

        let actions = run_reconcile(&store, &live, &snapshot, &index, &no_cmdlines());

        // The live PTY was bound as observed (process-anchored unique).
        assert!(actions.iter().any(|a| matches!(
            a,
            ReconcileAction::Bind { session_id, origin, .. }
                if session_id == "real-sess" && *origin == BindOrigin::Observed
        )));
        let recovered = store.get("real-sess").expect("observed record written");
        assert_eq!(
            recovered.origin.as_deref(),
            Some(ORIGIN_OBSERVED),
            "a process-anchored unique bind is recorded as observed"
        );
        assert!(
            recovered.confirmed_at.is_some(),
            "an observed record with a real transcript is confirmed"
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

    /// A pass that finds the SAME transcript again is idempotent (the confirmed
    /// row is left alone the second time).
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

        run_reconcile(&store, &live, &snapshot, &index, &no_cmdlines());
        // The bound record now carries the LIVE terminal id, so the 2nd pass
        // finds it confirmed-by-terminal and leaves it alone.
        let second = run_reconcile(&store, &live, &snapshot, &index, &no_cmdlines());
        assert!(
            second.iter().any(|a| matches!(
                a,
                ReconcileAction::LeaveAlone { terminal_id } if terminal_id == "live-term"
            )),
            "second pass leaves the now-confirmed record alone"
        );
        // Still exactly one row for the session.
        assert!(store.get("real-sess").is_some());
    }

    /// RUNG 2 end-to-end: the cmdline map is keyed by the PTY's CLAUDE ANCHOR
    /// pid — a typed `--session-id` on that pid binds AUTHORITATIVE (not
    /// observed), even though a rung-3 transcript correlation was also possible.
    #[test]
    fn run_reconcile_cmdline_anchor_binds_authoritative() {
        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();

        // The PTY's claude anchor is pid 777, started at 1000.
        let live = vec![pty_anchored("live-term", 777, 1_000, "C:/repo")];
        let snapshot = ProcessSnapshot::default();

        let mut by_wd = HashMap::new();
        by_wd.insert(
            "C:/repo".to_string(),
            vec![cand("typed-id", "C:/cfg", Some(1_500))],
        );
        let index = FakeIndex {
            by_wd,
            existing_ids: ["typed-id".to_string()].into_iter().collect(),
        };
        let cmdlines: HashMap<u32, String> =
            [(777u32, "typed-id".to_string())].into_iter().collect();

        run_reconcile(&store, &live, &snapshot, &index, &cmdlines);

        let rec = store.get("typed-id").expect("cmdline bind written");
        assert_eq!(
            rec.origin.as_deref(),
            Some(ORIGIN_AUTHORITATIVE),
            "a typed --session-id IS the identity — authoritative, not observed"
        );
        assert!(rec.confirmed_at.is_some(), "transcript on disk ⇒ confirmed");
    }

    /// PHASE 4 seam, end-to-end: a CONFIRMED `observed` bind reaches
    /// `record_open` ALREADY confirmed, so the store's single-tenant-terminal
    /// supersede retires the identity seam's unconfirmed authoritative sibling on
    /// the SAME terminal while that terminal is still alive. (A post-hoc
    /// `confirm_session` would flip the flag only AFTER the supersede scan had
    /// declined to run — the orphan class this closes.)
    #[test]
    fn run_reconcile_observed_bind_supersedes_seam_sibling() {
        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();

        // The identity seam's spawn-time pin on the LIVE terminal: authoritative,
        // never confirmed (the absolute-path claude bypassed the shim + hook).
        store.record_open(TerminalSessionRecord {
            claude_session_id: "seam-pin".to_string(),
            config_dir: None,
            working_dir: Some("C:/repo".to_string()),
            page_id: "default".to_string(),
            zone_index: -1,
            title: Some("shell".to_string()),
            terminal_id: "live-term".to_string(),
            opened_at: 0,
            last_seen_at: 0,
            state: "open".to_string(),
            closed_at: None,
            close_reason: None,
            provider: DEFAULT_PROVIDER.to_string(),
            origin: Some(ORIGIN_AUTHORITATIVE.to_string()),
            restore_pending_at: None,
            confirmed_at: None,
        });

        let live = vec![pty("live-term", Some(4242), "C:/repo")];
        let mut snapshot = ProcessSnapshot::default();
        snapshot.creation_times.insert(4242, 1_000);
        let mut by_wd = HashMap::new();
        by_wd.insert(
            "C:/repo".to_string(),
            vec![cand("real-sess", "C:/cfg", Some(1_500))],
        );
        let index = FakeIndex {
            by_wd,
            // BOTH ids have "transcripts", so the phantom prune cannot be what
            // retires the seam row — only the supersede can.
            existing_ids: ["real-sess".to_string(), "seam-pin".to_string()]
                .into_iter()
                .collect(),
        };

        run_reconcile(&store, &live, &snapshot, &index, &no_cmdlines());

        let seam = store.get("seam-pin").expect("seam row still present");
        assert_eq!(
            seam.state, "closed",
            "the confirmed observed bind superseded the seam's unconfirmed sibling"
        );
        assert_eq!(seam.close_reason.as_deref(), Some("superseded"));
        assert_eq!(
            store.get("real-sess").unwrap().state,
            "open",
            "the observed bind owns the terminal"
        );
    }

    // ── Disk-only restore net (G3) — pure selection ─────────────────────────

    fn recent(
        id: &str,
        cfg: &str,
        wd: &str,
        last_ms: i64,
    ) -> crate::terminal::transcript::RecentTranscript {
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
            recent(
                "edge",
                "C:/cfg-B",
                "C:/repoB",
                now - DISK_ONLY_RESTORE_WINDOW_MS,
            ),
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
        assert!(
            a.confirmed_at.is_none(),
            "disk-only candidate is unconfirmed"
        );
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
