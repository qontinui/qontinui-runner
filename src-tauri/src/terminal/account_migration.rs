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
//! 3. **Pick target** — the non-exhausted, credential-valid, non-cooled
//!    account whose spare weekly capacity is closest to expiring
//!    (`pick_migration_target` — the same `ai_provider::config::cmp_rank` key
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
//!    the source file is never touched) and, best-effort, its sibling
//!    directory `<sid>/` (subagent transcripts — without it no subagent spawned
//!    before the migration can be resumed), respawn
//!    `claude --permission-mode bypassPermissions --resume <sid>` in a fresh
//!    PTY pinned to the target account, on the same grid page/zone, and only
//!    THEN close the old pane. The respawn re-keys the session's lifecycle
//!    row onto the new PTY; the old pane's exit is answered as
//!    `TerminalSuperseded` and closes nothing. A failed respawn leaves the
//!    old pane in place.
//!
//! Guards: per-session migration cap (no ping-pong when every account is
//! dry), settings kill-switch
//! (`claude_cli.auto_migrate_on_token_exhaustion`), and a no-op with fewer
//! than two configured accounts. The manual Tauri command
//! (`terminal_migrate_session_account`) reuses step 4 directly — an operator
//! click is its own confirmation.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use serde_json::json;
use tauri::Emitter;
use tracing::{info, warn};

use crate::session::session_lifecycle_store::TerminalSessionRecord;

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

    let now_ms = chrono::Utc::now().timestamp_millis();
    if !migration_cap_permits(&record.claude_session_id, now_ms) {
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

    match migrate_session_off_runtime(&app, &record, &src, &dst).await {
        Ok(outcome) => {
            info!(
                session = %record.claude_session_id,
                new_terminal = %outcome.new_terminal_id,
                from = %outcome.from_config_dir,
                to = %outcome.to_config_dir,
                "session migrated"
            );
        }
        Err(e) => {
            warn!(
                session = %record.claude_session_id,
                error = %e,
                "session migration failed"
            );
            emit_skipped(&app, &record, &src, &format!("migration-failed: {e}"));
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
fn spawn_continue_nudge(terminal_id: String) {
    spawn_prompt_when_idle(terminal_id, CONTINUE_NUDGE.to_string(), "continue-nudge");
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
    use tauri::Manager;
    tauri::async_runtime::spawn(async move {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
        let mut debounce = ReadyDebounce::default();
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            let Some(app) = crate::tauri_app_handle::current() else {
                return;
            };
            let Some(tm) = app.try_state::<std::sync::Arc<crate::terminal::TerminalManager>>()
            else {
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
    });
}

/// What [`copy_transcript`] carried across accounts.
#[derive(Debug)]
pub(crate) struct TranscriptCopy {
    /// The session transcript (`<sid>.jsonl`) under the destination account.
    pub path: std::path::PathBuf,
    /// The session's sibling directory (`<sid>/`), carried best-effort.
    pub session_dir: SessionDirCarry,
}

/// Tally of the best-effort `<sid>/` directory carry, counted per file (a
/// directory cut off by [`SESSION_DIR_MAX_DEPTH`] counts once).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SessionDirCarry {
    /// Files written to the destination.
    pub copied: usize,
    /// Destination already held the file (an append-only `.jsonl` at least as
    /// large, or any other file with identical bytes) — left untouched.
    pub skipped_present: usize,
    /// Never followed: a symlink or junction (including `<sid>/` itself), a
    /// leftover `*.partial`, a directory past the depth bound, or a session id
    /// that is not one plain path component.
    pub skipped_other: usize,
    /// Read or write errors (including an unreadable `<sid>/`); each is logged.
    pub failed: usize,
}

/// Nesting bound for the `<sid>/` carry. Measured 2026-10-12, Claude Code
/// writes one level of directories under `<sid>/` (`subagents/`,
/// `tool-results/`) plus top-level files such as `custom-title.json`; the bound
/// only stops a pathological tree.
const SESSION_DIR_MAX_DEPTH: usize = 8;

/// Suffix of the temporary file a copy writes before renaming it into place.
const PARTIAL_SUFFIX: &str = ".partial";

/// Copy the session transcript from the source account's project dir into
/// the target account's matching project dir. The source file is never
/// modified or removed. Skips the copy when the destination already exists
/// and is at least as large (idempotent retry).
///
/// Then carries the session's sibling directory `<sid>/` file by file. Claude
/// Code keeps each subagent's transcript there
/// (`<sid>/subagents/agent-<id>.jsonl` + `.meta.json`) and resolves a
/// `SendMessage` resume against the ACTIVE account's config dir, so copying
/// `<sid>.jsonl` alone left every subagent spawned before the migration
/// unresumable — "No transcript found for agent ID" — which is exactly the
/// state a usage-limit kill leaves a fan-out in. Plan
/// `2026-10-12-account-migration-leaves-subagent-transcripts-behind`.
///
/// The directory carry never fails the call: a session that spawned no
/// subagent has no `<sid>/`, and a per-file error is logged and counted rather
/// than stranding the whole session on an exhausted account. It runs even when
/// the main transcript was already present, so a retry completes a partial
/// carry. Every write goes to `<name>.partial` and is renamed into place, so a
/// copy cut short by a killed runner never looks finished to the size rule.
pub(crate) fn copy_transcript(
    src_config_dir: &str,
    dst_config_dir: &str,
    working_dir: &str,
    claude_session_id: &str,
) -> Result<TranscriptCopy, String> {
    // The id names files and a directory we copy recursively; a Claude session
    // id is a uuid, and anything else is refused before any path is built.
    // Strict: the hyphenated form only (`parse_str` also takes the simple,
    // braced and `urn:uuid:` spellings).
    let is_hyphenated_uuid = uuid::Uuid::try_parse(claude_session_id).is_ok_and(|u| {
        u.hyphenated()
            .to_string()
            .eq_ignore_ascii_case(claude_session_id)
    });
    if !is_hyphenated_uuid {
        return Err(format!(
            "not a Claude session id (expected a uuid): {claude_session_id:?}"
        ));
    }
    let src = super::transcript::session_transcript_path(
        Path::new(src_config_dir),
        working_dir,
        claude_session_id,
    );
    let dst = super::transcript::session_transcript_path(
        Path::new(dst_config_dir),
        working_dir,
        claude_session_id,
    );
    let started = std::time::Instant::now();
    let src_meta = std::fs::metadata(&src)
        .map_err(|e| format!("source transcript missing at {}: {e}", src.display()))?;
    let dst_len = std::fs::metadata(&dst).map(|dst_meta| dst_meta.len()).ok();
    // Which side ran the session last, decided once from the append-only main
    // transcript: the larger one. Sidecars that are rewritten in place (not
    // appended) follow it -- on a migration the source is newer, but on a
    // respawn that tops up from several holders the TARGET may be.
    let source_is_newer = dst_len.is_none_or(|len| src_meta.len() >= len);
    // Already migrated (retry / repeated hint).
    let already_present = dst_len.is_some_and(|len| len >= src_meta.len());
    if !already_present {
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create {} failed: {e}", parent.display()))?;
        }
        copy_via_partial(&src, &dst)
            .map_err(|e| format!("copy {} -> {} failed: {e}", src.display(), dst.display()))?;
    }
    let session_dir = carry_session_dir(&src, &dst, claude_session_id, source_is_newer);
    // Times the main copy and the `<sid>/` carry together.
    let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    if session_dir.failed > 0 {
        warn!(
            session = %claude_session_id,
            copied = session_dir.copied,
            skipped_present = session_dir.skipped_present,
            skipped_other = session_dir.skipped_other,
            failed = session_dir.failed,
            elapsed_ms,
            "session transcript carry: <sid>/ carried PARTIALLY — some subagent transcripts may not resume on this account"
        );
    } else if session_dir != SessionDirCarry::default() {
        info!(
            session = %claude_session_id,
            copied = session_dir.copied,
            skipped_present = session_dir.skipped_present,
            skipped_other = session_dir.skipped_other,
            elapsed_ms,
            "session transcript carry: carried the session directory (subagent transcripts)"
        );
    }
    Ok(TranscriptCopy {
        path: dst,
        session_dir,
    })
}

/// Copy `from` to a temporary `<to>.<pid>.<seq>.partial`, then rename it over
/// `to`. The name is unique per call, so two writers racing on one destination
/// (a manual migrate click against the automatic hint, or a migration against
/// a respawn) never rename each other's half-written file into place.
fn copy_via_partial(from: &Path, to: &Path) -> std::io::Result<()> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut partial_name = to
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .ok_or_else(|| std::io::Error::other("destination has no file name"))?;
    partial_name.push(format!(".{}.{seq}{PARTIAL_SUFFIX}", std::process::id()));
    let partial = to.with_file_name(partial_name);
    let renamed = std::fs::copy(from, &partial).and_then(|_| rename_with_retry(&partial, to));
    if renamed.is_err() {
        let _ = std::fs::remove_file(&partial);
    }
    renamed
}

/// `rename`, retried once after a short pause on `PermissionDenied` -- on
/// Windows an antivirus or indexer filter can briefly hold a file that was just
/// written. Callers run on the blocking pool (see [`migrate_session_off_runtime`]).
fn rename_with_retry(from: &Path, to: &Path) -> std::io::Result<()> {
    match std::fs::rename(from, to) {
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            std::thread::sleep(std::time::Duration::from_millis(200));
            std::fs::rename(from, to)
        }
        other => other,
    }
}

/// Carry `<sid>/` from beside `src_transcript` to beside `dst_transcript`.
/// Only "no such directory" is silent; every other way of carrying nothing is
/// counted and logged, so a stranded fan-out never reads as "no subagents".
fn carry_session_dir(
    src_transcript: &Path,
    dst_transcript: &Path,
    claude_session_id: &str,
    source_is_newer: bool,
) -> SessionDirCarry {
    let mut carry = SessionDirCarry::default();
    // The id names a directory we copy recursively, so it must be exactly one
    // ordinary path component — never `..`, a separator, or a drive prefix.
    let mut components = Path::new(claude_session_id).components();
    let single_normal = matches!(
        (components.next(), components.next()),
        (Some(std::path::Component::Normal(_)), None)
    );
    if !single_normal {
        warn!(
            session = %claude_session_id,
            "session transcript carry: session id is not one plain path component — <sid>/ not carried"
        );
        carry.skipped_other += 1;
        return carry;
    }
    let (Some(src_parent), Some(dst_parent)) = (src_transcript.parent(), dst_transcript.parent())
    else {
        return carry;
    };
    let src_dir = src_parent.join(claude_session_id);
    match std::fs::symlink_metadata(&src_dir) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => {
            warn!(
                dir = %src_dir.display(),
                "session transcript carry: <sid> is not a plain directory (symlink/junction/file) — not followed"
            );
            carry.skipped_other += 1;
            return carry;
        }
        // No subagents, no sidecars: the common case, and the only silent one.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return carry,
        Err(e) => {
            warn!(dir = %src_dir.display(), error = %e, "session transcript carry: <sid>/ unreadable");
            carry.failed += 1;
            return carry;
        }
    }
    copy_tree(
        &src_dir,
        &dst_parent.join(claude_session_id),
        0,
        source_is_newer,
        &mut carry,
    );
    carry
}

/// Whether the destination already holds what `from` should leave there. A
/// `.jsonl` transcript is append-only, so a destination at least as large is
/// the newer copy (a repeated hint, or a migration back) and must never be
/// truncated. Anything else (e.g. `custom-title.json`, a JSON file overwritten
/// in place rather than appended to) keeps the destination's copy when the
/// destination side ran the session last, and otherwise is current only when
/// its bytes are identical.
fn destination_is_current(from: &Path, to: &Path, from_len: u64, source_is_newer: bool) -> bool {
    let Ok(to_meta) = std::fs::metadata(to) else {
        return false;
    };
    if from.extension().is_some_and(|ext| ext == "jsonl") {
        return to_meta.len() >= from_len;
    }
    !source_is_newer
        || (to_meta.len() == from_len
            && matches!(
                (std::fs::read(from), std::fs::read(to)),
                (Ok(a), Ok(b)) if a == b
            ))
}

fn copy_tree(
    src: &Path,
    dst: &Path,
    depth: usize,
    source_is_newer: bool,
    carry: &mut SessionDirCarry,
) {
    if depth >= SESSION_DIR_MAX_DEPTH {
        warn!(dir = %src.display(), "session transcript carry: depth bound reached — not followed");
        carry.skipped_other += 1;
        return;
    }
    let entries = match std::fs::read_dir(src) {
        Ok(entries) => entries,
        Err(e) => {
            warn!(dir = %src.display(), error = %e, "session transcript carry: dir unreadable");
            carry.failed += 1;
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                warn!(dir = %src.display(), error = %e, "session transcript carry: unreadable entry");
                carry.failed += 1;
                continue;
            }
        };
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let meta = match std::fs::symlink_metadata(&from) {
            Ok(meta) => meta,
            Err(e) => {
                warn!(path = %from.display(), error = %e, "session transcript carry: stat failed");
                carry.failed += 1;
                continue;
            }
        };
        if meta.is_dir() {
            copy_tree(&from, &to, depth + 1, source_is_newer, carry);
            continue;
        }
        let is_partial = entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.ends_with(PARTIAL_SUFFIX));
        if !meta.is_file() || is_partial {
            carry.skipped_other += 1;
            continue;
        }
        if destination_is_current(&from, &to, meta.len(), source_is_newer) {
            carry.skipped_present += 1;
            continue;
        }
        let written = std::fs::create_dir_all(dst).and_then(|()| copy_via_partial(&from, &to));
        match written {
            Ok(()) => carry.copied += 1,
            Err(e) => {
                warn!(
                    from = %from.display(),
                    to = %to.display(),
                    error = %e,
                    "session transcript carry: file copy failed"
                );
                carry.failed += 1;
            }
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
            crate::session::claude_hook::direct_spawn_settings_args(),
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
/// carry it can copy a whole session directory — measured 2026-10-12 at up to
/// ~125 MB / 250 files per session on this fleet. A usage limit is per
/// account, so every session on that account migrates at about the same moment;
/// run inline, each of those would hold a tokio worker for the length of its
/// copy. Both async callers (the usage-limit hint and the manual Tauri command)
/// go through here.
pub async fn migrate_session_off_runtime(
    app: &tauri::AppHandle,
    record: &TerminalSessionRecord,
    src_config_dir: &str,
    dst_config_dir: &str,
) -> Result<MigrationOutcome, String> {
    let (app, record) = (app.clone(), record.clone());
    let (src, dst) = (src_config_dir.to_string(), dst_config_dir.to_string());
    qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked(move || {
        migrate_session(&app, &record, &src, &dst)
    })
    .await
    .map_err(|e| format!("migration task did not complete: {e}"))?
}

/// Perform the migration mechanics: transcript copy → trust for the
/// destination → respawn under the target account (which re-keys the
/// lifecycle row onto the new PTY) → close the old pane → emit event.
///
/// **The old pane closes only after the respawn has succeeded.** Any failure
/// before that — transcript, trust, the spawn itself — returns with the old
/// pane untouched, still sitting at its usage-limit prompt, and the caller
/// emits `skipped`. Until 2026-09-18 the order was close-then-respawn, so a
/// respawn failure (three that day, from a resolver defect) was a vanished
/// window with nothing in its place. For a moment two PTYs now carry one
/// session; the old one is idle at a limit prompt and consumes nothing.
///
/// Shared by the automatic path ([`handle_usage_limit_hint`], which has
/// already confirmed exhaustion) and the manual Tauri command
/// (`terminal_migrate_session_account`, where the operator's click is the
/// confirmation).
#[expect(
    clippy::string_slice,
    reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
)]
pub fn migrate_session(
    app: &tauri::AppHandle,
    record: &TerminalSessionRecord,
    src_config_dir: &str,
    dst_config_dir: &str,
) -> Result<MigrationOutcome, String> {
    use tauri::Manager;

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
    // The `<sid>/` carry inside it is best-effort; its tally rides the event.
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
        ));
    }

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
    // three `try_state` lookups, `copy_transcript`, and the trust-gate
    // refusal), and it must stay that way. An early exit inserted inside the
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
            return Err(e);
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

    // 5. Optionally nudge the resumed session to pick the task back up once
    // the CLI paints its idle prompt — a bare `--resume` restores context but
    // then sits at the input box with nobody typing.
    if crate::settings::get_ai_settings()
        .claude_cli
        .auto_continue_after_migration
    {
        spawn_continue_nudge(new_terminal_id.clone());
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
            "sessionDirCarry": {
                "copied": carried.session_dir.copied,
                "skippedPresent": carried.session_dir.skipped_present,
                "skippedOther": carried.session_dir.skipped_other,
                "failed": carried.session_dir.failed,
            },
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
    fn copy_transcript_copies_and_is_idempotent() {
        let tmp =
            std::env::temp_dir().join(format!("qontinui-acctmig-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let src_cfg = tmp.join(".claude-hotmail");
        let dst_cfg = tmp.join(".claude-gmail");
        let working_dir = "D:\\qontinui-root";
        let sid = "11111111-2222-3333-4444-555555555555";

        let src_path =
            crate::terminal::transcript::session_transcript_path(&src_cfg, working_dir, sid);
        std::fs::create_dir_all(src_path.parent().unwrap()).unwrap();
        std::fs::write(&src_path, b"{\"type\":\"user\"}\n").unwrap();

        let dst = copy_transcript(
            src_cfg.to_str().unwrap(),
            dst_cfg.to_str().unwrap(),
            working_dir,
            sid,
        )
        .expect("copy should succeed")
        .path;
        assert!(dst.exists());
        assert_eq!(
            std::fs::read(&dst).unwrap(),
            std::fs::read(&src_path).unwrap()
        );
        // Source untouched.
        assert!(src_path.exists());

        // Second call (repeated hint / retry) is a no-op success.
        let dst2 = copy_transcript(
            src_cfg.to_str().unwrap(),
            dst_cfg.to_str().unwrap(),
            working_dir,
            sid,
        )
        .expect("idempotent retry should succeed")
        .path;
        assert_eq!(dst, dst2);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Lay out `<cfg>/projects/<slug>/<sid>.jsonl` plus the files given as
    /// `(path relative to <sid>/, bytes)`; returns the `<sid>/` directory.
    fn seed_session(
        cfg: &Path,
        working_dir: &str,
        sid: &str,
        files: &[(&str, &[u8])],
    ) -> std::path::PathBuf {
        let transcript =
            crate::terminal::transcript::session_transcript_path(cfg, working_dir, sid);
        let project = transcript.parent().unwrap().to_path_buf();
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(&transcript, b"{\"type\":\"user\"}\n").unwrap();
        let session_dir = project.join(sid);
        for (rel, bytes) in files {
            let path = session_dir.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, bytes).unwrap();
        }
        session_dir
    }

    fn session_dir_of(cfg: &Path, working_dir: &str, sid: &str) -> std::path::PathBuf {
        crate::terminal::transcript::session_transcript_path(cfg, working_dir, sid)
            .parent()
            .unwrap()
            .join(sid)
    }

    #[test]
    fn copy_transcript_carries_subagent_transcripts_and_sidecars() {
        let tmp =
            std::env::temp_dir().join(format!("qontinui-acctmig-sidecar-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let (src_cfg, dst_cfg) = (tmp.join(".claude-erik"), tmp.join(".claude-qontinui"));
        let working_dir = "D:\\qontinui-root";
        let sid = "beeae8aa-7d4d-4843-87fb-8d71dcce6f2e";
        let files: &[(&str, &[u8])] = &[
            (
                "subagents/agent-a0a89f0b0f9dfeb83.jsonl",
                b"{\"type\":\"assistant\"}\n",
            ),
            (
                "subagents/agent-a0a89f0b0f9dfeb83.meta.json",
                b"{\"agentType\":\"x\"}",
            ),
            ("tool-results/hook-1-stdout.txt", b"large tool output"),
        ];
        seed_session(&src_cfg, working_dir, sid, files);

        let carried = copy_transcript(
            src_cfg.to_str().unwrap(),
            dst_cfg.to_str().unwrap(),
            working_dir,
            sid,
        )
        .expect("copy should succeed");
        assert_eq!(
            carried.session_dir,
            SessionDirCarry {
                copied: 3,
                ..SessionDirCarry::default()
            }
        );
        let dst_dir = session_dir_of(&dst_cfg, working_dir, sid);
        for (rel, bytes) in files {
            assert_eq!(
                std::fs::read(dst_dir.join(rel)).unwrap(),
                *bytes,
                "{rel} must land under the destination account byte-for-byte"
            );
        }

        // A retry (the main transcript is now present) still runs the carry,
        // and finds everything already there.
        let again = copy_transcript(
            src_cfg.to_str().unwrap(),
            dst_cfg.to_str().unwrap(),
            working_dir,
            sid,
        )
        .expect("retry should succeed");
        assert_eq!(
            again.session_dir,
            SessionDirCarry {
                skipped_present: 3,
                ..SessionDirCarry::default()
            }
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn copy_transcript_without_session_dir_carries_nothing() {
        let tmp =
            std::env::temp_dir().join(format!("qontinui-acctmig-nodir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let (src_cfg, dst_cfg) = (tmp.join(".claude-a"), tmp.join(".claude-b"));
        let working_dir = "D:\\qontinui-root";
        let sid = "22222222-3333-4444-5555-666666666666";
        seed_session(&src_cfg, working_dir, sid, &[]);

        let carried = copy_transcript(
            src_cfg.to_str().unwrap(),
            dst_cfg.to_str().unwrap(),
            working_dir,
            sid,
        )
        .expect("a session that spawned no subagent still migrates");
        assert!(carried.path.exists());
        assert_eq!(carried.session_dir, SessionDirCarry::default());
        assert!(!session_dir_of(&dst_cfg, working_dir, sid).exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn copy_transcript_never_truncates_a_newer_destination_subagent_file() {
        let tmp =
            std::env::temp_dir().join(format!("qontinui-acctmig-newer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let (src_cfg, dst_cfg) = (tmp.join(".claude-a"), tmp.join(".claude-b"));
        let working_dir = "D:\\qontinui-root";
        let sid = "33333333-4444-5555-6666-777777777777";
        seed_session(
            &src_cfg,
            working_dir,
            sid,
            &[
                ("subagents/agent-old.jsonl", b"short"),
                (
                    "subagents/agent-grown.jsonl",
                    b"a much longer source transcript",
                ),
            ],
        );
        // Ping-pong: the destination already holds a LONGER copy of one file
        // (the session ran there after an earlier migration) and a SHORTER,
        // stale copy of the other.
        seed_session(
            &dst_cfg,
            working_dir,
            sid,
            &[
                ("subagents/agent-old.jsonl", b"short, then appended to"),
                ("subagents/agent-grown.jsonl", b"stale"),
            ],
        );

        let carried = copy_transcript(
            src_cfg.to_str().unwrap(),
            dst_cfg.to_str().unwrap(),
            working_dir,
            sid,
        )
        .expect("copy should succeed");
        assert_eq!(
            carried.session_dir,
            SessionDirCarry {
                copied: 1,
                skipped_present: 1,
                ..SessionDirCarry::default()
            }
        );
        let dst_dir = session_dir_of(&dst_cfg, working_dir, sid);
        assert_eq!(
            std::fs::read(dst_dir.join("subagents/agent-old.jsonl")).unwrap(),
            b"short, then appended to",
            "a larger destination file is the newer copy and is left alone"
        );
        assert_eq!(
            std::fs::read(dst_dir.join("subagents/agent-grown.jsonl")).unwrap(),
            b"a much longer source transcript",
            "a smaller destination file is replaced with the source bytes"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn carry_session_dir_refuses_a_session_id_that_is_not_one_path_component() {
        let tmp =
            std::env::temp_dir().join(format!("qontinui-acctmig-traverse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let src_project = tmp.join("src").join("project");
        let dst_project = tmp.join("dst").join("project");
        // A sibling directory a hostile id could reach with `..`.
        std::fs::create_dir_all(&src_project).unwrap();
        std::fs::create_dir_all(tmp.join("src").join("secret")).unwrap();
        std::fs::write(tmp.join("src").join("secret").join("f"), b"x").unwrap();
        let mut bad_ids = vec!["..", "../secret", "a/b", ""];
        if cfg!(windows) {
            bad_ids.extend(["..\\secret", "C:", "C:secret"]);
        }
        for bad in bad_ids {
            let carried = carry_session_dir(
                &src_project.join("t.jsonl"),
                &dst_project.join("t.jsonl"),
                bad,
                true,
            );
            assert_eq!(
                carried,
                SessionDirCarry {
                    skipped_other: 1,
                    ..SessionDirCarry::default()
                },
                "id {bad:?} must carry nothing, and say so rather than read as 'no subagents'"
            );
        }
        assert!(!tmp.join("dst").exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn copy_transcript_replaces_a_stale_non_transcript_file_even_when_larger() {
        let tmp =
            std::env::temp_dir().join(format!("qontinui-acctmig-title-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let (src_cfg, dst_cfg) = (tmp.join(".claude-a"), tmp.join(".claude-b"));
        let working_dir = "D:\\qontinui-root";
        let sid = "44444444-5555-6666-7777-888888888888";
        // Renamed to something SHORTER on the account the session just ran on;
        // the destination holds the older, longer title from a prior migration.
        seed_session(
            &src_cfg,
            working_dir,
            sid,
            &[("custom-title.json", b"{\"customTitle\":\"x\"}")],
        );
        seed_session(
            &dst_cfg,
            working_dir,
            sid,
            &[(
                "custom-title.json",
                b"{\"customTitle\":\"a much longer old title\"}",
            )],
        );
        let carried = copy_transcript(
            src_cfg.to_str().unwrap(),
            dst_cfg.to_str().unwrap(),
            working_dir,
            sid,
        )
        .expect("copy should succeed");
        assert_eq!(carried.session_dir.copied, 1);
        assert_eq!(
            std::fs::read(session_dir_of(&dst_cfg, working_dir, sid).join("custom-title.json"))
                .unwrap(),
            b"{\"customTitle\":\"x\"}",
            "only append-only .jsonl files use the size rule"
        );
        // Identical bytes afterwards: a retry leaves it alone.
        let again = copy_transcript(
            src_cfg.to_str().unwrap(),
            dst_cfg.to_str().unwrap(),
            working_dir,
            sid,
        )
        .expect("retry should succeed");
        assert_eq!(again.session_dir.skipped_present, 1);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn copy_transcript_keeps_the_destination_sidecar_when_the_destination_ran_last() {
        let tmp =
            std::env::temp_dir().join(format!("qontinui-acctmig-newer-dst-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let (src_cfg, dst_cfg) = (tmp.join(".claude-old"), tmp.join(".claude-new"));
        let working_dir = "D:\\qontinui-root";
        let sid = "88888888-9999-aaaa-bbbb-cccccccccccc";
        // A respawn topping up from an OLDER holder: the target's main
        // transcript is the larger one, so its rewritten-in-place sidecar wins.
        seed_session(
            &src_cfg,
            working_dir,
            sid,
            &[("custom-title.json", b"{\"customTitle\":\"old\"}")],
        );
        seed_session(
            &dst_cfg,
            working_dir,
            sid,
            &[("custom-title.json", b"{\"customTitle\":\"new\"}")],
        );
        let dst_main =
            crate::terminal::transcript::session_transcript_path(&dst_cfg, working_dir, sid);
        std::fs::write(
            &dst_main,
            b"{\"type\":\"user\"}\n{\"type\":\"assistant\"}\n",
        )
        .unwrap();

        let carried = copy_transcript(
            src_cfg.to_str().unwrap(),
            dst_cfg.to_str().unwrap(),
            working_dir,
            sid,
        )
        .expect("copy should succeed");
        assert_eq!(carried.session_dir.skipped_present, 1);
        assert_eq!(
            std::fs::read(session_dir_of(&dst_cfg, working_dir, sid).join("custom-title.json"))
                .unwrap(),
            b"{\"customTitle\":\"new\"}",
            "the side that ran the session last keeps its sidecar"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn copy_transcript_refuses_a_session_id_that_is_not_a_uuid() {
        let tmp =
            std::env::temp_dir().join(format!("qontinui-acctmig-notuuid-{}", std::process::id()));
        let err = copy_transcript(
            tmp.join("src").to_str().unwrap(),
            tmp.join("dst").to_str().unwrap(),
            "D:\\qontinui-root",
            "../11111111-2222-3333-4444-555555555555",
        )
        .unwrap_err();
        assert!(err.contains("expected a uuid"), "got: {err}");
        assert!(!tmp.exists(), "nothing is written for a refused id");
    }

    #[test]
    fn copy_transcript_never_carries_a_leftover_partial_file() {
        let tmp =
            std::env::temp_dir().join(format!("qontinui-acctmig-partial-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let (src_cfg, dst_cfg) = (tmp.join(".claude-a"), tmp.join(".claude-b"));
        let working_dir = "D:\\qontinui-root";
        let sid = "55555555-6666-7777-8888-999999999999";
        seed_session(
            &src_cfg,
            working_dir,
            sid,
            &[
                ("subagents/agent-a.jsonl", b"{}\n"),
                ("subagents/agent-b.jsonl.partial", b"half a co"),
            ],
        );
        let carried = copy_transcript(
            src_cfg.to_str().unwrap(),
            dst_cfg.to_str().unwrap(),
            working_dir,
            sid,
        )
        .expect("copy should succeed");
        assert_eq!(
            carried.session_dir,
            SessionDirCarry {
                copied: 1,
                skipped_other: 1,
                ..SessionDirCarry::default()
            }
        );
        let dst_sub = session_dir_of(&dst_cfg, working_dir, sid).join("subagents");
        assert!(dst_sub.join("agent-a.jsonl").exists());
        assert!(!dst_sub.join("agent-b.jsonl.partial").exists());
        let leftovers: Vec<_> = std::fs::read_dir(&dst_sub)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(PARTIAL_SUFFIX))
            .collect();
        assert!(
            leftovers.is_empty(),
            "no temp file left behind: {leftovers:?}"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Link `link` -> `target` as a directory symlink (unix) or a junction
    /// (Windows; `mklink /J` needs no privilege). `false` when the platform
    /// refused, so the caller can say the case went unexercised.
    fn link_dir(target: &Path, link: &Path) -> bool {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
        #[cfg(windows)]
        {
            std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()
                .is_ok_and(|out| out.status.success())
        }
    }

    #[test]
    fn carry_never_follows_a_linked_directory() {
        let tmp =
            std::env::temp_dir().join(format!("qontinui-acctmig-links-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let (src_cfg, dst_cfg) = (tmp.join(".claude-a"), tmp.join(".claude-b"));
        let working_dir = "D:\\qontinui-root";
        let sid = "66666666-7777-8888-9999-aaaaaaaaaaaa";
        let session_dir = seed_session(
            &src_cfg,
            working_dir,
            sid,
            &[("subagents/agent-a.jsonl", b"{}\n")],
        );
        let outside = tmp.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), b"must not be copied").unwrap();
        assert!(
            link_dir(&outside, &session_dir.join("linked")),
            "could not create a directory link to exercise the rule"
        );

        let carried = copy_transcript(
            src_cfg.to_str().unwrap(),
            dst_cfg.to_str().unwrap(),
            working_dir,
            sid,
        )
        .expect("copy should succeed");
        assert_eq!(
            carried.session_dir,
            SessionDirCarry {
                copied: 1,
                skipped_other: 1,
                ..SessionDirCarry::default()
            }
        );
        let dst_dir = session_dir_of(&dst_cfg, working_dir, sid);
        assert!(
            !dst_dir.join("linked").exists(),
            "a linked dir is never followed"
        );

        // `<sid>` itself being a link is refused and counted, not read as "none".
        let other_sid = "77777777-8888-9999-aaaa-bbbbbbbbbbbb";
        let other_transcript =
            crate::terminal::transcript::session_transcript_path(&src_cfg, working_dir, other_sid);
        std::fs::write(&other_transcript, b"{}\n").unwrap();
        assert!(link_dir(
            &session_dir,
            &other_transcript.parent().unwrap().join(other_sid)
        ));
        let linked_root = copy_transcript(
            src_cfg.to_str().unwrap(),
            dst_cfg.to_str().unwrap(),
            working_dir,
            other_sid,
        )
        .expect("main transcript still migrates");
        assert_eq!(
            linked_root.session_dir,
            SessionDirCarry {
                skipped_other: 1,
                ..SessionDirCarry::default()
            }
        );
        assert!(!session_dir_of(&dst_cfg, working_dir, other_sid).exists());
        let _ = std::fs::remove_dir_all(&tmp);
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

    #[test]
    fn copy_transcript_missing_source_errors() {
        let tmp =
            std::env::temp_dir().join(format!("qontinui-acctmig-missing-{}", std::process::id()));
        let err = copy_transcript(
            tmp.join("nope-src").to_str().unwrap(),
            tmp.join("nope-dst").to_str().unwrap(),
            "D:\\qontinui-root",
            "00000000-0000-0000-0000-000000000000",
        )
        .unwrap_err();
        assert!(err.contains("source transcript missing"), "got: {err}");
    }
}
