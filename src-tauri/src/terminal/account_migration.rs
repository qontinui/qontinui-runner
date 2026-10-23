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
use crate::terminal::graceful_exit::ProcIdentity;
use crate::terminal::migration_identity::{
    migration_verdict, resolve_migration_identity, roster_spelling, MigrationTrigger,
    MigrationVerdict,
};
use crate::terminal::session_carry::{copy_transcript, CarriedSubagent};

mod followup;

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

    // IDENTITY, before the exhaustion probe: which session does this pane
    // PROVABLY run, and under which account? The lifecycle record is the
    // binder's guess; on 2026-10-21 a mis-bound one made this path close a
    // pane running something else and resume a session still live in another
    // pane — a fork. The provider's own `sessions/<pid>.json` decides, and an
    // unresolved or elsewhere-held identity does not migrate. The source
    // account is the registry row's, not the record's.
    let (record, src, old_writer) =
        match resolve_migration_subject(&app, &store, &record, MigrationTrigger::UsageLimitHint)
            .await
        {
            Ok(subject) => (subject.record, subject.src_config_dir, subject.old_writer),
            Err(reason) => {
                let src = record.config_dir.clone().unwrap_or_default();
                emit_skipped(&app, &record, &src, &reason);
                return;
            }
        };

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

    match migrate_session_off_runtime(&app, &record, &src, &dst, old_writer).await {
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

// The continue nudge ([`continue_nudge`]) is delivered by the migration's
// detached follow-up ([`followup`]) through [`prompt_when_idle`]: bounded
// (~3 min), one submission max, standing down when the session is already
// busy or the terminal goes away. Deliberately NOT standing down on a visible
// usage-limit message: a `--resume` repaint can re-render the OLD account's
// historical banner (see the `usage_limit` module doc). When it gives up, the
// follow-up decides between one `Esc` + retry and a reported stall (Phase 2b).

/// How many carried subagents [`continue_nudge`] names before summarising the
/// rest as a count — a fan-out measured at 45 on one session.
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

/// How one [`prompt_when_idle`] run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum IdlePromptOutcome {
    /// The prompt was typed and submitted.
    Submitted,
    /// The session was already mid-turn ([`IdleWait::stand_down_on_busy`]):
    /// it picked itself up, nothing was typed.
    AlreadyBusy,
    /// The terminal (or the app) went away.
    TerminalGone,
    /// The PTY write failed.
    SubmitFailed(String),
    /// The deadline passed with no submission; `last_refusal` is why the last
    /// read was not ready.
    GaveUp { last_refusal: String },
}

/// How long, and how patiently, [`prompt_when_idle`] waits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IdleWait {
    pub deadline: std::time::Duration,
    /// `true`: a mid-turn read ends the wait (the session picked itself up,
    /// so a nudge is moot). `false`: wait THROUGH the turn for the next idle
    /// prompt — for a message that must arrive regardless (the Phase 2b
    /// transcript-tail catch-up).
    pub stand_down_on_busy: bool,
}

impl IdleWait {
    /// The continue nudge and coord's respawn `initial_prompt`: ~3 min,
    /// standing down on a busy session.
    pub(crate) const NUDGE: Self = Self {
        deadline: std::time::Duration::from_secs(180),
        stand_down_on_busy: true,
    };
}

/// What one grid read tells the watcher loop to do.
#[derive(Debug, PartialEq, Eq)]
enum IdleStep {
    Submit,
    StandDown,
    /// Read again; carries why this read was not ready.
    Wait(String),
}

/// Pure: one rendered screen through [`probe_idle_prompt`] and the
/// [`ReadyDebounce`], under `wait`'s busy policy.
fn idle_step(
    screen: crate::terminal::graceful_exit::ScreenText,
    debounce: &mut ReadyDebounce,
    wait: IdleWait,
) -> IdleStep {
    match probe_idle_prompt(&screen) {
        IdlePromptProbe::Busy if wait.stand_down_on_busy => IdleStep::StandDown,
        IdlePromptProbe::Busy => {
            debounce.reset();
            IdleStep::Wait("the session is mid-turn".to_string())
        }
        IdlePromptProbe::NotYet(why) => {
            debounce.reset();
            IdleStep::Wait(why)
        }
        IdlePromptProbe::Ready if !debounce.settled(screen) => IdleStep::Wait(
            "idle prompt seen once; waiting for an identical second read".to_string(),
        ),
        IdlePromptProbe::Ready => IdleStep::Submit,
    }
}

/// The generalized idle-watcher: wait for the resumed CLI to paint its idle
/// prompt, then submit `prompt` once, and say how it ended.
///
/// One watcher for every caller — the migration's continue nudge and
/// transcript-tail catch-up ([`followup`]) and the respawn receiver's coord
/// `initial_prompt` ([`spawn_prompt_when_idle`]) — so they never drift.
/// `label` only names the caller in the log lines.
pub(crate) async fn prompt_when_idle(
    terminal_id: &str,
    prompt: &str,
    label: &'static str,
    wait: IdleWait,
) -> IdlePromptOutcome {
    use tauri::Manager;
    let deadline = std::time::Instant::now() + wait.deadline;
    let mut debounce = ReadyDebounce::default();
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        let Some(app) = crate::tauri_app_handle::current() else {
            return IdlePromptOutcome::TerminalGone;
        };
        let Some(tm) = app.try_state::<std::sync::Arc<crate::terminal::TerminalManager>>() else {
            return IdlePromptOutcome::TerminalGone;
        };
        let Some(session) = tm.get(terminal_id) else {
            info!(
                terminal_id,
                label, "prompt-when-idle: terminal gone — standing down"
            );
            return IdlePromptOutcome::TerminalGone;
        };
        let refusal = match idle_step(session.grid_screen(), &mut debounce, wait) {
            IdleStep::StandDown => {
                info!(
                    terminal_id,
                    label, "prompt-when-idle: session already working — no prompt needed"
                );
                return IdlePromptOutcome::AlreadyBusy;
            }
            IdleStep::Wait(why) => why,
            IdleStep::Submit => {
                // `submit_prompt` reports the neutralized body (fee48d4c).
                return match session.submit_prompt(
                    prompt,
                    crate::terminal::session::PtyWriteCaller::AccountMigration,
                ) {
                    Ok(_) => {
                        info!(terminal_id, label, "prompt-when-idle submitted");
                        IdlePromptOutcome::Submitted
                    }
                    Err(e) => {
                        warn!(terminal_id, label, error = %e, "prompt-when-idle submit failed");
                        IdlePromptOutcome::SubmitFailed(e)
                    }
                };
            }
        };
        if std::time::Instant::now() >= deadline {
            warn!(
                terminal_id,
                label,
                wait_s = wait.deadline.as_secs(),
                last_refusal = %refusal,
                "prompt-when-idle: resumed CLI never painted its idle prompt within the wait — giving up"
            );
            return IdlePromptOutcome::GaveUp {
                last_refusal: refusal,
            };
        }
    }
}

/// [`prompt_when_idle`] detached, with the [`IdleWait::NUDGE`] policy — the
/// respawn receiver ([`crate::session::respawn`]) delivers coord's optional
/// `initial_prompt` through it.
pub(crate) fn spawn_prompt_when_idle(terminal_id: String, prompt: String, label: &'static str) {
    tauri::async_runtime::spawn(async move {
        prompt_when_idle(&terminal_id, &prompt, label, IdleWait::NUDGE).await;
    });
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
    // The old pane's `claude`, identified BEFORE anything is closed: the
    // detached delta sync waits for exactly this process to leave the table.
    // Taken by [`resolve_migration_subject`] from the same snapshot that
    // resolved the identity, because a snapshot is async and
    // `migrate_session` is not.
    old_writer: Option<ProcIdentity>,
) -> Result<MigrationOutcome, String> {
    let (app, record) = (app.clone(), record.clone());
    let (src, dst) = (src_config_dir.to_string(), dst_config_dir.to_string());
    qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked(move || {
        migrate_session(&app, &record, &src, &dst, old_writer)
    })
    .await
    .map_err(|e| format!("migration task did not complete: {e}"))?
}

/// The session a migration acts on, as the provider's registry proves it.
pub(crate) struct MigrationSubject {
    /// The lifecycle record to migrate — already corrected to the provider's
    /// id on `Rebound`.
    pub record: TerminalSessionRecord,
    /// The account to migrate OUT of: the registry row's (in the roster's
    /// spelling), or the record's on the operator's unresolved fallback.
    pub src_config_dir: String,
    /// The old pane's `claude` anchor, for the post-close delta sync.
    pub old_writer: Option<ProcIdentity>,
    /// How the identity was settled, in words — returned by the operator's
    /// command so a migration of an unverified identity says so.
    pub identity_note: String,
}

/// Resolve which session `record`'s pane provably runs (plan
/// `2026-10-21-account-migration-strands-subagent-transcripts-and-forks-live-sessions`
/// Phase 3) and decide what the migration does with it — the step both async
/// callers of [`migrate_session`] take ABOVE the continuation GUARD window.
///
/// `Err(reason)` is a refusal; `reason` is the typed skip reason
/// (`session-live-in-another-process(pid N)` / `identity-unresolved(<why>)`).
/// On `Rebound` the lifecycle record is corrected first (authoritative origin,
/// confirmed iff the new id's transcript exists), so the respawn's re-key
/// lands on the right row.
pub(crate) async fn resolve_migration_subject(
    app: &tauri::AppHandle,
    store: &crate::session::session_lifecycle_store::SessionLifecycleStore,
    record: &TerminalSessionRecord,
    trigger: MigrationTrigger,
) -> Result<MigrationSubject, String> {
    use tauri::Manager;
    let pane_root = app
        .try_state::<std::sync::Arc<crate::terminal::TerminalManager>>()
        .and_then(|m| m.inner().get(&record.terminal_id))
        .and_then(|session| session.pane_root_pid());
    let (identity, old_writer) =
        resolve_migration_identity(pane_root, &record.claude_session_id).await;
    match migration_verdict(identity, &record.claude_session_id, trigger) {
        MigrationVerdict::Proceed {
            sid,
            config_dir,
            rebound_from,
        } => {
            let src = roster_spelling(&config_dir, &crate::settings::get_claude_config_dirs());
            let identity_note = match &rebound_from {
                None => format!("confirmed: the pane runs {sid} under {src} (provider registry)"),
                Some(from) => format!(
                    "rebound: the pane runs {sid} under {src}, not the record's {from} (provider registry)"
                ),
            };
            let record = match rebound_from {
                None => record.clone(),
                Some(from) => {
                    // No transcript for the provider's id under its account:
                    // refuse BEFORE rewriting the record (two open records on
                    // one terminal, and a copy that would fail anyway).
                    let has_transcript = record.working_dir.as_deref().is_some_and(|wd| {
                        super::transcript::session_transcript_path(Path::new(&src), wd, &sid)
                            .exists()
                    });
                    crate::terminal::migration_identity::rebound_transcript_gate(
                        &sid,
                        &src,
                        has_transcript,
                    )?;
                    let confirmed_at = Some(chrono::Utc::now().timestamp_millis());
                    let corrected = rebound_record(record, &sid, &src, confirmed_at);
                    store.record_open(corrected.clone());
                    warn!(
                        terminal_id = %record.terminal_id,
                        record_session = %from,
                        provider_session = %sid,
                        account = %src,
                        "account migration: the pane runs a different session than its record — record corrected from the provider's live registry, migrating the provider's id"
                    );
                    corrected
                }
            };
            Ok(MigrationSubject {
                record,
                src_config_dir: src,
                old_writer,
                identity_note,
            })
        }
        MigrationVerdict::ProceedOnRecord { why } => {
            let src = record
                .config_dir
                .clone()
                .or_else(crate::ai_provider::get_resolved_config_dir)
                .ok_or("the session's current account is unknown — cannot migrate")?;
            warn!(
                terminal_id = %record.terminal_id,
                session = %record.claude_session_id,
                trigger = ?trigger,
                why = %why,
                "account migration: the pane's identity was NOT established — migrating the runner's record of the session (identity not checked)"
            );
            Ok(MigrationSubject {
                record: record.clone(),
                src_config_dir: src,
                old_writer,
                identity_note: format!(
                    "unresolved — migrated the runner's record of the session: {why}"
                ),
            })
        }
        MigrationVerdict::Refuse { reason } => {
            warn!(
                terminal_id = %record.terminal_id,
                session = %record.claude_session_id,
                pane_claude_pid = ?old_writer.as_ref().map(|w| w.pid),
                reason = %reason,
                "account migration refused: the pane's session identity is not proven safe to move"
            );
            Err(reason)
        }
    }
}

/// Pure: `record` re-keyed onto the provider's id `sid` under `config_dir`,
/// stamped authoritative (the provider's own pid -> session index is the same
/// evidence grade as a typed `--session-id`). Placement, terminal and working
/// dir are kept; the old session's name is not carried onto the new id.
fn rebound_record(
    record: &TerminalSessionRecord,
    sid: &str,
    config_dir: &str,
    confirmed_at: Option<i64>,
) -> TerminalSessionRecord {
    TerminalSessionRecord {
        claude_session_id: sid.to_string(),
        config_dir: Some(config_dir.to_string()),
        origin: Some(crate::session::session_lifecycle_store::ORIGIN_AUTHORITATIVE.to_string()),
        confirmed_at,
        session_name: None,
        name_source: None,
        ..record.clone()
    }
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
    old_writer: Option<ProcIdentity>,
) -> Result<MigrationOutcome, String> {
    use tauri::Manager;

    if crate::model_gateway::gateway_declared() {
        return Err(crate::model_gateway::SUBSCRIPTION_OFF_REASON.to_string());
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
    // The duplicate-host detector must not read this hand-off (old pane still
    // alive, resumed pane already up) as a fork.
    crate::terminal::migration_handoff::note_migration_respawn(
        &new_terminal_id,
        &record.claude_session_id,
        chrono::Utc::now().timestamp_millis(),
    );
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

    // The usage-limit watcher must not read the new pane's replayed banner as
    // news until the pane has produced a turn (Phase 2b).
    crate::terminal::migration_handoff::note_respawned_pane(
        &new_terminal_id,
        std::time::Instant::now(),
    );

    // 4b + 5. The detached follow-up ([`followup`]): once `old_writer` is gone
    // from the whole process table (bounded), top `<sid>/` up with whatever
    // the old pane wrote between the carry and its exit and rescue the main
    // transcript's tail beside it — never touching either `<sid>.jsonl` or a
    // file the resumed process has written. Meanwhile nudge the resumed
    // session to pick the task back up once the CLI paints its idle prompt (a
    // bare `--resume` restores context but then sits at the input box with
    // nobody typing), recovering or reporting a nudge that gives up. Last,
    // deliver the tail's catch-up. Runs after a failed close too: the pane was
    // already gone, and the wait still decides.
    let nudge = crate::settings::get_ai_settings()
        .claude_cli
        .auto_continue_after_migration
        .then(|| continue_nudge(&carried.subagents));
    followup::spawn_post_migration(followup::PostMigration {
        old_writer,
        src_config_dir: src_config_dir.to_string(),
        dst_config_dir: dst_config_dir.to_string(),
        working_dir: working_dir.clone(),
        claude_session_id: record.claude_session_id.clone(),
        new_terminal_id: new_terminal_id.clone(),
        carried: carried.clone(),
        nudge,
    });

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

    fn record(sid: &str) -> TerminalSessionRecord {
        TerminalSessionRecord {
            claude_session_id: sid.to_string(),
            config_dir: Some(".claude-tiohorst".to_string()),
            working_dir: Some("D:/w".to_string()),
            page_id: "page-1".to_string(),
            zone_index: 3,
            title: Some("t".to_string()),
            terminal_id: "314176b7".to_string(),
            opened_at: 1,
            last_seen_at: 2,
            state: "open".to_string(),
            closed_at: None,
            close_reason: None,
            provider: "claude".to_string(),
            origin: Some("observed".to_string()),
            restore_pending_at: None,
            confirmed_at: Some(5),
            handle: None,
            account_label: None,
            account_wrapper: None,
            session_name: Some("old name".to_string()),
            name_source: Some("operator".to_string()),
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

    /// The `Rebound` correction re-keys the record onto the provider's id and
    /// account, authoritative, on the SAME terminal and placement, and drops
    /// the old session's name.
    ///
    /// Mutation check: leaving `origin` as the record's (`observed`), or
    /// keeping `config_dir`, fails this.
    #[test]
    fn a_rebound_record_is_the_provider_identity_on_the_same_pane() {
        let r = rebound_record(&record("0c4e3025"), "506a0190", ".claude-lukas", None);
        assert_eq!(r.claude_session_id, "506a0190");
        assert_eq!(r.config_dir.as_deref(), Some(".claude-lukas"));
        assert_eq!(
            r.origin.as_deref(),
            Some(crate::session::session_lifecycle_store::ORIGIN_AUTHORITATIVE)
        );
        assert_eq!(r.confirmed_at, None);
        assert_eq!(r.session_name, None);
        assert_eq!(r.terminal_id, "314176b7");
        assert_eq!((r.page_id.as_str(), r.zone_index), ("page-1", 3));
    }

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

    // ---- Phase 2b: the watcher's busy policy -------------------------------

    fn busy_screen() -> crate::terminal::graceful_exit::ScreenText {
        let rule = "─".repeat(75);
        v2_screen(
            &["✻ Compiling… (esc to interrupt)", &rule, "❯\u{a0}", &rule],
            2,
            2,
        )
    }

    /// Mutation check: dropping the `stand_down_on_busy` guard makes the
    /// catch-up stand down on a busy pane (the second assert fails).
    #[test]
    fn the_nudge_stands_down_on_a_busy_pane_but_the_catch_up_waits_through_it() {
        let mut debounce = ReadyDebounce::default();
        assert_eq!(
            idle_step(busy_screen(), &mut debounce, IdleWait::NUDGE),
            IdleStep::StandDown
        );
        let through = IdleWait {
            stand_down_on_busy: false,
            ..IdleWait::NUDGE
        };
        assert_eq!(
            idle_step(busy_screen(), &mut debounce, through),
            IdleStep::Wait("the session is mid-turn".to_string())
        );
    }

    /// The debounce still governs submission: a busy read between two ready
    /// reads restarts it.
    #[test]
    fn a_busy_read_restarts_the_ready_debounce() {
        let rule = "─".repeat(75);
        let ready = v2_screen(
            &[
                "● Done.",
                &rule,
                "❯\u{a0}",
                &rule,
                "  ⏵⏵ bypass permissions on (shift+tab to cycle)",
            ],
            2,
            2,
        );
        let wait = IdleWait {
            stand_down_on_busy: false,
            ..IdleWait::NUDGE
        };
        let mut debounce = ReadyDebounce::default();
        assert!(matches!(
            idle_step(ready.clone(), &mut debounce, wait),
            IdleStep::Wait(_)
        ));
        assert!(matches!(
            idle_step(busy_screen(), &mut debounce, wait),
            IdleStep::Wait(_)
        ));
        assert!(
            matches!(
                idle_step(ready.clone(), &mut debounce, wait),
                IdleStep::Wait(_)
            ),
            "the busy read reset the debounce"
        );
        assert_eq!(idle_step(ready, &mut debounce, wait), IdleStep::Submit);
    }
}
