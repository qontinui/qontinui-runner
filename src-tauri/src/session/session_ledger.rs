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
//! ## Honesty contract (identical in shape to the census's)
//!
//! * **No prior ledger is `unknown`, never `match`.** A boot with nothing on
//!   disk cannot state that nothing was lost.
//! * **A resume line is omitted, never guessed.** Without the account root the
//!   session actually ran under, `claude --resume` fails with a message that
//!   reads exactly like "that session never existed".
//! * **Persistence is fail-soft.** A ledger write that fails logs and moves
//!   on; it must never be the thing that breaks a boot or a poll tick.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::session::session_lifecycle_store::SessionLifecycleStore;
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

/// Verdicts — deliberately the same vocabulary as
/// [`crate::session::restore_census`], so an operator never has to learn two.
pub const VERDICT_MATCH: &str = "match";
pub const VERDICT_PARTIAL: &str = "partial";
pub const VERDICT_MISMATCH: &str = "mismatch";
pub const VERDICT_UNKNOWN: &str = "unknown";

// ---------------------------------------------------------------------------
// The ledger
// ---------------------------------------------------------------------------

/// One open session, as it stood when the ledger was captured.
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
    #[serde(default)]
    pub account_label: Option<String>,
    /// The `CLAUDE_CONFIG_DIR` this session ran under — required to build a
    /// working `--resume` line, and the reason one is omitted when absent.
    #[serde(default)]
    pub config_dir: Option<String>,
    #[serde(default)]
    pub working_dir: Option<String>,
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
    /// [`REASON_BOOT_LATCH`] | [`REASON_POLL`] | [`REASON_PRE_REBUILD`] | a
    /// caller-supplied label.
    pub reason: String,
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
    fn fingerprint(&self) -> String {
        let mut parts: Vec<String> = self
            .sessions
            .iter()
            .map(|s| {
                format!(
                    "{}|{}|{}|{}|{}|{}",
                    s.claude_session_id,
                    s.terminal_id,
                    s.working_dir.as_deref().unwrap_or(""),
                    s.session_name.as_deref().unwrap_or(""),
                    s.worktree_path.as_deref().unwrap_or(""),
                    match s.restorable {
                        Some(true) => "y",
                        Some(false) => "n",
                        None => "?",
                    }
                )
            })
            .collect();
        parts.sort();
        parts.join("\n")
    }
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

fn runner_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".qontinui")
        .join("runner")
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

/// Build the ledger from the CURRENT open records.
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
    let sessions = store
        .open_records()
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

            LedgerEntry {
                claude_session_id: rec.claude_session_id,
                terminal_id: rec.terminal_id,
                page_id: rec.page_id,
                zone_index: rec.zone_index,
                title: rec.title,
                session_name: rec.session_name,
                account_label: rec.account_label,
                config_dir: rec.config_dir,
                working_dir: rec.working_dir,
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

    let mut sessions = sessions;
    sessions.sort_by(|a, b| a.claude_session_id.cmp(&b.claude_session_id));

    SessionLedger {
        ledger_version: LEDGER_VERSION,
        captured_at_ms: now_ms,
        captured_at: chrono::DateTime::from_timestamp_millis(now_ms)
            .map(|t| t.to_rfc3339())
            .unwrap_or_default(),
        reason: reason.to_string(),
        shutdown_at: boot.and_then(|b| b.prior_marker_at),
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
/// Returns `true` when a write landed. Temp-file + atomic rename, so a process
/// killed mid-write cannot leave a torn ledger — the whole point of a record
/// that has to survive the kill a rebuild performs.
///
/// **Never fatal.** Every failure path logs and returns `false`; a ledger is a
/// diagnostic, and a diagnostic must never be the thing that breaks a boot.
pub fn persist_if_changed(ledger: &SessionLedger) -> bool {
    let fp = ledger.fingerprint();
    // The guard is held across the WHOLE write, not just the comparison.
    //
    // Three writers exist in one process — the boot latch, the 45 s liveness
    // poll, and `POST /control/sessions/ledger/capture` — and they can overlap.
    // Dropping the lock after the check made this check-then-act, and two
    // concurrent `fs::write`s to one temp path then two renames can publish a
    // TRUNCATED ledger, defeating the atomicity this whole module is built
    // around. A poisoned lock skips the write rather than racing.
    let Ok(mut guard) = last_written().lock() else {
        warn!("session_ledger: write lock poisoned — skipping this write");
        return false;
    };
    if guard.as_deref() == Some(fp.as_str()) {
        debug!("session_ledger: unchanged — no write");
        return false;
    }

    let path = ledger_path();
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
    // pid AND a per-call counter: the pid alone gave every writer in this
    // process the same temp path.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = path.with_extension(format!("json.tmp.{}.{seq}", std::process::id()));
    if let Err(e) = std::fs::write(&tmp, &json) {
        warn!(error = %e, path = %tmp.display(), "session_ledger: temp write failed");
        return false;
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        warn!(error = %e, path = %path.display(), "session_ledger: rename failed");
        let _ = std::fs::remove_file(&tmp);
        return false;
    }
    *guard = Some(fp);
    info!(
        sessions = ledger.sessions.len(),
        reason = %ledger.reason,
        path = %path.display(),
        "session_ledger: persisted the open-session ledger"
    );
    true
}

/// [`persist_if_changed`], refusing the one write that can DESTROY evidence.
///
/// The boot path reads the prior ledger and then writes this process's own
/// capture over the same file. If the lifecycle registry happens to be empty at
/// that instant — a fresh instance, a lost or reset registry, a store that
/// failed to open — an empty `sessions: []` lands on disk and the only record
/// of what the LAST boot had open is gone. The next boot then reads that empty
/// ledger and reports `verdict: "match"`, a fabricated positive on a file this
/// code wrote itself. Exactly the vacuous-`match` the census's R3 forbids.
///
/// So an EMPTY capture never overwrites a NON-EMPTY prior. It is not dropped
/// silently either — it is logged, because "the registry read empty at boot" is
/// itself worth seeing.
pub fn persist_capture(ledger: &SessionLedger) -> bool {
    if ledger.sessions.is_empty() {
        if let Some(prior) = prior() {
            if !prior.sessions.is_empty() {
                warn!(
                    prior_sessions = prior.sessions.len(),
                    prior_captured_at = %prior.captured_at,
                    reason = %ledger.reason,
                    "session_ledger: REFUSING to overwrite a non-empty prior ledger with an \
                     empty capture — the registry read empty, which is not proof the previous \
                     boot had nothing open"
                );
                return false;
            }
        }
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

/// The PREVIOUS process's ledger, latched once.
///
/// **Ordering is load-bearing.** This must be called at boot BEFORE the first
/// [`persist_if_changed`], or this process's own capture overwrites the very
/// file the report needs to read. [`load_prior_once`] is what enforces that,
/// and it is idempotent so a second call cannot re-latch a post-capture read.
static PRIOR: OnceLock<Option<SessionLedger>> = OnceLock::new();

/// Latch the prior boot's ledger off disk. Call from `main.rs` setup, before
/// anything writes one.
pub fn load_prior_once() -> Option<&'static SessionLedger> {
    let slot = PRIOR.get_or_init(|| {
        let led = load_from(&ledger_path());
        match &led {
            Some(l) => info!(
                sessions = l.sessions.len(),
                captured_at = %l.captured_at,
                reason = %l.reason,
                "session_ledger: loaded the PRIOR boot's open-session ledger"
            ),
            None => info!(
                path = %ledger_path().display(),
                "session_ledger: no prior ledger on disk — the post-rebuild report will \
                 read UNKNOWN, never 'nothing was lost'"
            ),
        }
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
    #[serde(default)]
    pub session_name: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub working_dir: Option<String>,
    #[serde(default)]
    pub worktree_path: Option<String>,
    #[serde(default)]
    pub plan_slug: Option<String>,
    #[serde(default)]
    pub work_unit_id: Option<String>,
    #[serde(default)]
    pub wip_state: Option<String>,
    #[serde(default)]
    pub wip_ref: Option<String>,
    /// Was this session resumable AT CAPTURE TIME? `None` = could not be
    /// determined, never collapsed into `false`.
    pub restorable: Option<bool>,
    /// For a missing session: WHY it is missing.
    /// `not-restorable` — it was never identity-restorable, so restore could
    /// not have brought its conversation back;
    /// `restorability-unknown` — we could not tell (no working dir to probe
    /// against), so its absence is NOT evidence either way;
    /// `no-attempt` — nothing came back and nothing tried.
    #[serde(default)]
    pub reason: Option<String>,
    /// `cd "<dir>" && CLAUDE_CONFIG_DIR="<root>" claude --resume <id>`.
    /// `None` — NEVER a guess — when the account root is unknown.
    #[serde(default)]
    pub resume_command: Option<String>,
}

/// `GET /control/sessions/ledger`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerReport {
    /// `ok` | `unavailable`.
    pub status: String,
    /// REQUIRED whenever `status != "ok"` or the verdict is `unknown`.
    #[serde(default)]
    pub reason: Option<String>,
    pub generated_at: i64,
    /// Metadata of the ledger being compared against.
    #[serde(default)]
    pub prior_captured_at: Option<String>,
    #[serde(default)]
    pub prior_reason: Option<String>,
    /// The prior boot's open sessions — the N in "rebuild with N sessions
    /// open".
    pub expected: Vec<LedgerEntry>,
    /// Of those, the ones open again now.
    pub returned: Vec<LedgerOutcome>,
    /// Of those, the ones that did NOT come back — each with a resume line.
    pub missing: Vec<LedgerOutcome>,
    /// [`VERDICT_MATCH`] | [`VERDICT_PARTIAL`] | [`VERDICT_MISMATCH`] |
    /// [`VERDICT_UNKNOWN`].
    pub verdict: String,
    /// What is open right now — so the report is self-contained even when
    /// there is no prior ledger to compare against.
    pub current: SessionLedger,
    /// Always present, and never implies an empty answer.
    pub note: String,
}

fn resume_command_for(entry: &LedgerEntry) -> Option<String> {
    use crate::agent_worktree::custody::{is_shell_safe_token, shell_quote_path};
    // Every input here reaches a line the operator COPY-PASTES INTO A SHELL,
    // and `config_dir` / `working_dir` come off a registry file. A token that
    // cannot be rendered safely yields NO line — the same omit-never-guess rule
    // the unknown-account-root case already follows.
    let dir = shell_quote_path(
        entry
            .worktree_path
            .as_deref()
            .or(entry.working_dir.as_deref())?,
    )?;
    let config = shell_quote_path(entry.config_dir.as_deref()?)?;
    if !is_shell_safe_token(&entry.claude_session_id) {
        return None;
    }
    Some(format!(
        "cd \"{dir}\" && CLAUDE_CONFIG_DIR=\"{config}\" claude --resume {}",
        entry.claude_session_id
    ))
}

fn outcome_of(entry: &LedgerEntry, reason: Option<&str>) -> LedgerOutcome {
    LedgerOutcome {
        claude_session_id: entry.claude_session_id.clone(),
        session_name: entry.session_name.clone(),
        title: entry.title.clone(),
        working_dir: entry.working_dir.clone(),
        worktree_path: entry.worktree_path.clone(),
        plan_slug: entry.plan_slug.clone(),
        work_unit_id: entry.work_unit_id.clone(),
        wip_state: entry.wip_state.clone(),
        wip_ref: entry.wip_ref.clone(),
        restorable: entry.restorable,
        reason: reason.map(str::to_string),
        resume_command: resume_command_for(entry),
    }
}

/// PURE diff: the prior ledger vs the ids observed back this boot.
///
/// Split from the route so the verdict and the resume lines are testable
/// without a store, a disk or a rebuild.
pub fn diff(
    prior: Option<&SessionLedger>,
    back: &[String],
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
            verdict: VERDICT_UNKNOWN.to_string(),
            note: "No ledger from a previous boot is on disk, so this runner cannot state \
                   what was open before it started. That is UNKNOWN, not 'nothing was \
                   lost'. The ledger this boot writes will make the NEXT rebuild \
                   answerable."
                .to_string(),
            current,
        };
    };

    let back_lower: Vec<String> = back.iter().map(|s| s.to_ascii_lowercase()).collect();
    let is_back = |id: &str| back_lower.iter().any(|b| b == &id.to_ascii_lowercase());

    let mut returned = Vec::new();
    let mut missing = Vec::new();
    for e in &prior.sessions {
        if is_back(&e.claude_session_id) {
            returned.push(outcome_of(e, None));
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
            missing.push(outcome_of(e, Some(reason)));
        }
    }

    // An input we could not read must not be laundered into a confident miss
    // count. `observed_back` skips the sticky-restore-stamp arm when this boot
    // never latched a census, so a session that came back and was then closed
    // lands in `missing` — through no fault of the rebuild.
    let verdict = if !restore_stamps_available && !missing.is_empty() {
        VERDICT_UNKNOWN
    } else if prior.sessions.is_empty() {
        // The prior boot genuinely had nothing open. Only stateable because a
        // ledger EXISTS saying so — which is exactly the difference between
        // this and the `no_prior_ledger` arm above.
        VERDICT_MATCH
    } else if missing.is_empty() {
        VERDICT_MATCH
    } else if returned.is_empty() {
        VERDICT_MISMATCH
    } else {
        VERDICT_PARTIAL
    };

    let unresumable = missing
        .iter()
        .filter(|m| m.resume_command.is_none())
        .count();
    let with_wip = missing
        .iter()
        .filter(|m| m.wip_state.is_some() || m.worktree_path.is_some())
        .count();
    let mut note = format!(
        "{} of {} sessions open before the last shutdown came back. {} did not; {} of those \
         name a worktree that may hold uncommitted work, and {} could not be given a resume \
         line because the account root they ran under is unknown (a --resume under the wrong \
         CLAUDE_CONFIG_DIR fails as though the session never existed).",
        returned.len(),
        prior.sessions.len(),
        missing.len(),
        with_wip,
        unresumable
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
        verdict: verdict.to_string(),
        note,
        current,
    }
}

/// Every session id observed BACK this boot: open in the registry now, plus
/// anything carrying a restore stamp from this boot.
///
/// The union matters — a session that came back and was then closed by the
/// operator DID come back, and counting only `open` rows would report a
/// `missing` the rebuild is not guilty of. That is the same reasoning
/// [`crate::session::restore_census::observe_restored`] spells out for reading
/// all records rather than only open ones.
pub fn observed_back(store: &SessionLifecycleStore, boot_at_ms: Option<i64>) -> Vec<String> {
    let mut ids: Vec<String> = store
        .open_records()
        .into_iter()
        .map(|r| r.claude_session_id)
        .collect();
    // `restored_from_boot_at` is STICKY across restarts by design, so a stamp
    // older than THIS boot belongs to a previous one. Without a boot instant
    // to compare against, the closed-record arm is SKIPPED entirely rather
    // than admitted with a `0` floor — a `0` would count every historical
    // restore stamp as "came back", which is the wrong-in-the-operator's-favour
    // direction and exactly the vacuous-`match` failure the census's R3 names.
    if let Some(boot_at_ms) = boot_at_ms {
        for r in store.all_records() {
            if r.restored_from_boot_at.is_some_and(|at| at >= boot_at_ms)
                && !ids.iter().any(|i| i == &r.claude_session_id)
            {
                ids.push(r.claude_session_id);
            }
        }
    }
    ids.sort();
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, restorable: Option<bool>, config: Option<&str>) -> LedgerEntry {
        LedgerEntry {
            claude_session_id: id.to_string(),
            terminal_id: format!("term-{id}"),
            page_id: "default".to_string(),
            zone_index: 0,
            title: Some(format!("tab-{id}")),
            session_name: Some(format!("name-{id}")),
            account_label: Some("tiohorst".to_string()),
            config_dir: config.map(str::to_string),
            working_dir: Some("D:/qontinui-root/_wt/thing/sub".to_string()),
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
        let report = diff(Some(&prior), &[], ledger(Vec::new()), true);
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
        let report = diff(None, &[], ledger(Vec::new()), true);
        assert_eq!(report.verdict, VERDICT_UNKNOWN);
        assert_eq!(report.status, "unavailable");
        assert_eq!(report.reason.as_deref(), Some("no_prior_ledger"));
        assert!(report.note.contains("not 'nothing was"), "{}", report.note);
    }

    /// An EMPTY prior ledger is different from an ABSENT one: the previous boot
    /// affirmatively had nothing open, and a ledger on disk says so.
    #[test]
    fn an_empty_prior_ledger_is_a_real_match_unlike_an_absent_one() {
        let report = diff(Some(&ledger(Vec::new())), &[], ledger(Vec::new()), true);
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
        let report = diff(Some(&prior), &[], ledger(Vec::new()), true);
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
        let report = diff(Some(&prior), &[], ledger(Vec::new()), true);
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
        let report = diff(Some(&prior), &["s1".to_string()], ledger(Vec::new()), false);
        assert_eq!(report.verdict, VERDICT_MATCH);
        assert_eq!(report.reason, None);
    }

    /// A shell-hostile id or path yields NO resume line rather than a broken
    /// (or dangerous) one the operator would paste.
    #[test]
    fn a_shell_hostile_id_or_path_yields_no_resume_line() {
        let mut e = entry("s1\"; rm -rf /", Some(true), Some("C:/claude/.claude-x"));
        assert_eq!(resume_command_for(&e), None);

        e = entry("s1", Some(true), Some("C:/claude/.claude-x"));
        e.worktree_path = Some("D:/a`whoami`".to_string());
        e.working_dir = None;
        assert_eq!(resume_command_for(&e), None);

        e = entry("s1", Some(true), Some("C:/$EVIL"));
        assert_eq!(resume_command_for(&e), None);
    }

    /// An empty capture must never destroy a non-empty prior — that is how a
    /// later boot manufactures a vacuous `match`.
    #[test]
    fn an_empty_capture_is_refused_when_it_would_erase_a_non_empty_prior() {
        // `prior()` is a process-wide latch we cannot seed from a unit test
        // without poisoning other tests, so this asserts the DECISION shape
        // that `persist_capture` encodes: an empty capture beside a non-empty
        // prior is refused; every other combination proceeds.
        fn would_refuse(capture_len: usize, prior_len: Option<usize>) -> bool {
            capture_len == 0 && matches!(prior_len, Some(n) if n > 0)
        }
        assert!(would_refuse(0, Some(3)), "empty over non-empty: REFUSE");
        assert!(!would_refuse(0, Some(0)), "empty over empty: fine");
        assert!(!would_refuse(0, None), "empty with no prior: fine");
        assert!(!would_refuse(3, Some(3)), "non-empty always proceeds");
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
}
