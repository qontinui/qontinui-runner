//! The account migration's detached follow-up, run once the resumed pane is
//! up and the old one has been told to close (plan
//! `2026-10-21-account-migration-strands-subagent-transcripts-and-forks-live-sessions`
//! Phases 2 and 2b).
//!
//! One ordering, each step after the one before:
//! 1. **The gate** ([`super::run_after_delta_sync`]): wait, bounded by
//!    [`super::NUDGE_SYNC_WAIT`], for the step-4b delta sync
//!    ([`session_carry::spawn_delta_sync_after_exit`]) to report.
//! 2. **The TAIL rescue** ([`session_carry::tail::rescue_tail`]) — the lines
//!    the old account's `claude` appended after the carry, which on
//!    2026-10-22 included the only record that a background agent had died.
//!    Only when the gate proves the old writer gone
//!    ([`super::SyncGate::writer_proven_gone`]); otherwise the tail is
//!    reported not read.
//! 3. **The continue nudge**, naming the subagents resumable at the
//!    destination after the sync (recomputed, not the carry's list), with
//!    stall recovery. On 2026-10-22 the resumed
//!    CLI wrote no turn for 62 minutes: the nudge gave up after 3 minutes and
//!    nothing followed. Now a give-up captures the pane's screen and logs it,
//!    and either sends ONE `Esc` and retries ONCE (the screen shows the CLI's
//!    limit-wait banner on an account the probe says is NOT exhausted), or
//!    reports the stall — a `session-account-migration-stalled` event and one
//!    best-effort coord finding. Never more than one `Esc` and one retry.
//! 4. **The catch-up**: when the rescued tail holds task notifications or
//!    operator prompts, ONE bounded single-line message through the same idle
//!    watcher, waiting through the nudge's turn rather than standing down on
//!    it. Then `session-account-migration-tail` reports the tail.
//!
//! The decisions are pure ([`decide_stall`], [`screen_shows_limit_wait`],
//! [`evidence_line`], [`screen_excerpt`], [`stall_finding_body`],
//! [`tail_event`]); the rest is wiring.

use serde_json::json;
use tauri::Emitter;
use tracing::{info, warn};

use super::{prompt_when_idle, IdlePromptOutcome, IdleWait, SyncGate};
use crate::terminal::output_scan::normalize;
use crate::terminal::session_carry::{self, tail::TailRescue, DeltaSyncOutcome, TranscriptCopy};

/// The catch-up waits THROUGH turns (the nudge's, typically) for an idle
/// prompt, so it gets a longer ceiling than the nudge's 3 minutes.
const CATCH_UP_WAIT: IdleWait = IdleWait {
    deadline: std::time::Duration::from_secs(20 * 60),
    stand_down_on_busy: false,
};

/// How many bottom rows of the pane the give-up log keeps.
const EXCERPT_ROWS: usize = 15;

/// Character bound on the give-up log's screen excerpt.
const EXCERPT_MAX_CHARS: usize = 1500;

/// Character bound on the evidence line the stalled event and finding carry.
const EVIDENCE_MAX_CHARS: usize = 200;

/// Everything the follow-up needs, moved out of `migrate_session`.
pub(crate) struct PostMigration {
    pub src_config_dir: String,
    pub dst_config_dir: String,
    pub claude_session_id: String,
    pub new_terminal_id: String,
    /// The carry's record: where the tail rescue reads and writes, and the
    /// offset it starts at.
    pub carried: TranscriptCopy,
    /// `auto_continue_after_migration`: whether to type the continue nudge.
    /// Its text is built after the delta-sync gate, naming the subagents
    /// resumable at the destination then ([`super::nudge_text_after_sync`]).
    pub auto_continue: bool,
}

/// Run the follow-up detached, gated on the step-4b delta sync's report
/// (`sync_done`; `None` when no sync runs). Best-effort throughout: nothing
/// here reaches the migration's own result.
pub(crate) fn spawn_post_migration(
    p: PostMigration,
    sync_done: Option<tokio::sync::oneshot::Receiver<DeltaSyncOutcome>>,
) {
    let terminal_id = p.new_terminal_id.clone();
    tauri::async_runtime::spawn(super::run_after_delta_sync(
        terminal_id,
        sync_done,
        super::NUDGE_SYNC_WAIT,
        move |gate| run(p, gate),
    ));
}

async fn run(p: PostMigration, gate: SyncGate) {
    let tail = if gate.writer_proven_gone() {
        rescue_tail(&p).await
    } else {
        info!(
            terminal_id = %p.new_terminal_id,
            session = %p.claude_session_id,
            gate = ?gate,
            "session tail rescue: skipped — the old pane's claude is not proven gone, so its transcript may still be growing"
        );
        None
    };
    if p.auto_continue {
        let carried = p.carried.clone();
        let text = super::nudge_text_after_sync(
            &p.new_terminal_id,
            p.carried.subagents.clone(),
            move || super::resumable_subagents_after_sync(carried),
        )
        .await;
        nudge_with_stall_recovery(&p, &text).await;
    }

    let message = tail.as_ref().and_then(TailRescue::message);
    let delivered = match message {
        Some(message) => {
            let outcome = prompt_when_idle(
                &p.new_terminal_id,
                &message,
                "migration-tail-catch-up",
                CATCH_UP_WAIT,
            )
            .await;
            if outcome != IdlePromptOutcome::Submitted {
                warn!(
                    terminal_id = %p.new_terminal_id,
                    session = %p.claude_session_id,
                    outcome = ?outcome,
                    "migration tail catch-up NOT delivered — the rescued lines are still on disk under <sid>/migration-tail/"
                );
            }
            outcome == IdlePromptOutcome::Submitted
        }
        None => false,
    };
    emit(
        "session-account-migration-tail",
        tail_event(&p, tail.as_ref(), delivered),
    );
}

/// The tail rescue, off the async workers (disk I/O). `None` when it failed —
/// logged, and reported as not read.
async fn rescue_tail(p: &PostMigration) -> Option<TailRescue> {
    let (src_config_dir, sid, carried) = (
        p.src_config_dir.clone(),
        p.claude_session_id.clone(),
        p.carried.clone(),
    );
    let rescued = qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked(move || {
        session_carry::tail::rescue_tail(
            &src_config_dir,
            &sid,
            &carried,
            chrono::Utc::now().timestamp_millis(),
        )
    })
    .await
    .map_err(|e| format!("tail rescue task did not complete: {e}"))
    .and_then(|r| r);
    match rescued {
        Ok(rescue) => Some(rescue),
        Err(e) => {
            warn!(
                session = %p.claude_session_id,
                error = %e,
                "session tail rescue: failed (the migration itself is unaffected)"
            );
            None
        }
    }
}

/// Pure: the `session-account-migration-tail` payload. `transcriptTailLines`
/// is `null` when the tail was not read (writer not proven gone, or a failed
/// read) and when the source was shorter than the carry's offset — both are
/// UNKNOWN, never zero.
fn tail_event(p: &PostMigration, tail: Option<&TailRescue>, delivered: bool) -> serde_json::Value {
    let (status, lines, unparseable, path) = match tail {
        None => ("not-read", None, None, None),
        Some(TailRescue::Empty) => ("empty", Some(0), Some(0), None),
        Some(TailRescue::SourceShorter { .. }) => ("source-shorter-unknown", None, None, None),
        Some(TailRescue::Rescued { path, scan, .. }) => (
            "rescued",
            Some(scan.lines),
            Some(scan.unparseable),
            Some(path.display().to_string()),
        ),
    };
    json!({
        "claudeSessionId": p.claude_session_id,
        "newTerminalId": p.new_terminal_id,
        "toConfigDir": p.dst_config_dir,
        "transcriptTail": status,
        "transcriptTailLines": lines,
        "transcriptTailUnparseable": unparseable,
        "transcriptTailPath": path,
        "transcriptTailDelivered": delivered,
    })
}

// ---- stall recovery -------------------------------------------------------

/// Whether the destination account is exhausted, as a fresh probe says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Exhaustion {
    Exhausted,
    NotExhausted,
    /// The probe returned nothing usable for the account.
    Unknown,
}

/// Why the nudge is reported stalled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StallReason {
    /// The screen could not be read.
    ScreenUnreadable,
    /// The screen shows something other than the limit-wait banner.
    UnrecognisedScreen,
    /// The limit-wait banner, and the account IS exhausted: the CLI's own
    /// wait is correct, so no `Esc`.
    LimitWaitOnExhaustedAccount,
    /// The limit-wait banner, and the probe could not say: an UNKNOWN never
    /// authorizes cancelling the CLI's wait.
    LimitWaitExhaustionUnknown,
    /// The one `Esc` could not be written.
    EscNotDelivered,
    /// The one retry gave up too.
    RetryGaveUp,
}

impl StallReason {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ScreenUnreadable => "screen-unreadable",
            Self::UnrecognisedScreen => "unrecognised-screen",
            Self::LimitWaitOnExhaustedAccount => "limit-wait-on-exhausted-account",
            Self::LimitWaitExhaustionUnknown => "limit-wait-exhaustion-unknown",
            Self::EscNotDelivered => "esc-not-delivered",
            Self::RetryGaveUp => "retry-gave-up",
        }
    }
}

/// What follows a nudge give-up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StallAction {
    /// Send one `Esc`, wait for idle once more, retry the nudge once.
    EscThenRetry,
    /// Report the stall; nothing more is typed.
    Stalled(StallReason),
}

/// Pure: whether the screen shows the CLI's limit-wait banner
/// (`… · continuing automatically at 6pm · esc to cancel`). A bare
/// `esc to cancel` also labels many ordinary CLI dialogs, so it counts only
/// beside a usage-limit phrase.
pub(crate) fn screen_shows_limit_wait(screen: &str) -> bool {
    let normalized = normalize(screen);
    normalized.contains("continuing automatically at")
        || (normalized.contains("esc to cancel")
            && crate::terminal::usage_limit::window_indicates_usage_limit(screen).is_some())
}

/// Pure: the give-up decision. `screen` is `None` when it could not be read;
/// `retried` is whether this give-up is the retry's. At most one `Esc` and one
/// retry per migration: a retry's give-up is always a stall.
pub(crate) fn decide_stall(
    screen: Option<&str>,
    exhaustion: Exhaustion,
    retried: bool,
) -> StallAction {
    if retried {
        return StallAction::Stalled(StallReason::RetryGaveUp);
    }
    let Some(screen) = screen else {
        return StallAction::Stalled(StallReason::ScreenUnreadable);
    };
    if !screen_shows_limit_wait(screen) {
        return StallAction::Stalled(StallReason::UnrecognisedScreen);
    }
    match exhaustion {
        Exhaustion::NotExhausted => StallAction::EscThenRetry,
        Exhaustion::Exhausted => StallAction::Stalled(StallReason::LimitWaitOnExhaustedAccount),
        Exhaustion::Unknown => StallAction::Stalled(StallReason::LimitWaitExhaustionUnknown),
    }
}

/// `s` on one line, bounded to `max` characters.
fn bounded_line(s: &str, max: usize) -> String {
    let line = session_carry::tail::one_line(s);
    if line.chars().count() <= max {
        return line;
    }
    let mut out: String = line.chars().take(max).collect();
    out.push('…');
    out
}

/// Pure: the one screen row that says what the pane is doing — the lowest row
/// showing the limit-wait banner, else the lowest non-blank row.
pub(crate) fn evidence_line(screen: &str) -> String {
    let rows: Vec<&str> = screen.lines().filter(|l| !l.trim().is_empty()).collect();
    let row = rows
        .iter()
        .rev()
        .find(|row| screen_shows_limit_wait(row))
        .or_else(|| rows.last())
        .copied()
        .unwrap_or("");
    bounded_line(row, EVIDENCE_MAX_CHARS)
}

/// Pure: the bottom [`EXCERPT_ROWS`] non-blank rows, joined with ` ⏎ `,
/// bounded to [`EXCERPT_MAX_CHARS`] — the give-up log line's screen capture.
pub(crate) fn screen_excerpt(screen: &str) -> String {
    let rows: Vec<String> = screen
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(session_carry::tail::one_line)
        .collect();
    let bottom = rows
        .get(rows.len().saturating_sub(EXCERPT_ROWS)..)
        .unwrap_or(&[]);
    let joined = bottom.join(" ⏎ ");
    if joined.chars().count() <= EXCERPT_MAX_CHARS {
        return joined;
    }
    // Keep the END: the prompt area and any banner sit at the bottom.
    let skip = joined.chars().count() - EXCERPT_MAX_CHARS;
    format!("…{}", joined.chars().skip(skip).collect::<String>())
}

/// Pure: the stall's coord finding — the shape
/// [`crate::session::duplicate_hosts::post_session_finding`] posts.
pub(crate) fn stall_finding_body(
    claude_session_id: &str,
    terminal_id: &str,
    to_config_dir: &str,
    reason: StallReason,
    screen_line: &str,
) -> serde_json::Value {
    json!({
        "title": format!(
            "resumed session {claude_session_id} stalled after an account migration ({})",
            reason.as_str()
        ),
        "body": format!(
            "The runner migrated session {claude_session_id} to {to_config_dir} (pane \
             {terminal_id}), and the continue nudge could not be delivered: the resumed CLI never \
             painted an idle prompt ({}). The pane's screen read: \"{screen_line}\". The runner \
             typed nothing further; the session sits on that screen until someone acts on the \
             pane. Reported once per migration by the account migration's follow-up.",
            reason.as_str()
        ),
        "kind": "investigation",
        "topic": "session-identity",
        "resource_keys": [format!("session:{claude_session_id}")],
    })
}

/// The pane's rendered screen — the same `text_snapshot` read the usage-limit
/// watcher scans. `None` when the app or the terminal is gone.
fn pane_screen_text(terminal_id: &str) -> Option<String> {
    use tauri::Manager;
    let app = crate::tauri_app_handle::current()?;
    let tm = app.try_state::<std::sync::Arc<crate::terminal::TerminalManager>>()?;
    let session = tm.get(terminal_id)?;
    let grid = session.grid();
    let guard = grid.lock().unwrap_or_else(|e| e.into_inner());
    Some(guard.text_snapshot().text)
}

/// One `Esc` to the pane.
fn send_esc(terminal_id: &str) -> Result<(), String> {
    use tauri::Manager;
    let app = crate::tauri_app_handle::current().ok_or("no app handle")?;
    let tm = app
        .try_state::<std::sync::Arc<crate::terminal::TerminalManager>>()
        .ok_or("TerminalManager not managed")?;
    let session = tm.get(terminal_id).ok_or("terminal gone")?;
    session.write(
        b"\x1b",
        crate::terminal::session::PtyWriteCaller::AccountMigration,
    )
}

/// A fresh usage probe's verdict on `config_dir`.
async fn probe_exhaustion(config_dir: &str) -> Exhaustion {
    let usage = crate::commands::ai_settings::refresh_account_usage_snapshot().await;
    let probed = usage
        .iter()
        .any(|i| i.config_dir == config_dir && i.error.is_none());
    if !probed {
        Exhaustion::Unknown
    } else if crate::ai_provider::account_known_exhausted(config_dir) {
        Exhaustion::Exhausted
    } else {
        Exhaustion::NotExhausted
    }
}

/// Deliver the nudge; on a give-up, capture and log the screen, then either
/// `Esc` + retry once or report the stall.
async fn nudge_with_stall_recovery(p: &PostMigration, nudge: &str) {
    let mut retried = false;
    loop {
        let label = if retried {
            "continue-nudge-retry"
        } else {
            "continue-nudge"
        };
        let outcome = prompt_when_idle(&p.new_terminal_id, nudge, label, IdleWait::NUDGE).await;
        let IdlePromptOutcome::GaveUp { last_refusal } = outcome else {
            return;
        };
        let screen = pane_screen_text(&p.new_terminal_id);
        warn!(
            terminal_id = %p.new_terminal_id,
            session = %p.claude_session_id,
            label,
            last_refusal = %last_refusal,
            screen = %screen.as_deref().map_or_else(|| "<unreadable>".to_string(), screen_excerpt),
            "continue nudge gave up — what the resumed pane shows"
        );
        // Probe only when the decision depends on it.
        let exhaustion = if !retried && screen.as_deref().is_some_and(screen_shows_limit_wait) {
            probe_exhaustion(&p.dst_config_dir).await
        } else {
            Exhaustion::Unknown
        };
        match decide_stall(screen.as_deref(), exhaustion, retried) {
            StallAction::EscThenRetry => {
                if let Err(e) = send_esc(&p.new_terminal_id) {
                    warn!(terminal_id = %p.new_terminal_id, error = %e, "continue nudge: the one Esc could not be written");
                    report_stalled(p, StallReason::EscNotDelivered, screen.as_deref()).await;
                    return;
                }
                info!(
                    terminal_id = %p.new_terminal_id,
                    session = %p.claude_session_id,
                    account = %p.dst_config_dir,
                    "continue nudge: the pane sits in the CLI's limit-wait on an account the probe says is NOT exhausted — sent one Esc, retrying the nudge once"
                );
                retried = true;
            }
            StallAction::Stalled(reason) => {
                report_stalled(p, reason, screen.as_deref()).await;
                return;
            }
        }
    }
}

/// The stalled event plus one best-effort coord finding through the
/// duplicate-host reporter's door.
async fn report_stalled(p: &PostMigration, reason: StallReason, screen: Option<&str>) {
    let line = screen.map_or_else(|| "<screen unreadable>".to_string(), evidence_line);
    warn!(
        terminal_id = %p.new_terminal_id,
        session = %p.claude_session_id,
        reason = reason.as_str(),
        screen_line = %line,
        "account migration: the resumed session is STALLED — the nudge was not delivered; reported"
    );
    emit(
        "session-account-migration-stalled",
        json!({
            "claudeSessionId": p.claude_session_id,
            "terminalId": p.new_terminal_id,
            "toConfigDir": p.dst_config_dir,
            "reason": reason.as_str(),
            "screenLine": line,
        }),
    );
    crate::session::duplicate_hosts::post_session_finding(
        stall_finding_body(
            &p.claude_session_id,
            &p.new_terminal_id,
            &p.dst_config_dir,
            reason,
            &line,
        ),
        &p.claude_session_id,
        "account migration stall",
    )
    .await;
}

fn emit(event: &str, payload: serde_json::Value) {
    let Some(app) = crate::tauri_app_handle::current() else {
        return;
    };
    if let Err(e) = app.emit(event, payload) {
        warn!(error = %e, event, "failed to emit an account-migration follow-up event");
    }
}

#[cfg(test)]
#[path = "followup_tests.rs"]
mod tests;
