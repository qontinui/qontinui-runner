//! Coord PR-merge diagnosis client + disposition classification — Phase 3 of
//! plan `2026-07-04-runner-pr-shepherd.md`.
//!
//! When the device-local shepherd sees a PR that has been fully green on an
//! unchanged head past the threshold yet still open (Phase 2's
//! `PrAction::GreenButUnmerged`), *something* is not landing it. This module
//! asks coord "what is the merge train doing with this PR, and is coord itself
//! the broken component?", then maps the answer onto a [`ShepherdDisposition`]
//! and a log-only [`ShepherdIntent`].
//!
//! ## Reads (all behind `FleetPrincipal`; device/agent JWT is accepted)
//!
//! | Method | Coord route | Purpose |
//! |---|---|---|
//! | [`CoordPrMergeClient::verdict`] | `GET /pr-merge/verdict/:owner/:name/:pr` | **Primary.** The `compute_pr_merge_verdict` join — outer_state, block_reason, escalate view, `next_action`. |
//! | [`CoordPrMergeClient::latest_block`] | `GET /pr-merge/events/:owner/:name/:pr` | Fallback + evidence: newest `predicate_eval` block_reason_code + `unlandable_cycle` counter. |
//! | [`CoordPrMergeClient::graph`] | `GET /pr-merge/graph?repo&pr` | Dependency-graph cycle detection + member readiness. |
//! | [`CoordPrMergeClient::queue_has`] | `GET /merge/queue` | Is a merge proposal actually enqueued for this PR? |
//! | [`CoordPrMergeClient::health_leader`] | `GET /pr-merge/health` | Is coord's leader lease fresh (or is the cluster leaderless)? |
//! | [`CoordPrMergeClient::reevaluate`] | `POST /pr-merge/prs/:owner/:name/:pr/reevaluate` | The reevaluate-once nudge on a suspected coord defect. |
//!
//! ## Classification — consume coord's taxonomy, never re-derive one
//!
//! The disposition is keyed on the verdict's `outer_state` + `block_reason`
//! (coord's own [`BlockReason::code`] wire strings) plus a few evidence
//! thresholds — **not** a parallel taxonomy authored here. The taxonomy's
//! source of truth is coord `predicate.rs::scheduler_phase` /
//! `BlockReason::code` (`predicate.rs:486`, `predicate.rs:844`); the
//! `merge-train-steward` "do NOT author a parallel taxonomy" rule binds this
//! daemon too.
//!
//! [`ShepherdDisposition::CoordDown`] is the one disposition the verdict cannot
//! self-report (a wedged/leaderless coord can't tell you it's wedged). It is
//! derived RUNNER-SIDE: the verdict/health call is unreachable, or the leader
//! lease is stale, while the runner's own `GitHubClient` is the independent
//! witness that the PR is genuinely green (we only reach classification from
//! the `GreenButUnmerged` handler, so that witness always holds here).
//!
//! Note: the original `coord:upstream-of` **inversion** defect class is FIXED
//! on coord main (#954 `3e378ca` — an `upstream-of` label no longer blocks the
//! labeled PR); no variant models it. Its 2026-07-04 deadlock payloads survive
//! only as classification fixtures for the *cycle-spin* class (see the tests).
//!
//! ## This module classifies; `watchers::pr_watcher` acts
//!
//! [`decide_action`] returns an *intent*; the `GreenButUnmerged` handler in
//! `watchers::pr_watcher` logs it and then EXECUTES it (Phases 4/5): the
//! reevaluate-once `POST` on a suspected [`ShepherdDisposition::CoordDefect`]
//! (a cheap idempotent nudge, guarded per `(pr, head)`), the device-local
//! author notify ([`ShepherdIntent::NotifyAuthor`] /
//! [`ShepherdIntent::CoordDownLocalAlert`], Phase 4), and the remediation-plan
//! + last-resort vet-imp spawn ([`ShepherdIntent::Escalate`], Phase 5). This
//! module itself performs only the read + reevaluate calls; it never notifies
//! or spawns.

use std::collections::HashSet;
use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, Utc};
use once_cell::sync::Lazy;
use serde::Deserialize;
use serde_json::Value;

/// Timeout for every coord diagnosis call. Short — a slow/wedged coord must not
/// stall the PR watcher's poll loop; a timeout is itself a `CoordDown` signal.
const COORD_HTTP_TIMEOUT: Duration = Duration::from_secs(10);

/// Cycle count above which an unlandable-cycle spin (with all members green) is
/// treated as a coord defect rather than legitimate churn. Mirrors the plan's
/// "cycles > 5" threshold; coord's own shepherd escalates on a similar counter.
const UNLANDABLE_CYCLE_DEFECT_THRESHOLD: u64 = 5;

/// Dwell (as a multiple of the green threshold) a green PR may sit in
/// `merge-state-unsettled` before it reads as a phantom-context hang rather
/// than GitHub still recomputing merge-state. Plan: "> 2× threshold".
const PHANTOM_HANG_THRESHOLD_MULT: u32 = 2;

/// Age past which a coord-side signal (stale ingest head, pass-but-no-land) is
/// treated as a defect rather than in-flight latency. Plan: "> 1h".
const DEFECT_AGE_FLOOR: Duration = Duration::from_secs(3600);

// ---------------------------------------------------------------------------
// Call errors — reachability is load-bearing for CoordDown classification.
// ---------------------------------------------------------------------------

/// Why a coord diagnosis call failed. The distinction is load-bearing: only
/// [`CoordCallError::Unreachable`] (connect/timeout/transport) is evidence for
/// `CoordDown`. An HTTP response — 401/403 from an expired device bearer, a
/// 404, a 500 — proves coord IS reachable and answering; treating those as
/// unreachability would turn the runner's own broken credentials into
/// COORD-DOWN alerts + orphaned-red spawns while coord is perfectly healthy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoordCallError {
    /// Transport-level failure (connect refused, DNS, timeout) — coord may
    /// genuinely be down/unreachable.
    Unreachable(String),
    /// Coord answered with a non-2xx status (or a 2xx whose body failed to
    /// decode) — coord IS reachable; the call just didn't yield data.
    Http { status: u16, detail: String },
}

impl CoordCallError {
    /// Is this an auth failure (401/403) — i.e. the RUNNER's credentials are
    /// broken, not coord?
    pub fn is_auth(&self) -> bool {
        matches!(
            self,
            CoordCallError::Http {
                status: 401 | 403,
                ..
            }
        )
    }

    /// Did coord answer at all? `true` for any HTTP response.
    pub fn coord_reachable(&self) -> bool {
        matches!(self, CoordCallError::Http { .. })
    }
}

impl std::fmt::Display for CoordCallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CoordCallError::Unreachable(e) => write!(f, "unreachable: {e}"),
            CoordCallError::Http { status, detail } => write!(f, "HTTP {status}: {detail}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Wire shapes — mirror the coord structs we consume (deserialize-only).
// ---------------------------------------------------------------------------

/// The escalate-path slice of the verdict, mirrored from coord's
/// `merge_verdict::EscalateView`. Present only when the PR is blocked on
/// `escalate-path-matched`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct EscalateView {
    pub disposition: String,
    pub category: String,
    pub pending_gate: String,
    #[serde(default)]
    pub reason: Option<String>,
}

/// The per-PR verdict, mirrored from coord's `merge_verdict::VerdictView`. Only
/// the fields this daemon consumes are modeled; unknown fields are ignored.
#[derive(Debug, Clone, Deserialize)]
pub struct PrMergeVerdict {
    #[serde(default)]
    pub outer_state: Option<String>,
    #[serde(default)]
    pub block_reason: Option<String>,
    #[serde(default)]
    pub block_evidence: Value,
    #[serde(default)]
    pub rollout_state: Option<String>,
    #[serde(default)]
    pub escalate: Option<EscalateView>,
    #[serde(default)]
    pub next_action: String,
    #[serde(default)]
    pub as_of: Option<DateTime<Utc>>,
}

/// Evidence distilled from the newest rows of `GET /pr-merge/events`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlockDiagnosis {
    /// `block_reason_code` of the newest `predicate_eval` row.
    pub code: Option<String>,
    /// `payload.detail` of that newest `predicate_eval` (opaque evidence blob).
    pub detail: Value,
    /// `head_sha` coord evaluated against on that newest `predicate_eval` — i.e.
    /// coord's INGESTED (hydrated) head. Compared with the runner's real head to
    /// detect stale ingest.
    pub head_sha: Option<String>,
    /// `cycles` of the newest `unlandable_cycle` row (the escalation counter).
    pub unlandable_cycles: Option<u64>,
    /// Alias of [`Self::head_sha`] — coord's hydrated head — named for the
    /// stale-ingest comparison the plan describes ("hydration head ≠ current
    /// head > 1h"). Coord exposes no distinct `hydration_head` field on events,
    /// so the eval head IS the hydration head.
    pub hydration_head: Option<String>,
}

/// Dependency-graph cycle facts distilled from `GET /pr-merge/graph`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GraphDiagnosis {
    pub cycle_detected: bool,
    pub cycle_member_count: usize,
    /// True when every node named in `cycle_members` is `ready` in the graph's
    /// node list — a cycle of green PRs that still won't land is the
    /// unlandable-spin defect shape.
    pub cycle_members_all_green: bool,
}

// ---- graph response (deserialize-only projection of coord's GraphResponse) --

#[derive(Debug, Deserialize)]
struct GraphResponseWire {
    #[serde(default)]
    nodes: Vec<GraphNodeWire>,
    #[serde(default)]
    cycle_detected: bool,
    #[serde(default)]
    cycle_members: Vec<PrRefWire>,
}

#[derive(Debug, Deserialize)]
struct GraphNodeWire {
    repo: String,
    pr_number: i32,
    #[serde(default)]
    ready: bool,
}

#[derive(Debug, Deserialize)]
struct PrRefWire {
    repo: String,
    pr: i32,
}

// ---- events response (deserialize-only projection) -------------------------

#[derive(Debug, Deserialize)]
struct EventsResponseWire {
    #[serde(default)]
    events: Vec<PrEventWire>,
}

#[derive(Debug, Deserialize)]
struct PrEventWire {
    #[serde(default)]
    event_kind: String,
    #[serde(default)]
    payload: Option<Value>,
    /// When coord recorded the event (`coord.pr_events.created_at`). Used by the
    /// Phase 5.4 coexistence check to tell a LIVE coord shepherd attempt (recent
    /// event on the current head) from a stale one.
    #[serde(default)]
    created_at: Option<DateTime<Utc>>,
}

// ---- merge queue response (deserialize-only projection) --------------------

#[derive(Debug, Deserialize)]
struct QueueResponseWire {
    #[serde(default)]
    proposals: Vec<ProposalWire>,
}

#[derive(Debug, Deserialize)]
struct ProposalWire {
    #[serde(default)]
    repos: Vec<RepoRefWire>,
}

#[derive(Debug, Deserialize)]
struct RepoRefWire {
    #[serde(default)]
    head_sha: String,
}

// ---- health response (deserialize-only projection) -------------------------

#[derive(Debug, Deserialize)]
struct HealthResponseWire {
    #[serde(default)]
    leader: Option<LeaderWire>,
}

#[derive(Debug, Deserialize)]
struct LeaderWire {
    #[serde(default)]
    lease_fresh: Option<bool>,
}

// ---------------------------------------------------------------------------
// Dispositions + intents
// ---------------------------------------------------------------------------

/// The typed class of a suspected coord defect — a green PR coord is wedged on.
/// Each maps from `block_reason_code` / verdict fields; see [`classify`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoordDefectClass {
    /// A dependency cycle of GREEN PRs spinning past
    /// [`UNLANDABLE_CYCLE_DEFECT_THRESHOLD`] cycles without landing.
    /// Suspected coord path: `engine.rs` cross-repo dep resolution /
    /// `unlandable_lifecycle.rs`.
    UnlandableCycleSpin,
    /// A green head stuck on `merge-state-unsettled` past
    /// [`PHANTOM_HANG_THRESHOLD_MULT`]× the green threshold — GitHub's
    /// merge-state should have settled long ago. Suspected coord path:
    /// `predicate.rs` merge-state gate / lost `heal_mergestate` webhook.
    PhantomContextHang,
    /// Coord's ingested (hydrated) head lags the real head past
    /// [`DEFECT_AGE_FLOOR`] — a lost `synchronize`/`converted_to_draft`
    /// webhook left `repo_branches` stale. Suspected coord path:
    /// `stuck_pr_reconciler.rs` hydration.
    StaleIngest,
    /// The verdict says pass/READY, the PR has been green past
    /// [`DEFECT_AGE_FLOOR`], yet no merge proposal is enqueued — the land was
    /// never scheduled. Suspected coord path: `merge_scheduler.rs` enqueue /
    /// leader reconcile.
    PassButNoLand,
}

impl CoordDefectClass {
    /// Stable kebab id for logs / plan filenames / dedup keys.
    pub fn as_str(self) -> &'static str {
        match self {
            CoordDefectClass::UnlandableCycleSpin => "unlandable-cycle-spin",
            CoordDefectClass::PhantomContextHang => "phantom-context-hang",
            CoordDefectClass::StaleIngest => "stale-ingest",
            CoordDefectClass::PassButNoLand => "pass-but-no-land",
        }
    }

    /// One-line hint at the suspected coord code path — carried into a Phase-5
    /// remediation plan. Not consumed in Phase 3 (log-only) beyond the intent
    /// log, but kept beside the class so the mapping has a single home.
    pub fn suspected_coord_path(self) -> &'static str {
        match self {
            CoordDefectClass::UnlandableCycleSpin => {
                "engine.rs::maybe_resolve_cross_repo_deps / unlandable_lifecycle.rs"
            }
            CoordDefectClass::PhantomContextHang => {
                "predicate.rs merge-state gate / heal_mergestate webhook"
            }
            CoordDefectClass::StaleIngest => "stuck_pr_reconciler.rs hydration",
            CoordDefectClass::PassButNoLand => "merge_scheduler.rs enqueue / leader reconcile",
        }
    }
}

/// What the shepherd concludes about a green-but-unmerged PR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShepherdDisposition {
    /// Coord is working the PR correctly and it will resolve on its own
    /// (CI settling, land in flight, below green-dwell). Do nothing.
    Transient,
    /// Coord is correctly HOLDING the PR pending author/operator action
    /// (escalate path, CI red, self-triggering-CI gate, …). Notify the author.
    LegitimateHold,
    /// Coord is the broken/wedged component on a PR the runner witnesses green.
    CoordDefect { class: CoordDefectClass },
    /// Coord is unreachable or leaderless — the one disposition the verdict
    /// can't self-report; derived runner-side against the GitHub-green witness.
    CoordDown,
}

/// The log-only action the shepherd would take. Phase 3 emits these as
/// `tracing` events; Phases 4/5 turn `NotifyAuthor` / `Escalate` into real
/// device-local injection + vet-imp spawns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShepherdIntent {
    /// No action.
    Nothing,
    /// Device-local author nudge (Phase 4 fills the transport).
    NotifyAuthor { reason: String },
    /// Ask coord to re-evaluate this PR once (the cheap idempotent nudge tried
    /// before escalating a suspected defect).
    Reevaluate,
    /// Escalate a confirmed coord defect to a remediation plan + vet-imp spawn
    /// (Phase 5 fills the spawn).
    Escalate { class: CoordDefectClass },
    /// Coord-down: emit a device-local alert (the coord.alert-equivalent the
    /// wedged coord cannot emit itself).
    CoordDownLocalAlert,
}

// ---------------------------------------------------------------------------
// Classification — pure, table-driven, unit-tested.
// ---------------------------------------------------------------------------

/// The distilled, coord-independent input to [`classify`]. Every field is
/// either a coord-reported value (verdict/events/graph/queue/health) or a
/// runner-side witness; classification is a pure function of this struct so it
/// is fully unit-testable from recorded payloads.
#[derive(Debug, Clone)]
pub struct ClassificationInput {
    /// The runner's own `GitHubClient` witnesses the PR as green. Always true in
    /// production (we only classify from `GreenButUnmerged`), carried
    /// explicitly so the `CoordDown` reasoning is legible + testable.
    pub github_green: bool,
    /// The verdict (and thus coord) was reachable. `false` → `CoordDown`.
    pub coord_reachable: bool,
    /// Coord's leader lease is fresh. `false` (leaderless / stale) → `CoordDown`.
    pub leader_healthy: bool,
    /// `outer_state` from the verdict (`OuterState::as_str`).
    pub outer_state: Option<String>,
    /// `block_reason` from the verdict (`BlockReason::code`).
    pub block_reason: Option<String>,
    /// Newest `unlandable_cycle` counter (from events).
    pub unlandable_cycles: u64,
    /// Dep-graph cycle detected for this PR's component.
    pub cycle_detected: bool,
    /// Every member of that cycle is green/ready.
    pub cycle_members_all_green: bool,
    /// The green streak has outlived [`PHANTOM_HANG_THRESHOLD_MULT`]× the green
    /// threshold (computed at the call site from `green_for`).
    pub merge_state_unsettled_exceeded: bool,
    /// Coord's hydrated head lags the real head past [`DEFECT_AGE_FLOOR`].
    pub stale_ingest: bool,
    /// Green past [`DEFECT_AGE_FLOOR`] (the pass-but-no-land age gate).
    pub aged_past_floor: bool,
    /// A merge proposal for this PR's head is enqueued in `/merge/queue`.
    pub in_queue: bool,
}

impl Default for ClassificationInput {
    fn default() -> Self {
        ClassificationInput {
            github_green: true,
            coord_reachable: true,
            leader_healthy: true,
            outer_state: None,
            block_reason: None,
            unlandable_cycles: 0,
            cycle_detected: false,
            cycle_members_all_green: false,
            merge_state_unsettled_exceeded: false,
            stale_ingest: false,
            aged_past_floor: false,
            in_queue: false,
        }
    }
}

/// Map the distilled evidence onto a [`ShepherdDisposition`]. PURE + total —
/// every input resolves. Ordering matters: `CoordDown` dominates (a wedged
/// coord can misreport everything else), then defect classes (a green PR coord
/// is stuck on), then legitimate holds (coord correctly waiting on a human),
/// then transient.
pub fn classify(input: &ClassificationInput) -> ShepherdDisposition {
    // 1. CoordDown — the verdict can't self-report this. The runner witnesses
    //    the PR green (github_green) but coord is unreachable or leaderless.
    if input.github_green && (!input.coord_reachable || !input.leader_healthy) {
        return ShepherdDisposition::CoordDown;
    }

    // 2. Coord-defect classes — a green PR coord is wedged on. Keyed on coord's
    //    own block_reason taxonomy + evidence thresholds.
    //
    //    NOTE: `has-blocking-label` is deliberately NOT a defect class. Per the
    //    fleet convention (CLAUDE.md: "To hold a PR, set coord:blocked"), a
    //    blocking label on a green PR is the OPERATOR's intended hold
    //    mechanism — coord's label gate is functioning correctly. It maps to
    //    LegitimateHold (author-action nudge) below.
    if matches!(input.block_reason.as_deref(), Some("merge-state-unsettled"))
        && input.merge_state_unsettled_exceeded
    {
        return ShepherdDisposition::CoordDefect {
            class: CoordDefectClass::PhantomContextHang,
        };
    }

    // Unlandable-cycle spin: a cycle of green PRs churning past the threshold.
    if input.cycle_detected
        && input.cycle_members_all_green
        && input.unlandable_cycles > UNLANDABLE_CYCLE_DEFECT_THRESHOLD
    {
        return ShepherdDisposition::CoordDefect {
            class: CoordDefectClass::UnlandableCycleSpin,
        };
    }

    // Stale ingest: coord's hydrated head lags the real head past the floor.
    if input.stale_ingest {
        return ShepherdDisposition::CoordDefect {
            class: CoordDefectClass::StaleIngest,
        };
    }

    // Pass-but-no-land: verdict says pass/READY, green past the floor, yet no
    // proposal is enqueued — the land was never scheduled.
    if input.aged_past_floor
        && !input.in_queue
        && is_pass_state(&input.outer_state, &input.block_reason)
    {
        return ShepherdDisposition::CoordDefect {
            class: CoordDefectClass::PassButNoLand,
        };
    }

    // 3. Legitimate holds — coord is correctly holding pending a human. These
    //    are coord's real, actionable block reasons.
    if is_legitimate_hold(&input.outer_state, &input.block_reason) {
        return ShepherdDisposition::LegitimateHold;
    }

    // 4. Otherwise transient — coord is working through it (CI pending, land in
    //    flight, below green-dwell, freshly ready, or never-evaluated).
    ShepherdDisposition::Transient
}

/// A "pass" verdict state — coord believes the PR should land. Used by the
/// pass-but-no-land gate.
fn is_pass_state(outer_state: &Option<String>, block_reason: &Option<String>) -> bool {
    matches!(outer_state.as_deref(), Some("READY"))
        || matches!(block_reason.as_deref(), Some("none"))
}

/// A block reason / outer state that represents a LEGITIMATE hold pending
/// author or operator action (coord is behaving correctly). Keyed on coord's
/// `BlockReason::code` wire strings. `has-blocking-label` belongs here — a
/// `coord:blocked` label is the operator's intended hold (fleet convention),
/// so the right response is the author-action nudge, never a coord-defect
/// escalation.
fn is_legitimate_hold(outer_state: &Option<String>, block_reason: &Option<String>) -> bool {
    if matches!(
        block_reason.as_deref(),
        Some("has-blocking-label")
            | Some("escalate-path-matched")
            | Some("self-triggering-ci-gate")
            | Some("ci-not-green")
            | Some("main-red")
            | Some("main-status-unknown")
            | Some("required-checks-missing")
            | Some("uncoordinated-version-bump")
            | Some("cargo-fmt-drift")
            | Some("has-cross-repo-dependency")
            | Some("auto-merge-disabled")
            | Some("behind-main-or-unstable")
    ) {
        return true;
    }
    matches!(
        outer_state.as_deref(),
        Some("ESCALATED") | Some("CI_RED_TRIAGE")
    )
}

/// Map a disposition to the (terminal) intent. PURE + total — unit tested.
///
/// For [`ShepherdDisposition::CoordDefect`] this returns the TERMINAL intent
/// ([`ShepherdIntent::Escalate`]); the call site emits a transient
/// [`ShepherdIntent::Reevaluate`] first (once per `(pr, head)`), and only falls
/// through to this escalate intent when the reevaluate nudge did not clear the
/// defect on the next poll.
pub fn decide_action(disposition: ShepherdDisposition) -> ShepherdIntent {
    match disposition {
        ShepherdDisposition::Transient => ShepherdIntent::Nothing,
        ShepherdDisposition::LegitimateHold => ShepherdIntent::NotifyAuthor {
            reason: "pr_hold_triage".to_string(),
        },
        ShepherdDisposition::CoordDefect { class } => ShepherdIntent::Escalate { class },
        ShepherdDisposition::CoordDown => ShepherdIntent::CoordDownLocalAlert,
    }
}

// ---------------------------------------------------------------------------
// Evidence assembly (pure) — build ClassificationInput from parsed responses.
// ---------------------------------------------------------------------------

/// Everything the diagnosis reads, bundled for the pure builder. Borrowed so
/// tests can construct it from recorded payloads without owning the client.
pub struct Evidence<'a> {
    /// The runner's own GitHub-green witness (true in the shepherd path).
    pub github_green: bool,
    /// Coord answered the verdict call with SOME HTTP response — including a
    /// 401/403 from the runner's own expired bearer, a 404, or a 5xx. Only a
    /// transport-level failure ([`CoordCallError::Unreachable`]) sets this
    /// `false`. This is deliberately DECOUPLED from `verdict.is_some()`: an
    /// auth failure yields `verdict: None` but `coord_reachable: true`, which
    /// classifies as `Transient` rather than fabricating `CoordDown`.
    pub coord_reachable: bool,
    /// `Some` iff the verdict call succeeded with a decodable 2xx body.
    pub verdict: Option<&'a PrMergeVerdict>,
    /// `Some(true/false)` iff the `/pr-merge/health` call succeeded (the leader
    /// lease's `lease_fresh`); `None` → the health call itself failed.
    ///
    /// A `None` (health-call blip) does NOT force `CoordDown` on its own — only
    /// an EXPLICIT `Some(false)` (coord reports itself leaderless) or an
    /// unreachable VERDICT does. Otherwise a transient health-endpoint hiccup
    /// while the verdict is perfectly reachable would mask the verdict's real
    /// defect signal. See [`build_classification_input`].
    pub leader_fresh: Option<bool>,
    pub block: Option<&'a BlockDiagnosis>,
    pub graph: Option<&'a GraphDiagnosis>,
    pub in_queue: bool,
    /// The runner's real current head for the PR (stale-ingest comparison).
    pub current_head: &'a str,
    /// The Phase-2 green streak length.
    pub green_for: Duration,
    /// The configured green threshold (for the phantom-hang multiple).
    pub green_threshold: Duration,
}

/// Distill [`Evidence`] into a [`ClassificationInput`]. PURE (no IO) so it is
/// unit-testable from recorded verdict/events/graph payloads.
pub fn build_classification_input(ev: &Evidence) -> ClassificationInput {
    let coord_reachable = ev.coord_reachable;
    // Unknown health (call failed) defaults to healthy: only an EXPLICIT
    // leaderless report — or an unreachable verdict (above) — should trip
    // CoordDown, so a health blip can't mask the verdict's defect signal.
    let leader_healthy = ev.leader_fresh.unwrap_or(true);

    let (outer_state, block_reason) = match ev.verdict {
        Some(v) => (v.outer_state.clone(), v.block_reason.clone()),
        None => (None, None),
    };

    let unlandable_cycles = ev.block.and_then(|b| b.unlandable_cycles).unwrap_or(0);
    let (cycle_detected, cycle_members_all_green) = match ev.graph {
        Some(g) => (g.cycle_detected, g.cycle_members_all_green),
        None => (false, false),
    };

    let merge_state_unsettled_exceeded =
        ev.green_for > ev.green_threshold * PHANTOM_HANG_THRESHOLD_MULT;
    let aged_past_floor = ev.green_for > DEFECT_AGE_FLOOR;

    // Stale ingest: coord's hydrated head (from the newest predicate_eval)
    // differs from the runner's real head AND the streak has outlived the floor.
    let stale_ingest = aged_past_floor
        && ev
            .block
            .and_then(|b| b.hydration_head.as_deref())
            .map(|coord_head| !coord_head.is_empty() && coord_head != ev.current_head)
            .unwrap_or(false);

    ClassificationInput {
        github_green: ev.github_green,
        coord_reachable,
        leader_healthy,
        outer_state,
        block_reason,
        unlandable_cycles,
        cycle_detected,
        cycle_members_all_green,
        merge_state_unsettled_exceeded,
        stale_ingest,
        aged_past_floor,
        in_queue: ev.in_queue,
    }
}

// ---------------------------------------------------------------------------
// The HTTP client.
// ---------------------------------------------------------------------------

/// A thin reqwest client over coord's `/pr-merge/*` + `/merge/queue` diagnosis
/// surface. Reuses the runner's canonical coord base-URL resolution
/// (`profiles::resolve_coord_base`, honoring `COORD_HTTP_URL`) and the cached
/// device/agent bearer (`AuthManager::get_access_token`) — the same plumbing
/// `fleet.rs` and `claude_session::coord_register` use. Every method is
/// best-effort: it returns `Err` on any transport/status failure and never
/// panics, so the caller can treat unreachability as a `CoordDown` signal.
pub struct CoordPrMergeClient {
    client: reqwest::Client,
    base: String,
    token: Option<String>,
}

impl CoordPrMergeClient {
    /// Build a client, resolving the coord base + bearer once. Returns `None`
    /// when no coord base is configured (env `COORD_HTTP_URL` / active
    /// profile) — the shepherd then cannot diagnose and treats the PR as
    /// undiagnosable rather than fabricating a verdict.
    pub fn new() -> Option<Self> {
        let base = coord_http_base()?;
        let token = crate::auth::AuthManager::new()
            .get_access_token()
            .ok()
            .filter(|t| !t.is_empty());
        let client = reqwest::Client::builder()
            .timeout(COORD_HTTP_TIMEOUT)
            .build()
            .ok()?;
        Some(Self {
            client,
            base: base.trim_end_matches('/').to_string(),
            token,
        })
    }

    /// Attach the bearer (when present) to a request builder.
    fn auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.token {
            Some(t) => req.bearer_auth(t),
            None => req,
        }
    }

    /// GET + deserialize JSON, mapping failures onto [`CoordCallError`]:
    /// transport failure → `Unreachable` (a CoordDown signal); any HTTP
    /// response (non-2xx, or a 2xx that fails to decode) → `Http` (coord IS
    /// reachable — never a CoordDown signal).
    async fn get_json<T: for<'de> Deserialize<'de>>(&self, url: &str) -> Result<T, CoordCallError> {
        let resp = self
            .auth(self.client.get(url))
            .send()
            .await
            .map_err(|e| CoordCallError::Unreachable(format!("GET {url}: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(CoordCallError::Http {
                status: status.as_u16(),
                detail: format!("GET {url}"),
            });
        }
        resp.json::<T>().await.map_err(|e| CoordCallError::Http {
            status: status.as_u16(),
            detail: format!("GET {url}: decode: {e}"),
        })
    }

    /// **Primary read.** `GET /pr-merge/verdict/:owner/:name/:pr` →
    /// `compute_pr_merge_verdict`. The daemon's one-call "what is the train
    /// doing with this PR and what should I do". Returns the typed
    /// [`CoordCallError`] so the caller can tell an auth/HTTP failure (coord
    /// reachable) from genuine unreachability — the CoordDown discriminator.
    pub async fn verdict(
        &self,
        owner: &str,
        repo: &str,
        pr: u64,
    ) -> Result<PrMergeVerdict, CoordCallError> {
        let url = format!("{}/pr-merge/verdict/{owner}/{repo}/{pr}", self.base);
        self.get_json(&url).await
    }

    /// Fallback + evidence: distill the newest `predicate_eval` block +
    /// `unlandable_cycle` counter from `GET /pr-merge/events/:owner/:name/:pr`.
    pub async fn latest_block(
        &self,
        owner: &str,
        repo: &str,
        pr: u64,
    ) -> Result<BlockDiagnosis, String> {
        let url = format!("{}/pr-merge/events/{owner}/{repo}/{pr}", self.base);
        let wire: EventsResponseWire = self.get_json(&url).await.map_err(|e| e.to_string())?;
        Ok(block_from_events(&wire.events))
    }

    /// `GET /pr-merge/graph?repo&pr` → cycle detection + member readiness.
    pub async fn graph(&self, repo: &str, pr: u64) -> Result<GraphDiagnosis, String> {
        let url = format!("{}/pr-merge/graph", self.base);
        let pr_s = pr.to_string();
        let resp = self
            .auth(self.client.get(&url))
            .query(&[("repo", repo), ("pr", pr_s.as_str())])
            .send()
            .await
            .map_err(|e| format!("GET {url}: {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(format!("GET {url}: HTTP {status}"));
        }
        let wire: GraphResponseWire = resp
            .json()
            .await
            .map_err(|e| format!("GET {url}: decode: {e}"))?;
        Ok(graph_from_wire(&wire))
    }

    /// `GET /merge/queue` → is a proposal enqueued whose head matches `head`?
    pub async fn queue_has(&self, head_sha: &str) -> Result<bool, String> {
        let url = format!("{}/merge/queue", self.base);
        let wire: QueueResponseWire = self.get_json(&url).await.map_err(|e| e.to_string())?;
        Ok(queue_has_head(&wire, head_sha))
    }

    /// `GET /pr-merge/health` → is coord's leader lease fresh? A stale/absent
    /// lease is leaderless (a `CoordDown` signal).
    pub async fn health_leader(&self) -> Result<bool, String> {
        let url = format!("{}/pr-merge/health", self.base);
        let wire: HealthResponseWire = self.get_json(&url).await.map_err(|e| e.to_string())?;
        Ok(wire.leader.and_then(|l| l.lease_fresh).unwrap_or(false))
    }

    /// **Phase 5.4 coexistence rung (i).** Is coord's Unlandable Shepherd
    /// *currently* attempting this exact `(pr, head)`? Coord writes an
    /// `unlandable_cycle` (and a `diagnosis`) `pr_events` row on every shepherd
    /// sweep tick, so a RECENT such event on the current head is the strongest
    /// signal available over the read surface that coord's shepherd is alive and
    /// engaged. When coord is the broken component (leaderless / wedged) no
    /// fresh events appear → returns `false` → the runner proceeds as the
    /// last-resort rung. Best-effort: any transport failure → `Ok(false)` (we do
    /// not want an events blip to suppress the last-resort remediation).
    pub async fn unlandable_attempt_live(
        &self,
        owner: &str,
        repo: &str,
        pr: u64,
        head_sha: &str,
        within: Duration,
    ) -> Result<bool, String> {
        let url = format!("{}/pr-merge/events/{owner}/{repo}/{pr}", self.base);
        let wire: EventsResponseWire = self.get_json(&url).await.map_err(|e| e.to_string())?;
        Ok(coord_attempt_live(
            &wire.events,
            head_sha,
            within,
            Utc::now(),
        ))
    }

    /// `POST /pr-merge/prs/:owner/:name/:pr/reevaluate` — the cheap idempotent
    /// nudge tried once before escalating a suspected defect. Returns `Ok` on
    /// any 2xx.
    pub async fn reevaluate(&self, owner: &str, repo: &str, pr: u64) -> Result<(), String> {
        let url = format!("{}/pr-merge/prs/{owner}/{repo}/{pr}/reevaluate", self.base);
        let resp = self
            .auth(self.client.post(&url))
            .send()
            .await
            .map_err(|e| format!("POST {url}: {e}"))?;
        let status = resp.status();
        if status.is_success() {
            Ok(())
        } else {
            Err(format!("POST {url}: HTTP {status}"))
        }
    }
}

// ---------------------------------------------------------------------------
// Pure distillers over the wire shapes (unit-tested from recorded JSON).
// ---------------------------------------------------------------------------

/// Distill a [`BlockDiagnosis`] from a newest-first `pr_events` list: the newest
/// `predicate_eval` supplies the block code / detail / hydrated head, and the
/// newest `unlandable_cycle` supplies the escalation counter.
fn block_from_events(events: &[PrEventWire]) -> BlockDiagnosis {
    let mut out = BlockDiagnosis::default();
    let mut have_eval = false;
    let mut have_cycle = false;
    for ev in events {
        match ev.event_kind.as_str() {
            "predicate_eval" if !have_eval => {
                have_eval = true;
                if let Some(p) = &ev.payload {
                    out.code = p
                        .get("block_reason_code")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    out.detail = p.get("detail").cloned().unwrap_or(Value::Null);
                    let head = p
                        .get("head_sha")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    out.head_sha = head.clone();
                    out.hydration_head = head;
                }
            }
            "unlandable_cycle" if !have_cycle => {
                have_cycle = true;
                if let Some(p) = &ev.payload {
                    out.unlandable_cycles = p.get("cycles").and_then(Value::as_u64).or_else(|| {
                        p.get("cycles")
                            .and_then(Value::as_i64)
                            .map(|n| n.max(0) as u64)
                    });
                }
            }
            _ => {}
        }
        if have_eval && have_cycle {
            break;
        }
    }
    out
}

/// Distill a [`GraphDiagnosis`] from the graph response — cycle detection plus
/// whether every cycle member is `ready` in the node list.
fn graph_from_wire(wire: &GraphResponseWire) -> GraphDiagnosis {
    let all_green = !wire.cycle_members.is_empty()
        && wire.cycle_members.iter().all(|m| {
            wire.nodes
                .iter()
                .find(|n| n.repo == m.repo && n.pr_number == m.pr)
                .map(|n| n.ready)
                .unwrap_or(false)
        });
    GraphDiagnosis {
        cycle_detected: wire.cycle_detected,
        cycle_member_count: wire.cycle_members.len(),
        cycle_members_all_green: all_green,
    }
}

/// PURE core of [`CoordPrMergeClient::unlandable_attempt_live`]: is there a
/// `unlandable_cycle` or `diagnosis` event on `head_sha` newer than `now -
/// within`? An event with no `created_at` is treated as NOT recent (we can't
/// prove liveness), and one with no `head_sha` in its payload matches only if
/// the current head is empty (never, in the shepherd path). Unit-tested from
/// recorded event JSON.
fn coord_attempt_live(
    events: &[PrEventWire],
    head_sha: &str,
    within: Duration,
    now: DateTime<Utc>,
) -> bool {
    if head_sha.is_empty() {
        return false;
    }
    let cutoff =
        now - chrono::Duration::from_std(within).unwrap_or_else(|_| chrono::Duration::zero());
    events.iter().any(|ev| {
        if !matches!(ev.event_kind.as_str(), "unlandable_cycle" | "diagnosis") {
            return false;
        }
        let Some(created) = ev.created_at else {
            return false;
        };
        if created < cutoff {
            return false;
        }
        // Match the event's head to the current head when the payload carries
        // one; a shepherd sweep records the head it evaluated.
        ev.payload
            .as_ref()
            .and_then(|p| p.get("head_sha").and_then(Value::as_str))
            .map(|h| h == head_sha)
            .unwrap_or(false)
    })
}

/// Does any enqueued proposal carry a repo head matching `head_sha`?
fn queue_has_head(wire: &QueueResponseWire, head_sha: &str) -> bool {
    if head_sha.is_empty() {
        return false;
    }
    wire.proposals
        .iter()
        .any(|p| p.repos.iter().any(|r| r.head_sha == head_sha))
}

// ---------------------------------------------------------------------------
// Reevaluate once-guard — one nudge per (pr, head), process-global.
// ---------------------------------------------------------------------------

/// PRs already nudged with a `reevaluate` this process, keyed on
/// `(pr_number, head_sha)`. A head change (a push) clears the block class and
/// makes the PR eligible for a fresh nudge — the plan's "head-stable identity"
/// rule: we key on the head so a re-parented PR gets one nudge per head, never
/// a nudge storm on the same head.
static REEVALUATE_ONCE: Lazy<Mutex<HashSet<(u64, String)>>> =
    Lazy::new(|| Mutex::new(HashSet::new()));

/// Has the single reevaluate nudge for `(pr, head)` already been SENT
/// (successfully)? Non-consuming peek — pair with [`mark_reevaluate_sent`].
///
/// The claim is recorded only AFTER a confirmed-successful POST (claim-after-
/// success): a transient reevaluate failure must leave the guard unclaimed so
/// the nudge — the documented cheap first rung — retries on the next poll
/// instead of being permanently burnt and falling through to Escalate.
pub fn reevaluate_already_sent(pr: u64, head_sha: &str) -> bool {
    let g = REEVALUATE_ONCE.lock().unwrap_or_else(|e| e.into_inner());
    g.contains(&(pr, head_sha.to_string()))
}

/// Record that the reevaluate nudge for `(pr, head)` was successfully sent.
/// Call ONLY after the POST returned 2xx (see [`reevaluate_already_sent`]).
pub fn mark_reevaluate_sent(pr: u64, head_sha: &str) {
    let mut g = REEVALUATE_ONCE.lock().unwrap_or_else(|e| e.into_inner());
    g.insert((pr, head_sha.to_string()));
}

/// Test-only: clear the once-guard so tests don't bleed into each other.
#[cfg(test)]
pub fn reset_reevaluate_guard_for_test() {
    REEVALUATE_ONCE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
}

/// Resolve the coord HTTP base (env `COORD_HTTP_URL` → active profile
/// `coord_url`, ws→http). `None` when nothing is configured — mirrors
/// `fleet::coord_http_base`'s deliberate no-localhost-fallback posture.
fn coord_http_base() -> Option<String> {
    match qontinui_runner_lib::profiles::resolve_coord_base() {
        qontinui_runner_lib::profiles::CoordBase::Configured(base) => Some(base),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // -- classify: one fixture per disposition / defect class ----------------

    fn green_defect_base() -> ClassificationInput {
        // A reachable, led coord with a green PR — the substrate every defect
        // fixture perturbs one axis of.
        ClassificationInput {
            github_green: true,
            coord_reachable: true,
            leader_healthy: true,
            aged_past_floor: true,
            ..Default::default()
        }
    }

    #[test]
    fn coord_down_when_unreachable_but_github_green() {
        let input = ClassificationInput {
            github_green: true,
            coord_reachable: false,
            leader_healthy: false,
            ..Default::default()
        };
        assert_eq!(classify(&input), ShepherdDisposition::CoordDown);
    }

    #[test]
    fn coord_down_when_leaderless_even_if_reachable() {
        let input = ClassificationInput {
            github_green: true,
            coord_reachable: true,
            leader_healthy: false,
            block_reason: Some("has-blocking-label".to_string()),
            ..Default::default()
        };
        // Leaderless dominates the has-blocking-label hold: a leaderless coord
        // can misreport the block, so CoordDown wins.
        assert_eq!(classify(&input), ShepherdDisposition::CoordDown);
    }

    #[test]
    fn not_coord_down_when_github_not_green() {
        // Without the green witness we cannot claim CoordDown (the PR may be
        // legitimately unmergeable); fall through to transient.
        let input = ClassificationInput {
            github_green: false,
            coord_reachable: false,
            leader_healthy: false,
            ..Default::default()
        };
        assert_eq!(classify(&input), ShepherdDisposition::Transient);
    }

    #[test]
    fn has_blocking_label_is_legitimate_hold_not_defect() {
        // A coord:blocked label is the OPERATOR's intended hold mechanism
        // (fleet convention: "To hold a PR, set coord:blocked"); coord's label
        // gate is functioning correctly. The right response is the
        // author-action nudge — never a coord-defect remediation plan/spawn.
        let input = ClassificationInput {
            block_reason: Some("has-blocking-label".to_string()),
            ..green_defect_base()
        };
        assert_eq!(classify(&input), ShepherdDisposition::LegitimateHold);
        assert_eq!(
            decide_action(classify(&input)),
            ShepherdIntent::NotifyAuthor {
                reason: "pr_hold_triage".to_string()
            }
        );
    }

    #[test]
    fn phantom_context_hang_needs_exceeded_dwell() {
        let base = ClassificationInput {
            block_reason: Some("merge-state-unsettled".to_string()),
            ..green_defect_base()
        };
        // Under the dwell multiple → not yet a defect (transient — GitHub may
        // still be recomputing merge-state).
        assert_eq!(
            classify(&ClassificationInput {
                merge_state_unsettled_exceeded: false,
                ..base.clone()
            }),
            ShepherdDisposition::Transient
        );
        // Past 2× the threshold → phantom-context hang.
        assert_eq!(
            classify(&ClassificationInput {
                merge_state_unsettled_exceeded: true,
                ..base
            }),
            ShepherdDisposition::CoordDefect {
                class: CoordDefectClass::PhantomContextHang
            }
        );
    }

    #[test]
    fn unlandable_cycle_spin_requires_cycle_green_and_over_threshold() {
        let base = ClassificationInput {
            cycle_detected: true,
            cycle_members_all_green: true,
            unlandable_cycles: UNLANDABLE_CYCLE_DEFECT_THRESHOLD + 1,
            ..green_defect_base()
        };
        assert_eq!(
            classify(&base),
            ShepherdDisposition::CoordDefect {
                class: CoordDefectClass::UnlandableCycleSpin
            }
        );
        // Exactly at threshold → not yet (strictly greater than).
        assert_eq!(
            classify(&ClassificationInput {
                unlandable_cycles: UNLANDABLE_CYCLE_DEFECT_THRESHOLD,
                ..base.clone()
            }),
            ShepherdDisposition::Transient
        );
        // Members not all green → churn, not the spin defect.
        assert_eq!(
            classify(&ClassificationInput {
                cycle_members_all_green: false,
                ..base
            }),
            ShepherdDisposition::Transient
        );
    }

    #[test]
    fn stale_ingest_is_defect() {
        let input = ClassificationInput {
            stale_ingest: true,
            ..green_defect_base()
        };
        assert_eq!(
            classify(&input),
            ShepherdDisposition::CoordDefect {
                class: CoordDefectClass::StaleIngest
            }
        );
    }

    #[test]
    fn pass_but_no_land_requires_pass_state_aged_and_no_queue() {
        let base = ClassificationInput {
            outer_state: Some("READY".to_string()),
            block_reason: Some("none".to_string()),
            aged_past_floor: true,
            in_queue: false,
            ..green_defect_base()
        };
        assert_eq!(
            classify(&base),
            ShepherdDisposition::CoordDefect {
                class: CoordDefectClass::PassButNoLand
            }
        );
        // Enqueued → the land IS scheduled → transient (in flight).
        assert_eq!(
            classify(&ClassificationInput {
                in_queue: true,
                ..base.clone()
            }),
            ShepherdDisposition::Transient
        );
        // Not yet aged past the floor → transient (in-flight latency).
        assert_eq!(
            classify(&ClassificationInput {
                aged_past_floor: false,
                ..base
            }),
            ShepherdDisposition::Transient
        );
    }

    #[test]
    fn legitimate_holds_map_to_hold() {
        for reason in [
            "has-blocking-label",
            "escalate-path-matched",
            "self-triggering-ci-gate",
            "ci-not-green",
            "main-red",
            "required-checks-missing",
            "uncoordinated-version-bump",
            "cargo-fmt-drift",
            "has-cross-repo-dependency",
            "auto-merge-disabled",
            "behind-main-or-unstable",
        ] {
            let input = ClassificationInput {
                block_reason: Some(reason.to_string()),
                ..green_defect_base()
            };
            assert_eq!(
                classify(&input),
                ShepherdDisposition::LegitimateHold,
                "block reason {reason:?} must be a legitimate hold"
            );
        }
    }

    #[test]
    fn escalated_and_ci_red_outer_states_are_holds() {
        for state in ["ESCALATED", "CI_RED_TRIAGE"] {
            let input = ClassificationInput {
                outer_state: Some(state.to_string()),
                ..green_defect_base()
            };
            assert_eq!(
                classify(&input),
                ShepherdDisposition::LegitimateHold,
                "{state}"
            );
        }
    }

    #[test]
    fn transient_states_do_nothing() {
        for state in ["CI_PENDING", "MERGING", "MERGED", "OPEN"] {
            let input = ClassificationInput {
                outer_state: Some(state.to_string()),
                aged_past_floor: false,
                ..green_defect_base()
            };
            assert_eq!(classify(&input), ShepherdDisposition::Transient, "{state}");
        }
    }

    // -- decide_action: pure disposition → intent ----------------------------

    #[test]
    fn decide_action_maps_every_disposition() {
        assert_eq!(
            decide_action(ShepherdDisposition::Transient),
            ShepherdIntent::Nothing
        );
        assert_eq!(
            decide_action(ShepherdDisposition::LegitimateHold),
            ShepherdIntent::NotifyAuthor {
                reason: "pr_hold_triage".to_string()
            }
        );
        assert_eq!(
            decide_action(ShepherdDisposition::CoordDefect {
                class: CoordDefectClass::UnlandableCycleSpin
            }),
            ShepherdIntent::Escalate {
                class: CoordDefectClass::UnlandableCycleSpin
            }
        );
        assert_eq!(
            decide_action(ShepherdDisposition::CoordDown),
            ShepherdIntent::CoordDownLocalAlert
        );
    }

    // -- distillers over recorded wire JSON ----------------------------------

    #[test]
    fn verdict_deserializes_from_coord_shape() {
        // A recorded escalate-path verdict payload (coord merge_verdict shape).
        let v: PrMergeVerdict = serde_json::from_value(json!({
            "repo": "qontinui/qontinui-coord",
            "pr_number": 42,
            "outer_state": "BLOCKED",
            "block_reason": "escalate-path-matched",
            "block_evidence": { "category": "migrations" },
            "rollout_state": "live",
            "escalate": {
                "disposition": "auto_if_provably_safe",
                "category": "migrations",
                "pending_gate": "migration_classifier",
                "reason": "op.drop_column(public.x) — destructive"
            },
            "next_action": "Migration classifier REJECTED the change.",
            "as_of": "2026-07-04T12:00:00Z"
        }))
        .expect("verdict deserializes");
        assert_eq!(v.outer_state.as_deref(), Some("BLOCKED"));
        assert_eq!(v.block_reason.as_deref(), Some("escalate-path-matched"));
        assert_eq!(
            v.escalate.as_ref().map(|e| e.pending_gate.as_str()),
            Some("migration_classifier")
        );
    }

    #[test]
    fn block_from_events_reads_newest_eval_and_cycle() {
        // Newest-first, as coord returns. The 2026-07-04 deadlock shape: a
        // has-blocking-label predicate_eval on a green head with a spinning
        // unlandable_cycle counter.
        let events = vec![
            PrEventWire {
                event_kind: "state_change".to_string(),
                payload: Some(json!({ "state": "BLOCKED" })),
                created_at: None,
            },
            PrEventWire {
                event_kind: "predicate_eval".to_string(),
                payload: Some(json!({
                    "block_reason_code": "has-blocking-label",
                    "head_sha": "abc123",
                    "detail": { "label": "coord:upstream-of=qontinui-coord#912" }
                })),
                created_at: None,
            },
            PrEventWire {
                event_kind: "unlandable_cycle".to_string(),
                payload: Some(json!({ "head_sha": "abc123", "cycles": 37 })),
                created_at: None,
            },
        ];
        let block = block_from_events(&events);
        assert_eq!(block.code.as_deref(), Some("has-blocking-label"));
        assert_eq!(block.head_sha.as_deref(), Some("abc123"));
        assert_eq!(block.hydration_head.as_deref(), Some("abc123"));
        assert_eq!(block.unlandable_cycles, Some(37));
    }

    #[test]
    fn graph_from_wire_computes_all_green_cycle() {
        let wire: GraphResponseWire = serde_json::from_value(json!({
            "nodes": [
                { "repo": "qontinui/qontinui-web", "pr_number": 708, "ready": true },
                { "repo": "qontinui/qontinui-coord", "pr_number": 912, "ready": true }
            ],
            "edges": [],
            "topo_order": [],
            "cycle_detected": true,
            "cycle_members": [
                { "repo": "qontinui/qontinui-web", "pr": 708 },
                { "repo": "qontinui/qontinui-coord", "pr": 912 }
            ]
        }))
        .unwrap();
        let g = graph_from_wire(&wire);
        assert!(g.cycle_detected);
        assert_eq!(g.cycle_member_count, 2);
        assert!(g.cycle_members_all_green, "both members ready → all green");

        // Flip one member to not-ready → not all green.
        let wire2: GraphResponseWire = serde_json::from_value(json!({
            "nodes": [
                { "repo": "a", "pr_number": 1, "ready": true },
                { "repo": "b", "pr_number": 2, "ready": false }
            ],
            "cycle_detected": true,
            "cycle_members": [
                { "repo": "a", "pr": 1 },
                { "repo": "b", "pr": 2 }
            ]
        }))
        .unwrap();
        assert!(!graph_from_wire(&wire2).cycle_members_all_green);
    }

    #[test]
    fn coord_attempt_live_detects_recent_cycle_on_current_head() {
        let now = Utc::now();
        let within = Duration::from_secs(24 * 3600);
        // A recent unlandable_cycle on the current head → coord shepherd alive.
        let events = vec![PrEventWire {
            event_kind: "unlandable_cycle".to_string(),
            payload: Some(json!({ "head_sha": "abc123", "cycles": 4 })),
            created_at: Some(now - chrono::Duration::minutes(10)),
        }];
        assert!(coord_attempt_live(&events, "abc123", within, now));

        // Same event but on a DIFFERENT head → not our head → not live for us.
        assert!(!coord_attempt_live(&events, "NEWHEAD", within, now));

        // A stale event (older than the window) → not live.
        let stale = vec![PrEventWire {
            event_kind: "diagnosis".to_string(),
            payload: Some(json!({ "head_sha": "abc123" })),
            created_at: Some(now - chrono::Duration::hours(48)),
        }];
        assert!(!coord_attempt_live(&stale, "abc123", within, now));

        // An event with no created_at can't prove liveness → not live.
        let undated = vec![PrEventWire {
            event_kind: "unlandable_cycle".to_string(),
            payload: Some(json!({ "head_sha": "abc123" })),
            created_at: None,
        }];
        assert!(!coord_attempt_live(&undated, "abc123", within, now));

        // A recent but irrelevant event kind (predicate_eval) → not an attempt.
        let other = vec![PrEventWire {
            event_kind: "predicate_eval".to_string(),
            payload: Some(json!({ "head_sha": "abc123" })),
            created_at: Some(now),
        }];
        assert!(!coord_attempt_live(&other, "abc123", within, now));

        // Empty current head never matches.
        assert!(!coord_attempt_live(&events, "", within, now));
    }

    #[test]
    fn queue_has_head_matches_repo_head() {
        let wire: QueueResponseWire = serde_json::from_value(json!({
            "proposals": [
                { "repos": [ { "repo": "qontinui/qontinui-web", "head_sha": "deadbeef" } ] }
            ]
        }))
        .unwrap();
        assert!(queue_has_head(&wire, "deadbeef"));
        assert!(!queue_has_head(&wire, "cafef00d"));
        assert!(!queue_has_head(&wire, ""), "empty head never matches");
    }

    // -- end-to-end: the 2026-07-04 deadlock, now a FIXED/historical fixture --

    #[test]
    fn deadlock_2026_07_04_classifies_as_unlandable_cycle_spin() {
        // The motivating incident: qontinui-web#708 ↔ qontinui-coord#912 both
        // green, wedged ~20h while coord counted 37+ unlandable cycles. The
        // upstream-of INVERSION that caused it is fixed on coord main (#954),
        // so we keep the payload only as the cycle-spin classification fixture.
        let verdict = PrMergeVerdict {
            outer_state: Some("BLOCKED".to_string()),
            block_reason: Some("has-cross-repo-dependency".to_string()),
            block_evidence: json!({}),
            rollout_state: Some("live".to_string()),
            escalate: None,
            next_action: "PR is BLOCKED on `has-cross-repo-dependency`.".to_string(),
            as_of: None,
        };
        let block = BlockDiagnosis {
            code: Some("has-cross-repo-dependency".to_string()),
            unlandable_cycles: Some(37),
            ..Default::default()
        };
        let graph = GraphDiagnosis {
            cycle_detected: true,
            cycle_member_count: 2,
            cycle_members_all_green: true,
        };
        let ev = Evidence {
            github_green: true,
            coord_reachable: true,
            verdict: Some(&verdict),
            leader_fresh: Some(true),
            block: Some(&block),
            graph: Some(&graph),
            in_queue: false,
            current_head: "abc123",
            green_for: Duration::from_secs(20 * 3600),
            green_threshold: Duration::from_secs(45 * 60),
        };
        let input = build_classification_input(&ev);
        // The cycle-spin defect is detected BEFORE the has-cross-repo-dependency
        // legitimate-hold branch (defect classes are checked first), which is
        // correct: a green cycle spinning 37× is a defect, not a legit hold.
        assert_eq!(
            classify(&input),
            ShepherdDisposition::CoordDefect {
                class: CoordDefectClass::UnlandableCycleSpin
            }
        );
        assert_eq!(
            decide_action(classify(&input)),
            ShepherdIntent::Escalate {
                class: CoordDefectClass::UnlandableCycleSpin
            }
        );
    }

    #[test]
    fn health_blip_with_reachable_verdict_does_not_force_coord_down() {
        // Verdict reachable (coord up) but the /pr-merge/health call itself
        // failed (leader_fresh None). A green PR hung on merge-state-unsettled
        // past 2× the threshold must still classify as the PhantomContextHang
        // defect — NOT CoordDown — so a health hiccup can't mask the verdict's
        // real signal.
        let verdict = PrMergeVerdict {
            outer_state: Some("BLOCKED".to_string()),
            block_reason: Some("merge-state-unsettled".to_string()),
            block_evidence: json!({}),
            rollout_state: Some("live".to_string()),
            escalate: None,
            next_action: String::new(),
            as_of: None,
        };
        let ev = Evidence {
            github_green: true,
            coord_reachable: true,
            verdict: Some(&verdict),
            leader_fresh: None, // health call blipped
            block: None,
            graph: None,
            in_queue: false,
            current_head: "abc123",
            green_for: Duration::from_secs(3 * 3600),
            green_threshold: Duration::from_secs(45 * 60),
        };
        let input = build_classification_input(&ev);
        assert_eq!(
            classify(&input),
            ShepherdDisposition::CoordDefect {
                class: CoordDefectClass::PhantomContextHang
            }
        );

        // But an EXPLICIT leaderless report (Some(false)) DOES trip CoordDown.
        let ev_leaderless = Evidence {
            leader_fresh: Some(false),
            ..ev
        };
        assert_eq!(
            classify(&build_classification_input(&ev_leaderless)),
            ShepherdDisposition::CoordDown
        );
    }

    #[test]
    fn coord_down_end_to_end_when_verdict_absent() {
        // Verdict call failed at the TRANSPORT level (coord unreachable) but
        // the runner witnesses green.
        let ev = Evidence {
            github_green: true,
            coord_reachable: false,
            verdict: None,
            leader_fresh: None,
            block: None,
            graph: None,
            in_queue: false,
            current_head: "abc123",
            green_for: Duration::from_secs(3 * 3600),
            green_threshold: Duration::from_secs(45 * 60),
        };
        let input = build_classification_input(&ev);
        assert_eq!(classify(&input), ShepherdDisposition::CoordDown);
    }

    #[test]
    fn auth_failure_is_not_coord_down() {
        // The runner's own bearer expired: coord answered 401 — verdict body
        // absent but coord REACHABLE. Must classify Transient, never CoordDown
        // (no COORD-DOWN alert, no coord-down injection, no orphaned-red spawn
        // while coord is healthy and only our credentials are broken).
        let ev = Evidence {
            github_green: true,
            coord_reachable: true, // 401/403 IS an answer from coord
            verdict: None,         // ...but no verdict body
            leader_fresh: None,
            block: None,
            graph: None,
            in_queue: false,
            current_head: "abc123",
            green_for: Duration::from_secs(3 * 3600),
            green_threshold: Duration::from_secs(45 * 60),
        };
        let input = build_classification_input(&ev);
        assert!(input.coord_reachable);
        assert_eq!(classify(&input), ShepherdDisposition::Transient);
    }

    #[test]
    fn coord_call_error_distinguishes_auth_and_reachability() {
        let auth = CoordCallError::Http {
            status: 401,
            detail: "GET /pr-merge/verdict".to_string(),
        };
        assert!(auth.is_auth());
        assert!(auth.coord_reachable());
        let forbidden = CoordCallError::Http {
            status: 403,
            detail: String::new(),
        };
        assert!(forbidden.is_auth());
        let server_err = CoordCallError::Http {
            status: 500,
            detail: String::new(),
        };
        assert!(!server_err.is_auth());
        assert!(
            server_err.coord_reachable(),
            "any HTTP response = reachable"
        );
        let down = CoordCallError::Unreachable("connect refused".to_string());
        assert!(!down.is_auth());
        assert!(!down.coord_reachable());
    }

    #[test]
    fn reevaluate_guard_claims_only_after_success() {
        reset_reevaluate_guard_for_test();
        // Peek is non-consuming: a failed POST leaves the nudge retryable.
        assert!(!reevaluate_already_sent(708, "headA"));
        assert!(
            !reevaluate_already_sent(708, "headA"),
            "peek must not consume — a POST failure retries next poll"
        );
        // Mark only after a confirmed-successful POST.
        mark_reevaluate_sent(708, "headA");
        assert!(reevaluate_already_sent(708, "headA"), "sent once — dedup");
        // A new head (a push) is eligible for a fresh nudge.
        assert!(
            !reevaluate_already_sent(708, "headB"),
            "new head is a fresh nudge"
        );
        // A different PR is independent.
        assert!(
            !reevaluate_already_sent(912, "headA"),
            "different PR independent"
        );
    }

    #[test]
    fn build_input_flags_stale_ingest_on_head_mismatch() {
        let verdict = PrMergeVerdict {
            outer_state: Some("BLOCKED".to_string()),
            block_reason: Some("behind-main-or-unstable".to_string()),
            block_evidence: json!({}),
            rollout_state: Some("live".to_string()),
            escalate: None,
            next_action: String::new(),
            as_of: None,
        };
        let block = BlockDiagnosis {
            code: Some("behind-main-or-unstable".to_string()),
            head_sha: Some("OLDHEAD".to_string()),
            hydration_head: Some("OLDHEAD".to_string()),
            ..Default::default()
        };
        let ev = Evidence {
            github_green: true,
            coord_reachable: true,
            verdict: Some(&verdict),
            leader_fresh: Some(true),
            block: Some(&block),
            graph: None,
            in_queue: false,
            current_head: "NEWHEAD",
            green_for: Duration::from_secs(2 * 3600),
            green_threshold: Duration::from_secs(45 * 60),
        };
        let input = build_classification_input(&ev);
        assert!(
            input.stale_ingest,
            "coord head OLDHEAD ≠ real NEWHEAD, aged > 1h"
        );
        assert_eq!(
            classify(&input),
            ShepherdDisposition::CoordDefect {
                class: CoordDefectClass::StaleIngest
            }
        );
    }
}
