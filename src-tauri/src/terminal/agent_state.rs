//! Per-terminal agent truth — the impure glue around the pure
//! [`qontinui_runner_lib::agent_truth`] reducer (plan
//! `2026-09-20-terminal-session-state-comes-from-events-not-screen-scraping`,
//! Phases 3 and 4).
//!
//! Each `TerminalSession` owns ONE [`AgentStateSlot`] (it replaced the
//! sideband-only `agent_status_last`). Everything that knows something about
//! the agent in a pane offers it here:
//!
//! - the Claude Code http hooks, via `POST /terminals/agent-event` →
//!   [`ingest`] (`Source::Hook`);
//! - the OSC 9999 sideband, via `agent_status_sideband::dispatch`
//!   (`Source::Sideband`);
//! - the webview's regex detector and screen-stability observer, via the
//!   `offer_agent_observation` Tauri command (`Source::Regex` /
//!   `Source::ScreenStability`).
//!
//! PTY input evidence (`PtyInputSlots`) and the grid-idle observation are fed
//! at verdict-READ time ([`read_session`]), so no extra hot-path work exists.
//! A verdict change is published as the Tauri event [`EVENT_NAME`] plus the WS
//! re-broadcast `terminal-exit` uses, so a headless runner's remote webview
//! sees it too. The periodic sweep ([`publish_all_once`], riding the grid-scan
//! tick) re-publishes changes that come from TIME alone — a freshness TTL
//! lapsing, a human answering a permission prompt, `HookDelivery` turning
//! `Absent` ten seconds after a silent submit.
//!
//! ## Untrusted input
//!
//! The ingest body is projected to the allowlist on parse
//! ([`qontinui_runner_lib::agent_event::project_bytes`]); nothing else of it is
//! kept, and no field content is ever logged — only counters
//! ([`ingest_counters`]).

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tracing::debug;

use qontinui_runner_lib::agent_event::{
    self, AgentEventProjection, CarrierEvidence, DeliveryEvidence, HookDelivery, ProjectionError,
};
use qontinui_runner_lib::agent_truth::{
    AgentState, AgentTruth, GridIdle, InputEvidence, Observation, ObservationKind, ObserveOutcome,
    RegexState, SidebandWord, Source, StateCapabilities, Verdict,
};

use crate::terminal::agent_metrics::{MetricsSlot, SessionMetrics, TerminalAgentMetrics};
use crate::terminal::agent_status_sideband::{
    LimiterDecision, ObservedAgentState, SidebandRateLimiter,
};

/// The Tauri event (and WS channel) a verdict change is published on.
pub const EVENT_NAME: &str = "terminal-agent-state";

/// Hook events closer together than this for one terminal are coalesced
/// latest-wins (a level is a level). Short: a `PermissionRequest` right after
/// `UserPromptSubmit` should not reach the chip seconds late.
const HOOK_LIMITER_INTERVAL: Duration = Duration::from_millis(250);

/// How many distinct Claude `session_id`s a slot remembers for the header-less
/// fallback (`/clear` and `--resume` mint new ones inside one pane).
const MAX_KNOWN_SESSION_IDS: usize = 8;

/// How often the read-only settings inspection for `Shadowed` is repeated.
const SHADOW_RECHECK_MS: u64 = 60_000;

/// How often the installed CLI version is re-probed.
const CLI_VERSION_RECHECK: Duration = Duration::from_secs(60 * 60);

/// Largest settings file the shadow inspection will read.
const MAX_SETTINGS_FILE_BYTES: u64 = 1024 * 1024;

fn now_ms() -> u64 {
    u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or(0)
}

/// Unix millis of a monotonic instant in the past, anchored at `now_ms`.
fn instant_to_unix_ms(at: Instant, now_ms: u64) -> u64 {
    let ago = u64::try_from(at.elapsed().as_millis()).unwrap_or(u64::MAX);
    now_ms.saturating_sub(ago)
}

// ---------------------------------------------------------------------------
// Ingest counters (Phase 4 surfaces them)
// ---------------------------------------------------------------------------

static RECEIVED: AtomicU64 = AtomicU64::new(0);
static ACCEPTED: AtomicU64 = AtomicU64::new(0);
static DROPPED_OVERSIZE: AtomicU64 = AtomicU64::new(0);
static DROPPED_MALFORMED: AtomicU64 = AtomicU64::new(0);
static DROPPED_UNKNOWN_EVENT: AtomicU64 = AtomicU64::new(0);
static DROPPED_NO_TERMINAL: AtomicU64 = AtomicU64::new(0);
static DROPPED_SUBAGENT: AtomicU64 = AtomicU64::new(0);
static COALESCED: AtomicU64 = AtomicU64::new(0);
static DROPPED_NO_STATE: AtomicU64 = AtomicU64::new(0);

/// A snapshot of the `POST /terminals/agent-event` counters since start.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestCounters {
    pub received: u64,
    /// Applied to a terminal's reducer (immediately or by a deferred flush).
    pub accepted: u64,
    pub dropped_oversize: u64,
    /// Not JSON, not an object, or no usable `hook_event_name`.
    pub dropped_malformed: u64,
    pub dropped_unknown_event: u64,
    /// Neither the header nor the body's `session_id` named a live terminal.
    pub dropped_no_terminal: u64,
    /// Carried an `agent_id` (ignored in v1).
    pub dropped_subagent: u64,
    /// Superseded inside the per-terminal limiter window (latest wins).
    pub coalesced: u64,
    /// Asserted no state (`SessionStart: compact`, …) or arrived out of order.
    pub dropped_no_state: u64,
}

impl IngestCounters {
    /// One line, for the config report.
    pub fn summary(&self) -> String {
        format!(
            "received={} accepted={} oversize={} malformed={} unknown_event={} no_terminal={} \
             subagent={} coalesced={} no_state={}",
            self.received,
            self.accepted,
            self.dropped_oversize,
            self.dropped_malformed,
            self.dropped_unknown_event,
            self.dropped_no_terminal,
            self.dropped_subagent,
            self.coalesced,
            self.dropped_no_state,
        )
    }
}

pub fn ingest_counters() -> IngestCounters {
    let r = |c: &AtomicU64| c.load(Ordering::Relaxed);
    IngestCounters {
        received: r(&RECEIVED),
        accepted: r(&ACCEPTED),
        dropped_oversize: r(&DROPPED_OVERSIZE),
        dropped_malformed: r(&DROPPED_MALFORMED),
        dropped_unknown_event: r(&DROPPED_UNKNOWN_EVENT),
        dropped_no_terminal: r(&DROPPED_NO_TERMINAL),
        dropped_subagent: r(&DROPPED_SUBAGENT),
        coalesced: r(&COALESCED),
        dropped_no_state: r(&DROPPED_NO_STATE),
    }
}

fn bump(c: &AtomicU64) {
    c.fetch_add(1, Ordering::Relaxed);
}

// ---------------------------------------------------------------------------
// The slot
// ---------------------------------------------------------------------------

/// Everything the runner holds about the agent in one pane.
#[derive(Debug)]
pub struct AgentStateSlot {
    truth: AgentTruth,
    hook_limiter: SidebandRateLimiter<Observation>,
    session_ids: VecDeque<String>,
    first_event_at_ms: Option<u64>,
    last_event_at_ms: Option<u64>,
    last_user_prompt_submit_at_ms: Option<u64>,
    beacon_settings_delivered: Option<bool>,
    /// `(checked_at_ms, finding)`.
    shadow: Option<(u64, Option<String>)>,
    /// What was last published, for change detection.
    published: Option<(Verdict, HookDelivery)>,
    /// Context usage and headroom (plan Phase 6).
    metrics: MetricsSlot,
}

impl AgentStateSlot {
    pub fn new(caps: StateCapabilities) -> Self {
        Self {
            truth: AgentTruth::new(caps),
            hook_limiter: SidebandRateLimiter::with_interval(HOOK_LIMITER_INTERVAL),
            session_ids: VecDeque::new(),
            first_event_at_ms: None,
            last_event_at_ms: None,
            last_user_prompt_submit_at_ms: None,
            beacon_settings_delivered: None,
            shadow: None,
            published: None,
            metrics: MetricsSlot::default(),
        }
    }

    pub fn metrics(&self) -> &MetricsSlot {
        &self.metrics
    }

    pub fn metrics_mut(&mut self) -> &mut MetricsSlot {
        &mut self.metrics
    }

    /// The reducer's current state, for consumers that gate on a turn
    /// boundary (the headroom trigger).
    pub fn current_state(&self, now_ms: u64) -> AgentState {
        self.truth.verdict(now_ms).state
    }

    /// A pane's slot. Claude is the only provider with an event source today,
    /// so its capability matrix is the default (declared by its adapter).
    pub fn for_terminal() -> Self {
        Self::new(crate::session::provider_adapter::adapter_for("claude").state_capabilities())
    }

    /// Offer one observation to the reducer.
    pub fn offer(&mut self, obs: &Observation, now_ms: u64) -> ObserveOutcome {
        self.truth.observe(obs, now_ms)
    }

    /// Record that a hook event ARRIVED (delivery evidence), whatever the
    /// reducer then does with it.
    pub fn note_hook_event(&mut self, p: &AgentEventProjection, now_ms: u64) {
        self.first_event_at_ms.get_or_insert(now_ms);
        self.last_event_at_ms = Some(now_ms);
        if p.hook_event_name == "UserPromptSubmit" {
            self.last_user_prompt_submit_at_ms = Some(now_ms);
        }
        if let Some(sid) = &p.session_id {
            if !self.session_ids.iter().any(|s| s == sid) {
                if self.session_ids.len() == MAX_KNOWN_SESSION_IDS {
                    self.session_ids.pop_front();
                }
                self.session_ids.push_back(sid.clone());
            }
        }
    }

    pub fn knows_session_id(&self, sid: &str) -> bool {
        self.session_ids.iter().any(|s| s == sid)
    }

    /// The identity shim's beacon for this pane.
    pub fn note_beacon(&mut self, settings_delivered: bool) {
        self.beacon_settings_delivered = Some(settings_delivered);
    }

    /// The sideband's own last report, in the shape wind-down eligibility has
    /// always read (`working | waiting_human | blocked | finished`). Kept until
    /// plan Phase 9 moves wind-down onto the verdict.
    pub fn sideband_view(&self) -> Option<ObservedAgentState> {
        let (state, at) = self.truth.last_reported(Source::Sideband)?;
        let word = match state {
            AgentState::Working => "working",
            AgentState::NeedsYou { .. } => "waiting_human",
            AgentState::Failed { .. } => "blocked",
            _ => "finished",
        };
        Some(ObservedAgentState {
            state: word.to_string(),
            set_at_ms: i64::try_from(at).unwrap_or(i64::MAX),
        })
    }

    fn shadow_due(&self, now_ms: u64) -> bool {
        self.shadow
            .as_ref()
            .is_none_or(|(at, _)| now_ms.saturating_sub(*at) >= SHADOW_RECHECK_MS)
    }

    fn last_seen_ages(&self, now_ms: u64) -> LastSeenAges {
        let age = |s: Source| self.truth.last_seen_ms(s).map(|t| now_ms.saturating_sub(t));
        LastSeenAges {
            hook: age(Source::Hook),
            sideband: age(Source::Sideband),
            statusline: age(Source::Statusline),
            transcript: age(Source::Transcript),
            screen_stability: age(Source::ScreenStability),
            regex: age(Source::Regex),
        }
    }

    /// Feed the read-time context and compute (verdict, delivery).
    fn read(&mut self, ctx: &ReadContext, now_ms: u64) -> (Verdict, HookDelivery) {
        self.truth.observe_input(ctx.input, now_ms);
        if let Some(grid) = ctx.grid {
            self.truth.observe_grid(grid, now_ms);
        }
        let verdict = self.truth.verdict(now_ms);
        let evidence = DeliveryEvidence {
            carrier: Some(ctx.carrier),
            beacon_settings_delivered: self.beacon_settings_delivered,
            first_event_at_ms: self.first_event_at_ms,
            last_event_at_ms: self.last_event_at_ms,
            last_user_prompt_submit_at_ms: self.last_user_prompt_submit_at_ms,
            last_runner_submit_at_ms: ctx.runner_submit_ms,
            shadowed_by: self.shadow.as_ref().and_then(|(_, f)| f.clone()),
            cli_version: ctx.cli_version.clone(),
        };
        (verdict, agent_event::hook_delivery(&evidence, now_ms))
    }

    /// True (and remembered) when `(v, d)` differs from what was last
    /// published.
    fn take_if_changed(&mut self, v: Verdict, d: &HookDelivery) -> bool {
        if self
            .published
            .as_ref()
            .is_some_and(|(pv, pd)| *pv == v && pd == d)
        {
            return false;
        }
        self.published = Some((v, d.clone()));
        true
    }
}

/// Per-source "how long since this source last reported", in millis.
/// `None` = never reported.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct LastSeenAges {
    pub hook: Option<u64>,
    pub sideband: Option<u64>,
    pub statusline: Option<u64>,
    pub transcript: Option<u64>,
    pub screen_stability: Option<u64>,
    pub regex: Option<u64>,
}

/// The `terminal-agent-state` event payload.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentStateEvent {
    pub terminal_id: String,
    pub verdict: Verdict,
    pub hook_delivery: HookDelivery,
}

/// One row of `get_terminal_agent_states` / `GET /terminals/agent-state`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalAgentState {
    pub terminal_id: String,
    pub verdict: Verdict,
    pub hook_delivery: HookDelivery,
    pub last_seen_age_ms: LastSeenAges,
    /// Context usage and account headroom (plan Phase 6).
    pub metrics: SessionMetrics,
}

/// What a read feeds the slot from outside it.
#[derive(Debug, Clone)]
struct ReadContext {
    input: InputEvidence,
    runner_submit_ms: Option<u64>,
    grid: Option<GridIdle>,
    carrier: CarrierEvidence,
    cli_version: Option<String>,
}

// ---------------------------------------------------------------------------
// Offering observations from the webview
// ---------------------------------------------------------------------------

/// Parse an `offer_agent_observation` into an observation.
///
/// `source` is `"regex"` (with `state` ∈ `working | approval_shaped |
/// question_shaped | completed | error | idle`) or `"screen_stability"` (with
/// `busy`). Anything else is an error — the webview may offer only the two
/// LOWEST-ranked sources, so it can never forge an authoritative state.
pub fn webview_observation(
    source: &str,
    state: Option<&str>,
    busy: Option<bool>,
    now_ms: u64,
) -> Result<Observation, String> {
    let kind = match source {
        "regex" => {
            let r = match state.unwrap_or("") {
                "working" => RegexState::Working,
                "approval_shaped" => RegexState::ApprovalShaped,
                "question_shaped" => RegexState::QuestionShaped,
                "completed" => RegexState::Completed,
                "error" => RegexState::Error,
                "idle" => RegexState::Idle,
                other => return Err(format!("unknown regex state {other:?}")),
            };
            ObservationKind::Regex(r)
        }
        "screen_stability" => ObservationKind::ScreenStability {
            busy: busy.ok_or("screen_stability requires `busy`")?,
        },
        other => {
            return Err(format!(
                "source {other:?} may not be offered by the webview"
            ))
        }
    };
    Ok(Observation::new(kind, now_ms))
}

/// The sideband word as a reducer observation.
pub fn sideband_observation(word: &str, now_ms: u64) -> Option<Observation> {
    let word = SidebandWord::from_wire(word)?;
    Some(Observation::new(ObservationKind::Sideband(word), now_ms))
}

// ---------------------------------------------------------------------------
// Hook ingest
// ---------------------------------------------------------------------------

/// Where the ingest finds a terminal's slot. Implemented by `TerminalManager`;
/// tests use a map.
pub trait SlotDirectory {
    fn by_terminal(&self, terminal_id: &str) -> Option<Arc<Mutex<AgentStateSlot>>>;
    /// The live terminal whose pane runs Claude session `sid` (its pinned
    /// `--session-id`, or one a header-bearing event already taught its slot).
    fn by_session_id(&self, sid: &str) -> Option<(String, Arc<Mutex<AgentStateSlot>>)>;
}

/// What [`ingest`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngestOutcome {
    Dropped(&'static str),
    /// Applied now — the caller publishes.
    Applied {
        terminal_id: String,
    },
    /// Held; the caller owes [`flush_deferred`] after `delay`.
    Deferred {
        terminal_id: String,
        delay: Duration,
    },
    /// Held behind an already-scheduled flush.
    Coalesced {
        terminal_id: String,
    },
}

/// Is `raw` a plausible terminal id? Bounded identifier charset.
fn valid_terminal_id(raw: &str) -> bool {
    !raw.is_empty()
        && raw.len() <= 128
        && raw
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Handle one `POST /terminals/agent-event` body. Never logs field content;
/// every outcome is counted.
///
/// A top-level `StopFailure{error: rate_limit}` is ALSO an immediate,
/// non-regex hint into the reactive account-migration path (plan Phase 8).
/// That path is probe-confirmed, so transient throttling with a healthy probe
/// migrates nothing; the hint shares the grid scanner's per-terminal debounce.
pub fn ingest(
    dir: &impl SlotDirectory,
    header_terminal: Option<&str>,
    body: &[u8],
    now_ms: u64,
) -> IngestOutcome {
    let out = ingest_at(dir, header_terminal, body, now_ms, Instant::now());
    let resolved = match &out {
        IngestOutcome::Applied { terminal_id }
        | IngestOutcome::Deferred { terminal_id, .. }
        | IngestOutcome::Coalesced { terminal_id } => Some(terminal_id),
        IngestOutcome::Dropped(_) => None,
    };
    if let Some(terminal_id) = resolved {
        if agent_event::project_bytes(body).is_ok_and(|p| stop_failure_is_rate_limit(&p)) {
            super::usage_limit::fire_event_hint(terminal_id.clone(), RATE_LIMIT_HINT);
        }
    }
    out
}

/// The label the `StopFailure` rate-limit hint carries into the migration
/// path's logs.
pub const RATE_LIMIT_HINT: &str = "hook:StopFailure{error:rate_limit}";

/// Is this projection a top-level `StopFailure` carrying `rate_limit`?
/// (`error` is read first, `error_type` as the fallback — the projection
/// already folded them.) A subagent's is ignored, as for state.
pub fn stop_failure_is_rate_limit(p: &AgentEventProjection) -> bool {
    p.hook_event_name == "StopFailure" && !p.is_subagent && p.error.as_deref() == Some("rate_limit")
}

/// [`ingest`] with the limiter's monotonic clock injected (the test seam).
fn ingest_at(
    dir: &impl SlotDirectory,
    header_terminal: Option<&str>,
    body: &[u8],
    now_ms: u64,
    now_instant: Instant,
) -> IngestOutcome {
    bump(&RECEIVED);
    let projection = match agent_event::project_bytes(body) {
        Ok(p) => p,
        Err(e) => {
            match e {
                ProjectionError::Oversize => bump(&DROPPED_OVERSIZE),
                ProjectionError::UnknownEvent => bump(&DROPPED_UNKNOWN_EVENT),
                ProjectionError::NotJson
                | ProjectionError::NotObject
                | ProjectionError::NoEventName => bump(&DROPPED_MALFORMED),
            }
            debug!(reason = e.as_str(), "agent-event: dropped");
            return IngestOutcome::Dropped(e.as_str());
        }
    };

    let by_header = header_terminal
        .map(str::trim)
        .filter(|h| valid_terminal_id(h))
        .and_then(|h| dir.by_terminal(h).map(|slot| (h.to_string(), slot)));
    let resolved = by_header.or_else(|| {
        projection
            .session_id
            .as_deref()
            .and_then(|sid| dir.by_session_id(sid))
    });
    let Some((terminal_id, slot)) = resolved else {
        bump(&DROPPED_NO_TERMINAL);
        debug!(
            event = projection.hook_event_name,
            "agent-event: no terminal"
        );
        return IngestOutcome::Dropped("no_terminal");
    };

    let Ok(mut slot) = slot.lock() else {
        bump(&DROPPED_NO_TERMINAL);
        return IngestOutcome::Dropped("slot_poisoned");
    };
    // A subagent's event still PROVES delivery; it just claims no state (v1).
    slot.note_hook_event(&projection, now_ms);
    if projection.is_subagent {
        bump(&DROPPED_SUBAGENT);
        return IngestOutcome::Dropped("subagent");
    }
    let obs = Observation::hook(projection.to_hook_event(), now_ms);
    match slot.hook_limiter.offer(obs, now_instant) {
        LimiterDecision::EmitNow(obs) => {
            apply(&mut slot, &obs, now_ms);
            IngestOutcome::Applied { terminal_id }
        }
        LimiterDecision::Defer { delay } => IngestOutcome::Deferred { terminal_id, delay },
        LimiterDecision::Held => {
            bump(&COALESCED);
            IngestOutcome::Coalesced { terminal_id }
        }
    }
}

fn apply(slot: &mut AgentStateSlot, obs: &Observation, now_ms: u64) {
    match slot.offer(obs, now_ms) {
        ObserveOutcome::Accepted => bump(&ACCEPTED),
        ObserveOutcome::DroppedSubagent => bump(&DROPPED_SUBAGENT),
        ObserveOutcome::DroppedNoState | ObserveOutcome::DroppedOutOfOrder => {
            bump(&DROPPED_NO_STATE)
        }
    }
}

/// Redeem a deferred hook flush. `true` when something was applied (the
/// caller publishes).
pub fn flush_deferred(dir: &impl SlotDirectory, terminal_id: &str, now_ms: u64) -> bool {
    let Some(slot) = dir.by_terminal(terminal_id) else {
        return false;
    };
    let Ok(mut slot) = slot.lock() else {
        return false;
    };
    let Some(obs) = slot.hook_limiter.flush(Instant::now()) else {
        return false;
    };
    apply(&mut slot, &obs, now_ms);
    true
}

impl SlotDirectory for crate::terminal::TerminalManager {
    fn by_terminal(&self, terminal_id: &str) -> Option<Arc<Mutex<AgentStateSlot>>> {
        self.get(terminal_id).map(|s| s.agent_state_slot().clone())
    }

    fn by_session_id(&self, sid: &str) -> Option<(String, Arc<Mutex<AgentStateSlot>>)> {
        self.sessions_snapshot().into_iter().find_map(|(id, s)| {
            let slot = s.agent_state_slot().clone();
            let knows = s.pinned_session_id() == sid
                || slot
                    .lock()
                    .map(|g| g.knows_session_id(sid))
                    .unwrap_or(false);
            knows.then_some((id, slot))
        })
    }
}

// ---------------------------------------------------------------------------
// Reading and publishing
// ---------------------------------------------------------------------------

/// Compute one pane's state. `with_grid` also takes a grid-idle observation
/// (a full grid read — only on an explicit read, never on the sweep).
pub fn read_session(
    session: &crate::terminal::session::TerminalSession,
    terminal_id: &str,
    with_grid: bool,
) -> TerminalAgentState {
    let (state, _) = compute(session, with_grid);
    let TerminalAgentStateParts {
        verdict,
        delivery,
        ages,
    } = state;
    let (metrics, _) = crate::terminal::agent_metrics::compute(session, terminal_id, false);
    TerminalAgentState {
        terminal_id: terminal_id.to_string(),
        verdict,
        hook_delivery: delivery,
        last_seen_age_ms: ages,
        metrics: metrics.metrics,
    }
}

struct TerminalAgentStateParts {
    verdict: Verdict,
    delivery: HookDelivery,
    ages: LastSeenAges,
}

/// Compute (and return whether it changed since the last publish).
fn compute(
    session: &crate::terminal::session::TerminalSession,
    with_grid: bool,
) -> (TerminalAgentStateParts, bool) {
    let now = now_ms();
    let slots = session.last_input();
    let input = InputEvidence {
        // Any human or runner input, control responses excluded — the
        // "human answered" signal of the reducer's rule 5.
        last_submit_ms: slots.latest().map(|o| instant_to_unix_ms(o.at, now)),
    };
    let runner_submit_ms = slots
        .last_submit
        .as_ref()
        .map(|o| instant_to_unix_ms(o.at, now));
    let grid = with_grid.then(|| match session.observe_grid_idle() {
        qontinui_runner_lib::wind_down::GridIdle::Idle { since_ms } => GridIdle::Idle {
            since_ms: u64::try_from(since_ms).unwrap_or(0),
        },
        qontinui_runner_lib::wind_down::GridIdle::Busy => GridIdle::Busy,
        qontinui_runner_lib::wind_down::GridIdle::Unknown => GridIdle::Unknown,
    });
    let ctx = ReadContext {
        input,
        runner_submit_ms,
        grid,
        carrier: crate::session::claude_hook::agent_event_carrier_evidence(),
        cli_version: cli_version(),
    };

    // The shadow inspection is file IO: done OUTSIDE the slot lock.
    let due = session
        .agent_state_slot()
        .lock()
        .map(|s| s.shadow_due(now))
        .unwrap_or(false);
    let shadow = due.then(|| settings_shadow(session.working_dir()));

    let slot = session.agent_state_slot();
    let mut guard = slot.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(finding) = shadow {
        guard.shadow = Some((now, finding));
    }
    let (verdict, delivery) = guard.read(&ctx, now);
    let changed = guard.take_if_changed(verdict, &delivery);
    let ages = guard.last_seen_ages(now);
    (
        TerminalAgentStateParts {
            verdict,
            delivery,
            ages,
        },
        changed,
    )
}

/// Publish `session`'s verdict if it changed since the last publish.
///
/// Also publishes the pane's metrics when THEY changed, and offers the
/// verdict + headroom to the proactive-migration trigger (a zero-cost no-op
/// while `QONTINUI_PROACTIVE_MIGRATION` is off).
pub fn publish_session(session: &crate::terminal::session::TerminalSession, terminal_id: &str) {
    let (parts, changed) = compute(session, false);
    let (metrics, metrics_changed) =
        crate::terminal::agent_metrics::compute(session, terminal_id, true);
    crate::terminal::headroom::on_tick(
        terminal_id,
        &parts.verdict.state,
        metrics.account.as_deref(),
        metrics.headroom.as_ref(),
        now_ms(),
    );
    if !changed && !metrics_changed {
        return;
    }
    let Some(app) = crate::tauri_app_handle::current() else {
        return;
    };
    if changed {
        emit(
            &app,
            &AgentStateEvent {
                terminal_id: terminal_id.to_string(),
                verdict: parts.verdict,
                hook_delivery: parts.delivery,
            },
        );
    }
    if metrics_changed {
        crate::terminal::agent_metrics::emit(
            &app,
            &TerminalAgentMetrics {
                terminal_id: terminal_id.to_string(),
                metrics: metrics.metrics,
            },
        );
    }
}

/// Publish one terminal by id, if it is live.
pub fn publish_terminal(terminal_id: &str) {
    use tauri::Manager;
    let Some(app) = crate::tauri_app_handle::current() else {
        return;
    };
    let Some(tm) = app.try_state::<Arc<crate::terminal::TerminalManager>>() else {
        return;
    };
    if let Some(session) = tm.get(terminal_id) {
        publish_session(&session, terminal_id);
    }
}

fn emit(app: &tauri::AppHandle, event: &AgentStateEvent) {
    use tauri::Emitter;
    if let Err(e) = app.emit(EVENT_NAME, event) {
        tracing::warn!(terminal_id = %event.terminal_id, error = %e, "agent-state: emit failed");
    }
    if let Ok(payload) = serde_json::to_value(event) {
        crate::event_system::broadcast_ws_notification(app, EVENT_NAME, &payload);
    }
}

/// Re-publish every live pane whose verdict changed with time alone. Rides the
/// grid-scan tick (`auto_response::spawn_grid_scan_loop`); cheap — no grid
/// read, one lock per pane, and a settings stat at most once a minute.
pub fn publish_all_once() {
    use tauri::Manager;
    refresh_cli_version_if_due();
    let Some(app) = crate::tauri_app_handle::current() else {
        return;
    };
    let Some(tm) = app.try_state::<Arc<crate::terminal::TerminalManager>>() else {
        return;
    };
    let sessions = tm.sessions_snapshot();
    crate::terminal::headroom::retain_live(sessions.iter().map(|(id, _)| id.as_str()));
    for (id, session) in sessions {
        publish_session(&session, &id);
    }
}

/// Every live pane's state, for `get_terminal_agent_states` and
/// `GET /terminals/agent-state`.
pub fn read_all(tm: &crate::terminal::TerminalManager) -> Vec<TerminalAgentState> {
    tm.sessions_snapshot()
        .into_iter()
        .map(|(id, session)| read_session(&session, &id, true))
        .collect()
}

/// The identity shim's beacon reached `/control/shim-beacon` for `terminal_id`.
pub fn note_shim_beacon(terminal_id: &str, tool: &str, detail: &str) {
    use tauri::Manager;
    if tool != "claude" || !valid_terminal_id(terminal_id) {
        return;
    }
    let delivered = if detail.contains("settings=true") {
        true
    } else if detail.contains("settings=false") {
        false
    } else {
        return;
    };
    let Some(app) = crate::tauri_app_handle::current() else {
        return;
    };
    let Some(tm) = app.try_state::<Arc<crate::terminal::TerminalManager>>() else {
        return;
    };
    if let Some(session) = tm.get(terminal_id) {
        if let Ok(mut slot) = session.agent_state_slot().lock() {
            slot.note_beacon(delivered);
        }
    }
}

// ---------------------------------------------------------------------------
// Phase 4 evidence: settings shadowing (read-only) and the CLI version
// ---------------------------------------------------------------------------

/// Which key in a parsed settings file disables the runner's hooks.
fn shadow_key(v: &serde_json::Value) -> Option<&'static str> {
    if v.get("disableAllHooks").and_then(|b| b.as_bool()) == Some(true) {
        Some("disableAllHooks")
    } else if v.get("allowManagedHooksOnly").and_then(|b| b.as_bool()) == Some(true) {
        Some("allowManagedHooksOnly")
    } else {
        None
    }
}

/// The settings files Claude Code reads for a pane in `cwd`, labelled. The
/// user file follows the RUNNER's `CLAUDE_CONFIG_DIR` (or `~/.claude`) — a
/// pane pinned to another account dir is not seen, a documented limitation.
fn settings_files(cwd: &str) -> Vec<(&'static str, PathBuf)> {
    let mut files = Vec::new();
    let user_dir = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".claude")));
    if let Some(dir) = user_dir {
        files.push(("user settings", dir.join("settings.json")));
    }
    if !cwd.is_empty() {
        let project = PathBuf::from(cwd).join(".claude");
        files.push(("project settings", project.join("settings.json")));
        files.push((
            "local project settings",
            project.join("settings.local.json"),
        ));
    }
    let managed = if cfg!(target_os = "windows") {
        PathBuf::from(r"C:\Program Files\ClaudeCode\managed-settings.json")
    } else if cfg!(target_os = "macos") {
        PathBuf::from("/Library/Application Support/ClaudeCode/managed-settings.json")
    } else {
        PathBuf::from("/etc/claude-code/managed-settings.json")
    };
    files.push(("managed settings", managed));
    files
}

/// Read-only: does any settings file this pane's CLI reads disable hooks?
/// NEVER writes any file.
fn settings_shadow(cwd: &str) -> Option<String> {
    settings_files(cwd).into_iter().find_map(|(label, path)| {
        let len = std::fs::metadata(&path).ok()?.len();
        if len > MAX_SETTINGS_FILE_BYTES {
            return None;
        }
        let text = std::fs::read_to_string(&path).ok()?;
        let v: serde_json::Value = serde_json::from_str(&text).ok()?;
        shadow_key(&v).map(|key| format!("{key} in {label}"))
    })
}

#[derive(Default)]
struct CliVersionState {
    value: Option<String>,
    probed_at: Option<Instant>,
    in_flight: bool,
}

static CLI_VERSION: Mutex<CliVersionState> = Mutex::new(CliVersionState {
    value: None,
    probed_at: None,
    in_flight: false,
});

/// The installed CLI version (`claude --version`), when a probe answered.
pub fn cli_version() -> Option<String> {
    CLI_VERSION.lock().ok().and_then(|s| s.value.clone())
}

/// The leading `N.N.N` of `claude --version` output.
fn parse_cli_version(stdout: &str) -> Option<String> {
    let token = stdout.split_whitespace().next()?;
    let ok = !token.is_empty()
        && token.chars().all(|c| c.is_ascii_digit() || c == '.')
        && token.contains('.');
    ok.then(|| token.to_string())
}

/// Probe `claude --version` on a detached thread when never probed or an hour
/// stale. Never blocks the caller; a failed probe leaves the version UNKNOWN.
fn refresh_cli_version_if_due() {
    {
        let Ok(mut s) = CLI_VERSION.lock() else {
            return;
        };
        let due = !s.in_flight
            && s.probed_at
                .is_none_or(|t| t.elapsed() >= CLI_VERSION_RECHECK);
        if !due {
            return;
        }
        s.in_flight = true;
    }
    let spawned = std::thread::Builder::new()
        .name("agent-state-cli-version".into())
        .spawn(|| {
            let value = crate::process_helpers::no_window("claude")
                .arg("--version")
                .output()
                .ok()
                .filter(|o| o.status.success())
                .and_then(|o| parse_cli_version(&String::from_utf8_lossy(&o.stdout)));
            if let Ok(mut s) = CLI_VERSION.lock() {
                s.value = value.or(s.value.take());
                s.probed_at = Some(Instant::now());
                s.in_flight = false;
            }
        });
    if spawned.is_err() {
        if let Ok(mut s) = CLI_VERSION.lock() {
            s.in_flight = false;
            s.probed_at = Some(Instant::now());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const NOW: u64 = 1_900_000_000_000;

    #[derive(Default)]
    struct Dir {
        slots: HashMap<String, Arc<Mutex<AgentStateSlot>>>,
        pinned: HashMap<String, String>,
    }

    impl Dir {
        fn with(ids: &[(&str, &str)]) -> Self {
            let mut d = Dir::default();
            for (tid, pinned) in ids {
                d.slots.insert(
                    tid.to_string(),
                    Arc::new(Mutex::new(AgentStateSlot::new(StateCapabilities::claude()))),
                );
                d.pinned.insert(pinned.to_string(), tid.to_string());
            }
            d
        }
        fn verdict(&self, tid: &str) -> Verdict {
            self.slots[tid].lock().unwrap().truth.verdict(NOW + 1)
        }
    }

    impl SlotDirectory for Dir {
        fn by_terminal(&self, id: &str) -> Option<Arc<Mutex<AgentStateSlot>>> {
            self.slots.get(id).cloned()
        }
        fn by_session_id(&self, sid: &str) -> Option<(String, Arc<Mutex<AgentStateSlot>>)> {
            if let Some(tid) = self.pinned.get(sid) {
                return Some((tid.clone(), self.slots[tid].clone()));
            }
            self.slots.iter().find_map(|(id, s)| {
                s.lock()
                    .unwrap()
                    .knows_session_id(sid)
                    .then(|| (id.clone(), s.clone()))
            })
        }
    }

    fn body(v: serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&v).unwrap()
    }

    /// A monotonic instant `k` seconds after a fixed base — far enough apart
    /// that the hook limiter never coalesces, whatever the box's load.
    fn at(k: u64) -> Instant {
        static BASE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        *BASE.get_or_init(Instant::now) + Duration::from_secs(k)
    }

    fn ingest(
        dir: &impl SlotDirectory,
        header: Option<&str>,
        body: &[u8],
        now_ms: u64,
    ) -> IngestOutcome {
        static TICK: AtomicU64 = AtomicU64::new(0);
        ingest_at(
            dir,
            header,
            body,
            now_ms,
            at(TICK.fetch_add(1, Ordering::Relaxed)),
        )
    }

    #[test]
    fn agent_event_route_oversize_body_is_dropped_whole_and_counted() {
        let dir = Dir::with(&[("t1", "sid-1")]);
        let before = ingest_counters().dropped_oversize;
        let mut v = serde_json::json!({"hook_event_name": "UserPromptSubmit"});
        v["tool_input"] = serde_json::Value::String("x".repeat(agent_event::MAX_BODY_BYTES));
        let out = ingest(&dir, Some("t1"), &body(v), NOW);
        assert_eq!(out, IngestOutcome::Dropped("oversize"));
        assert!(ingest_counters().dropped_oversize > before);
        assert!(
            dir.verdict("t1").is_unknown(),
            "nothing reached the reducer"
        );
    }

    #[test]
    fn agent_event_route_forbidden_key_is_ignored() {
        let dir = Dir::with(&[("t1", "sid-1")]);
        let out = ingest(
            &dir,
            Some("t1"),
            &body(serde_json::json!({
                "hook_event_name": "PermissionRequest",
                "tool_name": "Bash",
                "tool_input": {"command": "cat ~/.ssh/id_rsa"},
                "prompt": "SECRET",
                // A forged authority key the projection must not read.
                "state": "working",
            })),
            NOW,
        );
        assert_eq!(
            out,
            IngestOutcome::Applied {
                terminal_id: "t1".into()
            }
        );
        let v = dir.verdict("t1");
        assert!(v.is_authoritative_permission_ask());
        let dbg = format!("{:?}", dir.slots["t1"].lock().unwrap());
        assert!(!dbg.contains("SECRET") && !dbg.contains("id_rsa"), "{dbg}");
    }

    #[test]
    fn agent_event_route_unknown_event_is_counted() {
        let dir = Dir::with(&[("t1", "sid-1")]);
        let before = ingest_counters().dropped_unknown_event;
        let out = ingest(
            &dir,
            Some("t1"),
            &body(serde_json::json!({"hook_event_name": "PreToolUse"})),
            NOW,
        );
        assert_eq!(out, IngestOutcome::Dropped("unknown_event"));
        assert!(ingest_counters().dropped_unknown_event > before);
        assert!(dir.verdict("t1").is_unknown());
    }

    #[test]
    fn agent_event_route_headerless_falls_back_to_session_id() {
        let dir = Dir::with(&[("t1", "sid-pinned"), ("t2", "sid-other")]);
        // Pinned id, no header.
        let out = ingest(
            &dir,
            None,
            &body(
                serde_json::json!({"hook_event_name": "UserPromptSubmit", "session_id": "sid-other"}),
            ),
            NOW,
        );
        assert_eq!(
            out,
            IngestOutcome::Applied {
                terminal_id: "t2".into()
            }
        );
        // An EMPTY header (the CLI did not interpolate) falls back too.
        let out = ingest(
            &dir,
            Some(""),
            &body(
                serde_json::json!({"hook_event_name": "UserPromptSubmit", "session_id": "sid-pinned"}),
            ),
            NOW,
        );
        assert_eq!(
            out,
            IngestOutcome::Applied {
                terminal_id: "t1".into()
            }
        );
        // A session id learned from a header-bearing event (after /clear).
        let _ = ingest(
            &dir,
            Some("t2"),
            &body(
                serde_json::json!({"hook_event_name": "SessionStart", "source": "clear", "session_id": "sid-new"}),
            ),
            NOW + 1_000,
        );
        let out = ingest(
            &dir,
            Some("no-such-terminal"),
            &body(serde_json::json!({"hook_event_name": "Stop", "session_id": "sid-new"})),
            NOW + 2_000,
        );
        assert_eq!(
            out,
            IngestOutcome::Applied {
                terminal_id: "t2".into()
            }
        );
        // Nothing resolves ⇒ dropped and counted.
        let before = ingest_counters().dropped_no_terminal;
        let out = ingest(
            &dir,
            None,
            &body(serde_json::json!({"hook_event_name": "Stop", "session_id": "nobody"})),
            NOW,
        );
        assert_eq!(out, IngestOutcome::Dropped("no_terminal"));
        assert!(ingest_counters().dropped_no_terminal > before);
    }

    #[test]
    fn agent_event_route_burst_is_coalesced_latest_wins() {
        let dir = Dir::with(&[("t1", "sid-1")]);
        // One instant for the whole burst: inside one limiter window.
        let burst = at(10_000);
        let send = |name: &str| {
            ingest_at(
                &dir,
                Some("t1"),
                &body(serde_json::json!({"hook_event_name": name, "tool_name": "Bash"})),
                NOW,
                burst,
            )
        };
        assert!(matches!(
            send("UserPromptSubmit"),
            IngestOutcome::Applied { .. }
        ));
        assert!(matches!(
            send("PermissionRequest"),
            IngestOutcome::Deferred { .. }
        ));
        assert!(matches!(send("Stop"), IngestOutcome::Coalesced { .. }));
        assert!(flush_deferred(&dir, "t1", NOW + 300));
        assert_eq!(dir.verdict("t1").state, AgentState::TurnEnded);
    }

    #[test]
    fn agent_event_subagent_event_proves_delivery_but_sets_no_state() {
        let dir = Dir::with(&[("t1", "sid-1")]);
        let out = ingest(
            &dir,
            Some("t1"),
            &body(serde_json::json!({"hook_event_name": "Stop", "agent_id": "sub-1"})),
            NOW,
        );
        assert_eq!(out, IngestOutcome::Dropped("subagent"));
        let slot = dir.slots["t1"].lock().unwrap();
        assert!(slot.first_event_at_ms.is_some());
        assert!(slot.truth.verdict(NOW + 1).is_unknown());
    }

    #[test]
    fn agent_event_webview_may_offer_only_the_two_lowest_sources() {
        let o = webview_observation("regex", Some("approval_shaped"), None, NOW).unwrap();
        assert_eq!(o.source(), Source::Regex);
        let o = webview_observation("screen_stability", None, Some(true), NOW).unwrap();
        assert_eq!(o.source(), Source::ScreenStability);
        for bad in ["hook", "sideband", "statusline", "transcript", ""] {
            assert!(webview_observation(bad, Some("working"), Some(true), NOW).is_err());
        }
        assert!(webview_observation("regex", Some("needs-input"), None, NOW).is_err());
        assert!(webview_observation("screen_stability", None, None, NOW).is_err());
    }

    #[test]
    fn agent_event_sideband_view_keeps_the_wind_down_vocabulary() {
        let mut slot = AgentStateSlot::new(StateCapabilities::claude());
        assert_eq!(slot.sideband_view(), None);
        for (word, want) in [
            ("working", "working"),
            ("waiting_human", "waiting_human"),
            ("stalled", "blocked"),
            ("finished", "finished"),
        ] {
            let at = NOW + slot.session_ids.len() as u64;
            slot.offer(&sideband_observation(word, at).unwrap(), at);
            let view = slot.sideband_view().unwrap();
            assert_eq!(view.state, want);
            assert_eq!(view.is_working(), want == "working");
        }
        assert!(sideband_observation("banana", NOW).is_none());
    }

    #[test]
    fn agent_event_publish_change_detection() {
        let mut slot = AgentStateSlot::new(StateCapabilities::claude());
        let d = HookDelivery::Unknown { evidence: None };
        assert!(slot.take_if_changed(Verdict::UNKNOWN, &d));
        assert!(!slot.take_if_changed(Verdict::UNKNOWN, &d));
        assert!(slot.take_if_changed(Verdict::UNKNOWN, &HookDelivery::Installed));
    }

    #[test]
    fn agent_event_wire_payload_is_camel_case() {
        let ev = AgentStateEvent {
            terminal_id: "t1".into(),
            verdict: Verdict::UNKNOWN,
            hook_delivery: HookDelivery::Installed,
        };
        let v = serde_json::to_value(&ev).unwrap();
        assert_eq!(v["terminalId"], "t1");
        assert_eq!(v["verdict"]["state"]["name"], "unknown");
        assert_eq!(
            v["hookDelivery"],
            serde_json::json!({"status": "installed"})
        );
        let row = TerminalAgentState {
            terminal_id: "t1".into(),
            verdict: Verdict::UNKNOWN,
            hook_delivery: HookDelivery::Installed,
            last_seen_age_ms: LastSeenAges {
                hook: Some(5),
                ..Default::default()
            },
            metrics: SessionMetrics::default(),
        };
        let v = serde_json::to_value(&row).unwrap();
        assert_eq!(v["lastSeenAgeMs"]["hook"], 5);
        assert!(v["lastSeenAgeMs"]["screen_stability"].is_null());
        assert!(v["metrics"]["contextUsedPct"].is_null());
        assert!(v["metrics"]["costUsd"].is_null());
    }

    #[test]
    fn migration_stop_failure_rate_limit_is_an_immediate_hint() {
        let p = |v: serde_json::Value| agent_event::project_bytes(&body(v)).unwrap();
        assert!(stop_failure_is_rate_limit(&p(
            serde_json::json!({"hook_event_name": "StopFailure", "error": "rate_limit"})
        )));
        // The documented `error_type` spelling is the fallback.
        assert!(stop_failure_is_rate_limit(&p(
            serde_json::json!({"hook_event_name": "StopFailure", "error_type": "rate_limit"})
        )));
        // Other failures, other events and subagents are not hints.
        assert!(!stop_failure_is_rate_limit(&p(
            serde_json::json!({"hook_event_name": "StopFailure", "error": "overloaded"})
        )));
        assert!(!stop_failure_is_rate_limit(&p(
            serde_json::json!({"hook_event_name": "Stop", "error": "rate_limit"})
        )));
        assert!(!stop_failure_is_rate_limit(&p(serde_json::json!({
            "hook_event_name": "StopFailure", "error": "rate_limit", "agent_id": "sub-1"
        }))));
    }

    #[test]
    fn hook_delivery_shadow_inspection_is_read_only_and_finds_the_key() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join(".claude");
        std::fs::create_dir_all(&project).unwrap();
        let cwd = tmp.path().to_string_lossy().into_owned();
        // (The user/managed files on this box are whatever they are; only the
        // project finding is asserted, and only when nothing earlier shadows.)
        std::fs::write(
            project.join("settings.local.json"),
            r#"{"disableAllHooks": true}"#,
        )
        .unwrap();
        let before = std::fs::read(project.join("settings.local.json")).unwrap();
        let finding = settings_shadow(&cwd).expect("a shadowing key is found");
        assert!(finding.contains("disableAllHooks") || finding.contains("allowManagedHooksOnly"));
        assert_eq!(
            std::fs::read(project.join("settings.local.json")).unwrap(),
            before
        );
        assert_eq!(
            shadow_key(&serde_json::json!({"disableAllHooks": false})),
            None
        );
        assert_eq!(
            shadow_key(&serde_json::json!({"allowManagedHooksOnly": true})),
            Some("allowManagedHooksOnly")
        );
    }

    #[test]
    fn hook_delivery_cli_version_parse() {
        assert_eq!(
            parse_cli_version("2.1.285 (Claude Code)\n").as_deref(),
            Some("2.1.285")
        );
        assert_eq!(parse_cli_version(""), None);
        assert_eq!(parse_cli_version("error: nope"), None);
    }
}
