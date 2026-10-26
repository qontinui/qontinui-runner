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
//!    it — unless `auto_continue_after_migration` is off, when nothing is
//!    typed (the tail is still saved and reported). Then
//!    `session-account-migration-tail` reports the tail.
//!
//! Every screen rule reads the pane's BOTTOM rows only
//! ([`crate::terminal::usage_limit::bottom_rows_text`]): a banner or a spinner
//! quoted higher up the conversation is history, not state. The one `Esc` is
//! sent only after re-reading the screen, and only while the banner still
//! shows and no turn has started (2026-10-23 review fixes 1 and 2). A nudge
//! whose PTY write fails is a stall like a give-up (fix 5).
//!
//! ## The `session-account-migration-tail` event
//!
//! | field | meaning |
//! |---|---|
//! | `transcriptTail` | `rescued` / `empty` / `not-read` / `source-shorter-unknown` / `offset-unknown` |
//! | `transcriptTailLines`, `transcriptTailUnparseable` | counts; `null` when the tail is UNKNOWN (never 0) |
//! | `transcriptTailTruncated` | the read hit its byte bound; `null` when UNKNOWN |
//! | `transcriptTailPath` | where the tail was saved, when it was |
//! | `transcriptTailDelivery` | `none_to_deliver` / `delivered` / `submit_failed` / `gave_up` (the idle watcher stood down without submitting) / `skipped_auto_continue_off`; `null` when the tail is UNKNOWN |
//! | `transcriptTailDelivered` | `transcriptTailDelivery == "delivered"` |
//!
//! The decisions are pure ([`decide_stall`], [`screen_shows_limit_wait`],
//! [`esc_still_warranted`], [`nudge_step`], [`evidence_line`],
//! [`screen_excerpt`], [`stall_finding_body`], [`catch_up_plan`],
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

    let delivery = match catch_up_plan(tail.as_ref(), p.auto_continue) {
        CatchUpPlan::Unknown => None,
        CatchUpPlan::NoneToDeliver => Some(TailDelivery::NoneToDeliver),
        CatchUpPlan::SkipAutoContinueOff => {
            info!(
                terminal_id = %p.new_terminal_id,
                session = %p.claude_session_id,
                "migration tail catch-up NOT typed — auto_continue_after_migration is off; the rescued lines are on disk under <sid>/migration-tail/"
            );
            Some(TailDelivery::SkippedAutoContinueOff)
        }
        CatchUpPlan::Deliver(message) => {
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
            Some(tail_delivery(&outcome))
        }
    };
    emit(
        "session-account-migration-tail",
        tail_event(&p, tail.as_ref(), delivery),
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

/// What the follow-up does with a rescued tail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CatchUpPlan {
    /// The tail is UNKNOWN (not read, source shorter, no offset): nothing to
    /// decide from.
    Unknown,
    /// Read, and nothing in it the session needs to hear.
    NoneToDeliver,
    /// Something to deliver, but `auto_continue_after_migration` is off: the
    /// runner types nothing into the resumed session (2026-10-23 review fix
    /// 13). The tail is still saved and the event still sent.
    SkipAutoContinueOff,
    /// Type this ONE message once the pane is idle.
    Deliver(String),
}

/// Pure: the catch-up plan for a tail. `auto_continue` is
/// `auto_continue_after_migration` ([`PostMigration::auto_continue`]).
pub(crate) fn catch_up_plan(tail: Option<&TailRescue>, auto_continue: bool) -> CatchUpPlan {
    match tail {
        None | Some(TailRescue::SourceShorter { .. }) | Some(TailRescue::OffsetUnknown) => {
            CatchUpPlan::Unknown
        }
        Some(t) => match t.message() {
            None => CatchUpPlan::NoneToDeliver,
            Some(_) if !auto_continue => CatchUpPlan::SkipAutoContinueOff,
            Some(message) => CatchUpPlan::Deliver(message),
        },
    }
}

/// How the tail's catch-up ended — `transcriptTailDelivery`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TailDelivery {
    NoneToDeliver,
    Delivered,
    SubmitFailed,
    /// The idle watcher stood down without submitting: its deadline passed,
    /// or the pane went away.
    GaveUp,
    SkippedAutoContinueOff,
}

impl TailDelivery {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::NoneToDeliver => "none_to_deliver",
            Self::Delivered => "delivered",
            Self::SubmitFailed => "submit_failed",
            Self::GaveUp => "gave_up",
            Self::SkippedAutoContinueOff => "skipped_auto_continue_off",
        }
    }
}

/// Pure: the delivery an idle-watcher outcome amounts to.
pub(crate) fn tail_delivery(outcome: &IdlePromptOutcome) -> TailDelivery {
    match outcome {
        IdlePromptOutcome::Submitted => TailDelivery::Delivered,
        IdlePromptOutcome::SubmitFailed(_) => TailDelivery::SubmitFailed,
        IdlePromptOutcome::GaveUp { .. }
        | IdlePromptOutcome::TerminalGone
        | IdlePromptOutcome::AlreadyBusy => TailDelivery::GaveUp,
    }
}

/// Pure: the `session-account-migration-tail` payload (fields in the module
/// doc). Counts and `transcriptTailTruncated` are `null` when the tail was not
/// read (writer not proven gone, or a failed read), when the source was
/// shorter than the carry's offset, and when the carry recorded no offset —
/// all UNKNOWN, never zero. `delivery` is `None` exactly then too.
fn tail_event(
    p: &PostMigration,
    tail: Option<&TailRescue>,
    delivery: Option<TailDelivery>,
) -> serde_json::Value {
    let (status, lines, unparseable, truncated, path) = match tail {
        None => ("not-read", None, None, None, None),
        Some(TailRescue::Empty) => ("empty", Some(0), Some(0), Some(false), None),
        Some(TailRescue::SourceShorter { .. }) => {
            ("source-shorter-unknown", None, None, None, None)
        }
        Some(TailRescue::OffsetUnknown) => ("offset-unknown", None, None, None, None),
        Some(TailRescue::Rescued {
            path,
            scan,
            truncated,
            ..
        }) => (
            "rescued",
            Some(scan.lines),
            Some(scan.unparseable),
            Some(*truncated),
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
        "transcriptTailTruncated": truncated,
        "transcriptTailPath": path,
        "transcriptTailDelivery": delivery.map(TailDelivery::as_str),
        "transcriptTailDelivered": delivery == Some(TailDelivery::Delivered),
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
    /// The idle prompt was there, but writing the nudge to the PTY failed
    /// (2026-10-23 review fix 5).
    SubmitFailed,
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
            Self::SubmitFailed => "submit-failed",
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

/// Pure: whether the screen's BOTTOM rows
/// ([`crate::terminal::usage_limit::bottom_rows_text`]) show the CLI's
/// limit-wait banner (`… · continuing automatically at 6pm · esc to cancel`).
/// A banner higher up is history — a dialog or a draft below it is what the
/// CLI is showing now, and an `Esc` would cancel THAT (2026-10-23 review fix
/// 1). A bare `esc to cancel` also labels many ordinary CLI dialogs, so it
/// counts only beside a usage-limit phrase.
pub(crate) fn screen_shows_limit_wait(screen: &str) -> bool {
    let bottom = crate::terminal::usage_limit::bottom_rows_text(screen);
    let normalized = normalize(&bottom);
    normalized.contains("continuing automatically at")
        || (normalized.contains("esc to cancel")
            && crate::terminal::usage_limit::window_indicates_usage_limit(&bottom).is_some())
}

/// Pure: whether the one `Esc` may be sent on `screen`, re-read IMMEDIATELY
/// before it: the banner must still be in the bottom rows and no turn may
/// have started (2026-10-23 review fix 2). Between the give-up's read and the
/// `Esc` the probe ran for seconds; the CLI's own wait may have ended and a
/// turn begun, which the `Esc` would interrupt. `None` (unreadable) never
/// authorizes it.
pub(crate) fn esc_still_warranted(screen: Option<&str>) -> bool {
    screen.is_some_and(|s| {
        screen_shows_limit_wait(s) && !crate::terminal::usage_limit::screen_shows_turn(s)
    })
}

/// What one nudge attempt's outcome leads to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NudgeStep {
    /// Delivered, or moot (the session picked itself up, or the pane is gone).
    Done,
    /// The idle watcher gave up: decide between `Esc` + retry and a stall.
    GaveUp { last_refusal: String },
    /// Report this stall now.
    Stalled(StallReason),
}

/// Pure: the step an idle-watcher outcome leads to. A failed PTY write is a
/// stall like a give-up, never a silent end (2026-10-23 review fix 5).
pub(crate) fn nudge_step(outcome: IdlePromptOutcome) -> NudgeStep {
    match outcome {
        IdlePromptOutcome::Submitted
        | IdlePromptOutcome::AlreadyBusy
        | IdlePromptOutcome::TerminalGone => NudgeStep::Done,
        IdlePromptOutcome::SubmitFailed(_) => NudgeStep::Stalled(StallReason::SubmitFailed),
        IdlePromptOutcome::GaveUp { last_refusal } => NudgeStep::GaveUp { last_refusal },
    }
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

/// Pure: what happened, in words, for one stall reason — the finding says
/// exactly what the runner did and did not type (2026-10-23 review fix 10).
/// `esc_sent` is whether the one `Esc` was written (it matters for
/// [`StallReason::RetryGaveUp`] only: the `Esc` is withheld when the banner
/// had cleared by the re-read).
fn stall_what_happened(reason: StallReason, esc_sent: bool) -> &'static str {
    match reason {
        StallReason::ScreenUnreadable => {
            "the continue nudge gave up (the resumed CLI never painted an idle prompt) and the \
             pane's screen could not be read, so nothing was decided from it. The runner typed \
             nothing further"
        }
        StallReason::UnrecognisedScreen => {
            "the continue nudge gave up: the resumed CLI never painted an idle prompt, and the \
             screen shows neither that prompt nor the CLI's limit-wait banner. The runner typed \
             nothing further"
        }
        StallReason::LimitWaitOnExhaustedAccount => {
            "the continue nudge gave up: the pane sits in the CLI's limit-wait, and a fresh probe \
             says this account IS exhausted, so the wait is correct and no Esc was sent. The \
             runner typed nothing further"
        }
        StallReason::LimitWaitExhaustionUnknown => {
            "the continue nudge gave up: the pane sits in the CLI's limit-wait, and the probe \
             could not say whether this account is exhausted; an unknown never authorizes \
             cancelling the wait, so no Esc was sent. The runner typed nothing further"
        }
        StallReason::EscNotDelivered => {
            "the continue nudge gave up with the pane in the CLI's limit-wait on an account a \
             fresh probe says is NOT exhausted; the runner attempted ONE Esc to cancel the wait, \
             and writing it failed. Nothing else was typed"
        }
        StallReason::RetryGaveUp if esc_sent => {
            "the continue nudge gave up with the pane in the CLI's limit-wait on an account a \
             fresh probe says is NOT exhausted; the runner sent ONE Esc to cancel the wait and \
             retried the nudge once, and the retry gave up too. Nothing else was typed"
        }
        StallReason::RetryGaveUp => {
            "the continue nudge gave up with the pane in the CLI's limit-wait on an account a \
             fresh probe says is NOT exhausted; by the re-read just before the Esc the banner had \
             cleared (or a turn had started), so none was sent; the runner retried the nudge \
             once, and the retry gave up too. Nothing else was typed"
        }
        StallReason::SubmitFailed => {
            "the resumed CLI painted its idle prompt, but writing the continue nudge to the pane \
             failed. The runner typed nothing further"
        }
    }
}

/// Pure: the stall's coord finding — the shape
/// [`crate::session::duplicate_hosts::post_session_finding`] posts.
pub(crate) fn stall_finding_body(
    claude_session_id: &str,
    terminal_id: &str,
    to_config_dir: &str,
    reason: StallReason,
    screen_line: &str,
    esc_sent: bool,
) -> serde_json::Value {
    json!({
        "title": format!(
            "resumed session {claude_session_id} stalled after an account migration ({})",
            reason.as_str()
        ),
        "body": format!(
            "The runner migrated session {claude_session_id} to {to_config_dir} (pane \
             {terminal_id}), and {} ({}). The pane's screen read: \"{screen_line}\". The session \
             sits on that screen until someone acts on the pane. Reported once per migration by \
             the account migration's follow-up.",
            stall_what_happened(reason, esc_sent),
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
    let mut esc_sent = false;
    loop {
        let label = if retried {
            "continue-nudge-retry"
        } else {
            "continue-nudge"
        };
        let outcome = prompt_when_idle(&p.new_terminal_id, nudge, label, IdleWait::NUDGE).await;
        let last_refusal = match nudge_step(outcome) {
            NudgeStep::Done => return,
            NudgeStep::Stalled(reason) => {
                let screen = pane_screen_text(&p.new_terminal_id);
                report_stalled(p, reason, screen.as_deref(), esc_sent).await;
                return;
            }
            NudgeStep::GaveUp { last_refusal } => last_refusal,
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
                // The probe took seconds: re-read right before the Esc.
                let fresh = pane_screen_text(&p.new_terminal_id);
                if esc_still_warranted(fresh.as_deref()) {
                    if let Err(e) = send_esc(&p.new_terminal_id) {
                        warn!(terminal_id = %p.new_terminal_id, error = %e, "continue nudge: the one Esc could not be written");
                        report_stalled(p, StallReason::EscNotDelivered, fresh.as_deref(), false)
                            .await;
                        return;
                    }
                    esc_sent = true;
                    info!(
                        terminal_id = %p.new_terminal_id,
                        session = %p.claude_session_id,
                        account = %p.dst_config_dir,
                        "continue nudge: the pane sits in the CLI's limit-wait on an account the probe says is NOT exhausted — sent one Esc, retrying the nudge once"
                    );
                } else {
                    info!(
                        terminal_id = %p.new_terminal_id,
                        session = %p.claude_session_id,
                        "continue nudge: by the re-read the limit-wait banner had cleared (or a turn started) — no Esc sent, retrying the nudge once"
                    );
                }
                retried = true;
            }
            StallAction::Stalled(reason) => {
                report_stalled(p, reason, screen.as_deref(), esc_sent).await;
                return;
            }
        }
    }
}

/// The stalled event plus one best-effort coord finding through the
/// duplicate-host reporter's door.
async fn report_stalled(
    p: &PostMigration,
    reason: StallReason,
    screen: Option<&str>,
    esc_sent: bool,
) {
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
            "escSent": esc_sent,
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
            esc_sent,
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
