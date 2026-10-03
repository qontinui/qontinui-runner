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
//! at verdict-compute time. A verdict change is published as the Tauri event
//! [`EVENT_NAME`] plus the WS re-broadcast `terminal-exit` uses, so a headless
//! runner's remote webview sees it too. Every published change carries a
//! per-terminal, monotonically increasing `seq` (so is every read row), which
//! lets the webview discard a snapshot older than an event it already holds.
//!
//! Only a PUBLISH consumes a change; a READ ([`read_session`], [`read_all`])
//! never does, so a read between a change and its publish cannot swallow it.
//! A human answering a `NeedsYou` is published from the PTY input path
//! ([`on_pty_input`], debounced, off the writer's thread). The periodic sweep
//! ([`publish_all_once`], riding the grid-scan tick) re-publishes changes that
//! come from TIME alone — a freshness TTL lapsing, `HookDelivery` turning
//! `Absent` ten seconds after a silent submit.
//!
//! ## Untrusted input
//!
//! The ingest body is projected to the allowlist on parse
//! ([`qontinui_runner_lib::agent_event::project_bytes`]); nothing else of it is
//! kept, and no field content is ever logged — only counters
//! ([`ingest_counters`]).

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
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

/// A `claude --version` probe that has not answered by then is killed.
const CLI_VERSION_TIMEOUT: Duration = Duration::from_secs(10);

/// PTY input is published at most once per this window per terminal (an
/// answer is a few keystrokes; one publish covers them).
const INPUT_PUBLISH_DEBOUNCE: Duration = Duration::from_millis(50);

/// Largest settings file the shadow inspection will read.
const MAX_SETTINGS_FILE_BYTES: u64 = 1024 * 1024;

/// Wall-clock unix millis — only for comparisons against provider-stamped
/// times (the headroom trigger). The reducer runs on [`now_ms`].
fn wall_now_ms() -> u64 {
    u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or(0)
}

/// The process's one `Instant` ↔ unix-millis anchor, taken on first use.
fn clock_anchor() -> (Instant, u64) {
    static ANCHOR: OnceLock<(Instant, u64)> = OnceLock::new();
    let (base, base_ms) = *ANCHOR.get_or_init(|| (Instant::now(), wall_now_ms()));
    (base, apply_test_anchor_skew(base_ms))
}

#[cfg(test)]
thread_local! {
    /// Test seam: shifts this THREAD's view of the anchor's unix millis, to
    /// model a wall clock that stepped after the anchor was taken.
    static TEST_ANCHOR_SKEW_MS: std::cell::Cell<i64> = const { std::cell::Cell::new(0) };
}

/// Test seam: skew this thread's anchor by `skew_ms` (0 restores it).
#[cfg(test)]
pub(crate) fn set_test_anchor_skew_ms(skew_ms: i64) {
    TEST_ANCHOR_SKEW_MS.with(|c| c.set(skew_ms));
}

#[cfg(test)]
fn apply_test_anchor_skew(base_ms: u64) -> u64 {
    let skew = TEST_ANCHOR_SKEW_MS.with(|c| c.get());
    base_ms.saturating_add_signed(skew)
}

#[cfg(not(test))]
fn apply_test_anchor_skew(base_ms: u64) -> u64 {
    base_ms
}

/// Map a reducer stamp ([`now_ms`] timeline) to wall-clock unix millis, for a
/// consumer that compares it with `chrono::Utc::now()` / `Date.now()`. The
/// reducer's timeline drifts from the wall clock by every wall step since the
/// anchor, so the stamp is carried over as an AGE: `wall_now - (mono_now -
/// mono_ts)` (a future stamp clamps to `wall_now`).
fn mono_to_wall_at(mono_ts: u64, mono_now: u64, wall_now: u64) -> u64 {
    wall_now.saturating_sub(mono_now.saturating_sub(mono_ts))
}

/// The inverse of [`mono_to_wall_at`]: a wall-clock stamp (the grid-idle
/// tracker's) placed on the reducer's timeline by its age.
fn wall_to_mono_at(wall_ts: u64, mono_now: u64, wall_now: u64) -> u64 {
    mono_now.saturating_sub(wall_now.saturating_sub(wall_ts))
}

/// A verdict with every reducer stamp it carries mapped to wall clock — the
/// form that leaves this module (events, reads, rows).
fn verdict_to_wall(v: Verdict, mono_now: u64, wall_now: u64) -> Verdict {
    use qontinui_runner_lib::agent_truth::Disagreement;
    let to_wall = |t: u64| mono_to_wall_at(t, mono_now, wall_now);
    Verdict {
        since_ms: v.since_ms.map(to_wall),
        disagreement: v.disagreement.map(|d| match d {
            Disagreement::QuietWhileWorking { grid_idle_since_ms } => {
                Disagreement::QuietWhileWorking {
                    grid_idle_since_ms: to_wall(grid_idle_since_ms),
                }
            }
            other => other,
        }),
        ..v
    }
}

/// The reducer's clock: unix millis that never step backwards. Derived from
/// the monotonic clock through ONE anchor per process, so a wall-clock step
/// (NTP, a suspend, a manual change) cannot reorder observations, and every
/// stamp — an ingest's, an input slot's, a read's — is on the same timeline.
pub fn now_ms() -> u64 {
    instant_to_unix_ms(Instant::now())
}

/// Unix millis of a monotonic instant, on the [`now_ms`] timeline. The same
/// instant always maps to the same millis (no per-call re-anchoring).
fn instant_to_unix_ms(at: Instant) -> u64 {
    let (base, base_ms) = clock_anchor();
    let ms = |d: Duration| u64::try_from(d.as_millis()).unwrap_or(u64::MAX);
    // FLOOR on both sides of the anchor (an instant 0.5ms before it is -1, not
    // 0), so `at + 1s` always maps exactly 1000ms later — truncating toward
    // the anchor made an instant just before it collide with one just after.
    match at.checked_duration_since(base) {
        Some(after) => base_ms.saturating_add(ms(after)),
        None => {
            let before = base.duration_since(at);
            let ceil = ms(before) + u64::from(before.subsec_nanos() % 1_000_000 != 0);
            base_ms.saturating_sub(ceil)
        }
    }
}

// ---------------------------------------------------------------------------
// Ingest counters (Phase 4 surfaces them)
// ---------------------------------------------------------------------------

static RECEIVED: AtomicU64 = AtomicU64::new(0);
static ACCEPTED: AtomicU64 = AtomicU64::new(0);
static DROPPED_OVERSIZE: AtomicU64 = AtomicU64::new(0);
static DROPPED_BROKEN_STREAM: AtomicU64 = AtomicU64::new(0);
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
    /// The request body stream failed before it was read in full.
    pub dropped_broken_stream: u64,
    /// Not JSON, not an object, or no usable `hook_event_name`.
    pub dropped_malformed: u64,
    pub dropped_unknown_event: u64,
    /// Neither the header nor the body's `session_id` named a live terminal.
    pub dropped_no_terminal: u64,
    /// Carried an `agent_id` (ignored in v1).
    pub dropped_subagent: u64,
    /// Superseded inside the per-terminal limiter window (latest wins).
    pub coalesced: u64,
    /// Asserted no state (`SessionStart: compact`, `Notification:
    /// agent_completed`, …).
    pub dropped_no_state: u64,
}

impl IngestCounters {
    /// One line, for the config report.
    pub fn summary(&self) -> String {
        format!(
            "received={} accepted={} oversize={} broken_stream={} malformed={} unknown_event={} \
             no_terminal={} subagent={} coalesced={} no_state={}",
            self.received,
            self.accepted,
            self.dropped_oversize,
            self.dropped_broken_stream,
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
        dropped_broken_stream: r(&DROPPED_BROKEN_STREAM),
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

/// Why a `POST /terminals/agent-event` body never reached [`ingest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnreadableBody {
    /// Longer than `agent_event::MAX_BODY_BYTES`.
    Oversize,
    /// The body stream failed (client hung up, transport error).
    BrokenStream,
}

/// Count a body the route could not read — without allocating anything.
pub fn note_unreadable_body(why: UnreadableBody) {
    bump(&RECEIVED);
    match why {
        UnreadableBody::Oversize => bump(&DROPPED_OVERSIZE),
        UnreadableBody::BrokenStream => bump(&DROPPED_BROKEN_STREAM),
    }
    debug!(reason = ?why, "agent-event: body unreadable, dropped");
}

/// The source of every published `seq`: one process-wide counter, so a
/// terminal's sequence is strictly increasing even across a slot that was
/// replaced under the same terminal id.
static PUBLISH_SEQ: AtomicU64 = AtomicU64::new(0);

/// This process's publish epoch: a random boot id minted once. `seq` restarts
/// at 0 with every runner process, so a webview that outlives a runner
/// restart compares seqs only within one epoch — a different epoch always
/// wins and resets the seq it holds.
pub fn publish_epoch() -> &'static str {
    static EPOCH: OnceLock<String> = OnceLock::new();
    EPOCH.get_or_init(|| uuid::Uuid::new_v4().simple().to_string())
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
    /// The `seq` of the last publish (0 = never published).
    seq: u64,
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
            seq: 0,
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
        self.sideband_view_at(now_ms(), wall_now_ms())
    }

    /// [`Self::sideband_view`] against explicit clocks (`mono_now` on the
    /// reducer's timeline, `wall_now` unix wall millis).
    pub(crate) fn sideband_view_at(
        &self,
        mono_now: u64,
        wall_now: u64,
    ) -> Option<ObservedAgentState> {
        let (state, at) = self.truth.last_reported(Source::Sideband)?;
        let word = match state {
            AgentState::Working => "working",
            AgentState::NeedsYou { .. } => "waiting_human",
            AgentState::Failed { .. } => "blocked",
            _ => "finished",
        };
        // `at` is on the reducer's monotonic timeline; wind-down folds
        // `set_at_ms` with wall-clock stamps, so it leaves as wall clock.
        Some(ObservedAgentState {
            state: word.to_string(),
            set_at_ms: i64::try_from(mono_to_wall_at(at, mono_now, wall_now)).unwrap_or(i64::MAX),
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

    /// True when `(v, d)` differs from what was last published. Pure: a
    /// READ asks this and consumes nothing.
    fn differs_from_published(&self, v: Verdict, d: &HookDelivery) -> bool {
        !self
            .published
            .as_ref()
            .is_some_and(|(pv, pd)| *pv == v && pd == d)
    }

    /// True (and remembered, with a fresh `seq`) when `(v, d)` differs from
    /// what was last published. Only a PUBLISH calls this.
    fn take_if_changed(&mut self, v: Verdict, d: &HookDelivery) -> bool {
        if !self.differs_from_published(v, d) {
            return false;
        }
        self.published = Some((v, d.clone()));
        self.seq = PUBLISH_SEQ.fetch_add(1, Ordering::Relaxed) + 1;
        true
    }

    /// The `seq` of the last publish (0 = never published).
    pub fn published_seq(&self) -> u64 {
        self.seq
    }

    /// Could PTY input change this pane's verdict right now? Only while the
    /// published verdict is a `NeedsYou` (rule 5: the human answered) — every
    /// other state ignores input, so the input path does no work for it.
    fn input_may_answer(&self) -> bool {
        self.published
            .as_ref()
            .is_some_and(|(v, _)| matches!(v.state, AgentState::NeedsYou { .. }))
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
    /// [`publish_epoch`]: `seq` is ordered only within one epoch.
    pub epoch: &'static str,
    /// This publish's per-terminal sequence number (strictly increasing).
    pub seq: u64,
    pub verdict: Verdict,
    pub hook_delivery: HookDelivery,
}

/// One row of `get_terminal_agent_states` / `GET /terminals/agent-state`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalAgentState {
    pub terminal_id: String,
    /// [`publish_epoch`]: `seq` is ordered only within one epoch.
    pub epoch: &'static str,
    /// The `seq` of the terminal's last publish when this row was read. The
    /// row is at least as new as that publish, so a reader holding a HIGHER
    /// `seq` (from an event) must discard the row.
    pub seq: u64,
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
    let (out, rate_limit_hint) = ingest_at(dir, header_terminal, body, now_ms, Instant::now());
    if let Some(terminal_id) = rate_limit_hint {
        super::usage_limit::fire_event_hint(terminal_id, RATE_LIMIT_HINT);
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
/// The second value is the resolved terminal when the body was a top-level
/// `StopFailure{rate_limit}` — read off the ONE projection, never a re-parse.
fn ingest_at(
    dir: &impl SlotDirectory,
    header_terminal: Option<&str>,
    body: &[u8],
    now_ms: u64,
    now_instant: Instant,
) -> (IngestOutcome, Option<String>) {
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
            return (IngestOutcome::Dropped(e.as_str()), None);
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
        return (IngestOutcome::Dropped("no_terminal"), None);
    };
    let rate_limit_hint = stop_failure_is_rate_limit(&projection).then(|| terminal_id.clone());

    let Ok(mut slot) = slot.lock() else {
        bump(&DROPPED_NO_TERMINAL);
        return (IngestOutcome::Dropped("slot_poisoned"), None);
    };
    // A subagent's event still PROVES delivery; it just claims no state (v1).
    slot.note_hook_event(&projection, now_ms);
    if projection.is_subagent {
        bump(&DROPPED_SUBAGENT);
        return (IngestOutcome::Dropped("subagent"), rate_limit_hint);
    }
    let obs = Observation::hook(projection.to_hook_event(), now_ms);
    // An event that claims no state is dropped BEFORE the limiter: latest-wins
    // would otherwise let it displace a pending real edge (a deferred `Stop`)
    // and then apply nothing.
    if obs.kind.claimed_state().is_none() {
        bump(&DROPPED_NO_STATE);
        return (IngestOutcome::Dropped("no_state"), rate_limit_hint);
    }
    let out = match slot.hook_limiter.offer(obs, now_instant) {
        LimiterDecision::EmitNow(obs) => {
            apply(&mut slot, &obs, now_ms);
            IngestOutcome::Applied { terminal_id }
        }
        LimiterDecision::Defer { delay } => IngestOutcome::Deferred { terminal_id, delay },
        LimiterDecision::Held => {
            bump(&COALESCED);
            IngestOutcome::Coalesced { terminal_id }
        }
    };
    (out, rate_limit_hint)
}

fn apply(slot: &mut AgentStateSlot, obs: &Observation, now_ms: u64) {
    match slot.offer(obs, now_ms) {
        ObserveOutcome::Accepted => bump(&ACCEPTED),
        ObserveOutcome::DroppedSubagent => bump(&DROPPED_SUBAGENT),
        ObserveOutcome::DroppedNoState => bump(&DROPPED_NO_STATE),
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
/// (a full grid read — only on an explicit read, never on the sweep). A READ:
/// it consumes no pending publish.
pub fn read_session(
    session: &crate::terminal::session::TerminalSession,
    terminal_id: &str,
    with_grid: bool,
) -> TerminalAgentState {
    let (state, _) = compute(session, with_grid, false);
    let TerminalAgentStateParts {
        verdict,
        delivery,
        ages,
        seq,
    } = state;
    let (metrics, _) = crate::terminal::agent_metrics::compute(session, terminal_id, false);
    TerminalAgentState {
        terminal_id: terminal_id.to_string(),
        epoch: publish_epoch(),
        seq,
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
    /// The slot's `seq` after this compute (bumped only by a consuming one
    /// that saw a change).
    seq: u64,
}

/// Compute a pane's state, and whether it changed since the last publish.
///
/// `consume` is true ONLY for a publish: it records the result as published
/// (and assigns a new `seq`). A read passes `false`, reports `changed = false`
/// and leaves the pending change for the publish that owes it — a read that
/// consumed would leave subscribers holding the stale verdict (e.g. an
/// authoritative permission ask the human already answered).
fn compute(
    session: &crate::terminal::session::TerminalSession,
    with_grid: bool,
    consume: bool,
) -> (TerminalAgentStateParts, bool) {
    let now = now_ms();
    let wall_now = wall_now_ms();
    let slots = session.last_input();
    let input = InputEvidence {
        // Any human or runner input, control responses excluded — the
        // "human answered" signal of the reducer's rule 5.
        last_submit_ms: slots.latest().map(|o| instant_to_unix_ms(o.at)),
    };
    let runner_submit_ms = slots.last_submit.as_ref().map(|o| instant_to_unix_ms(o.at));
    let grid = with_grid.then(|| match session.observe_grid_idle() {
        // The tracker stamps wall clock; the reducer runs on `now_ms`.
        qontinui_runner_lib::wind_down::GridIdle::Idle { since_ms } => GridIdle::Idle {
            since_ms: wall_to_mono_at(u64::try_from(since_ms).unwrap_or(0), now, wall_now),
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
    let changed = consume && guard.take_if_changed(verdict, &delivery);
    let ages = guard.last_seen_ages(now);
    let seq = guard.published_seq();
    // Change detection above compared reducer-timeline verdicts; what leaves
    // the module is wall clock.
    let verdict = verdict_to_wall(verdict, now, wall_now);
    (
        TerminalAgentStateParts {
            verdict,
            delivery,
            ages,
            seq,
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
    let (parts, changed) = compute(session, false, true);
    let (metrics, metrics_changed) =
        crate::terminal::agent_metrics::compute(session, terminal_id, true);
    crate::terminal::headroom::on_tick(
        terminal_id,
        &parts.verdict,
        metrics.account.as_deref(),
        metrics.headroom.as_ref(),
        wall_now_ms(),
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
                epoch: publish_epoch(),
                seq: parts.seq,
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

/// PTY input reached terminal `terminal_id` (called by `TerminalSession`'s
/// input recorder AFTER the bytes are on the wire and its own lock is
/// released). When the pane's published verdict is a `NeedsYou` — the only
/// state input can change — schedule ONE debounced publish of this terminal
/// on a detached thread, so the answered state reaches the webview now rather
/// than on the next sweep tick. Any other state costs one short slot lock.
pub fn on_pty_input(terminal_id: &str, slot: &Mutex<AgentStateSlot>) {
    let may_answer = slot
        .lock()
        .map(|s| s.input_may_answer())
        .unwrap_or_else(|e| e.into_inner().input_may_answer());
    if may_answer {
        schedule_input_publish(terminal_id);
    }
}

/// Terminals with an input-triggered publish already scheduled.
static INPUT_PUBLISH_PENDING: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// Claim (`true`) or release (`false`) a terminal's pending input publish.
/// A claim fails while one is already pending.
fn input_publish_pending(terminal_id: &str, claim: bool) -> bool {
    let mut guard = INPUT_PUBLISH_PENDING
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let set = guard.get_or_insert_with(HashSet::new);
    if claim {
        set.insert(terminal_id.to_string())
    } else {
        set.remove(terminal_id)
    }
}

/// Schedule one publish of `terminal_id` after [`INPUT_PUBLISH_DEBOUNCE`].
/// `false` when one was already pending (the keystrokes coalesce into it).
fn schedule_input_publish(terminal_id: &str) -> bool {
    if !input_publish_pending(terminal_id, true) {
        return false;
    }
    let id = terminal_id.to_string();
    let spawned = std::thread::Builder::new()
        .name("agent-state-input-publish".into())
        .spawn(move || {
            std::thread::sleep(INPUT_PUBLISH_DEBOUNCE);
            input_publish_pending(&id, false);
            publish_terminal(&id);
        });
    if spawned.is_err() {
        // The sweep tick still publishes it.
        input_publish_pending(terminal_id, false);
    }
    true
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
/// stale, killed after [`CLI_VERSION_TIMEOUT`]. Never blocks the caller; a
/// failed or timed-out probe leaves the version UNKNOWN.
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
            // Bounded: a hung CLI is killed at the budget, so `in_flight`
            // always clears and the next probe is only an hour away.
            let mut cmd = crate::process_helpers::no_window("claude");
            cmd.arg("--version");
            let value = crate::process_helpers::output_with_timeout(cmd, CLI_VERSION_TIMEOUT)
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
        .0
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
            .0
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
    fn agent_event_reducer_stamps_cross_the_boundary_by_age() {
        // The reducer's timeline runs an hour ahead of the wall clock (a wall
        // step back since the anchor): a stamp 5s old is still 5s old.
        let (mono_now, wall_now) = (NOW + 3_600_000, NOW);
        assert_eq!(
            mono_to_wall_at(mono_now - 5_000, mono_now, wall_now),
            NOW - 5_000
        );
        assert_eq!(
            wall_to_mono_at(NOW - 5_000, mono_now, wall_now),
            mono_now - 5_000
        );
        // A future stamp clamps to "now" rather than wrapping.
        assert_eq!(mono_to_wall_at(mono_now + 10, mono_now, wall_now), wall_now);

        use qontinui_runner_lib::agent_truth::{Confidence, Disagreement};
        let v = Verdict {
            state: AgentState::Working,
            source: Some(Source::Hook),
            since_ms: Some(mono_now - 7_000),
            confidence: Some(Confidence::Authoritative),
            disagreement: Some(Disagreement::QuietWhileWorking {
                grid_idle_since_ms: mono_now - 2_000,
            }),
        };
        let w = verdict_to_wall(v, mono_now, wall_now);
        assert_eq!(w.since_ms, Some(NOW - 7_000));
        assert_eq!(
            w.disagreement,
            Some(Disagreement::QuietWhileWorking {
                grid_idle_since_ms: NOW - 2_000
            })
        );
        assert_eq!(
            (w.state, w.source, w.confidence),
            (v.state, v.source, v.confidence)
        );
    }

    #[test]
    fn agent_event_publish_epoch_is_one_boot_id_per_process() {
        let e = publish_epoch();
        assert_eq!(e.len(), 32, "a uuid boot id: {e}");
        assert!(std::ptr::eq(e, publish_epoch()), "minted once");
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
            epoch: publish_epoch(),
            seq: 7,
            verdict: Verdict::UNKNOWN,
            hook_delivery: HookDelivery::Installed,
        };
        let v = serde_json::to_value(&ev).unwrap();
        assert_eq!(v["terminalId"], "t1");
        assert_eq!(v["seq"], 7);
        assert_eq!(v["epoch"], publish_epoch());
        assert_eq!(v["verdict"]["state"]["name"], "unknown");
        assert_eq!(
            v["hookDelivery"],
            serde_json::json!({"status": "installed"})
        );
        let row = TerminalAgentState {
            terminal_id: "t1".into(),
            epoch: publish_epoch(),
            seq: 3,
            verdict: Verdict::UNKNOWN,
            hook_delivery: HookDelivery::Installed,
            last_seen_age_ms: LastSeenAges {
                hook: Some(5),
                ..Default::default()
            },
            metrics: SessionMetrics::default(),
        };
        let v = serde_json::to_value(&row).unwrap();
        assert_eq!(v["seq"], 3);
        assert_eq!(v["epoch"], publish_epoch());
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

    fn permission_ask(at_ms: u64) -> Observation {
        Observation::hook(
            agent_event::project_bytes(&body(serde_json::json!({
                "hook_event_name": "PermissionRequest",
                "tool_name": "Bash",
            })))
            .unwrap()
            .to_hook_event(),
            at_ms,
        )
    }

    fn test_session() -> crate::terminal::session::TerminalSession {
        crate::terminal::session::tests::make_test_session(Arc::new(Mutex::new(Vec::new())))
    }

    /// H1: a READ between a change and its publish must not consume the
    /// change — the publish still emits it, with a new `seq`.
    #[test]
    fn agent_event_read_between_change_and_publish_does_not_swallow_it() {
        let session = test_session();
        let (first, _) = compute(&session, false, true);
        let seq0 = first.seq;
        session
            .agent_state_slot()
            .lock()
            .unwrap()
            .offer(&permission_ask(now_ms()), now_ms());

        // A read (GET /terminals/agent-state, get_terminal_agent_states).
        let (read, changed) = compute(&session, false, false);
        assert!(!changed, "a read reports no change");
        assert!(read.verdict.is_authoritative_permission_ask());
        assert_eq!(read.seq, seq0, "a read assigns no seq");
        let row = read_session(&session, "test", false);
        assert_eq!(row.seq, seq0);

        // The publish still sees — and emits — the change.
        let (published, changed) = compute(&session, false, true);
        assert!(changed, "the publish must still emit the change");
        assert!(published.verdict.is_authoritative_permission_ask());
        assert!(published.seq > seq0);
        // … exactly once.
        let (_, again) = compute(&session, false, true);
        assert!(!again);
    }

    /// M2: `seq` is strictly increasing per terminal, across publishes and
    /// even across a replaced slot (one process-wide source).
    #[test]
    fn agent_event_publish_seq_is_strictly_increasing() {
        let mut a = AgentStateSlot::new(StateCapabilities::claude());
        assert_eq!(a.published_seq(), 0);
        let d = HookDelivery::Installed;
        assert!(a.take_if_changed(Verdict::UNKNOWN, &d));
        let s1 = a.published_seq();
        assert!(s1 > 0);
        assert!(!a.take_if_changed(Verdict::UNKNOWN, &d));
        assert_eq!(a.published_seq(), s1, "no change, no new seq");
        assert!(a.take_if_changed(Verdict::UNKNOWN, &HookDelivery::Unknown { evidence: None }));
        let s2 = a.published_seq();
        assert!(s2 > s1);
        let mut replaced = AgentStateSlot::new(StateCapabilities::claude());
        assert!(replaced.take_if_changed(Verdict::UNKNOWN, &d));
        assert!(replaced.published_seq() > s2);
    }

    /// M1: a no-state hook (`Notification: agent_completed`) is dropped BEFORE
    /// the limiter, so it cannot displace a deferred real edge.
    #[test]
    fn agent_event_no_state_hook_never_displaces_a_pending_edge() {
        let dir = Dir::with(&[("t1", "sid-1")]);
        let burst = at(20_000);
        let send = |v: serde_json::Value| ingest_at(&dir, Some("t1"), &body(v), NOW, burst).0;
        assert!(matches!(
            send(serde_json::json!({"hook_event_name": "UserPromptSubmit"})),
            IngestOutcome::Applied { .. }
        ));
        assert!(matches!(
            send(serde_json::json!({"hook_event_name": "Stop"})),
            IngestOutcome::Deferred { .. }
        ));
        let before = ingest_counters().dropped_no_state;
        assert_eq!(
            send(serde_json::json!({
                "hook_event_name": "Notification",
                "notification_type": "agent_completed",
            })),
            IngestOutcome::Dropped("no_state")
        );
        assert!(ingest_counters().dropped_no_state > before);
        assert!(flush_deferred(&dir, "t1", NOW + 300));
        assert_eq!(
            dir.verdict("t1").state,
            AgentState::TurnEnded,
            "the Stop survived"
        );
    }

    /// L2: the rate-limit hint comes off the ingest's own projection.
    #[test]
    fn migration_rate_limit_hint_is_returned_by_the_ingest() {
        let dir = Dir::with(&[("t1", "sid-1")]);
        let (out, hint) = ingest_at(
            &dir,
            Some("t1"),
            &body(serde_json::json!({"hook_event_name": "StopFailure", "error": "rate_limit"})),
            NOW,
            at(30_000),
        );
        assert!(matches!(out, IngestOutcome::Applied { .. }));
        assert_eq!(hint.as_deref(), Some("t1"));
        let (_, hint) = ingest_at(
            &dir,
            Some("t1"),
            &body(serde_json::json!({"hook_event_name": "Stop"})),
            NOW + 1,
            at(30_010),
        );
        assert_eq!(hint, None);
        let (_, hint) = ingest_at(
            &dir,
            Some("nobody"),
            &body(serde_json::json!({"hook_event_name": "StopFailure", "error": "rate_limit"})),
            NOW + 2,
            at(30_020),
        );
        assert_eq!(hint, None, "an unresolved terminal gets no hint");
    }

    /// L1: an unreadable body is counted without allocating, and a broken
    /// stream is told apart from an oversize one.
    #[test]
    fn agent_event_route_unreadable_body_is_counted_by_kind() {
        let c0 = ingest_counters();
        note_unreadable_body(UnreadableBody::Oversize);
        let c1 = ingest_counters();
        assert!(c1.dropped_oversize > c0.dropped_oversize);
        assert!(c1.received > c0.received);
        note_unreadable_body(UnreadableBody::BrokenStream);
        let c2 = ingest_counters();
        assert!(c2.dropped_broken_stream > c1.dropped_broken_stream);
        assert!(c2.summary().contains("broken_stream="));
    }

    /// M4: PTY input schedules a publish only while the published verdict is
    /// a `NeedsYou`, and a burst of keystrokes coalesces into one.
    #[test]
    fn agent_event_pty_input_publishes_only_an_answerable_verdict() {
        let mut slot = AgentStateSlot::new(StateCapabilities::claude());
        assert!(!slot.input_may_answer(), "never published");
        slot.offer(&permission_ask(NOW), NOW);
        let v = slot.truth.verdict(NOW + 1);
        assert!(slot.take_if_changed(v, &HookDelivery::Installed));
        assert!(slot.input_may_answer());
        let working = Verdict {
            state: AgentState::Working,
            ..v
        };
        assert!(slot.take_if_changed(working, &HookDelivery::Installed));
        assert!(
            !slot.input_may_answer(),
            "input cannot change a Working verdict"
        );

        let tid = format!("t-input-{}", uuid::Uuid::new_v4());
        assert!(schedule_input_publish(&tid));
        assert!(
            !schedule_input_publish(&tid),
            "coalesced into the pending one"
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while !schedule_input_publish(&tid) {
            assert!(Instant::now() < deadline, "the pending publish never ran");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// M7: the reducer clock is one anchored timeline — the same instant maps
    /// to the same millis on every call, and later instants never map earlier.
    #[test]
    fn agent_event_clock_is_anchored_and_monotonic() {
        let i = Instant::now();
        let a = instant_to_unix_ms(i);
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(instant_to_unix_ms(i), a, "no per-call re-anchoring");
        let later = now_ms();
        assert!(later >= a);
        assert!(instant_to_unix_ms(i + Duration::from_secs(1)) >= a + 1_000);
    }
}
