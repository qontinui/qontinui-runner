//! Automatic cross-account migration of token-exhausted Claude terminal
//! sessions.
//!
//! ## Problem
//!
//! A long-running interactive Claude session pins the account it was launched
//! under (`CLAUDE_CONFIG_DIR`). When that account runs out of tokens the CLI
//! prints a usage-limit message and the session is stranded — the operator
//! has to manually re-open it under another account (the `/resume-foreign`
//! slash-command flow) even when a sibling account still has capacity —
//! capacity that expires at that account's own reset if nothing spends it.
//!
//! ## Flow
//!
//! 1. **Hint** — [`super::usage_limit::UsageLimitWatchHook`] sees a
//!    usage-limit message in the PTY stream and calls
//!    [`handle_usage_limit_hint`] (debounced per terminal).
//! 2. **Confirm** — re-probe every configured account
//!    (`refresh_account_usage_snapshot`) and require the *probe* to agree the
//!    session's account is exhausted. Conversation text that merely quotes a
//!    limit message never migrates anything.
//! 3. **Pick target** — a non-exhausted, credential-valid, non-cooled account,
//!    chosen by the selection mode's handoff strategy (`pick_migration_target`;
//!    `AlternatingHandoff`, the default, and `WeightedRandom` are described in
//!    `ai_provider::account_usage`). Every other mode, and the fallback when no
//!    account is under pace, takes the account whose spare weekly capacity is
//!    closest to expiring (the same `ai_provider::config::cmp_rank` key
//!    the spawn-time picker uses: `exhausted` dominates, then the pace tier,
//!    then, among accounts under their projected pace, the HIGHEST
//!    `expected_utilization` — the window furthest along — because unused
//!    weekly capacity expires at the reset and does not roll over. A roster
//!    with no under-pace account falls back to the RATIO
//!    `utilization / expected_utilization` ascending, not the difference,
//!    which is not comparable across accounts at different points in their
//!    windows).
//! 4. **Migrate** — copy the session transcript
//!    (`<src>/projects/<slug>/<sid>.jsonl` → same slug under the target dir;
//!    the source file is never touched; a target copy that diverged from it is
//!    renamed aside to `<sid>.jsonl.diverged-<unix_ms>`, never deleted, so the
//!    live conversation is what resumes) and, best-effort, its sibling
//!    directory `<sid>/` (subagent transcripts — without it no subagent spawned
//!    before the migration can be resumed; a diverged one is renamed aside the
//!    same way), respawn
//!    `claude --permission-mode bypassPermissions --resume <sid>` in a fresh
//!    PTY pinned to the target account, on the same grid page/zone, and only
//!    THEN close the old pane. The respawn re-keys the session's lifecycle
//!    row onto the new PTY; the old pane's exit is answered as
//!    `TerminalSuperseded` and closes nothing. A failed respawn leaves the
//!    old pane in place.
//!
//! Guards: per-session migration cap (no ping-pong when every account is
//! dry), one migration in flight per session ([`begin_migration`], which also
//! refuses a session that already moved or is closed), settings kill-switch
//! (`claude_cli.auto_migrate_on_token_exhaustion`), and a no-op with fewer
//! than two configured accounts. A refusal by [`begin_migration`] is not a
//! failure: the automatic path logs it, reports nothing to the UI and gives
//! back the cap slot it took ([`settle_hint_migration`]). The manual Tauri
//! command (`terminal_migrate_session_account`) reuses step 4 directly — an
//! operator click is its own confirmation.
//!
//! The in-flight guard covers the migration itself — carry, trust, respawn,
//! close — and NOT the detached step-4b delta sync
//! ([`crate::terminal::session_carry::spawn_delta_sync_after_exit`]), which
//! outlives it by up to its writer-exit wait plus the copy. That is
//! deliberate: a fast second hop is legitimate (the target account can run
//! dry too), and holding the guard through the sync would refuse it. Nor does
//! the sync need it for safety: it writes only a destination file still
//! exactly as its own carry left it, so it backs off anything the resumed
//! pane has touched. (A second hop's carry never writes the first sync's
//! destination — it reads it as its own source.)
//!
//! What the sync does NOT give is convergence across a fast second hop.
//! Nothing orders the first hop's sync (A→B) against the second hop's carry
//! and sync (B→C). The first sync's late writes reach C only if they land in
//! B before the second hop's carry or sync walks B; otherwise they stay on B —
//! not lost, just not forwarded — and a later migration or respawn picks them
//! up from there.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;

use serde_json::json;
use tauri::Emitter;
use tracing::{info, warn};

use crate::session::session_lifecycle_store::TerminalSessionRecord;
use crate::terminal::graceful_exit::ProcIdentity;
use crate::terminal::session_carry::{
    copy_transcript, CarriedSubagent, DeltaSyncOutcome, TranscriptCopy,
};

/// Max migrations per Claude session within [`MIGRATION_CAP_WINDOW_MS`].
/// A second hop is legitimate (the target can run dry too); an unbounded
/// chain means every account is exhausted and migrating is pure churn.
pub(crate) const MIGRATION_CAP: usize = 3;
const MIGRATION_CAP_WINDOW_MS: i64 = 24 * 60 * 60 * 1000;

/// Cooldown stamped on the exhausted source account so spawn-time selection
/// (`pick_best_account` / `rotate_account_on_rate_limit`) stays off it. One
/// hour — matches `EXHAUSTION_STALE_TTL`'s trust window for the probe's
/// `exhausted` flag; a weekly/5-hour cap doesn't clear in minutes.
const EXHAUSTED_ACCOUNT_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(3600);

/// Per-session migration timestamps (unix millis), for the cap.
static MIGRATION_HISTORY: Mutex<Option<HashMap<String, Vec<i64>>>> = Mutex::new(None);

/// Record-and-check the migration cap for a session. Returns `false` when
/// the cap is exhausted (caller must not migrate).
pub(crate) fn migration_cap_permits(claude_session_id: &str, now_ms: i64) -> bool {
    let Ok(mut hist) = MIGRATION_HISTORY.lock() else {
        return false;
    };
    let map = hist.get_or_insert_with(HashMap::new);
    let entry = map.entry(claude_session_id.to_string()).or_default();
    entry.retain(|t| now_ms - *t <= MIGRATION_CAP_WINDOW_MS);
    if entry.len() >= MIGRATION_CAP {
        return false;
    }
    entry.push(now_ms);
    true
}

/// Give back one cap slot [`migration_cap_permits`] took at `stamp` for a
/// migration that never ran (refused by [`begin_migration`]). Removes exactly
/// one entry with that stamp under the history lock, so two hints that took
/// slots in the same millisecond still leave the count right; a stamp already
/// aged out of the window is a no-op.
fn refund_migration_cap(claude_session_id: &str, stamp: i64) {
    let mut hist = MIGRATION_HISTORY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(entry) = hist.as_mut().and_then(|m| m.get_mut(claude_session_id)) {
        if let Some(i) = entry.iter().position(|t| *t == stamp) {
            entry.remove(i);
        }
    }
}

/// Why [`migrate_session_off_runtime`] did not migrate.
#[derive(Debug)]
pub(crate) enum MigrateError {
    /// [`begin_migration`] refused before anything was read or written: the
    /// session is already migrating, has already moved, is closed, or is gone. Nothing
    /// failed and nothing changed. Also the re-check just before the respawn
    /// ([`still_migratable`]): the session closed or moved while the carry
    /// ran, so nothing was respawned and the old pane was not touched; the
    /// carried files stay at the destination, which a carry never makes worse.
    Refused(MigrationRefusal),
    /// The migration ran and failed; the old pane is left in place.
    Failed(String),
}

impl From<String> for MigrateError {
    fn from(e: String) -> Self {
        Self::Failed(e)
    }
}

impl From<&str> for MigrateError {
    fn from(e: &str) -> Self {
        Self::Failed(e.to_string())
    }
}

impl std::fmt::Display for MigrateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(refusal) => refusal.fmt(f),
            Self::Failed(e) => f.write_str(e),
        }
    }
}

/// Claude session ids with a migration in flight, for [`begin_migration`].
static MIGRATIONS_IN_FLIGHT: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// Why [`begin_migration`] refused to start a migration. `Display` is the
/// error string the manual Tauri command returns to the UI; the automatic
/// path only logs it ([`settle_hint_migration`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MigrationRefusal {
    /// Another migration of the same session is still running.
    InFlight { session: String },
    /// The session no longer runs where the caller saw it: its record now
    /// names another account (`to` = that config dir) or another terminal.
    AlreadyMoved { to: String },
    /// The session's lifecycle record is gone.
    Unknown { session: String },
    /// The session's lifecycle record is closed: there is no pane to migrate.
    Closed { session: String },
}

impl std::fmt::Display for MigrationRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InFlight { session } => write!(f, "migration already in flight for {session}"),
            Self::AlreadyMoved { to } => write!(f, "session already moved to {to}"),
            Self::Unknown { session } => {
                write!(f, "session {session} is no longer in the lifecycle store")
            }
            Self::Closed { session } => {
                write!(
                    f,
                    "session {session} is closed — there is no pane to migrate"
                )
            }
        }
    }
}

/// Holds one session's slot in [`MIGRATIONS_IN_FLIGHT`]; dropping it — on
/// return, on `?`, or while unwinding from a panic — releases the slot.
#[derive(Debug)]
pub(crate) struct MigrationInFlight {
    session: String,
}

impl Drop for MigrationInFlight {
    fn drop(&mut self) {
        // A poisoned lock still guards a valid set; never leave a slot held.
        let mut set = MIGRATIONS_IN_FLIGHT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(set) = set.as_mut() {
            set.remove(&self.session);
        }
    }
}

/// Take `claude_session_id`'s in-flight slot, or refuse when another migration
/// of the same session holds it.
fn claim_migration_slot(claude_session_id: &str) -> Result<MigrationInFlight, MigrationRefusal> {
    let mut set = MIGRATIONS_IN_FLIGHT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !set
        .get_or_insert_with(HashSet::new)
        .insert(claude_session_id.to_string())
    {
        return Err(MigrationRefusal::InFlight {
            session: claude_session_id.to_string(),
        });
    }
    Ok(MigrationInFlight {
        session: claude_session_id.to_string(),
    })
}

/// Serialise migrations per session, then re-check the caller's view.
///
/// Nothing else orders two migrations of one session: the usage-limit hint is
/// debounced per terminal only, the manual Tauri command has no debounce at
/// all, and [`migration_cap_permits`] allows [`MIGRATION_CAP`]. Run
/// concurrently, the second one's carry could find the live transcript the
/// first one's resumed pane is writing, read it as a diverged fork, move it
/// aside and copy a stale source over it. So: take the session's in-flight
/// slot (a second concurrent request is refused with
/// [`MigrationRefusal::InFlight`]), and, under it, re-read the session's
/// record (`current_record`) and refuse unless it is still open
/// ([`MigrationRefusal::Closed`] — `current_record` answers closed rows too,
/// and a closed session has no pane to migrate) and still runs under `src` in
/// the caller's terminal ([`MigrationRefusal::AlreadyMoved`]) — a migration
/// that finished while this one waited to be scheduled has already moved it.
///
/// An unpinned record (no `config_dir`) has never been migrated — a
/// migration always pins its destination — so the caller's fallback
/// resolution of `src` stands for it.
///
/// The returned guard must be held until the migration has finished.
pub(crate) fn begin_migration(
    caller_record: &TerminalSessionRecord,
    src_config_dir: &str,
    current_record: impl FnOnce() -> Option<TerminalSessionRecord>,
) -> Result<MigrationInFlight, MigrationRefusal> {
    let guard = claim_migration_slot(&caller_record.claude_session_id)?;
    still_migratable(caller_record, src_config_dir, current_record())?;
    Ok(guard)
}

/// The caller's view of the session against `current` (the session's record
/// as read NOW): it must still exist ([`MigrationRefusal::Unknown`]), be open
/// ([`MigrationRefusal::Closed`]), and run under `src_config_dir` in the
/// caller's terminal ([`MigrationRefusal::AlreadyMoved`]); an unpinned record
/// (no `config_dir`) stands for `src_config_dir`.
///
/// Run twice per migration, both times under the session's in-flight guard:
/// by [`begin_migration`] as it takes the guard, and by [`migrate_session`]
/// again just before the respawn, because the async prepare and the carry in
/// between take real time and the operator may close the pane meanwhile. A
/// closed pane must not be respawned: the second check aborts with the
/// refusal, leaving the carried files in place (a carry never destroys data)
/// and spawning nothing.
fn still_migratable(
    caller_record: &TerminalSessionRecord,
    src_config_dir: &str,
    current: Option<TerminalSessionRecord>,
) -> Result<(), MigrationRefusal> {
    let sid = &caller_record.claude_session_id;
    let Some(current) = current else {
        return Err(MigrationRefusal::Unknown {
            session: sid.clone(),
        });
    };
    if current.state != "open" {
        return Err(MigrationRefusal::Closed {
            session: sid.clone(),
        });
    }
    match current.config_dir.as_deref() {
        Some(dir) if dir != src_config_dir => {
            return Err(MigrationRefusal::AlreadyMoved {
                to: dir.to_string(),
            })
        }
        _ => {}
    }
    if current.terminal_id != caller_record.terminal_id {
        return Err(MigrationRefusal::AlreadyMoved {
            to: format!("terminal {}", current.terminal_id),
        });
    }
    Ok(())
}

/// Outcome payload emitted on `session-account-migrated`.
#[derive(Debug)]
pub struct MigrationOutcome {
    pub new_terminal_id: String,
    pub from_config_dir: String,
    pub to_config_dir: String,
}

/// Entry point for a PTY usage-limit hint (spawned async off the reader
/// thread). Resolves everything it needs from the global app handle, mirrors
/// `agent_runtime`'s state-access pattern.
pub async fn handle_usage_limit_hint(terminal_id: String, matched_pattern: &'static str) {
    use tauri::Manager;

    // Migration moves a session between subscription accounts, and those are
    // off while a model gateway is declared (`crate::model_gateway`).
    if crate::model_gateway::gateway_declared() {
        crate::model_gateway::note_subscription_path_off("account_migration");
        return;
    }

    let Some(app) = crate::tauri_app_handle::current() else {
        return;
    };
    let Some(store) = app.try_state::<std::sync::Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>() else {
        return;
    };

    // Only sessions the lifecycle registry knows about can migrate — a plain
    // shell pane (or an unregistered session) has no transcript binding.
    let Some(record) = store.find_open_by_terminal(&terminal_id) else {
        info!(
            terminal_id,
            matched_pattern,
            "usage-limit hint on a terminal with no registered Claude session — ignoring"
        );
        return;
    };

    // Source account: the dir the session launched under, else the runner's
    // currently-resolved global dir (pre-existing sessions registered before
    // config_dir capture).
    let Some(src) = record
        .config_dir
        .clone()
        .or_else(crate::ai_provider::get_resolved_config_dir)
    else {
        warn!(
            terminal_id,
            session = %record.claude_session_id,
            "usage-limit hint but the session's account is unknown — cannot migrate"
        );
        return;
    };

    let ai_settings = crate::settings::get_ai_settings();
    if !ai_settings.claude_cli.auto_migrate_on_token_exhaustion {
        info!(
            session = %record.claude_session_id,
            "usage-limit confirmed-candidate but auto-migration is disabled in settings"
        );
        return;
    }
    if crate::settings::get_claude_config_dirs().len() < 2 {
        return; // nothing to migrate to
    }

    // CONFIRM: a usage-limit *message* is only a hint (conversation text can
    // quote one; a resume repaint can re-render a historical one). Re-probe
    // and require the account to actually be exhausted before acting.
    let usage = crate::commands::ai_settings::refresh_account_usage_snapshot().await;
    if !crate::ai_provider::account_known_exhausted(&src) {
        info!(
            terminal_id,
            session = %record.claude_session_id,
            account = %src,
            matched_pattern,
            "usage-limit message NOT confirmed by probe — skipping (likely echoed text)"
        );
        return;
    }

    // Keep spawn-time selection off the dead account, and repoint the global
    // resolved dir so unrelated new sessions stop landing on it. When the
    // usage stats carry the 5-hour window's actual reset time, cool down
    // until then (clamped) instead of the blanket hour — the account comes
    // back into rotation the moment its session window rolls over.
    let cooldown = usage
        .iter()
        .find(|i| i.config_dir == src)
        .and_then(|i| i.session_resets_at)
        .and_then(|reset_ts| {
            let now = chrono::Utc::now().timestamp();
            let secs = (reset_ts as i64) - now;
            (secs > 0).then(|| std::time::Duration::from_secs((secs as u64).clamp(300, 6 * 3600)))
        })
        .unwrap_or(EXHAUSTED_ACCOUNT_COOLDOWN);
    crate::ai_provider::mark_account_rate_limited_with_duration(&src, cooldown);
    crate::ai_provider::pick_best_account();

    let cap_stamp = chrono::Utc::now().timestamp_millis();
    if !migration_cap_permits(&record.claude_session_id, cap_stamp) {
        warn!(
            session = %record.claude_session_id,
            cap = MIGRATION_CAP,
            "migration cap reached for this session — not migrating again"
        );
        emit_skipped(&app, &record, &src, "migration-cap-reached");
        return;
    }

    let Some(dst) = crate::ai_provider::pick_migration_target(&src) else {
        warn!(
            session = %record.claude_session_id,
            from = %src,
            "no usable migration target (every other account exhausted/cooled/unauthenticated)"
        );
        emit_skipped(&app, &record, &src, "no-usable-target");
        return;
    };

    info!(
        session = %record.claude_session_id,
        from = %src,
        to = %dst,
        matched_pattern,
        "token exhaustion confirmed — migrating session to a fresh account"
    );

    // The migration decides workspace trust for the destination through the
    // trust gate's SYNC door (it is a blocking function); warm the dial here,
    // on the async side, so that decision is the tenant's rather than a cold
    // cache's.
    crate::claude_session::trust_gate::warm_dial().await;

    let result = migrate_session_off_runtime(&app, &record, &src, &dst).await;
    settle_hint_migration(
        &record.claude_session_id,
        cap_stamp,
        result,
        || crate::ai_provider::unrecord_handoff(&dst),
        |reason| emit_skipped(&app, &record, &src, reason),
    );
}

/// The automatic path's answer to the migration's result.
///
/// - Migrated: logged.
/// - [`MigrateError::Refused`]: another migration of this session holds its
///   slot, or already moved it (or its record is gone, or closed — no pane
///   is left to migrate). Nothing failed, so it
///   is logged at info and NOT reported as `session-account-migration-skipped`
///   — the UI would otherwise toast a "migration failed" for a session that
///   is migrating or has migrated. The cap slot this hint took at
///   `cap_stamp` is given back ([`refund_migration_cap`]) since no migration
///   ran; the refund, not a pre-check, because only the guard inside
///   [`begin_migration`] decides a refusal race-free.
/// - [`MigrateError::Failed`]: warned and reported `migration-failed: …`.
///
/// Refused and Failed both `unrecord_handoff`: the session never reached the
/// target this hint picked, so it must not count as a handoff to it.
fn settle_hint_migration(
    claude_session_id: &str,
    cap_stamp: i64,
    result: Result<MigrationOutcome, MigrateError>,
    unrecord_handoff: impl FnOnce(),
    emit_skipped: impl FnOnce(&str),
) {
    match result {
        Ok(outcome) => info!(
            session = %claude_session_id,
            new_terminal = %outcome.new_terminal_id,
            from = %outcome.from_config_dir,
            to = %outcome.to_config_dir,
            "session migrated"
        ),
        Err(MigrateError::Refused(refusal)) => {
            info!(
                session = %claude_session_id,
                %refusal,
                "usage-limit migration not started — the session is already migrating, has moved, or is closed; nothing to report"
            );
            unrecord_handoff();
            refund_migration_cap(claude_session_id, cap_stamp);
        }
        Err(MigrateError::Failed(e)) => {
            warn!(
                session = %claude_session_id,
                error = %e,
                "session migration failed"
            );
            unrecord_handoff();
            emit_skipped(&format!("migration-failed: {e}"));
        }
    }
}

/// The model the session was last actually running on, from the transcript's
/// most recent assistant turn (`.message.model`). `None` when the transcript
/// is unreadable or records no real model (synthetic error turns are
/// skipped) — the respawn then omits `--model` and the CLI default applies.
fn transcript_last_model(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    for line in content.lines().rev() {
        // Cheap pre-filter before parsing multi-KB JSONL lines.
        if !line.contains("\"assistant\"") || !line.contains("\"model\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if v.get("type").and_then(|t| t.as_str()) != Some("assistant") {
            continue;
        }
        if let Some(m) = v
            .get("message")
            .and_then(|m| m.get("model"))
            .and_then(|m| m.as_str())
        {
            if m.starts_with("claude-") {
                return Some(m.to_string());
            }
        }
    }
    None
}

/// Text typed into the migrated session once the resumed CLI is idle.
const CONTINUE_NUDGE: &str = "This session was automatically migrated to a fresh Claude account \
after the previous account hit a usage limit. Continue the task you were working on from where \
it left off; if you were idle awaiting operator input, say so and wait. Subagents interrupted by \
the limit should still have their transcripts on this account: resume each one with SendMessage \
to its agent id instead of spawning a replacement (cancel any resume gate you booked for it \
first), and only if SendMessage reports \"No transcript found\" fall back to its checkpoint or \
worktree.";

/// How the watcher decides the resumed CLI is ready for input:
/// [`crate::terminal::graceful_exit::input_prompt_ready`], the predicate the
/// session message poller already pastes through (`❯` on the cursor row, an
/// idle screen, an empty input line, no dialog), read on two consecutive
/// identical screens ([`ReadyDebounce`]).
///
/// It used to be the Ink footer hint `? for shortcuts`, which Claude Code
/// paints only in the DEFAULT permission mode. The sessions this watcher
/// serves are resumed with `--permission-mode bypassPermissions` unless a
/// launch template overrides it, and in that mode the footer slot reads
/// `⏵⏵ bypass permissions on (shift+tab to cycle)` instead. The runner logs on
/// merytshost read 24 `never painted its idle prompt` give-ups and zero
/// submissions over 2026-09-23..2026-10-02, so neither the migration nudge nor
/// coord's respawn `initial_prompt` was delivered. The hint is not kept as a
/// fallback: it was matched against the whole screen, transcript included, so
/// it would have overridden every refusal (a draft, a dialog) the shared
/// predicate makes, and a default-mode v2 pane draws `❯` as well.
///
/// Painted while Claude is mid-turn — if visible, the session already picked
/// itself up (or the operator beat us to it) and no nudge is needed.
const BUSY_MARKER: &str = "esc to interrupt";

/// What one grid read tells the idle watcher.
#[derive(Debug, PartialEq, Eq)]
enum IdlePromptProbe {
    /// Claude is mid-turn: the session picked itself up, no prompt is needed.
    Busy,
    /// The input box is idle and empty: submit now.
    Ready,
    /// Neither yet (still repainting, a dialog, a draft): read again. Carries
    /// why the shared predicate refused, for the give-up log line.
    NotYet(String),
}

/// Classify one rendered screen. Busy wins over ready, as before: a pane
/// showing `esc to interrupt` never receives the prompt.
fn probe_idle_prompt(screen: &crate::terminal::graceful_exit::ScreenText) -> IdlePromptProbe {
    let normalized = super::output_scan::normalize(&screen.lines.join("\n"));
    if normalized.contains(BUSY_MARKER) {
        return IdlePromptProbe::Busy;
    }
    match crate::terminal::graceful_exit::input_prompt_ready(screen) {
        Ok(()) => IdlePromptProbe::Ready,
        Err(why) => IdlePromptProbe::NotYet(why),
    }
}

/// Holds a `Ready` read until the NEXT read shows the identical screen. A
/// resuming v2 pane can paint an idle `❯` for a moment while its start-up
/// hooks and MCP servers are still loading; one read would submit into a pane
/// that is still settling. The session message poller debounces the same way
/// (`looks_idle_quiescent`).
#[derive(Default)]
struct ReadyDebounce {
    last_ready: Option<crate::terminal::graceful_exit::ScreenText>,
}

impl ReadyDebounce {
    /// `true` when `screen` read `Ready` and is identical to the previous
    /// read, which also read `Ready`.
    fn settled(&mut self, screen: crate::terminal::graceful_exit::ScreenText) -> bool {
        let settled = self.last_ready.as_ref() == Some(&screen);
        self.last_ready = Some(screen);
        settled
    }

    /// A read that was not `Ready` restarts the debounce.
    fn reset(&mut self) {
        self.last_ready = None;
    }
}

/// Watch the freshly-respawned terminal until the resumed CLI paints its idle
/// prompt, then submit [`CONTINUE_NUDGE`]. Detached and strictly best-effort:
/// bounded (~3 min), one submission max, and it stands down when the session
/// is already busy or the terminal goes away. Deliberately does NOT stand
/// down on a visible usage-limit message: a `--resume` repaint can re-render
/// the OLD account's historical banner (see the `usage_limit` module doc),
/// and a genuinely-dry target is caught by the grid scanner firing a fresh
/// migration hint on this terminal anyway.
///
/// It first waits for the step-4b delta sync (`sync_done`) to report, bounded
/// by [`NUDGE_SYNC_WAIT`], so the resumed session is not told to `SendMessage`
/// a subagent whose last lines are still being topped up. The sync starts no
/// later than [`crate::terminal::session_carry::DELTA_SYNC_EXIT_WAIT`] after
/// it was spawned (its writer-exit wait counts no look that completes after
/// that) and this wait starts after the sync was spawned, so the nudge
/// normally goes after the sync. The bounded exception: a sync whose copy
/// itself outlasts [`DELTA_SYNC_REPORT_MARGIN`] (a slow disk, a saturated
/// blocking pool) — the nudge then goes first, and the log says the sync had
/// not reported. See [`run_continue_nudge`].
///
/// The subagents the nudge names are recomputed once that wait is over
/// ([`crate::terminal::session_carry::resumable_subagents_now`]), so one the
/// sync first copied is named and one still missing is not; the carry-time
/// list is the fallback when that recompute fails.
fn spawn_continue_nudge(
    terminal_id: String,
    carried: TranscriptCopy,
    sync_done: Option<tokio::sync::oneshot::Receiver<DeltaSyncOutcome>>,
) {
    let fallback = carried.subagents.clone();
    tauri::async_runtime::spawn(run_continue_nudge(
        terminal_id.clone(),
        sync_done,
        NUDGE_SYNC_WAIT,
        fallback,
        move || async move {
            // A directory listing plus a few stats: still disk I/O, so off
            // the async workers.
            qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked(move || {
                crate::terminal::session_carry::resumable_subagents_now(&carried)
                    .map_err(|e| e.to_string())
            })
            .await
            .map_err(|e| format!("subagent recompute task did not complete: {e}"))?
        },
        move |prompt| prompt_when_idle(terminal_id, prompt, "continue-nudge"),
    ));
}

/// Time the delta sync gets to finish its copy and report after its
/// writer-exit wait ([`crate::terminal::session_carry::DELTA_SYNC_EXIT_WAIT`])
/// has ended, before the continue nudge stops waiting for it.
const DELTA_SYNC_REPORT_MARGIN: std::time::Duration = std::time::Duration::from_secs(15);

/// How long the continue nudge waits for the delta sync to report: the sync's
/// writer-exit wait plus [`DELTA_SYNC_REPORT_MARGIN`].
const NUDGE_SYNC_WAIT: std::time::Duration =
    crate::terminal::session_carry::DELTA_SYNC_EXIT_WAIT.saturating_add(DELTA_SYNC_REPORT_MARGIN);

/// What the continue nudge heard from the delta sync before it started.
#[derive(Debug, PartialEq, Eq)]
enum SyncGate {
    /// No sync was started (no writer identity to wait on).
    NoSync,
    /// The sync finished, or gave up, with this outcome.
    Reported(DeltaSyncOutcome),
    /// The sync task ended without reporting.
    Dropped,
    /// Nothing within the deadline.
    TimedOut,
}

/// Wait up to `deadline` for the delta sync's outcome.
async fn await_delta_sync(
    sync_done: Option<tokio::sync::oneshot::Receiver<DeltaSyncOutcome>>,
    deadline: std::time::Duration,
) -> SyncGate {
    let Some(rx) = sync_done else {
        return SyncGate::NoSync;
    };
    match tokio::time::timeout(deadline, rx).await {
        Ok(Ok(outcome)) => SyncGate::Reported(outcome),
        Ok(Err(_)) => SyncGate::Dropped,
        Err(_) => SyncGate::TimedOut,
    }
}

/// The continue nudge's body: wait for the delta sync (bounded by
/// `deadline`), log what it heard, then `recompute` the resumable subagents
/// and `deliver` the [`continue_nudge`] naming them. A sync that times out,
/// fails or never reports holds the nudge no longer than `deadline`; the
/// nudge still goes, and the log says why. A failed recompute falls back to
/// `carry_time_subagents`, the list the carry produced.
async fn run_continue_nudge<R, RF, D, DF>(
    terminal_id: String,
    sync_done: Option<tokio::sync::oneshot::Receiver<DeltaSyncOutcome>>,
    deadline: std::time::Duration,
    carry_time_subagents: Vec<CarriedSubagent>,
    recompute: R,
    deliver: D,
) where
    R: FnOnce() -> RF,
    RF: std::future::Future<Output = Result<Vec<CarriedSubagent>, String>>,
    D: FnOnce(String) -> DF,
    DF: std::future::Future<Output = ()>,
{
    match await_delta_sync(sync_done, deadline).await {
        SyncGate::NoSync | SyncGate::Reported(DeltaSyncOutcome::Synced(_)) => {}
        SyncGate::Reported(other) => info!(
            terminal_id,
            outcome = ?other,
            "continue-nudge: the delta sync did not complete — nudging anyway"
        ),
        SyncGate::Dropped => warn!(
            terminal_id,
            "continue-nudge: the delta sync ended without reporting — nudging anyway"
        ),
        SyncGate::TimedOut => warn!(
            terminal_id,
            wait_s = deadline.as_secs(),
            "continue-nudge: the delta sync did not report within its deadline — nudging anyway"
        ),
    }
    let subagents = match recompute().await {
        Ok(now) => now,
        Err(e) => {
            warn!(
                terminal_id,
                error = %e,
                "continue-nudge: could not recompute the resumable subagents — naming the carry-time list"
            );
            carry_time_subagents
        }
    };
    deliver(continue_nudge(&subagents)).await;
}

/// How many carried subagents [`continue_nudge`] names before summarising the
/// rest as a count, so a large fan-out cannot make the typed prompt unbounded.
const NUDGE_MAX_LISTED_SUBAGENTS: usize = 12;

/// Character bound on one subagent description in [`continue_nudge`].
const NUDGE_MAX_DESCRIPTION_CHARS: usize = 60;

/// The text typed into the migrated session: [`CONTINUE_NUDGE`] verbatim when
/// the carry found no subagent; otherwise it plus ONE more sentence naming the
/// carried agent ids (with their `.meta.json` descriptions) as resumable with
/// `SendMessage`. Kept on one line — the text is typed into a PTY, where a
/// newline would submit early — so a description's control characters are
/// replaced with spaces and its length is bounded.
fn continue_nudge(subagents: &[CarriedSubagent]) -> String {
    if subagents.is_empty() {
        return CONTINUE_NUDGE.to_string();
    }
    let listed: Vec<String> = subagents
        .iter()
        .take(NUDGE_MAX_LISTED_SUBAGENTS)
        .map(|agent| match agent.description.as_deref() {
            Some(description) => {
                let one_line: String = description
                    .chars()
                    .map(|c| if c.is_control() { ' ' } else { c })
                    .collect();
                let one_line = one_line.split_whitespace().collect::<Vec<_>>().join(" ");
                let mut bounded: String =
                    one_line.chars().take(NUDGE_MAX_DESCRIPTION_CHARS).collect();
                if one_line.chars().count() > NUDGE_MAX_DESCRIPTION_CHARS {
                    bounded.push('…');
                }
                format!("{} (\"{bounded}\")", agent.agent_id)
            }
            None => agent.agent_id.clone(),
        })
        .collect();
    let more = subagents.len().saturating_sub(NUDGE_MAX_LISTED_SUBAGENTS);
    let tail = if more > 0 {
        format!(", and {more} more under this session's subagents/ folder")
    } else {
        String::new()
    };
    format!(
        "{CONTINUE_NUDGE} The migration carried {} subagent transcript(s) to this account, \
         resumable with SendMessage to their agent id: {}{tail}.",
        subagents.len(),
        listed.join(", ")
    )
}

/// The generalized idle-watcher behind [`spawn_continue_nudge`]: wait for the
/// resumed CLI to paint its idle prompt, then submit `prompt` once.
///
/// Extracted so the respawn receiver
/// ([`crate::session::respawn`]) can deliver coord's optional
/// `initial_prompt` through the SAME bounded, stand-down-on-busy watcher
/// instead of growing a second one that would drift from it. `label` only
/// names the caller in the log lines.
pub(crate) fn spawn_prompt_when_idle(terminal_id: String, prompt: String, label: &'static str) {
    tauri::async_runtime::spawn(prompt_when_idle(terminal_id, prompt, label));
}

/// The body of [`spawn_prompt_when_idle`]; its ~3 min bound starts when this
/// future is first polled.
async fn prompt_when_idle(terminal_id: String, prompt: String, label: &'static str) {
    use tauri::Manager;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
    let mut debounce = ReadyDebounce::default();
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        let Some(app) = crate::tauri_app_handle::current() else {
            return;
        };
        let Some(tm) = app.try_state::<std::sync::Arc<crate::terminal::TerminalManager>>() else {
            return;
        };
        let Some(session) = tm.get(&terminal_id) else {
            info!(
                terminal_id,
                label, "prompt-when-idle: terminal gone — standing down"
            );
            return;
        };
        let screen = session.grid_screen();
        let refusal = match probe_idle_prompt(&screen) {
            IdlePromptProbe::Busy => {
                info!(
                    terminal_id,
                    label, "prompt-when-idle: session already working — no prompt needed"
                );
                return;
            }
            IdlePromptProbe::NotYet(why) => {
                debounce.reset();
                why
            }
            IdlePromptProbe::Ready if !debounce.settled(screen) => {
                "idle prompt seen once; waiting for an identical second read".to_string()
            }
            IdlePromptProbe::Ready => {
                // Composes both sides of the 2026-08-29 rebase: OUR generalized
                // `prompt`/`label` seam (one impl for the migration nudge and the
                // respawn prompt), with MAIN's `Ok(_)` — `submit_prompt` stopped
                // returning `()` in fee48d4c, which now reports the neutralized body.
                match session.submit_prompt(
                    &prompt,
                    crate::terminal::session::PtyWriteCaller::AccountMigration,
                ) {
                    Ok(_) => info!(terminal_id, label, "prompt-when-idle submitted"),
                    Err(e) => {
                        warn!(terminal_id, label, error = %e, "prompt-when-idle submit failed")
                    }
                }
                return;
            }
        };
        if std::time::Instant::now() >= deadline {
            warn!(
                terminal_id,
                label,
                last_refusal = %refusal,
                "prompt-when-idle: resumed CLI never painted its idle prompt within 3m — giving up"
            );
            return;
        }
    }
}

/// One resumed-Claude PTY spawn, described.
///
/// The parameter block for [`spawn_resumed_pane`], which is the shipped
/// `claude --permission-mode bypassPermissions --resume <sid>` respawn — step 3
/// of [`migrate_session`], lifted out verbatim so the cross-machine respawn
/// receiver (`crate::session::respawn`) reuses it rather than growing a second,
/// drifting copy of the launch-spec + capture-hint wiring.
pub(crate) struct ResumeSpawn<'a> {
    /// The Claude session id to `--resume`. NEVER synthesized: a caller with
    /// no id must fail rather than invent one.
    pub claude_session_id: &'a str,
    /// The session's working dir — both the PTY cwd and the transcript
    /// resolver's project path.
    pub working_dir: &'a str,
    /// Transcript to sniff the session's last real model from, so `--resume`
    /// does not silently drop to the target account's default model. A missing
    /// or unreadable file simply omits `--model`.
    pub model_transcript: std::path::PathBuf,
    /// `CLAUDE_CONFIG_DIR` for the new PTY — the TARGET account's dir. It also
    /// keeps the durable restore record consistent (the record stores the dir
    /// the session actually launched under).
    pub config_dir: &'a str,
    pub title: String,
    pub page_id: String,
    pub zone_index: i32,
    /// Work unit the resumed session belongs to, carried onto the new coord
    /// session's intent. `None` on the migration path (which keeps the coord
    /// row it already had).
    pub work_unit_slug: Option<String>,
    /// Correlation topic carried onto the new coord session's intent.
    pub correlation_topic: Option<String>,
    /// `intent.repo` for the new coord session. `None` leaves it unset — the
    /// PTY cwd is `working_dir` either way.
    pub intent_repo: Option<String>,
    /// Coord lineage to stamp on the new coord session row, when this respawn
    /// continues a known coord session. `None` = no lineage claim (the
    /// account-migration path, whose coord row is the same session moving).
    pub coord_lineage: Option<crate::commands::terminal::CoordSessionLineage>,
    /// Forwarded to the spawn-time resource gate. See the two call sites — they
    /// answer it differently and each says why.
    pub resource_override: bool,
    /// The gate continuation this respawn is carrying, when the session being
    /// resumed WAS one. Injects `QONTINUI_GATE_ID` /
    /// `QONTINUI_GATE_DEVICE_ID` into the new PTY so the resumed session can
    /// still report its own work outcome.
    ///
    /// `None` on the cross-machine respawn receiver, whose source session's
    /// continuation registration lives on a different device entirely.
    pub gate_identity: Option<crate::commands::terminal::GateIdentity>,
}

/// Spawn `claude --permission-mode bypassPermissions [--model m] --resume <sid>`
/// in a fresh PTY pinned to `spec.config_dir`, tracked in the lifecycle store
/// and mirrored into coord. Returns `(terminal_id, coord_session_id)`.
///
/// Same resume form the boot-restore path uses (post-#547): bypassPermissions
/// so the resumed session doesn't wedge on its first tool-approval prompt with
/// nobody watching.
///
/// The account pin is threaded through `capture_hint.config_dir`, which is BOTH
/// the `CLAUDE_CONFIG_DIR` handed to the PTY and the dir recorded on the durable
/// restore record — deliberately one value, so a restore cannot resurrect the
/// session under a different account than it ran on. It is never a
/// `switch_claude_account` mutation, which would leak this one spawn's account
/// choice into every later spawn on the box.
pub(crate) fn spawn_resumed_pane(
    app: &tauri::AppHandle,
    terminal_manager: &std::sync::Arc<crate::terminal::TerminalManager>,
    session_registry: &std::sync::Arc<crate::session::SessionRegistry>,
    spec: ResumeSpawn<'_>,
) -> Result<(String, Option<uuid::Uuid>), String> {
    let model = transcript_last_model(&spec.model_transcript);
    // Compose the respawn argv through the shared launch seam so the operator's
    // global + per-account (destination) launch flags layer onto the required
    // resume flags. `permission = BypassPermissions` preserves today's emitted
    // `--permission-mode bypassPermissions`. The #782 transcript-sniffed model is
    // fed as `spec.model`, so it wins over any template `--model` and the session
    // keeps its actual model across the hop. `resume_id` names the exact session
    // id. With no operator config the argv is the historical hand-built
    // `claude --permission-mode bypassPermissions [--model m] --resume <id>`
    // plus the `--settings <hook file>` pair below.
    let launch_cfg =
        crate::claude_session::launch_spec::LaunchConfig::from_settings(Some(spec.config_dir));
    let prompt_carrier = crate::session::spawn_prompt::resolve_system_prompt_carrier(None);
    let command = resume_respawn_argv(
        // The ABSOLUTE binary — see `resume_respawn_argv` for why the bare
        // `claude_bin_path()` cannot be the PTY child's program on Windows.
        &crate::agent_runtime::resolve_claude_bin(),
        spec.claude_session_id,
        model,
        &launch_cfg,
        // The hook carrier, spelled out because this respawn execs the
        // resolved `resolve_claude_bin()` DIRECTLY — the identity shim,
        // which is what appends `--settings` for a PATH-resolved `claude`,
        // is not in this chain. Without it the migrated session runs with no
        // `SessionStart` hook, and `SessionStart` on a `--resume` is exactly
        // when the policy injection matters most: the session carries its
        // old context but not the policies as they now stand
        // ([`crate::mcp::policy_context`]). Empty on a materialize failure
        // ⇒ no flag, which is the pre-existing behaviour.
        //
        // Plus the policy body at spawn (plan
        // `2026-09-15-runner-policy-injection-off-sessionstart-hook-channel`):
        // this respawn has no briefing, so it carries a BODY-ONLY composed
        // file when the tenant's cache exists, and nothing otherwise.
        resume_respawn_extra_required(
            // A migrated GATE continuation keeps its Stop-armed carrier.
            crate::session::claude_hook::spawn_settings_args(spec.gate_identity.is_some()),
            prompt_carrier.as_ref(),
        ),
    );
    let capture_hint = crate::commands::terminal::SessionCaptureHint {
        config_dir: Some(spec.config_dir.to_string()),
        working_dir: spec.working_dir.to_string(),
        title: spec.title.clone(),
        // `--resume` does not re-pass `--name`; the name persists via the transcript.
        spawn_name: None,
        page_id: Some(spec.page_id.clone()),
        // `--resume <id>` names the exact session id → synchronous pinned
        // record (same row, new terminal/account/zone) + verification arm.
        claude_session_id: Some(spec.claude_session_id.to_string()),
        zone_index: Some(spec.zone_index),
        // A resume preserves the original session's nature (operator or
        // autonomous); don't force the agent identity onto an unknown session.
        inject_agent_git_identity: false,
        // Carried, not re-claimed: an account migration moves the SAME session
        // to a new PTY, so the gate it was spawned for is still the gate it
        // must answer. Dropping it here would leave the resumed session unable
        // to report `work_completed` for work it goes on to finish.
        gate_identity: spec.gate_identity,
        coord_lineage: spec.coord_lineage,
        // From the SAME carrier the argv above was built from — withheld when
        // an operator template put a replacement prompt in that argv.
        policy_delivery: crate::session::spawn_prompt::delivery_unless_replacement(
            prompt_carrier.as_ref().and_then(|c| c.policy_delivery()),
            &command,
        ),
    };
    crate::commands::terminal::create_tracked_terminal_session_backend(
        terminal_manager,
        session_registry,
        app.clone(),
        spec.title,
        spec.working_dir.to_string(),
        spec.work_unit_slug,
        spec.correlation_topic,
        spec.intent_repo,
        Some(command),
        None,
        capture_hint,
        Some(spec.page_id),
        spec.resource_override,
    )
}

/// The full PTY-child argv of the `--resume` respawn. With no operator launch
/// template it is `[<claude_bin>, --permission-mode bypassPermissions,
/// [--model m], --resume <sid>, <extra_required…>]`; template flags land
/// between `--resume` and the tail (`compose_flags` step 4). Pure — the head
/// is whatever the caller passes — so the argv is assertable without a PTY
/// and without reading process-global env.
///
/// `claude_bin` MUST be the ABSOLUTE binary
/// ([`crate::agent_runtime::resolve_claude_bin`]), never the bare
/// `claude_bin_path()`. This argv becomes the PTY child's program
/// (`Some([program, args…])` → portable-pty `CommandBuilder::new(program)` →
/// Windows `CreateProcessW`), and a bare `claude` is searched against the
/// CHILD's PATH — where the always-on identity-shim dir is prepended — and
/// lands on the EXTENSIONLESS shim script, which `CreateProcessW` refuses with
/// `%1 is not a valid Win32 application (os error 193)`. Measured 2026-09-18:
/// three usage-limit migrations each closed the source terminal (the
/// pre-reorder [`migrate_session`] closed BEFORE it respawned) and then failed
/// at the respawn, so the pane vanished with nothing in its place (two more of
/// the same shape the day before). Same resolver the condition-check / gate-continuation PTY spawns
/// use.
///
/// Consequence worth knowing off Windows, where the extensionless shim was
/// launchable and this respawn used to go THROUGH it: the shim's `--resume`
/// branch also appended `--mcp-config $QONTINUI_MCP_CONFIG` and fired its
/// beacon. Execing the absolute binary drops both, exactly as the
/// gate-continuation direct spawn already does; `--settings` is covered by
/// `extra_required`, and a cwd that declares coord-mcp (every allocated
/// worktree) needs no injection.
///
/// `resolve_claude_bin` does blocking filesystem stats. Its callers here —
/// [`spawn_resumed_pane`] via [`migrate_session`] and the respawn receiver —
/// run on the blocking pool ([`migrate_session_off_runtime`], and the
/// receiver's own `spawn_blocking_tracked` around the transcript copy) or
/// alongside the PTY spawn itself, so a PATH walk widens nothing.
fn resume_respawn_argv(
    claude_bin: &str,
    claude_session_id: &str,
    model: Option<String>,
    launch_cfg: &crate::claude_session::launch_spec::LaunchConfig,
    extra_required: Vec<String>,
) -> Vec<String> {
    crate::claude_session::launch_spec::render_argv(
        &crate::claude_session::launch_spec::LaunchSpec {
            permission: crate::claude_session::launch_spec::PermissionMode::BypassPermissions,
            resume_id: Some(claude_session_id.to_string()),
            model,
            extra_required,
            ..Default::default()
        },
        launch_cfg,
        claude_bin,
    )
}

/// The verbatim `extra_required` tail of the `--resume` respawn: the hook
/// carrier's `--settings` pair, then the system-prompt carrier's pair when
/// there is one. Pure, so the argv shape is assertable without a PTY.
fn resume_respawn_extra_required(
    settings_args: Vec<String>,
    carrier: Option<&crate::session::spawn_prompt::SystemPromptCarrier>,
) -> Vec<String> {
    let mut out = settings_args;
    if let Some(carrier) = carrier {
        out.extend(carrier.argv());
    }
    out
}

/// [`migrate_session`] on the blocking pool rather than an async worker.
///
/// The migration is synchronous file and process work, and since the `<sid>/`
/// carry it can copy a whole session directory, which for a long fan-out can
/// be many files. A usage limit is per account, so every session on that
/// account migrates at about the same moment; run inline, each of those would
/// hold a tokio worker for the length of its copy. Both async callers (the
/// usage-limit hint and the manual Tauri command) go through here.
///
/// The whole migration runs under the session's in-flight guard
/// ([`begin_migration`]): a second concurrent migration of the same session,
/// or one whose view of the session is stale, is refused
/// ([`MigrateError::Refused`]) before anything is read or written. The guard
/// moves into the blocking task, so it is released when the migration itself
/// ends — on success, on error, or on a panic — even if this future is
/// dropped first; and a future dropped BEFORE the blocking task is spawned
/// drops the guard with it ([`run_migration_guarded`]).
///
/// The guard does NOT cover the detached step-4b delta sync that
/// [`migrate_session`] starts and returns without awaiting: it is released
/// while that sync may still be waiting for the old writer to exit. A fast
/// second hop is legitimate (the target can run dry too), and the sync needs
/// no guard for safety — it only writes a destination still exactly as its
/// carry left it. It does not make a fast second hop converge: its late writes
/// reach the next account only if they land before that hop's carry or sync
/// walks its source, and otherwise stay on the intermediate account until a
/// later migration or respawn picks them up. See the module doc.
pub(crate) async fn migrate_session_off_runtime(
    app: &tauri::AppHandle,
    record: &TerminalSessionRecord,
    src_config_dir: &str,
    dst_config_dir: &str,
) -> Result<MigrationOutcome, MigrateError> {
    use tauri::Manager;
    let store = app
        .try_state::<std::sync::Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>()
        .ok_or_else(|| MigrateError::Failed("SessionLifecycleStore not managed".to_string()))?
        .inner()
        .clone();
    let (app_owned, record_owned) = (app.clone(), record.clone());
    let (src, dst) = (src_config_dir.to_string(), dst_config_dir.to_string());
    run_migration_guarded(
        || {
            begin_migration(record, src_config_dir, || {
                store.get(&record.claude_session_id)
            })
        },
        // The old pane's `claude`, identified BEFORE anything is closed: the
        // detached delta sync waits for exactly this process to leave the
        // table. Taken here because the snapshot is async and
        // `migrate_session` is not.
        || old_pane_claude(app, &record.terminal_id),
        move |old_writer| migrate_session(&app_owned, &record_owned, &src, &dst, old_writer),
    )
    .await
    .inspect_err(|e| {
        if let MigrateError::Refused(refusal) = e {
            info!(session = %record.claude_session_id, %refusal, "session migration refused");
        }
    })
}

/// The guard-holding shape of [`migrate_session_off_runtime`], free of the app
/// handle so it is testable: take the in-flight slot (`begin`), run the async
/// `prepare`, then run `work` on the blocking pool with the guard moved into
/// it. The guard is a local of this future until that spawn, so dropping the
/// future at any await before it releases the slot; after it, the blocking
/// task owns the guard and releases it when `work` ends.
async fn run_migration_guarded<B, P, PF, W, T, E>(
    begin: B,
    prepare: P,
    work: W,
) -> Result<T, MigrateError>
where
    B: FnOnce() -> Result<MigrationInFlight, MigrationRefusal>,
    P: FnOnce() -> PF,
    PF: std::future::Future,
    PF::Output: Send + 'static,
    W: FnOnce(PF::Output) -> Result<T, E> + Send + 'static,
    T: Send + 'static,
    E: Send + 'static,
    MigrateError: From<E>,
{
    let in_flight = begin().map_err(MigrateError::Refused)?;
    let prepared = prepare().await;
    qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked(move || {
        let _in_flight = in_flight;
        work(prepared)
    })
    .await
    .map_err(|e| MigrateError::Failed(format!("migration task did not complete: {e}")))?
    .map_err(MigrateError::from)
}

/// The `claude` anchor (oldest `claude` image, pid + start time) in the pane
/// `terminal_id`, from one process-table snapshot. `None` when the pane is
/// gone, remote (no local pid), hosts no `claude`, or the table is unreadable.
async fn old_pane_claude(app: &tauri::AppHandle, terminal_id: &str) -> Option<ProcIdentity> {
    use tauri::Manager;
    let manager = app
        .try_state::<std::sync::Arc<crate::terminal::TerminalManager>>()?
        .inner()
        .clone();
    let root = manager.get(terminal_id)?.pane_root_pid()?;
    let snapshot = crate::process_capture::process_tree::snapshot_process_table_public().await;
    let (pid, started_at) =
        crate::process_capture::process_tree::claude_anchor_in_subtree(root, &snapshot)?;
    Some(ProcIdentity { pid, started_at })
}

/// Perform the migration mechanics: transcript copy → trust for the
/// destination → respawn under the target account (which re-keys the
/// lifecycle row onto the new PTY) → close the old pane → emit event.
///
/// **The old pane closes only after the respawn has succeeded.** Any failure
/// before that — transcript, trust, the spawn itself — returns with the old
/// pane untouched (and a session closed or moved while the carry ran is
/// refused just before the respawn, [`still_migratable`]), still sitting at its usage-limit prompt, and the caller
/// emits `skipped`. Until 2026-09-18 the order was close-then-respawn, so a
/// respawn failure (three that day, from a resolver defect) was a vanished
/// window with nothing in its place. For a moment two PTYs now carry one
/// session; the old one is idle at a limit prompt and consumes nothing.
///
/// Shared by the automatic path ([`handle_usage_limit_hint`], which has
/// already confirmed exhaustion) and the manual Tauri command
/// (`terminal_migrate_session_account`, where the operator's click is the
/// confirmation).
///
/// Private on purpose: the only caller is [`migrate_session_off_runtime`],
/// which holds the per-session in-flight guard across it, so the compiler —
/// not a comment — keeps it from running unguarded.
#[expect(
    clippy::string_slice,
    reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
)]
fn migrate_session(
    app: &tauri::AppHandle,
    record: &TerminalSessionRecord,
    src_config_dir: &str,
    dst_config_dir: &str,
    old_writer: Option<ProcIdentity>,
) -> Result<MigrationOutcome, MigrateError> {
    use tauri::Manager;

    if crate::model_gateway::gateway_declared() {
        return Err(crate::model_gateway::SUBSCRIPTION_OFF_REASON.into());
    }

    let working_dir = record.working_dir.clone().ok_or_else(|| {
        "session record has no working_dir — cannot locate transcript".to_string()
    })?;

    let terminal_manager = app
        .try_state::<std::sync::Arc<crate::terminal::TerminalManager>>()
        .ok_or("TerminalManager not managed")?
        .inner()
        .clone();
    let session_registry = app
        .try_state::<std::sync::Arc<crate::session::SessionRegistry>>()
        .ok_or("SessionRegistry not managed")?
        .inner()
        .clone();
    let store = app
        .try_state::<std::sync::Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>()
        .ok_or("SessionLifecycleStore not managed")?
        .inner()
        .clone();

    // 1. Transcript first — if this fails the old pane is left untouched.
    // A destination `<sid>.jsonl` that diverged from the live one is moved
    // aside (`<sid>.jsonl.diverged-<ms>`) so the live conversation is what
    // resumes; when it cannot be moved aside, this step fails. The `<sid>/`
    // carry inside it is best-effort; its tally rides the event.
    let carried = copy_transcript(
        src_config_dir,
        dst_config_dir,
        &working_dir,
        &record.claude_session_id,
    )?;

    // 1b. Workspace trust for the DESTINATION account, decided up front. The
    // respawn below reaches `TerminalManager::create` on its pinned arm, which
    // derives trust for `dst` through the trust gate and refuses when it
    // cannot. Since 2026-09-18 the old pane is closed only AFTER the respawn
    // succeeds, so a refusal inside the spawn no longer destroys anything —
    // but deciding it here still gives the operator a typed, named refusal
    // (the caller emits `skipped` with it) instead of a generic spawn failure,
    // and a grant means the seam's own check short-circuits on
    // `AlreadyTrusted`. The destination is not pre-trusted by the source's
    // spawn (that spawn mints for its own account only), so this is the usual
    // case, not an edge: trust the session had under `src` is not the same key
    // under `dst`.
    let trust = crate::claude_session::trust_gate::pre_accept_for_account_sync(
        &working_dir,
        Some(dst_config_dir),
        crate::claude_session::trust_gate::SpawnSurface::InteractivePty,
    );
    if let Some(refusal) = trust.decision.refusal() {
        return Err(format!(
            "workspace trust for the destination account could not be derived — the old pane is left in place: {refusal}"
        )
        .into());
    }

    // 1c. Re-read the session's record, still under the in-flight guard, now
    // that the carry is done: the operator may have closed the pane (or the
    // session moved) since `begin_migration` read it, and a closed pane must
    // not come back as a respawn. Abort with the refusal before anything is
    // lifted or spawned; the carried files stay at the destination, which a
    // carry never makes worse. Above the step-2 lift on purpose (see GUARD).
    still_migratable(record, src_config_dir, store.get(&record.claude_session_id))
        .map_err(MigrateError::Refused)?;

    // 2. Lift any gate-continuation registration off the old terminal
    // BEFORE the respawn: the new PTY is spawned with the carried gate identity
    // and the registration is re-pinned onto it on success. On failure it goes
    // straight back onto the old terminal, which is still alive — so the
    // liveness reaper never sees a registration on a dead pane and never posts
    // a false `work_unreported` for a session that merely failed to move.
    // (Coord admits exactly one `spawned → work_*` transition, so that false
    // negative would be irreversible.)
    //
    // The lift hands back an OWNED reservation on the continuation's anchor,
    // taken under the same lock that removed the live row. It is held across
    // the synchronous respawn below and settled by whichever re-pin arm runs,
    // so the anchor is never in neither half of the registry and a same-anchor
    // continuation dispatched during the hop is deferred by P3 rather than
    // spawned alongside this one.
    //
    // GUARD: anchor-held window OPENS here and closes at the
    // `restore_continuation_registration` call INSIDE each arm of the
    // `match spawned` below — see the matching GUARD marker there, which is
    // where the arms diverge, not where the window ends. NOTHING between the
    // lift and those two calls may exit early
    // — no `?`, no `return`, no `unwrap`/`expect` you add. Every fallible step
    // of this function is deliberately ABOVE this line (the `working_dir` and
    // three `try_state` lookups, `copy_transcript`, the trust-gate refusal,
    // and the step-1c record re-check), and it must stay that way. An early exit inserted inside the
    // window drops `carried_continuation`, whose RAII drop releases the
    // anchor, which silently reopens the double-spawn window this lift exists
    // to close — with NO compiler warning, no clippy lint and no failing test,
    // because `#[must_use]` does not fire on a value that IS bound and no test
    // calls `migrate_session`. The RAII drop is the PANIC backstop only; the
    // no-early-exit property is a reviewer obligation. If you need a new
    // failure check here, put it above the lift or restructure so the compiler
    // can enforce the pairing.
    let carried_continuation =
        crate::agent_runtime::take_continuation_registration(&record.terminal_id);

    // 3. Respawn under the target account, through the SHARED resume seam
    // ([`spawn_resumed_pane`]) — the same code path the respawn receiver
    // (`crate::session::respawn`) uses, so the two never drift. The pinned-id
    // capture hint inside `create_terminal_session_backend` re-keys the SAME
    // lifecycle row (`--resume <id>`) onto the new terminal/account
    // synchronously, preserving page/zone; the old PTY is still running at
    // this point and no longer owns a row.
    let title = record.title.clone().unwrap_or_else(|| {
        format!(
            "Resumed {}",
            &record.claude_session_id[..8.min(record.claude_session_id.len())]
        )
    });
    // The gate the carried continuation answers, paired with THIS device's id —
    // the consuming device coord keys the outcome write on. Absent when the
    // session was not a continuation, or when the local device id is unreadable
    // (in which case nothing could report anyway).
    let carried_gate_identity = carried_continuation.as_ref().and_then(|(c, _reservation)| {
        let gate_id = c.gate_id?;
        let device_id = crate::agent_runtime::load_local_device_id()?;
        Some(crate::commands::terminal::GateIdentity {
            gate_id,
            consuming_device_id: device_id,
        })
    });
    let spawned = spawn_resumed_pane(
        app,
        &terminal_manager,
        &session_registry,
        ResumeSpawn {
            claude_session_id: &record.claude_session_id,
            working_dir: &working_dir,
            // Model is sniffed off the SOURCE transcript: `--resume` alone
            // restores the conversation but the model falls back to the target
            // account's default.
            model_transcript: super::transcript::session_transcript_path(
                Path::new(src_config_dir),
                &working_dir,
                &record.claude_session_id,
            ),
            config_dir: dst_config_dir,
            title: title.clone(),
            page_id: record.page_id.clone(),
            zone_index: record.zone_index,
            // The migration keeps the coord row it already had — it claims no
            // new work-unit / topic / repo, and no lineage.
            work_unit_slug: None,
            correlation_topic: None,
            intent_repo: None,
            coord_lineage: None,
            // OVERRIDE — the one call site that passes `true`, and the only one
            // that should. An account migration is not the creation of a new
            // session: the operator's session already exists, and this respawn
            // is the first half of a move whose second half (step 4) closes the
            // old PTY the moment this one is up. Refusing here would not
            // protect a live session — it would strand one at a usage-limit
            // prompt, which inverts the whole point of the guard (plan §Part D
            // step 5: never touch an already-running session; this gate fires
            // only when something NEW is created). The migration is
            // commit-neutral by construction: one `claude` comes up, one goes
            // away a moment later, and the one going away is idle at a limit
            // prompt. A RESPAWN, by contrast, creates something genuinely new
            // (its source is already closed) and passes `false`.
            resource_override: true,
            gate_identity: carried_gate_identity,
        },
    );
    // GUARD: anchor-held window closes at the `restore_continuation_registration`
    // call INSIDE each arm below, not at this line — an early exit added in
    // the `Ok` arm before that call would still drop the permit and free the
    // anchor. This marker is where the two arms diverge; see the matching
    // GUARD marker at the lift for what may not appear anywhere in between.
    let new_terminal_id = match spawned {
        Ok((terminal_id, _coord_session_id)) => {
            // Re-pin the continuation onto the terminal that now hosts it, so
            // the registry's dedup/cap view stays true and the eventual real
            // exit still reports an outcome. The permit taken by the lift is
            // handed on here: the live row replaces the reservation under one
            // lock, so the anchor moves from held-by-permit to
            // held-by-session with no gap.
            if let Some((carried, reservation)) = carried_continuation {
                crate::agent_runtime::restore_continuation_registration(
                    terminal_id.clone(),
                    carried,
                    reservation,
                );
            }
            terminal_id
        }
        Err(e) => {
            // The respawn failed and the old PTY is UNTOUCHED — the session is
            // exactly where it was, at its usage-limit prompt. Put the
            // registration back on it so the eventual real exit reports
            // honestly. The caller emits `skipped` with this error. The permit
            // goes with it — the failed arm settles the reservation exactly
            // like the successful one, just onto the OLD terminal id.
            if let Some((carried, reservation)) = carried_continuation {
                crate::agent_runtime::restore_continuation_registration(
                    record.terminal_id.clone(),
                    carried,
                    reservation,
                );
            }
            return Err(e.into());
        }
    };

    // 4. The replacement is up: close the old pane. Its lifecycle row already
    // belongs to the new terminal (step 3), so the exit hook's later
    // `(session id, old terminal)` close must not be resolved against the map
    // — it would read as a mis-bound `NotFound`. Mark the hand-off first so the
    // store answers that close with `TerminalSuperseded`: no mutation, no coord
    // `Closed`, and the workdir nonce the resumed pane still uses is kept.
    // There is no close-reason to record because the row is never closed —
    // which is also what keeps boot-restore from ever offering the old pane.
    store.mark_terminal_superseded(&record.terminal_id, &new_terminal_id);
    if let Err(e) = terminal_manager.close(&record.terminal_id) {
        // Old pane was already gone (user closed it after exhaustion) — not
        // fatal, the replacement is what matters. Its exit-close then either
        // arrived before the mark or never will, so withdraw the mark rather
        // than leave an entry nothing consumes.
        store.unmark_terminal_superseded(&record.terminal_id);
        info!(
            terminal_id = %record.terminal_id,
            new_terminal_id = %new_terminal_id,
            error = %e,
            "old terminal close failed after a successful respawn"
        );
    }

    // 4b. Top up `<sid>/` with whatever the old pane wrote between the carry
    // and its exit — detached, because `close` does not wait for the `claude`
    // and this fn is synchronous. It waits (bounded) until `old_writer` is
    // gone from the whole process table and never touches `<sid>.jsonl`; for
    // a side-car file it backs out if the destination changed since the
    // carry — best-effort, see `session_carry::delta_sync`. Runs after a
    // failed close too: the pane was already gone, and the wait still decides.
    let sync_done = crate::terminal::session_carry::spawn_delta_sync_after_exit(
        old_writer,
        record.claude_session_id.clone(),
        carried.clone(),
    );

    // 5. Optionally nudge the resumed session to pick the task back up once
    // the CLI paints its idle prompt — a bare `--resume` restores context but
    // then sits at the input box with nobody typing. The nudge waits for 4b's
    // outcome first, bounded by 4b's writer-exit wait plus a report margin
    // (`NUDGE_SYNC_WAIT`), so it normally goes after the sync.
    if crate::settings::get_ai_settings()
        .claude_cli
        .auto_continue_after_migration
    {
        spawn_continue_nudge(new_terminal_id.clone(), carried.clone(), sync_done);
    }

    let outcome = MigrationOutcome {
        new_terminal_id: new_terminal_id.clone(),
        from_config_dir: src_config_dir.to_string(),
        to_config_dir: dst_config_dir.to_string(),
    };
    if let Err(e) = app.emit(
        "session-account-migrated",
        json!({
            "claudeSessionId": record.claude_session_id,
            "fromConfigDir": outcome.from_config_dir,
            "toConfigDir": outcome.to_config_dir,
            "newTerminalId": outcome.new_terminal_id,
            "pageId": record.page_id,
            "zoneIndex": record.zone_index,
            // What happened to the main `<sid>.jsonl`; kept OUT of
            // `sessionDirCarry`, which tallies `<sid>/` only. `.._backup` is
            // set only for `diverged_preserved`.
            "mainTranscript": carried.main.event_name(),
            "mainTranscriptBackup": carried.main.backup().map(|p| p.display().to_string()),
            // `subagentsDivergedPreserved`: `.jsonl` files under `<sid>/` whose
            // diverged destination was moved aside (`<file>.diverged-<ms>`)
            // and replaced by the live one. `subagentsDivergedKept`: diverged
            // ones kept in place instead — a non-migration carry mode only,
            // so 0 here today; reported apart rather than folded into
            // `skippedOther`, which means "not followed".
            "sessionDirCarry": {
                "copied": carried.session_dir.copied,
                "skippedPresent": carried.session_dir.skipped_present,
                "skippedOther": carried.session_dir.skipped_other,
                "subagentsDivergedPreserved": carried.session_dir.diverged_preserved,
                "subagentsDivergedKept": carried.session_dir.diverged_kept,
                "failed": carried.session_dir.failed,
            },
            "subagentsCarried": carried.subagents.len(),
        }),
    ) {
        warn!(error = %e, "failed to emit session-account-migrated");
    }
    Ok(outcome)
}

fn emit_skipped(app: &tauri::AppHandle, record: &TerminalSessionRecord, src: &str, reason: &str) {
    if let Err(e) = app.emit(
        "session-account-migration-skipped",
        json!({
            "claudeSessionId": record.claude_session_id,
            "fromConfigDir": src,
            "reason": reason,
        }),
    ) {
        warn!(error = %e, "failed to emit session-account-migration-skipped");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- idle-prompt watcher -------------------------------------------------

    fn v2_screen(
        rows: &[&str],
        cursor_row: u16,
        cursor_col: u16,
    ) -> crate::terminal::graceful_exit::ScreenText {
        crate::terminal::graceful_exit::ScreenText {
            lines: rows.iter().map(|r| r.to_string()).collect(),
            cursor_row,
            cursor_col,
        }
    }

    /// The screen every resumed session actually paints: bypass mode, so the
    /// footer carries no `? for shortcuts`. The old literal check never matched
    /// it, and the watcher gave up on every migration nudge and respawn prompt.
    #[test]
    fn an_idle_bypass_mode_prompt_is_ready_without_the_shortcuts_hint() {
        let rule = "─".repeat(75);
        let s = v2_screen(
            &[
                "● Done — the branch is pushed.",
                &rule,
                "❯\u{a0}Try \"write a test for <filepath>\"",
                &rule,
                "  ⏵⏵ bypass permissions on (shift+tab to cycle) · ← for agents",
            ],
            2,
            2,
        );
        assert!(
            !super::super::output_scan::normalize(&s.lines.join("\n")).contains("? for shortcuts")
        );
        assert_eq!(probe_idle_prompt(&s), IdlePromptProbe::Ready);
    }

    #[test]
    fn a_working_pane_is_busy_even_with_a_prompt_on_screen() {
        let rule = "─".repeat(75);
        let s = v2_screen(
            &[
                "✻ Compiling… (esc to interrupt)",
                &rule,
                "❯\u{a0}",
                &rule,
                "  ⏵⏵ bypass permissions on (shift+tab to cycle)",
            ],
            2,
            2,
        );
        assert_eq!(probe_idle_prompt(&s), IdlePromptProbe::Busy);
    }

    /// An operator draft on the input line must never receive the prompt
    /// appended to it.
    #[test]
    fn an_unsent_draft_is_not_ready() {
        let rule = "─".repeat(75);
        let s = v2_screen(
            &[
                &rule,
                "❯\u{a0}half typed",
                &rule,
                "  ⏵⏵ bypass permissions on (shift+tab to cycle)",
            ],
            1,
            12,
        );
        assert!(matches!(probe_idle_prompt(&s), IdlePromptProbe::NotYet(_)));
    }

    #[test]
    fn a_pane_with_no_prompt_drawn_yet_is_not_ready() {
        let s = v2_screen(
            &[
                "$ claude --permission-mode bypassPermissions --resume abc",
                "",
            ],
            1,
            0,
        );
        assert!(matches!(probe_idle_prompt(&s), IdlePromptProbe::NotYet(_)));
    }

    /// The old literal readiness check read the whole screen, so a transcript
    /// row quoting the default-mode hint must not admit an operator's draft.
    #[test]
    fn a_quoted_shortcuts_hint_does_not_admit_a_draft() {
        let rule = "─".repeat(75);
        let s = v2_screen(
            &[
                "● The footer used to read `? for shortcuts`.",
                &rule,
                "❯\u{a0}half typed",
                &rule,
                "  ⏵⏵ bypass permissions on (shift+tab to cycle)",
            ],
            2,
            12,
        );
        assert!(matches!(probe_idle_prompt(&s), IdlePromptProbe::NotYet(_)));
    }

    #[test]
    fn ready_is_submitted_only_on_a_second_identical_read() {
        let first = v2_screen(&["❯\u{a0}"], 0, 2);
        let mut changed = first.clone();
        changed
            .lines
            .insert(0, "  ⎿ SessionStart hook loading".to_string());
        let mut d = ReadyDebounce::default();
        assert!(!d.settled(first.clone()), "one read is never enough");
        assert!(
            !d.settled(changed.clone()),
            "the screen moved between reads"
        );
        assert!(
            d.settled(changed.clone()),
            "two identical idle reads settle"
        );
        d.reset();
        assert!(
            !d.settled(changed),
            "a refusal in between restarts the count"
        );
    }

    /// The `--resume` respawn carries the hook carrier and, with a cached body,
    /// the body-only file — and never the inline flag, since it has no briefing.
    #[test]
    fn resume_respawn_tail_carries_settings_then_the_body_file_only_when_resolved() {
        use crate::session::spawn_prompt::{SystemPromptCarrier, APPEND_SYSTEM_PROMPT_FLAG};
        let settings = vec!["--settings".to_string(), "/h/s.json".to_string()];
        let file = SystemPromptCarrier::File {
            path: std::path::PathBuf::from("/h/spawn-prompts/spawn-1.md"),
            policy_sha: "cd".repeat(32),
        };
        assert_eq!(
            resume_respawn_extra_required(settings.clone(), Some(&file)),
            vec![
                "--settings",
                "/h/s.json",
                "--append-system-prompt-file",
                "/h/spawn-prompts/spawn-1.md"
            ]
        );
        let bare = resume_respawn_extra_required(settings.clone(), None);
        assert_eq!(bare, settings, "no cached body ⇒ exactly today's tail");
        assert!(!bare.iter().any(|a| a == APPEND_SYSTEM_PROMPT_FLAG));
    }

    /// The 2026-09-18 regression: the respawn argv's head was the bare
    /// `claude_bin_path()`, which the PTY spawn resolved against the child's
    /// PATH to the extensionless identity-shim script and `CreateProcessW`
    /// refused (os error 193) — after the source pane had already been closed.
    /// The head must be exactly the binary the caller resolved, verbatim, with
    /// the flags the migration has always carried behind it. The head is passed
    /// in rather than read from the resolver so this reads no process-global
    /// env (`PATH`, `QONTINUI_CLAUDE_BIN`) that other tests in this binary
    /// mutate; the one-line wiring in `spawn_resumed_pane` is the
    /// `resolve_claude_bin()` call itself.
    #[test]
    fn resume_respawn_argv_head_is_the_binary_the_caller_resolved() {
        let bin = if cfg!(windows) {
            r"C:\fake\bin\claude.exe"
        } else {
            "/fake/bin/claude"
        };
        let argv = resume_respawn_argv(
            bin,
            "7b4a9fb4-5526-4ab0-b89e-f13b6316874a",
            Some("claude-opus-5".to_string()),
            &crate::claude_session::launch_spec::LaunchConfig::default(),
            vec!["--settings".to_string(), "hooks.json".to_string()],
        );
        assert_eq!(
            argv[0], bin,
            "head must be the resolved binary verbatim: {argv:?}"
        );
        assert_ne!(argv[0], "claude", "the bare name is the os-error-193 shape");
        assert!(
            argv.windows(2)
                .any(|w| w == ["--permission-mode", "bypassPermissions"]),
            "{argv:?}"
        );
        assert!(
            argv.windows(2).any(|w| w == ["--model", "claude-opus-5"]),
            "{argv:?}"
        );
        assert!(
            argv.windows(2)
                .any(|w| w == ["--resume", "7b4a9fb4-5526-4ab0-b89e-f13b6316874a"]),
            "{argv:?}"
        );
        assert!(
            argv.windows(2).any(|w| w == ["--settings", "hooks.json"]),
            "extra_required tail must survive: {argv:?}"
        );
    }

    #[test]
    fn migration_cap_allows_then_blocks() {
        let sid = "test/account_migration/cap-session";
        let base = 1_750_000_000_000i64;
        assert!(migration_cap_permits(sid, base));
        assert!(migration_cap_permits(sid, base + 1000));
        assert!(migration_cap_permits(sid, base + 2000));
        assert!(
            !migration_cap_permits(sid, base + 3000),
            "4th migration within the window must be blocked"
        );
        // Outside the 24h window the old entries age out.
        assert!(migration_cap_permits(
            sid,
            base + MIGRATION_CAP_WINDOW_MS + 4000
        ));
    }

    // ---- one migration in flight per session (W1) ----------------------------

    /// A minimal open record for `sid` in `terminal`, under `config_dir`.
    fn session_record(
        sid: &str,
        terminal: &str,
        config_dir: Option<&str>,
    ) -> TerminalSessionRecord {
        serde_json::from_value(json!({
            "claudeSessionId": sid,
            "configDir": config_dir,
            "workingDir": "D:\\qontinui-root",
            "pageId": "default",
            "zoneIndex": 0,
            "terminalId": terminal,
            "openedAt": 1,
            "lastSeenAt": 1,
            "state": "open",
            "finishSynced": false,
        }))
        .expect("a minimal TerminalSessionRecord")
    }

    #[test]
    fn a_second_migration_of_the_same_session_is_refused_while_one_is_in_flight() {
        let sid = "test/account_migration/in-flight";
        let rec = session_record(sid, "t-1", Some("C:/a"));
        let held = begin_migration(&rec, "C:/a", || Some(rec.clone())).expect("first one starts");
        let refusal = begin_migration(&rec, "C:/a", || Some(rec.clone())).unwrap_err();
        assert_eq!(
            refusal,
            MigrationRefusal::InFlight {
                session: sid.to_string()
            }
        );
        assert_eq!(
            refusal.to_string(),
            format!("migration already in flight for {sid}")
        );
        let other = session_record("test/account_migration/in-flight-other", "t-2", None);
        assert!(
            begin_migration(&other, "C:/a", || Some(other.clone())).is_ok(),
            "another session is not blocked"
        );
        drop(held);
        assert!(
            begin_migration(&rec, "C:/a", || Some(rec.clone())).is_ok(),
            "the slot is free once the first migration ends"
        );
    }

    #[test]
    fn the_in_flight_slot_is_released_after_an_error_return_a_panic_and_a_moved_refusal() {
        let sid = "test/account_migration/release";
        let rec = session_record(sid, "t-1", Some("C:/a"));
        // An early error return (`?`) while the guard is held.
        let failing = || -> Result<(), String> {
            let _guard =
                begin_migration(&rec, "C:/a", || Some(rec.clone())).map_err(|r| r.to_string())?;
            Err("the carry failed".to_string())
        };
        assert!(failing().is_err());
        assert!(begin_migration(&rec, "C:/a", || Some(rec.clone())).is_ok());
        // A panic while the guard is held.
        let panicked = std::panic::catch_unwind(|| {
            let _guard = begin_migration(&rec, "C:/a", || Some(rec.clone())).unwrap();
            panic!("the migration panicked");
        });
        assert!(panicked.is_err());
        assert!(begin_migration(&rec, "C:/a", || Some(rec.clone())).is_ok());
        // A refusal by the moved-check releases the slot it took.
        assert!(begin_migration(&rec, "C:/stale", || Some(rec.clone())).is_err());
        assert!(begin_migration(&rec, "C:/a", || Some(rec.clone())).is_ok());
    }

    #[test]
    fn a_session_that_already_moved_is_refused() {
        let sid = "test/account_migration/moved";
        let seen = session_record(sid, "t-1", Some("C:/a"));
        // The record now says the session runs under C:/b (a migration that
        // finished while this one waited).
        let now = session_record(sid, "t-9", Some("C:/b"));
        let refusal = begin_migration(&seen, "C:/a", || Some(now.clone())).unwrap_err();
        assert_eq!(
            refusal,
            MigrationRefusal::AlreadyMoved {
                to: "C:/b".to_string()
            }
        );
        assert_eq!(refusal.to_string(), "session already moved to C:/b");
        // Same account, another terminal: moved too.
        let rehosted = session_record(sid, "t-9", Some("C:/a"));
        assert!(matches!(
            begin_migration(&seen, "C:/a", || Some(rehosted.clone())),
            Err(MigrationRefusal::AlreadyMoved { .. })
        ));
        // Gone from the store.
        assert!(matches!(
            begin_migration(&seen, "C:/a", || None),
            Err(MigrationRefusal::Unknown { .. })
        ));
        // Unchanged, or never pinned to an account: proceeds.
        assert!(begin_migration(&seen, "C:/a", || Some(seen.clone())).is_ok());
        let unpinned = session_record(sid, "t-1", None);
        assert!(begin_migration(&unpinned, "C:/a", || Some(unpinned.clone())).is_ok());
    }

    // ---- the guard's async shape (S6) ---------------------------------------

    /// The migration future dropped while it is still in its async prepare
    /// step — before the blocking task that would take over the guard was
    /// spawned — releases the slot with it.
    #[tokio::test]
    async fn dropping_the_migration_before_its_blocking_spawn_releases_the_slot() {
        use std::future::Future;
        let sid = "test/account_migration/dropped-before-spawn";
        let rec = session_record(sid, "t-1", Some("C:/a"));
        let mut migration = Box::pin(run_migration_guarded(
            || begin_migration(&rec, "C:/a", || Some(rec.clone())),
            // `old_pane_claude`'s stand-in: never completes.
            std::future::pending::<()>,
            |()| -> Result<(), String> { unreachable!("the blocking work is never spawned") },
        ));
        // One poll: the guard is taken, then the future parks in `prepare`.
        std::future::poll_fn(|cx| {
            assert!(migration.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        assert!(
            matches!(
                begin_migration(&rec, "C:/a", || Some(rec.clone())),
                Err(MigrationRefusal::InFlight { .. })
            ),
            "the parked migration holds the slot"
        );
        drop(migration);
        assert!(
            begin_migration(&rec, "C:/a", || Some(rec.clone())).is_ok(),
            "dropping it before the blocking spawn released the slot"
        );
    }

    /// Held through the blocking work, released when it ends; a failure and a
    /// refusal are typed apart, and a refusal prepares and runs nothing.
    #[tokio::test]
    async fn the_guarded_migration_holds_the_slot_through_its_work_and_types_a_refusal() {
        let sid = "test/account_migration/guarded";
        let rec = session_record(sid, "t-1", Some("C:/a"));
        let probe = rec.clone();
        let out = run_migration_guarded(
            || begin_migration(&rec, "C:/a", || Some(rec.clone())),
            || std::future::ready(7),
            move |prepared| -> Result<_, String> {
                let refused = matches!(
                    begin_migration(&probe, "C:/a", || Some(probe.clone())),
                    Err(MigrationRefusal::InFlight { .. })
                );
                Ok((prepared, refused))
            },
        )
        .await
        .expect("the migration runs");
        assert_eq!(
            out,
            (7, true),
            "prepare's output reaches the work, under the guard"
        );
        assert!(begin_migration(&rec, "C:/a", || Some(rec.clone())).is_ok());

        let failed = run_migration_guarded(
            || begin_migration(&rec, "C:/a", || Some(rec.clone())),
            || std::future::ready(()),
            |()| -> Result<(), String> { Err("the carry failed".to_string()) },
        )
        .await;
        assert!(matches!(failed, Err(MigrateError::Failed(ref e)) if e == "the carry failed"));
        assert!(begin_migration(&rec, "C:/a", || Some(rec.clone())).is_ok());

        let refused = run_migration_guarded(
            || begin_migration(&rec, "C:/stale", || Some(rec.clone())),
            || async { panic!("a refused migration prepares nothing") },
            |()| -> Result<(), String> { unreachable!() },
        )
        .await;
        let Err(MigrateError::Refused(refusal)) = refused else {
            panic!("expected a refusal, got {refused:?}");
        };
        assert_eq!(
            MigrateError::Refused(refusal.clone()).to_string(),
            refusal.to_string(),
            "the manual command's UI string is the refusal's own sentence"
        );
    }

    // ---- the auto path's answer to a refusal (W1) ----------------------------

    /// What `settle_hint_migration` did: (unrecorded, skipped reason).
    fn settle(
        sid: &str,
        stamp: i64,
        result: Result<MigrationOutcome, MigrateError>,
    ) -> (bool, Option<String>) {
        let mut unrecorded = false;
        let mut skipped = None;
        settle_hint_migration(
            sid,
            stamp,
            result,
            || unrecorded = true,
            |reason| skipped = Some(reason.to_string()),
        );
        (unrecorded, skipped)
    }

    /// How many more migrations the cap admits for `sid` at `now` (consumes
    /// them).
    fn cap_left(sid: &str, now: i64) -> usize {
        (0..=MIGRATION_CAP)
            .take_while(|i| migration_cap_permits(sid, now + *i as i64))
            .count()
    }

    #[test]
    fn a_refused_auto_migration_reports_nothing_and_gives_its_cap_slot_back() {
        let base = 1_760_000_000_000i64;
        for (i, refusal) in [
            MigrationRefusal::InFlight {
                session: "s".to_string(),
            },
            MigrationRefusal::AlreadyMoved {
                to: "C:/b".to_string(),
            },
        ]
        .into_iter()
        .enumerate()
        {
            let sid = format!("test/account_migration/refused-{i}");
            assert!(migration_cap_permits(&sid, base), "the hint takes a slot");
            let (unrecorded, skipped) = settle(&sid, base, Err(MigrateError::Refused(refusal)));
            assert!(unrecorded, "the picked target must not count as a handoff");
            assert_eq!(skipped, None, "no session-account-migration-skipped");
            assert_eq!(
                cap_left(&sid, base + 10),
                MIGRATION_CAP,
                "the refused hint's slot was given back"
            );
        }
    }

    /// S3 — `SessionLifecycleStore::get` answers closed rows too: a session
    /// whose record is closed by the time the guard re-reads it has no pane to
    /// migrate, and is refused `Closed` on either path (both go through
    /// `begin_migration`). The auto path treats that like any refusal: nothing
    /// reported, the cap slot given back. The slot is released, and a closed
    /// record that also moved is still refused as closed.
    #[test]
    fn a_closed_session_is_refused_and_the_auto_path_refunds_its_cap_slot() {
        let sid = "test/account_migration/closed";
        let seen = session_record(sid, "t-1", Some("C:/a"));
        let mut closed = seen.clone();
        closed.state = "closed".to_string();
        let refusal = begin_migration(&seen, "C:/a", || Some(closed.clone())).unwrap_err();
        assert_eq!(
            refusal,
            MigrationRefusal::Closed {
                session: sid.to_string()
            }
        );
        assert_eq!(
            refusal.to_string(),
            format!("session {sid} is closed — there is no pane to migrate")
        );
        let mut closed_elsewhere = session_record(sid, "t-9", Some("C:/b"));
        closed_elsewhere.state = "closed".to_string();
        assert!(matches!(
            begin_migration(&seen, "C:/a", || Some(closed_elsewhere.clone())),
            Err(MigrationRefusal::Closed { .. })
        ));
        assert!(
            begin_migration(&seen, "C:/a", || Some(seen.clone())).is_ok(),
            "the refusal released the slot, and an open record proceeds"
        );

        let base = 1_760_000_000_000i64;
        assert!(migration_cap_permits(sid, base), "the hint takes a slot");
        let (unrecorded, skipped) = settle(sid, base, Err(MigrateError::Refused(refusal)));
        assert!(unrecorded);
        assert_eq!(skipped, None, "no session-account-migration-skipped");
        assert_eq!(cap_left(sid, base + 10), MIGRATION_CAP);
    }

    /// R9 S5a — the re-check `migrate_session` runs just before the respawn,
    /// under the guard `begin_migration` took: a pane the operator closed (or
    /// a session that moved, or vanished) while the carry ran is refused with
    /// the typed refusal, without contending for the slot it already holds;
    /// an unchanged open record proceeds. A refusal returned from inside the
    /// guarded work reaches the caller as `Refused`, not as a failure — so
    /// the auto path reports nothing, exactly as for an up-front refusal.
    #[tokio::test]
    async fn a_session_closed_during_the_carry_is_refused_before_the_respawn() {
        let sid = "test/account_migration/closed-mid-carry";
        let seen = session_record(sid, "t-1", Some("C:/a"));
        let held = begin_migration(&seen, "C:/a", || Some(seen.clone())).expect("starts open");
        let mut closed = seen.clone();
        closed.state = "closed".to_string();
        assert_eq!(
            still_migratable(&seen, "C:/a", Some(closed.clone())),
            Err(MigrationRefusal::Closed {
                session: sid.to_string()
            }),
            "closed while the carry ran: no respawn"
        );
        assert_eq!(
            still_migratable(
                &seen,
                "C:/a",
                Some(session_record(sid, "t-1", Some("C:/b")))
            ),
            Err(MigrationRefusal::AlreadyMoved {
                to: "C:/b".to_string()
            })
        );
        assert_eq!(
            still_migratable(
                &seen,
                "C:/a",
                Some(session_record(sid, "t-2", Some("C:/a")))
            ),
            Err(MigrationRefusal::AlreadyMoved {
                to: "terminal t-2".to_string()
            })
        );
        assert_eq!(
            still_migratable(&seen, "C:/a", None),
            Err(MigrationRefusal::Unknown {
                session: sid.to_string()
            })
        );
        assert_eq!(
            still_migratable(&seen, "C:/a", Some(seen.clone())),
            Ok(()),
            "still open, same account, same terminal — and the held slot is no obstacle"
        );
        drop(held);

        let rec = seen.clone();
        let out = run_migration_guarded(
            || begin_migration(&rec, "C:/a", || Some(rec.clone())),
            || std::future::ready(()),
            move |()| -> Result<(), MigrateError> {
                still_migratable(&seen, "C:/a", Some(closed)).map_err(MigrateError::Refused)
            },
        )
        .await;
        assert!(
            matches!(
                out,
                Err(MigrateError::Refused(MigrationRefusal::Closed { .. }))
            ),
            "{out:?}"
        );
        let base = 1_770_000_000_000i64;
        assert!(migration_cap_permits(sid, base));
        let (unrecorded, skipped) = settle(
            sid,
            base,
            out.map(|()| -> MigrationOutcome { unreachable!() }),
        );
        assert!(unrecorded, "a late refusal gives the cap slot back");
        assert_eq!(skipped, None, "and reports nothing");
    }

    #[test]
    fn a_failed_auto_migration_is_reported_and_keeps_its_cap_slot() {
        let base = 1_760_000_000_000i64;
        let sid = "test/account_migration/failed";
        assert!(migration_cap_permits(sid, base));
        let (unrecorded, skipped) = settle(
            sid,
            base,
            Err(MigrateError::Failed("respawn failed".to_string())),
        );
        assert!(unrecorded);
        assert_eq!(skipped.as_deref(), Some("migration-failed: respawn failed"));
        assert_eq!(cap_left(sid, base + 10), MIGRATION_CAP - 1);
    }

    /// Two hints that took slots in the same millisecond: refunding one leaves
    /// the other's.
    #[test]
    fn a_refund_gives_back_exactly_one_slot() {
        let base = 1_760_000_000_000i64;
        let sid = "test/account_migration/refund-one";
        assert!(migration_cap_permits(sid, base));
        assert!(migration_cap_permits(sid, base));
        refund_migration_cap(sid, base);
        assert_eq!(cap_left(sid, base + 10), MIGRATION_CAP - 1);
        refund_migration_cap("test/account_migration/never-capped", base);
    }

    #[test]
    fn migration_cap_is_per_session() {
        let base = 1_750_000_000_000i64;
        for i in 0..MIGRATION_CAP {
            assert!(migration_cap_permits("test/cap/a", base + i as i64));
        }
        assert!(!migration_cap_permits("test/cap/a", base + 10));
        assert!(
            migration_cap_permits("test/cap/b", base + 10),
            "an unrelated session is not capped"
        );
    }

    #[test]
    fn transcript_last_model_prefers_latest_real_model() {
        let tmp =
            std::env::temp_dir().join(format!("qontinui-acctmig-model-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let path = tmp.join("t.jsonl");
        std::fs::write(
            &path,
            concat!(
                "{\"type\":\"user\",\"message\":{\"content\":\"hi\"}}\n",
                "{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-4-8\",\"content\":[]}}\n",
                "{\"type\":\"assistant\",\"message\":{\"model\":\"claude-fable-5\",\"content\":[]}}\n",
                "{\"type\":\"assistant\",\"message\":{\"model\":\"<synthetic>\",\"content\":[]}}\n",
                "{\"type\":\"user\",\"message\":{\"content\":\"quoted \\\"assistant\\\" \\\"model\\\" text\"}}\n",
            ),
        )
        .unwrap();
        assert_eq!(
            transcript_last_model(&path).as_deref(),
            Some("claude-fable-5"),
            "synthetic error turns and quoting user turns must be skipped"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn transcript_last_model_missing_file_is_none() {
        assert!(transcript_last_model(Path::new("Z:/nope/never.jsonl")).is_none());
    }

    // ---- continue nudge (plan 2026-10-21-…-forks-live-sessions, Phase 2) ----

    fn agent(id: &str, description: Option<&str>) -> CarriedSubagent {
        CarriedSubagent {
            agent_id: id.to_string(),
            description: description.map(str::to_string),
        }
    }

    #[test]
    fn with_no_subagent_the_nudge_is_the_existing_text_verbatim() {
        assert_eq!(continue_nudge(&[]), CONTINUE_NUDGE);
    }

    #[test]
    fn carried_subagents_are_named_on_one_line_as_resumable() {
        let text = continue_nudge(&[
            // Whitespace controls AND a non-whitespace one (ESC), which only
            // the control-character scrub removes.
            agent("a10fe2a6718228f32", Some("Finish\nPhase 2\r\tnow\u{1b}[0m")),
            agent("a56400", None),
        ]);
        assert_eq!(
            text.strip_prefix(CONTINUE_NUDGE)
                .expect("the existing text, then the list"),
            " The migration carried 2 subagent transcript(s) to this account, resumable with \
             SendMessage to their agent id: a10fe2a6718228f32 (\"Finish Phase 2 now [0m\"), a56400."
        );
        assert!(
            !text.chars().any(char::is_control),
            "typed into a PTY: a newline would submit early"
        );
    }

    #[test]
    fn a_large_fan_out_is_listed_up_to_the_bound_then_counted() {
        let many: Vec<CarriedSubagent> = (0..NUDGE_MAX_LISTED_SUBAGENTS + 3)
            .map(|i| agent(&format!("a{i:02}"), Some(&"x".repeat(200))))
            .collect();
        let text = continue_nudge(&many);
        assert!(text.contains(&format!("a{:02}", NUDGE_MAX_LISTED_SUBAGENTS - 1)));
        assert!(!text.contains(&format!("a{:02}", NUDGE_MAX_LISTED_SUBAGENTS)));
        assert!(text.ends_with(", and 3 more under this session's subagents/ folder."));
        assert!(text.contains(&format!("\"{}…\"", "x".repeat(NUDGE_MAX_DESCRIPTION_CHARS))));
    }

    // ---- the nudge waits for the step-4b delta sync (S4) ---------------------

    use crate::terminal::session_carry::{DeltaSync, DELTA_SYNC_EXIT_WAIT};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    fn delivery_flag() -> (
        Arc<AtomicBool>,
        impl FnOnce(String) -> std::future::Ready<()> + Send + 'static,
    ) {
        let delivered = Arc::new(AtomicBool::new(false));
        let flag = delivered.clone();
        (delivered, move |_prompt| {
            flag.store(true, Ordering::SeqCst);
            std::future::ready(())
        })
    }

    /// A recompute that finds no subagent.
    fn no_subagents() -> std::future::Ready<Result<Vec<CarriedSubagent>, String>> {
        std::future::ready(Ok(Vec::new()))
    }

    /// The nudge is not delivered while the delta sync is still running — not
    /// even just short of the deadline — and is delivered as soon as it
    /// reports.
    #[tokio::test(start_paused = true)]
    async fn the_continue_nudge_is_not_delivered_before_the_delta_sync_reports() {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let (delivered, deliver) = delivery_flag();
        let nudge = tokio::spawn(run_continue_nudge(
            "t-1".to_string(),
            Some(rx),
            NUDGE_SYNC_WAIT,
            Vec::new(),
            no_subagents,
            deliver,
        ));
        tokio::time::sleep(NUDGE_SYNC_WAIT - Duration::from_secs(1)).await;
        assert!(
            !delivered.load(Ordering::SeqCst),
            "the nudge fired before the delta sync completed"
        );
        let reported_at = tokio::time::Instant::now();
        tx.send(DeltaSyncOutcome::Synced(DeltaSync::default()))
            .unwrap();
        nudge.await.unwrap();
        assert!(delivered.load(Ordering::SeqCst));
        assert_eq!(
            tokio::time::Instant::now(),
            reported_at,
            "delivered on the report, not at the deadline"
        );
    }

    /// A delta sync that never reports holds the nudge only until the
    /// deadline; the nudge still goes.
    #[tokio::test(start_paused = true)]
    async fn a_delta_sync_that_never_reports_releases_the_nudge_at_the_deadline() {
        let (_tx, rx) = tokio::sync::oneshot::channel::<DeltaSyncOutcome>();
        let (delivered, deliver) = delivery_flag();
        let started = tokio::time::Instant::now();
        run_continue_nudge(
            "t-2".to_string(),
            Some(rx),
            NUDGE_SYNC_WAIT,
            Vec::new(),
            no_subagents,
            deliver,
        )
        .await;
        assert!(delivered.load(Ordering::SeqCst));
        assert!(started.elapsed() >= NUDGE_SYNC_WAIT);
        assert!(started.elapsed() < NUDGE_SYNC_WAIT + Duration::from_secs(1));
    }

    /// W3 — the delta sync starts as late as `DELTA_SYNC_EXIT_WAIT` (the
    /// writer proven gone on its very last look) and then needs time to copy.
    /// A sync that reports just AFTER `DELTA_SYNC_EXIT_WAIT` still holds the
    /// nudge back: the nudge's own wait carries `DELTA_SYNC_REPORT_MARGIN` on
    /// top. Driven end to end — the real writer-exit wait and the real nudge
    /// wait on one paused clock.
    #[tokio::test(start_paused = true)]
    async fn a_sync_finishing_just_after_the_exit_wait_still_gates_the_nudge() {
        use crate::terminal::session_carry::{run_delta_sync_after_exit, WriterProbe};
        let poll = crate::terminal::graceful_exit::POLL_INTERVAL;
        let started = tokio::time::Instant::now();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let sync = tokio::spawn(async move {
            let outcome = run_delta_sync_after_exit(
                // Alive until the deadline; gone on the looks that land on it.
                move || {
                    std::future::ready(if started.elapsed() + poll >= DELTA_SYNC_EXIT_WAIT {
                        WriterProbe::Gone
                    } else {
                        WriterProbe::Alive
                    })
                },
                DELTA_SYNC_EXIT_WAIT,
                poll,
                || async {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    Ok(DeltaSync::default())
                },
            )
            .await;
            let _ = tx.send(outcome);
        });
        let (delivered, deliver) = delivery_flag();
        let delivered_at = Arc::new(std::sync::Mutex::new(None));
        let at = delivered_at.clone();
        run_continue_nudge(
            "t-4".to_string(),
            Some(rx),
            NUDGE_SYNC_WAIT,
            Vec::new(),
            no_subagents,
            move |prompt| {
                *at.lock().unwrap() = Some(tokio::time::Instant::now());
                deliver(prompt)
            },
        )
        .await;
        sync.await.unwrap();
        assert!(delivered.load(Ordering::SeqCst));
        let elapsed = delivered_at.lock().unwrap().unwrap() - started;
        assert!(
            elapsed > DELTA_SYNC_EXIT_WAIT,
            "the sync finished after the exit wait, and the nudge waited for it: {elapsed:?}"
        );
        assert!(
            elapsed < NUDGE_SYNC_WAIT,
            "delivered on the sync's report, not at the nudge's deadline: {elapsed:?}"
        );
    }

    /// No sync started (no writer identity), or a sync task that died without
    /// reporting: nothing to wait for.
    #[tokio::test(start_paused = true)]
    async fn with_no_delta_sync_to_wait_for_the_nudge_goes_at_once() {
        let started = tokio::time::Instant::now();
        let (delivered, deliver) = delivery_flag();
        run_continue_nudge(
            "t-3".to_string(),
            None,
            NUDGE_SYNC_WAIT,
            Vec::new(),
            no_subagents,
            deliver,
        )
        .await;
        assert!(delivered.load(Ordering::SeqCst));
        let (tx, rx) = tokio::sync::oneshot::channel::<DeltaSyncOutcome>();
        drop(tx);
        assert_eq!(
            await_delta_sync(Some(rx), NUDGE_SYNC_WAIT).await,
            SyncGate::Dropped
        );
        // The nudge itself, behind a sync that died without reporting.
        let (tx, rx) = tokio::sync::oneshot::channel::<DeltaSyncOutcome>();
        drop(tx);
        let (delivered, deliver) = delivery_flag();
        run_continue_nudge(
            "t-3b".to_string(),
            Some(rx),
            NUDGE_SYNC_WAIT,
            Vec::new(),
            no_subagents,
            deliver,
        )
        .await;
        assert!(delivered.load(Ordering::SeqCst));
        assert_eq!(started.elapsed(), Duration::ZERO);
    }

    // ---- the nudge names the subagents as they are after the sync (S4) -------

    /// Run the nudge with the sync already reported; returns the typed prompt.
    async fn nudge_text(
        carry_time: Vec<CarriedSubagent>,
        recomputed: Result<Vec<CarriedSubagent>, String>,
    ) -> String {
        let (tx, rx) = tokio::sync::oneshot::channel();
        tx.send(DeltaSyncOutcome::Synced(DeltaSync::default()))
            .unwrap();
        let typed = Arc::new(std::sync::Mutex::new(None));
        let sink = typed.clone();
        run_continue_nudge(
            "t-5".to_string(),
            Some(rx),
            NUDGE_SYNC_WAIT,
            carry_time,
            move || std::future::ready(recomputed),
            move |prompt| {
                *sink.lock().unwrap() = Some(prompt);
                std::future::ready(())
            },
        )
        .await;
        let text = typed.lock().unwrap().take().expect("delivered");
        text
    }

    /// The carry-time list named `a1` and missed `n9` (written by the old pane
    /// after the carry, first copied by the delta sync); the recompute after
    /// the sync names `n9` and not `a1` (no longer resumable at the
    /// destination). A failed recompute falls back to the carry-time list.
    #[tokio::test]
    async fn the_nudge_names_the_subagents_recomputed_after_the_sync() {
        let text = nudge_text(vec![agent("a1", None)], Ok(vec![agent("n9", None)])).await;
        assert!(
            text.ends_with("resumable with SendMessage to their agent id: n9."),
            "{text}"
        );
        assert!(!text.contains("a1"), "{text}");

        let text = nudge_text(vec![agent("a1", None)], Err("unreadable".to_string())).await;
        assert!(
            text.ends_with("resumable with SendMessage to their agent id: a1."),
            "{text}"
        );

        let text = nudge_text(vec![agent("a1", None)], Ok(Vec::new())).await;
        assert_eq!(text, CONTINUE_NUDGE, "nothing resumable now: nothing named");
    }
}
