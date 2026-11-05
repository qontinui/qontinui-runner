//! Rebuild-safe session ledger — **the thing the screenshots are standing in
//! for.**
//!
//! Plan `2026-08-22-wip-custody-rebuild-survivable-attribution`, **Phase 4**.
//!
//! ## The operator's problem, in their words
//!
//! > I occasionally need to rebuild the runner when there are many open
//! > sessions. I take screenshots of the runner's terminal tabs to capture the
//! > names of the sessions … but often don't get around to resuming all of the
//! > sessions that were open before rebuilding. There is probably WIP in many
//! > of these sessions and … I can't identify easily which session the WIP
//! > refers to.
//!
//! ## What was missing, precisely
//!
//! [`crate::session::restore_census`] already computes the right set — the
//! pre-restart `expected` census, latched at boot BEFORE any restore can
//! mutate a record. But it is held in `static BOOT_CENSUS: OnceLock<BootCensus>`
//! with **zero disk persistence anywhere in the module**, so it dies with the
//! process. That makes it structurally unable to answer *"what did the
//! PREVIOUS boot fail to restore"* — which is the only moment the question is
//! ever asked. An absent latch yields `verdict: "unknown"`,
//! `reason: "census_not_latched"`, correctly, and unhelpfully.
//!
//! This module is the disk half: the same set, written to
//! `~/.qontinui/runner[/instance-<name>]/session-ledger.json`, so the NEXT
//! process can read what the LAST one had open.
//!
//! ## Built on the route that is LIVE
//!
//! `/control/sessions/restore-health` answers `200` on the running build today
//! (`{success, data: {sessions: […96], unrestorable: 23}}`) while
//! `/control/sessions/restore-census` 404s — not because it is unbuilt, but
//! because the running build is 93 commits behind the commit that added it.
//! So this phase **extends both** rather than re-implementing either, and takes
//! that **23 unrestorable of 96** as its baseline rather than deriving a new
//! one.
//!
//! ## Why this joins Phases 1–3
//!
//! Each entry carries the session's `worktree_path` and, from that worktree's
//! `$GIT_DIR/qontinui-custody.json`, its `plan_slug` / `work_unit_id` / WIP
//! state. So the ledger does not merely say *"session X did not come back"*,
//! it says *"session X did not come back; it was working on plan P in worktree
//! W, which still holds uncommitted work; here is the line that resumes it."*
//! Unlike a screenshot, that is machine-readable.
//!
//! ## The roster of record (plan `2026-10-04-runner-session-roster-restore-picker`, Phase 2)
//!
//! * **What is captured** is the restore-admissible roster
//!   ([`SessionLifecycleStore::roster_records`]), not merely `open` rows, so a
//!   graceful stop that closes every PTY cannot shrink it; its UNFINISHED part
//!   is the boot-restore set by construction. Finished sessions stay on it,
//!   flagged, and are reported in their own `finished` bucket, never `missing`.
//! * **Generations.** On each boot the previous process's ledger is retired
//!   into `session-ledger.<boot-ms>.json` beside the live file and the newest
//!   [`LEDGER_GENERATIONS_KEPT`] are kept, so a second restart during a rebuild
//!   no longer overwrites the cohort the operator needed. The report diffs each
//!   one on its own.
//! * **One builder** — [`report`] and [`capture_now`] — behind both the HTTP
//!   routes and the Tauri commands `session_ledger_report` /
//!   `session_ledger_capture`.
//!
//! ## Honesty contract (identical in shape to the census's)
//!
//! * **No prior ledger is `unknown`, never `match`.** A boot with nothing on
//!   disk cannot state that nothing was lost.
//! * **A resume line is omitted, never guessed.** Without the account root the
//!   session actually ran under, `claude --resume` fails with a message that
//!   reads exactly like "that session never existed".
//! * **Persistence is fail-soft.** A ledger write that fails logs and moves
//!   on; it must never be the thing that breaks a boot or a poll tick.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::session::claude_process_read::{self as proc_read, PaneRead, ResumeExpectation};
use crate::session::session_lifecycle_store::{
    SessionLifecycleStore, TerminalSessionRecord, NAME_SOURCE_DERIVED, RESTORE_TIER_FAILED,
    RESTORE_TIER_RESUMED, RESTORE_TIER_TERMINAL_ONLY,
};
use crate::session::shutdown_marker::BootClassification;
use crate::session::snapshot_history::{is_restorable_identity, TranscriptProbe};

/// Bumped only on a shape change a reader must branch on. A ledger whose
/// version we do not recognise is IGNORED (treated as absent → `unknown`)
/// rather than partially parsed.
pub const LEDGER_VERSION: u32 = 1;

/// Capture reason: written by the boot latch, from the same set
/// `restore_census::latch_expected` latches.
pub const REASON_BOOT_LATCH: &str = "boot-latch";
/// Capture reason: the periodic liveness poll observed the open set change.
pub const REASON_POLL: &str = "poll";
/// Capture reason: a DELIBERATE pre-rebuild capture, requested over
/// `POST /control/sessions/ledger/capture`.
pub const REASON_PRE_REBUILD: &str = "pre-rebuild";
/// Capture reason: the operator's "Capture now" in the UI (Tauri
/// `session_ledger_capture`).
pub const REASON_OPERATOR: &str = "operator";

/// Outcome classes of one prior session in the report: it came back, it did
/// not (unfinished), it was marked finished and is not expected back, or the
/// operator has since closed it on purpose (its row is closed `user-close`
/// NOW) and it is not expected back either. Between `back` and `missing` sit
/// the four outcomes of a session this boot did something about without a
/// process read proving it back ([`Observed`]).
pub const OUTCOME_BACK: &str = "back";
pub const OUTCOME_ATTEMPTED_UNVERIFIED: &str = "attempted-unverified";
pub const OUTCOME_TERMINAL_ONLY: &str = "terminal-only";
pub const OUTCOME_FAILED: &str = "failed";
pub const OUTCOME_UNKNOWN: &str = "unknown";
pub const OUTCOME_MISSING: &str = "missing";
pub const OUTCOME_FINISHED: &str = "finished";
pub const OUTCOME_CLOSED_BY_USER: &str = "closed-by-user";

/// Verdicts — deliberately the same vocabulary as
/// [`crate::session::restore_census`], so an operator never has to learn two.
pub const VERDICT_MATCH: &str = "match";
pub const VERDICT_PARTIAL: &str = "partial";
pub const VERDICT_MISMATCH: &str = "mismatch";
pub const VERDICT_UNKNOWN: &str = "unknown";

// ---------------------------------------------------------------------------
// The ledger
// ---------------------------------------------------------------------------

/// One session on the roster, as it stood when the ledger was captured.
///
/// The roster is the restore set — exactly the rows the next boot's restore
/// would bring back — plus the FINISHED rows it would skip, included and
/// flagged (see [`SessionLifecycleStore::roster_records`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerEntry {
    pub claude_session_id: String,
    pub terminal_id: String,
    pub page_id: String,
    pub zone_index: i32,
    /// The tab label the operator was screenshotting.
    #[serde(default)]
    pub title: Option<String>,
    /// The in-provider session name (`/rename`).
    #[serde(default)]
    pub session_name: Option<String>,
    /// Provenance of [`Self::session_name`], verbatim from the registry:
    /// `Some("derived")` is Claude Code's own auto-name; anything else,
    /// INCLUDING absent, is operator-chosen. Feeds [`display_name`].
    #[serde(default)]
    pub name_source: Option<String>,
    #[serde(default)]
    pub account_label: Option<String>,
    /// The `CLAUDE_CONFIG_DIR` this session ran under — required to build a
    /// working `--resume` line, and the reason one is omitted when absent.
    #[serde(default)]
    pub config_dir: Option<String>,
    /// The account root a resume runs under, RESOLVED at capture by
    /// [`resolve_config_dir`]: the recorded [`Self::config_dir`], else the one
    /// config dir holding the session's transcript. `None` = the account is
    /// UNKNOWN (or the ledger predates the field) — never a default.
    #[serde(default)]
    pub resume_config_dir: Option<String>,
    /// The shared copy-able resume line ([`resume_command_for`]) as of
    /// capture, under [`Self::resume_config_dir`]. `None` when the account or
    /// the working dir is unknown. What `/copy-names` emits for the current
    /// roster.
    #[serde(default)]
    pub resume_command: Option<String>,
    /// The AI-CLI provider (`"claude"`, `"gemini"`, …). `None` only in a
    /// ledger written before the field existed.
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub working_dir: Option<String>,
    /// The registry's `last_seen_at` as of the last WRITE of this ledger.
    /// Deliberately NOT part of the change key (it moves every poll tick), so
    /// it can trail the true last-seen instant by up to one quiet stretch.
    #[serde(default)]
    pub last_seen_at: Option<i64>,
    /// The session was marked FINISHED (the work axis) at capture. Kept in the
    /// ledger rather than dropped, so a mistaken Finish stays undoable from
    /// the roster; never reported as `missing`.
    #[serde(default)]
    pub finished: bool,
    /// `confirmed && transcriptExists` AT CAPTURE TIME — the same join
    /// `/control/sessions/restore-health` reports.
    ///
    /// **A TRI-STATE, deliberately.** `Some(false)` says the conversation was
    /// never resumable, so its later absence is not a restore defect —
    /// a real claim the report renders as *"restore could not have brought its
    /// conversation back"*. But the underlying `TranscriptProbe` returns a bare
    /// `bool` that **cannot express "could not determine"** (its own docs say
    /// so, and the disk impl returns `false` for a missing or blank
    /// `working_dir`). Collapsing that into `Some(false)` would tell the
    /// operator not to bother resuming a session that has real WIP. So an
    /// unprobeable record is `None`, and the report gives it its own reason.
    #[serde(default)]
    pub restorable: Option<bool>,

    // --- the Phase 1-3 join -------------------------------------------------
    /// The git worktree root containing [`Self::working_dir`], when it is
    /// inside one. This is what makes an entry actionable: it names the tree
    /// the WIP is in.
    #[serde(default)]
    pub worktree_path: Option<String>,
    /// From that worktree's `$GIT_DIR/qontinui-custody.json`.
    #[serde(default)]
    pub plan_slug: Option<String>,
    #[serde(default)]
    pub work_unit_id: Option<String>,
    /// Verbatim custody `wip_state`. `captured` is the ONLY value meaning the
    /// uncommitted work is snapshotted to `refs/wip/<id>`.
    #[serde(default)]
    pub wip_state: Option<String>,
    #[serde(default)]
    pub wip_ref: Option<String>,
    /// The custody record in that worktree names a DIFFERENT session than this
    /// one. Surfaced rather than silently preferred either way: it usually
    /// means two sessions shared a worktree, which is exactly the ambiguity
    /// the operator needs to see.
    #[serde(default)]
    pub custody_session_mismatch: bool,
}

/// A captured ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionLedger {
    pub ledger_version: u32,
    /// Unix millis of capture.
    pub captured_at_ms: i64,
    pub captured_at: String,
    /// [`REASON_BOOT_LATCH`] | [`REASON_POLL`] | [`REASON_PRE_REBUILD`] |
    /// [`REASON_OPERATOR`] | a caller-supplied label.
    pub reason: String,
    /// Unix millis at which the runner process that captured this ledger
    /// booted ([`process_boot_ms`]) — the generation's boot time. `None` only
    /// in a ledger written before the field existed.
    #[serde(default)]
    pub boot_at_ms: Option<i64>,
    /// `at` of the prior shutdown marker as this process classified it.
    #[serde(default)]
    pub shutdown_at: Option<i64>,
    /// `true` iff the previous shutdown was clean; `null` when this process
    /// never classified its boot — UNKNOWN, not `false`.
    #[serde(default)]
    pub clean_shutdown: Option<bool>,
    pub sessions: Vec<LedgerEntry>,
}

impl SessionLedger {
    /// The change key. Content-derived over the fields that make an entry
    /// actionable, so a rewrite happens **on change** and a quiet poll tick
    /// costs one comparison rather than a disk write.
    ///
    /// `finished` is in it so a Finish/Unfinish reaches disk on the next tick,
    /// and so are the page/zone a restore lands on and the custody `plan_slug`
    /// / `wip_state` the picker shows; `last_seen_at` is NOT, because it moves
    /// on every tick.
    fn fingerprint(&self) -> String {
        let opt = |s: &Option<String>| s.as_deref().unwrap_or("").to_string();
        let mut parts: Vec<String> = self
            .sessions
            .iter()
            .map(|s| {
                [
                    s.claude_session_id.clone(),
                    s.terminal_id.clone(),
                    s.page_id.clone(),
                    s.zone_index.to_string(),
                    opt(&s.working_dir),
                    opt(&s.title),
                    opt(&s.session_name),
                    opt(&s.name_source),
                    opt(&s.account_label),
                    opt(&s.config_dir),
                    opt(&s.resume_config_dir),
                    opt(&s.provider),
                    opt(&s.worktree_path),
                    opt(&s.plan_slug),
                    opt(&s.wip_state),
                    match s.restorable {
                        Some(true) => "y",
                        Some(false) => "n",
                        None => "?",
                    }
                    .to_string(),
                    if s.finished { "F" } else { "-" }.to_string(),
                ]
                .join("|")
            })
            .collect();
        parts.sort();
        parts.join("\n")
    }
}

/// The name an operator recognises a session by: the `/rename` name when it is
/// operator-chosen, Claude Code's auto-name only as a fallback behind the tab
/// title. `session_name` wins unless `name_source == "derived"`; then `title`.
/// Whichever is preferred falls back to the other, and with neither (or only
/// blanks) to `claude <id8>` — the runner's own label for a nameless session.
///
/// This is THE display-name rule (plan
/// `2026-10-04-runner-session-roster-restore-picker`); the frontend mirrors it
/// in `src/lib/session-ledger.ts` `displayNameOf`.
pub fn display_name(
    session_name: Option<&str>,
    name_source: Option<&str>,
    title: Option<&str>,
    claude_session_id: &str,
) -> String {
    fn nonblank(s: Option<&str>) -> Option<&str> {
        s.map(str::trim).filter(|s| !s.is_empty())
    }
    let name = nonblank(session_name);
    let title = nonblank(title);
    let preferred = if name_source == Some(NAME_SOURCE_DERIVED) {
        title.or(name)
    } else {
        name.or(title)
    };
    match preferred {
        Some(p) => p.to_string(),
        None => format!(
            "claude {}",
            claude_session_id.chars().take(8).collect::<String>()
        ),
    }
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

fn runner_dir() -> PathBuf {
    qontinui_runner_lib::ambient::runner_dir_or_cwd()
}

/// Instance-scoped ledger path, a sibling of the lifecycle store.
///
/// Scoping matters and is not incidental: a temp runner spawned to verify a
/// branch must NOT read or clobber the primary's ledger. That is the
/// 2026-08-10 regression `restore_census`'s module docs warn about, and it
/// applies identically here.
pub fn ledger_path() -> PathBuf {
    crate::instance::scope_path(&runner_dir()).join("session-ledger.json")
}

/// Unix millis at which THIS runner process booted, latched on first call —
/// which is [`load_prior_once`] at boot. Stamped on every ledger this process
/// writes ([`SessionLedger::boot_at_ms`]) and used as the rotation instant of
/// the generation it retires.
pub fn process_boot_ms() -> i64 {
    static BOOT_MS: OnceLock<i64> = OnceLock::new();
    *BOOT_MS.get_or_init(|| chrono::Utc::now().timestamp_millis())
}

// ---------------------------------------------------------------------------
// Capture
// ---------------------------------------------------------------------------

/// Walk up from `dir` looking for a `.git` entry — the worktree root.
///
/// Mirrors the shipped Phase-1 hook's `find_toplevel`, including its stated
/// coverage gap: a session whose cwd is not inside a git worktree yields
/// `None`, and nothing here guesses at a repo it was not standing in.
pub fn worktree_root_of(dir: &str) -> Option<PathBuf> {
    let mut p = PathBuf::from(dir.replace('\\', "/"));
    loop {
        if p.join(".git").exists() {
            return Some(p);
        }
        if !p.pop() {
            return None;
        }
        if p.as_os_str().is_empty() {
            return None;
        }
    }
}

/// Build the ledger from the CURRENT session roster —
/// [`SessionLifecycleStore::roster_records`], i.e. the restore set (same
/// admission against the same anchors, same terminal dedupe — so a graceful
/// stop that closes every PTY `pty-exit` cannot shrink the roster, and the
/// "will come back" count matches the next boot's restore census) plus the
/// finished rows, included and flagged.
///
/// `probe` supplies the transcript half of `restorable` — the same join
/// `/control/sessions/restore-health` performs, so the two surfaces cannot
/// disagree about whether a session was resumable.
pub fn capture(
    store: &SessionLifecycleStore,
    probe: &dyn TranscriptProbe,
    reason: &str,
    now_ms: i64,
    boot: Option<BootClassification>,
) -> SessionLedger {
    // Exactly the two arguments `terminal_session_list_open` passes to
    // `restorable_records`, so the roster's unfinished part IS the restore set.
    let prior_marker_at = boot.and_then(|b| b.prior_marker_at);
    let boot_was_clean = boot.map(|b| !b.crash_recovery).unwrap_or(false);
    let mut sessions = store
        .roster_records(prior_marker_at, boot_was_clean)
        .into_iter()
        .map(|rec| {
            let confirmed = rec.confirmed_at.is_some();
            // Guarded exactly like `SessionLifecycleStore::probe_transcript_exists`:
            // a missing or BLANK `working_dir` makes the disk probe answer a
            // question it was never asked, and its `false` means "I could not
            // look", not "there is no transcript".
            let restorable = rec
                .working_dir
                .as_deref()
                .map(str::trim)
                .filter(|w| !w.is_empty())
                .map(|w| {
                    is_restorable_identity(
                        confirmed,
                        probe.transcript_exists(&rec.claude_session_id, Some(w)),
                    )
                });

            // The Phase 1-3 join: which worktree, and what does its custody
            // record say about the work in it.
            let worktree = rec.working_dir.as_deref().and_then(worktree_root_of);
            let custody = worktree
                .as_deref()
                .and_then(crate::agent_worktree::custody::read_custody);
            let custody_session_mismatch = custody
                .as_ref()
                .and_then(|c| c.session_id.as_deref())
                .is_some_and(|id| !id.eq_ignore_ascii_case(&rec.claude_session_id));

            // The account a resume runs under: the recorded dir, else the one
            // config dir holding the transcript. The transcript scan runs only
            // for a record that carries no dir of its own.
            let has_recorded = rec
                .config_dir
                .as_deref()
                .is_some_and(|d| !d.trim().is_empty());
            let holders = if has_recorded {
                None
            } else {
                probe.transcript_config_dirs(&rec.claude_session_id, rec.working_dir.as_deref())
            };
            let resume_config_dir =
                resolve_config_dir(rec.config_dir.as_deref(), holders.as_deref());
            let resume_command = resume_command_for(
                resume_dir_of(
                    rec.working_dir.as_deref(),
                    worktree.as_deref().and_then(|p| p.to_str()),
                ),
                resume_config_dir.as_deref(),
                &rec.claude_session_id,
            );

            LedgerEntry {
                claude_session_id: rec.claude_session_id,
                terminal_id: rec.terminal_id,
                page_id: rec.page_id,
                zone_index: rec.zone_index,
                title: rec.title,
                session_name: rec.session_name,
                name_source: rec.name_source,
                account_label: rec.account_label,
                config_dir: rec.config_dir,
                resume_config_dir,
                resume_command,
                provider: Some(rec.provider),
                working_dir: rec.working_dir,
                last_seen_at: Some(rec.last_seen_at),
                finished: rec.finished_at.is_some(),
                restorable,
                worktree_path: worktree.map(|p| p.to_string_lossy().replace('\\', "/")),
                plan_slug: custody.as_ref().and_then(|c| c.plan_slug.clone()),
                work_unit_id: custody.as_ref().and_then(|c| c.work_unit_id.clone()),
                wip_state: custody.as_ref().and_then(|c| c.wip_state.clone()),
                wip_ref: custody.as_ref().and_then(|c| c.wip_ref.clone()),
                custody_session_mismatch,
            }
        })
        .collect::<Vec<_>>();
    sessions.sort_by(|a, b| a.claude_session_id.cmp(&b.claude_session_id));

    SessionLedger {
        ledger_version: LEDGER_VERSION,
        captured_at_ms: now_ms,
        captured_at: chrono::DateTime::from_timestamp_millis(now_ms)
            .map(|t| t.to_rfc3339())
            .unwrap_or_default(),
        reason: reason.to_string(),
        boot_at_ms: Some(process_boot_ms()),
        shutdown_at: prior_marker_at,
        clean_shutdown: boot.map(|b| !b.crash_recovery),
        sessions,
    }
}

// ---------------------------------------------------------------------------
// Persistence — on change, atomically, fail-soft
// ---------------------------------------------------------------------------

/// Fingerprint of the last ledger THIS process wrote. Purely an optimisation:
/// a miss costs one extra write, never a wrong answer.
fn last_written() -> &'static Mutex<Option<String>> {
    static LAST: OnceLock<Mutex<Option<String>>> = OnceLock::new();
    LAST.get_or_init(|| Mutex::new(None))
}

/// Write the ledger to disk **only when its content changed**.
///
/// Returns `true` when a write landed. [`crate::fs_atomic::atomic_write`]
/// (temp file + rename), so a process killed mid-write cannot leave a torn
/// ledger — the whole point of a record that has to survive the kill a rebuild
/// performs.
///
/// **Never fatal.** Every failure path logs and returns `false`; a ledger is a
/// diagnostic, and a diagnostic must never be the thing that breaks a boot.
pub fn persist_if_changed(ledger: &SessionLedger) -> bool {
    let fp = ledger.fingerprint();
    // The guard is held across the WHOLE write, not just the comparison.
    //
    // Several writers exist in one process — the boot latch, the 45 s
    // liveness poll, `POST /control/sessions/ledger/capture` and the Tauri
    // `session_ledger_capture` — and they can overlap. Dropping the lock after
    // the check made this check-then-act. A poisoned lock skips the write
    // rather than racing.
    let Ok(mut guard) = last_written().lock() else {
        warn!("session_ledger: write lock poisoned — skipping this write");
        return false;
    };
    if guard.as_deref() == Some(fp.as_str()) {
        debug!("session_ledger: unchanged — no write");
        return false;
    }

    let path = ledger_path();
    if !write_ledger(&path, ledger) {
        return false;
    }
    if !ledger.sessions.is_empty() {
        WROTE_NON_EMPTY.store(true, Ordering::Relaxed);
    }
    *guard = Some(fp);
    info!(
        sessions = ledger.sessions.len(),
        reason = %ledger.reason,
        path = %path.display(),
        "session_ledger: persisted the session-roster ledger"
    );
    true
}

/// Serialize `ledger` to `path` atomically, creating the parent dir. Logs and
/// returns `false` on any failure.
fn write_ledger(path: &Path, ledger: &SessionLedger) -> bool {
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            warn!(error = %e, path = %parent.display(), "session_ledger: mkdir failed");
            return false;
        }
    }
    let Ok(json) = serde_json::to_vec_pretty(ledger) else {
        warn!("session_ledger: serialize failed");
        return false;
    };
    if let Err(e) = crate::fs_atomic::atomic_write(path, &json) {
        warn!(error = %e, path = %path.display(), "session_ledger: atomic write failed");
        return false;
    }
    true
}

/// THIS process has written a non-empty ledger — from then on the live file is
/// this process's own roster, and an empty capture is a real answer.
static WROTE_NON_EMPTY: AtomicBool = AtomicBool::new(false);

/// The one write that can DESTROY evidence: an EMPTY capture over a NON-EMPTY
/// prior, before this process has written a roster of its own.
///
/// The boot path reads the prior ledger and then writes this process's own
/// capture over the same file. If the lifecycle registry happens to be empty at
/// that instant — a fresh instance, a lost or reset registry, a store that
/// failed to open — an empty `sessions: []` lands on disk and the only record
/// of what the LAST boot had open is gone. The next boot then reads that empty
/// ledger and reports `verdict: "match"`, a fabricated positive on a file this
/// code wrote itself. Exactly the vacuous-`match` the census's R3 forbids.
///
/// The guard lapses once `wrote_non_empty` — once THIS process has written a
/// non-empty ledger, the registry demonstrably was read, and the previous
/// boot's roster is already retained as a generation. An empty capture after
/// that is the operator having closed (or finished) every session, and it must
/// be written: refusing it kept the last non-empty roster on disk, so the next
/// boot offered back every tab the operator had deliberately closed.
fn refuses_overwrite(
    capture: &SessionLedger,
    prior: Option<&SessionLedger>,
    wrote_non_empty: bool,
) -> bool {
    !wrote_non_empty && capture.sessions.is_empty() && prior.is_some_and(|p| !p.sessions.is_empty())
}

/// [`persist_if_changed`], refusing the one write [`refuses_overwrite`] names.
/// The refusal is logged, not silent, because "the registry read empty at
/// boot" is itself worth seeing.
pub fn persist_capture(ledger: &SessionLedger) -> bool {
    let prior = prior();
    if refuses_overwrite(ledger, prior, WROTE_NON_EMPTY.load(Ordering::Relaxed)) {
        warn!(
            prior_sessions = prior.map(|p| p.sessions.len()).unwrap_or(0),
            prior_captured_at = prior.map(|p| p.captured_at.as_str()).unwrap_or(""),
            reason = %ledger.reason,
            "session_ledger: REFUSING to overwrite a non-empty prior ledger with an \
             empty capture — the registry read empty, which is not proof the previous \
             boot had nothing open"
        );
        return false;
    }
    persist_if_changed(ledger)
}

/// Read a ledger from `path`. `None` on any failure AND on an unrecognised
/// `ledgerVersion` — a shape we cannot read is ABSENT, never half-parsed.
pub fn load_from(path: &Path) -> Option<SessionLedger> {
    let raw = std::fs::read_to_string(path).ok()?;
    let led: SessionLedger = serde_json::from_str(&raw).ok()?;
    (led.ledger_version == LEDGER_VERSION).then_some(led)
}

// ---------------------------------------------------------------------------
// Generations — the last LEDGER_GENERATIONS_KEPT boots, each kept whole
// ---------------------------------------------------------------------------

/// How many retired generations are kept beside the live file.
///
/// Five survives a crash-loop or a re-build cycle (2-4 restarts per rebuild
/// session is the measured norm) at a few KB per generation; a single
/// generation let the second restart of a rebuild overwrite the cohort the
/// operator actually needed.
pub const LEDGER_GENERATIONS_KEPT: usize = 5;

/// One retired ledger on disk: `session-ledger.<rotated_at_ms>.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredGeneration {
    pub path: PathBuf,
    /// Unix millis of the boot that retired it — i.e. when the restart that
    /// ended that generation happened.
    pub rotated_at_ms: i64,
    pub ledger: SessionLedger,
}

fn generation_file_name(rotated_at_ms: i64) -> String {
    format!("session-ledger.{rotated_at_ms}.json")
}

/// `session-ledger.<digits>.json` → `<digits>`. Anything else (the live file,
/// an atomic-write temp, a stray) is not a generation.
fn parse_generation_file_name(name: &str) -> Option<i64> {
    let ts = name
        .strip_prefix("session-ledger.")?
        .strip_suffix(".json")?;
    if ts.is_empty() || !ts.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    ts.parse().ok()
}

/// Every generation FILE in `dir` (readable or not), newest first.
fn generation_files_in(dir: &Path) -> Vec<(i64, PathBuf)> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<(i64, PathBuf)> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name();
            let ts = parse_generation_file_name(name.to_str()?)?;
            Some((ts, e.path()))
        })
        .collect();
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
    files
}

/// Every READABLE generation in `dir`, newest first. A generation file we
/// cannot parse (or of an unknown version) is skipped — absent, never
/// half-parsed, exactly like [`load_from`].
fn generations_in(dir: &Path) -> Vec<StoredGeneration> {
    generation_files_in(dir)
        .into_iter()
        .filter_map(|(rotated_at_ms, path)| {
            let ledger = load_from(&path)?;
            Some(StoredGeneration {
                path,
                rotated_at_ms,
                ledger,
            })
        })
        .collect()
}

/// The retained generations beside the live ledger, newest STAMP first (see
/// [`generation_reports`] for the order the report serves).
pub fn generations() -> Vec<StoredGeneration> {
    let live = ledger_path();
    match live.parent() {
        Some(dir) => generations_in(dir),
        None => Vec::new(),
    }
}

/// The generation file holding the ledger THIS process latched as its prior —
/// the one [`rotate_in`] wrote at boot, or the identical one it found already
/// retained. `None` before [`load_prior_once`] or when nothing was retained.
static THIS_BOOT_GENERATION: OnceLock<Option<PathBuf>> = OnceLock::new();

fn this_boot_generation() -> Option<&'static Path> {
    THIS_BOOT_GENERATION.get().and_then(|o| o.as_deref())
}

/// Retire the live ledger at `live` into a generation file stamped
/// `rotated_at_ms`, then prune to the newest `keep`.
///
/// A COPY, not a move: the live file stays in place, so the empty-capture
/// refusal still protects it and a boot that writes nothing leaves the record
/// where every reader looks. A live ledger byte-for-byte equal (as parsed) to
/// the newest generation is NOT rotated again — a run of boots whose captures
/// were all refused would otherwise fill every slot with one cohort and push
/// the older, different ones out.
///
/// Returns the generation that now holds the live ledger — the path written,
/// or the identical newest generation it declined to duplicate — and `None`
/// when the live ledger is retained nowhere (absent, unreadable, or the write
/// failed). Fail-soft.
fn rotate_in(live: &Path, rotated_at_ms: i64, keep: usize) -> Option<PathBuf> {
    let dir = live.parent()?;
    let raw = std::fs::read(live).ok()?;
    let Some(ledger) = serde_json::from_slice::<SessionLedger>(&raw)
        .ok()
        .filter(|l| l.ledger_version == LEDGER_VERSION)
    else {
        warn!(
            path = %live.display(),
            "session_ledger: live ledger unreadable — not rotated into a generation"
        );
        return None;
    };
    if let Some(newest) = generations_in(dir)
        .into_iter()
        .next()
        .filter(|newest| newest.ledger == ledger)
    {
        debug!("session_ledger: live ledger already retained as the newest generation");
        return Some(newest.path);
    }
    let target = dir.join(generation_file_name(rotated_at_ms));
    if let Err(e) = crate::fs_atomic::atomic_write(&target, &raw) {
        warn!(error = %e, path = %target.display(), "session_ledger: generation write failed");
        return None;
    }
    // Never the generation just written: with the clock stepped backwards its
    // stamp sorts OLDEST, and pruning it would lose the cohort this very
    // rotation exists to keep.
    for (_, stale) in generation_files_in(dir)
        .into_iter()
        .filter(|(_, p)| p != &target)
        .skip(keep.saturating_sub(1))
    {
        if let Err(e) = std::fs::remove_file(&stale) {
            warn!(error = %e, path = %stale.display(), "session_ledger: generation prune failed");
        }
    }
    info!(
        sessions = ledger.sessions.len(),
        path = %target.display(),
        "session_ledger: retired the prior boot's ledger into a generation"
    );
    Some(target)
}

/// The PREVIOUS process's ledger, latched once.
///
/// **Ordering is load-bearing.** This must be called at boot BEFORE the first
/// [`persist_if_changed`], or this process's own capture overwrites the very
/// file the report needs to read. [`load_prior_once`] is what enforces that,
/// and it is idempotent so a second call cannot re-latch a post-capture read.
static PRIOR: OnceLock<Option<SessionLedger>> = OnceLock::new();

/// Latch the prior boot's ledger off disk AND retire it into a generation
/// (once per process, before anything in this process writes a ledger). Call
/// from `main.rs` setup, before anything writes one.
pub fn load_prior_once() -> Option<&'static SessionLedger> {
    let slot = PRIOR.get_or_init(|| {
        let boot_ms = process_boot_ms();
        let path = ledger_path();
        let led = load_from(&path);
        match &led {
            Some(l) => info!(
                sessions = l.sessions.len(),
                captured_at = %l.captured_at,
                reason = %l.reason,
                "session_ledger: loaded the PRIOR boot's session-roster ledger"
            ),
            None => info!(
                path = %path.display(),
                "session_ledger: no prior ledger on disk — the post-rebuild report will \
                 read UNKNOWN, never 'nothing was lost'"
            ),
        }
        let retained = rotate_in(&path, boot_ms, LEDGER_GENERATIONS_KEPT);
        let _ = THIS_BOOT_GENERATION.set(retained);
        led
    });
    slot.as_ref()
}

/// The latched prior ledger, without loading. `None` here is ambiguous between
/// "never latched" and "none on disk", which is why the report calls
/// [`load_prior_once`] instead.
pub fn prior() -> Option<&'static SessionLedger> {
    PRIOR.get().and_then(|o| o.as_ref())
}

// ---------------------------------------------------------------------------
// The post-rebuild report
// ---------------------------------------------------------------------------

/// One prior session and what became of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerOutcome {
    pub claude_session_id: String,
    pub terminal_id: String,
    pub page_id: String,
    pub zone_index: i32,
    /// [`display_name`] over this entry — the name to show the operator.
    pub display_name: String,
    pub session_name: Option<String>,
    pub name_source: Option<String>,
    pub title: Option<String>,
    pub account_label: Option<String>,
    pub config_dir: Option<String>,
    /// `config_dir` is present and non-blank. `false` means a resume would run
    /// under the DEFAULT account unless the account is resolved first.
    pub config_dir_known: bool,
    pub provider: Option<String>,
    pub working_dir: Option<String>,
    pub worktree_path: Option<String>,
    pub plan_slug: Option<String>,
    pub work_unit_id: Option<String>,
    pub wip_state: Option<String>,
    pub wip_ref: Option<String>,
    pub custody_session_mismatch: bool,
    /// Unix millis — see [`LedgerEntry::last_seen_at`].
    pub last_seen_at: Option<i64>,
    /// Was this session resumable AT CAPTURE TIME? `None` = could not be
    /// determined, never collapsed into `false`.
    pub restorable: Option<bool>,
    /// FINISHED as of NOW when the registry still holds the row (so a
    /// Finish/Unfinish shows on every generation at once), else as captured.
    pub finished: bool,
    /// [`OUTCOME_BACK`] | [`OUTCOME_ATTEMPTED_UNVERIFIED`] |
    /// [`OUTCOME_TERMINAL_ONLY`] | [`OUTCOME_FAILED`] | [`OUTCOME_UNKNOWN`] |
    /// [`OUTCOME_MISSING`] | [`OUTCOME_FINISHED`] | [`OUTCOME_CLOSED_BY_USER`].
    pub outcome: String,
    /// For a missing session: WHY it is missing.
    /// `not-restorable` — it was never identity-restorable, so restore could
    /// not have brought its conversation back;
    /// `restorability-unknown` — we could not tell (no working dir to probe
    /// against), so its absence is NOT evidence either way;
    /// `no-attempt` — nothing came back and nothing tried.
    pub reason: Option<String>,
    /// The directory a resume opens in ([`resume_dir_of`]) — the same one the
    /// copy line `cd`s into and the one-click resume launches in.
    pub resume_dir: Option<String>,
    /// `cd "<dir>" && CLAUDE_CONFIG_DIR="<root>" claude --resume <id>`.
    /// `None` — NEVER a guess — when the account root is unknown.
    pub resume_command: Option<String>,
    /// The account a TYPED resume runs under — what the UI's one-click resume
    /// sets as `CLAUDE_CONFIG_DIR` (see [`ResumeAccount`]).
    pub resume_account: ResumeAccount,
}

/// The account a typed `--resume` runs under, shared by every surface that
/// resumes a session in a new tab (the "Since restart" picker and Past
/// Sessions):
///
/// - `known: true, config_dir: Some(d)` — type `CLAUDE_CONFIG_DIR=d`;
/// - `known: true, config_dir: None` — the DEFAULT home (`~/.claude`): type no
///   `CLAUDE_CONFIG_DIR`, because setting it would swap `~/.claude.json` for
///   `~/.claude/.claude.json` (see `discovery::is_default_config_home`);
/// - `known: false` — the account is UNKNOWN: the operator must choose one.
///   Resuming under the default instead fails as "No conversation found",
///   which reads exactly like a session that never existed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumeAccount {
    pub known: bool,
    pub config_dir: Option<String>,
}

/// [`ResumeAccount`] for a RESOLVED account root ([`resolve_config_dir`]).
pub fn resume_account_for(resolved: Option<&str>) -> ResumeAccount {
    match resolved.map(str::trim).filter(|d| !d.is_empty()) {
        None => ResumeAccount {
            known: false,
            config_dir: None,
        },
        Some(dir)
            if qontinui_runner_lib::session_archive::discovery::is_default_config_home_from_env(
                dir,
            ) =>
        {
            ResumeAccount {
                known: true,
                config_dir: None,
            }
        }
        Some(dir) => ResumeAccount {
            known: true,
            config_dir: Some(dir.replace('\\', "/")),
        },
    }
}

/// A retained prior generation, diffed on its own against what is back now.
///
/// Per-generation, never a union: the ledger records no user-close, so a union
/// would resurrect sessions the operator closed in a later generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerGeneration {
    /// File name inside the ledger directory.
    pub file: String,
    /// Unix millis of the boot that retired this generation (the restart that
    /// ended it).
    pub rotated_at_ms: i64,
    /// Unix millis at which the process that wrote this generation booted.
    /// `None` for a generation written before the field existed.
    pub boot_at_ms: Option<i64>,
    /// This is the generation THIS boot retired (the roster that was up when
    /// this runner started). Always the first entry of
    /// [`LedgerReport::generations`] when set; at most one carries it.
    pub this_boot: bool,
    pub captured_at_ms: i64,
    pub captured_at: String,
    pub reason: String,
    pub clean_shutdown: Option<bool>,
    pub session_count: usize,
    pub verdict: String,
    pub returned: Vec<LedgerOutcome>,
    pub attempted_unverified: Vec<LedgerOutcome>,
    pub terminal_only: Vec<LedgerOutcome>,
    pub failed: Vec<LedgerOutcome>,
    pub unknown: Vec<LedgerOutcome>,
    pub missing: Vec<LedgerOutcome>,
    pub finished: Vec<LedgerOutcome>,
    pub closed_by_user: Vec<LedgerOutcome>,
}

/// `GET /control/sessions/ledger` and the Tauri `session_ledger_report`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerReport {
    /// `ok` | `unavailable`.
    pub status: String,
    /// REQUIRED whenever `status != "ok"` or the verdict is `unknown`.
    pub reason: Option<String>,
    pub generated_at: i64,
    /// Metadata of the ledger being compared against.
    pub prior_captured_at: Option<String>,
    pub prior_reason: Option<String>,
    /// The prior boot's roster — the N in "rebuild with N sessions open".
    pub expected: Vec<LedgerEntry>,
    /// Of those, the ones VERIFIED back (D2): restored with tier `resumed`
    /// this boot, or `open` on a live pane whose process read shows the
    /// session's `claude` running.
    pub returned: Vec<LedgerOutcome>,
    /// A resume was attempted this boot and no process read has verified it.
    pub attempted_unverified: Vec<LedgerOutcome>,
    /// Bound to a live terminal (or restored as one) in which no `claude` runs.
    pub terminal_only: Vec<LedgerOutcome>,
    /// A resume was attempted this boot and ended without landing.
    pub failed: Vec<LedgerOutcome>,
    /// Its pane's process read was unavailable, so it could not be judged —
    /// never counted back, and the verdict reads `unknown`.
    pub unknown: Vec<LedgerOutcome>,
    /// Of those, the UNFINISHED ones that did NOT come back — each with a
    /// resume line.
    pub missing: Vec<LedgerOutcome>,
    /// Of those, the FINISHED ones that did not come back — not expected back,
    /// never counted as missing.
    pub finished: Vec<LedgerOutcome>,
    /// Of those, the unfinished ones the operator has since CLOSED on purpose
    /// (their row is closed `user-close` now) — not expected back, never
    /// counted as missing.
    pub closed_by_user: Vec<LedgerOutcome>,
    /// [`VERDICT_MATCH`] | [`VERDICT_PARTIAL`] | [`VERDICT_MISMATCH`] |
    /// [`VERDICT_UNKNOWN`].
    pub verdict: String,
    /// What is on the roster right now — so the report is self-contained even
    /// when there is no prior ledger to compare against.
    pub current: SessionLedger,
    /// Every retained prior generation — the one THIS boot retired first
    /// (flagged [`LedgerGeneration::this_boot`]), the rest newest first — each
    /// with its boot time and its own diff.
    pub generations: Vec<LedgerGeneration>,
    /// Unix millis at which THIS process last wrote the live ledger file — the
    /// roster the NEXT boot will report against. `None` when this process has
    /// written none yet (the file on disk, if any, is the previous boot's).
    /// The file is rewritten only when the roster CHANGES, so an old instant
    /// with [`Self::saved_matches_current`] set is current, not stale.
    pub saved_at_ms: Option<i64>,
    /// The saved roster equals the current one (same change key — names,
    /// accounts, dirs, finished flags). `false` when nothing is saved yet.
    pub saved_matches_current: bool,
    /// Always present, and never implies an empty answer.
    pub note: String,
}

/// Resolve the `CLAUDE_CONFIG_DIR` a session resumes under — THE config-dir
/// resolution every resume surface shares (plan
/// `2026-10-04-runner-session-roster-restore-picker`, Phase 3):
///
/// 1. the RECORDED dir, when the record carries a non-blank one;
/// 2. else the ONE config dir whose transcript for the session exists
///    (`transcript_holders`) — evidence, not a guess;
/// 3. else `None`: the account is UNKNOWN. Zero holders, several holders, or
///    no evidence at all (`None`) never resolve to a default.
///
/// The boot-restore classifier applies the same order on the frontend
/// (`resolveRestoreAccount` in `useTerminalInitialization.ts`), fed by the
/// same transcript probe.
pub fn resolve_config_dir(
    recorded: Option<&str>,
    transcript_holders: Option<&[PathBuf]>,
) -> Option<String> {
    if let Some(dir) = recorded.map(str::trim).filter(|d| !d.is_empty()) {
        return Some(dir.to_string());
    }
    match transcript_holders? {
        [only] => only.to_str().map(str::to_string),
        _ => None,
    }
}

/// THE copy-able resume line: `cd "<dir>" && CLAUDE_CONFIG_DIR="<config>"
/// claude --resume <id>` — shared by the ledger report, Past Sessions and the
/// live-session listing (`/copy-names`), so no surface hand-builds its own.
///
/// `config_dir` is the RESOLVED account ([`resolve_config_dir`]); `None` (an
/// unknown account) yields NO line, because a `--resume` under the wrong
/// account fails with "No conversation found" — indistinguishable from a
/// session that never existed. The one account resumed WITHOUT the variable is
/// the default home (`~/.claude`), whose global config lives outside it (see
/// `discovery::is_default_config_home`). The account's CLI wrapper
/// (`clg`, `clp`, …) is never used: it covers a fixed handful of accounts, and
/// any other account would silently resume under the default one.
///
/// Every input reaches a line the operator COPY-PASTES INTO A SHELL, and the
/// paths come off registry files. A token that cannot be rendered safely
/// yields no line. Backslashes are normalized to `/` first (both shells this
/// lands in accept forward slashes; a `\` cannot be made safe inside the
/// double quotes). The `cd` is required: Claude Code scopes sessions by
/// project directory, so a resume from the wrong cwd finds nothing.
pub fn resume_command_for(
    working_dir: Option<&str>,
    config_dir: Option<&str>,
    session_id: &str,
) -> Option<String> {
    use crate::agent_worktree::custody::{is_shell_safe_token, shell_quote_path};
    let slashed = |p: &str| p.trim().replace('\\', "/");
    let dir = shell_quote_path(&slashed(working_dir?))?;
    let config = shell_quote_path(&slashed(config_dir?))?;
    if !is_shell_safe_token(session_id) {
        return None;
    }
    if qontinui_runner_lib::session_archive::discovery::is_default_config_home_from_env(&config) {
        return Some(format!("cd \"{dir}\" && claude --resume {session_id}"));
    }
    Some(format!(
        "cd \"{dir}\" && CLAUDE_CONFIG_DIR=\"{config}\" claude --resume {session_id}"
    ))
}

/// A ledger entry's resolved account root: the recorded `config_dir`, else the
/// transcript holder resolved at capture ([`LedgerEntry::resume_config_dir`]).
/// An entry written before that field existed, with no recorded dir, resolves
/// to an unknown account.
fn ledger_config_dir(entry: &LedgerEntry) -> Option<String> {
    resolve_config_dir(entry.config_dir.as_deref(), None).or_else(|| {
        entry
            .resume_config_dir
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty())
            .map(str::to_string)
    })
}

/// THE directory a resume runs in — shared by the copy line, the ledger's
/// one-click resume and (through [`LedgerOutcome::resume_dir`]) the picker: the
/// session's recorded `working_dir`, EXACTLY, else the worktree root holding it.
///
/// The launch dir comes first because Claude Code scopes sessions by the exact
/// directory it was started in; a resume from the worktree root of a session
/// launched in a subdirectory finds no conversation.
pub fn resume_dir_of<'a>(
    working_dir: Option<&'a str>,
    worktree: Option<&'a str>,
) -> Option<&'a str> {
    let nonblank = |d: Option<&'a str>| d.filter(|d| !d.trim().is_empty());
    nonblank(working_dir).or(nonblank(worktree))
}

/// A ledger entry's resume line: [`resume_dir_of`], under the resolved
/// account ([`ledger_config_dir`]).
fn ledger_resume_command(entry: &LedgerEntry) -> Option<String> {
    resume_command_for(
        resume_dir_of(entry.working_dir.as_deref(), entry.worktree_path.as_deref()),
        ledger_config_dir(entry).as_deref(),
        &entry.claude_session_id,
    )
}

fn outcome_of(
    entry: &LedgerEntry,
    outcome: &str,
    reason: Option<&str>,
    finished: bool,
) -> LedgerOutcome {
    LedgerOutcome {
        claude_session_id: entry.claude_session_id.clone(),
        terminal_id: entry.terminal_id.clone(),
        page_id: entry.page_id.clone(),
        zone_index: entry.zone_index,
        display_name: display_name(
            entry.session_name.as_deref(),
            entry.name_source.as_deref(),
            entry.title.as_deref(),
            &entry.claude_session_id,
        ),
        session_name: entry.session_name.clone(),
        name_source: entry.name_source.clone(),
        title: entry.title.clone(),
        account_label: entry.account_label.clone(),
        config_dir: entry.config_dir.clone(),
        config_dir_known: entry
            .config_dir
            .as_deref()
            .is_some_and(|c| !c.trim().is_empty()),
        provider: entry.provider.clone(),
        working_dir: entry.working_dir.clone(),
        worktree_path: entry.worktree_path.clone(),
        plan_slug: entry.plan_slug.clone(),
        work_unit_id: entry.work_unit_id.clone(),
        wip_state: entry.wip_state.clone(),
        wip_ref: entry.wip_ref.clone(),
        custody_session_mismatch: entry.custody_session_mismatch,
        last_seen_at: entry.last_seen_at,
        restorable: entry.restorable,
        finished,
        outcome: outcome.to_string(),
        reason: reason.map(str::to_string),
        resume_dir: resume_dir_of(entry.working_dir.as_deref(), entry.worktree_path.as_deref())
            .map(|d| d.trim().replace('\\', "/")),
        resume_command: ledger_resume_command(entry),
        resume_account: resume_account_for(ledger_config_dir(entry).as_deref()),
    }
}

/// The registry's CURRENT state per session, keyed by lowercased id — the
/// overlays [`diff`] applies over a generation's capture-time view.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegistryNow {
    /// Finished as of now — overrides the captured flag both ways, so a
    /// Finish/Unfinish shows on every generation at once.
    pub finished: HashMap<String, bool>,
    /// Sessions whose row is closed `user-close` NOW: the operator closed them
    /// on purpose after the capture, so a generation must not offer them back
    /// as missing.
    ///
    /// LIMIT: the overlay lasts only as long as the closed row is RETAINED —
    /// `SessionLifecycleStore::prune` drops a closed row 24 h after it closed
    /// (while its terminal is gone). The ledger itself records no user-close,
    /// so after that a retained generation (kept for five restarts, of any
    /// age) shows the session as `missing` again: an older generation can
    /// offer back a session the operator closed more than a day ago.
    pub closed_by_user: HashSet<String>,
}

/// [`RegistryNow`] from every registry row (open and closed).
pub fn registry_now(store: &SessionLifecycleStore) -> RegistryNow {
    let mut now = RegistryNow::default();
    for r in store.all_records() {
        let id = r.claude_session_id.to_ascii_lowercase();
        if r.state == "closed" && r.close_reason.as_deref() == Some("user-close") {
            now.closed_by_user.insert(id.clone());
        }
        now.finished.insert(id, r.finished_at.is_some());
    }
    now
}

/// What this boot observed of one session (plan
/// `2026-11-04-unplanned-restart-restore-is-page-gated-and-counts-unverified-resumes`,
/// D2). Only [`Observed::Back`] is a return; each other variant is its own
/// outcome, never laundered into `returned`.
///
/// Declared in MERGE order: when one session is observed on more than one row,
/// the strongest observation stands ([`observe_returns`]). `Unknown` sits just
/// below `Back` because an unreadable pane might be the one that IS back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Observed {
    /// Bound to a live terminal (or restored as one) in which no `claude` runs.
    TerminalOnly,
    /// A resume was attempted this boot and ended without landing.
    Failed,
    /// A resume was attempted this boot and no process read has verified it.
    AttemptedUnverified,
    /// Its pane is live and the process read that would judge it was
    /// unavailable.
    Unknown,
    /// Verified back: tier `resumed` this boot, or a process read of its live
    /// pane shows its `claude` running.
    Back,
}

/// [`Observed`] per session, keyed by LOWERCASED session id. A session absent
/// from the map was not observed at all this boot (`missing`).
pub type Observations = HashMap<String, Observed>;

/// One generation's buckets and verdict.
struct Classified {
    returned: Vec<LedgerOutcome>,
    attempted_unverified: Vec<LedgerOutcome>,
    terminal_only: Vec<LedgerOutcome>,
    failed: Vec<LedgerOutcome>,
    unknown: Vec<LedgerOutcome>,
    missing: Vec<LedgerOutcome>,
    finished: Vec<LedgerOutcome>,
    closed_by_user: Vec<LedgerOutcome>,
    verdict: &'static str,
    /// Which unknown, when the verdict is [`VERDICT_UNKNOWN`].
    unknown_reason: Option<&'static str>,
}

impl Classified {
    /// Expected back and not verified back: everything the verdict counts
    /// against `match`.
    fn not_back(&self) -> usize {
        self.missing.len()
            + self.attempted_unverified.len()
            + self.terminal_only.len()
            + self.failed.len()
            + self.unknown.len()
    }
}

/// The prior ledger's sessions, each id (case-insensitively) once, first
/// listing kept — the same set [`classify`] buckets, so `expected` and the
/// bucket counts agree.
fn unique_sessions(led: &SessionLedger) -> Vec<LedgerEntry> {
    let mut seen: HashSet<String> = HashSet::new();
    led.sessions
        .iter()
        .filter(|e| seen.insert(e.claude_session_id.to_ascii_lowercase()))
        .cloned()
        .collect()
}

/// PURE classification of one prior ledger against what this boot observed.
///
/// Back wins (it came back, finished or not); otherwise a FINISHED session —
/// as of now when the registry knows it, else as captured — is its own class
/// and never `missing`; so is one the operator has closed on purpose since
/// (`closed-by-user`); then each [`Observed`] outcome short of back is its own
/// class; everything else is `missing` with a reason. A session listed twice
/// in the prior ledger is classified once.
fn classify(
    prior: &SessionLedger,
    observed: &Observations,
    now: &RegistryNow,
    restore_stamps_available: bool,
) -> Classified {
    let mut c = Classified {
        returned: Vec::new(),
        attempted_unverified: Vec::new(),
        terminal_only: Vec::new(),
        failed: Vec::new(),
        unknown: Vec::new(),
        missing: Vec::new(),
        finished: Vec::new(),
        closed_by_user: Vec::new(),
        verdict: VERDICT_UNKNOWN,
        unknown_reason: None,
    };
    for e in &unique_sessions(prior) {
        let id = e.claude_session_id.to_ascii_lowercase();
        let is_finished = now.finished.get(&id).copied().unwrap_or(e.finished);
        let seen_now = observed.get(&id).copied();
        if seen_now == Some(Observed::Back) {
            c.returned
                .push(outcome_of(e, OUTCOME_BACK, None, is_finished));
        } else if is_finished {
            c.finished.push(outcome_of(e, OUTCOME_FINISHED, None, true));
        } else if now.closed_by_user.contains(&id) {
            c.closed_by_user
                .push(outcome_of(e, OUTCOME_CLOSED_BY_USER, None, false));
        } else if let Some(o) = seen_now {
            let out = |class: &str| outcome_of(e, class, None, false);
            match o {
                Observed::AttemptedUnverified => c
                    .attempted_unverified
                    .push(out(OUTCOME_ATTEMPTED_UNVERIFIED)),
                Observed::TerminalOnly => c.terminal_only.push(out(OUTCOME_TERMINAL_ONLY)),
                Observed::Failed => c.failed.push(out(OUTCOME_FAILED)),
                Observed::Unknown => c.unknown.push(out(OUTCOME_UNKNOWN)),
                // Taken by the first arm; spelled so the match is total.
                Observed::Back => c
                    .returned
                    .push(outcome_of(e, OUTCOME_BACK, None, is_finished)),
            }
        } else {
            // Two honest reasons, and no third: a session that was never
            // identity-restorable is not a restore defect, and everything else
            // is simply "nothing brought it back".
            let reason = match e.restorable {
                Some(true) => "no-attempt",
                Some(false) => "not-restorable",
                // Never "not-restorable": that sentence tells the operator the
                // conversation could not have come back, which we do not know.
                None => "restorability-unknown",
            };
            c.missing
                .push(outcome_of(e, OUTCOME_MISSING, Some(reason), false));
        }
    }

    // An input we could not read must not be laundered into a confident miss
    // count. `observe_returns` skips the sticky-restore-stamp arm when this
    // boot never latched a census, so a session that came back and was then
    // closed lands in `missing` — through no fault of the rebuild. Likewise a
    // pane whose process read was unavailable might be back, so no `match`
    // (and no `mismatch`) can be stated over it.
    //
    // `match` only when every session expected back is verified back. An
    // empty prior (or one whose every session is finished) is a real `match`
    // — only stateable because a ledger EXISTS saying so, which is the
    // difference from the `no_prior_ledger` arm of [`diff`].
    let not_back = c.not_back();
    (c.verdict, c.unknown_reason) = if !restore_stamps_available && not_back > 0 {
        (VERDICT_UNKNOWN, Some("restore_stamps_unavailable"))
    } else if !c.unknown.is_empty() {
        (VERDICT_UNKNOWN, Some("process_read_unavailable"))
    } else if not_back == 0 {
        (VERDICT_MATCH, None)
    } else if c.returned.is_empty() {
        (VERDICT_MISMATCH, None)
    } else {
        (VERDICT_PARTIAL, None)
    };
    c
}

/// PURE diff: the prior ledger vs what this boot observed.
///
/// Split from the route so the verdict and the resume lines are testable
/// without a store, a disk or a rebuild. `generations` is left empty; the
/// [`report`] builder fills it.
pub fn diff(
    prior: Option<&SessionLedger>,
    observed: &Observations,
    now: &RegistryNow,
    current: SessionLedger,
    restore_stamps_available: bool,
) -> LedgerReport {
    let generated_at = current.captured_at_ms;
    let Some(prior) = prior else {
        // No prior ledger cannot mean "nothing was lost". Same reading as
        // `restore_census`'s `census_not_latched` and served policy
        // `verification-and-evidence` `silent-empty-is-unknown`.
        return LedgerReport {
            status: "unavailable".to_string(),
            reason: Some("no_prior_ledger".to_string()),
            generated_at,
            prior_captured_at: None,
            prior_reason: None,
            expected: Vec::new(),
            returned: Vec::new(),
            attempted_unverified: Vec::new(),
            terminal_only: Vec::new(),
            failed: Vec::new(),
            unknown: Vec::new(),
            missing: Vec::new(),
            finished: Vec::new(),
            closed_by_user: Vec::new(),
            verdict: VERDICT_UNKNOWN.to_string(),
            note: "No ledger from a previous boot is on disk, so this runner cannot state \
                   what was open before it started. That is UNKNOWN, not 'nothing was \
                   lost'. The ledger this boot writes will make the NEXT rebuild \
                   answerable."
                .to_string(),
            current,
            generations: Vec::new(),
            saved_at_ms: None,
            saved_matches_current: false,
        };
    };

    let c = classify(prior, observed, now, restore_stamps_available);
    let not_back = c.not_back();
    let Classified {
        returned,
        attempted_unverified,
        terminal_only,
        failed,
        unknown,
        missing,
        finished,
        closed_by_user,
        verdict,
        unknown_reason,
    } = c;

    let unresumable = missing
        .iter()
        .filter(|m| m.resume_command.is_none())
        .count();
    let with_wip = missing
        .iter()
        .filter(|m| m.wip_state.is_some() || m.worktree_path.is_some())
        .count();
    // The headline counts only the sessions EXPECTED back — the same "N of M"
    // the strip shows. Finished and closed-on-purpose sessions are not
    // expected, so they are named separately rather than inflating M.
    let mut note = format!(
        "{} of {} sessions open before the last shutdown came back (verified: a process read \
         shows its claude running, or its restore verified this boot).",
        returned.len(),
        returned.len() + not_back,
    );
    let unverified =
        attempted_unverified.len() + terminal_only.len() + failed.len() + unknown.len();
    if unverified > 0 {
        note.push_str(&format!(
            " {} more are NOT counted back: {} resume(s) attempted and not verified, {} only a \
             terminal with no claude in it, {} resume(s) failed, {} whose process read was \
             unavailable.",
            unverified,
            attempted_unverified.len(),
            terminal_only.len(),
            failed.len(),
            unknown.len(),
        ));
    }
    note.push_str(&format!(
        " {} did not come back; {} of those name a worktree that may hold uncommitted work, and \
         {} could not be given a resume line because the account root they ran under is unknown \
         (a --resume under the wrong CLAUDE_CONFIG_DIR fails as though the session never \
         existed). {} more were marked finished and {} were closed by the operator since; \
         neither is expected back.",
        missing.len(),
        with_wip,
        unresumable,
        finished.len(),
        closed_by_user.len()
    ));
    if !restore_stamps_available {
        note.push_str(
            " CAVEAT: this boot never latched a restore census, so a session that came back \
             and was then CLOSED cannot be distinguished from one that never returned. The \
             miss count is an UPPER BOUND, which is why the verdict reads `unknown`.",
        );
    }

    LedgerReport {
        status: "ok".to_string(),
        reason: unknown_reason.map(str::to_string),
        generated_at,
        prior_captured_at: Some(prior.captured_at.clone()),
        prior_reason: Some(prior.reason.clone()),
        expected: unique_sessions(prior),
        returned,
        attempted_unverified,
        terminal_only,
        failed,
        unknown,
        missing,
        finished,
        closed_by_user,
        verdict: verdict.to_string(),
        note,
        current,
        generations: Vec::new(),
        saved_at_ms: None,
        saved_matches_current: false,
    }
}

/// Every retained generation's own diff, in the order the report serves: the
/// generation at `this_boot` (the one this process's boot rotated its prior
/// ledger into) FIRST and flagged [`LedgerGeneration::this_boot`], every other
/// in newest-first stamp order.
///
/// The stamp is the rotating boot's WALL clock, so a clock stepped backwards
/// between two boots stamps this boot's generation OLDER than the previous
/// one, and stamp order alone would put the wrong cohort first — the cohort the
/// panel opens on and pre-checks. The explicit mark is what the UI defaults to.
pub fn generation_reports(
    stored: &[StoredGeneration],
    this_boot: Option<&Path>,
    observed: &Observations,
    now: &RegistryNow,
    restore_stamps_available: bool,
) -> Vec<LedgerGeneration> {
    let is_this_boot = |g: &StoredGeneration| this_boot.is_some_and(|p| g.path == p);
    stored
        .iter()
        .filter(|g| is_this_boot(g))
        .chain(stored.iter().filter(|g| !is_this_boot(g)))
        .map(|g| {
            let mut r = generation_report(g, observed, now, restore_stamps_available);
            r.this_boot = is_this_boot(g);
            r
        })
        .collect()
}

/// One retained generation's own diff (`this_boot` unset — see
/// [`generation_reports`]).
pub fn generation_report(
    generation: &StoredGeneration,
    observed: &Observations,
    now: &RegistryNow,
    restore_stamps_available: bool,
) -> LedgerGeneration {
    let led = &generation.ledger;
    let Classified {
        returned,
        attempted_unverified,
        terminal_only,
        failed,
        unknown,
        missing,
        finished,
        closed_by_user,
        verdict,
        unknown_reason: _,
    } = classify(led, observed, now, restore_stamps_available);
    LedgerGeneration {
        file: generation
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        rotated_at_ms: generation.rotated_at_ms,
        boot_at_ms: led.boot_at_ms,
        this_boot: false,
        captured_at_ms: led.captured_at_ms,
        captured_at: led.captured_at.clone(),
        reason: led.reason.clone(),
        clean_shutdown: led.clean_shutdown,
        session_count: unique_sessions(led).len(),
        verdict: verdict.to_string(),
        returned,
        attempted_unverified,
        terminal_only,
        failed,
        unknown,
        missing,
        finished,
        closed_by_user,
    }
}

/// THE report builder — the one assembly behind `GET /control/sessions/ledger`
/// and the Tauri `session_ledger_report`, so the two can never disagree.
///
/// `store` is `None` only when the lifecycle store is not in Tauri state; the
/// report then reads `unavailable` WITH a reason (never a bare empty ledger),
/// and still lists the retained generations.
///
/// `live` is what this process can see of its panes right now ([`LiveView`]:
/// the PTYs alive in the `TerminalManager` and the process read of each) —
/// what makes an `open` row count as back (see [`observe_returns`]). `None`
/// when the terminal manager is not available: open rows then cannot be
/// confirmed back, and the verdict says `unknown` rather than pretending.
///
/// Read-only apart from latching (and, on a process's first call, rotating)
/// the prior ledger — see [`load_prior_once`].
pub fn report(store: Option<&SessionLifecycleStore>, live: Option<&LiveView>) -> LedgerReport {
    use crate::session::reconcile::DiskTranscriptIndex;

    let now = chrono::Utc::now().timestamp_millis();
    let boot = crate::session::shutdown_marker::boot_classification();
    // Latch the PRIOR ledger before anything in this process can overwrite it.
    // Idempotent, so calling it here as well as at boot is safe — and doing it
    // here means the report still answers on a path whose boot latch did not
    // run.
    let prior = load_prior_once();
    let stored = generations();

    let Some(store) = store else {
        warn!(
            "session_ledger: lifecycle store unavailable — reporting unavailable WITH a \
             reason (never a bare empty ledger)"
        );
        let empty = SessionLedger {
            ledger_version: LEDGER_VERSION,
            captured_at_ms: now,
            captured_at: chrono::Utc::now().to_rfc3339(),
            reason: "unavailable".to_string(),
            boot_at_ms: Some(process_boot_ms()),
            shutdown_at: boot.and_then(|b| b.prior_marker_at),
            clean_shutdown: boot.map(|b| !b.crash_recovery),
            sessions: Vec::new(),
        };
        let none = RegistryNow::default();
        let nothing = Observations::new();
        let mut report = diff(prior, &nothing, &none, empty, false);
        report.status = "unavailable".to_string();
        report.reason = Some("lifecycle_store_unavailable".to_string());
        report.verdict = VERDICT_UNKNOWN.to_string();
        report.generations =
            generation_reports(&stored, this_boot_generation(), &nothing, &none, false);
        return report;
    };

    let index = DiskTranscriptIndex::discover();
    let current = capture(store, &index, "read", now, boot);
    // "Came back" = verified by a process read or a `resumed` restore this
    // boot — a session that returned and was then closed did in fact return.
    // `None` when the boot census never latched — see `observe_returns`: an
    // absent boot instant SKIPS the sticky-restore-stamp arm rather than
    // admitting it with a `0` floor, and that is reported (verdict `unknown`
    // on any miss), not laundered into a miss count.
    let boot_at = crate::session::restore_census::latched().map(|c| c.boot_at_ms);
    let observed = observe_returns(store, boot_at, live);
    let now_state = registry_now(store);
    // Both inputs the observation rests on must be readable for a miss count
    // to be a claim rather than an upper bound.
    let observable = boot_at.is_some() && live.is_some();
    let mut report = diff(prior, &observed, &now_state, current, observable);
    report.generations = generation_reports(
        &stored,
        this_boot_generation(),
        &observed,
        &now_state,
        observable,
    );
    let saved = saved_by_this_process(&ledger_path(), process_boot_ms());
    report.saved_at_ms = saved.as_ref().map(|l| l.captured_at_ms);
    report.saved_matches_current = saved
        .as_ref()
        .is_some_and(|l| l.fingerprint() == report.current.fingerprint());
    report
}

/// What this process can see of its panes — [`report`]'s live input.
///
/// Built by [`live_view`] from the `TerminalManager` and one process-table
/// read, or by a test directly, so the back predicate needs no real `/proc`.
#[derive(Debug, Clone, Default)]
pub struct LiveView {
    /// Every PTY alive in this process now ([`alive_terminal_ids`]).
    pub terminals: HashSet<String>,
    /// The process read of each live pane. A live terminal with no entry, or
    /// an `Err` entry, is UNKNOWN — never back.
    pub panes: HashMap<String, PaneRead>,
}

/// [`LiveView`] of this process: the live PTYs, and the `claude` processes
/// under each, from ONE process-table snapshot. `None` when the terminal
/// manager is not in app state, which [`report`] reads as UNKNOWN, never as
/// "nothing is live". An unreadable process table leaves every pane's read an
/// `Err`, which [`observe_returns`] reads as [`Observed::Unknown`].
///
/// Memoised for [`LIVE_VIEW_TTL`]: the ledger, the census and the strip's
/// refreshes each ask, and one process-table read (PowerShell WMI on
/// Windows) answers them all. A memo rather than a roster-only bypass,
/// because it bounds every caller at one place — including the ones that DO
/// need the live read — with no per-route flag to keep right.
pub async fn live_view<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Option<LiveView> {
    use tauri::Manager;
    let tm = app
        .try_state::<std::sync::Arc<crate::terminal::TerminalManager>>()?
        .inner()
        .clone();
    let now = std::time::Instant::now();
    let cached = memo_fresh(
        &live_view_memo().lock().unwrap_or_else(|e| e.into_inner()),
        now,
    );
    if cached.is_some() {
        return cached;
    }
    let terminals = alive_terminal_ids(tm.list(), crate::process_capture::health::pid_alive);
    let panes = proc_read::read_panes(&tm, terminals.iter().cloned()).await;
    let view = LiveView { terminals, panes };
    *live_view_memo().lock().unwrap_or_else(|e| e.into_inner()) = Some((now, view.clone()));
    Some(view)
}

/// How long one [`live_view`] read serves later requests.
pub const LIVE_VIEW_TTL: std::time::Duration = std::time::Duration::from_secs(5);

fn live_view_memo() -> &'static Mutex<Option<(std::time::Instant, LiveView)>> {
    static MEMO: OnceLock<Mutex<Option<(std::time::Instant, LiveView)>>> = OnceLock::new();
    MEMO.get_or_init(|| Mutex::new(None))
}

/// PURE: the memoised view when it was read less than [`LIVE_VIEW_TTL`]
/// before `now`.
fn memo_fresh(
    memo: &Option<(std::time::Instant, LiveView)>,
    now: std::time::Instant,
) -> Option<LiveView> {
    memo.as_ref()
        .filter(|(at, _)| now.saturating_duration_since(*at) < LIVE_VIEW_TTL)
        .map(|(_, view)| view.clone())
}

/// The ids of the listed terminals whose PTY process is actually alive. The
/// manager keeps an exited PTY in its list (with `is_alive: false`) until the
/// tab closes, and a terminal still holding an `open` row's binding must not
/// count that row as back once its shell is gone. A terminal whose pid is
/// unknown is judged by the manager's own `is_alive` alone; one with a pid
/// must also pass `pid_alive`.
fn alive_terminal_ids(
    infos: impl IntoIterator<Item = crate::terminal::types::TerminalInfo>,
    pid_alive: impl Fn(u32) -> bool,
) -> HashSet<String> {
    infos
        .into_iter()
        .filter(|t| t.is_alive && t.pid.is_none_or(&pid_alive))
        .map(|t| t.id)
        .collect()
}

/// The live ledger at `path`, only when the process that booted at `boot_ms`
/// wrote it. Until this process's first write lands, the file is the PREVIOUS
/// boot's roster, which says nothing about what a restart now would bring
/// back.
fn saved_by_this_process(path: &Path, boot_ms: i64) -> Option<SessionLedger> {
    load_from(path).filter(|l| l.boot_at_ms == Some(boot_ms))
}

/// What a capture did — `POST /control/sessions/ledger/capture` and the Tauri
/// `session_ledger_capture`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerCapture {
    /// A write landed. `false` means the content was identical to the last
    /// one written (or an empty capture was refused over a non-empty prior),
    /// not that the capture failed.
    pub persisted: bool,
    /// Where the live ledger lives.
    pub path: String,
    pub ledger: SessionLedger,
}

/// THE capture — one deliberate, operator-timed capture of the roster,
/// persisted through [`persist_capture`] (so an empty capture can never erase
/// a non-empty prior).
pub fn capture_now(store: &SessionLifecycleStore, reason: &str) -> LedgerCapture {
    use crate::session::reconcile::DiskTranscriptIndex;

    // Latch (and retire) the prior ledger first, so a capture taken before any
    // read cannot destroy the previous boot's record of what was open.
    let _ = load_prior_once();
    let now = chrono::Utc::now().timestamp_millis();
    let boot = crate::session::shutdown_marker::boot_classification();
    let index = DiskTranscriptIndex::discover();
    let ledger = capture(store, &index, reason, now, boot);
    let persisted = persist_capture(&ledger);
    LedgerCapture {
        persisted,
        path: ledger_path().to_string_lossy().replace('\\', "/"),
        ledger,
    }
}

/// What this boot observed of every session (plan
/// `2026-11-04-unplanned-restart-restore-is-page-gated-and-counts-unverified-resumes`,
/// D2), keyed by lowercased id. A session is BACK only when:
///
/// * a restore THIS boot set its tier `resumed` (a verified handshake, or the
///   liveness poll seeing `claude` alive in its pane), or
/// * its row is `open`, bound to a terminal alive in this process, and that
///   pane's process read shows `claude` holding this session id ([`judge`]
///   against the row's recorded cwd and account).
///
/// Every other row this boot touched gets its own [`Observed`] outcome:
///
/// * a live-bound pane whose process read was unavailable — [`Observed::Unknown`];
/// * a live-bound pane holding a `claude` for this id that does not verify
///   (two of them, a wrong cwd or account) — [`Observed::AttemptedUnverified`];
/// * a live-bound pane with no `claude` for this id — what its restore stamp
///   says, else [`Observed::TerminalOnly`] (a bare shell);
/// * not live-bound, carrying this boot's restore stamp — what the stamp says:
///   tier `failed` with its pending marker still set is an attempt nothing
///   verified ([`Observed::AttemptedUnverified`]), `failed` with the marker
///   cleared is [`Observed::Failed`], `terminal-only` is
///   [`Observed::TerminalOnly`].
///
/// The stamp arm matters: a session that came back and was then closed by the
/// operator DID come back. But a stamp is an ATTEMPT — the frontend stamps
/// tier `failed` the moment it tries — so only a `resumed` stamp is a return.
/// `restored_from_boot_at` is STICKY across restarts by design, so a stamp
/// older than THIS boot belongs to a previous one. Without a boot instant to
/// compare against, the stamp arm is SKIPPED entirely rather than admitted
/// with a `0` floor — the wrong-in-the-operator's-favour direction and exactly
/// the vacuous-`match` failure the census's R3 names.
///
/// An `open` row is NOT back by being open, and not by being bound to a live
/// terminal either: a crash leaves every row `open`, and a restore that
/// spawned a shell and never typed the resume leaves a live bare shell.
///
/// A session observed on more than one row keeps its strongest observation
/// (the [`Observed`] order), so it is counted once.
///
/// [`judge`]: crate::session::claude_process_read::judge
pub fn observe_returns(
    store: &SessionLifecycleStore,
    boot_at_ms: Option<i64>,
    live: Option<&LiveView>,
) -> Observations {
    let mut out = Observations::new();
    for r in store.all_records() {
        if let Some(o) = observe_row(&r, boot_at_ms, live) {
            let slot = out
                .entry(r.claude_session_id.to_ascii_lowercase())
                .or_insert(o);
            *slot = (*slot).max(o);
        }
    }
    out
}

/// [`observe_returns`] for one row; `None` = this boot did nothing about it.
/// THE per-row back predicate: the restore census judges its rows with it
/// too ([`crate::session::restore_census::observe_restored`]).
pub fn observe_row(
    r: &TerminalSessionRecord,
    boot_at_ms: Option<i64>,
    live: Option<&LiveView>,
) -> Option<Observed> {
    let stamped_this_boot =
        boot_at_ms.is_some_and(|boot| r.restored_from_boot_at.is_some_and(|at| at >= boot));
    let stamp = stamped_this_boot.then(|| match r.restore_tier.as_deref() {
        Some(RESTORE_TIER_RESUMED) => Observed::Back,
        Some(RESTORE_TIER_TERMINAL_ONLY) => Observed::TerminalOnly,
        Some(RESTORE_TIER_FAILED) if r.restore_pending_at.is_none() => Observed::Failed,
        // `failed` with the pending marker still set is the attempt stamp
        // itself; an unrecognised tier proves no more than that.
        _ => Observed::AttemptedUnverified,
    });
    if stamp == Some(Observed::Back) {
        return stamp;
    }
    // A CLOSED row that never verified is not a pending outcome of this
    // boot: nothing holds it, and the resume door takes it. It reads as
    // `missing` (resumable), so every non-back outcome names an OPEN row.
    if r.state != "open" {
        return None;
    }
    let live_pane = live.filter(|l| r.state == "open" && l.terminals.contains(&r.terminal_id));
    let Some(view) = live_pane else {
        return stamp;
    };
    let Some(Ok(procs)) = view.panes.get(&r.terminal_id) else {
        return Some(Observed::Unknown);
    };
    let want = ResumeExpectation {
        session_id: &r.claude_session_id,
        working_dir: r.working_dir.as_deref(),
        config_dir: r.config_dir.as_deref(),
    };
    Some(match proc_read::judge(procs, &want, &r.terminal_id) {
        Ok(_) => Observed::Back,
        // A claude runs here and does not verify as this session: wrong cwd
        // or account, two of them, or one whose command line names no id or
        // another one (`--continue`, an in-session `/resume`,
        // `--fork-session`). Not a bare terminal, and not back.
        Err(Some(_)) => Observed::AttemptedUnverified,
        Err(None) if !procs.is_empty() => Observed::AttemptedUnverified,
        // The read succeeded and no claude runs in the pane.
        Err(None) => stamp.unwrap_or(Observed::TerminalOnly),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none() -> RegistryNow {
        RegistryNow::default()
    }

    fn entry(id: &str, restorable: Option<bool>, config: Option<&str>) -> LedgerEntry {
        LedgerEntry {
            claude_session_id: id.to_string(),
            terminal_id: format!("term-{id}"),
            page_id: "default".to_string(),
            zone_index: 0,
            title: Some(format!("tab-{id}")),
            session_name: Some(format!("name-{id}")),
            name_source: None,
            account_label: Some("tiohorst".to_string()),
            config_dir: config.map(str::to_string),
            resume_config_dir: config.map(str::to_string),
            resume_command: None,
            provider: Some("claude".to_string()),
            working_dir: Some("D:/qontinui-root/_wt/thing/sub".to_string()),
            last_seen_at: Some(900),
            finished: false,
            restorable,
            worktree_path: Some("D:/qontinui-root/_wt/thing".to_string()),
            plan_slug: Some("2026-08-22-wip-custody".to_string()),
            work_unit_id: None,
            wip_state: Some("captured".to_string()),
            wip_ref: Some(format!("refs/wip/{id}")),
            custody_session_mismatch: false,
        }
    }

    fn ledger(entries: Vec<LedgerEntry>) -> SessionLedger {
        SessionLedger {
            ledger_version: LEDGER_VERSION,
            captured_at_ms: 1_000,
            captured_at: "2026-08-24T00:00:00Z".to_string(),
            reason: REASON_PRE_REBUILD.to_string(),
            boot_at_ms: Some(100),
            shutdown_at: Some(900),
            clean_shutdown: Some(true),
            sessions: entries,
        }
    }

    /// THE Phase-4 acceptance: rebuild with N open; afterwards the ledger names
    /// all N, marks which returned, and gives a resume line for each that did
    /// not.
    #[test]
    fn the_report_names_all_n_marks_returns_and_gives_a_resume_line_for_each_miss() {
        let prior = ledger(vec![
            entry("s1", Some(true), Some("C:/claude/.claude-gmail")),
            entry("s2", Some(true), Some("C:/claude/.claude-tiohorst")),
            entry("s3", Some(true), Some("C:/claude/.claude-paktis")),
        ]);
        let report = diff(
            Some(&prior),
            &obs(&["s1"]),
            &none(),
            ledger(vec![entry(
                "s1",
                Some(true),
                Some("C:/claude/.claude-gmail"),
            )]),
            true,
        );

        assert_eq!(report.expected.len(), 3, "the ledger names ALL N");
        assert_eq!(report.returned.len(), 1);
        assert_eq!(report.missing.len(), 2);
        assert_eq!(report.verdict, VERDICT_PARTIAL);
        for m in &report.missing {
            let cmd = m
                .resume_command
                .as_deref()
                .unwrap_or_else(|| panic!("{} must carry a resume line", m.claude_session_id));
            assert!(cmd.contains("claude --resume"), "{cmd}");
            assert!(cmd.contains("CLAUDE_CONFIG_DIR="), "{cmd}");
            assert!(cmd.contains("D:/qontinui-root/_wt/thing"), "{cmd}");
            // …and it says what work is at risk.
            assert_eq!(m.plan_slug.as_deref(), Some("2026-08-22-wip-custody"));
            assert_eq!(m.wip_state.as_deref(), Some("captured"));
        }
    }

    /// A resume line is OMITTED, never guessed, when the account root is
    /// unknown — and the note says how many were omitted for that reason.
    #[test]
    fn an_unknown_account_root_yields_no_resume_line_and_the_note_says_so() {
        let prior = ledger(vec![entry("s1", Some(true), None)]);
        let report = diff(Some(&prior), &obs(&[]), &none(), ledger(Vec::new()), true);
        assert_eq!(report.missing.len(), 1);
        assert_eq!(report.missing[0].resume_command, None);
        assert!(
            report
                .note
                .contains("account root they ran under is unknown"),
            "{}",
            report.note
        );
        assert_eq!(report.verdict, VERDICT_MISMATCH);
    }

    /// No prior ledger is UNKNOWN, never `match`. This is the whole reason the
    /// in-process `OnceLock` was not enough.
    #[test]
    fn no_prior_ledger_is_unknown_never_a_vacuous_match() {
        let report = diff(None, &obs(&[]), &none(), ledger(Vec::new()), true);
        assert_eq!(report.verdict, VERDICT_UNKNOWN);
        assert_eq!(report.status, "unavailable");
        assert_eq!(report.reason.as_deref(), Some("no_prior_ledger"));
        assert!(report.note.contains("not 'nothing was"), "{}", report.note);
    }

    /// An EMPTY prior ledger is different from an ABSENT one: the previous boot
    /// affirmatively had nothing open, and a ledger on disk says so.
    #[test]
    fn an_empty_prior_ledger_is_a_real_match_unlike_an_absent_one() {
        let report = diff(
            Some(&ledger(Vec::new())),
            &obs(&[]),
            &none(),
            ledger(Vec::new()),
            true,
        );
        assert_eq!(report.verdict, VERDICT_MATCH);
        assert_eq!(report.status, "ok");
    }

    /// A session that was never identity-restorable is not a restore defect,
    /// and the report says which kind of miss it was.
    #[test]
    fn a_never_restorable_session_is_reported_as_such_not_as_a_failed_restore() {
        let prior = ledger(vec![
            entry("s1", Some(false), Some("C:/claude/.claude-gmail")),
            entry("s2", Some(true), Some("C:/claude/.claude-gmail")),
        ]);
        let report = diff(Some(&prior), &obs(&[]), &none(), ledger(Vec::new()), true);
        let by = |id: &str| {
            report
                .missing
                .iter()
                .find(|m| m.claude_session_id == id)
                .unwrap()
                .reason
                .clone()
        };
        assert_eq!(by("s1").as_deref(), Some("not-restorable"));
        assert_eq!(by("s2").as_deref(), Some("no-attempt"));
    }

    /// Ids are matched case-insensitively — a uuid re-cased anywhere in the
    /// chain must not manufacture a phantom `missing`.
    #[test]
    fn id_matching_is_case_insensitive() {
        let prior = ledger(vec![entry(
            "AAAA-1111",
            Some(true),
            Some("C:/claude/.claude-x"),
        )]);
        let report = diff(
            Some(&prior),
            &obs(&["aaaa-1111"]),
            &none(),
            ledger(Vec::new()),
            true,
        );
        assert_eq!(report.returned.len(), 1);
        assert!(report.missing.is_empty());
        assert_eq!(report.verdict, VERDICT_MATCH);
    }

    /// An UNPROBEABLE record must not be told its conversation could not have
    /// come back. That sentence, on a session with real WIP, is the operator
    /// deciding not to bother resuming it.
    #[test]
    fn an_unprobeable_session_is_restorability_unknown_not_not_restorable() {
        let prior = ledger(vec![
            entry("s1", None, Some("C:/claude/.claude-gmail")),
            entry("s2", Some(false), Some("C:/claude/.claude-gmail")),
        ]);
        let report = diff(Some(&prior), &obs(&[]), &none(), ledger(Vec::new()), true);
        let by = |id: &str| {
            report
                .missing
                .iter()
                .find(|m| m.claude_session_id == id)
                .unwrap()
                .reason
                .clone()
        };
        assert_eq!(by("s1").as_deref(), Some("restorability-unknown"));
        assert_eq!(by("s2").as_deref(), Some("not-restorable"));
        // …and it still gets a resume line, because we do not know it is dead.
        let s1 = report
            .missing
            .iter()
            .find(|m| m.claude_session_id == "s1")
            .unwrap();
        assert!(s1.resume_command.is_some());
    }

    /// A skipped input must not become a confident miss count.
    #[test]
    fn absent_restore_stamps_downgrade_the_verdict_to_unknown() {
        let prior = ledger(vec![entry("s1", Some(true), Some("C:/claude/.claude-x"))]);
        let report = diff(
            Some(&prior),
            &obs(&[]),
            &none(),
            ledger(Vec::new()),
            /* stamps */ false,
        );
        assert_eq!(report.verdict, VERDICT_UNKNOWN);
        assert_eq!(report.reason.as_deref(), Some("restore_stamps_unavailable"));
        assert!(report.note.contains("UPPER BOUND"), "{}", report.note);
        // The evidence is still shown, exactly as the census does for `unknown`.
        assert_eq!(report.missing.len(), 1);
    }

    /// No miss ⇒ no caveat: an absent input only matters when it could have
    /// changed the answer.
    #[test]
    fn absent_restore_stamps_do_not_downgrade_a_full_match() {
        let prior = ledger(vec![entry("s1", Some(true), Some("C:/claude/.claude-x"))]);
        let report = diff(
            Some(&prior),
            &obs(&["s1"]),
            &none(),
            ledger(Vec::new()),
            false,
        );
        assert_eq!(report.verdict, VERDICT_MATCH);
        assert_eq!(report.reason, None);
    }

    /// A shell-hostile id or path yields NO resume line rather than a broken
    /// (or dangerous) one the operator would paste.
    #[test]
    fn a_shell_hostile_id_or_path_yields_no_resume_line() {
        let mut e = entry("s1\"; rm -rf /", Some(true), Some("C:/claude/.claude-x"));
        assert_eq!(ledger_resume_command(&e), None);

        e = entry("s1", Some(true), Some("C:/claude/.claude-x"));
        e.worktree_path = Some("D:/a`whoami`".to_string());
        e.working_dir = None;
        assert_eq!(ledger_resume_command(&e), None);

        e = entry("s1", Some(true), Some("C:/$EVIL"));
        assert_eq!(ledger_resume_command(&e), None);
    }

    /// The shared helper: an UNKNOWN account gives no line, never a bare
    /// `claude --resume` that would run under the default account.
    #[test]
    fn shared_resume_line_is_omitted_for_an_unknown_account() {
        assert_eq!(resume_command_for(Some("/w"), None, "sess-1"), None);
        assert_eq!(
            resume_command_for(None, Some("/a/.claude-x"), "sess-1"),
            None,
            "no cwd ⇒ no line: a resume from the wrong dir finds nothing"
        );
        assert_eq!(resume_command_for(Some("/w"), Some("  "), "sess-1"), None);
    }

    /// An account OUTSIDE the five names the old wrapper table knew gets a
    /// correct `CLAUDE_CONFIG_DIR` line — the wrapper table gave it a bare
    /// `claude`, i.e. the default account.
    #[test]
    fn shared_resume_line_names_any_account_by_its_config_dir() {
        assert_eq!(
            resume_command_for(Some("D:\\repo\\x"), Some("C:/claude/.claude-niklas"), "sess-1")
                .as_deref(),
            Some("cd \"D:/repo/x\" && CLAUDE_CONFIG_DIR=\"C:/claude/.claude-niklas\" claude --resume sess-1"),
            "backslashes normalize to forward slashes"
        );
    }

    /// The default home (`~/.claude`) resumes with NO `CLAUDE_CONFIG_DIR` —
    /// typing it would swap the default account's `~/.claude.json` for
    /// `~/.claude/.claude.json`.
    #[test]
    fn shared_resume_line_omits_the_variable_for_the_default_home() {
        let Some(home) = dirs::home_dir() else {
            return;
        };
        let default_home = home.join(".claude");
        let default_home = default_home.to_str().unwrap().replace('\\', "/");
        assert_eq!(
            resume_command_for(Some("/w"), Some(&default_home), "sess-1").as_deref(),
            Some("cd \"/w\" && claude --resume sess-1")
        );
    }

    /// The resolution order: recorded, else a UNIQUE transcript holder, else
    /// unknown — zero or several holders never pick one.
    #[test]
    fn config_dir_resolution_takes_recorded_then_a_unique_holder() {
        let x = PathBuf::from("/a/.claude-x");
        let y = PathBuf::from("/a/.claude-y");
        assert_eq!(
            resolve_config_dir(Some("/rec"), Some(&[x.clone(), y.clone()])).as_deref(),
            Some("/rec")
        );
        assert_eq!(
            resolve_config_dir(None, Some(std::slice::from_ref(&x))).as_deref(),
            Some("/a/.claude-x")
        );
        assert_eq!(
            resolve_config_dir(Some(" "), Some(std::slice::from_ref(&x))).as_deref(),
            Some("/a/.claude-x")
        );
        assert_eq!(resolve_config_dir(None, Some(&[x, y])), None);
        assert_eq!(resolve_config_dir(None, Some(&[])), None);
        assert_eq!(resolve_config_dir(None, None), None);
    }

    /// An empty capture must never destroy a non-empty prior — that is how a
    /// later boot manufactures a vacuous `match`.
    #[test]
    fn an_empty_capture_is_refused_when_it_would_erase_a_non_empty_prior() {
        let full = ledger(vec![entry("s1", Some(true), None)]);
        let empty = ledger(Vec::new());
        assert!(
            refuses_overwrite(&empty, Some(&full), false),
            "empty over non-empty: REFUSE"
        );
        assert!(
            !refuses_overwrite(&empty, Some(&empty), false),
            "empty over empty: fine"
        );
        assert!(
            !refuses_overwrite(&empty, None, false),
            "empty with no prior: fine"
        );
        assert!(
            !refuses_overwrite(&full, Some(&full), false),
            "non-empty always proceeds"
        );
    }

    /// Once THIS process has written a roster of its own, an empty capture is
    /// the operator having closed every tab — it is written, so the next boot
    /// does not offer back sessions that were closed on purpose.
    #[test]
    fn an_empty_capture_is_written_once_this_process_has_written_a_non_empty_one() {
        let full = ledger(vec![entry("s1", Some(true), None)]);
        let empty = ledger(Vec::new());
        assert!(!refuses_overwrite(&empty, Some(&full), true));
    }

    /// The change key moves on the fields that make an entry actionable, and
    /// only on those — so a quiet poll tick costs a comparison, not a write.
    #[test]
    fn the_fingerprint_moves_on_content_and_not_on_capture_time() {
        let a = ledger(vec![entry("s1", Some(true), Some("C:/claude/.claude-x"))]);
        let mut b = a.clone();
        b.captured_at_ms = 999_999;
        b.captured_at = "2027-01-01T00:00:00Z".to_string();
        b.reason = REASON_POLL.to_string();
        assert_eq!(
            a.fingerprint(),
            b.fingerprint(),
            "time alone is not a change"
        );

        let mut c = a.clone();
        c.sessions
            .push(entry("s2", Some(true), Some("C:/claude/.claude-x")));
        assert_ne!(
            a.fingerprint(),
            c.fingerprint(),
            "a new session IS a change"
        );

        let mut d = a.clone();
        d.sessions[0].worktree_path = Some("D:/elsewhere".to_string());
        assert_ne!(
            a.fingerprint(),
            d.fingerprint(),
            "a moved worktree IS a change"
        );
    }

    /// Entry order must not manufacture a change.
    #[test]
    fn the_fingerprint_is_order_independent() {
        let a = ledger(vec![
            entry("s1", Some(true), Some("C:/claude/.claude-x")),
            entry("s2", Some(true), Some("C:/claude/.claude-x")),
        ]);
        let b = ledger(vec![
            entry("s2", Some(true), Some("C:/claude/.claude-x")),
            entry("s1", Some(true), Some("C:/claude/.claude-x")),
        ]);
        assert_eq!(a.fingerprint(), b.fingerprint());
    }

    /// A ledger whose version we do not recognise is ABSENT, never
    /// half-parsed.
    #[test]
    fn an_unknown_ledger_version_reads_as_absent() {
        let dir = std::env::temp_dir().join(format!("qontinui-ledger-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("v99.json");
        let mut l = ledger(vec![entry("s1", Some(true), None)]);
        l.ledger_version = 99;
        std::fs::write(&p, serde_json::to_vec(&l).unwrap()).unwrap();
        assert!(load_from(&p).is_none());

        l.ledger_version = LEDGER_VERSION;
        std::fs::write(&p, serde_json::to_vec(&l).unwrap()).unwrap();
        assert_eq!(load_from(&p).map(|l| l.sessions.len()), Some(1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_ledger_file_is_none_not_an_error() {
        assert!(load_from(Path::new("D:/definitely/not/here/ledger.json")).is_none());
    }

    // --- plan 2026-10-04-runner-session-roster-restore-picker, Phase 2 -----

    /// A FINISHED session is its own outcome class and never `missing`; with
    /// every unfinished one back, the verdict is a real `match`.
    #[test]
    fn a_finished_session_is_never_reported_missing() {
        let mut done = entry("done", Some(true), Some("C:/claude/.claude-x"));
        done.finished = true;
        let prior = ledger(vec![
            entry("live", Some(true), Some("C:/claude/.claude-x")),
            done,
        ]);
        let report = diff(
            Some(&prior),
            &obs(&["live"]),
            &none(),
            ledger(Vec::new()),
            true,
        );
        assert!(report.missing.is_empty(), "{:?}", report.missing);
        assert_eq!(report.finished.len(), 1);
        assert_eq!(report.finished[0].claude_session_id, "done");
        assert_eq!(report.finished[0].outcome, OUTCOME_FINISHED);
        assert!(report.finished[0].finished);
        assert_eq!(report.returned[0].outcome, OUTCOME_BACK);
        assert_eq!(report.verdict, VERDICT_MATCH);
        assert!(
            report.note.contains("1 more were marked finished"),
            "{}",
            report.note
        );
        // The headline counts only sessions EXPECTED back — the strip's
        // "1 of 1", never "1 of 2".
        assert!(
            report.note.starts_with("1 of 1 sessions"),
            "{}",
            report.note
        );
    }

    /// A session the operator CLOSED on purpose after the capture (its row is
    /// `user-close` now) is its own class: not missing, not counted in the
    /// headline, and a session that came back and was then closed is still
    /// `back`.
    #[test]
    fn a_session_closed_by_the_user_since_is_not_reported_missing() {
        let prior = ledger(vec![
            entry("kept", Some(true), Some("C:/claude/.claude-x")),
            entry("closed-on-purpose", Some(true), Some("C:/claude/.claude-x")),
            entry("back-then-closed", Some(true), Some("C:/claude/.claude-x")),
            entry("lost", Some(true), Some("C:/claude/.claude-x")),
        ]);
        let mut now = RegistryNow::default();
        now.closed_by_user.insert("closed-on-purpose".to_string());
        now.closed_by_user.insert("back-then-closed".to_string());
        let report = diff(
            Some(&prior),
            &obs(&["kept", "back-then-closed"]),
            &now,
            ledger(Vec::new()),
            true,
        );
        let ids = |v: &[LedgerOutcome]| -> Vec<String> {
            v.iter().map(|o| o.claude_session_id.clone()).collect()
        };
        assert_eq!(
            ids(&report.closed_by_user),
            vec!["closed-on-purpose".to_string()]
        );
        assert_eq!(report.closed_by_user[0].outcome, OUTCOME_CLOSED_BY_USER);
        assert_eq!(ids(&report.missing), vec!["lost".to_string()]);
        assert_eq!(
            ids(&report.returned),
            vec!["kept".to_string(), "back-then-closed".to_string()]
        );
        assert!(
            report.note.starts_with("2 of 3 sessions"),
            "{}",
            report.note
        );
        assert!(
            report.note.contains("1 were closed by the operator"),
            "{}",
            report.note
        );
    }

    /// The resume directory is the session's exact launch dir (Claude Code
    /// scopes sessions by it); the worktree root is only the fallback.
    #[test]
    fn the_resume_dir_is_the_working_dir_then_the_worktree_root() {
        let prior = ledger(vec![entry("s1", Some(true), Some("C:/claude/.claude-x"))]);
        let report = diff(Some(&prior), &obs(&[]), &none(), ledger(Vec::new()), true);
        let m = &report.missing[0];
        assert_eq!(
            m.resume_dir.as_deref(),
            Some("D:/qontinui-root/_wt/thing/sub")
        );
        assert!(
            m.resume_command
                .as_deref()
                .unwrap()
                .starts_with("cd \"D:/qontinui-root/_wt/thing/sub\" && "),
            "{:?}",
            m.resume_command
        );
        assert_eq!(
            resume_dir_of(None, Some("D:/wt")),
            Some("D:/wt"),
            "no recorded launch dir: the worktree root"
        );
        assert_eq!(resume_dir_of(Some("  "), Some("D:/wt")), Some("D:/wt"));
        assert_eq!(resume_dir_of(None, None), None);
    }

    /// The registry's CURRENT finished state overrides the captured flag both
    /// ways, so a Finish/Unfinish shows on every generation at once.
    #[test]
    fn the_current_finished_state_overrides_the_captured_one() {
        let mut was_done = entry("was-done", Some(true), Some("C:/claude/.claude-x"));
        was_done.finished = true;
        let prior = ledger(vec![
            entry("now-done", Some(true), Some("C:/claude/.claude-x")),
            was_done,
        ]);
        let mut now = RegistryNow::default();
        now.finished.insert("now-done".to_string(), true);
        now.finished.insert("was-done".to_string(), false);
        let report = diff(Some(&prior), &obs(&[]), &now, ledger(Vec::new()), true);
        let ids = |v: &[LedgerOutcome]| -> Vec<String> {
            v.iter().map(|o| o.claude_session_id.clone()).collect()
        };
        assert_eq!(ids(&report.finished), vec!["now-done".to_string()]);
        assert_eq!(ids(&report.missing), vec!["was-done".to_string()]);
        assert_eq!(report.missing[0].outcome, OUTCOME_MISSING);
        assert!(!report.missing[0].finished);
    }

    /// Every outcome carries the account and naming fields the picker needs —
    /// `account_label` was dropped before this phase.
    #[test]
    fn outcomes_carry_account_and_name_fields() {
        let mut e = entry(
            "abcdef0123456789",
            Some(true),
            Some("C:/claude/.claude-alt"),
        );
        e.account_label = Some("alt".to_string());
        e.name_source = Some("derived".to_string());
        let mut unknown = entry("s2", Some(true), Some("   "));
        unknown.session_name = Some("my-rename".to_string());
        let report = diff(
            Some(&ledger(vec![e, unknown])),
            &obs(&[]),
            &none(),
            ledger(Vec::new()),
            true,
        );
        let by = |id: &str| {
            report
                .missing
                .iter()
                .find(|m| m.claude_session_id == id)
                .unwrap()
                .clone()
        };
        let a = by("abcdef0123456789");
        assert_eq!(a.account_label.as_deref(), Some("alt"));
        assert_eq!(a.config_dir.as_deref(), Some("C:/claude/.claude-alt"));
        assert!(a.config_dir_known);
        assert_eq!(a.name_source.as_deref(), Some("derived"));
        assert_eq!(
            a.display_name, "tab-abcdef0123456789",
            "derived name falls back to title"
        );
        assert_eq!(a.provider.as_deref(), Some("claude"));
        assert_eq!(a.last_seen_at, Some(900));
        assert_eq!(a.page_id, "default");

        let b = by("s2");
        assert!(
            !b.config_dir_known,
            "a blank config dir is not a known account"
        );
        assert_eq!(b.display_name, "my-rename", "an operator-chosen name wins");

        let json = serde_json::to_value(&a).unwrap();
        for key in [
            "accountLabel",
            "configDir",
            "configDirKnown",
            "nameSource",
            "displayName",
            "finished",
            "outcome",
            "lastSeenAt",
            "provider",
        ] {
            assert!(
                json.get(key).is_some(),
                "missing camelCase key {key}: {json}"
            );
        }
    }

    /// THE display-name rule: an operator-chosen (or source-less) name wins;
    /// a `derived` auto-name falls back to the tab title; blanks are absent;
    /// nothing at all is `claude <id8>`.
    #[test]
    fn the_display_name_rule() {
        let id = "0123456789abcdef";
        assert_eq!(display_name(Some("mine"), None, Some("tab"), id), "mine");
        assert_eq!(
            display_name(Some("mine"), Some("user"), Some("tab"), id),
            "mine"
        );
        assert_eq!(
            display_name(Some("repo-3f"), Some("derived"), Some("tab"), id),
            "tab"
        );
        assert_eq!(
            display_name(Some("repo-3f"), Some("derived"), None, id),
            "repo-3f"
        );
        assert_eq!(display_name(Some("  "), None, Some("tab"), id), "tab");
        assert_eq!(display_name(None, None, Some(" "), id), "claude 01234567");
    }

    fn write_live(dir: &Path, l: &SessionLedger) -> PathBuf {
        let live = dir.join("session-ledger.json");
        assert!(write_ledger(&live, l));
        live
    }

    fn numbered(n: usize) -> SessionLedger {
        let mut l = ledger(
            (0..=n)
                .map(|i| entry(&format!("s{i}"), Some(true), None))
                .collect(),
        );
        l.captured_at_ms = n as i64;
        l
    }

    /// Rotation keeps the newest five generations and deletes the oldest.
    #[test]
    fn rotation_keeps_five_generations_and_deletes_the_oldest() {
        let dir = tempfile::tempdir().unwrap();
        for boot in 1..=7_usize {
            let live = write_live(dir.path(), &numbered(boot));
            let rotated = rotate_in(&live, 1_000 + boot as i64, LEDGER_GENERATIONS_KEPT);
            assert!(rotated.is_some(), "boot {boot} retires a distinct ledger");
        }
        let gens = generations_in(dir.path());
        assert_eq!(gens.len(), LEDGER_GENERATIONS_KEPT);
        let stamps: Vec<i64> = gens.iter().map(|g| g.rotated_at_ms).collect();
        assert_eq!(
            stamps,
            vec![1_007, 1_006, 1_005, 1_004, 1_003],
            "newest first"
        );
        assert!(!dir.path().join("session-ledger.1001.json").exists());
        assert!(!dir.path().join("session-ledger.1002.json").exists());
        // The live file is copied, never moved.
        assert!(dir.path().join("session-ledger.json").exists());
        // Each generation keeps its own content and boot time.
        assert_eq!(gens[0].ledger.sessions.len(), 8);
        assert_eq!(gens[0].ledger.boot_at_ms, Some(100));
        // A stray that is not a generation is neither listed nor pruned.
        std::fs::write(dir.path().join("session-ledger.json.tmp.1.2.3"), b"x").unwrap();
        assert_eq!(
            generation_files_in(dir.path()).len(),
            LEDGER_GENERATIONS_KEPT
        );
    }

    /// A second boot whose registry reads EMPTY does not erase the earlier
    /// generation: the live file is protected by the empty-capture refusal,
    /// and the next boot does not re-rotate the same cohort into a second
    /// slot.
    #[test]
    fn a_second_boot_with_an_empty_registry_does_not_erase_the_earlier_generation() {
        let dir = tempfile::tempdir().unwrap();
        // Boot 1 ran with three sessions.
        let boot1 = numbered(2);
        let live = write_live(dir.path(), &boot1);

        // Boot 2: latch + retire the prior, then capture an EMPTY registry.
        let prior = load_from(&live);
        assert!(rotate_in(&live, 2_000, LEDGER_GENERATIONS_KEPT).is_some());
        let empty = ledger(Vec::new());
        assert!(
            refuses_overwrite(&empty, prior.as_ref(), false),
            "the empty write is refused"
        );

        // Boot 3: the live file still holds boot 1's cohort; retiring it again
        // would only duplicate it.
        assert_eq!(load_from(&live).as_ref(), Some(&boot1));
        assert_eq!(
            rotate_in(&live, 3_000, LEDGER_GENERATIONS_KEPT),
            Some(dir.path().join("session-ledger.2000.json")),
            "the identical retained generation is this boot's prior"
        );

        let gens = generations_in(dir.path());
        assert_eq!(gens.len(), 1);
        assert_eq!(gens[0].rotated_at_ms, 2_000);
        assert_eq!(gens[0].ledger, boot1);
    }

    /// Plan `2026-10-28-force-exited-runner-strands-open-sessions-out-of-the-restore-set`,
    /// Phase 5: the 2026-10-27 shape, proved on this code rather than rebuilt.
    /// The prior boot left a 65-session ledger; this boot's poll then shrank
    /// the live file to 57 and then to 1. On the incident build each capture
    /// overwrote the only copy. Here the boot latch keeps the 65 twice: the
    /// in-memory prior the report diffs (`expected` reads 65), and the
    /// generation `rotate_in` wrote before any capture, which the NEXT boot
    /// still lists although its own prior is the 1-session file. Steps are in
    /// `load_prior_once` / `persist_capture` order, on the same primitives.
    #[test]
    fn poll_captures_that_shrink_the_live_file_keep_the_boot_generation() {
        let roster = |n: usize| {
            let mut l = ledger(
                (0..n)
                    .map(|i| entry(&format!("s-{i:02}"), Some(true), None))
                    .collect(),
            );
            l.reason = REASON_POLL.to_string();
            l
        };
        let dir = tempfile::tempdir().unwrap();
        let live = write_live(dir.path(), &roster(65));

        // Boot: latch the prior, then retire it into a generation, before any
        // capture is written.
        let prior = load_from(&live);
        let this_boot =
            rotate_in(&live, 2_000, LEDGER_GENERATIONS_KEPT).expect("the prior is retained");

        // The poll's captures shrink the live file. A non-empty capture is
        // never refused, so both land.
        let mut wrote_non_empty = false;
        for n in [57, 1] {
            let capture = roster(n);
            assert!(!refuses_overwrite(
                &capture,
                prior.as_ref(),
                wrote_non_empty
            ));
            assert!(write_ledger(&live, &capture));
            wrote_non_empty = true;
        }
        assert_eq!(load_from(&live).map(|l| l.sessions.len()), Some(1));

        // GET /control/sessions/ledger diffs the latched prior: expected 65.
        let back = obs(&["s-00"]);
        let report = diff(prior.as_ref(), &back, &none(), roster(1), true);
        assert_eq!(report.expected.len(), 65);
        assert_eq!(report.returned.len(), 1);
        assert_eq!(report.missing.len(), 64);
        assert_eq!(report.verdict, VERDICT_PARTIAL);

        // The boot generation still holds the whole prior roster, served first.
        let gens = generation_reports(
            &generations_in(dir.path()),
            Some(&this_boot),
            &back,
            &none(),
            true,
        );
        assert!(gens[0].this_boot);
        assert_eq!(gens[0].session_count, 65);

        // The NEXT boot latches the 1-session file as its prior, and the 65
        // generation survives beside it.
        assert_eq!(load_from(&live).map(|l| l.sessions.len()), Some(1));
        assert!(rotate_in(&live, 3_000, LEDGER_GENERATIONS_KEPT).is_some());
        let counts: Vec<(i64, usize)> = generations_in(dir.path())
            .iter()
            .map(|g| (g.rotated_at_ms, g.ledger.sessions.len()))
            .collect();
        assert_eq!(counts, vec![(3_000, 1), (2_000, 65)]);
    }

    /// Each retained generation is diffed on its own, against the same ids
    /// that are back now — never unioned.
    #[test]
    fn each_generation_reports_its_own_diff_with_its_boot_time() {
        let mut older = ledger(vec![
            entry("a", Some(true), Some("C:/claude/.claude-x")),
            entry("b", Some(true), Some("C:/claude/.claude-x")),
        ]);
        older.boot_at_ms = Some(10);
        let g = StoredGeneration {
            path: PathBuf::from("/x/session-ledger.2000.json"),
            rotated_at_ms: 2_000,
            ledger: older,
        };
        let r = generation_report(&g, &obs(&["a"]), &none(), true);
        assert_eq!(r.file, "session-ledger.2000.json");
        assert_eq!(r.rotated_at_ms, 2_000);
        assert_eq!(r.boot_at_ms, Some(10));
        assert_eq!(r.session_count, 2);
        assert_eq!(r.returned.len(), 1);
        assert_eq!(r.missing.len(), 1);
        assert_eq!(r.verdict, VERDICT_PARTIAL);
    }

    #[test]
    fn generation_file_names_parse_strictly() {
        assert_eq!(
            parse_generation_file_name("session-ledger.1700000000000.json"),
            Some(1_700_000_000_000)
        );
        assert_eq!(parse_generation_file_name("session-ledger.json"), None);
        assert_eq!(parse_generation_file_name("session-ledger.-5.json"), None);
        assert_eq!(
            parse_generation_file_name("session-ledger.12.json.tmp.1"),
            None
        );
        assert_eq!(generation_file_name(42), "session-ledger.42.json");
    }

    #[derive(Debug)]
    struct NoTranscripts;
    impl TranscriptProbe for NoTranscripts {
        fn transcript_exists(&self, _session_id: &str, _working_dir: Option<&str>) -> bool {
            false
        }
    }

    fn store_rec(id: &str) -> crate::session::session_lifecycle_store::TerminalSessionRecord {
        crate::session::session_lifecycle_store::TerminalSessionRecord {
            claude_session_id: id.to_string(),
            config_dir: Some("C:/claude/.claude-x".to_string()),
            working_dir: Some("C:/repo".to_string()),
            page_id: "default".to_string(),
            zone_index: 0,
            title: Some("claude".to_string()),
            terminal_id: format!("term-{id}"),
            opened_at: 0,
            last_seen_at: 0,
            state: "open".to_string(),
            closed_at: None,
            close_reason: None,
            provider: "claude".to_string(),
            origin: None,
            restore_pending_at: None,
            awaiting_account_since: None,
            confirmed_at: None,
            handle: None,
            account_label: Some("x".to_string()),
            account_wrapper: None,
            session_name: Some(format!("name-{id}")),
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
        }
    }

    /// A graceful stop closes every PTY `pty-exit`. A capture taken then must
    /// still list the whole cohort — `open_records()` alone read EMPTY here,
    /// which is how the roster shrank during shutdown while the next boot still
    /// restored those rows. A finished row stays on the roster, flagged.
    #[test]
    fn a_store_whose_every_row_closed_pty_exit_captures_a_non_empty_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap();
        for id in ["g1", "g2", "g3"] {
            store.record_open(store_rec(id));
        }
        store
            .set_finished("g3", true, None)
            .expect("a known session");
        for id in ["g1", "g2", "g3"] {
            store.record_close(id, "pty-exit");
        }
        assert!(store.open_records().is_empty());

        let led = capture(&store, &NoTranscripts, REASON_POLL, 5_000, None);
        let ids: Vec<&str> = led
            .sessions
            .iter()
            .map(|e| e.claude_session_id.as_str())
            .collect();
        assert_eq!(ids, vec!["g1", "g2", "g3"]);
        assert!(
            led.sessions
                .iter()
                .find(|e| e.claude_session_id == "g3")
                .unwrap()
                .finished
        );
        assert!(!led.sessions[0].finished);
        assert_eq!(led.sessions[0].provider.as_deref(), Some("claude"));
        assert!(led.sessions[0].last_seen_at.is_some());
        // …and its unfinished part is exactly the restore set.
        let mut restore: Vec<String> = store
            .restorable_records(None, false)
            .into_iter()
            .map(|r| r.claude_session_id)
            .collect();
        restore.sort();
        assert_eq!(restore, vec!["g1".to_string(), "g2".to_string()]);
    }

    /// A Finish must reach disk on the next tick: `finished` is in the change
    /// key, `last_seen_at` is not.
    #[test]
    fn the_fingerprint_moves_on_finished_but_not_on_last_seen() {
        let a = ledger(vec![entry("s1", Some(true), Some("C:/claude/.claude-x"))]);
        let mut b = a.clone();
        b.sessions[0].last_seen_at = Some(123_456);
        assert_eq!(a.fingerprint(), b.fingerprint());
        let mut c = a.clone();
        c.sessions[0].finished = true;
        assert_ne!(a.fingerprint(), c.fingerprint());
    }

    /// The picker shows the WIP chip and the plan, and a restore lands on the
    /// page/zone — a change to any of them must reach disk too.
    #[test]
    fn the_fingerprint_moves_on_wip_plan_page_and_zone() {
        let a = ledger(vec![entry("s1", Some(true), Some("C:/claude/.claude-x"))]);
        let edits: [fn(&mut LedgerEntry); 4] = [
            |e| e.wip_state = Some("deferred".to_string()),
            |e| e.plan_slug = Some("another-plan".to_string()),
            |e| e.page_id = "page-2".to_string(),
            |e| e.zone_index = 3,
        ];
        for edit in edits {
            let mut b = a.clone();
            edit(&mut b.sessions[0]);
            assert_ne!(a.fingerprint(), b.fingerprint());
        }
    }

    /// A clock stepped BACKWARDS gives the new generation the oldest stamp;
    /// the prune must still never delete the generation it just wrote.
    #[test]
    fn rotation_never_prunes_the_generation_just_written() {
        let dir = tempfile::tempdir().unwrap();
        for boot in 1..=LEDGER_GENERATIONS_KEPT {
            let live = write_live(dir.path(), &numbered(boot));
            assert!(rotate_in(&live, 10_000 + boot as i64, LEDGER_GENERATIONS_KEPT).is_some());
        }
        let live = write_live(dir.path(), &numbered(99));
        let written = rotate_in(&live, 5, LEDGER_GENERATIONS_KEPT).unwrap();
        assert!(written.exists(), "the generation just written survives");
        assert_eq!(
            generation_files_in(dir.path()).len(),
            LEDGER_GENERATIONS_KEPT
        );
        assert!(
            !dir.path().join("session-ledger.10001.json").exists(),
            "the oldest OTHER generation is the one pruned"
        );
    }

    /// A clock stepped BACKWARDS: this boot's generation carries the OLDEST
    /// stamp, yet the report serves it first and flagged, so the panel's
    /// default view is this boot's prior roster, not the previous boot's.
    #[test]
    fn this_boots_generation_is_served_first_under_a_backwards_clock() {
        let dir = tempfile::tempdir().unwrap();
        for boot in 1..=2_usize {
            let live = write_live(dir.path(), &numbered(boot));
            assert!(rotate_in(&live, 10_000 + boot as i64, LEDGER_GENERATIONS_KEPT).is_some());
        }
        let live = write_live(dir.path(), &numbered(3));
        let this_boot = rotate_in(&live, 5, LEDGER_GENERATIONS_KEPT).unwrap();
        let stored = generations_in(dir.path());
        assert_eq!(stored[0].rotated_at_ms, 10_002, "stamp order puts it last");

        let gens = generation_reports(&stored, Some(&this_boot), &obs(&[]), &none(), true);
        let order: Vec<(&str, bool)> = gens
            .iter()
            .map(|g| (g.file.as_str(), g.this_boot))
            .collect();
        assert_eq!(
            order,
            vec![
                ("session-ledger.5.json", true),
                ("session-ledger.10002.json", false),
                ("session-ledger.10001.json", false),
            ]
        );
        // Unknown this-boot generation: plain stamp order, nothing flagged.
        let gens = generation_reports(&stored, None, &obs(&[]), &none(), true);
        assert!(gens.iter().all(|g| !g.this_boot));
        assert_eq!(gens[0].file, "session-ledger.10002.json");
    }

    /// Every id in `ids` observed [`Observed::Back`].
    fn obs(ids: &[&str]) -> Observations {
        ids.iter()
            .map(|id| (id.to_ascii_lowercase(), Observed::Back))
            .collect()
    }

    /// A [`LiveView`] whose live terminals are exactly `panes`, each with its
    /// injected process read — no `/proc` involved.
    fn view(panes: Vec<(&str, PaneRead)>) -> LiveView {
        LiveView {
            terminals: panes.iter().map(|(t, _)| t.to_string()).collect(),
            panes: panes.into_iter().map(|(t, r)| (t.to_string(), r)).collect(),
        }
    }

    /// A `claude --resume <id>` process as the pane's read shows it.
    fn resumed_proc(id: &str) -> crate::session::claude_process_read::ClaudeProc {
        crate::session::claude_process_read::ClaudeProc {
            pid: 4242,
            cmdline: format!("/usr/bin/claude --resume {id}"),
            cwd: None,
            config_dir: None,
        }
    }

    const U1: &str = "0b9f6c1e-1111-4222-8333-444455550001";
    const U2: &str = "0b9f6c1e-1111-4222-8333-444455550002";
    const U3: &str = "0b9f6c1e-1111-4222-8333-444455550003";

    fn boot_a_minute_ago() -> i64 {
        chrono::Utc::now().timestamp_millis() - 60_000
    }

    fn fresh_store() -> (tempfile::TempDir, SessionLifecycleStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap();
        (dir, store)
    }

    /// The ids [`observe_returns`] counts back this boot, sorted.
    fn back_ids(store: &SessionLifecycleStore, boot: i64, live: &LiveView) -> Vec<String> {
        let mut ids: Vec<String> = observe_returns(store, Some(boot), Some(live))
            .into_iter()
            .filter(|(_, o)| *o == Observed::Back)
            .map(|(id, _)| id)
            .collect();
        ids.sort();
        ids
    }

    /// D2: an attempt stamp is not a return. The frontend stamps
    /// `restored_from_boot_at` with tier `failed` the moment it ATTEMPTS a
    /// resume (`mark_restore_pending{boot:true}`); on 2026-11-04 103 rows sat
    /// there with no `claude` ever typed, and the ledger read them back.
    #[test]
    fn a_stamp_only_row_is_not_back() {
        let (_dir, store) = fresh_store();
        let boot = boot_a_minute_ago();
        store.record_open(store_rec("stamped"));
        store.mark_restore_pending("stamped", true);
        // Its pre-restart terminal is gone: nothing live binds it.
        let live = view(Vec::new());
        assert_eq!(back_ids(&store, boot, &live), Vec::<String>::new());
        assert_eq!(
            observe_returns(&store, Some(boot), Some(&live)).get("stamped"),
            Some(&Observed::AttemptedUnverified)
        );
    }

    /// D2: a live terminal is not a live conversation. A pane the restore
    /// created as a plain shell is alive and bound to the row, and no `claude`
    /// runs in it.
    #[test]
    fn an_open_row_bound_to_a_live_bare_shell_is_not_back() {
        let (_dir, store) = fresh_store();
        let boot = boot_a_minute_ago();
        store.record_open(store_rec("shell"));
        let live = view(vec![("term-shell", Ok(Vec::new()))]);
        assert_eq!(back_ids(&store, boot, &live), Vec::<String>::new());
        assert_eq!(
            observe_returns(&store, Some(boot), Some(&live)).get("shell"),
            Some(&Observed::TerminalOnly)
        );
    }

    /// D2: back is a process read, or a restore that verified (`resumed`).
    #[test]
    fn a_verified_resume_is_back_by_process_read_or_by_a_resumed_tier() {
        let (_dir, store) = fresh_store();
        let boot = boot_a_minute_ago();
        for id in [U1, U2] {
            store.record_open(store_rec(id));
        }
        // U1: its live pane runs `claude --resume U1`, with no stamp at all.
        // U2: its restore verified this boot; its pane is not live (it may
        // have been closed by the operator since — it did come back).
        store.mark_restored_from_boot(U2, RESTORE_TIER_RESUMED);
        let live = view(vec![(
            format!("term-{U1}").as_str(),
            Ok(vec![resumed_proc(U1)]),
        )]);
        assert_eq!(
            back_ids(&store, boot, &live),
            vec![U1.to_string(), U2.to_string()]
        );
    }

    /// A `claude` for a DIFFERENT session in the pane is not this session —
    /// and not a bare terminal either: a claude runs there, unverified.
    #[test]
    fn a_pane_running_another_sessions_claude_is_not_back() {
        let (_dir, store) = fresh_store();
        let boot = boot_a_minute_ago();
        store.record_open(store_rec(U1));
        let live = view(vec![(
            format!("term-{U1}").as_str(),
            Ok(vec![resumed_proc(U2)]),
        )]);
        assert_eq!(
            observe_returns(&store, Some(boot), Some(&live)).get(U1),
            Some(&Observed::AttemptedUnverified)
        );
        // A claude whose command line names no session (`--continue`).
        let mut bare = resumed_proc(U1);
        bare.cmdline = "/usr/bin/claude --continue".to_string();
        let live = view(vec![(format!("term-{U1}").as_str(), Ok(vec![bare]))]);
        assert_eq!(
            observe_returns(&store, Some(boot), Some(&live)).get(U1),
            Some(&Observed::AttemptedUnverified)
        );
    }

    /// `judge` rejecting the claude it found (here its cwd is not the row's)
    /// is an unverified attempt, never back. The same claude in the row's
    /// own cwd is back, so the comparison really ran.
    #[test]
    fn a_claude_in_the_wrong_cwd_is_attempted_unverified() {
        let (_dir, store) = fresh_store();
        let boot = boot_a_minute_ago();
        store.record_open(store_rec(U1));
        let in_dir = |cwd: &str| {
            let mut p = resumed_proc(U1);
            p.cwd = Some(cwd.to_string());
            view(vec![(format!("term-{U1}").as_str(), Ok(vec![p]))])
        };
        let elsewhere = in_dir("/elsewhere");
        assert_eq!(
            observe_returns(&store, Some(boot), Some(&elsewhere)).get(U1),
            Some(&Observed::AttemptedUnverified)
        );
        // `store_rec` records working dir `C:/repo`.
        let home = in_dir("C:/repo");
        assert_eq!(
            observe_returns(&store, Some(boot), Some(&home)).get(U1),
            Some(&Observed::Back)
        );
    }

    /// No live view (the terminal manager is not in app state): only this
    /// boot's stamps speak, and an open unstamped row is not observed at all.
    #[test]
    fn without_a_live_view_only_this_boots_stamps_are_observed() {
        let (_dir, store) = fresh_store();
        let boot = boot_a_minute_ago();
        for id in ["resumed", "pending", "open-unstamped"] {
            store.record_open(store_rec(id));
        }
        store.mark_restored_from_boot("resumed", RESTORE_TIER_RESUMED);
        store.mark_restore_pending("pending", true);
        let seen = observe_returns(&store, Some(boot), None);
        assert_eq!(seen.get("resumed"), Some(&Observed::Back));
        assert_eq!(seen.get("pending"), Some(&Observed::AttemptedUnverified));
        assert_eq!(seen.get("open-unstamped"), None);
        assert_eq!(seen.len(), 2);
    }

    /// A CLOSED row this boot stamped but never verified is not a pending
    /// outcome: nothing holds it and the door can take it, so it reads as
    /// `missing`. A closed row whose restore verified did come back.
    #[test]
    fn a_closed_unverified_row_is_missing_and_a_closed_verified_one_is_back() {
        let (_dir, store) = fresh_store();
        let boot = boot_a_minute_ago();
        for (id, tier) in [
            ("closed-shell", RESTORE_TIER_TERMINAL_ONLY),
            ("closed-back", RESTORE_TIER_RESUMED),
        ] {
            store.record_open(store_rec(id));
            store.mark_restored_from_boot(id, tier);
            store.record_close(id, "pty-exit");
            assert_eq!(store.get(id).unwrap().state, "closed");
        }
        let seen = observe_returns(&store, Some(boot), Some(&view(Vec::new())));
        assert_eq!(seen.get("closed-shell"), None);
        assert_eq!(seen.get("closed-back"), Some(&Observed::Back));
    }

    /// One process-table read serves requests for [`LIVE_VIEW_TTL`], then
    /// is read again.
    #[test]
    fn the_live_view_memo_serves_within_its_ttl_only() {
        let at = std::time::Instant::now();
        let memo = Some((at, view(vec![("t", Ok(Vec::new()))])));
        assert!(memo_fresh(&memo, at).is_some());
        assert!(memo_fresh(
            &memo,
            at + LIVE_VIEW_TTL - std::time::Duration::from_millis(1)
        )
        .is_some());
        assert!(memo_fresh(&memo, at + LIVE_VIEW_TTL).is_none());
        assert!(memo_fresh(&None, at).is_none());
    }

    /// D2: an unreadable process table is UNKNOWN, never back — and the
    /// verdict says `unknown` with the reason, not `match` or `mismatch`.
    #[test]
    fn an_unreadable_process_read_is_unknown_never_back() {
        use crate::session::claude_process_read::PaneReadError;
        let (_dir, store) = fresh_store();
        let boot = boot_a_minute_ago();
        store.record_open(store_rec(U1));
        store.record_open(store_rec(U2));
        let live = view(vec![
            (
                format!("term-{U1}").as_str(),
                Err(PaneReadError::TableUnreadable),
            ),
            (
                format!("term-{U2}").as_str(),
                Err(PaneReadError::NoChildPid),
            ),
        ]);
        let seen = observe_returns(&store, Some(boot), Some(&live));
        assert_eq!(seen.get(U1), Some(&Observed::Unknown));
        assert_eq!(seen.get(U2), Some(&Observed::Unknown));
        // A live terminal the read has no entry for is unknown too.
        let mut gap = view(Vec::new());
        gap.terminals.insert(format!("term-{U1}"));
        assert_eq!(
            observe_returns(&store, Some(boot), Some(&gap)).get(U1),
            Some(&Observed::Unknown)
        );

        let prior = ledger(vec![
            entry(U1, Some(true), None),
            entry(U2, Some(true), None),
        ]);
        let report = diff(Some(&prior), &seen, &none(), ledger(Vec::new()), true);
        assert!(report.returned.is_empty());
        assert_eq!(report.unknown.len(), 2);
        assert_eq!(report.unknown[0].outcome, OUTCOME_UNKNOWN);
        assert_eq!(report.verdict, VERDICT_UNKNOWN);
        assert_eq!(report.reason.as_deref(), Some("process_read_unavailable"));
    }

    /// The stamp arm: what this boot's restore stamp says, when no live pane
    /// can say more. A stamp from a previous boot says nothing.
    #[test]
    fn each_stamp_gets_its_own_outcome_and_an_old_stamp_none() {
        let (_dir, store) = fresh_store();
        let boot = boot_a_minute_ago();
        for id in [
            "pending",
            "failed",
            "terminal",
            "old-stamp",
            "needs-account",
        ] {
            store.record_open(store_rec(id));
        }
        store.mark_restore_pending("pending", true);
        store.mark_restored_from_boot("failed", RESTORE_TIER_FAILED);
        store.mark_restored_from_boot("terminal", RESTORE_TIER_TERMINAL_ONLY);
        let mut old = store_rec("old-stamp");
        old.restored_from_boot_at = Some(boot - 1);
        old.restore_tier = Some(RESTORE_TIER_RESUMED.to_string());
        store.record_open(old);

        let seen = observe_returns(&store, Some(boot), Some(&view(Vec::new())));
        assert_eq!(seen.get("pending"), Some(&Observed::AttemptedUnverified));
        assert_eq!(seen.get("failed"), Some(&Observed::Failed));
        assert_eq!(seen.get("terminal"), Some(&Observed::TerminalOnly));
        assert_eq!(seen.get("old-stamp"), None);
        assert_eq!(seen.get("needs-account"), None);
        // No boot instant: the stamp arm is skipped entirely.
        assert!(observe_returns(&store, None, Some(&view(Vec::new()))).is_empty());
    }

    /// 0, 1 and many rows: the buckets, and `match` only when `returned` is
    /// the whole expected set.
    #[test]
    fn the_verdict_is_match_only_when_every_expected_session_is_verified_back() {
        let seen = |pairs: &[(&str, Observed)]| -> Observations {
            pairs.iter().map(|(id, o)| (id.to_string(), *o)).collect()
        };
        let e = |id: &str| entry(id, Some(true), None);

        // 0 rows: an empty prior is a real match.
        let zero = diff(
            Some(&ledger(Vec::new())),
            &seen(&[]),
            &none(),
            ledger(Vec::new()),
            true,
        );
        assert_eq!(zero.verdict, VERDICT_MATCH);

        // 1 row, verified back: match. Attempted only: mismatch, never match.
        let one = ledger(vec![e("a")]);
        let back = diff(
            Some(&one),
            &seen(&[("a", Observed::Back)]),
            &none(),
            ledger(Vec::new()),
            true,
        );
        assert_eq!(
            (back.verdict.as_str(), back.returned.len()),
            (VERDICT_MATCH, 1)
        );
        let tried = diff(
            Some(&one),
            &seen(&[("a", Observed::AttemptedUnverified)]),
            &none(),
            ledger(Vec::new()),
            true,
        );
        assert_eq!(tried.verdict, VERDICT_MISMATCH);
        assert!(tried.returned.is_empty());
        assert_eq!(
            tried.attempted_unverified[0].outcome,
            OUTCOME_ATTEMPTED_UNVERIFIED
        );

        // Many rows: each outcome in its own bucket, partial.
        let many = ledger(vec![e("a"), e("u"), e("t"), e("f"), e("m")]);
        let r = diff(
            Some(&many),
            &seen(&[
                ("a", Observed::Back),
                ("u", Observed::AttemptedUnverified),
                ("t", Observed::TerminalOnly),
                ("f", Observed::Failed),
            ]),
            &none(),
            ledger(Vec::new()),
            true,
        );
        let ids = |v: &[LedgerOutcome]| -> Vec<String> {
            v.iter().map(|o| o.claude_session_id.clone()).collect()
        };
        assert_eq!(ids(&r.returned), ["a"]);
        assert_eq!(ids(&r.attempted_unverified), ["u"]);
        assert_eq!(ids(&r.terminal_only), ["t"]);
        assert_eq!(ids(&r.failed), ["f"]);
        assert_eq!(ids(&r.missing), ["m"]);
        assert_eq!(r.terminal_only[0].outcome, OUTCOME_TERMINAL_ONLY);
        assert_eq!(r.failed[0].outcome, OUTCOME_FAILED);
        assert_eq!(r.verdict, VERDICT_PARTIAL);
        assert!(r.note.starts_with("1 of 5 sessions"), "{}", r.note);
    }

    /// A pane this boot ADOPTED (a holder that survived the restart, its
    /// `claude` still running) carries no restore stamp. The ledger and the
    /// restore census judge it by one predicate, so both count it back
    /// (Phase 0 run4: the ledger said 12 returned, the census 0 restored).
    #[test]
    fn an_adopted_verified_pane_is_back_on_the_ledger_and_restored_on_the_census() {
        use crate::session::restore_census::{
            diff_census, observe_restored, CensusExpected, RESTORE_VERDICT_MATCH,
        };
        let (_dir, store) = fresh_store();
        let boot = boot_a_minute_ago();
        store.record_open(store_rec(U1));
        let live = view(vec![(
            format!("term-{U1}").as_str(),
            Ok(vec![resumed_proc(U1)]),
        )]);

        let seen = observe_returns(&store, Some(boot), Some(&live));
        let prior = ledger(vec![entry(U1, Some(true), None)]);
        let report = diff(Some(&prior), &seen, &none(), ledger(Vec::new()), true);
        assert_eq!(report.returned.len(), 1);
        assert_eq!(report.verdict, VERDICT_MATCH);

        let expected = [CensusExpected {
            claude_session_id: U1.to_string(),
            terminal_id: Some(format!("term-{U1}")),
            page_id: "default".to_string(),
            zone_index: 0,
            account_label: None,
            session_name: None,
            restorable: true,
        }];
        let observed = observe_restored(&store, boot, Some(&live));
        let census = diff_census(&expected, &observed, Some(true), true);
        assert_eq!(census.restored.len(), 1, "{census:?}");
        assert!(census.missing.is_empty());
        assert_eq!(census.verdict, RESTORE_VERDICT_MATCH);

        // The same pane as a bare shell: neither surface counts it.
        let shell = view(vec![(format!("term-{U1}").as_str(), Ok(Vec::new()))]);
        assert!(observe_restored(&store, boot, Some(&shell)).is_empty());

        // The same pane unreadable: the census sees it, and cannot say.
        let unread = view(vec![(
            format!("term-{U1}").as_str(),
            Err(crate::session::claude_process_read::PaneReadError::CmdlineUnreadable),
        )]);
        let observed = observe_restored(&store, boot, Some(&unread));
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].observed, Some(Observed::Unknown));
        let census = diff_census(&expected, &observed, Some(true), true);
        assert_eq!(
            census.verdict,
            crate::session::restore_census::RESTORE_VERDICT_UNKNOWN
        );
        let seen = observe_returns(&store, Some(boot), Some(&shell));
        assert!(diff(Some(&prior), &seen, &none(), ledger(Vec::new()), true)
            .returned
            .is_empty());
    }

    /// The same session seen twice — on two registry rows, or listed twice in
    /// the prior ledger — counts once, with its strongest observation.
    #[test]
    fn the_same_session_seen_twice_counts_once() {
        let (_dir, store) = fresh_store();
        let boot = boot_a_minute_ago();
        // Two registry rows for one session (the ids differ only in case).
        store.record_open(store_rec(U3));
        let mut upper = store_rec(&U3.to_ascii_uppercase());
        upper.terminal_id = "term-upper".to_string();
        store.record_open(upper);
        let live = view(vec![
            (format!("term-{U3}").as_str(), Ok(vec![resumed_proc(U3)])),
            ("term-upper", Ok(Vec::new())),
        ]);
        let seen = observe_returns(&store, Some(boot), Some(&live));
        assert_eq!(seen.len(), 1);
        assert_eq!(seen.get(U3), Some(&Observed::Back));

        let prior = ledger(vec![
            entry(U3, Some(true), None),
            entry(U3, Some(true), None),
        ]);
        let report = diff(Some(&prior), &seen, &none(), ledger(Vec::new()), true);
        assert_eq!(report.returned.len(), 1);
        assert_eq!(report.expected.len(), 1, "expected agrees with the counts");
        assert_eq!(report.verdict, VERDICT_MATCH);
        let generation = generation_report(
            &StoredGeneration {
                path: PathBuf::from("session-ledger.1.json"),
                rotated_at_ms: 1,
                ledger: prior.clone(),
            },
            &seen,
            &none(),
            true,
        );
        assert_eq!(generation.session_count, 1);
        assert!(
            report.note.starts_with("1 of 1 sessions"),
            "{}",
            report.note
        );
    }

    /// Only a terminal whose PTY process is alive binds an `open` row as back:
    /// an exited PTY the manager still lists, or one whose pid is gone, is not.
    #[test]
    fn live_terminal_ids_count_only_terminals_whose_pty_is_alive() {
        let info =
            |id: &str, pid: Option<u32>, is_alive: bool| crate::terminal::types::TerminalInfo {
                id: id.to_string(),
                title: id.to_string(),
                pid,
                cols: 80,
                rows: 24,
                working_dir: "/w".to_string(),
                is_alive,
                exit_code: None,
                created_at: 0,
                total_bytes_produced: 0,
                page_id: "default".to_string(),
            };
        let ids = alive_terminal_ids(
            vec![
                info("live", Some(10), true),
                info("exited", Some(11), false),
                info("pid-gone", Some(12), true),
                info("no-pid", None, true),
            ],
            |pid| pid != 12,
        );
        let mut ids: Vec<String> = ids.into_iter().collect();
        ids.sort();
        assert_eq!(ids, vec!["live".to_string(), "no-pid".to_string()]);
    }

    /// The serialized key set of every type is pinned against the golden the
    /// hand-written TS types (`src/lib/session-ledger.ts`) are ALSO tested
    /// against, so the two sides cannot drift apart silently.
    #[test]
    fn serialized_keys_match_the_typescript_golden() {
        let golden: serde_json::Value = serde_json::from_str(include_str!(
            "../../../src/lib/__golden__/session-ledger-keys.json"
        ))
        .unwrap();
        let want = |ty: &str| -> Vec<String> {
            let mut v: Vec<String> = golden[ty]
                .as_array()
                .unwrap_or_else(|| panic!("golden lacks {ty}"))
                .iter()
                .map(|k| k.as_str().unwrap().to_string())
                .collect();
            v.sort();
            v
        };
        let keys = |v: &serde_json::Value| -> Vec<String> {
            let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
            k.sort();
            k
        };

        let mut done = entry("done", Some(true), Some("C:/claude/.claude-x"));
        done.finished = true;
        let prior = ledger(vec![
            entry("back", Some(true), Some("C:/claude/.claude-x")),
            entry("gone", Some(true), None),
            done,
        ]);
        let mut report = diff(
            Some(&prior),
            &obs(&["back"]),
            &none(),
            ledger(vec![entry("back", Some(true), None)]),
            true,
        );
        report.generations = vec![generation_report(
            &StoredGeneration {
                path: PathBuf::from("session-ledger.1.json"),
                rotated_at_ms: 1,
                ledger: prior.clone(),
            },
            &obs(&["back"]),
            &none(),
            true,
        )];
        let capture = LedgerCapture {
            persisted: true,
            path: "x".to_string(),
            ledger: prior.clone(),
        };

        let r = serde_json::to_value(&report).unwrap();
        assert_eq!(keys(&r), want("LedgerReport"));
        assert_eq!(keys(&r["missing"][0]), want("LedgerOutcome"));
        assert_eq!(
            keys(&r["missing"][0]["resumeAccount"]),
            want("ResumeAccount")
        );
        assert_eq!(keys(&r["finished"][0]), want("LedgerOutcome"));
        assert_eq!(keys(&r["returned"][0]), want("LedgerOutcome"));
        assert_eq!(keys(&r["generations"][0]), want("LedgerGeneration"));
        assert_eq!(keys(&r["current"]), want("SessionLedger"));
        assert_eq!(keys(&r["expected"][0]), want("LedgerEntry"));
        let c = serde_json::to_value(&capture).unwrap();
        assert_eq!(keys(&c), want("LedgerCapture"));
    }

    /// The typed-resume account: unknown stays unknown, the default home types
    /// no `CLAUDE_CONFIG_DIR`, anything else types itself (forward-slashed).
    #[test]
    fn resume_account_for_names_unknown_default_and_explicit_accounts() {
        assert_eq!(
            resume_account_for(None),
            ResumeAccount {
                known: false,
                config_dir: None
            }
        );
        assert!(!resume_account_for(Some("  ")).known);
        if let Some(home) = dirs::home_dir() {
            let default_home = home.join(".claude");
            assert_eq!(
                resume_account_for(default_home.to_str()),
                ResumeAccount {
                    known: true,
                    config_dir: None
                },
                "the default home resumes with no CLAUDE_CONFIG_DIR"
            );
        }
        assert_eq!(
            resume_account_for(Some("C:\\claude\\.claude-paktis")),
            ResumeAccount {
                known: true,
                config_dir: Some("C:/claude/.claude-paktis".to_string())
            }
        );
    }

    /// An entry with no RECORDED config dir still resumes under the account
    /// resolved at capture from transcript evidence — in its copy line AND in
    /// the typed-resume account — and an entry with neither stays unknown.
    #[test]
    fn outcomes_use_the_capture_time_resolved_account() {
        let mut located = entry("loc", Some(true), None);
        located.resume_config_dir = Some("C:/claude/.claude-extra".to_string());
        let unknown = entry("unk", Some(true), None);
        let report = diff(
            Some(&ledger(vec![located, unknown])),
            &obs(&[]),
            &none(),
            ledger(vec![]),
            true,
        );
        let by_id = |id: &str| {
            report
                .missing
                .iter()
                .find(|m| m.claude_session_id == id)
                .unwrap()
                .clone()
        };
        let loc = by_id("loc");
        assert!(!loc.config_dir_known, "nothing was RECORDED");
        assert_eq!(
            loc.resume_account,
            ResumeAccount {
                known: true,
                config_dir: Some("C:/claude/.claude-extra".to_string())
            }
        );
        assert!(loc
            .resume_command
            .as_deref()
            .is_some_and(|c| c.contains("CLAUDE_CONFIG_DIR=\"C:/claude/.claude-extra\"")));
        let unk = by_id("unk");
        assert!(!unk.resume_account.known);
        assert_eq!(unk.resume_command, None);
    }

    /// The saved-roster stamp is this process's write only: the previous
    /// boot's live file (a different `boot_at_ms`) is never reported as saved.
    #[test]
    fn saved_by_this_process_ignores_the_previous_boots_file() {
        let dir = std::env::temp_dir().join(format!(
            "session-ledger-saved-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session-ledger.json");
        assert!(saved_by_this_process(&path, 1_000).is_none(), "no file");
        let mut led = ledger(vec![entry("s1", Some(true), Some("C:/claude/.claude-x"))]);
        led.boot_at_ms = Some(1_000);
        std::fs::write(&path, serde_json::to_vec(&led).unwrap()).unwrap();
        assert_eq!(
            saved_by_this_process(&path, 1_000).map(|l| l.captured_at_ms),
            Some(led.captured_at_ms)
        );
        assert!(
            saved_by_this_process(&path, 2_000).is_none(),
            "another boot's file is not this process's save"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
