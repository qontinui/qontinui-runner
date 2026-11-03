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
//!    owned by it and never re-bound here — unless the pane's own `claude`
//!    contradicts it (see "checks the bindings" below): a pane whose every
//!    confirmed row reads `unknown` is unbound again.
//! 2. **Provider registry** (rung 2b, ranked FIRST among the rungs here): the
//!    pane's own top-level `claude` names a session in its Claude Code record
//!    `<config_dir>/sessions/<pid>.json` ([`crate::session::pane_binding`]) —
//!    the provider's statement about the process, where the argv and the spawn
//!    pin are the runner's launch-time prediction. That IS the identity: bound
//!    `authoritative` ([`BindOrigin::ProviderRegistry`]), confirmed iff a
//!    transcript for that exact id is already on disk, and it pre-empts a
//!    DIFFERING argv id. When the record names a session another LIVE pane
//!    holds or runs, the pane is left alone — never bound to something its own
//!    record contradicts. A row on a dead terminal does not hold its session
//!    against the pane's own record: the session moves to the pane that runs
//!    it. A pane whose record names a session it holds no confirmed row of is
//!    unbound, whatever else it holds (a missed hook — `bound_panes`). A
//!    record that cannot be read binds nothing, and the argv rung decides.
//!
//!    **Cmdline-extracted** (rung 2): the live claude process's own argv carries
//!    `--session-id <uuid>` → that IS the identity. Bound `authoritative`,
//!    confirmed iff a transcript for that exact id already exists (else
//!    provisional until it does). No guessing — and never a session a peer
//!    live pane holds or runs: the store never refuses an authoritative bind. The cmdline query is TARGETED at
//!    exactly the anchor pids of unbound terminals — never a table-wide fetch —
//!    and fails open (unavailable ⇒ fall through to rung 3).
//! 3. **Observed** (rung 3): the claude descendant's **process start** anchors a
//!    transcript correlation — a candidate qualifies only when its FIRST event is
//!    at/after that start (minus skew). A UNIQUENESS gate is the safety: bind only
//!    when EXACTLY ONE candidate passes; >1 ⇒ skip this tick. A claimed-id set
//!    removes every id open on ANOTHER live terminal — confirmed or not, unless
//!    that pane's own record contradicts it — plus every confirmed one, every id
//!    another pane's own record names, and every id this pass binds, so a
//!    session a peer pane runs is never correlated onto this one, and two
//!    same-second launches converge across ticks. (Before plan
//!    `2026-10-31-runner-binds-a-terminal-to-the-wrong-live-session` the set held
//!    confirmed rows only, and the Phase 1 reproduction shows rung 3 then moving
//!    a live peer's unconfirmed session onto another pane — the shape seen on
//!    merytshost on 2026-10-25, whose actual binder is UNKNOWN.) The transcript
//!    proves the session exists ⇒ `origin:"observed"` + confirmed ⇒ auto-resume
//!    eligible.
//!
//!    Rungs 2 and 3 both require a claude IMAGE in the pane's subtree. Accepted
//!    narrowing: a shim-bypassed claude whose image is not detected as claude
//!    (e.g. hosted under `node`) is no longer bound by this binder — such a
//!    pane is left alone, and the lifecycle poll already treats it as
//!    claude-absent. Likewise a freshly restored pane whose claude has not
//!    started yet is skipped by the boot pass and picked up by the periodic
//!    poll once its claude process appears.
//! 4. **Reconciled** (rung 4): the mtime guess AND the degraded rung-3 case
//!    where NO start anchor could be resolved (the process table yielded
//!    neither a claude nor a shell creation time). Without the anchor the
//!    post-start filter is a no-op and "unique in this cwd" is the only
//!    evidence left — which is exactly the guess this rung exists to grade
//!    down. Never auto-resumed: restore opens a plain terminal only. The
//!    grade follows the anchor, not the uniqueness gate.
//!
//! The anchor is the **claude descendant**, not the shell: anchoring on shell
//! start would admit a foreign same-cwd session started after the shell but
//! before claude. A pane with no claude image in its subtree is left alone —
//! never bound. The shell pid's start is a fallback anchor ONLY when a claude
//! image WAS found but its own start time is unknown.
//!
//! It also PRUNES the inverse: a **phantom** provisional record — an
//! authoritative-but-unconfirmed row written at spawn for a plain shell that
//! never ran a provider — with no live process AND no transcript. Pruning it
//! stops the restore classifier from ever auto-`--resume`-ing a session that
//! never existed.
//!
//! ## Every pass also CHECKS the bindings it does not make
//!
//! Every open row on a live pane is judged by the three rules of
//! [`crate::session::pane_binding::binding_verdicts`]: `matched` when its
//! pane's own `claude` record names its session; `unknown` — logged
//! (`session binder: binding mismatch`) and counted once per episode — when
//! ANOTHER live pane's record names it; otherwise no verdict. The
//! tracking-health census serves those stored verdicts, while they are the
//! current observation's, as `/restart-readiness` `processes[].bindingState`,
//! judging nothing itself. The same records decide which panes count as bound
//! (`bound_panes`), and the verdicts are stored again after the pass binds.
//! This check is
//! the only thing that sees a binding gone wrong on a confirmed pane — which
//! is why the pass runs on every tick, not only when a pane is unbound.
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
use crate::session::claude_activity::LivePid;
use crate::session::pane_binding::{self, PaneEvidence, PaneObservation};
use crate::session::session_lifecycle_store::{
    BindingState, RecordOpenOutcome, SessionLifecycleStore, TerminalSessionRecord,
    DEFAULT_PROVIDER, ORIGIN_AUTHORITATIVE, ORIGIN_OBSERVED, ORIGIN_RECONCILED, UNZONED_ZONE_INDEX,
};
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;

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
    /// is present in the subtree — [`run_reconcile`] then leaves the pane alone
    /// (a shell with no claude is never bound to a transcript). Callers may
    /// leave this `None`: [`run_reconcile_pass`] resolves it from that pass's
    /// process snapshot.
    pub ai_pid: Option<u32>,
    /// The anchor process's creation time (epoch seconds). `None` (or `<= 0`)
    /// with `ai_pid` resolved means the claude's own start is unknown — the
    /// correlation then degrades to the shell `pid`'s start, and failing that
    /// to the uniqueness gate alone.
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
    /// The pane's own top-level `claude` names this id in its Claude Code
    /// registry file (rung 2b, [`crate::session::pane_binding`]) — the
    /// provider's statement about the process, not the runner's launch-time
    /// prediction. Stored `authoritative`; and, unlike rung 2's argv id, a
    /// correction the tab may adopt ([`SessionBoundPayload::provider_reported`])
    /// and the coord row is re-keyed to
    /// ([`crate::session::coord_identity::RekeySource::ProviderRegistry`]).
    ProviderRegistry,
    /// A process-start-anchored, uniquely-correlated transcript bind (rung 3) —
    /// the transcript proves the session exists. A CONFIRMED observed row is
    /// auto-resume-eligible (unlike the conservative `reconciled` mtime guess).
    Observed,
    /// A uniquely-correlated transcript bind whose START ANCHOR WAS UNAVAILABLE
    /// (rung 4) — the process table gave us neither the claude nor the shell
    /// creation time, so the post-start filter could not run and the only
    /// evidence left is "exactly one transcript lives in this cwd". That is the
    /// mtime-guess class: it can name a foreign session, so it is graded
    /// `reconciled`, which the frontend's `classifyRestoreAction` restores
    /// `terminal-only` — a plain terminal, no resume typed for the guessed id.
    /// Auto-resume MUST follow the anchor, not the uniqueness gate alone.
    Reconciled,
}

impl BindOrigin {
    /// The store `origin` const this maps to.
    pub fn as_origin_const(self) -> &'static str {
        match self {
            BindOrigin::Authoritative | BindOrigin::ProviderRegistry => ORIGIN_AUTHORITATIVE,
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
    /// A `Bind` the store REFUSED: it would have moved the session's row off
    /// `held_by`, a live pane that still runs it
    /// ([`SessionLifecycleStore::record_open_checked`]). Nothing was written.
    BindRefused {
        terminal_id: String,
        session_id: String,
        held_by: String,
    },
    /// An open row on `terminal_id` NEWLY reads `bindingState: unknown`: its
    /// session's own `claude` runs in `runs_on`, another live pane, whose
    /// record names it ([`crate::session::pane_binding`], rule 2).
    BindingMismatch {
        terminal_id: String,
        session_id: String,
        runs_on: String,
    },
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
///   [`run_reconcile`] passes `existing` only for a pane it found unbound; a
///   pane whose confirmed rows its own `claude` contradicts is unbound there.
/// - **Rung 2b (provider registry), FIRST:** `registry_session_id = Some(id)` ⇒
///   the pane's own top-level `claude` names `id` in its Claude Code record —
///   `Bind` `ProviderRegistry`, `confirmed` iff a transcript for `id` already
///   exists. It pre-empts a differing argv id. When `id` is in `peer_claimed`
///   (another live pane holds it, or another live pane's own record names it
///   too) ⇒ `LeaveAlone`: nothing the pane's own record contradicts is ever
///   bound to it, so neither the argv nor a transcript guess is consulted.
/// - **Rung 2 (cmdline):** else `cmdline_session_id = Some(id)` ⇒ the anchor
///   process typed its own `--session-id`, so that IS the identity — `Bind`
///   `Authoritative`, `confirmed` iff a transcript for `id` already exists
///   (else provisional). No guessing. An authoritative bind is never refused
///   by the store, so an `id` in `peer_claimed` ⇒ `LeaveAlone`: the argv is a
///   launch-time prediction, and a peer pane that holds or runs `id` outranks
///   it.
/// - **Rung 3 (observed):** else correlate to `candidates` that are BOTH
///   post-start (`started + skew >= anchor_start_unix`) AND not already
///   `claimed_ids`. UNIQUENESS gate: exactly one passes ⇒ `Bind` `confirmed`;
///   >1 passes ⇒ `SkipAmbiguous` (retry next tick); 0 pass ⇒ `LeaveAlone`.
/// - **Rung 4 (degraded):** `anchor_start_unix <= 0` disables the start filter
///   (the uniqueness gate still applies), so the bind rests on "unique in this
///   cwd" alone. It is therefore graded `Reconciled` — restored as a plain
///   terminal (`terminal-only`), never auto-resumed. The grade tracks the
///   anchor; only an anchored bind earns
///   `Observed`.
///
/// `anchor_start_unix` is the claude anchor's creation time (epoch seconds).
/// `peer_claimed` are the session ids rungs 2b and 2 may not take: every id
/// open on ANOTHER live terminal and not read `unknown` there, every id
/// another live pane's own record names, and every one this pass already
/// bound ([`run_reconcile`]). A row on a DEAD terminal is not among them: the
/// pane's own record outweighs a row nothing runs, so a hookless
/// `claude --resume S` pane is bound to `S` wherever `S`'s row was left.
/// `claimed_ids` — what the correlation rung (3) may not take — are those plus
/// every confirmed open row's id not read `unknown`, wherever it lives: a
/// transcript guess never takes a session that may still be restored.
/// Removing them keeps a peer pane's session where it is and lets two
/// same-second launches converge across ticks (each terminal claims one,
/// disambiguating the other).
#[allow(clippy::too_many_arguments)]
pub fn decide_bind_for_live_pty(
    pty: &LivePty,
    existing: Option<&TerminalSessionRecord>,
    anchor_start_unix: i64,
    cmdline_session_id: Option<&str>,
    registry_session_id: Option<&str>,
    candidates: &[TranscriptCandidate],
    peer_claimed: &HashSet<String>,
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

    // RUNG 2b — the session the pane's own `claude` names about ITSELF in
    // `<config_dir>/sessions/<pid>.json`. Not a correlation: the provider is
    // saying which session the process runs, where the argv is what the runner
    // (or a typed line) asked it to run at launch. So it comes first, and a
    // differing argv id is not consulted. A named session this pane may not take
    // (another pane holds it, or runs it too) leaves the pane alone: binding
    // anything else here would contradict the pane's own record. The store's
    // binding guard still decides whether the row may move here.
    if let Some(id) = registry_session_id {
        if peer_claimed.contains(id) {
            return ReconcileAction::LeaveAlone {
                terminal_id: pty.terminal_id.clone(),
            };
        }
        let matching = candidates.iter().find(|c| c.session_id == id);
        return ReconcileAction::Bind {
            terminal_id: pty.terminal_id.clone(),
            session_id: id.to_string(),
            config_dir: matching.map(|c| c.config_dir.clone()).unwrap_or_default(),
            origin: BindOrigin::ProviderRegistry,
            confirmed: matching.is_some(),
        };
    }

    // RUNG 2 — the id the anchor's own argv NAMES (`--session-id`, `--resume`,
    // `-r` — see `process_tree::parse_session_id_from_cmdline`) IS the identity:
    // the live process is demonstrably running under it. No correlation guess:
    // bind it authoritative. It's confirmed only if a real transcript for that
    // exact id is already on disk; otherwise it's provisional (the transcript
    // will appear a tick or two later and a hook / the next tick confirms it).
    // The store never refuses an authoritative bind, so a session a peer pane
    // holds or runs is not proposed here at all.
    if let Some(id) = cmdline_session_id {
        if peer_claimed.contains(id) {
            return ReconcileAction::LeaveAlone {
                terminal_id: pty.terminal_id.clone(),
            };
        }
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
    // have — the same guess `reconciled` exists to grade down. Grading it
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
/// this closes).
///
/// `zone_index` is passed IN rather than defaulted here. This module binds an
/// IDENTITY; it has no view of the grid, so it knows nothing about layout — and
/// [`SessionLifecycleStore::record_open_checked`] takes `zone_index` unconditionally
/// (it is also the frontend zone-move backstop's write path, which must be able
/// to move a session to any zone INCLUDING [`UNZONED_ZONE_INDEX`], so the store
/// cannot tell an assertion from an omission). Writing the sentinel here on
/// every poll therefore ERASED the real zone: the backstop had already
/// corrected the record, saw no further change against its own ledger, and
/// never re-emitted — so `placement.zoneIndex` read `-1` for every session in
/// the store and the session-info dropdown could not report where a session
/// lives. The caller passes the zone the terminal's existing open record
/// already carries, so an unknown never overwrites a known value.
fn bind_record(
    pty: &LivePty,
    session_id: &str,
    config_dir: &str,
    origin: &str,
    confirmed: bool,
    zone_index: i32,
) -> TerminalSessionRecord {
    TerminalSessionRecord {
        claude_session_id: session_id.to_string(),
        config_dir: Some(config_dir.to_string()),
        working_dir: Some(pty.working_dir.clone()),
        page_id: pty.page_id.clone(),
        zone_index,
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
        awaiting_account_since: None,
        confirmed_at: confirmed.then(|| chrono::Utc::now().timestamp_millis()),
        handle: None,
        account_label: None,
        account_wrapper: None,
        session_name: None,
        name_source: None,
        tenant_id: None,
        task_run_id: None,
        bypass_permissions: None,
        restored_from_boot_at: None,
        restore_tier: None,
        finished_at: None,
        wind_down_outcome: None,
        wind_down_at: None,
        wind_down_arm: None,
        finish_reason: None,
        finish_synced: false,
        spawn_device_default: None,
        adopted_from: None,
        last_session_start_at: None,
        gate_id: None,
        gate_consuming_device_id: None,
        gate_bound_boot_ms: None,
        binding_state: None,
        placed_strongly: false,
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
/// cmdline ids + per-pane registry evidence, so the whole flow is
/// unit-testable; [`run_reconcile_pass`] wires the real sources.
///
/// `cmdline_ids` maps an anchor pid ([`LivePty::ai_pid`]) to the `--session-id`
/// UUID parsed from that process's argv — the rung-2 evidence. An empty map (the
/// cmdline query failed, or found nothing) simply degrades every terminal to the
/// next rung: fail-open by construction.
///
/// `evidence` maps a live terminal id to what its pane's own top-level `claude`
/// records ([`PaneEvidence`]) — the rung-2b evidence, what decides which panes
/// are bound ([`bound_panes`]), AND what every open row on a live pane is
/// judged against ([`crate::session::pane_binding::binding_verdicts`]). A pane
/// absent from the map has no evidence this pass: it is neither bound from
/// nor judged.
///
/// The judgement comes first and is stored ([`apply_verdicts`]): a row found
/// `unknown` is counted even when this pass goes on to heal it. It then
/// decides which panes are bound and which ids are held. After the binds every
/// row is judged again, on the pane it now sits on. The verdicts are tagged
/// with the store oracle's current epoch ([`SessionLifecycleStore::pane_epoch`]);
/// the runner's own pass tags them with the epoch of the observation it hands
/// the oracle ([`run_reconcile_at_epoch`]).
///
/// Returns the actions taken (for logging/tests). Side effects: each `Bind`
/// writes a record (CONFIRMED when the bind is confirmed) unless the store
/// refuses it (`BindRefused`); each `PrunePhantom` removes the row; each
/// `BindingMismatch` is a row whose `binding_state` just turned `unknown`.
/// `LeaveAlone` / `SkipAmbiguous` are no-ops.
pub fn run_reconcile<I: TranscriptIndex>(
    store: &SessionLifecycleStore,
    live: &[LivePty],
    snapshot: &ProcessSnapshot,
    index: &I,
    cmdline_ids: &HashMap<u32, String>,
    evidence: &HashMap<String, PaneEvidence>,
) -> Vec<ReconcileAction> {
    run_reconcile_at_epoch(
        store,
        live,
        snapshot,
        index,
        cmdline_ids,
        evidence,
        store.pane_epoch(),
    )
}

/// [`run_reconcile`], its verdicts tagged with `epoch` — the number the oracle
/// gave the observation `evidence` was read from.
pub fn run_reconcile_at_epoch<I: TranscriptIndex>(
    store: &SessionLifecycleStore,
    live: &[LivePty],
    snapshot: &ProcessSnapshot,
    index: &I,
    cmdline_ids: &HashMap<u32, String>,
    evidence: &HashMap<String, PaneEvidence>,
    epoch: Option<u64>,
) -> Vec<ReconcileAction> {
    let live_terminal_ids: HashSet<&str> = live.iter().map(|p| p.terminal_id.as_str()).collect();
    let mut actions = Vec::new();

    // 0) Judge every open row on a live pane — including the confirmed panes
    //    step 1 leaves alone, which is where a wrong binding lives (plan
    //    `2026-10-31-runner-binds-a-terminal-to-the-wrong-live-session`).
    apply_verdicts(store, evidence, epoch, &mut actions);
    // The observation's verdicts are servable now that its first judgement is
    // stored; until this line the previous observation's stayed served.
    if let Some(epoch) = epoch {
        store.serve_verdicts_from(epoch);
    }
    let open_now = store.open_records();
    let contradicted = |r: &TerminalSessionRecord| r.binding_state == Some(BindingState::Unknown);
    // Every index this pass needs, built ONCE (a fully bound fleet then costs
    // no per-pane scan of the rows): the panes already bound...
    let bound_panes = bound_panes(&open_now, evidence, &live_terminal_ids);
    // ...the session ids no pane may correlate onto itself — every confirmed
    // open row not read `unknown`, wherever it lives...
    let confirmed_ids: HashSet<String> = open_now
        .iter()
        .filter(|r| r.confirmed_at.is_some() && !contradicted(r))
        .map(|r| r.claude_session_id.clone())
        .collect();
    // ...the ids this pass binds, which no other pane may take (two
    // same-second launches then converge: each terminal claims one,
    // disambiguating the other)...
    let mut bound_this_pass: HashSet<String> = HashSet::new();
    // ...how many open rows each terminal holds (a re-bind that could retire a
    // sibling is always written)...
    let mut rows_on_terminal: HashMap<&str, usize> = HashMap::new();
    for r in &open_now {
        *rows_on_terminal.entry(r.terminal_id.as_str()).or_default() += 1;
    }
    // ...and, per session, the live panes that hold it or whose own record
    // names it. A pane holds a session by an open row of it, confirmed or not,
    // that is not read `unknown` — or that is, but a STRONG bind placed it
    // there (`placed_strongly`: a hook, a pinned spawn, a rung-2/2b bind), the
    // pane still runs a `claude`, and it is the session that most recently
    // started on that pane (`latest_started_on_terminal`). A pane may never
    // take a session a PEER holds or runs (plan
    // `2026-10-31-runner-binds-a-terminal-to-the-wrong-live-session`):
    // claiming only confirmed rows is what let rung 3, in the Phase 1
    // reproduction, move a live peer's unconfirmed session onto another pane.
    // So rung 2b heals a row a WEAK bind put on the wrong pane — the shape
    // rung 3 reproduces — and a strong row its pane has since moved past (a
    // `/clear`, an in-TUI `/resume`, an exited `claude`), but not the row a
    // hook placed last on a pane that still runs it, whatever a stale record
    // elsewhere says. A pane may always re-take a session whose row it holds
    // itself.
    let holds_against_its_contradiction = |r: &TerminalSessionRecord| {
        r.placed_strongly
            && evidence.get(&r.terminal_id) != Some(&PaneEvidence::NoClaude)
            && store.latest_started_on_terminal(&r.terminal_id).as_deref()
                == Some(r.claude_session_id.as_str())
    };
    let mut holders: HashMap<&str, HashSet<&str>> = HashMap::new();
    let mut held_here: HashMap<&str, HashSet<&str>> = HashMap::new();
    for r in &open_now {
        if live_terminal_ids.contains(r.terminal_id.as_str())
            && (!contradicted(r) || holds_against_its_contradiction(r))
        {
            holders
                .entry(r.claude_session_id.as_str())
                .or_default()
                .insert(r.terminal_id.as_str());
            held_here
                .entry(r.terminal_id.as_str())
                .or_default()
                .insert(r.claude_session_id.as_str());
        }
    }
    for (terminal, ev) in evidence {
        if let Some(id) = ev.named_session() {
            holders.entry(id).or_default().insert(terminal.as_str());
        }
    }

    // 1) Bind live PTYs that are not bound.
    for pty in live {
        // Steady state: a bound terminal is owned by the deterministic path —
        // short-circuit BEFORE the disk scan, so a fully-bound fleet does zero
        // transcript work per tick.
        if bound_panes.contains(&pty.terminal_id) {
            actions.push(ReconcileAction::LeaveAlone {
                terminal_id: pty.terminal_id.clone(),
            });
            continue;
        }

        // A pane with NO claude image in its subtree is never bound. Without
        // this, the shell's start time became the anchor and ANY same-cwd
        // transcript started after the shell qualified — a bare shell pane got
        // a confirmed `observed` row for an unrelated session (2026-09-30:
        // shell d8d6a412 bound to 01bcee5d), and the next restore typed
        // `claude --resume` into a fresh pane. A claude that is present but
        // not detected by image name is confirmed by its pinned id + the
        // SessionStart hook instead.
        if pty.ai_pid.is_none() {
            actions.push(ReconcileAction::LeaveAlone {
                terminal_id: pty.terminal_id.clone(),
            });
            continue;
        }

        let existing = store.find_open_by_terminal(&pty.terminal_id);
        // Anchor on the CLAUDE descendant's start when resolved. The shell
        // pid's start is a fallback ONLY for a present claude whose own start
        // is unknown — dropping it would downgrade a real session from
        // `Observed` (auto-resume) to `Reconciled`.
        let anchor_start = pty.ai_start_unix.filter(|s| *s > 0).unwrap_or_else(|| {
            pty.pid
                .and_then(|pid| snapshot.creation_times.get(&pid).copied())
                .unwrap_or(0)
        });
        let cmdline_sid: Option<&str> = pty
            .ai_pid
            .and_then(|pid| cmdline_ids.get(&pid))
            .map(|s| s.as_str());
        let registry_sid: Option<&str> = evidence
            .get(&pty.terminal_id)
            .and_then(PaneEvidence::named_session);
        // This pane's claims, built only for a pane that needs binding. The
        // registry and argv rungs': every session a PEER live pane holds or
        // runs — never the pane's own, which it may still confirm — and every
        // one this pass bound. The correlation rung's: those plus every
        // confirmed row's session, wherever it lives.
        let own = held_here.get(pty.terminal_id.as_str());
        let peer_claimed: HashSet<String> = holders
            .iter()
            .filter(|(id, panes)| {
                panes.iter().any(|t| *t != pty.terminal_id)
                    && !own.is_some_and(|own| own.contains(*id))
            })
            .map(|(id, _)| (*id).to_string())
            .chain(bound_this_pass.iter().cloned())
            .collect();
        let claimed_here: HashSet<String> = peer_claimed
            .iter()
            .chain(confirmed_ids.iter())
            .cloned()
            .collect();
        let candidates = index.candidates_for(&pty.working_dir);
        // A pane whose own record names a session is decided from that record
        // ([`bound_panes`]): a confirmed row of ANOTHER session it holds — a
        // missed hook, a reused terminal — does not keep rung 2b away.
        let decide_existing = if registry_sid.is_some() {
            None
        } else {
            existing.as_ref().filter(|r| !contradicted(r))
        };
        let action = decide_bind_for_live_pty(
            pty,
            decide_existing,
            anchor_start,
            cmdline_sid,
            registry_sid,
            &candidates,
            &peer_claimed,
            &claimed_here,
        );
        match &action {
            ReconcileAction::Bind {
                session_id,
                config_dir,
                origin,
                confirmed,
                ..
            } => {
                // Carry the zone the terminal's existing open record already
                // holds. `existing` is keyed on the TERMINAL, which is the
                // right key: the zone is a property of where the PTY sits in
                // the grid, not of the session id that happens to own it — so
                // this survives a bind that supersedes a differently-keyed
                // sibling row on the same terminal. No record yet ⇒ genuinely
                // unknown ⇒ the sentinel, which is honest rather than lossy.
                let zone_index = existing
                    .as_ref()
                    .map(|r| r.zone_index)
                    .unwrap_or(UNZONED_ZONE_INDEX);
                // A confirmed bind is written CONFIRMED (see `bind_record`) so
                // `record_open_checked`'s supersede retires the identity seam's
                // unconfirmed sibling on this terminal in the same write.
                let record = bind_record(
                    pty,
                    session_id,
                    config_dir,
                    origin.as_origin_const(),
                    *confirmed,
                    zone_index,
                );
                let row = store.get(session_id);
                // A re-bind that changes no binding: the row is already open
                // here, under this origin, at least this confirmed.
                let unchanged =
                    bind_is_unchanged(row.as_ref(), &pty.terminal_id, *origin, *confirmed);
                // ...and one that would write nothing at all is not written: a
                // hookless pane is re-bound every tick, about 1,900 durable
                // writes a day each. Its restore mirror is still offered the
                // row — it re-emits only a material change, and this is how
                // one it skipped for want of a coord session is retried.
                let siblings = rows_on_terminal
                    .get(pty.terminal_id.as_str())
                    .is_some_and(|n| *n > 1);
                let outcome = if unchanged
                    && row
                        .as_ref()
                        .is_some_and(|row| rebind_writes_nothing(row, &record, siblings))
                {
                    store.refresh_restore_mirror(session_id);
                    RecordOpenOutcome::Recorded
                } else {
                    store.record_open_checked(record)
                };
                match outcome {
                    RecordOpenOutcome::Recorded => {}
                    RecordOpenOutcome::MoveRefused { held_by } => {
                        // The store's binding guard: the session's row stays
                        // on the live pane that still runs it. Nothing was
                        // written, so no `session-bound` event and no coord
                        // re-key either.
                        actions.push(ReconcileAction::BindRefused {
                            terminal_id: pty.terminal_id.clone(),
                            session_id: session_id.clone(),
                            held_by,
                        });
                        continue;
                    }
                    RecordOpenOutcome::Unavailable => {
                        tracing::warn!(
                            terminal_id = %pty.terminal_id,
                            claude_session = %session_id,
                            "session binder: the registry is unavailable — bind not written"
                        );
                        actions.push(ReconcileAction::LeaveAlone {
                            terminal_id: pty.terminal_id.clone(),
                        });
                        continue;
                    }
                }
                // Claim the id — confirmed or not, it now lives on this live
                // pane — so no other terminal in this pass binds it too.
                bound_this_pass.insert(session_id.clone());
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
                    // A hookless pane whose recorded session has no transcript
                    // yet is re-bound, unchanged, every tick: say so once, at
                    // info, only when the bind changed the row.
                    BindOrigin::ProviderRegistry if unchanged => {
                        tracing::debug!(
                            terminal_id = %pty.terminal_id,
                            claude_session = %session_id,
                            confirmed = *confirmed,
                            "session binder: re-bound the session the pane's own claude records (unchanged)"
                        );
                    }
                    BindOrigin::ProviderRegistry => {
                        tracing::info!(
                            terminal_id = %pty.terminal_id,
                            claude_session = %session_id,
                            confirmed = *confirmed,
                            "session binder: bound the session the pane's own claude records (provider registry)"
                        );
                    }
                    BindOrigin::Reconciled => {
                        // Degraded rung 4: no start anchor was resolvable, so the
                        // bind rests on the uniqueness gate alone and is written
                        // `reconciled`, which restore opens as a plain terminal
                        // (terminal-only, never auto-resumed). Logged at info
                        // (not debug) because a
                        // recurring one means the process table is failing to
                        // yield creation times — the anchor, not the bind, is the
                        // thing to fix.
                        tracing::info!(
                            terminal_id = %pty.terminal_id,
                            claude_session = %session_id,
                            working_dir = %pty.working_dir,
                            "session binder: no start anchor available — bound the unique cwd transcript as RECONCILED (restores as a plain terminal, never auto-resumed)"
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
            ReconcileAction::LeaveAlone { .. }
            | ReconcileAction::PrunePhantom { .. }
            | ReconcileAction::BindRefused { .. }
            | ReconcileAction::BindingMismatch { .. } => {}
        }
        actions.push(action);
    }

    // 2) Prune phantom provisional records (no live PTY, no transcript).
    let live_terminals: HashSet<String> =
        live_terminal_ids.iter().map(|t| (*t).to_string()).collect();
    for rec in store.open_records() {
        let has_transcript =
            index.transcript_exists(&rec.claude_session_id, rec.working_dir.as_deref());
        if is_phantom_record(&rec, &live_terminals, has_transcript) {
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

    // 3) Judge again after the binds, so each row is judged on the pane it now
    //    sits on. A transition step 0 already counted is not counted again.
    apply_verdicts(store, evidence, epoch, &mut actions);

    actions
}

/// Judge every open row on a live pane by the three rules
/// ([`pane_binding::binding_verdicts`]), store the verdicts tagged with
/// `epoch`, and report each row that NEWLY reads `unknown` as a
/// `BindingMismatch`.
fn apply_verdicts(
    store: &SessionLifecycleStore,
    evidence: &HashMap<String, PaneEvidence>,
    epoch: Option<u64>,
    actions: &mut Vec<ReconcileAction>,
) {
    let verdicts = pane_binding::binding_verdicts(&store.open_records(), evidence);
    for mismatch in pane_binding::apply_binding_verdicts(store, &verdicts, epoch) {
        actions.push(ReconcileAction::BindingMismatch {
            terminal_id: mismatch.terminal_id,
            session_id: mismatch.claude_session_id,
            runs_on: mismatch.runs_on.unwrap_or_default(),
        });
    }
}

/// The live panes the deterministic path owns, which a pass leaves alone —
/// and [`run_reconcile_pass`] neither queries an argv for nor prescans. A pane
/// whose own record names a session is bound iff it holds a CONFIRMED open row
/// of that session, the row rule 1 makes `matched`. One whose record names a
/// session it holds no confirmed row of is NOT bound, whatever else it holds:
/// rung 2b binds the session the pane runs — a `/clear`, an in-TUI `/resume`
/// or a new `claude` whose hook never reached the runner. A pane whose record
/// names nothing is bound iff it holds a confirmed open row not read
/// `unknown`.
///
/// Records follow an in-process switch in Claude Code 2.1.316 and 2.1.317
/// ([`crate::session::pane_binding`], "The verdict"), so on those builds the
/// session a pane's record names is the one it runs. On a build whose records
/// do NOT follow switches, a pane that ran `/clear` keeps naming its
/// predecessor; once no live pane holds or runs that predecessor, this rule
/// leaves the pane unbound and rung 2b binds the predecessor back onto it —
/// authoritatively, so a confirmed bind's supersede closes the row of the
/// session the pane really runs, and the pane's coord session is re-keyed.
fn bound_panes(
    open: &[TerminalSessionRecord],
    evidence: &HashMap<String, PaneEvidence>,
    live_terminal_ids: &HashSet<&str>,
) -> HashSet<String> {
    open.iter()
        .filter(|r| {
            r.state == "open"
                && r.confirmed_at.is_some()
                && live_terminal_ids.contains(r.terminal_id.as_str())
        })
        .filter(|r| {
            match evidence
                .get(&r.terminal_id)
                .and_then(PaneEvidence::named_session)
            {
                Some(named) => named == r.claude_session_id,
                None => r.binding_state != Some(BindingState::Unknown),
            }
        })
        .map(|r| r.terminal_id.clone())
        .collect()
}

// ---------------------------------------------------------------------------
// Drivers — wire the real live-PTY set, process snapshot, cmdline query, and
// disk scan.
// ---------------------------------------------------------------------------

/// Real [`TranscriptIndex`] backed by [`crate::terminal::transcript`] — scans
/// every known Claude config dir for transcripts of a project path.
#[derive(Debug)]
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
        !self
            .transcript_config_dirs(session_id, working_dir)
            .is_empty()
    }
}

impl DiskTranscriptIndex {
    /// Every config dir whose transcript for `session_id` exists. Empty for a
    /// missing `working_dir` — the same bare answer `transcript_exists` gives,
    /// which the store's guarded entry points map to UNKNOWN.
    pub fn transcript_config_dirs(
        &self,
        session_id: &str,
        working_dir: Option<&str>,
    ) -> Vec<PathBuf> {
        let Some(working_dir) = working_dir else {
            return Vec::new();
        };
        self.config_dirs
            .iter()
            .filter(|dir| {
                crate::terminal::transcript::session_transcript_path(dir, working_dir, session_id)
                    .exists()
            })
            .cloned()
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Off-runtime prescan (2026-08-23 wedge, stage 3)
// ---------------------------------------------------------------------------

/// A one-shot SNAPSHOT of the transcript disk state, built ONCE on the blocking
/// pool and then answered purely from memory.
///
/// ## Why this exists
///
/// [`DiskTranscriptIndex`] is lazy: `candidates_for` walks a project directory
/// the moment the correlation asks for it. `run_reconcile` is a synchronous
/// function called INLINE from `run_reconcile_pass`, which is `async` — so
/// every one of those walks ran on a tokio worker, and each walk parked that
/// worker on the process-global `SESSION_CACHE` mutex for its whole duration
/// (see `terminal::transcript`). This box has 8 logical CPUs and therefore 8
/// workers; the 45s reconcile tick plus the 30s transcript polls only had to
/// line up a handful of times before every worker was parked and axum stopped
/// answering while the kernel still accepted connections.
///
/// The walk is unavoidable. Doing it on a worker is not: parking a *blocking*
/// thread degrades throughput, parking a *worker* stops the runtime.
///
/// ## Answering from a snapshot
///
/// The inputs are all knowable before the pass runs — the unbound live PTYs'
/// working dirs, and the `(session_id, working_dir)` pairs the phantom prune
/// will probe — so one prescan covers the whole pass.
///
/// **Missing evidence is UNKNOWN, never "no".** A `transcript_exists` probe
/// for a pair the prescan did not cover answers `true`, because the only
/// consumer is the phantom-record PRUNE: answering `false` on absent evidence
/// would delete a real session's record. Absent candidates answer "empty",
/// which binds nothing — the same fail-open the lazy index had.
#[derive(Debug, Default)]
pub struct PrescannedTranscriptIndex {
    candidates: HashMap<String, Vec<TranscriptCandidate>>,
    /// Pairs the prescan actually probed.
    probed: HashSet<(String, String)>,
    /// The probed pairs that DO have a transcript on disk.
    exists: HashSet<(String, String)>,
}

impl PrescannedTranscriptIndex {
    /// Build the snapshot on the blocking pool.
    ///
    /// `working_dirs` — the project dirs whose candidates the pass may ask for
    /// (i.e. the UNBOUND live PTYs; a confirmed terminal must still cost zero
    /// disk work). `probes` — the `(session_id, working_dir)` pairs the phantom
    /// prune will ask about.
    ///
    /// Returns an empty (fail-open) snapshot when there is nothing to scan or
    /// when the blocking task could not be joined.
    pub async fn scan_off_runtime(
        working_dirs: Vec<String>,
        probes: Vec<(String, Option<String>)>,
    ) -> Self {
        if working_dirs.is_empty() && probes.is_empty() {
            // Steady state: nothing unbound, nothing open. No disk work at all.
            return Self::default();
        }
        Self::spawn_scan(move || Self::scan_blocking(working_dirs, probes)).await
    }

    /// Upper bound on one prescan.
    ///
    /// Generous: a cold walk of a large workspace legitimately takes seconds.
    /// It exists to stop an *indefinite* wait, not to police latency.
    const PRESCAN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

    /// Run `scan` on the BLOCKING pool and await it, **under a bound measured
    /// off the tokio runtime**.
    ///
    /// The one line that matters for stage 3, factored out so a test can hand
    /// it a scan that never returns and assert that tokio workers keep
    /// servicing other tasks anyway. Calling `scan()` inline here is exactly
    /// the regression.
    ///
    /// Iteration 14 left this await **unbounded**, on the reasoning that a
    /// `spawn_blocking` always eventually resolves. It does not when the
    /// blocking pool is exhausted — the stage-1 condition — and the obvious
    /// repair (`tokio::time::timeout`) is inert here for the same reason it was
    /// inert on the transcript scan: the timer wheel stops turning when the
    /// runtime is the thing that is sick. The bound is therefore
    /// [`crate::off_runtime::deadline`], measured by the OS scheduler.
    ///
    /// Degrading is safe by contract: an empty snapshot binds nothing and
    /// prunes nothing, because a pair the prescan never probed answers
    /// `transcript_exists = true` (missing evidence is UNKNOWN, never "no").
    async fn spawn_scan<F>(scan: F) -> Self
    where
        F: FnOnce() -> Self + Send + 'static,
    {
        Self::spawn_scan_bounded(scan, Self::PRESCAN_TIMEOUT).await
    }

    /// [`Self::spawn_scan`] with the bound injected, so a test drives the REAL
    /// code path without waiting out the production 30s.
    async fn spawn_scan_bounded<F>(scan: F, timeout: std::time::Duration) -> Self
    where
        F: FnOnce() -> Self + Send + 'static,
    {
        let bound = crate::off_runtime::deadline(timeout);
        let task = spawn_blocking_tracked(scan);
        tokio::select! {
            joined = task => match joined {
                Ok(idx) => idx,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "session reconcile: transcript prescan task failed — this pass binds and prunes nothing"
                    );
                    Self::default()
                }
            },
            _ = bound => {
                tracing::warn!(
                    timeout_secs = timeout.as_secs(),
                    "session reconcile: the transcript prescan did not finish within its bound \
                     (blocking pool exhausted?) — this pass binds and prunes nothing. The bound \
                     is measured on a dedicated OS thread, so it fires even when every tokio \
                     worker is parked."
                );
                Self::default()
            }
        }
    }

    /// The blocking half. Never call this from async code.
    fn scan_blocking(working_dirs: Vec<String>, probes: Vec<(String, Option<String>)>) -> Self {
        let disk = DiskTranscriptIndex::discover();
        let mut candidates: HashMap<String, Vec<TranscriptCandidate>> = HashMap::new();
        for wd in working_dirs {
            if candidates.contains_key(&wd) {
                continue;
            }
            let found = disk.candidates_for(&wd);
            candidates.insert(wd, found);
        }

        let mut probed: HashSet<(String, String)> = HashSet::new();
        let mut exists: HashSet<(String, String)> = HashSet::new();
        for (session_id, working_dir) in probes {
            // A record with no working dir is answered `false` by contract —
            // no probe needed, and recording it would be misleading.
            let Some(working_dir) = working_dir else {
                continue;
            };
            let key = (session_id.clone(), working_dir.clone());
            if !probed.insert(key.clone()) {
                continue;
            }
            if TranscriptIndex::transcript_exists(&disk, &session_id, Some(&working_dir)) {
                exists.insert(key);
            }
        }

        Self {
            candidates,
            probed,
            exists,
        }
    }
}

impl TranscriptIndex for PrescannedTranscriptIndex {
    fn candidates_for(&self, working_dir: &str) -> Vec<TranscriptCandidate> {
        self.candidates
            .get(working_dir)
            .cloned()
            .unwrap_or_default()
    }

    fn transcript_exists(&self, session_id: &str, working_dir: Option<&str>) -> bool {
        let Some(working_dir) = working_dir else {
            return false;
        };
        let key = (session_id.to_string(), working_dir.to_string());
        if self.probed.contains(&key) {
            self.exists.contains(&key)
        } else {
            // UNKNOWN. The only consumer is the phantom PRUNE, so the safe
            // answer is "assume it exists" — never delete on absent evidence.
            true
        }
    }
}

/// The same on-disk existence stat, exposed to the snapshot history's write-time
/// restorability stamp. One implementation, one answer: the field a recovery
/// line carries means exactly what the phantom PRUNE above means.
impl crate::session::snapshot_history::TranscriptProbe for DiskTranscriptIndex {
    fn transcript_exists(&self, session_id: &str, working_dir: Option<&str>) -> bool {
        TranscriptIndex::transcript_exists(self, session_id, working_dir)
    }

    fn transcript_config_dirs(
        &self,
        session_id: &str,
        working_dir: Option<&str>,
    ) -> Option<Vec<PathBuf>> {
        Some(DiskTranscriptIndex::transcript_config_dirs(
            self,
            session_id,
            working_dir,
        ))
    }
}

// ---------------------------------------------------------------------------
// Provider session index — the in-provider session NAME (`/rename`) + its id,
// keyed by the live claude process's pid.
// ---------------------------------------------------------------------------

/// What the provider itself says about one live session: the id it is actually
/// running under, and the name the operator sees in the TUI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderSessionMeta {
    /// The provider's own session id for this process — the cross-check that
    /// stops a stale/pid-reused file from retitling the wrong session.
    pub session_id: String,
    /// The in-provider session name (`/rename`), when set.
    pub name: Option<String>,
}

/// Lookup of [`ProviderSessionMeta`] by the live claude process's pid. Injected
/// so the title correlation is unit-testable without disk.
pub trait ProviderSessionIndex {
    fn meta_for_pid(&self, pid: u32) -> Option<ProviderSessionMeta>;
}

/// Real [`ProviderSessionIndex`], backed by the provider's OWN live-session
/// index at `<config_dir>/sessions/<pid>.json`.
///
/// ## Why this file and not the transcript
///
/// An in-Claude `/rename` sets the session's name but emits NO OSC-0 title, so
/// the runner's existing rename sink
/// ([`SessionLifecycleStore::update_title_by_terminal`]) — which is driven by
/// terminal title escapes — is never called for it, and the registry keeps the
/// spawn-time title forever. The obvious alternative source, a `summary` record
/// in the transcript JSONL, **does not exist**: a sweep of every transcript
/// under every config root found zero `{"type":"summary"}` records, and
/// `TranscriptSession::display_name` is *derived* from the first user message,
/// not from the rename. It would restate the spawn-time guess, not the operator's
/// intent.
///
/// The provider instead maintains this small JSON file per live session —
/// `{pid, sessionId, cwd, startedAt, name, status, …}` — whose `name` IS the
/// in-TUI name and whose `pid` is the claude image the binder already anchors on
/// ([`claude_anchor_in_subtree`](crate::process_capture::process_tree::claude_anchor_in_subtree)).
/// It is read-only to us and best-effort: an absent/unparseable file, or a
/// provider version that stops writing it, degrades to "no refresh this tick"
/// and the title simply stays as it is.
#[derive(Debug)]
pub struct DiskProviderSessionIndex {
    config_dirs: Vec<PathBuf>,
}

impl DiskProviderSessionIndex {
    pub fn discover() -> Self {
        Self {
            config_dirs: crate::terminal::transcript::find_claude_config_dirs(),
        }
    }
}

/// Wire shape of `<config_dir>/sessions/<pid>.json`. Every field is optional to
/// us: this is a foreign file we neither own nor version, so an added/removed
/// key must never fail the parse.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProviderSessionFile {
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

impl ProviderSessionIndex for DiskProviderSessionIndex {
    fn meta_for_pid(&self, pid: u32) -> Option<ProviderSessionMeta> {
        for dir in &self.config_dirs {
            let path = dir.join("sessions").join(format!("{pid}.json"));
            let Ok(raw) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(parsed) = serde_json::from_str::<ProviderSessionFile>(&raw) else {
                continue;
            };
            let session_id = parsed.session_id.filter(|s| !s.trim().is_empty())?;
            return Some(ProviderSessionMeta {
                session_id,
                name: parsed.name.filter(|n| !n.trim().is_empty()),
            });
        }
        None
    }
}

/// Pure: should this record's title be refreshed to the provider's session name,
/// and to what? `None` = leave it alone.
///
/// Guards, each closing a way this could rename the WRONG session:
/// - **id match** — the file is keyed by pid, and pids are reused. A file whose
///   `sessionId` is not this record's is either stale or describes a different
///   session; adopting its name would mislabel a live tab.
/// - **non-empty name** — an absent/blank name is "the operator never renamed
///   it", which must not erase the spawn-time title.
/// - **already current** — keeps the caller's write a no-op (the sink also
///   guards, but not re-entering it keeps the reconcile tick allocation-free in
///   the steady state).
pub fn decide_title_refresh(
    rec: &TerminalSessionRecord,
    meta: Option<&ProviderSessionMeta>,
) -> Option<String> {
    let meta = meta?;
    if meta.session_id != rec.claude_session_id {
        return None;
    }
    let name = meta.name.as_deref()?.trim();
    if name.is_empty() || rec.title.as_deref() == Some(name) {
        return None;
    }
    Some(name.to_string())
}

/// One title the pass refreshed — returned for logging/tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TitleRefresh {
    pub terminal_id: String,
    pub title: String,
}

/// Refresh each live PTY's registry title from the provider's own session name,
/// routing through the EXISTING rename sink
/// ([`SessionLifecycleStore::update_title_by_terminal`]) rather than adding a
/// parallel title writer — so an in-Claude `/rename` converges on the registry
/// within one reconcile tick, exactly like an OSC-0 rename does immediately.
///
/// Only terminals with a bound open record are considered: the title belongs to
/// a record, and an unbound PTY has none to update.
///
/// Precedence note: the other writer into the same sink is `terminal_set_title`
/// (xterm.js's OSC-0 `onTitleChange`). These do not fight in practice — the very
/// symptom this fixes is that a provider session emits no OSC-0 title, so the
/// registry title sits frozen at its spawn-time value. Were a PTY to emit OSC-0
/// titles continuously, the two would alternate at one write per tick; the
/// `info!` below makes that visible rather than silent.
pub fn refresh_titles<N: ProviderSessionIndex>(
    store: &SessionLifecycleStore,
    live: &[LivePty],
    index: &N,
) -> Vec<TitleRefresh> {
    let mut out = Vec::new();
    for pty in live {
        let Some(ai_pid) = pty.ai_pid else { continue };
        let Some(rec) = store.find_open_by_terminal(&pty.terminal_id) else {
            continue;
        };
        let meta = index.meta_for_pid(ai_pid);
        let Some(title) = decide_title_refresh(&rec, meta.as_ref()) else {
            continue;
        };
        store.update_title_by_terminal(&pty.terminal_id, &title);
        tracing::info!(
            terminal_id = %pty.terminal_id,
            claude_session = %rec.claude_session_id,
            title = %title,
            "session reconcile: adopted the provider's in-session name as the registry title"
        );
        out.push(TitleRefresh {
            terminal_id: pty.terminal_id.clone(),
            title,
        });
    }
    out
}

/// Title-refresh driver: resolve each live PTY's claude anchor from an
/// ALREADY-TAKEN process snapshot, then converge registry titles on the
/// provider's own session names. Returns the refreshes made (for logging/tests).
///
/// ## Why this is its OWN pass, not part of [`run_reconcile_pass`]
///
/// The bind pass is guarded by a steady-state check ("some live PTY lacks a
/// CONFIRMED record") so a fully-bound fleet does zero disk work per tick. A
/// rename, however, happens on a session that is bound, confirmed, and healthy —
/// i.e. EXACTLY when that guard is false. Folding the title refresh into the
/// bind pass would therefore mean it never runs for the case it exists to serve.
///
/// The cost it adds to the poll tick is one small read per live claude anchor
/// (the provider file is tiny and in the OS cache), not a transcript scan.
/// Fail-open: a missing/unreadable provider index refreshes nothing.
pub fn refresh_titles_pass(
    store: &SessionLifecycleStore,
    live: &[LivePty],
    snapshot: &ProcessSnapshot,
) -> Vec<TitleRefresh> {
    let live = anchor_live_ptys(live, snapshot);
    refresh_titles(store, &live, &DiskProviderSessionIndex::discover())
}

/// Parse an ISO-8601 timestamp into Unix epoch seconds (best-effort).
fn parse_iso_to_unix_secs(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp())
}

/// Resolve each live PTY's CLAUDE anchor — the claude-image descendant pid + its
/// process start — from an ALREADY-TAKEN snapshot (no extra sweep). The
/// correlation must anchor on when CLAUDE started, not on when its shell did (a
/// foreign same-cwd session started after the shell but before claude would
/// otherwise qualify); the shell pid's start is only a fallback for a found
/// claude whose own start is unknown. A PTY whose subtree hosts no claude image
/// keeps `ai_pid: None`, and [`run_reconcile`] never binds it.
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
/// Steady-state cheap — it now runs on EVERY liveness tick, because its binding
/// check is the only thing that sees a confirmed pane bound to the wrong
/// session: a bound terminal is skipped before any disk work, the cmdline query
/// is skipped entirely when no unbound terminal has a claude anchor (an empty
/// pid list spawns no subprocess), the transcript prescan runs only when some
/// pane is unbound, and the one read every tick makes is each live pane's
/// top-level `claude`'s small `<config_dir>/sessions/<pid>.json`, on the
/// blocking pool under [`pane_binding::RECORD_READ_BOUND`]. That read is made
/// FIRST — which panes are bound, and so what the argv query and the prescan
/// cover, depends on it — and its result is handed to the store's oracle
/// ([`SessionLifecycleStore::observe_panes`]) just before the binds and
/// verdicts. The oracle numbers it: the verdicts carry that number, and the
/// binding guard and the tracking-health census answer from it rather than
/// reading again.
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

    // The panes' own evidence — rung 2b's, what decides which panes are bound,
    // and what every row is judged against: each live pane's top-level
    // `claude`, its Claude Code record read ONCE for the whole pass. A failed
    // snapshot saw no process at all — which is not evidence that a pane runs
    // nothing — so it publishes nothing (the oracle keeps the last good view,
    // until it ages out) and no pane has evidence this pass: none is bound
    // from it, none is judged.
    let snapshot_ok = !snapshot.parent_map.is_empty();
    let panes: HashMap<String, Vec<LivePid>> = if snapshot_ok {
        live.iter()
            .filter_map(|pty| {
                let root = pty.pid?;
                Some((
                    pty.terminal_id.clone(),
                    pane_binding::top_level_claude(root, snapshot),
                ))
            })
            .collect()
    } else {
        HashMap::new()
    };
    // The observation is aged from here, the start of the read: the argv
    // query and the prescan below must not make it look younger.
    let read_at = std::time::Instant::now();
    let readings = pane_binding::read_records_off_runtime(
        panes.values().flatten().copied().collect(),
        pane_binding::RECORD_READ_BOUND,
    )
    .await;
    let observation = PaneObservation {
        panes,
        sessions: pane_binding::sessions_from_readings(&readings),
        read_at: Some(read_at),
    };
    let evidence = observation.evidence_map();

    // Rung 2 — ONE targeted cmdline query, for exactly the anchor pids of the
    // terminals that still need binding. Never a table-wide fetch; an empty list
    // means no query at all. Fail-open: an unavailable cmdline just leaves the
    // map empty and every terminal falls through to the rung-3 transcript anchor.
    //
    // The unbound set is computed once, by the rule `run_reconcile` applies
    // ([`bound_panes`]: the stored rows against this pass's evidence), and
    // reused: it drives BOTH the cmdline query below and the transcript
    // prescan further down, so a fully-bound fleet still does zero of either.
    // A pane whose row this pass newly reads `unknown` while its record names
    // nothing is unbound inside `run_reconcile` only — and prescanned from the
    // next pass on, once its verdict is stored.
    let live_terminal_ids: HashSet<&str> = live.iter().map(|p| p.terminal_id.as_str()).collect();
    let bound = bound_panes(&store.open_records(), &evidence, &live_terminal_ids);
    let unbound: Vec<&LivePty> = live
        .iter()
        .filter(|pty| !bound.contains(&pty.terminal_id))
        .collect();
    let anchor_pids: Vec<u32> = unbound.iter().filter_map(|pty| pty.ai_pid).collect();
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

    // Stage-3 of the 2026-08-23 wedge fix: the transcript directory walk used
    // to happen INSIDE `run_reconcile` — a synchronous call from this async
    // function, i.e. on a tokio worker, holding the process-global
    // `SESSION_CACHE` mutex for the length of the walk. Prescan it on the
    // blocking pool instead and hand `run_reconcile` a pure in-memory index,
    // so this pass can no longer park a worker no matter how slow the disk is.
    //
    // The phantom-prune probes ride along only when some pane is unbound —
    // the condition the pass used to be gated on as a whole. A fully-bound
    // fleet therefore still does no transcript disk work per tick: the empty
    // prescan answers every probe UNKNOWN, which prunes nothing.
    let scan_dirs: Vec<String> = unbound.iter().map(|pty| pty.working_dir.clone()).collect();
    let probes: Vec<(String, Option<String>)> = if unbound.is_empty() {
        Vec::new()
    } else {
        store
            .open_records()
            .into_iter()
            .map(|r| (r.claude_session_id, r.working_dir))
            .collect()
    };
    let index = PrescannedTranscriptIndex::scan_off_runtime(scan_dirs, probes).await;

    // Every live pane, an empty list for one with no `claude` — and an empty
    // map when no pane is live: a pane absent here is not live. Handed to the
    // oracle only now, just before it is judged by: the oracle numbers it,
    // this pass's verdicts carry that number, and a verdict from the previous
    // observation stays current until this pass replaces it — not through the
    // argv query and the prescan.
    let epoch = if snapshot_ok {
        store.observe_panes(observation)
    } else {
        None
    };

    let before: HashMap<String, TerminalSessionRecord> = store
        .open_records()
        .into_iter()
        .map(|r| (r.claude_session_id.clone(), r))
        .collect();
    let actions = run_reconcile_at_epoch(
        store,
        &live,
        snapshot,
        &index,
        &cmdline_ids,
        &evidence,
        epoch,
    );

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
    let refused = actions
        .iter()
        .filter(|a| matches!(a, ReconcileAction::BindRefused { .. }))
        .count();
    let mismatched = actions
        .iter()
        .filter(|a| matches!(a, ReconcileAction::BindingMismatch { .. }))
        .count();
    // Binds that changed a row. A refusal, or a re-bind that wrote nothing new,
    // recurs every tick for as long as its cause lasts (a hookless pane, the
    // same session in two panes): those log at debug, so INFO says what moved.
    let changed = actions
        .iter()
        .filter(|a| match a {
            ReconcileAction::Bind {
                terminal_id,
                session_id,
                origin,
                confirmed,
                ..
            } => !bind_is_unchanged(before.get(session_id), terminal_id, *origin, *confirmed),
            _ => false,
        })
        .count();
    if changed > 0 || pruned > 0 || ambiguous > 0 || mismatched > 0 {
        tracing::info!(
            bound,
            changed,
            observed,
            ambiguous,
            pruned,
            refused,
            mismatched,
            live = live.len(),
            context,
            "session reconcile: pass complete"
        );
    } else if bound > 0 || refused > 0 {
        tracing::debug!(
            bound,
            refused,
            live = live.len(),
            context,
            "session reconcile: pass complete (nothing changed)"
        );
    }
    actions
}

/// Whether a bind of `session_id` onto `terminal_id` writes nothing new over
/// `existing`, the session's row before it: already open on that terminal,
/// under the same origin, and confirmed if the bind is.
fn bind_is_unchanged(
    existing: Option<&TerminalSessionRecord>,
    terminal_id: &str,
    origin: BindOrigin,
    confirmed: bool,
) -> bool {
    existing.is_some_and(|r| {
        r.state == "open"
            && r.terminal_id == terminal_id
            && r.origin.as_deref() == Some(origin.as_origin_const())
            && (r.confirmed_at.is_some() || !confirmed)
    })
}

/// Whether writing `bind` over `row` — the session's row, already open on the
/// bind's terminal under its origin ([`bind_is_unchanged`]) — would change
/// nothing a reader sees: every field the bind refreshes already holds the
/// bind's value, and the write could retire no sibling (`siblings`: another
/// row is open on the terminal), which only an authoritative or confirmed
/// bind does. `last_seen_at` is not among them: the liveness poll keeps it.
fn rebind_writes_nothing(
    row: &TerminalSessionRecord,
    bind: &TerminalSessionRecord,
    siblings: bool,
) -> bool {
    let may_retire_a_sibling = bind.confirmed_at.is_some()
        || bind.origin.as_deref() == Some(BindOrigin::Authoritative.as_origin_const());
    row.working_dir == bind.working_dir
        && row.page_id == bind.page_id
        && row.zone_index == bind.zone_index
        && row.title == bind.title
        && row.provider == bind.provider
        && bind
            .config_dir
            .as_deref()
            .is_none_or(|dir| dir.trim().is_empty() || row.config_dir.as_deref() == Some(dir))
        && !(siblings && may_retire_a_sibling)
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
    // One-time P4 registry-corruption repair (plan
    // `2026-07-19-runner-session-restore-mass-strand-and-git-popup`, Phase 3):
    // collapse any `open` rows already sharing a `terminal_id` down to the single
    // live tenant, so a terminal-reuse pileup (sequential `claude` runs on one
    // long-lived PTY whose prior exit-closes never fired) is cleaned in the
    // durable store. This is DURABLE HYGIENE, not the race-critical guard: this
    // pass is spawned with a delay and can run AFTER the frontend's on-mount
    // restore read, so the read itself is made collision-immune independently by
    // the one-live-session-per-terminal dedupe inside
    // `SessionLifecycleStore::restorable_records` (both rank the same contenders
    // — open rows bound to a terminal — by `open_authority_key`, so they agree on
    // which row to keep; a row held for an account choice is unbound, so it is a
    // contender in neither). Idempotent — a healthy registry closes
    // nothing. Runs before the nothing-to-do fast path so a registry that holds
    // ONLY stale collided rows (no live PTYs) is still repaired.
    let repaired = store.repair_terminal_id_collisions();
    if repaired > 0 {
        tracing::info!(
            closed = repaired,
            "session reconcile: boot repair collapsed terminal_id collisions to the live tenant (P4 registry corruption)"
        );
    }
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
    /// Did the PROVIDER report this id about itself, or did the runner infer it?
    ///
    /// `origin` alone cannot answer that, and the difference decides whether a
    /// bind may CORRECT an id a tab already holds. A rung-2 reconcile bind is
    /// graded [`BindOrigin::Authoritative`] because the id was lifted from the
    /// anchor process's typed `--session-id` — but that is precisely the id the
    /// runner PREDICTED and passed in, so it is worth nothing as a correction:
    /// letting it overwrite would clobber a true id with the guess.
    ///
    /// `POST /control/session-open` — the provider's own SessionStart hook,
    /// reporting the id it actually adopted — sets this `true`, and so does a
    /// rung-2b bind ([`BindOrigin::ProviderRegistry`]): the id is the one the
    /// pane's own `claude` records about itself in its Claude Code registry
    /// file. Every other bind this module produces is an inference and leaves
    /// it `false`.
    pub provider_reported: bool,
}

/// Event name for the binder→frontend tab-stamp channel.
pub const SESSION_BOUND_EVENT: &str = "session-bound";

/// Project the [`ReconcileAction::Bind`]s of a pass into `session-bound` wire
/// payloads. Pure — the caller (main.rs poll / boot driver) does the actual
/// Tauri emit, so this is unit-testable without an AppHandle.
pub fn session_bound_payloads(actions: &[ReconcileAction]) -> Vec<SessionBoundPayload> {
    // A bind the same pass found its pane's own `claude` contradicting is not
    // stamped on the tab: the pane does not run it.
    let contradicted: HashSet<(&str, &str)> = actions
        .iter()
        .filter_map(|a| match a {
            ReconcileAction::BindingMismatch {
                terminal_id,
                session_id,
                ..
            } => Some((terminal_id.as_str(), session_id.as_str())),
            _ => None,
        })
        .collect();
    actions
        .iter()
        .filter_map(|a| match a {
            ReconcileAction::Bind {
                terminal_id,
                session_id,
                config_dir,
                origin,
                confirmed,
            } if !contradicted.contains(&(terminal_id.as_str(), session_id.as_str())) => {
                Some(SessionBoundPayload {
                    terminal_id: terminal_id.clone(),
                    session_id: session_id.clone(),
                    config_dir: config_dir.clone(),
                    origin: origin.as_origin_const().to_string(),
                    confirmed: *confirmed,
                    // Rung 2b's id is the provider's own registry record about
                    // the process. Everything else this module binds is the
                    // runner's inference — including the rung-2 `authoritative`
                    // bind, whose id IS the runner's prediction.
                    provider_reported: *origin == BindOrigin::ProviderRegistry,
                })
            }
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::session_lifecycle_store::BindingState;
    use tempfile::tempdir;

    // ── Item 3b: the transcript walk must not run on a tokio worker ──────

    /// One worker thread, one scan that never returns. If the prescan runs on
    /// the worker (the pre-2026-08-23 shape — `run_reconcile` called inline
    /// from an async fn), that single worker is parked and NOTHING else on the
    /// runtime can be serviced, which is precisely how the runner stopped
    /// answering while its socket still accepted. Off the worker, an unrelated
    /// task still completes.
    ///
    /// Neuter check: replace `spawn_blocking(scan)` in `spawn_scan` with a
    /// direct `scan()` call and this test fails on the `unrelated task` timeout.
    #[test]
    fn the_transcript_prescan_does_not_park_a_tokio_worker() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("runtime");

        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();

        rt.block_on(async move {
            let scan = tokio::spawn(async move {
                PrescannedTranscriptIndex::spawn_scan(move || {
                    let _ = entered_tx.send(());
                    let _ = release_rx.recv(); // a walk that never returns
                    PrescannedTranscriptIndex::default()
                })
                .await
            });

            // Wait (off the worker) until the scan is genuinely running.
            spawn_blocking_tracked(move || entered_rx.recv())
                .await
                .expect("join")
                .expect("scan entered");

            // The load-bearing assertion: the runtime is still alive.
            //
            // Measured with a std channel, NOT `tokio::time::timeout`: the
            // time driver runs on the very worker under test, so on the
            // regressed (parked) runtime the timer would never fire and this
            // test would HANG instead of failing. A blocking recv on the
            // block_on thread is outside the runtime and always reports.
            let (done_tx, done_rx) = std::sync::mpsc::channel::<u8>();
            tokio::spawn(async move {
                let _ = done_tx.send(42);
            });
            let got = done_rx.recv_timeout(std::time::Duration::from_secs(5));
            assert_eq!(
                got.ok(),
                Some(42),
                "a tokio worker was parked by the transcript walk — the scan is back on the runtime"
            );

            let _ = release_tx.send(());
            let _ = scan.await;
        });
    }

    /// Item 3: a prescan that never returns must be ABANDONED, not awaited
    /// forever. Iteration 14 left this await unbounded.
    ///
    /// **Measured from outside the runtime** (a std `recv_timeout` on the
    /// `block_on` thread): the bound under test is
    /// `off_runtime::deadline`, and asserting it with `tokio::time::timeout`
    /// would hang instead of failing whenever the regressed shape is present.
    ///
    /// Neuter check: replace the `tokio::select!` in `spawn_scan_bounded` with
    /// a bare `task.await` (iteration 14's shape) and this test fails on the
    /// `never gave up` assertion.
    #[test]
    fn a_prescan_that_never_returns_is_abandoned_by_the_bound() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("runtime");

        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let (gave_up_tx, gave_up_rx) = std::sync::mpsc::channel::<usize>();

        rt.spawn(async move {
            let idx = PrescannedTranscriptIndex::spawn_scan_bounded(
                move || {
                    let _ = release_rx.recv(); // a walk that never returns
                    PrescannedTranscriptIndex::default()
                },
                std::time::Duration::from_millis(300),
            )
            .await;
            let _ = gave_up_tx.send(idx.probed.len());
        });

        let got = gave_up_rx.recv_timeout(std::time::Duration::from_secs(10));
        assert_eq!(
            got.ok(),
            Some(0),
            "the prescan never gave up — the await is unbounded again, so an exhausted blocking \
             pool holds this pass open forever"
        );

        // And the degraded snapshot must still fail OPEN: an unprobed pair
        // reads UNKNOWN, so nothing is pruned on absent evidence.
        let empty = PrescannedTranscriptIndex::default();
        assert!(
            empty.transcript_exists("anything", Some("C:/wd")),
            "a degraded prescan must not report sessions as missing"
        );

        let _ = release_tx.send(());
    }

    /// Absent evidence must read as UNKNOWN, not as "no transcript" — the
    /// phantom prune deletes records on a `false`, so a pair the prescan never
    /// probed must answer `true`.
    #[test]
    fn an_unprobed_pair_is_unknown_not_missing() {
        let mut idx = PrescannedTranscriptIndex::default();
        idx.probed
            .insert(("known-gone".to_string(), "C:/wd".to_string()));

        assert!(
            !idx.transcript_exists("known-gone", Some("C:/wd")),
            "a probed pair with no transcript must answer false"
        );
        assert!(
            idx.transcript_exists("never-probed", Some("C:/wd")),
            "an unprobed pair must NOT be reported as missing"
        );
        assert!(
            !idx.transcript_exists("anything", None),
            "a record with no working dir keeps the existing false contract"
        );
    }

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

    /// No pane evidence: no pane's own `claude` record was readable.
    fn no_evidence() -> HashMap<String, PaneEvidence> {
        HashMap::new()
    }

    fn evidence(panes: &[(&str, PaneEvidence)]) -> HashMap<String, PaneEvidence> {
        panes
            .iter()
            .map(|(terminal, e)| (terminal.to_string(), e.clone()))
            .collect()
    }

    fn names(id: &str) -> PaneEvidence {
        PaneEvidence::Names(id.to_string())
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

    /// A registry record fixture — `id` on `terminal`, titled `title`.
    fn title_rec(id: &str, terminal: &str, title: Option<&str>) -> TerminalSessionRecord {
        TerminalSessionRecord {
            claude_session_id: id.to_string(),
            config_dir: None,
            working_dir: Some("C:/repo".to_string()),
            page_id: "default".to_string(),
            zone_index: -1,
            title: title.map(str::to_string),
            terminal_id: terminal.to_string(),
            opened_at: 0,
            last_seen_at: 0,
            state: "open".to_string(),
            closed_at: None,
            close_reason: None,
            provider: DEFAULT_PROVIDER.to_string(),
            origin: Some(ORIGIN_AUTHORITATIVE.to_string()),
            restore_pending_at: None,
            awaiting_account_since: None,
            confirmed_at: Some(1),
            handle: None,
            account_label: None,
            account_wrapper: None,
            session_name: None,
            name_source: None,
            tenant_id: None,
            task_run_id: None,
            bypass_permissions: None,
            restored_from_boot_at: None,
            restore_tier: None,
            finished_at: None,
            wind_down_outcome: None,
            wind_down_at: None,
            wind_down_arm: None,
            finish_reason: None,
            finish_synced: false,
            spawn_device_default: None,
            adopted_from: None,
            last_session_start_at: None,
            gate_id: None,
            gate_consuming_device_id: None,
            gate_bound_boot_ms: None,
            binding_state: None,
            placed_strongly: false,
        }
    }

    fn meta(session_id: &str, name: Option<&str>) -> ProviderSessionMeta {
        ProviderSessionMeta {
            session_id: session_id.to_string(),
            name: name.map(str::to_string),
        }
    }

    /// A fake provider-session index over in-memory fixtures.
    struct FakeProviderIndex(HashMap<u32, ProviderSessionMeta>);
    impl ProviderSessionIndex for FakeProviderIndex {
        fn meta_for_pid(&self, pid: u32) -> Option<ProviderSessionMeta> {
            self.0.get(&pid).cloned()
        }
    }

    /// B6: an in-Claude `/rename` reaches the registry — the provider's own
    /// session name replaces the stale spawn-time title.
    #[test]
    fn decide_title_refresh_adopts_the_provider_session_name() {
        let rec = title_rec("sess-1", "term-1", Some("qontinui"));
        assert_eq!(
            decide_title_refresh(&rec, Some(&meta("sess-1", Some("runner-test")))),
            Some("runner-test".to_string()),
            "the operator's in-session name wins over the spawn-time title"
        );
        // A record that never had a title still adopts one.
        let untitled = title_rec("sess-1", "term-1", None);
        assert_eq!(
            decide_title_refresh(&untitled, Some(&meta("sess-1", Some("runner-test")))),
            Some("runner-test".to_string())
        );
    }

    /// B6 guards: every way a refresh could mislabel the WRONG session is
    /// refused. The id cross-check is the load-bearing one — the provider file
    /// is keyed by PID, and pids are reused.
    #[test]
    fn decide_title_refresh_guards() {
        let rec = title_rec("sess-1", "term-1", Some("qontinui"));
        // (a) The file describes a DIFFERENT session (stale file / pid reuse).
        assert_eq!(
            decide_title_refresh(&rec, Some(&meta("other-sess", Some("someone-elses-name")))),
            None,
            "a pid-reused file must never retitle this session"
        );
        // (b) No provider file at all.
        assert_eq!(decide_title_refresh(&rec, None), None);
        // (c) Never renamed → must not erase the existing title.
        assert_eq!(
            decide_title_refresh(&rec, Some(&meta("sess-1", None))),
            None
        );
        // (d) Blank/whitespace name is not a name.
        assert_eq!(
            decide_title_refresh(&rec, Some(&meta("sess-1", Some("   ")))),
            None
        );
        // (e) Already current → no write.
        assert_eq!(
            decide_title_refresh(&rec, Some(&meta("sess-1", Some("qontinui")))),
            None
        );
    }

    /// B6 end-to-end over the store: `refresh_titles` routes the provider name
    /// through the EXISTING `update_title_by_terminal` sink, so one reconcile
    /// tick converges the registry title. An unbound PTY is skipped.
    #[test]
    fn refresh_titles_persists_the_rename_through_the_existing_sink() {
        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();
        store.record_open(title_rec("sess-1", "term-1", Some("qontinui")));

        let live = vec![
            pty_anchored("term-1", 4242, 1_000, "C:/repo"),
            // A live PTY with no record — nothing to title.
            pty_anchored("unbound-term", 5150, 1_000, "C:/repo"),
        ];
        let index = FakeProviderIndex(
            [
                (4242, meta("sess-1", Some("runner-test"))),
                (5150, meta("ghost", Some("ignored"))),
            ]
            .into_iter()
            .collect(),
        );

        let refreshed = refresh_titles(&store, &live, &index);
        assert_eq!(
            refreshed,
            vec![TitleRefresh {
                terminal_id: "term-1".to_string(),
                title: "runner-test".to_string(),
            }],
            "only the bound terminal is retitled"
        );
        assert_eq!(
            store
                .find_open_by_terminal("term-1")
                .unwrap()
                .title
                .as_deref(),
            Some("runner-test"),
            "the rename is DURABLE in the registry, not just in the TUI"
        );

        // Idempotent: a second tick with the same name writes nothing.
        assert!(
            refresh_titles(&store, &live, &index).is_empty(),
            "a converged title does not re-write every tick"
        );
    }

    /// The real provider index parses the provider's own live-session file
    /// (`<config_dir>/sessions/<pid>.json`) — camelCase `sessionId`, and tolerant
    /// of the unknown keys a foreign file carries.
    #[test]
    fn disk_provider_session_index_parses_the_provider_session_file() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sessions")).unwrap();
        std::fs::write(
            dir.path().join("sessions").join("31520.json"),
            r#"{"pid":31520,"sessionId":"ef07e29d-ed7d-4bb3-a85c-d6e3fe92c2f3","cwd":"D:\\qontinui-root","startedAt":1784135767563,"version":"2.1.210","kind":"interactive","name":"coord-push-to-branch-mcp","status":"idle"}"#,
        )
        .unwrap();
        let index = DiskProviderSessionIndex {
            config_dirs: vec![dir.path().to_path_buf()],
        };
        assert_eq!(
            index.meta_for_pid(31520),
            Some(meta(
                "ef07e29d-ed7d-4bb3-a85c-d6e3fe92c2f3",
                Some("coord-push-to-branch-mcp")
            ))
        );
        // An absent pid is a clean None, never an error.
        assert_eq!(index.meta_for_pid(9999), None);

        // A file with no name yet (never renamed) parses, name None.
        std::fs::write(
            dir.path().join("sessions").join("42.json"),
            r#"{"pid":42,"sessionId":"abc","status":"idle"}"#,
        )
        .unwrap();
        assert_eq!(index.meta_for_pid(42), Some(meta("abc", None)));

        // Garbage never panics or poisons the pass.
        std::fs::write(dir.path().join("sessions").join("43.json"), "not json").unwrap();
        assert_eq!(index.meta_for_pid(43), None);
    }

    /// RUNG 2: a typed `--session-id` IS the identity — bind authoritative.
    /// Confirmed iff a transcript for that exact id is already on disk; else
    /// provisional (empty config, unconfirmed).
    #[test]
    fn decide_cmdline_binds_authoritative_confirmed_iff_transcript() {
        let p = pty("term-1", Some(100), "C:/repo");
        // Transcript present for the typed id → confirmed, config carried over.
        let candidates = vec![cand("typed-id", "C:/cfg", Some(1_500))];
        let action = decide_bind_for_live_pty(
            &p,
            None,
            1_000,
            Some("typed-id"),
            None,
            &candidates,
            &no_claims(),
            &no_claims(),
        );
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
        let action2 = decide_bind_for_live_pty(
            &p,
            None,
            1_000,
            Some("typed-id"),
            None,
            &[],
            &no_claims(),
            &no_claims(),
        );
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
        let action = decide_bind_for_live_pty(
            &p,
            None,
            1_000,
            None,
            None,
            &candidates,
            &no_claims(),
            &no_claims(),
        );
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
        let action = decide_bind_for_live_pty(
            &p,
            None,
            1_000,
            None,
            None,
            &candidates,
            &no_claims(),
            &no_claims(),
        );
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
        let action = decide_bind_for_live_pty(
            &p,
            None,
            1_000,
            None,
            None,
            &candidates,
            &no_claims(),
            &claimed,
        );
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
        let action = decide_bind_for_live_pty(
            &p,
            None,
            1_000,
            None,
            None,
            &candidates,
            &no_claims(),
            &no_claims(),
        );
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
        let mut rec = bind_record(
            &p,
            "already",
            "C:/cfg",
            ORIGIN_OBSERVED,
            false,
            UNZONED_ZONE_INDEX,
        );
        rec.confirmed_at = Some(123);
        let candidates = vec![cand("ours", "C:/cfg", Some(2_000))];
        let action = decide_bind_for_live_pty(
            &p,
            Some(&rec),
            1_000,
            Some("typed-id"),
            None,
            &candidates,
            &no_claims(),
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
        // It binds, but graded `reconciled`, so restore opens it as a plain
        // terminal (terminal-only) and never types a resume for it.
        // Grading it `observed` here would auto-resume a possibly-foreign
        // session on the weakest evidence in the ladder.
        let one = vec![cand("a", "C:/cfg", Some(500))];
        let action =
            decide_bind_for_live_pty(&p, None, 0, None, None, &one, &no_claims(), &no_claims());
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
        let action2 =
            decide_bind_for_live_pty(&p, None, 0, None, None, &two, &no_claims(), &no_claims());
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
        let mut rec = bind_record(
            &p,
            "phantom",
            "C:/cfg",
            ORIGIN_OBSERVED,
            false,
            UNZONED_ZONE_INDEX,
        );
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
            awaiting_account_since: None,
            confirmed_at: None,
            handle: None,
            account_label: None,
            account_wrapper: None,
            session_name: None,
            name_source: None,
            tenant_id: None,
            task_run_id: None,
            bypass_permissions: None,
            restored_from_boot_at: None,
            restore_tier: None,
            finished_at: None,
            wind_down_outcome: None,
            wind_down_at: None,
            wind_down_arm: None,
            finish_reason: None,
            finish_synced: false,
            spawn_device_default: None,
            adopted_from: None,
            last_session_start_at: None,
            gate_id: None,
            gate_consuming_device_id: None,
            gate_bound_boot_ms: None,
            binding_state: None,
            placed_strongly: false,
        };
        store.record_open(phantom);

        // One live PTY with no record — a shim-bypassed claude, anchored on
        // its claude descendant (pid 4242, start = 1000); a post-start
        // transcript exists.
        let live = vec![pty_anchored("live-term", 4242, 1_000, "C:/repo")];
        let snapshot = ProcessSnapshot::default();

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

        let actions = run_reconcile(
            &store,
            &live,
            &snapshot,
            &index,
            &no_cmdlines(),
            &no_evidence(),
        );

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
        let live = vec![pty_anchored("live-term", 4242, 1_000, "C:/repo")];
        let snapshot = ProcessSnapshot::default();
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

        run_reconcile(
            &store,
            &live,
            &snapshot,
            &index,
            &no_cmdlines(),
            &no_evidence(),
        );
        // The bound record now carries the LIVE terminal id, so the 2nd pass
        // finds it confirmed-by-terminal and leaves it alone.
        let second = run_reconcile(
            &store,
            &live,
            &snapshot,
            &index,
            &no_cmdlines(),
            &no_evidence(),
        );
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

        run_reconcile(&store, &live, &snapshot, &index, &cmdlines, &no_evidence());

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
            awaiting_account_since: None,
            confirmed_at: None,
            handle: None,
            account_label: None,
            account_wrapper: None,
            session_name: None,
            name_source: None,
            tenant_id: None,
            task_run_id: None,
            bypass_permissions: None,
            restored_from_boot_at: None,
            restore_tier: None,
            finished_at: None,
            wind_down_outcome: None,
            wind_down_at: None,
            wind_down_arm: None,
            finish_reason: None,
            finish_synced: false,
            spawn_device_default: None,
            adopted_from: None,
            last_session_start_at: None,
            gate_id: None,
            gate_consuming_device_id: None,
            gate_bound_boot_ms: None,
            binding_state: None,
            placed_strongly: false,
        });

        let live = vec![pty_anchored("live-term", 4242, 1_000, "C:/repo")];
        let snapshot = ProcessSnapshot::default();
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

        run_reconcile(
            &store,
            &live,
            &snapshot,
            &index,
            &no_cmdlines(),
            &no_evidence(),
        );

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

    /// A bind must not erase the zone the terminal is KNOWN to sit in.
    ///
    /// This module has no view of the grid, so it once wrote
    /// [`UNZONED_ZONE_INDEX`] on every bind — and `record_open` takes
    /// `zone_index` unconditionally (it is also the frontend zone-move
    /// backstop's write path). The poll therefore overwrote the corrected zone
    /// on every tick, while the backstop — comparing against its own ledger of
    /// what it last WROTE — saw no change and never re-emitted. That is the
    /// self-perpetuating `-1` that made `placement.zoneIndex` read `-1` for
    /// every session in the store.
    ///
    /// The zone is carried from the record found by TERMINAL, so it survives a
    /// bind that supersedes a differently-keyed sibling row — which is exactly
    /// what happens here (`seam-pin` is retired, `real-sess` takes the zone).
    #[test]
    fn run_reconcile_bind_carries_the_terminals_known_zone_forward() {
        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();

        // The seam's spawn-time row, already resolved into a real zone by the
        // frontend backstop.
        store.record_open(TerminalSessionRecord {
            claude_session_id: "seam-pin".to_string(),
            config_dir: None,
            working_dir: Some("C:/repo".to_string()),
            page_id: "real-page".to_string(),
            zone_index: 3,
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
            awaiting_account_since: None,
            confirmed_at: None,
            handle: None,
            account_label: None,
            account_wrapper: None,
            session_name: None,
            name_source: None,
            tenant_id: None,
            task_run_id: None,
            bypass_permissions: None,
            restored_from_boot_at: None,
            restore_tier: None,
            finished_at: None,
            wind_down_outcome: None,
            wind_down_at: None,
            wind_down_arm: None,
            finish_reason: None,
            finish_synced: false,
            spawn_device_default: None,
            adopted_from: None,
            last_session_start_at: None,
            gate_id: None,
            gate_consuming_device_id: None,
            gate_bound_boot_ms: None,
            binding_state: None,
            placed_strongly: false,
        });

        let live = vec![pty_anchored("live-term", 4242, 1_000, "C:/repo")];
        let snapshot = ProcessSnapshot::default();
        let mut by_wd = HashMap::new();
        by_wd.insert(
            "C:/repo".to_string(),
            vec![cand("real-sess", "C:/cfg", Some(1_500))],
        );
        let index = FakeIndex {
            by_wd,
            existing_ids: ["real-sess".to_string()].into_iter().collect(),
        };

        run_reconcile(
            &store,
            &live,
            &snapshot,
            &index,
            &no_cmdlines(),
            &no_evidence(),
        );

        let bound = store.get("real-sess").expect("observed bind written");
        assert_eq!(
            bound.zone_index, 3,
            "the bind inherited the terminal's zone instead of erasing it"
        );
        // The two layout fields are sourced DIFFERENTLY, on purpose. The live
        // PTY genuinely knows its page (`LivePty::page_id`), so the bind takes
        // it from there and the record's stale `"real-page"` is correctly
        // overwritten. It genuinely does NOT know its zone — hence the carry.
        // Pinning both here keeps a future "make them consistent" refactor from
        // silently making the page a carry too.
        assert_eq!(
            bound.page_id, "default",
            "page comes from the live PTY, which is the authority on it"
        );
        assert_eq!(
            store.get("seam-pin").unwrap().state,
            "closed",
            "the superseded sibling is still retired — carrying its zone is not keeping it alive"
        );
    }

    /// The complement: with NO prior record for the terminal, the zone is
    /// genuinely unknown and the sentinel is the honest value. Carrying a zone
    /// forward must never become inventing one.
    #[test]
    fn run_reconcile_bind_writes_the_sentinel_when_no_zone_is_known() {
        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();

        let live = vec![pty_anchored("live-term", 4242, 1_000, "C:/repo")];
        let snapshot = ProcessSnapshot::default();
        let mut by_wd = HashMap::new();
        by_wd.insert(
            "C:/repo".to_string(),
            vec![cand("real-sess", "C:/cfg", Some(1_500))],
        );
        let index = FakeIndex {
            by_wd,
            existing_ids: ["real-sess".to_string()].into_iter().collect(),
        };

        run_reconcile(
            &store,
            &live,
            &snapshot,
            &index,
            &no_cmdlines(),
            &no_evidence(),
        );

        assert_eq!(
            store
                .get("real-sess")
                .expect("observed bind written")
                .zone_index,
            UNZONED_ZONE_INDEX,
            "nothing known about the grid ⇒ the sentinel, not a fabricated zone 0"
        );
    }

    /// A pane with a shell and NO claude (`ai_pid: None`) is never bound, even
    /// when exactly one same-cwd transcript started after the shell did. The
    /// shell-start fallback once turned that into a confirmed `observed` row
    /// (2026-09-30: bare shell d8d6a412 bound to unrelated session 01bcee5d),
    /// which a restore then auto-resumed into a fresh pane.
    #[test]
    fn run_reconcile_never_binds_a_shell_with_no_claude() {
        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();

        // Shell pid 4242 started at 1000; no claude image resolved.
        let live = vec![pty("shell-term", Some(4242), "C:/repo")];
        let mut snapshot = ProcessSnapshot::default();
        snapshot.creation_times.insert(4242, 1_000);
        let mut by_wd = HashMap::new();
        by_wd.insert(
            "C:/repo".to_string(),
            vec![cand("foreign-sess", "C:/cfg", Some(1_500))],
        );
        let index = FakeIndex {
            by_wd,
            existing_ids: ["foreign-sess".to_string()].into_iter().collect(),
        };

        let actions = run_reconcile(
            &store,
            &live,
            &snapshot,
            &index,
            &no_cmdlines(),
            &no_evidence(),
        );

        assert!(
            actions.iter().any(|a| matches!(
                a,
                ReconcileAction::LeaveAlone { terminal_id } if terminal_id == "shell-term"
            )),
            "a shell with no claude is left alone, got {actions:?}"
        );
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, ReconcileAction::Bind { .. })),
            "no bind for a shell with no claude, got {actions:?}"
        );
        assert!(
            store.get("foreign-sess").is_none(),
            "no row written for the unrelated transcript"
        );
        assert!(
            store.find_open_by_terminal("shell-term").is_none(),
            "no row written for the shell pane"
        );
    }

    /// The KEPT fallback: a claude image WAS found (`ai_pid: Some`) but its own
    /// start time is unknown (`ai_start_unix: None`). The shell pid's creation
    /// time then anchors the correlation, so a unique same-cwd transcript that
    /// started after the shell binds `Observed` (auto-resume eligible). Removing
    /// the fallback would leave the anchor at 0 and downgrade this real session
    /// to `Reconciled`.
    #[test]
    fn run_reconcile_claude_with_unknown_start_falls_back_to_shell_start() {
        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();

        // Shell pid 4242 started at 1000; claude 5151 found, start unknown.
        let live = vec![LivePty {
            ai_pid: Some(5151),
            ai_start_unix: None,
            ..pty("claude-term", Some(4242), "C:/repo")
        }];
        let mut snapshot = ProcessSnapshot::default();
        snapshot.creation_times.insert(4242, 1_000);
        let mut by_wd = HashMap::new();
        by_wd.insert(
            "C:/repo".to_string(),
            vec![cand("real-sess", "C:/cfg", Some(1_500))],
        );
        let index = FakeIndex {
            by_wd,
            existing_ids: ["real-sess".to_string()].into_iter().collect(),
        };

        let actions = run_reconcile(
            &store,
            &live,
            &snapshot,
            &index,
            &no_cmdlines(),
            &no_evidence(),
        );

        assert!(
            actions.contains(&ReconcileAction::Bind {
                terminal_id: "claude-term".to_string(),
                session_id: "real-sess".to_string(),
                config_dir: "C:/cfg".to_string(),
                origin: BindOrigin::Observed,
                confirmed: true,
            }),
            "shell-start fallback anchors the bind ⇒ Observed, got {actions:?}"
        );
        assert_eq!(
            store
                .get("real-sess")
                .expect("observed bind written")
                .origin
                .as_deref(),
            Some(ORIGIN_OBSERVED)
        );
    }

    /// An authoritative, UNCONFIRMED open row for `id` on `terminal` in
    /// `C:/repo` — the spawn-time pin of a pane whose hook never confirmed.
    fn unconfirmed_on(id: &str, terminal: &str) -> TerminalSessionRecord {
        TerminalSessionRecord {
            confirmed_at: None,
            ..title_rec(id, terminal, Some("T"))
        }
    }

    /// The Phase 1 reproduction (plan
    /// `2026-10-31-runner-binds-a-terminal-to-the-wrong-live-session`): the shape
    /// seen on merytshost on 2026-10-25 — which binder made the move there is
    /// UNKNOWN until that box's runner logs are read; this test shows rung 3 is
    /// one that can.
    ///
    /// Two live panes in one workdir, each with its own UNCONFIRMED open row.
    /// S_A's transcript FAILS rung 3's post-start filter (no parseable first
    /// timestamp); S_B's transcript started after A's claude, so for A it is the
    /// UNIQUE passing same-cwd candidate. While the claimed set held CONFIRMED
    /// rows only, nothing stopped rung 3 binding S_B to A: the store moved S_B's
    /// row off B and the supersede scan evicted S_A.
    ///
    /// Both terminals must keep their own session — here with NO pane record
    /// readable, so the claimed set alone (every id open on another live pane,
    /// confirmed or not) is what stops rung 3. Run alone on a clean
    /// `fd646a5be` (commit `6b06e1256`, tree line `dirty_files=0`) it FAILS:
    /// `Bind { term-a, s-b, Observed, confirmed }`, and S_A ends `closed`.
    #[test]
    fn rung3_never_moves_a_live_peers_unconfirmed_session_onto_another_pane() {
        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();
        store.record_open(unconfirmed_on("s-a", "term-a"));
        store.record_open(unconfirmed_on("s-b", "term-b"));

        let live = vec![
            pty_anchored("term-a", 100, 1_000, "C:/repo"),
            pty_anchored("term-b", 200, 900, "C:/repo"),
        ];
        let mut by_wd = HashMap::new();
        by_wd.insert(
            "C:/repo".to_string(),
            vec![
                // S_A's transcript carries no parseable first event.
                cand("s-a", "C:/cfg", None),
                // S_B's started after A's claude (1000) and B's (900).
                cand("s-b", "C:/cfg", Some(1_500)),
            ],
        );
        let index = FakeIndex {
            by_wd,
            existing_ids: ["s-a".to_string(), "s-b".to_string()].into_iter().collect(),
        };

        let actions = run_reconcile(
            &store,
            &live,
            &ProcessSnapshot::default(),
            &index,
            &no_cmdlines(),
            &no_evidence(),
        );

        let a = store.get("s-a").expect("S_A row");
        let b = store.get("s-b").expect("S_B row");
        assert_eq!(
            (a.state.as_str(), a.terminal_id.as_str()),
            ("open", "term-a"),
            "the thief pane keeps its own session (actions: {actions:?})"
        );
        assert_eq!(
            (b.state.as_str(), b.terminal_id.as_str()),
            ("open", "term-b"),
            "the victim pane keeps its row (actions: {actions:?})"
        );
    }

    /// The two panes of the reproduction — A (`term-a`, claude 100 started at
    /// 1000) and B (`term-b`, claude 200 started at 900) in one workdir — with
    /// S_A's transcript unanchorable (no first timestamp) and S_B's started
    /// after both claudes, so rung 3 would pick S_B for A.
    fn two_panes_one_workdir() -> (Vec<LivePty>, FakeIndex) {
        let live = vec![
            pty_anchored("term-a", 100, 1_000, "C:/repo"),
            pty_anchored("term-b", 200, 900, "C:/repo"),
        ];
        let mut by_wd = HashMap::new();
        by_wd.insert(
            "C:/repo".to_string(),
            vec![
                cand("s-a", "C:/cfg", None),
                cand("s-b", "C:/cfg", Some(1_500)),
            ],
        );
        let index = FakeIndex {
            by_wd,
            existing_ids: ["s-a".to_string(), "s-b".to_string()].into_iter().collect(),
        };
        (live, index)
    }

    fn no_transcripts() -> FakeIndex {
        FakeIndex {
            by_wd: HashMap::new(),
            existing_ids: HashSet::new(),
        }
    }

    /// A store whose panes answer `panes` and whose binding counters are its
    /// own.
    fn store_with_panes(
        dir: &std::path::Path,
        panes: &[(&str, PaneEvidence)],
    ) -> (
        SessionLifecycleStore,
        std::sync::Arc<pane_binding::BindingCounters>,
    ) {
        let store = SessionLifecycleStore::open(dir.join("s.json")).unwrap();
        store.attach_pane_evidence(pane_binding::FixedPanes::of(panes));
        let counters = std::sync::Arc::new(pane_binding::BindingCounters::default());
        store.attach_binding_counters(counters.clone());
        (store, counters)
    }

    /// Phase 3 regression, through `run_reconcile` and the store: the
    /// reproduction's shape WITH each pane's own `claude` record readable. Each
    /// pane is bound from its own record (rung 2b) before rung 3 can guess,
    /// both rows end on their own pane, matched, and each tab lights up with its
    /// own session — never the other's labels.
    #[test]
    fn two_panes_whose_records_name_their_own_sessions_each_keep_their_own() {
        let dir = tempdir().unwrap();
        let panes = [("term-a", names("s-a")), ("term-b", names("s-b"))];
        let (store, counters) = store_with_panes(dir.path(), &panes);
        store.record_open(unconfirmed_on("s-a", "term-a"));
        store.record_open(unconfirmed_on("s-b", "term-b"));
        let (live, index) = two_panes_one_workdir();

        let actions = run_reconcile(
            &store,
            &live,
            &ProcessSnapshot::default(),
            &index,
            &no_cmdlines(),
            &evidence(&panes),
        );

        for (session, terminal) in [("s-a", "term-a"), ("s-b", "term-b")] {
            assert!(
                actions.contains(&ReconcileAction::Bind {
                    terminal_id: terminal.to_string(),
                    session_id: session.to_string(),
                    config_dir: "C:/cfg".to_string(),
                    origin: BindOrigin::ProviderRegistry,
                    confirmed: true,
                }),
                "{terminal} is bound from its own claude's record: {actions:?}"
            );
            let row = store.get(session).unwrap();
            assert_eq!(
                (row.state.as_str(), row.terminal_id.as_str()),
                ("open", terminal),
                "{session} stays on its own pane"
            );
            assert_eq!(row.binding_state, Some(BindingState::Matched), "{session}");
            assert_eq!(
                store
                    .find_confirmed_open_by_terminal(terminal)
                    .map(|r| r.claude_session_id),
                Some(session.to_string()),
                "{terminal}'s tab lights up with its own session"
            );
        }
        assert!(
            !actions.iter().any(|a| matches!(
                a,
                ReconcileAction::BindRefused { .. } | ReconcileAction::BindingMismatch { .. }
            )),
            "{actions:?}"
        );
        assert_eq!((counters.mismatches(), counters.refused_moves()), (0, 0));
    }

    /// One transcript per session, every one started at `started` in the
    /// two panes' working dir — so a registry bind of any of them is
    /// confirmed.
    fn transcripts(sessions: &[&str], started: i64) -> FakeIndex {
        let mut by_wd = HashMap::new();
        by_wd.insert(
            "C:/repo".to_string(),
            sessions
                .iter()
                .map(|id| cand(id, "C:/cfg", Some(started)))
                .collect(),
        );
        FakeIndex {
            by_wd,
            existing_ids: sessions.iter().map(|id| id.to_string()).collect(),
        }
    }

    /// A missed hook: A's `claude` switched to `s-new` (a `/clear`, an in-TUI
    /// `/resume`, a new `claude`) and no hook reported it, so A's record names
    /// a session it holds no row of. The old row is judged on nothing — its
    /// `matched` withdrawn, nothing counted — and A is not left bound to it:
    /// holding no confirmed row of what its record names, A is unbound, and
    /// rung 2b binds `s-new` from the record. Confirmed (its transcript is
    /// there), the bind retires the old row; the next pass changes nothing.
    #[test]
    fn a_missed_hook_leaves_the_old_row_unjudged_and_rebinds_the_pane_from_its_record() {
        let dir = tempdir().unwrap();
        let panes = [
            ("term-a", names("s-new")),
            ("term-b", PaneEvidence::Unknown),
        ];
        let (store, counters) = store_with_panes(dir.path(), &panes);
        store.record_open(title_rec("s-old", "term-a", Some("T")));
        let _ = store.set_binding_state("s-old", "term-a", BindingState::Matched, Some(0));
        let (live, _) = two_panes_one_workdir();
        let index = transcripts(&["s-new"], 1_500);
        let pass = || {
            run_reconcile(
                &store,
                &live,
                &ProcessSnapshot::default(),
                &index,
                &no_cmdlines(),
                &evidence(&panes),
            )
        };

        let actions = pass();

        assert!(
            actions.contains(&ReconcileAction::Bind {
                terminal_id: "term-a".to_string(),
                session_id: "s-new".to_string(),
                config_dir: "C:/cfg".to_string(),
                origin: BindOrigin::ProviderRegistry,
                confirmed: true,
            }),
            "{actions:?}"
        );
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, ReconcileAction::BindingMismatch { .. })),
            "{actions:?}"
        );
        let new = store.get("s-new").unwrap();
        assert_eq!(
            (new.terminal_id.as_str(), new.binding_state),
            ("term-a", Some(BindingState::Matched))
        );
        let old = store.get("s-old").unwrap();
        assert_eq!(old.binding_state, None, "its matched was withdrawn");
        assert_eq!(old.state, "closed", "retired by the confirmed bind");
        assert_eq!(
            store
                .find_confirmed_open_by_terminal("term-a")
                .map(|r| r.claude_session_id),
            Some("s-new".to_string())
        );

        let again = pass();
        assert!(
            again.contains(&ReconcileAction::LeaveAlone {
                terminal_id: "term-a".to_string()
            }),
            "{again:?}"
        );
        assert_eq!((counters.mismatches(), counters.refused_moves()), (0, 0));
    }

    /// M2: the same session running in TWO panes (both records name it, its
    /// row on B). A's registry rung never proposes it — another pane holds it —
    /// so nothing is refused, nothing counted, and the row stays on B, tick
    /// after tick. A's spawn seam's unconfirmed pin gets no verdict: its pane
    /// runs another session, and nothing else runs the pin.
    #[test]
    fn a_session_two_panes_run_is_never_proposed_for_the_pane_that_does_not_hold_it() {
        let dir = tempdir().unwrap();
        let panes = [("term-a", names("s-b")), ("term-b", names("s-b"))];
        let (store, counters) = store_with_panes(dir.path(), &panes);
        store.record_open(unconfirmed_on("s-b", "term-b"));
        store.record_open(unconfirmed_on("pin-a", "term-a"));
        let (live, _) = two_panes_one_workdir();

        for _ in 0..3 {
            let actions = run_reconcile(
                &store,
                &live,
                &ProcessSnapshot::default(),
                &no_transcripts(),
                &no_cmdlines(),
                &evidence(&panes),
            );
            assert!(
                actions.contains(&ReconcileAction::LeaveAlone {
                    terminal_id: "term-a".to_string()
                }),
                "{actions:?}"
            );
            assert!(
                !actions
                    .iter()
                    .any(|a| matches!(a, ReconcileAction::BindRefused { .. })),
                "{actions:?}"
            );
        }
        assert_eq!(store.get("s-b").unwrap().terminal_id, "term-b");
        assert_eq!(
            store.get("s-b").unwrap().binding_state,
            Some(BindingState::Matched)
        );
        assert_eq!(store.get("pin-a").unwrap().binding_state, None);
        assert_eq!((counters.mismatches(), counters.refused_moves()), (0, 0));
    }

    /// `/clear` on pane A (`s` → `s2`, hook-reported), then the predecessor
    /// `s` resumed — in pane B (whose hook moves `s` there) or inside A's own
    /// TUI. In both worlds — A's record follows the switch or still names its
    /// launch session — nothing reads `unknown` and nothing is counted: the
    /// pane that runs `s` names `s`, so `s` is matched there.
    #[test]
    fn a_clear_then_a_resume_of_the_predecessor_marks_nothing_unknown() {
        // (A's record, B's record, where the resume moved `s`)
        let worlds = [
            ("follows: resumed in B", names("s2"), names("s"), "term-b"),
            ("lags: resumed in B", names("s"), names("s"), "term-b"),
            // Resumed inside A, A names `s` either way: as the session it
            // switched back to, or as the one it launched with.
            (
                "either: resumed in A",
                names("s"),
                PaneEvidence::NoClaude,
                "term-a",
            ),
        ];
        for (world, a, b, resumed_on) in worlds {
            let dir = tempdir().unwrap();
            let panes = [("term-a", a), ("term-b", b)];
            let (store, counters) = store_with_panes(dir.path(), &panes);
            // `s` launched on A; `/clear` made `s2` there, hook-confirmed.
            store.record_open(title_rec("s", "term-a", Some("T")));
            store.record_open(TerminalSessionRecord {
                adopted_from: Some("s".to_string()),
                ..title_rec("s2", "term-a", Some("T"))
            });
            // The resume's hook: a strong bind, never refused.
            assert_eq!(
                store.record_open_checked(title_rec("s", resumed_on, Some("T"))),
                RecordOpenOutcome::Recorded,
                "{world}"
            );
            let (live, _) = two_panes_one_workdir();

            for _ in 0..2 {
                let actions = run_reconcile(
                    &store,
                    &live,
                    &ProcessSnapshot::default(),
                    &no_transcripts(),
                    &no_cmdlines(),
                    &evidence(&panes),
                );
                assert!(
                    !actions.iter().any(|a| matches!(
                        a,
                        ReconcileAction::BindingMismatch { .. }
                            | ReconcileAction::BindRefused { .. }
                    )),
                    "{world}: {actions:?}"
                );
            }
            let row = store.get("s").unwrap();
            assert_eq!(
                (row.terminal_id.as_str(), row.binding_state),
                (resumed_on, Some(BindingState::Matched)),
                "{world}"
            );
            assert_ne!(
                store.get("s2").unwrap().binding_state,
                Some(BindingState::Unknown),
                "{world}"
            );
            assert_eq!(
                (counters.mismatches(), counters.refused_moves()),
                (0, 0),
                "{world}"
            );
        }
    }

    /// The lag world, through the pass: pane A's record still names `s` (its
    /// launch session — A has since switched to `s2`) while B runs `s`. `s` is
    /// matched on B; nothing on A reads `unknown`; and A's registry rung never
    /// takes `s` off B, the live pane that holds and runs it.
    #[test]
    fn a_stale_record_naming_a_session_another_pane_runs_marks_nothing_unknown() {
        let dir = tempdir().unwrap();
        let panes = [("term-a", names("s")), ("term-b", names("s"))];
        let (store, counters) = store_with_panes(dir.path(), &panes);
        store.record_open(title_rec("s2", "term-a", Some("T")));
        store.record_open(title_rec("s", "term-b", Some("T")));
        let (live, _) = two_panes_one_workdir();

        for _ in 0..3 {
            let actions = run_reconcile(
                &store,
                &live,
                &ProcessSnapshot::default(),
                &transcripts(&["s", "s2"], 1_500),
                &no_cmdlines(),
                &evidence(&panes),
            );
            assert!(
                actions.contains(&ReconcileAction::LeaveAlone {
                    terminal_id: "term-a".to_string()
                }),
                "{actions:?}"
            );
        }
        let s = store.get("s").unwrap();
        assert_eq!(
            (s.terminal_id.as_str(), s.binding_state),
            ("term-b", Some(BindingState::Matched))
        );
        let s2 = store.get("s2").unwrap();
        assert_eq!(
            (s2.state.as_str(), s2.terminal_id.as_str(), s2.binding_state),
            ("open", "term-a", None)
        );
        assert_eq!((counters.mismatches(), counters.refused_moves()), (0, 0));
    }

    /// A terminal-only cold restore re-points `s-old`'s row at a fresh plain
    /// shell (a weak rebind, off a terminal nothing runs). With no `claude`
    /// there the row is judged on nothing; then a `claude` starts in that pane
    /// — without a hook, and in a second run with one — and still nothing is
    /// counted: `s-old` runs nowhere.
    #[test]
    fn a_cold_restore_then_a_claude_starting_in_the_pane_counts_nothing() {
        for hook in [false, true] {
            let dir = tempdir().unwrap();
            let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();
            let registry = std::sync::Arc::new(pane_binding::LivePaneRegistry::new());
            store.attach_pane_evidence(registry.clone());
            let counters = std::sync::Arc::new(pane_binding::BindingCounters::default());
            store.attach_binding_counters(counters.clone());
            store.record_open(title_rec("s-old", "term-dead", Some("T")));
            let me = std::process::id();
            let observe = |claude: Option<&str>| {
                let observation = PaneObservation {
                    panes: [(
                        "term-a".to_string(),
                        claude
                            .map(|_| {
                                vec![LivePid {
                                    pid: me,
                                    process_started_ms: Some(1_000_000),
                                }]
                            })
                            .unwrap_or_default(),
                    )]
                    .into_iter()
                    .collect(),
                    sessions: claude.map(|id| (me, id.to_string())).into_iter().collect(),
                    read_at: None,
                };
                let evidence = observation.evidence_map();
                let epoch = store.observe_panes(observation);
                (evidence, epoch)
            };

            // The restore: a weak rebind, off a terminal no pane runs.
            assert_eq!(
                store.rebind_terminal_checked("s-old", "term-a", 0),
                RecordOpenOutcome::Recorded
            );
            // A plain shell: nothing to judge the row by.
            let (evidence, epoch) = observe(None);
            let shell = vec![pty("term-a", Some(1), "C:/repo")];
            run_reconcile_at_epoch(
                &store,
                &shell,
                &ProcessSnapshot::default(),
                &no_transcripts(),
                &no_cmdlines(),
                &evidence,
                epoch,
            );
            assert_eq!(store.get("s-old").unwrap().binding_state, None);

            // `claude` starts in the pane: a new session.
            if hook {
                store.record_open(title_rec("s-new", "term-a", Some("T")));
            }
            let (evidence, epoch) = observe(Some("s-new"));
            let running = vec![pty_anchored("term-a", me, 1_000, "C:/repo")];
            let actions = run_reconcile_at_epoch(
                &store,
                &running,
                &ProcessSnapshot::default(),
                &transcripts(&["s-new"], 1_500),
                &no_cmdlines(),
                &evidence,
                epoch,
            );
            assert!(
                !actions
                    .iter()
                    .any(|a| matches!(a, ReconcileAction::BindingMismatch { .. })),
                "hook={hook}: {actions:?}"
            );
            let new = store.get("s-new").unwrap();
            assert_eq!(
                (new.terminal_id.as_str(), new.binding_state),
                ("term-a", Some(BindingState::Matched)),
                "hook={hook}"
            );
            assert_eq!(
                store
                    .current_binding_verdicts()
                    .get("s-new")
                    .map(|v| (v.terminal_id.as_str(), v.state)),
                Some(("term-a", BindingState::Matched)),
                "hook={hook}: the pass's own epoch"
            );
            assert_ne!(
                store.get("s-old").unwrap().binding_state,
                Some(BindingState::Unknown),
                "hook={hook}"
            );
            assert_eq!(
                (counters.mismatches(), counters.refused_moves()),
                (0, 0),
                "hook={hook}"
            );
        }
    }

    /// The 2026-10-25 shape, as rung 3 reproduces it (which binder made the
    /// real move is UNKNOWN): a WEAK bind moved `s-b` off live pane B, which
    /// still runs it, onto pane A, which runs `s-a`, and `s-a`'s row was
    /// superseded by the move. (The move is made before any pane evidence
    /// exists, as a bind from before the guard would have been.) The victim
    /// row reads `unknown` and is counted ONCE — its session runs in B — then
    /// rung 2b heals both panes from their own records, and the next pass
    /// counts nothing.
    #[test]
    fn the_2026_10_25_shape_is_counted_once_and_healed_from_the_records() {
        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();
        store.record_open(title_rec("s-a", "term-a", Some("T")));
        store.record_open(title_rec("s-b", "term-b", Some("T")));
        // The wrong move: rung 3's confirmed `observed` bind of s-b onto A.
        store.record_open(TerminalSessionRecord {
            origin: Some(ORIGIN_OBSERVED.to_string()),
            ..title_rec("s-b", "term-a", Some("T"))
        });
        assert_eq!(store.get("s-a").unwrap().state, "closed");
        assert!(!store.get("s-b").unwrap().placed_strongly);
        let panes = [("term-a", names("s-a")), ("term-b", names("s-b"))];
        store.attach_pane_evidence(pane_binding::FixedPanes::of(&panes));
        let counters = std::sync::Arc::new(pane_binding::BindingCounters::default());
        store.attach_binding_counters(counters.clone());
        let (live, _) = two_panes_one_workdir();
        let pass = || {
            run_reconcile(
                &store,
                &live,
                &ProcessSnapshot::default(),
                &no_transcripts(),
                &no_cmdlines(),
                &evidence(&panes),
            )
        };

        let actions = pass();

        assert!(
            actions.contains(&ReconcileAction::BindingMismatch {
                terminal_id: "term-a".to_string(),
                session_id: "s-b".to_string(),
                runs_on: "term-b".to_string(),
            }),
            "{actions:?}"
        );
        assert_eq!(counters.mismatches(), 1);
        for (id, terminal) in [("s-a", "term-a"), ("s-b", "term-b")] {
            let row = store.get(id).unwrap();
            assert_eq!(
                (
                    row.state.as_str(),
                    row.terminal_id.as_str(),
                    row.binding_state
                ),
                ("open", terminal, Some(BindingState::Matched)),
                "{id}: {actions:?}"
            );
        }

        let again = pass();
        assert!(
            !again
                .iter()
                .any(|a| matches!(a, ReconcileAction::BindingMismatch { .. })),
            "{again:?}"
        );
        assert_eq!((counters.mismatches(), counters.refused_moves()), (1, 0));
    }

    /// What the SessionStart hook route does for `id` on `terminal`: an
    /// authoritative record written UNCONFIRMED, then the confirmation and the
    /// start stamp, separately (`install_effects_producer`). Its write never
    /// retires a confirmed sibling.
    fn hook(store: &SessionLifecycleStore, id: &str, terminal: &str) {
        assert!(store
            .record_open_checked(TerminalSessionRecord {
                confirmed_at: None,
                ..title_rec(id, terminal, Some("T"))
            })
            .is_recorded());
        store.confirm_session(id);
        store.note_session_start_hook(id);
    }

    /// Review M1, on a build whose records follow a switch: P ran `s` (hook),
    /// then `/clear` made `s2` there (hook). The hook route's unconfirmed write
    /// retires no confirmed sibling, so `s`'s row stays open on P, strongly
    /// placed, while P's record names `s2`. Then `s` is resumed in pane Q with
    /// no hook, and Q's record names it. `s` reads `unknown` on P — and, no
    /// longer the session that most recently started there, holds nothing: Q's
    /// registry rung takes it in ONE pass.
    #[test]
    fn a_strong_row_its_pane_has_moved_past_goes_to_the_pane_that_runs_it() {
        let dir = tempdir().unwrap();
        let panes = [("term-a", names("s2")), ("term-b", names("s"))];
        let (store, counters) = store_with_panes(dir.path(), &panes);
        hook(&store, "s", "term-a");
        hook(&store, "s2", "term-a");
        store.mark_adopted_from("s2", "s");
        let stale = store.get("s").unwrap();
        assert_eq!(
            (stale.state.as_str(), stale.placed_strongly),
            ("open", true),
            "precondition: the /clear left s open and strong on P"
        );
        let (live, _) = two_panes_one_workdir();

        let actions = run_reconcile(
            &store,
            &live,
            &ProcessSnapshot::default(),
            &transcripts(&["s", "s2"], 1_500),
            &no_cmdlines(),
            &evidence(&panes),
        );

        assert!(
            actions.contains(&ReconcileAction::Bind {
                terminal_id: "term-b".to_string(),
                session_id: "s".to_string(),
                config_dir: "C:/cfg".to_string(),
                origin: BindOrigin::ProviderRegistry,
                confirmed: true,
            }),
            "{actions:?}"
        );
        let s = store.get("s").unwrap();
        assert_eq!(
            (s.terminal_id.as_str(), s.binding_state),
            ("term-b", Some(BindingState::Matched))
        );
        assert_eq!(
            store.get("s2").unwrap().binding_state,
            Some(BindingState::Matched)
        );
        assert_eq!(counters.mismatches(), 1, "s read unknown on P once");
    }

    /// A strong row on a pane whose `claude` has EXITED holds nothing: nothing
    /// there can be running it. P ran `s` (hook) and its `claude` exited; `s`
    /// was resumed in pane Q with no hook. `s` moves to Q in one pass.
    #[test]
    fn a_strong_row_on_a_pane_with_no_claude_holds_nothing() {
        let dir = tempdir().unwrap();
        let panes = [("term-a", PaneEvidence::NoClaude), ("term-b", names("s"))];
        let (store, _counters) = store_with_panes(dir.path(), &panes);
        hook(&store, "s", "term-a");
        let (live, _) = two_panes_one_workdir();

        run_reconcile(
            &store,
            &live,
            &ProcessSnapshot::default(),
            &transcripts(&["s"], 1_500),
            &no_cmdlines(),
            &evidence(&panes),
        );

        let s = store.get("s").unwrap();
        assert_eq!(
            (s.terminal_id.as_str(), s.binding_state),
            ("term-b", Some(BindingState::Matched))
        );
    }

    /// Review M1, case (i), on a build whose records do NOT follow a switch:
    /// pane B launched `x`, then resumed `s` inside its TUI — B's hook moved
    /// `s`'s row there, a strong bind, and it is the session that most
    /// recently started on B — while pane A, which launched `s` and has since
    /// run `/clear`, still names `s`, and B still names `x`. `s` reads
    /// `unknown`, counted once for the episode, but stays on B pass after
    /// pass: it holds its session against A's provider-registry rung, so
    /// nothing moves it back.
    #[test]
    fn a_row_a_hook_placed_is_never_moved_back_by_a_stale_record() {
        let dir = tempdir().unwrap();
        let panes = [("term-a", names("s")), ("term-b", names("x"))];
        let (store, counters) = store_with_panes(dir.path(), &panes);
        // A launched `s`; its `/clear` made `s2` there.
        hook(&store, "s", "term-a");
        hook(&store, "s2", "term-a");
        store.mark_adopted_from("s2", "s");
        // B launched `x`, then resumed `s` inside its TUI: B's hook moves `s`.
        hook(&store, "x", "term-b");
        hook(&store, "s", "term-b");
        assert!(store.get("s").unwrap().placed_strongly);
        assert_eq!(
            store.latest_started_on_terminal("term-b").as_deref(),
            Some("s")
        );
        let (live, _) = two_panes_one_workdir();

        for _ in 0..3 {
            let actions = run_reconcile(
                &store,
                &live,
                &ProcessSnapshot::default(),
                &transcripts(&["s", "s2", "x"], 1_500),
                &no_cmdlines(),
                &evidence(&panes),
            );
            assert!(
                !actions.iter().any(|a| matches!(
                    a,
                    ReconcileAction::Bind { session_id, .. } if session_id == "s"
                )),
                "{actions:?}"
            );
            let row = store.get("s").unwrap();
            assert_eq!(
                (row.state.as_str(), row.terminal_id.as_str()),
                ("open", "term-b"),
                "{actions:?}"
            );
        }
        assert_eq!(
            store.get("s").unwrap().binding_state,
            Some(BindingState::Unknown)
        );
        assert_eq!(counters.mismatches(), 1, "one episode, counted once");
        assert_eq!(counters.refused_moves(), 0);
    }

    /// Round-3 review L1: a confirmed row on a DEAD terminal does not stop the
    /// registry rung. A hookless pane whose own record names `s` — an operator
    /// typed `claude --resume s` there — is bound to `s`, and the row leaves
    /// the terminal nothing runs on.
    #[test]
    fn a_row_on_a_dead_terminal_does_not_keep_a_live_pane_from_its_own_record() {
        let dir = tempdir().unwrap();
        let panes = [("term-a", names("s")), ("term-b", PaneEvidence::Unknown)];
        let (store, counters) = store_with_panes(dir.path(), &panes);
        store.record_open(title_rec("s", "term-dead", Some("S")));
        assert!(store.get("s").unwrap().confirmed_at.is_some());
        let (live, _) = two_panes_one_workdir();

        let actions = run_reconcile(
            &store,
            &live,
            &ProcessSnapshot::default(),
            &no_transcripts(),
            &no_cmdlines(),
            &evidence(&panes),
        );

        assert!(
            actions.iter().any(|a| matches!(
                a,
                ReconcileAction::Bind { terminal_id, session_id, origin: BindOrigin::ProviderRegistry, .. }
                    if terminal_id == "term-a" && session_id == "s"
            )),
            "{actions:?}"
        );
        let row = store.get("s").unwrap();
        assert_eq!(
            (row.terminal_id.as_str(), row.binding_state),
            ("term-a", Some(BindingState::Matched))
        );
        assert_eq!((counters.mismatches(), counters.refused_moves()), (0, 0));
    }

    /// Round-3 review L4: a hookless pane is re-bound every pass; once its row
    /// holds everything the bind would write, the re-bind writes nothing — no
    /// durable record of it — while a bind that changes the row still does.
    #[test]
    fn an_unchanged_re_bind_writes_nothing() {
        let dir = tempdir().unwrap();
        let panes = [("term-a", names("s")), ("term-b", PaneEvidence::Unknown)];
        let (store, _counters) = store_with_panes(dir.path(), &panes);
        let (live, _) = two_panes_one_workdir();
        let pass = || {
            run_reconcile(
                &store,
                &live,
                &ProcessSnapshot::default(),
                &no_transcripts(),
                &no_cmdlines(),
                &evidence(&panes),
            )
        };
        let rebound = |actions: &[ReconcileAction]| {
            actions.iter().any(|a| {
                matches!(a, ReconcileAction::Bind { terminal_id, session_id, .. }
                    if terminal_id == "term-a" && session_id == "s")
            })
        };

        let first = pass();
        assert!(rebound(&first), "{first:?}");
        let written = store.get("s").unwrap();
        assert!(written.confirmed_at.is_none(), "no transcript: provisional");

        std::thread::sleep(std::time::Duration::from_millis(5));
        let second = pass();
        assert!(rebound(&second), "the pane is still re-bound: {second:?}");
        assert_eq!(
            store.get("s").unwrap().last_seen_at,
            written.last_seen_at,
            "an unchanged re-bind is not written"
        );

        // A bind that would change the row is written.
        store.record_open(TerminalSessionRecord {
            confirmed_at: None,
            ..title_rec("s", "term-a", Some("an old title"))
        });
        let before = store.get("s").unwrap().last_seen_at;
        std::thread::sleep(std::time::Duration::from_millis(5));
        pass();
        let row = store.get("s").unwrap();
        assert_eq!(row.title.as_deref(), Some("T"), "the pane's title");
        assert!(row.last_seen_at > before);
    }

    /// Phase 3, as the redesign leaves it: rung 2's argv names a session a
    /// live peer pane holds and runs. The store never refuses an authoritative
    /// bind, so the pass never proposes it: A is left alone, nothing is written
    /// or refused, no tab is stamped, and the session stays where it runs.
    #[test]
    fn an_argv_naming_a_live_peers_session_is_never_bound() {
        let dir = tempdir().unwrap();
        let panes = [("term-a", PaneEvidence::Unknown), ("term-b", names("s-b"))];
        let (store, counters) = store_with_panes(dir.path(), &panes);
        store.record_open(unconfirmed_on("s-b", "term-b"));
        let (live, _) = two_panes_one_workdir();
        // A's claude was launched `--resume s-b`.
        let argv: HashMap<u32, String> = [(100, "s-b".to_string())].into_iter().collect();

        for _ in 0..3 {
            let actions = run_reconcile(
                &store,
                &live,
                &ProcessSnapshot::default(),
                &no_transcripts(),
                &argv,
                &evidence(&panes),
            );
            assert!(
                actions.contains(&ReconcileAction::LeaveAlone {
                    terminal_id: "term-a".to_string()
                }),
                "{actions:?}"
            );
            assert!(
                session_bound_payloads(&actions)
                    .iter()
                    .all(|p| p.terminal_id != "term-a"),
                "no tab stamped"
            );
        }
        let row = store.get("s-b").unwrap();
        assert_eq!(
            (row.terminal_id.as_str(), row.binding_state),
            ("term-b", Some(BindingState::Matched))
        );
        assert_eq!((counters.mismatches(), counters.refused_moves()), (0, 0));
    }

    /// A bind the same pass found contradicted is not stamped on a tab.
    #[test]
    fn session_bound_payloads_skip_a_bind_the_pass_found_contradicted() {
        let bind = |terminal: &str, session: &str| ReconcileAction::Bind {
            terminal_id: terminal.to_string(),
            session_id: session.to_string(),
            config_dir: String::new(),
            origin: BindOrigin::ProviderRegistry,
            confirmed: true,
        };
        let payloads = session_bound_payloads(&[
            bind("term-a", "s-a"),
            bind("term-b", "s-b"),
            ReconcileAction::BindingMismatch {
                terminal_id: "term-b".to_string(),
                session_id: "s-b".to_string(),
                runs_on: "term-x".to_string(),
            },
        ]);
        assert_eq!(
            payloads
                .iter()
                .map(|p| (p.terminal_id.as_str(), p.provider_reported))
                .collect::<Vec<_>>(),
            vec![("term-a", true)]
        );
        assert_eq!(payloads[0].origin, "authoritative");
    }

    /// Rung 2b ranks FIRST: the pane's own record binds before any transcript
    /// guess and pre-empts a DIFFERING argv id, confirmed iff a transcript for
    /// that id exists. A record naming a session the pane may not take leaves
    /// the pane alone — the argv and rung 3 are not consulted.
    #[test]
    fn decide_registry_rung_pre_empts_the_argv_and_never_binds_against_the_record() {
        let p = pty_anchored("term-1", 4242, 1_000, "C:/repo");
        let candidates = vec![
            cand("rec-id", "C:/cfg", Some(500)),
            cand("post-start", "C:/cfg", Some(2_000)),
        ];
        let decide = |argv: Option<&str>, registry: Option<&str>, claims: &HashSet<String>| {
            decide_bind_for_live_pty(&p, None, 1_000, argv, registry, &candidates, claims, claims)
        };
        let registry_bind = |id: &str, config_dir: &str, confirmed: bool| ReconcileAction::Bind {
            terminal_id: "term-1".to_string(),
            session_id: id.to_string(),
            config_dir: config_dir.to_string(),
            origin: BindOrigin::ProviderRegistry,
            confirmed,
        };
        assert_eq!(
            decide(None, Some("rec-id"), &no_claims()),
            registry_bind("rec-id", "C:/cfg", true)
        );
        assert_eq!(
            decide(None, Some("fresh-id"), &no_claims()),
            registry_bind("fresh-id", "", false),
            "no transcript yet: provisional"
        );
        assert_eq!(
            decide(Some("typed-id"), Some("rec-id"), &no_claims()),
            registry_bind("rec-id", "C:/cfg", true),
            "a differing argv id is pre-empted"
        );
        let claimed: HashSet<String> = ["rec-id".to_string()].into_iter().collect();
        assert_eq!(
            decide(Some("typed-id"), Some("rec-id"), &claimed),
            ReconcileAction::LeaveAlone {
                terminal_id: "term-1".to_string()
            },
            "never bound to anything its own record contradicts"
        );
        assert!(matches!(
            decide(Some("typed-id"), None, &no_claims()),
            ReconcileAction::Bind { ref session_id, origin: BindOrigin::Authoritative, .. }
                if session_id == "typed-id"
        ));
        // The argv rung takes no session a peer holds or runs either: the
        // store never refuses an authoritative bind.
        assert_eq!(
            decide(
                Some("typed-id"),
                None,
                &["typed-id".to_string()].into_iter().collect()
            ),
            ReconcileAction::LeaveAlone {
                terminal_id: "term-1".to_string()
            }
        );
        // Round-3 review L1: only a LIVE pane's claim stops the registry rung.
        // A confirmed row on a dead terminal claims its id against the
        // correlation rung alone.
        assert_eq!(
            decide_bind_for_live_pty(
                &p,
                None,
                1_000,
                None,
                Some("rec-id"),
                &candidates,
                &no_claims(),
                &claimed,
            ),
            registry_bind("rec-id", "C:/cfg", true),
        );
        assert_eq!(
            BindOrigin::ProviderRegistry.as_origin_const(),
            ORIGIN_AUTHORITATIVE
        );
    }

    /// Round-2 review L7: a re-bind that writes nothing new — the row already
    /// open on the pane, under the same origin, at least as confirmed — is
    /// "unchanged", and a pass made only of those (and of refusals) logs at
    /// debug, not info.
    #[test]
    fn a_re_bind_that_writes_nothing_new_is_unchanged() {
        let row = TerminalSessionRecord {
            confirmed_at: None,
            ..title_rec("s", "term-a", None)
        };
        assert!(bind_is_unchanged(
            Some(&row),
            "term-a",
            BindOrigin::ProviderRegistry,
            false
        ));
        assert!(
            !bind_is_unchanged(Some(&row), "term-a", BindOrigin::ProviderRegistry, true),
            "confirming the row changes it"
        );
        assert!(!bind_is_unchanged(
            Some(&row),
            "term-b",
            BindOrigin::ProviderRegistry,
            false
        ));
        assert!(!bind_is_unchanged(
            Some(&row),
            "term-a",
            BindOrigin::Observed,
            false
        ));
        assert!(!bind_is_unchanged(
            None,
            "term-a",
            BindOrigin::ProviderRegistry,
            false
        ));
    }
}
