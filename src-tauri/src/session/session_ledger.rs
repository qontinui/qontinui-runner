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

use crate::session::session_lifecycle_store::{SessionLifecycleStore, NAME_SOURCE_DERIVED};
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
/// NOW) and it is not expected back either.
pub const OUTCOME_BACK: &str = "back";
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
    /// [`OUTCOME_BACK`] | [`OUTCOME_MISSING`] | [`OUTCOME_FINISHED`].
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
    /// Of those, the ones open (or restored) again now.
    pub returned: Vec<LedgerOutcome>,
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

/// One generation's buckets and verdict.
struct Classified {
    returned: Vec<LedgerOutcome>,
    missing: Vec<LedgerOutcome>,
    finished: Vec<LedgerOutcome>,
    closed_by_user: Vec<LedgerOutcome>,
    verdict: &'static str,
}

/// PURE classification of one prior ledger against the ids back this boot.
///
/// Back wins (it came back, finished or not); otherwise a FINISHED session —
/// as of now when the registry knows it, else as captured — is its own class
/// and never `missing`; so is one the operator has closed on purpose since
/// (`closed-by-user`); everything else is `missing` with a reason.
fn classify(
    prior: &SessionLedger,
    back: &[String],
    now: &RegistryNow,
    restore_stamps_available: bool,
) -> Classified {
    let back_lower: Vec<String> = back.iter().map(|s| s.to_ascii_lowercase()).collect();
    let mut returned = Vec::new();
    let mut missing = Vec::new();
    let mut finished = Vec::new();
    let mut closed_by_user = Vec::new();
    for e in &prior.sessions {
        let id = e.claude_session_id.to_ascii_lowercase();
        let is_finished = now.finished.get(&id).copied().unwrap_or(e.finished);
        if back_lower.contains(&id) {
            returned.push(outcome_of(e, OUTCOME_BACK, None, is_finished));
        } else if is_finished {
            finished.push(outcome_of(e, OUTCOME_FINISHED, None, true));
        } else if now.closed_by_user.contains(&id) {
            closed_by_user.push(outcome_of(e, OUTCOME_CLOSED_BY_USER, None, false));
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
            missing.push(outcome_of(e, OUTCOME_MISSING, Some(reason), false));
        }
    }

    // An input we could not read must not be laundered into a confident miss
    // count. `observed_back` skips the sticky-restore-stamp arm when this boot
    // never latched a census, so a session that came back and was then closed
    // lands in `missing` — through no fault of the rebuild.
    //
    // An empty prior (or one whose every session is finished) is a real
    // `match` — only stateable because a ledger EXISTS saying so, which is the
    // difference from the `no_prior_ledger` arm of [`diff`].
    let verdict = if !restore_stamps_available && !missing.is_empty() {
        VERDICT_UNKNOWN
    } else if missing.is_empty() {
        VERDICT_MATCH
    } else if returned.is_empty() {
        VERDICT_MISMATCH
    } else {
        VERDICT_PARTIAL
    };
    Classified {
        returned,
        missing,
        finished,
        closed_by_user,
        verdict,
    }
}

/// PURE diff: the prior ledger vs the ids observed back this boot.
///
/// Split from the route so the verdict and the resume lines are testable
/// without a store, a disk or a rebuild. `generations` is left empty; the
/// [`report`] builder fills it.
pub fn diff(
    prior: Option<&SessionLedger>,
    back: &[String],
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

    let Classified {
        returned,
        missing,
        finished,
        closed_by_user,
        verdict,
    } = classify(prior, back, now, restore_stamps_available);

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
        "{} of {} sessions open before the last shutdown came back. {} did not; {} of those \
         name a worktree that may hold uncommitted work, and {} could not be given a resume \
         line because the account root they ran under is unknown (a --resume under the wrong \
         CLAUDE_CONFIG_DIR fails as though the session never existed). {} more were marked \
         finished and {} were closed by the operator since; neither is expected back.",
        returned.len(),
        returned.len() + missing.len(),
        missing.len(),
        with_wip,
        unresumable,
        finished.len(),
        closed_by_user.len()
    );
    if !restore_stamps_available {
        note.push_str(
            " CAVEAT: this boot never latched a restore census, so a session that came back \
             and was then CLOSED cannot be distinguished from one that never returned. The \
             miss count is an UPPER BOUND, which is why the verdict reads `unknown`.",
        );
    }

    LedgerReport {
        status: "ok".to_string(),
        reason: (verdict == VERDICT_UNKNOWN).then(|| "restore_stamps_unavailable".to_string()),
        generated_at,
        prior_captured_at: Some(prior.captured_at.clone()),
        prior_reason: Some(prior.reason.clone()),
        expected: prior.sessions.clone(),
        returned,
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
    back: &[String],
    now: &RegistryNow,
    restore_stamps_available: bool,
) -> Vec<LedgerGeneration> {
    let is_this_boot = |g: &StoredGeneration| this_boot.is_some_and(|p| g.path == p);
    stored
        .iter()
        .filter(|g| is_this_boot(g))
        .chain(stored.iter().filter(|g| !is_this_boot(g)))
        .map(|g| {
            let mut r = generation_report(g, back, now, restore_stamps_available);
            r.this_boot = is_this_boot(g);
            r
        })
        .collect()
}

/// One retained generation's own diff (`this_boot` unset — see
/// [`generation_reports`]).
pub fn generation_report(
    generation: &StoredGeneration,
    back: &[String],
    now: &RegistryNow,
    restore_stamps_available: bool,
) -> LedgerGeneration {
    let led = &generation.ledger;
    let Classified {
        returned,
        missing,
        finished,
        closed_by_user,
        verdict,
    } = classify(led, back, now, restore_stamps_available);
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
        session_count: led.sessions.len(),
        verdict: verdict.to_string(),
        returned,
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
/// `live_terminal_ids` is the set of PTYs alive in this process right now (the
/// `TerminalManager` list) — what makes an `open` row count as back (see
/// [`observed_back`]). `None` when the terminal manager is not available: open
/// rows then cannot be confirmed back, and the verdict says `unknown` rather
/// than pretending.
///
/// Read-only apart from latching (and, on a process's first call, rotating)
/// the prior ledger — see [`load_prior_once`].
pub fn report(
    store: Option<&SessionLifecycleStore>,
    live_terminal_ids: Option<&HashSet<String>>,
) -> LedgerReport {
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
        let mut report = diff(prior, &[], &none, empty, false);
        report.status = "unavailable".to_string();
        report.reason = Some("lifecycle_store_unavailable".to_string());
        report.verdict = VERDICT_UNKNOWN.to_string();
        report.generations = generation_reports(&stored, this_boot_generation(), &[], &none, false);
        return report;
    };

    let index = DiskTranscriptIndex::discover();
    let current = capture(store, &index, "read", now, boot);
    // "Came back" = open now, UNION anything stamped restored by THIS boot —
    // a session that returned and was then closed did in fact return.
    // `None` when the boot census never latched — see `observed_back`: an
    // absent boot instant SKIPS the sticky-restore-stamp arm rather than
    // admitting it with a `0` floor, and that is reported (verdict `unknown`
    // on any miss), not laundered into a miss count.
    let boot_at = crate::session::restore_census::latched().map(|c| c.boot_at_ms);
    let back = observed_back(store, boot_at, live_terminal_ids);
    let now_state = registry_now(store);
    // Both inputs `back` rests on must be readable for a miss count to be a
    // claim rather than an upper bound.
    let observable = boot_at.is_some() && live_terminal_ids.is_some();
    let mut report = diff(prior, &back, &now_state, current, observable);
    report.generations = generation_reports(
        &stored,
        this_boot_generation(),
        &back,
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

/// The ids of every PTY alive in this process (the `TerminalManager` list) —
/// [`report`]'s live-binding input. `None` when the manager is not in app
/// state, which [`report`] reads as UNKNOWN, never as "nothing is live".
pub fn live_terminal_ids<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Option<HashSet<String>> {
    use tauri::Manager;
    app.try_state::<std::sync::Arc<crate::terminal::TerminalManager>>()
        .map(|tm| alive_terminal_ids(tm.list(), crate::process_capture::health::pid_alive))
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

/// Every session id observed BACK this boot: a row bound to a terminal that is
/// ALIVE in this process now, plus anything carrying a restore stamp from this
/// boot.
///
/// The stamp arm matters — a session that came back and was then closed by the
/// operator DID come back, and counting only live rows would report a
/// `missing` the rebuild is not guilty of. That is the same reasoning
/// [`crate::session::restore_census::observe_restored`] spells out for reading
/// all records rather than only open ones.
///
/// An `open` row is NOT back by being open. A crash leaves every row of the
/// previous process `open`, so "open" counted the whole cohort back and hid
/// exactly the rows the boot restore did NOT bring back — a `needs-account`
/// row waiting for its account, a skipped one, one on a page not opened yet.
/// Only a live terminal binding (`live_terminal_ids`) or this boot's restore
/// stamp is evidence that it returned.
pub fn observed_back(
    store: &SessionLifecycleStore,
    boot_at_ms: Option<i64>,
    live_terminal_ids: Option<&HashSet<String>>,
) -> Vec<String> {
    // `restored_from_boot_at` is STICKY across restarts by design, so a stamp
    // older than THIS boot belongs to a previous one. Without a boot instant
    // to compare against, the stamp arm is SKIPPED entirely rather than
    // admitted with a `0` floor — a `0` would count every historical restore
    // stamp as "came back", which is the wrong-in-the-operator's-favour
    // direction and exactly the vacuous-`match` failure the census's R3 names.
    let mut ids: Vec<String> = store
        .all_records()
        .into_iter()
        .filter(|r| {
            let live_bound = r.state == "open"
                && live_terminal_ids.is_some_and(|live| live.contains(&r.terminal_id));
            let restored_this_boot =
                boot_at_ms.is_some_and(|boot| r.restored_from_boot_at.is_some_and(|at| at >= boot));
            live_bound || restored_this_boot
        })
        .map(|r| r.claude_session_id)
        .collect();
    ids.sort();
    ids.dedup();
    ids
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
            &["s1".to_string()],
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
        let report = diff(Some(&prior), &[], &none(), ledger(Vec::new()), true);
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
        let report = diff(None, &[], &none(), ledger(Vec::new()), true);
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
            &[],
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
        let report = diff(Some(&prior), &[], &none(), ledger(Vec::new()), true);
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
            &["aaaa-1111".to_string()],
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
        let report = diff(Some(&prior), &[], &none(), ledger(Vec::new()), true);
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
            &[],
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
            &["s1".to_string()],
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
            &["live".to_string()],
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
            &["kept".to_string(), "back-then-closed".to_string()],
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
        let report = diff(Some(&prior), &[], &none(), ledger(Vec::new()), true);
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
        let report = diff(Some(&prior), &[], &now, ledger(Vec::new()), true);
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
            &[],
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
        let r = generation_report(&g, &["a".to_string()], &none(), true);
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
            finish_reason: None,
            finish_synced: false,
            spawn_device_default: None,
            adopted_from: None,
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

        let gens = generation_reports(&stored, Some(&this_boot), &[], &none(), true);
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
        let gens = generation_reports(&stored, None, &[], &none(), true);
        assert!(gens.iter().all(|g| !g.this_boot));
        assert_eq!(gens[0].file, "session-ledger.10002.json");
    }

    /// A crash leaves every row `open`. An open row is back only when it is
    /// bound to a terminal alive NOW or carries this boot's restore stamp — a
    /// `needs-account` row the restore left for the operator is not.
    #[test]
    fn an_open_row_is_back_only_with_a_live_terminal_or_this_boots_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap();
        for id in ["live", "restored", "needs-account", "old-stamp"] {
            store.record_open(store_rec(id));
        }
        store.mark_restored_from_boot("restored", "resumed");
        let boot = chrono::Utc::now().timestamp_millis() - 60_000;
        let mut old = store_rec("old-stamp");
        old.restored_from_boot_at = Some(boot - 1);
        store.record_open(old);
        let live: HashSet<String> = ["term-live".to_string()].into_iter().collect();

        assert_eq!(
            observed_back(&store, Some(boot), Some(&live)),
            vec!["live".to_string(), "restored".to_string()]
        );
        // No live view: only this boot's stamps.
        assert_eq!(
            observed_back(&store, Some(boot), None),
            vec!["restored".to_string()]
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
            &["back".to_string()],
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
            &["back".to_string()],
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
            &[],
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
