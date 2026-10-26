//! Tell sessions to save and pause, and say when it is safe (plan
//! `2026-10-21-operator-prepare-for-restart-and-progressive-restore`, Phase 4;
//! D2).
//!
//! ## Delivery (D2)
//!
//! Each tick while a prepare is in its window, every unfinished cohort row
//! with a live terminal and no `notified_at` is offered ONE fixed notice
//! ([`notice_text`]) through the session message poller's two typed-terminal
//! arms ([`admit`]): the idle arm (`session_message_poller::idle_arm`) for a
//! session at an idle prompt, and — only when the pane is not idle — the
//! `prompt_accepts_input` arm (`session_message_poller::prompt_accepts_input_arm`)
//! for a session still mid-turn whose input box is empty and focused. Both
//! arms share every screen check (cursor-row `❯`, empty input, no numbered
//! choice, no dialog chrome) and both require a live foreground `claude`, so a
//! dialog, a selected choice, an operator's draft or a bare shell gets
//! nothing, and the row is retried next tick. Claude Code 2.1.306 QUEUES text
//! typed into a busy turn (Phase 1 probe 2), so a working session reads the
//! notice at its next input point — possibly mid-turn, which is earlier, not
//! unsafe. A queued message leaves `Press up to edit queued messages` in the
//! input row, which the busy arm refuses. The re-send guard is the row's own
//! `notified_at` AND its terminal: a pane already told (under any of the
//! session ids its lifecycle records carry) is never told again, and a stamp
//! is kept in memory even when the record write fails, so a second tick never
//! re-sends. A paste that went out without its submitting Enter
//! (`PROMPT_PARTIALLY_WRITTEN`) sits in the input box as a draft; that row is
//! not retried — the operator sends or clears it.
//!
//! A row that is not a Claude session (provider or model), or has no live
//! terminal, is labelled ([`CohortRow::notice_skipped`]) and skipped.
//!
//! ## Row state (refreshed each tick from the census)
//!
//! [`row_state`] reads each row's OWN process out of the restart census — the
//! same `wind_down_observer::fresh_pass` `GET /restart-readiness` takes, joined
//! per row and classified with `claude_activity::classify` the way
//! `restart_readiness.rs` classifies each process — into `working`,
//! `notified`, `paused`, `finished`, `closed`, `limit_hit` or `unknown`.
//! `paused` = told AND the row's activity classification reads `idle`. Live
//! children are NOT a conjunct: every `claude` on a box using the stdio
//! coord-mcp shim holds a permanent `python3` child, so requiring none made
//! `paused` unreachable (the plan's Phase 4 results). The classifier already
//! treats a live child as a veto on its pane arm and falls back to Claude
//! Code's own per-process record, so a foreground tool call still reads
//! `working`. An `unknown` activity is `unknown`, never `paused`. Each row
//! carries the census children hint (`has_live_children`, null =
//! uncomputable, `restart_readiness::BOUNDARY`) as information only — a
//! restart ends that row's MCP servers and background shells — and it never
//! blocks.
//!
//! ## The verdict
//!
//! [`verdict_of`]: `ready_to_restart` only when every unfinished row is
//! `paused` or `limit_hit`-and-idle (finished and closed rows are not
//! blockers), the hold is `confirmed` (or `local` / `local (secondary)` where
//! there is no coord lever), and the cohort was re-captured at or after the
//! last row change it did not itself produce. Otherwise `blockers` names each
//! reason. It is stored in the record (`GET /control/restart-prep`) and
//! re-evaluated fresh against each request's own census as the `restart_prep`
//! block of `GET /restart-readiness` ([`readiness_block`]); `safe_to_restart`
//! there is unchanged.
//!
//! ## Cancel
//!
//! A cancelled prepare's rows that WERE told get one "restart cancelled —
//! resume your work" line ([`resume_text`]) through the same arms, once
//! (`resume_notified_at`), retried for [`RESUME_NOTICE_WINDOW_MS`].

use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use super::cohort::{merge_recapture, Cohort, CohortRow, PauseState, StateAtPrepare};
use super::prepare::RestartPrep;
use super::{rfc3339, HoldStatus, PrepState, PrepareRecord};
use crate::mcp::session_message_poller::{idle_arm, prompt_accepts_input_arm, GateMiss, InputPane};
use crate::session::claude_activity::{self, Activity, RecordReading};
use crate::session::tracking_health::{SessionWorkStatus, TrackingHealthReport};

/// Bumped when the notice wording changes, so a transcript can be matched to
/// the template that produced it.
pub const NOTICE_TEMPLATE_VERSION: u32 = 3;

/// The notice loop's cadence while a prepare is in its window.
pub const TICK: Duration = Duration::from_secs(15);

/// How long after a cancel the "resume your work" line is still retried.
pub const RESUME_NOTICE_WINDOW_MS: i64 = 30 * 60 * 1000;

/// The fixed save-and-pause notice (D2). One paragraph, no secrets, the
/// prepare id so the transcript names which prepare asked.
///
/// v3: the push is scoped to branches the session itself created. v2 said
/// "commit and push your work now", and an end-to-end run showed a session
/// read that as licence to push a branch it did not own (served policy
/// `git-operations` `own-artifact-lifecycle` / `shared-checkout-route-around`:
/// another session's branch and a shared checkout's branch are never yours).
pub fn notice_text(prepare_id: &str) -> String {
    format!(
        "[runner restart prep {prepare_id}, notice v{NOTICE_TEMPLATE_VERSION}] The operator is \
         preparing to restart this runner. Please commit your work now (or WIP-save it); push \
         ONLY a branch you created yourself, never another session's branch or a shared checkout's \
         branch. Then write a one-paragraph note here saying where you are and what comes next, \
         stop any background shells or watchers you started, end your turn and wait: do not \
         start any new task. This session will be resumed with this conversation after the \
         restart, so do not mark yourself finished unless your work really is finished."
    )
}

/// The one-line follow-up a cancelled prepare sends every session it told.
pub fn resume_text(prepare_id: &str) -> String {
    format!(
        "[runner restart prep {prepare_id}] Restart cancelled — resume your work where you left \
         off."
    )
}

// ---------------------------------------------------------------------------
// Delivery
// ---------------------------------------------------------------------------

/// Which of the two typed-terminal input arms admitted a paste.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputArm {
    /// The idle prompt (`session_message_poller::idle_arm`).
    Idle,
    /// A busy turn's empty input box (`prompt_accepts_input_arm`).
    BusyPrompt,
}

impl InputArm {
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::BusyPrompt => "busy_prompt",
        }
    }
}

/// One pane the notice can be typed into.
#[async_trait]
pub trait NoticePane: Send + Sync {
    /// What the two arms read.
    fn input(&self) -> &dyn InputPane;
    /// Submit `text` as one prompt (`TerminalSession::submit_prompt`).
    async fn submit(&self, text: &str) -> Result<(), String>;
}

/// The live terminals, by terminal id.
pub trait NoticeTerminals: Send + Sync {
    /// The pane for `terminal_id`, or `None` when no live terminal holds it.
    fn pane(&self, terminal_id: &str) -> Option<Box<dyn NoticePane>>;
}

/// Try the idle arm; only when the pane is NOT idle, the busy-prompt arm. Any
/// other refusal (a draft or dialog at an idle prompt, no live `claude`) is
/// final for this tick.
pub async fn admit(pane: &dyn InputPane) -> Result<InputArm, GateMiss> {
    match idle_arm(pane).await {
        Ok(()) => Ok(InputArm::Idle),
        Err(GateMiss::NotIdle) => prompt_accepts_input_arm(pane)
            .await
            .map(|()| InputArm::BusyPrompt),
        Err(miss) => Err(miss),
    }
}

/// [`admit`], then submit. `Err` names why nothing was delivered.
pub async fn deliver(pane: &dyn NoticePane, text: &str) -> Result<InputArm, String> {
    let arm = admit(pane.input())
        .await
        .map_err(|miss| miss.detail().to_string())?;
    pane.submit(text).await?;
    Ok(arm)
}

/// Why the notice loop does not try `row` at all: a session that is not Claude
/// Code. A row whose provider and model are unread is tried — the arms
/// themselves require a live `claude` in the pane.
fn not_claude(row: &CohortRow) -> Option<String> {
    if let Some(p) = row.provider.as_deref() {
        if !p.eq_ignore_ascii_case(crate::session::session_lifecycle_store::DEFAULT_PROVIDER) {
            return Some(format!("provider `{p}` is not Claude Code — no notice"));
        }
    }
    if let Some(m) = row.model.as_deref() {
        // `contains`, not `starts_with`: a Bedrock / Vertex id reads
        // `us.anthropic.claude-…`.
        if !m.to_ascii_lowercase().contains("claude") {
            return Some(format!("model `{m}` is not a Claude model — no notice"));
        }
    }
    None
}

/// Whether the last attempt left the text in the input box unsubmitted.
fn partially_written(row: &CohortRow) -> bool {
    row.notice_deferred
        .as_deref()
        .is_some_and(|d| d.starts_with(crate::terminal::session::PROMPT_PARTIALLY_WRITTEN))
}

/// terminal id → the `claude` pids told on it, from every row that was told
/// and is still open (`None` where the told process was not identified).
fn told_processes(cohort: &Cohort) -> HashMap<String, Vec<Option<u32>>> {
    let mut told: HashMap<String, Vec<Option<u32>>> = HashMap::new();
    for r in cohort
        .rows
        .iter()
        .filter(|r| r.notified_at.is_some() && r.left_capture_at.is_none())
    {
        told.entry(r.session.terminal_id.clone())
            .or_default()
            .push(r.notified_pid);
    }
    told
}

/// Whether a row's pane was already told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaneTold {
    /// Not told: deliver normally.
    No,
    /// Told this tick, or earlier to this same `claude` process.
    Yes,
    /// Told earlier, and this row's process cannot be matched either way —
    /// neither re-sent nor counted as told.
    Unmatched,
}

/// The re-send guard's pane half. A pane told THIS tick is told. A pane told
/// on an earlier tick is told for the same process; a DIFFERENT process (a
/// new `claude` started in the same shell) was told nothing and is delivered
/// to; a process that cannot be identified on either side is `Unmatched`.
fn pane_already_told(
    told_before: &HashMap<String, Vec<Option<u32>>>,
    told_this_tick: &HashSet<String>,
    terminal_id: &str,
    pid: Option<u32>,
) -> PaneTold {
    if told_this_tick.contains(terminal_id) {
        return PaneTold::Yes;
    }
    let Some(pids) = told_before.get(terminal_id) else {
        return PaneTold::No;
    };
    match pid {
        Some(p) if pids.contains(&Some(p)) => PaneTold::Yes,
        Some(_) if pids.iter().all(Option::is_some) => PaneTold::No,
        _ => PaneTold::Unmatched,
    }
}

/// Whether `row` is still owed the save-and-pause notice.
fn wants_notice(row: &CohortRow) -> bool {
    row.notified_at.is_none()
        && !partially_written(row)
        && row.finished != Some(true)
        && row.left_capture_at.is_none()
        && !matches!(
            row.pause_state,
            Some(PauseState::Finished | PauseState::Closed)
        )
}

// ---------------------------------------------------------------------------
// Census evidence and row state
// ---------------------------------------------------------------------------

/// What the census says about one row's top-level `claude` process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcEvidence {
    /// The process id, which the re-send guard matches a pane's told process by.
    pub pid: u32,
    /// `LiveClaudeProcess::has_live_children`: `None` is UNCOMPUTABLE.
    pub has_live_children: Option<bool>,
    /// The coord work axis reads `finished` for the session.
    pub coord_finished: bool,
    /// `claude_activity::classify` over the pane observation and Claude
    /// Code's own record, as `restart_readiness.rs` classifies each process.
    pub activity: Activity,
}

/// One census pass, joined per session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CensusEvidence {
    /// `Some(cause)` when the census pass could not be taken: every row that
    /// needs it then reads `unknown`.
    pub census_error: Option<String>,
    /// claude session id → its top-level terminal-hosted process.
    pub by_session: HashMap<String, ProcEvidence>,
    /// Terminal ids with a live terminal in the `TerminalManager`.
    pub live_terminals: HashSet<String>,
    /// Live `claude` processes (and unresolved holder panes) the census could
    /// not attribute to any session: holder-hosted, unclassified, ambiguous.
    /// A row with no attributed process may be one of them, so it is never
    /// read as `closed` while this is non-zero — and the verdict is not ready
    /// while any exists, because nothing told it and a restart kills it.
    pub unattributed_live: usize,
    /// This runner's reading of coord's device drain
    /// (`coord_drain_state::current().label()`: `drained`, `clear`,
    /// `unknown`, `not_enrolled`); `None` when it was not read. A `confirmed`
    /// hold counts only while this reads `drained`.
    pub device_drain: Option<String>,
    /// claude session id → its lifecycle record's `close_reason`, for each
    /// cohort row whose record is closed. The re-capture records it on a row
    /// that left the capture, which decides whether the rebuilt runner
    /// restores it ([`super::cohort::CohortRow::restore_candidate`]).
    pub close_reasons: HashMap<String, String>,
    /// claude session id → what its activity was classified FROM (the pane
    /// observation, the children hint, Claude Code's record), for the
    /// told-but-busy diagnostic ([`told_busy_diagnostic`]). Information only.
    pub activity_detail: HashMap<String, String>,
}

impl CensusEvidence {
    /// No census: every row that needs one reads `unknown`.
    pub fn unavailable(cause: impl Into<String>) -> Self {
        Self {
            census_error: Some(cause.into()),
            by_session: HashMap::new(),
            live_terminals: HashSet::new(),
            unattributed_live: 0,
            device_drain: None,
            close_reasons: HashMap::new(),
            activity_detail: HashMap::new(),
        }
    }
}

/// Join a census pass per session. `report` is `None` when the pass could not
/// be taken (`census_error` then says why). `activity` is the same evidence
/// `restart_readiness.rs` gathers for `live_claude.by_activity`.
pub fn evidence_from(
    report: Option<&TrackingHealthReport>,
    census_error: Option<String>,
    activity: &crate::mcp::restart_readiness::ActivityEvidence,
    live_terminals: HashSet<String>,
    device_drain: Option<String>,
) -> CensusEvidence {
    let Some(report) = report else {
        let mut ev = CensusEvidence::unavailable(
            census_error.unwrap_or_else(|| "the restart census could not be taken".to_string()),
        );
        ev.live_terminals = live_terminals;
        ev.device_drain = device_drain;
        return ev;
    };
    let mut by_session: HashMap<String, ProcEvidence> = HashMap::new();
    let mut ambiguous: HashSet<String> = HashSet::new();
    let mut activity_detail: HashMap<String, String> = HashMap::new();
    let mut unattributed_live = report.holder_hosted.len()
        + report.live_untracked.len()
        + report.holder_unresolved_count()
        + usize::from(report.holder_census_error.is_some());
    for p in report
        .terminal_hosted
        .iter()
        .filter(|p| !p.nested_under_claude)
    {
        let Some(sid) = p.session_id.clone() else {
            unattributed_live += 1;
            continue;
        };
        let ev = ProcEvidence {
            pid: p.pid,
            has_live_children: p.has_live_children,
            coord_finished: matches!(
                p.session_status.as_deref().map(SessionWorkStatus::parse),
                Some(SessionWorkStatus::Finished)
            ),
            activity: claude_activity::classify(
                activity.pane_by_pid.get(&p.pid),
                p.has_live_children,
                activity
                    .records
                    .get(&p.pid)
                    .unwrap_or(&RecordReading::Missing),
                activity.now_ms,
            ),
        };
        let record = activity
            .records
            .get(&p.pid)
            .unwrap_or(&RecordReading::Missing);
        activity_detail.insert(
            sid.clone(),
            format!(
                "pane={:?} children={:?} record={record:?}",
                activity.pane_by_pid.get(&p.pid),
                p.has_live_children
            ),
        );
        if by_session.insert(sid.clone(), ev).is_some() {
            ambiguous.insert(sid);
        }
    }
    // Two top-level processes claiming one session: neither can speak for it.
    for sid in ambiguous {
        if let Some(ev) = by_session.get_mut(&sid) {
            ev.activity = Activity::Unknown;
            ev.has_live_children = None;
        }
    }
    CensusEvidence {
        census_error,
        by_session,
        live_terminals,
        unattributed_live,
        device_drain,
        close_reasons: HashMap::new(),
        activity_detail,
    }
}

/// A row's state from the census: `(state, idle_now, reason)`.
pub fn row_state(row: &CohortRow, ev: &CensusEvidence) -> (PauseState, Option<bool>, String) {
    let id = row.session.claude_session_id.as_str();
    let proc = ev.by_session.get(id);
    let finished = row.finished == Some(true) || proc.is_some_and(|p| p.coord_finished);
    if row.left_capture_at.is_some() {
        let why = if finished {
            "finished, and closed since (wound down)"
        } else {
            "the session's record closed since the prepare — it is no longer running here"
        };
        return (PauseState::Closed, None, why.to_string());
    }
    if finished {
        return (
            PauseState::Finished,
            None,
            "declared finished on the work axis — still running until wind-down closes it"
                .to_string(),
        );
    }
    if let Some(cause) = &ev.census_error {
        return (
            PauseState::Unknown,
            None,
            format!("the restart census could not be taken: {cause}"),
        );
    }
    let Some(p) = proc else {
        if !ev.live_terminals.contains(&row.session.terminal_id) && ev.unattributed_live == 0 {
            return (
                PauseState::Closed,
                None,
                "no live terminal and no live `claude` process for this session".to_string(),
            );
        }
        let why = if ev.unattributed_live > 0 {
            format!(
                "no live `claude` process is attributed to this session, and {} live process(es) \
                 or pane(s) are unattributed — it may be one of them",
                ev.unattributed_live
            )
        } else {
            "its terminal is live but no `claude` process is attributed to this session (exited, \
             or ambiguous)"
                .to_string()
        };
        return (PauseState::Unknown, None, why);
    };
    // The activity classification alone: children are reported, not required.
    let idle_now = match p.activity {
        Activity::Unknown => None,
        Activity::Idle => Some(true),
        Activity::Working | Activity::Stale => Some(false),
    };
    if row.state_at_prepare == StateAtPrepare::LimitHit {
        let why = match idle_now {
            Some(true) => "usage limit hit; idle",
            Some(false) => "usage limit hit; not idle",
            None => "usage limit hit; activity unreadable",
        };
        return (PauseState::LimitHit, idle_now, why.to_string());
    }
    if p.activity == Activity::Unknown {
        return (
            PauseState::Unknown,
            None,
            "its activity could not be classified from the pane or its Claude Code record"
                .to_string(),
        );
    }
    if row.notified_at.is_some() {
        if idle_now == Some(true) {
            return (PauseState::Paused, idle_now, "told; now idle".to_string());
        }
        return (
            PauseState::Notified,
            idle_now,
            "told; still working".to_string(),
        );
    }
    let why = match (&row.notice_skipped, &row.notice_deferred) {
        (Some(skip), _) => format!("not told: {skip}"),
        (None, Some(deferred)) => format!("not told yet: {deferred}"),
        (None, None) => "not told yet".to_string(),
    };
    (PauseState::Working, idle_now, why)
}

/// The told-but-busy diagnostic: for a row that was told to pause and still
/// does not read idle (`notified`), one line naming what the classification
/// read — the activity, what it was classified from, and which
/// `snapshot_looks_idle` check vetoes the pane's grid right now (`grid_veto`,
/// from `looping_agent::idle::idle_veto`; `None` = the grid itself reads idle,
/// so the veto is elsewhere). `None` for any other row.
///
/// Why it exists: an end-to-end run saw a told session on a page that was not
/// in view stay "told; still working" for ~11 minutes after its turn ended,
/// and flip to `paused` as soon as its page was shown; nothing in the log said
/// which input kept it busy.
pub fn told_busy_diagnostic(
    row: &CohortRow,
    ev: &CensusEvidence,
    grid_veto: Option<&str>,
) -> Option<String> {
    if row.pause_state != Some(PauseState::Notified) {
        return None;
    }
    let id = row.session.claude_session_id.as_str();
    let activity = ev
        .by_session
        .get(id)
        .map(|p| format!("{:?}", p.activity))
        .unwrap_or_else(|| "no process".to_string());
    let detail = ev
        .activity_detail
        .get(id)
        .map(String::as_str)
        .unwrap_or("-");
    Some(format!(
        "activity={activity}; grid_veto={}; {detail}",
        grid_veto.unwrap_or("none (the grid reads idle)")
    ))
}

/// The last told-but-busy line logged per session, so a row that stays busy
/// for the same reason logs once, and again only when the reason changes.
static TOLD_BUSY_LOGGED: std::sync::Mutex<Option<HashMap<String, String>>> =
    std::sync::Mutex::new(None);

/// Log [`told_busy_diagnostic`] for every told-but-busy row whose line changed
/// since it was last logged. Returns the lines it logged (for tests).
pub fn log_told_busy(
    cohort: &Cohort,
    ev: &CensusEvidence,
    grid_veto_of: &dyn Fn(&str) -> Option<String>,
) -> Vec<String> {
    let mut logged = Vec::new();
    let mut guard = TOLD_BUSY_LOGGED.lock().unwrap_or_else(|e| e.into_inner());
    let last = guard.get_or_insert_with(HashMap::new);
    // Only a told-but-busy row's pane is read: every other row costs nothing.
    for row in cohort
        .rows
        .iter()
        .filter(|r| r.pause_state == Some(PauseState::Notified))
    {
        let veto = grid_veto_of(&row.session.terminal_id);
        let Some(line) = told_busy_diagnostic(row, ev, veto.as_deref()) else {
            continue;
        };
        let id = &row.session.claude_session_id;
        if last.get(id) == Some(&line) {
            continue;
        }
        info!(session = %id, "restart_prep: told but not idle — {line}");
        last.insert(id.clone(), line.clone());
        logged.push(line);
    }
    logged
}

/// Refresh every row's state from `ev`.
pub fn refresh_rows(cohort: &mut Cohort, ev: &CensusEvidence) {
    for row in &mut cohort.rows {
        let (state, idle_now, reason) = row_state(row, ev);
        row.pause_state = Some(state);
        row.idle_now = idle_now;
        row.pause_reason = Some(reason);
        row.has_live_children = ev
            .by_session
            .get(&row.session.claude_session_id)
            .and_then(|p| p.has_live_children);
    }
}

// ---------------------------------------------------------------------------
// The verdict
// ---------------------------------------------------------------------------

/// One reason the runner is not ready to restart. `session` is `None` for a
/// prepare-wide reason (the hold, the re-capture, the census).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Blocker {
    pub session: Option<String>,
    pub display_name: Option<String>,
    pub reason: String,
}

/// `ready_to_restart` and why not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrepVerdict {
    pub prepare_id: String,
    pub state: PrepState,
    /// The hold as the panel spells it.
    pub hold: String,
    pub ready_to_restart: bool,
    pub blockers: Vec<Blocker>,
    /// When the cohort the verdict read was re-captured.
    pub cohort_captured_at_ms: i64,
    pub evaluated_at_ms: i64,
    pub evaluated_at: String,
    /// The runner PROCESS that evaluated it ([`super::running_process`]). A
    /// verdict persisted by a process that has since restarted describes
    /// sessions that are gone; `None` on a verdict written before this field.
    #[serde(default)]
    pub evaluated_by: Option<String>,
}

fn prepare_wide(reason: String) -> Blocker {
    Blocker {
        session: None,
        display_name: None,
        reason,
    }
}

/// PURE: the verdict over `rec`'s rows as they stand (refresh them first).
/// `capture_error` is a re-capture that failed this evaluation.
pub fn verdict_of(
    rec: &PrepareRecord,
    ev: &CensusEvidence,
    capture_error: Option<&str>,
    now_ms: i64,
) -> PrepVerdict {
    let mut blockers = Vec::new();
    if !rec.in_window() {
        blockers.push(prepare_wide(format!(
            "no prepare is in its window (state: {})",
            serde_json::to_value(rec.state)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default()
        )));
    }
    let hold_label = if rec.in_window() && rec.hold_expired(now_ms) {
        let at = rec.hold_until.clone().unwrap_or_default();
        blockers.push(prepare_wide(format!("the spawn hold expired at {at}")));
        format!("hold expired at {at}")
    } else {
        if !matches!(
            rec.hold,
            HoldStatus::Confirmed | HoldStatus::Local | HoldStatus::LocalSecondary
        ) {
            blockers.push(prepare_wide(format!(
                "the spawn hold is not confirmed ({})",
                rec.hold.label()
            )));
        }
        if rec.hold == HoldStatus::Confirmed && ev.device_drain.as_deref() != Some("drained") {
            // Confirmed once is not held now: an operator release, or coord
            // letting the lane go, would put agent-lane work back on this
            // device while the record still says `confirmed`.
            blockers.push(prepare_wide(format!(
                "the confirmed spawn hold is no longer observed: this runner reads coord's \
                 device drain as `{}`",
                ev.device_drain.as_deref().unwrap_or("unread")
            )));
        }
        rec.hold.label()
    };
    if let Some(e) = capture_error {
        blockers.push(prepare_wide(format!(
            "the cohort could not be re-captured: {e}"
        )));
    }
    let last_change = rec.last_change_at_ms.unwrap_or(rec.started_at_ms);
    if rec.cohort.captured_at_ms < last_change {
        blockers.push(prepare_wide(
            "the cohort has not been re-captured since its last change".to_string(),
        ));
    }
    if let Some(cause) = &ev.census_error {
        blockers.push(prepare_wide(format!(
            "the restart census could not be taken: {cause}"
        )));
    }
    if ev.unattributed_live > 0 {
        blockers.push(prepare_wide(format!(
            "{} live `claude` process(es) or holder pane(s) are attributed to no session \
             (holder-hosted, unclassified or ambiguous) — not told, and not in the cohort",
            ev.unattributed_live
        )));
    }
    for row in &rec.cohort.rows {
        let ok = match row.pause_state {
            Some(PauseState::Paused | PauseState::Finished | PauseState::Closed) => true,
            Some(PauseState::LimitHit) => row.idle_now == Some(true),
            _ => false,
        };
        if !ok {
            let state = row
                .pause_state
                .and_then(|s| serde_json::to_value(s).ok())
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_else(|| "not yet read".to_string());
            blockers.push(Blocker {
                session: Some(row.session.claude_session_id.clone()),
                display_name: row.display_name(),
                reason: format!(
                    "{state}: {}",
                    row.pause_reason.as_deref().unwrap_or("no reading yet")
                ),
            });
        }
    }
    // A live session the cohort does not hold was opened after its last
    // re-capture: nothing told it, and it is not recorded for the restore.
    let known: HashSet<&str> = rec
        .cohort
        .rows
        .iter()
        .map(|r| r.session.claude_session_id.as_str())
        .collect();
    let mut uncaptured: Vec<&String> = ev
        .by_session
        .keys()
        .filter(|sid| !known.contains(sid.as_str()))
        .collect();
    uncaptured.sort();
    for sid in uncaptured {
        blockers.push(Blocker {
            session: Some(sid.clone()),
            display_name: None,
            reason: "a live session opened after the cohort's last re-capture — not yet recorded \
                     or told"
                .to_string(),
        });
    }
    PrepVerdict {
        prepare_id: rec.prepare_id.clone(),
        state: rec.state,
        hold: hold_label,
        ready_to_restart: blockers.is_empty(),
        blockers,
        cohort_captured_at_ms: rec.cohort.captured_at_ms,
        evaluated_at_ms: now_ms,
        evaluated_at: rfc3339(now_ms),
        evaluated_by: Some(super::running_process().to_string()),
    }
}

// ---------------------------------------------------------------------------
// The tick
// ---------------------------------------------------------------------------

/// What one notice tick did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TickOutcome {
    NoRecord,
    /// The record is not in its window and owes no resume line.
    Idle(PrepState),
    /// The tick ran: how many notices it delivered, and the verdict.
    Ran {
        delivered: usize,
        ready_to_restart: bool,
        changed: bool,
    },
    /// A cancelled prepare's resume pass: how many lines it delivered.
    Resumed {
        delivered: usize,
    },
    Failed(String),
}

/// A row told to pause that has not had the resume line, is not finished and
/// is still open, and whose resume attempt did not leave a draft behind.
fn owes_resume_line(row: &CohortRow) -> bool {
    row.notified_at.is_some()
        && row.resume_notified_at.is_none()
        && row.finished != Some(true)
        && row.left_capture_at.is_none()
        && !partially_written(row)
}

impl RestartPrep {
    /// One notice-loop tick over a prepare in its window: fold `capture` (the
    /// ledger re-capture) into the cohort, refresh every row's state from
    /// `census`, deliver the notice to every row still owed it, compute the
    /// verdict, and move the record `preparing ⇄ ready` with it. Persists and
    /// emits `restart-prep-changed` only when something visible changed.
    pub async fn notice_tick(
        &self,
        terminals: &dyn NoticeTerminals,
        capture: Result<Cohort, String>,
        census: &CensusEvidence,
        now_ms: i64,
    ) -> TickOutcome {
        let _op = self.ops.lock().await;
        let Some(rec) = self.record() else {
            return TickOutcome::NoRecord;
        };
        if !rec.in_window() {
            return TickOutcome::Idle(rec.state);
        }
        let (mut cohort, capture_error) = match capture {
            Ok(fresh) => (
                merge_recapture(&rec.cohort, fresh, now_ms, &|id| {
                    census.close_reasons.get(id).cloned()
                }),
                None,
            ),
            Err(e) => (rec.cohort.clone(), Some(e)),
        };
        refresh_rows(&mut cohort, census);
        log_told_busy(&cohort, census, &|terminal_id| {
            let screen = terminals.pane(terminal_id)?.input().screen();
            qontinui_runner_lib::looping_agent::idle::idle_veto(&screen.lines, screen.cursor_row)
        });

        let mut delivered = 0usize;
        if rec.is_active(now_ms) {
            let text = notice_text(&rec.prepare_id);
            // The pane half of the re-send guard: one lifecycle record per
            // pane is not guaranteed (a transient phantom row; a new session id
            // after `/clear`), and the notice goes to the PANE's process.
            let told_before = told_processes(&cohort);
            let mut told_this_tick: HashSet<String> = HashSet::new();
            for row in cohort.rows.iter_mut().filter(|r| wants_notice(r)) {
                let pid = census
                    .by_session
                    .get(&row.session.claude_session_id)
                    .map(|p| p.pid);
                match pane_already_told(
                    &told_before,
                    &told_this_tick,
                    &row.session.terminal_id,
                    pid,
                ) {
                    PaneTold::Yes => {
                        // The same pane and process were told through
                        // another record.
                        row.notified_at = Some(now_ms);
                        row.notified_pid = pid;
                        row.notice_arm = Some("shared_terminal".to_string());
                        row.notice_skipped = None;
                        row.notice_deferred = None;
                        continue;
                    }
                    PaneTold::Unmatched => {
                        row.notice_deferred = Some(
                            "this pane was already told, and this record's process cannot be \
                             matched to the one that was — not re-sent"
                                .to_string(),
                        );
                        continue;
                    }
                    PaneTold::No => {}
                }
                if let Some(why) = not_claude(row) {
                    row.notice_skipped = Some(why);
                    continue;
                }
                let Some(pane) = terminals.pane(&row.session.terminal_id) else {
                    row.notice_skipped = Some("no live terminal — no notice".to_string());
                    continue;
                };
                row.notice_skipped = None;
                match deliver(pane.as_ref(), &text).await {
                    Ok(arm) => {
                        info!(
                            prepare = %rec.prepare_id,
                            session = %row.session.claude_session_id,
                            arm = arm.label(),
                            "restart_prep: save-and-pause notice delivered"
                        );
                        row.notified_at = Some(now_ms);
                        row.notified_pid = pid;
                        row.notice_arm = Some(arm.label().to_string());
                        row.notice_deferred = None;
                        row.pause_state = Some(PauseState::Notified);
                        row.pause_reason = Some("told this tick".to_string());
                        told_this_tick.insert(row.session.terminal_id.clone());
                        delivered += 1;
                    }
                    Err(why) => row.notice_deferred = Some(why),
                }
            }
        }

        let applied = self.update_quiet(&rec.prepare_id, |r| {
            // A `limit_hit` the freeze marked while this tick awaited the
            // panes is kept; its `last_change_at_ms` makes the verdict wait.
            for row in &mut cohort.rows {
                if let Some(cur) = r
                    .cohort
                    .rows
                    .iter()
                    .find(|c| c.session.claude_session_id == row.session.claude_session_id)
                {
                    if cur.state_at_prepare == StateAtPrepare::LimitHit {
                        row.state_at_prepare = StateAtPrepare::LimitHit;
                        row.limit_reset_at = cur.limit_reset_at.or(row.limit_reset_at);
                    }
                }
            }
            r.cohort = cohort;
            if delivered > 0 {
                r.last_change_at_ms = Some(r.last_change_at_ms.unwrap_or(0).max(now_ms));
            }
            let v = verdict_of(r, census, capture_error.as_deref(), now_ms);
            r.state = match (r.state, v.ready_to_restart) {
                (PrepState::Preparing, true) => PrepState::Ready,
                (PrepState::Ready, false) => PrepState::Preparing,
                (s, _) => s,
            };
            r.verdict = Some(PrepVerdict {
                state: r.state,
                ..v
            });
        });
        match applied {
            Ok((r, changed)) => TickOutcome::Ran {
                delivered,
                ready_to_restart: r.verdict.as_ref().is_some_and(|v| v.ready_to_restart),
                changed,
            },
            Err(e) => {
                warn!("{e}");
                TickOutcome::Failed(e)
            }
        }
    }

    /// Whether `rec` still owes a "resume your work" line at `now_ms`.
    ///
    /// Also a same-build Release hold (Phase 6): the record moves to
    /// `restoring`, and a told session that survived the restart (a pane a PTY
    /// holder kept and the runner adopted) is still waiting to be told to
    /// resume. A release on a REBUILT runner owes nothing — every session ended
    /// with the old build.
    pub fn owes_resume(rec: &PrepareRecord, now_ms: i64) -> bool {
        let released_by_cancel = match rec.state {
            PrepState::Cancelled => true,
            PrepState::Restoring => rec.release.as_ref().is_some_and(|r| {
                r.trigger == super::ReleaseTrigger::Cancel && r.on_build == rec.prepared_build
            }),
            _ => false,
        };
        released_by_cancel
            && rec
                .release
                .as_ref()
                .is_some_and(|r| now_ms - r.at_ms <= RESUME_NOTICE_WINDOW_MS)
            && rec.cohort.rows.iter().any(owes_resume_line)
    }

    /// A cancelled prepare's resume pass: every row that was told to pause and
    /// has not had the resume line gets it once, through the same arms.
    pub async fn resume_tick(&self, terminals: &dyn NoticeTerminals, now_ms: i64) -> TickOutcome {
        let _op = self.ops.lock().await;
        let Some(rec) = self.record() else {
            return TickOutcome::NoRecord;
        };
        if !Self::owes_resume(&rec, now_ms) {
            return TickOutcome::Idle(rec.state);
        }
        let text = resume_text(&rec.prepare_id);
        let mut outcome: HashMap<String, Result<(), String>> = HashMap::new();
        let mut resumed_terminals: HashSet<String> = rec
            .cohort
            .rows
            .iter()
            .filter(|r| r.resume_notified_at.is_some() && r.left_capture_at.is_none())
            .map(|r| r.session.terminal_id.clone())
            .collect();
        for row in rec.cohort.rows.iter().filter(|r| owes_resume_line(r)) {
            let id = row.session.claude_session_id.clone();
            if resumed_terminals.contains(&row.session.terminal_id) {
                outcome.insert(id, Ok(()));
                continue;
            }
            let result = match terminals.pane(&row.session.terminal_id) {
                None => Err("no live terminal — the resume line was not sent".to_string()),
                Some(pane) => deliver(pane.as_ref(), &text).await.map(|arm| {
                    info!(
                        prepare = %rec.prepare_id,
                        session = %id,
                        arm = arm.label(),
                        "restart_prep: resume line delivered after cancel"
                    );
                }),
            };
            if result.is_ok() {
                resumed_terminals.insert(row.session.terminal_id.clone());
            }
            outcome.insert(id, result);
        }
        let delivered = outcome.values().filter(|r| r.is_ok()).count();
        let applied = self.update_quiet(&rec.prepare_id, |r| {
            for row in &mut r.cohort.rows {
                match outcome.get(&row.session.claude_session_id) {
                    Some(Ok(())) => {
                        row.resume_notified_at = Some(now_ms);
                        row.notice_deferred = None;
                    }
                    Some(Err(why)) => row.notice_deferred = Some(why.clone()),
                    None => {}
                }
            }
        });
        match applied {
            Ok(_) => TickOutcome::Resumed { delivered },
            Err(e) => TickOutcome::Failed(e),
        }
    }
}

// ---------------------------------------------------------------------------
// The `restart_prep` block of GET /restart-readiness
// ---------------------------------------------------------------------------

/// One row as the readiness block reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RowReading {
    pub session: String,
    pub display_name: Option<String>,
    pub state: Option<PauseState>,
    pub reason: Option<String>,
    pub notified_at: Option<i64>,
    /// The census children hint, information only (see
    /// [`CohortRow::has_live_children`]).
    pub has_live_children: Option<bool>,
}

/// The `restart_prep` block: the verdict re-evaluated against THIS request's
/// census over the cohort the notice loop last re-captured. Read-only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RestartPrepBlock {
    #[serde(flatten)]
    pub verdict: PrepVerdict,
    pub rows: Vec<RowReading>,
    /// What the block was computed from.
    pub source: &'static str,
}

/// The readiness block's `source`.
pub const READINESS_SOURCE: &str = "this request's restart census, joined per row over the \
     cohort the notice loop last re-captured; read-only — the notice loop's own stored verdict \
     is in GET /control/restart-prep";

/// PURE: [`RestartPrepBlock`] for `rec` against `ev`. Writes nothing.
pub fn evaluate_fresh(rec: &PrepareRecord, ev: &CensusEvidence, now_ms: i64) -> RestartPrepBlock {
    let mut r = rec.clone();
    refresh_rows(&mut r.cohort, ev);
    let verdict = verdict_of(&r, ev, None, now_ms);
    RestartPrepBlock {
        verdict,
        rows: r
            .cohort
            .rows
            .iter()
            .map(|row| RowReading {
                session: row.session.claude_session_id.clone(),
                display_name: row.display_name(),
                state: row.pause_state,
                reason: row.pause_reason.clone(),
                notified_at: row.notified_at,
                has_live_children: row.has_live_children,
            })
            .collect(),
        source: READINESS_SOURCE,
    }
}

/// The terminal ids among `ids` with a live terminal in the app's manager.
fn live_terminals_in<'a>(
    app: &tauri::AppHandle,
    ids: impl Iterator<Item = &'a str>,
) -> HashSet<String> {
    use tauri::Manager;
    let Some(tm) = app.try_state::<Arc<crate::terminal::TerminalManager>>() else {
        return HashSet::new();
    };
    ids.filter(|id| tm.get(id).is_some_and(|t| t.is_alive()))
        .map(str::to_string)
        .collect()
}

/// The `restart_prep` block for `GET /restart-readiness`, from the census
/// that request already took. `None` when this runner holds no prepare record.
pub fn readiness_block(
    app: &tauri::AppHandle,
    report: Option<&TrackingHealthReport>,
    census_error: Option<String>,
    activity: &crate::mcp::restart_readiness::ActivityEvidence,
    now_ms: i64,
) -> Option<RestartPrepBlock> {
    let rec = super::prepare::install_live()?.record()?;
    let live = live_terminals_in(
        app,
        rec.cohort
            .rows
            .iter()
            .map(|r| r.session.terminal_id.as_str()),
    );
    let ev = evidence_from(
        report,
        census_error,
        activity,
        live,
        Some(crate::coord_drain_state::current().label().to_string()),
    );
    Some(evaluate_fresh(&rec, &ev, now_ms))
}

// ---------------------------------------------------------------------------
// The live loop
// ---------------------------------------------------------------------------

/// One read of Claude Code's session records at a time for this loop — its
/// own flag, so a slow read here never starves `GET /restart-readiness` of
/// its activity split.
static RECORD_READ_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

/// Set once the loop has waited out the boot read's bound. Past it, a boot
/// read that never finished (its runtime failed to start) must not switch the
/// notice loop off for the life of the process: ticks run anyway, warned.
static BOOT_WAIT_OVER: AtomicBool = AtomicBool::new(false);

/// The live pane: a terminal from the `TerminalManager`.
struct LiveNoticePane(crate::mcp::session_message_poller::LivePane);

#[async_trait]
impl NoticePane for LiveNoticePane {
    fn input(&self) -> &dyn InputPane {
        &self.0
    }

    async fn submit(&self, text: &str) -> Result<(), String> {
        self.0
             .0
            .submit_prompt(
                text,
                crate::terminal::session::PtyWriteCaller::RestartPrepNotice,
            )
            .map(|_| ())
    }
}

struct LiveTerminals(Option<Arc<crate::terminal::TerminalManager>>);

impl NoticeTerminals for LiveTerminals {
    fn pane(&self, terminal_id: &str) -> Option<Box<dyn NoticePane>> {
        let term = self.0.as_ref()?.get(terminal_id)?;
        term.is_alive().then(|| {
            Box::new(LiveNoticePane(
                crate::mcp::session_message_poller::LivePane(term),
            )) as Box<dyn NoticePane>
        })
    }
}

/// The census and its per-row join, from a fresh pass.
async fn gather_census(app: &tauri::AppHandle, rec: &PrepareRecord) -> CensusEvidence {
    use crate::mcp::restart_readiness::{gather_activity_evidence, ActivityEvidence};
    let fresh = crate::session::wind_down_observer::fresh_pass(
        app,
        qontinui_runner_lib::wind_down::grace_from_env(),
    )
    .await;
    let activity = match fresh.pass.as_ref() {
        Some(p) => {
            gather_activity_evidence(
                &p.report,
                &fresh.observed,
                crate::terminal::transcript::find_claude_config_dirs(),
                claude_activity::proc_start_ticks,
                crate::mcp::restart_readiness::RECORD_READ_TIMEOUT,
                &RECORD_READ_IN_FLIGHT,
            )
            .await
        }
        None => ActivityEvidence::default(),
    };
    let census_error = fresh.pass.is_none().then(|| {
        if fresh.unknowns.is_empty() {
            "the restart census could not be taken".to_string()
        } else {
            fresh.unknowns.join("; ")
        }
    });
    let live = live_terminals_in(
        app,
        rec.cohort
            .rows
            .iter()
            .map(|r| r.session.terminal_id.as_str()),
    );
    let mut evidence = evidence_from(
        fresh.pass.as_ref().map(|p| &p.report),
        census_error,
        &activity,
        live,
        Some(crate::coord_drain_state::current().label().to_string()),
    );
    evidence.close_reasons = close_reasons_in(app, rec);
    evidence
}

/// The lifecycle `close_reason` of every cohort row of `rec` whose record is
/// closed (see [`CensusEvidence::close_reasons`]).
fn close_reasons_in(app: &tauri::AppHandle, rec: &PrepareRecord) -> HashMap<String, String> {
    use tauri::Manager;
    let Some(store) =
        app.try_state::<Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>()
    else {
        return HashMap::new();
    };
    rec.cohort
        .rows
        .iter()
        .filter_map(|row| {
            let id = &row.session.claude_session_id;
            let record = store.get(id)?;
            (record.state == "closed")
                .then_some(record.close_reason)
                .flatten()
                .map(|reason| (id.clone(), reason))
        })
        .collect()
}

/// One tick on the live record: the notice tick while a prepare is in its
/// window, the resume pass while a cancelled one still owes it, else nothing.
pub async fn tick_live() {
    let Some(app) = crate::tauri_app_handle::current() else {
        return; // headless / unit-test context
    };
    let Some(ctl) = super::prepare::install_live() else {
        return;
    };
    // Before the boot step has run, a rebuilt runner's record still reads
    // `preparing` over a cohort whose sessions are gone: scoring it now would
    // record them `closed` and could read `ready`.
    if !crate::coord_drain_state::boot_read_complete()
        && !BOOT_WAIT_OVER.load(std::sync::atomic::Ordering::SeqCst)
    {
        return;
    }
    let Some(rec) = ctl.record() else {
        return;
    };
    let terminals = {
        use tauri::Manager;
        LiveTerminals(
            app.try_state::<Arc<crate::terminal::TerminalManager>>()
                .map(|t| t.inner().clone()),
        )
    };
    let outcome = if rec.in_window() {
        let census = gather_census(&app, &rec).await;
        let capture_app = app.clone();
        let capture = qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked(move || {
            super::live_cohort_from_app(&capture_app)
        })
        .await
        .map_err(|e| format!("cohort re-capture task failed: {e}"))
        .and_then(|r| r);
        // Read AFTER the re-capture: a notice stamped now is later than the
        // capture, so the verdict waits for the next re-capture to see it.
        ctl.notice_tick(&terminals, capture, &census, super::prepare::now_ms())
            .await
    } else {
        let now = super::prepare::now_ms();
        if !RestartPrep::owes_resume(&rec, now) {
            return;
        }
        ctl.resume_tick(&terminals, now).await
    };
    tracing::debug!(outcome = ?outcome, "restart_prep: notice tick");
}

/// Start the notice loop under the shared task supervisor (a panicking tick
/// respawns the loop). It does nothing on a tick with no prepare to serve.
pub fn start() {
    tauri::async_runtime::spawn(async move {
        crate::mcp::task_supervisor::spawn_supervised_forever(
            "restart-prep-notice",
            Duration::from_secs(5),
            Duration::from_secs(60),
            Duration::from_secs(120),
            run_loop,
        );
    });
    info!("restart_prep: notice loop started");
}

async fn run_loop() {
    // The boot step (inside `coord_drain_state::boot_read`) settles a
    // rebuilt runner's record first; `tick_live` also skips until it has.
    crate::coord_drain_state::await_boot_read(Duration::from_secs(120)).await;
    if !crate::coord_drain_state::boot_read_complete() {
        warn!(
            "restart_prep: the boot drain read has not completed after 120 s — the notice loop \
             runs anyway, so a prepare record is still served (a rebuilt runner's record may not \
             have been moved to `restoring` yet)"
        );
    }
    BOOT_WAIT_OVER.store(true, std::sync::atomic::Ordering::SeqCst);
    let mut ticker = tokio::time::interval(TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        tick_live().await;
    }
}

#[cfg(test)]
#[path = "notice_tests.rs"]
mod tests;
