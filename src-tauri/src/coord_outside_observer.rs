//! An OUTSIDE observer of coord's own liveness — the one fault class coord's
//! pager cannot report about itself.
//!
//! Plan `2026-09-12-merge-train-alerts-page-a-reader-and-act-on-nothing`
//! Phase 3b. Its §2 argues the split: a dead singleton under a LIVE leader is
//! visible to that leader's own pager (Phase 3a, in coord); **no leader at
//! all, or no coord at all, is visible only from outside**, and the runner is
//! the fleet's only outside party that already holds a credential
//! (`mcp::device_jwt_refresher` keeps the slot fresh), a cadence
//! (`session::coord_sync`'s heartbeat loop) and an operator-facing surface.
//!
//! # The boundary this module is written against
//!
//! The observer lives **inside the runner process**, never the dev-only
//! supervisor on `:9875` (`webview_recovery`'s module doc; the plan's §2
//! "Boundary honoured"). A user has no supervisor, so a detection that
//! depends on one ships nothing to a user.
//!
//! It **reads and surfaces; it never writes into a coord that may be the
//! thing that is broken** (plan D-F). The one coord write it makes — a
//! best-effort `POST /coord/agent-findings` — is gated on the read having
//! SUCCEEDED on that same cycle, so it is only ever issued to a coord that
//! just answered.
//!
//! It **cannot restart coord and must not try**: served policy
//! `production-and-cost` `runner-lifecycle` and the supervisor boundary both
//! forbid a user-facing recovery path that depends on anything outside the
//! runner, and coord's own recovery is ECS's. What shrinks here is the
//! fault-to-visibility interval, nothing else.
//!
//! # The read
//!
//! One `tools/call` for `coord_query_workers` over the SAME door
//! [`qontinui_runner_lib::coord_doctor::coord_reachable_check`] probes —
//! `profiles::coord_base_with_source()` + `/mcp`, resolved once by
//! [`qontinui_runner_lib::coord_doctor::resolve_coord_mcp_door`] so the two
//! callers can never disagree about the upstream or its source. The doctor
//! selects its bearer RAW because it reports on the credential chain; this
//! module is an ordinary data-plane caller and routes through
//! [`crate::auth::attach_device_auth_for`], which presents the same slot and
//! counts the call.
//!
//! # The three predicates
//!
//! | # | fires on | debounce |
//! |---|---|---|
//! | (i) [`FaultClass::Unreachable`] | coord did not answer, or answered non-2xx | 3 consecutive probes |
//! | (ii) [`FaultClass::WorkerDead`] | a leader-gated worker rolls up `dead` | first observation, per worker |
//! | (iii) [`FaultClass::NoLeader`] | a leader-gated worker reports `no_leader_tick` — NO live replica is running it | 3 consecutive probes |
//!
//! (ii) carries no cadence requirement because `dead` is already a debounced
//! verdict: coord's ledger only reaches it after >10 tick intervals of
//! silence. (i) and (iii) are single-sample facts and get the plan's
//! `≥ 3 cadences`.
//!
//! # What an UNKNOWN must never become
//!
//! [`ProbeOutcome::Unusable`] is the third arm, and it exists so an answer
//! this runner cannot READ is never rendered as either a healthy fleet or a
//! dead coord (served policy `verification-and-evidence`
//! `unknown-must-not-render-as-a-default`). It covers a 401/403 — which
//! `coord_reachable_check`'s own arms record as *"coord is up; this is a
//! credential fault"* — a JSON-RPC error, an `isError` tool result, a body
//! shape this build cannot parse, and coord's own honest-unknown verdict
//! (`drift_subclass: workers:no_observation`, which it returns for a
//! pre-migration or zero-row ledger and which is explicitly NOT a clean bill
//! of health). An `Unusable` answers (i) in the negative — coord spoke — and
//! says nothing at all about (ii) or (iii), so it resets the unreachable
//! streak and leaves the other two untouched rather than clearing them.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{json, Value as JsonValue};
use tracing::{debug, error, info, warn};

use crate::auth::TenantScope;

/// Per-machine kill switch. Exactly `"0"` disables the observer; absent,
/// `"1"`, or anything else leaves it on (`engineering-priorities`
/// `capability-ships-enabled` — the capability ships enabled and the flag is a
/// way OUT, never the authorization). Read once, at construction.
pub const OBSERVER_FLAG: &str = "QONTINUI_COORD_OUTSIDE_OBSERVER";

/// How many consecutive probes a single-sample predicate must hold before it
/// is reported. The plan's `≥ 3 cadences`; at the ~60 s probe cadence that is
/// ~3 minutes of continuous evidence.
pub const CADENCES_TO_FIRE: u32 = 3;

/// Target probe period. The host loop divides its own heartbeat cadence into
/// this to pick N, so the probe stays at about one per minute however
/// `QONTINUI_SESSION_HEARTBEAT_SECS` is tuned.
pub const PROBE_PERIOD_SECS: u64 = 60;

/// Per-probe HTTP timeout. Generous against coord's measured tail (the
/// runner's `/health` has been sampled past 10 s on a loaded box) and still
/// far under the probe period, so a slow door can never let two probes
/// overlap even without the in-flight latch.
const PROBE_TIMEOUT_SECS: u64 = 20;

/// How often an `Unusable` streak re-warns, in probes. The first one always
/// warns; after that roughly hourly, so a runner whose credential has gone
/// dark says so without burying the log.
const UNUSABLE_REWARN_EVERY: u32 = 60;

/// The coord tool this observer calls. Swapped in for the doctor's
/// `tools/list` — same door, a question with an answer in it.
const WORKERS_TOOL: &str = "coord_query_workers";

/// Topic every finding this module posts is filed under (plan Phase 3b.2).
const FINDING_TOPIC: &str = "coord-merge-train";

/// The honest-unknown subclass coord returns for a ledger it cannot read
/// (pre-migration, or a replica that has never observed a row). Never a clean
/// fleet — see the module doc.
const NO_OBSERVATION_SUBCLASS: &str = "workers:no_observation";

// ---------------------------------------------------------------------------
// Fault classes
// ---------------------------------------------------------------------------

/// The three classes this observer can report. Each maps to one notification
/// title, one stable `wedge-incidents.log` token and one `error_code`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FaultClass {
    /// (i) coord is not answering this runner at all.
    Unreachable,
    /// (ii) a leader-gated singleton has rolled up `dead`.
    WorkerDead,
    /// (iii) no replica reports itself leader, so nothing is running the
    /// leader-gated plane.
    NoLeader,
}

impl FaultClass {
    /// The greppable token written into `wedge-incidents.log`, in the same
    /// vocabulary as `health_monitor`'s `backend_wedged` / `ui_thread_wedged`.
    pub fn breadcrumb_reason(self) -> &'static str {
        match self {
            FaultClass::Unreachable => "coord_unreachable",
            FaultClass::WorkerDead => "coord_worker_dead",
            FaultClass::NoLeader => "coord_no_leader",
        }
    }

    /// The notification title — "titled for the class", per the plan.
    pub fn title(self) -> &'static str {
        match self {
            FaultClass::Unreachable => "Coord is not answering this runner",
            FaultClass::WorkerDead => "A coord leader-gated worker is dead",
            FaultClass::NoLeader => "Coord has no leader",
        }
    }

    /// Stable code the operator card prints and a log reader greps.
    pub fn error_code(self) -> &'static str {
        match self {
            FaultClass::Unreachable => "COORD_LIVENESS_UNREACHABLE",
            FaultClass::WorkerDead => "COORD_LIVENESS_WORKER_DEAD",
            FaultClass::NoLeader => "COORD_LIVENESS_NO_LEADER",
        }
    }

    /// What the operator can actually do. Deliberately never "restart coord":
    /// this runner has no lever on coord and must not pretend to one.
    pub fn suggested_action(self) -> &'static str {
        match self {
            FaultClass::Unreachable => {
                "Automation on this runner is unaffected — only coord coordination is. \
                 Check https://coord.qontinui.io/health from another box before \
                 concluding coord is down; a 401/403 would have been reported as a \
                 credential fault instead, so this is a transport or availability \
                 failure. Coord's own recovery is ECS's, not this runner's."
            }
            FaultClass::WorkerDead => {
                "A coord background singleton has stopped ticking, so whatever it \
                 drives (merge dispatch, gate sweeps, alert page-out) is stalled. \
                 Read `coord_query_workers` with {\"name\": \"<worker>\"} for the \
                 per-replica rows, and check `live_status` beside `status` before \
                 treating it as a real outage."
            }
            FaultClass::NoLeader => {
                "No coord replica is running the leader-gated plane, so every \
                 detector, pager and gate sweep in coord is idle. Check coord's \
                 leader lease and replica presence; nothing on this runner can \
                 elect one."
            }
        }
    }
}

// ---------------------------------------------------------------------------
// What one probe established
// ---------------------------------------------------------------------------

/// One leader-gated worker coord reported non-nominal.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkerVerdict {
    pub name: String,
    pub status: String,
    /// The SAME worst-of computed over still-writing replicas only. `None` is
    /// UNKNOWN (nothing still writing), never "fine" — coord's own tool
    /// description is emphatic about this, so it is carried into the
    /// notification rather than collapsed.
    pub live_status: Option<String>,
    pub reason: Option<String>,
    pub worst_replica_age_secs: Option<f64>,
    pub verdict_from_rolled_off_replica: Option<bool>,
    pub last_decision_code: Option<String>,
}

/// A successful read of coord's worker ledger.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LedgerRead {
    /// Leader-gated workers that rolled up `dead` and survived the rolled-off
    /// discrimination — predicate (ii)'s population.
    pub dead_leader_gated: Vec<WorkerVerdict>,
    /// Leader-gated workers no live replica is running (`reason:
    /// no_leader_tick`) — predicate (iii)'s population.
    pub leaderless: Vec<WorkerVerdict>,
    /// `components.counts.dead`, verbatim, for the report body.
    pub counts_dead: u64,
    /// Leader-gated `dead` rows EXCLUDED because coord could prove the verdict
    /// came from a replica a deploy had already replaced. Reported in the body
    /// so an excluded row is visible rather than silently dropped.
    pub rolled_off_excluded: Vec<String>,
    /// coord's own `components.non_nominal_workers_truncated`.
    ///
    /// The list is capped at `QUERY_WORKERS_MAX_LISTED` (40) by
    /// `qontinui-coord` `mcp::tools`' no-arg arm, and that `take(40)` runs
    /// over the rollup's order with NO priority sort — so a truncated list can
    /// drop a dead leader-gated worker that `counts.dead` still counts. The
    /// counts are over the full set; only the list is capped. Carried rather
    /// than ignored so a `counts_dead` this runner could not account for reads
    /// as UNKNOWN instead of as a clean fleet (served policy
    /// `verification-and-evidence` `unknown-must-not-render-as-a-default`).
    /// Latent today — 19 workers against a cap of 40 — which is exactly when
    /// it is cheap to get right.
    pub list_truncated: bool,
}

impl LedgerRead {
    /// `counts.dead` this read could not name: coord counted more dead
    /// workers than the (possibly truncated) list let this runner classify.
    ///
    /// Saturating, and NOT an error on its own — `counts.dead` is over every
    /// worker while `dead_leader_gated` is over the leader-gated survivors of
    /// the rolled-off discrimination, so a non-zero value is ordinary on an
    /// untruncated list. It is only evidence of a BLIND SPOT when
    /// [`Self::list_truncated`] is also true.
    pub fn dead_unaccounted(&self) -> u64 {
        self.counts_dead
            .saturating_sub(self.dead_leader_gated.len() as u64)
            .saturating_sub(self.rolled_off_excluded.len() as u64)
    }

    /// True when coord counted dead workers this runner could not see,
    /// BECAUSE the list was truncated. The one shape that would otherwise
    /// render a real outage as silence.
    pub fn dead_is_underread(&self) -> bool {
        self.list_truncated && self.dead_unaccounted() > 0
    }
}

/// The outcome of one probe. Three arms, not two — see the module doc on why
/// `Unusable` cannot be folded into either neighbour.
#[derive(Debug, Clone, PartialEq)]
pub enum ProbeOutcome {
    /// coord answered and this runner read a worker ledger out of it.
    Read(Box<LedgerRead>),
    /// coord did not answer, or answered a non-2xx that is not a credential
    /// refusal. Predicate (i)'s only input.
    Unreachable { reason: String },
    /// coord ANSWERED but this runner could not read a ledger out of it.
    /// Evidence that coord is up, and no evidence at all about (ii)/(iii).
    Unusable { reason: String },
}

// ---------------------------------------------------------------------------
// Pure: MCP envelope -> ProbeOutcome
// ---------------------------------------------------------------------------

/// Unwrap a JSON-RPC `tools/call` response into the tool's own JSON value.
///
/// coord's `/mcp` serializes a successful tool result as
/// `result.content[0].text` holding the tool's JSON as a STRING
/// (`qontinui-coord` `mcp::mod`'s `rpc_ok` arm), and a tool-level failure as
/// the same shape with `isError: true`. Both are 200s, so the status code
/// alone cannot tell them apart — which is why this returns a typed error
/// rather than an `Option`.
pub fn unwrap_tools_call(rpc: &JsonValue) -> Result<JsonValue, String> {
    // `.filter(|v| !v.is_null())` is load-bearing: some JSON-RPC serializers
    // emit `"error": null` beside a real `result`, and treating a present-
    // but-null key as a failure would turn every successful read into an
    // UNKNOWN.
    if let Some(err) = rpc.get("error").filter(|v| !v.is_null()) {
        let msg = err
            .get("message")
            .and_then(JsonValue::as_str)
            .unwrap_or("(no message)");
        let code = err.get("code").and_then(JsonValue::as_i64);
        return Err(match code {
            Some(c) => format!("coord /mcp returned a JSON-RPC error {c}: {msg}"),
            None => format!("coord /mcp returned a JSON-RPC error: {msg}"),
        });
    }
    let result = rpc
        .get("result")
        .ok_or_else(|| "coord /mcp response carried neither `result` nor `error`".to_string())?;
    let text = result
        .get("content")
        .and_then(JsonValue::as_array)
        .and_then(|c| c.first())
        .and_then(|c| c.get("text"))
        .and_then(JsonValue::as_str)
        .ok_or_else(|| "coord /mcp result carried no `content[0].text`".to_string())?;
    if result.get("isError").and_then(JsonValue::as_bool) == Some(true) {
        // The tool refused. Masked from this principal, a bad argument, or a
        // store it could not read — all of them mean "coord is up and this
        // read did not happen", never "coord is down".
        return Err(format!(
            "coord refused the {WORKERS_TOOL} call: {}",
            truncate(text, 400)
        ));
    }
    serde_json::from_str::<JsonValue>(text)
        .map_err(|e| format!("coord /mcp result text is not JSON: {e}"))
}

/// Classify a `coord_query_workers` no-arg response body.
///
/// Pure over the tool's own JSON so every predicate is unit-testable against a
/// canned body with no runtime, no socket and no credential.
pub fn classify_ledger(tool: &JsonValue) -> ProbeOutcome {
    let subclass = tool.get("drift_subclass").and_then(JsonValue::as_str);
    if subclass == Some(NO_OBSERVATION_SUBCLASS) {
        let note = tool
            .pointer("/components/note")
            .and_then(JsonValue::as_str)
            .unwrap_or("coord could not observe its own worker ledger");
        return ProbeOutcome::Unusable {
            reason: format!(
                "coord answered with its honest-unknown verdict ({NO_OBSERVATION_SUBCLASS}): \
                 {note}. That is a coverage gap, NOT a clean worker fleet."
            ),
        };
    }
    let Some(counts) = tool.pointer("/components/counts") else {
        return ProbeOutcome::Unusable {
            reason: format!(
                "coord answered but the {WORKERS_TOOL} body carried no `components.counts` — \
                 this runner build cannot read this response shape"
            ),
        };
    };
    let counts_dead = counts.get("dead").and_then(JsonValue::as_u64).unwrap_or(0);

    let listed = tool
        .pointer("/components/non_nominal_workers")
        .and_then(JsonValue::as_array)
        .cloned()
        .unwrap_or_default();

    let mut read = LedgerRead {
        counts_dead,
        list_truncated: tool
            .pointer("/components/non_nominal_workers_truncated")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false),
        ..LedgerRead::default()
    };
    for row in &listed {
        // Leader-gated only. A follower-plane worker's death is not the class
        // this observer exists for, and coord's own pager can see it.
        if row.get("leader_gated").and_then(JsonValue::as_bool) != Some(true) {
            continue;
        }
        let verdict = worker_verdict(row);
        // Cloned rather than matched in place: the arms MOVE `verdict`, and a
        // match on `verdict.status.as_str()` would hold a borrow of it for
        // the whole match.
        let status = verdict.status.clone();
        match status.as_str() {
            "dead" => {
                // The rolled-off discrimination Phase 3a applies, applied here
                // from the field coord computes for exactly this purpose.
                //
                // NOTE, recorded rather than silently improved: coord's own
                // `WorkerRollup::worst_replica_rolled_off` doc records that
                // this flag FLAPS across adjacent samples (coord finding
                // `adac6331-724c-4a5f-be20-e21672d21b5d`), and that
                // `live_status` is the stable second verdict. The plan names
                // `verdict_from_rolled_off_replica` as THE discriminator, so
                // that is what gates here; `live_status` travels in the
                // notification body so the reader can finish the judgement
                // the flag cannot.
                if verdict.verdict_from_rolled_off_replica == Some(true) {
                    read.rolled_off_excluded.push(verdict.name);
                } else {
                    read.dead_leader_gated.push(verdict);
                }
            }
            // `not_leader_here` as a ROLLUP status is non-nominal by
            // construction — coord's tool description: it means NO live
            // replica is running this worker, and it is unreachable while any
            // live replica reports `alive`. `reason` is the primary key
            // because it is the field coord sets for precisely this state.
            "not_leader_here" => read.leaderless.push(verdict),
            _ => {
                if verdict.reason.as_deref() == Some("no_leader_tick") {
                    read.leaderless.push(verdict);
                }
            }
        }
    }
    ProbeOutcome::Read(Box::new(read))
}

fn worker_verdict(row: &JsonValue) -> WorkerVerdict {
    WorkerVerdict {
        name: row
            .get("name")
            .and_then(JsonValue::as_str)
            .unwrap_or("(unnamed)")
            .to_string(),
        status: row
            .get("status")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_string(),
        live_status: row
            .get("live_status")
            .and_then(JsonValue::as_str)
            .map(str::to_string),
        reason: row
            .get("reason")
            .and_then(JsonValue::as_str)
            .map(str::to_string),
        worst_replica_age_secs: row
            .get("worst_replica_age_secs")
            .and_then(JsonValue::as_f64),
        verdict_from_rolled_off_replica: row
            .get("verdict_from_rolled_off_replica")
            .and_then(JsonValue::as_bool),
        last_decision_code: row
            .get("last_decision_code")
            .and_then(JsonValue::as_str)
            .map(str::to_string),
    }
}

// ---------------------------------------------------------------------------
// Pure: the streak state machine
// ---------------------------------------------------------------------------

/// One thing worth telling the operator about, produced by
/// [`ObserverState::observe`].
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub class: FaultClass,
    /// One line for the notification body and the incident log.
    pub summary: String,
    /// The raw read that justifies it, pretty-printed.
    pub raw: String,
    /// Whether this class may be carried to coord as a finding. False for
    /// [`FaultClass::Unreachable`] by construction — coord is not answering,
    /// so there is nothing to post to (plan Phase 3b.2 scopes the post to
    /// (ii) and (iii)).
    pub post_finding: bool,
}

/// Everything the observer carries between probes: the streaks the plan's
/// `≥ 3 cadences` needs, and the latches that keep one episode to one
/// notification.
///
/// Latches re-arm when the predicate stops holding, the way
/// `webview_recovery::clear_native_ui_thread_hang` re-arms the hang report and
/// `device_jwt_refresher`'s `dark_notified` re-arms on recovery — otherwise
/// the first coord outage of a process's life would be the only one the
/// operator ever hears about.
#[derive(Debug, Default)]
pub struct ObserverState {
    unreachable_streak: u32,
    unreachable_notified: bool,
    leaderless_streak: u32,
    leaderless_notified: bool,
    unusable_streak: u32,
    /// Workers already reported dead in the current episode. A worker that
    /// recovers is dropped, so a second death is reported again.
    dead_notified: BTreeSet<String>,
}

impl ObserverState {
    /// Fold one probe outcome in and return everything that newly became
    /// worth reporting. Pure: no I/O, no clock, no globals.
    pub fn observe(&mut self, outcome: &ProbeOutcome) -> Vec<Report> {
        match outcome {
            ProbeOutcome::Unreachable { reason } => self.observe_unreachable(reason),
            ProbeOutcome::Unusable { reason } => {
                // coord SPOKE. That settles (i) in the negative and says
                // nothing about (ii)/(iii), so those streaks are left exactly
                // where they were rather than cleared — an absent observation
                // is not a contradicting one.
                self.clear_unreachable();
                self.unusable_streak = self.unusable_streak.saturating_add(1);
                if self.unusable_streak == 1
                    || self.unusable_streak.is_multiple_of(UNUSABLE_REWARN_EVERY)
                {
                    warn!(
                        probes = self.unusable_streak,
                        reason = %reason,
                        "coord outside observer: coord answered but this runner could not read \
                         its worker ledger — coord liveness is UNKNOWN here, not healthy"
                    );
                }
                Vec::new()
            }
            ProbeOutcome::Read(read) => {
                self.clear_unreachable();
                self.unusable_streak = 0;
                let mut out = self.observe_dead(read);
                out.extend(self.observe_leaderless(read));
                out
            }
        }
    }

    fn clear_unreachable(&mut self) {
        self.unreachable_streak = 0;
        self.unreachable_notified = false;
    }

    fn observe_unreachable(&mut self, reason: &str) -> Vec<Report> {
        self.unreachable_streak = self.unreachable_streak.saturating_add(1);
        if self.unreachable_streak < CADENCES_TO_FIRE || self.unreachable_notified {
            return Vec::new();
        }
        self.unreachable_notified = true;
        vec![Report {
            class: FaultClass::Unreachable,
            summary: format!(
                "coord has not answered this runner on {} consecutive probes (~{}s). Last \
                 failure: {reason}",
                self.unreachable_streak,
                self.unreachable_streak as u64 * PROBE_PERIOD_SECS
            ),
            raw: reason.to_string(),
            post_finding: false,
        }]
    }

    fn observe_dead(&mut self, read: &LedgerRead) -> Vec<Report> {
        let live: BTreeSet<String> = read
            .dead_leader_gated
            .iter()
            .map(|w| w.name.clone())
            .collect();
        // Re-arm anything that recovered, so a second death pages again.
        self.dead_notified.retain(|n| live.contains(n));

        let mut out = Vec::new();
        for worker in &read.dead_leader_gated {
            if !self.dead_notified.insert(worker.name.clone()) {
                continue;
            }
            out.push(Report {
                class: FaultClass::WorkerDead,
                summary: format!(
                    "coord's leader-gated worker `{}` rolls up `{}` (live_status {}, worst \
                     replica age {}, last decision code {}). counts.dead = {}.",
                    worker.name,
                    worker.status,
                    worker.live_status.as_deref().unwrap_or("unknown"),
                    worker
                        .worst_replica_age_secs
                        .map(|a| format!("{a:.0}s"))
                        .unwrap_or_else(|| "unknown".into()),
                    worker.last_decision_code.as_deref().unwrap_or("none"),
                    read.counts_dead,
                ),
                raw: render_read(read),
                post_finding: true,
            });
        }
        out
    }

    fn observe_leaderless(&mut self, read: &LedgerRead) -> Vec<Report> {
        if read.leaderless.is_empty() {
            self.leaderless_streak = 0;
            self.leaderless_notified = false;
            return Vec::new();
        }
        self.leaderless_streak = self.leaderless_streak.saturating_add(1);
        if self.leaderless_streak < CADENCES_TO_FIRE || self.leaderless_notified {
            return Vec::new();
        }
        self.leaderless_notified = true;
        let names: Vec<&str> = read.leaderless.iter().map(|w| w.name.as_str()).collect();
        vec![Report {
            class: FaultClass::NoLeader,
            summary: format!(
                "no coord replica reports itself leader: {} leader-gated worker(s) have read \
                 `no_leader_tick` on {} consecutive probes (~{}s) — {}",
                names.len(),
                self.leaderless_streak,
                self.leaderless_streak as u64 * PROBE_PERIOD_SECS,
                names.join(", ")
            ),
            raw: render_read(read),
            post_finding: true,
        }]
    }
}

/// Render a successful read for a notification body / incident line / finding.
fn render_read(read: &LedgerRead) -> String {
    serde_json::to_string_pretty(&json!({
        "counts_dead": read.counts_dead,
        "dead_leader_gated": read.dead_leader_gated.iter().map(worker_json).collect::<Vec<_>>(),
        "leaderless": read.leaderless.iter().map(worker_json).collect::<Vec<_>>(),
        "excluded_as_rolled_off_replica": read.rolled_off_excluded,
        "non_nominal_workers_truncated": read.list_truncated,
        "dead_unaccounted_for_in_the_list": read.dead_unaccounted(),
    }))
    .unwrap_or_else(|_| "(could not render the read)".to_string())
}

fn worker_json(w: &WorkerVerdict) -> JsonValue {
    json!({
        "name": w.name,
        "status": w.status,
        "live_status": w.live_status,
        "reason": w.reason,
        "worst_replica_age_secs": w.worst_replica_age_secs,
        "verdict_from_rolled_off_replica": w.verdict_from_rolled_off_replica,
        "last_decision_code": w.last_decision_code,
    })
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
    format!("{head}… (truncated)")
}

// ---------------------------------------------------------------------------
// The probe
// ---------------------------------------------------------------------------

/// Which tenant's credential slot belongs on a coord `/mcp` call from this
/// runner.
///
/// Three-valued for the reason [`TenantScope`] exists: a machine that declares
/// no default binding is the legitimate single-tenant shape and must NOT
/// degrade, while a machine that cannot state its tenant at all is a
/// resolution FAILURE and must — presenting the default binding's credential
/// there is a cross-tenant call on a multi-bound box.
fn scope_for(pin: qontinui_runner_lib::tenant_pin::TenantPin) -> TenantScope {
    use qontinui_runner_lib::tenant_pin::TenantPin;
    match pin {
        TenantPin::Pinned(t) => TenantScope::Owned(t),
        TenantPin::Unpinned => TenantScope::Device,
        TenantPin::Unresolvable => TenantScope::Unresolved,
    }
}

/// One probe against an explicit `/mcp` URL. The URL and the scope are
/// parameters rather than resolved here so every transport arm is testable
/// against a stub with no global state and no credential;
/// [`CoordOutsideObserver::probe_and_report`] resolves the real door once per
/// cycle and hands it in.
pub async fn probe_door(client: &reqwest::Client, url: &str, scope: TenantScope) -> ProbeOutcome {
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": WORKERS_TOOL, "arguments": {} },
    });
    let rb = crate::auth::attach_device_auth_for(client.post(url).json(&body), scope);
    let resp = match rb.send().await {
        Ok(r) => r,
        Err(e) => {
            return ProbeOutcome::Unreachable {
                reason: format!("coord /mcp did not answer ({url}): {e}"),
            }
        }
    };
    let status = resp.status();
    if matches!(status.as_u16(), 401 | 403) {
        // `coord_reachable_check`'s own arm, kept verbatim in substance: a
        // 401/403 means coord is UP and refused this runner's credential.
        // Reporting that as "coord is down" is the misdiagnosis that whole
        // check exists to prevent, so it never reaches predicate (i).
        return ProbeOutcome::Unusable {
            reason: format!(
                "coord REACHED and REJECTED this runner's credential: HTTP {status} ({url}) — \
                 coord is up; this is a credential fault. `coord doctor` reports the chain."
            ),
        };
    }
    if !status.is_success() {
        return ProbeOutcome::Unreachable {
            reason: format!("coord /mcp returned HTTP {status} ({url})"),
        };
    }
    let text = match resp.text().await {
        Ok(t) => t,
        Err(e) => {
            return ProbeOutcome::Unreachable {
                reason: format!("coord /mcp answered {status} but the body did not read: {e}"),
            }
        }
    };
    let rpc: JsonValue = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            return ProbeOutcome::Unusable {
                reason: format!(
                    "coord /mcp answered {status} with a body that is not JSON: {e} — {}",
                    truncate(&text, 200)
                ),
            }
        }
    };
    match unwrap_tools_call(&rpc) {
        Ok(tool) => classify_ledger(&tool),
        Err(reason) => ProbeOutcome::Unusable { reason },
    }
}

// ---------------------------------------------------------------------------
// Surfacing
// ---------------------------------------------------------------------------

/// Put one report in front of the operator and on disk.
///
/// TWO surfaces, both of which the runner already renders, and no new UI —
/// the plan names the read and the predicate, not a component:
///
/// * the **`"error"` Tauri channel** carrying a
///   [`crate::error::UserFacingError`], which `StatusIndicator` renders
///   app-level as a severity-coloured, dismissible card with the raw read in
///   its collapsed "Technical Details" `<pre>`. It is the only surface in this
///   app whose contract is already title + message + raw body + suggested
///   action, which is exactly what "a notification titled for the class and
///   carrying the raw read" needs;
/// * the **durable line**, through `health_monitor::append_wedge_incident` —
///   the single writer for `wedge-incidents.log`, which is the one file to
///   read after an unexplained outage precisely because
///   `runner-lifecycle.log` is truncated at every startup.
///
/// The log line and the breadcrumb are unconditional; the card needs an
/// `AppHandle`, which a headless runner does not have. That asymmetry is
/// deliberate: a server-mode runner still leaves the durable evidence.
fn surface(app: Option<&tauri::AppHandle>, report: &Report) {
    error!(
        class = report.class.breadcrumb_reason(),
        summary = %report.summary,
        "coord outside observer: {}",
        report.class.title()
    );
    crate::health_monitor::append_wedge_incident(
        report.class.breadcrumb_reason(),
        &format!("{} — {}", report.class.title(), report.summary),
    );
    let Some(app) = app else {
        debug!(
            class = report.class.breadcrumb_reason(),
            "coord outside observer: no AppHandle (headless runner) — the incident line is the \
             only surface"
        );
        return;
    };
    let err = crate::error::UserFacingError {
        title: report.class.title().to_string(),
        message: report.summary.clone(),
        details: Some(report.raw.clone()),
        error_code: report.class.error_code().to_string(),
        // Warning, not Error: `StatusIndicator` auto-hides `info` after 5s and
        // keeps everything else until dismissed, and an operator must not miss
        // this one. It is not `Critical` — nothing on this runner is lost or
        // at risk; coord coordination is.
        severity: crate::error::ErrorSeverity::Warning,
        // The condition clears itself the moment coord recovers, and the
        // latch re-arms — so the card is a report on a recoverable state.
        recoverable: true,
        suggested_action: Some(report.class.suggested_action().to_string()),
    };
    // Discarded on purpose: `emit_user_facing_error` has already logged the
    // failure, and a background reporter that could not draw a card must not
    // turn that into a second failure. The incident line above is already on
    // disk either way.
    let _ = crate::error::emit_user_facing_error(app, &err);
}

/// Carry the observation to coord as a finding, so a session arriving later
/// finds it (plan Phase 3b.2).
///
/// Best-effort, warn-and-continue, one attempt — the `agent_runtime.rs`
/// posture. Gated by the caller on the read having SUCCEEDED on this same
/// cycle, which is what keeps Phase 3b from ever writing into a coord that may
/// be the thing that is broken.
async fn post_finding(
    client: &reqwest::Client,
    door: &qontinui_runner_lib::coord_doctor::CoordMcpDoor,
    report: &Report,
) {
    let url = format!("{}/coord/agent-findings", door.base.trim_end_matches('/'));
    // Body shape pinned to `qontinui-coord` `findings.rs` `PostFindingBody`
    // (`deny_unknown_fields`; identity fields are a 400, so none are sent) —
    // the same shape `mcp_api::post_coord_mcp_drift_finding` posts.
    let body = json!({
        "title": format!(
            "{} (observed from OUTSIDE coord by runner device {})",
            report.summary,
            qontinui_runner_lib::machine_identity::read_device_id()
                .unwrap_or_else(|_| "unknown".to_string())
        ),
        "body": format!(
            "{}\n\nEVIDENCE — the `{WORKERS_TOOL}` read this verdict was computed from:\n\n{}\n\n\
             METHOD: one JSON-RPC `tools/call` for `{WORKERS_TOOL}` against {} from inside a \
             runner process, on the ~{}s cadence of `session::coord_sync`'s heartbeat loop. \
             The predicate is `coord_outside_observer::classify_ledger` + \
             `ObserverState::observe` (plan \
             2026-09-12-merge-train-alerts-page-a-reader-and-act-on-nothing Phase 3b): a \
             leader-gated worker rolling up `dead` after the \
             `verdict_from_rolled_off_replica` discrimination, or `no_leader_tick` held for \
             {} consecutive probes.\n\n\
             WHAT A PEER SHOULD DO DIFFERENTLY: this is an OUTSIDE observation — coord's own \
             leader-gated pager cannot report it, which is the whole reason it exists. Read \
             `coord_query_workers` with the worker's name for the per-replica rows, and read \
             `live_status` beside `status` before treating it as a real outage. The runner \
             that posted this has no lever on coord and did not attempt one.",
            report.class.title(),
            report.raw,
            door.url,
            PROBE_PERIOD_SECS,
            CADENCES_TO_FIRE,
        ),
        "kind": "investigation",
        "topic": FINDING_TOPIC,
        "resource_keys": [
            "qontinui-runner/src-tauri/src/coord_outside_observer.rs",
            "2026-09-12-merge-train-alerts-page-a-reader-and-act-on-nothing",
        ],
    });
    let rb =
        crate::auth::attach_device_auth_for(client.post(&url).json(&body), scope_for(door.pin));
    match rb.send().await {
        Ok(resp) if resp.status().is_success() => {
            info!(
                class = report.class.breadcrumb_reason(),
                "coord outside observer: observation posted to coord as a finding"
            );
        }
        Ok(resp) => warn!(
            status = %resp.status(),
            class = report.class.breadcrumb_reason(),
            "coord outside observer: coord refused the finding; not retried"
        ),
        Err(e) => warn!(
            error = %e,
            class = report.class.breadcrumb_reason(),
            "coord outside observer: posting the finding failed; not retried"
        ),
    }
}

// ---------------------------------------------------------------------------
// The observer handle
// ---------------------------------------------------------------------------

/// The process-wide outside observer, owned by `session::coord_sync` so the
/// heartbeat loop can host its cadence.
///
/// `session/coord_sync.rs`'s loop is the right host and `health_monitor`'s is
/// not: the heartbeat task already runs on coord's own 15 s cadence with a
/// 60 s backoff on transport errors, while `health_monitor`'s 5 s self-probe
/// thread is coord-blind and far too hot for a `POST /mcp` per runner.
pub struct CoordOutsideObserver {
    http: reqwest::Client,
    state: Mutex<ObserverState>,
    /// Single-flight. A probe is spawned detached so it can never delay a
    /// session heartbeat, and this makes a slow door drop a tick instead of
    /// stacking probes behind it.
    in_flight: AtomicBool,
    enabled: bool,
}

impl CoordOutsideObserver {
    /// Build the observer. Reads [`OBSERVER_FLAG`] once, here — a changed
    /// value reaches a machine on its next runner start, never by restarting a
    /// running one (`production-and-cost` `runner-lifecycle`).
    pub fn new() -> Self {
        let enabled = std::env::var(OBSERVER_FLAG)
            .map(|v| v.trim() != "0")
            .unwrap_or(true);
        if !enabled {
            info!(
                "coord outside observer: DISABLED by {OBSERVER_FLAG}=0 — coord liveness is \
                 UNOBSERVED from this runner"
            );
        }
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(PROBE_TIMEOUT_SECS))
                .build()
                .unwrap_or_else(|e| {
                    warn!(error = %e, "coord outside observer: reqwest build failed; using default");
                    reqwest::Client::new()
                }),
            state: Mutex::new(ObserverState::default()),
            in_flight: AtomicBool::new(false),
            enabled,
        }
    }

    /// How many host ticks make one probe, so the probe lands at about
    /// [`PROBE_PERIOD_SECS`] however the host's own cadence is tuned. Never
    /// zero — a 0 would make every tick a probe.
    pub fn ticks_per_probe(host_cadence: Duration) -> u64 {
        let secs = host_cadence.as_secs().max(1);
        PROBE_PERIOD_SECS.div_ceil(secs).max(1)
    }

    /// Called once per host tick. Spawns a detached probe on the N-th one and
    /// returns immediately, so the host loop's own cadence is untouched.
    pub fn on_host_tick(
        self: &std::sync::Arc<Self>,
        tick: u64,
        host_cadence: Duration,
        app: Option<tauri::AppHandle>,
    ) {
        if !self.enabled {
            return;
        }
        if !tick.is_multiple_of(Self::ticks_per_probe(host_cadence)) {
            return;
        }
        if self.in_flight.swap(true, Ordering::SeqCst) {
            debug!("coord outside observer: previous probe still in flight — skipping this tick");
            return;
        }
        let me = std::sync::Arc::clone(self);
        tokio::spawn(async move {
            // The latch is released by a DROP GUARD, not by a statement after
            // the await. A panic inside the probe would skip a trailing
            // `store(false)` and latch this observer silent for the life of
            // the process — a watcher that fails silent is the defect class
            // this whole plan is about.
            let _guard = InFlightGuard(std::sync::Arc::clone(&me));
            me.probe_and_report(app.as_ref()).await;
        });
    }

    /// One full cycle: read, fold, surface, and (only when coord answered)
    /// carry the observation back as a finding.
    async fn probe_and_report(&self, app: Option<&tauri::AppHandle>) {
        // Resolved ONCE per cycle, so the read and any finding that follows
        // it cannot end up naming two different upstreams.
        let door = qontinui_runner_lib::coord_doctor::resolve_coord_mcp_door();
        debug!(
            url = %door.url,
            source = %door.base_source,
            slot = door.probed_slot,
            "coord outside observer: probing coord's worker ledger"
        );
        let outcome = probe_door(&self.http, &door.url, scope_for(door.pin)).await;
        // The one shape that would otherwise be SILENT: coord counted dead
        // workers whose rows its own 40-row cap kept out of the list, so
        // predicate (ii) is unsettled for them. Warned here rather than in
        // `ObserverState::observe`, which is pure by contract — and warned
        // even on a cycle that produces no report, because that is precisely
        // the cycle where the absence of a report means nothing.
        if let ProbeOutcome::Read(read) = &outcome {
            if read.dead_is_underread() {
                warn!(
                    counts_dead = read.counts_dead,
                    unaccounted = read.dead_unaccounted(),
                    named = read.dead_leader_gated.len(),
                    "coord outside observer: coord's non_nominal_workers list was TRUNCATED and \
                     counts.dead exceeds what this read could name — predicate (ii) is UNKNOWN \
                     for the unnamed workers, not clear. Call coord_query_workers with a `name` \
                     for the per-replica rows."
                );
            }
        }
        let coord_answered = !matches!(outcome, ProbeOutcome::Unreachable { .. });
        let reports = {
            let mut guard = match self.state.lock() {
                Ok(g) => g,
                Err(poisoned) => poisoned.into_inner(),
            };
            guard.observe(&outcome)
        };
        for report in &reports {
            surface(app, report);
            // The write is gated on coord having ANSWERED this cycle, not on
            // the report's class alone — `post_finding` is false for
            // `Unreachable` anyway, but the conjunction is what makes "never
            // write into a coord that may be broken" true by construction.
            if report.post_finding && coord_answered {
                post_finding(&self.http, &door, report).await;
            }
        }
    }
}

impl Default for CoordOutsideObserver {
    fn default() -> Self {
        Self::new()
    }
}

/// Releases [`CoordOutsideObserver::in_flight`] when the probe task ends —
/// including when it ends by PANIC, which a trailing `store(false)` would not
/// survive. `tokio::spawn` swallows a panicking task into its `JoinHandle`,
/// and nothing here joins it, so without this guard one panic would latch the
/// observer silent for the life of the process.
struct InFlightGuard(std::sync::Arc<CoordOutsideObserver>);

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.0.in_flight.store(false, Ordering::SeqCst);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A canned `coord_query_workers` no-arg body, shaped exactly as
    /// `qontinui-coord`'s `query_workers_handler` emits it.
    fn ledger_body(counts_dead: u64, non_nominal: JsonValue) -> JsonValue {
        json!({
            "instance": "workers",
            "drift_class": "active_negation",
            "drift_subclass": "workers:dead",
            "d3_outcome": "block",
            "posterior": 1.0,
            "coverage": 1.0,
            "provenance": "coord_worker_heartbeats",
            "credibility": 1.0,
            "carve_out": [],
            "components": {
                "counts": {
                    "total": 19, "alive": 18, "stale": 0,
                    "dead": counts_dead, "not_leader_here": 0, "non_nominal": 1
                },
                "non_nominal_workers": non_nominal,
                "non_nominal_workers_truncated": false,
            }
        })
    }

    /// The same body with coord's own truncation flag set — the shape its
    /// 40-row `take` produces on a fleet with more non-nominal workers than
    /// the cap.
    fn truncated_ledger_body(counts_dead: u64, non_nominal: JsonValue) -> JsonValue {
        let mut body = ledger_body(counts_dead, non_nominal);
        body["components"]["non_nominal_workers_truncated"] = json!(true);
        body
    }

    fn dead_row(name: &str, leader_gated: bool, rolled_off: bool) -> JsonValue {
        json!({
            "name": name,
            "status": "dead",
            "live_status": "dead",
            "reason": "dead",
            "leader_gated": leader_gated,
            "last_tick_secs_ago": 1499.0,
            "worst_replica_age_secs": 1499.99,
            "verdict_from_rolled_off_replica": rolled_off,
            "replicas_observed": 2,
            "replicas_departed": 610,
            "consecutive_errors": 0,
            "last_decision_code": "follower_skip",
            "last_detail_code": null,
        })
    }

    fn no_leader_row(name: &str) -> JsonValue {
        json!({
            "name": name,
            "status": "not_leader_here",
            "live_status": "not_leader_here",
            "reason": "no_leader_tick",
            "leader_gated": true,
            "last_tick_secs_ago": 3.0,
            "worst_replica_age_secs": 4.0,
            "verdict_from_rolled_off_replica": false,
            "replicas_observed": 2,
            "replicas_departed": 0,
            "consecutive_errors": 0,
            "last_decision_code": "follower_skip",
            "last_detail_code": null,
        })
    }

    fn rpc_ok(tool: &JsonValue) -> JsonValue {
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "content": [{ "type": "text", "text": serde_json::to_string(tool).unwrap() }],
                "isError": false,
            }
        })
    }

    // ── envelope ────────────────────────────────────────────────────────

    #[test]
    fn unwrap_tools_call_reads_the_text_block() {
        let tool = ledger_body(0, json!([]));
        let got = unwrap_tools_call(&rpc_ok(&tool)).expect("unwraps");
        assert_eq!(got, tool);
    }

    #[test]
    fn unwrap_tools_call_rejects_an_is_error_result() {
        let rpc = json!({
            "jsonrpc": "2.0", "id": 1,
            "result": {
                "content": [{"type": "text", "text": "tool `coord_query_workers` is masked"}],
                "isError": true,
            }
        });
        let err = unwrap_tools_call(&rpc).expect_err("isError is not a read");
        assert!(err.contains("refused"), "{err}");
    }

    #[test]
    fn unwrap_tools_call_tolerates_an_explicit_null_error_beside_a_result() {
        let tool = ledger_body(0, json!([]));
        let mut rpc = rpc_ok(&tool);
        rpc["error"] = JsonValue::Null;
        let got = unwrap_tools_call(&rpc).expect("a null `error` is not an error");
        assert_eq!(got, tool);
    }

    #[test]
    fn unwrap_tools_call_rejects_a_jsonrpc_error() {
        let rpc = json!({
            "jsonrpc": "2.0", "id": 1,
            "error": {"code": -32601, "message": "method not found"}
        });
        let err = unwrap_tools_call(&rpc).expect_err("a protocol error is not a read");
        assert!(err.contains("-32601"), "{err}");
    }

    // ── predicate (ii): a dead leader-gated worker ──────────────────────

    #[test]
    fn dead_leader_gated_worker_is_read_and_reported_on_the_first_observation() {
        let body = ledger_body(1, json!([dead_row("work_unit_derive.sweep", true, false)]));
        let ProbeOutcome::Read(read) = classify_ledger(&body) else {
            panic!("expected a Read");
        };
        assert_eq!(read.dead_leader_gated.len(), 1);
        assert_eq!(read.dead_leader_gated[0].name, "work_unit_derive.sweep");
        assert!(read.rolled_off_excluded.is_empty());

        let mut state = ObserverState::default();
        let reports = state.observe(&ProbeOutcome::Read(read));
        assert_eq!(reports.len(), 1, "dead fires on the first observation");
        assert_eq!(reports[0].class, FaultClass::WorkerDead);
        assert!(reports[0].post_finding);
        assert!(reports[0].summary.contains("work_unit_derive.sweep"));
    }

    #[test]
    fn a_dead_worker_is_reported_once_per_episode_and_re_arms_on_recovery() {
        let dead = ledger_body(1, json!([dead_row("merge_scheduler.dispatch", true, false)]));
        let clean = ledger_body(0, json!([]));
        let mut state = ObserverState::default();

        assert_eq!(state.observe(&classify_ledger(&dead)).len(), 1);
        assert_eq!(
            state.observe(&classify_ledger(&dead)).len(),
            0,
            "the same dead worker must not re-notify every minute"
        );
        assert_eq!(state.observe(&classify_ledger(&clean)).len(), 0);
        assert_eq!(
            state.observe(&classify_ledger(&dead)).len(),
            1,
            "a second death after a recovery is a new episode"
        );
    }

    #[test]
    fn a_second_dead_worker_is_reported_beside_the_first() {
        let one = ledger_body(1, json!([dead_row("a.sweep", true, false)]));
        let two = ledger_body(
            2,
            json!([dead_row("a.sweep", true, false), dead_row("b.sweep", true, false)]),
        );
        let mut state = ObserverState::default();
        assert_eq!(state.observe(&classify_ledger(&one)).len(), 1);
        let reports = state.observe(&classify_ledger(&two));
        assert_eq!(reports.len(), 1);
        assert!(reports[0].summary.contains("b.sweep"));
    }

    // ── the rolled-off exclusion ────────────────────────────────────────

    #[test]
    fn a_dead_verdict_from_a_rolled_off_replica_is_excluded_not_reported() {
        let body = ledger_body(1, json!([dead_row("pr_merge.reconcile_once", true, true)]));
        let ProbeOutcome::Read(read) = classify_ledger(&body) else {
            panic!("expected a Read");
        };
        assert!(
            read.dead_leader_gated.is_empty(),
            "a deploy-roll artifact must not page"
        );
        assert_eq!(read.rolled_off_excluded, vec!["pr_merge.reconcile_once"]);

        let mut state = ObserverState::default();
        assert!(state.observe(&ProbeOutcome::Read(read)).is_empty());
    }

    #[test]
    fn a_dead_worker_that_is_not_leader_gated_is_ignored() {
        let body = ledger_body(1, json!([dead_row("some.follower.loop", false, false)]));
        let ProbeOutcome::Read(read) = classify_ledger(&body) else {
            panic!("expected a Read");
        };
        assert!(read.dead_leader_gated.is_empty());
        assert!(read.rolled_off_excluded.is_empty());
    }

    // ── predicate (iii): no leader ──────────────────────────────────────

    #[test]
    fn no_leader_needs_three_cadences_then_fires_once() {
        let body = ledger_body(0, json!([no_leader_row("alert_pageout_worker")]));
        let mut state = ObserverState::default();
        assert!(state.observe(&classify_ledger(&body)).is_empty(), "probe 1");
        assert!(state.observe(&classify_ledger(&body)).is_empty(), "probe 2");
        let reports = state.observe(&classify_ledger(&body));
        assert_eq!(reports.len(), 1, "probe 3 fires");
        assert_eq!(reports[0].class, FaultClass::NoLeader);
        assert!(reports[0].post_finding);
        assert!(state.observe(&classify_ledger(&body)).is_empty(), "probe 4");
    }

    #[test]
    fn a_recovered_leader_re_arms_the_no_leader_report() {
        let dark = ledger_body(0, json!([no_leader_row("gate_evaluation_sweep")]));
        let clean = ledger_body(0, json!([]));
        let mut state = ObserverState::default();
        for _ in 0..3 {
            state.observe(&classify_ledger(&dark));
        }
        assert!(state.observe(&classify_ledger(&clean)).is_empty());
        for _ in 0..2 {
            assert!(state.observe(&classify_ledger(&dark)).is_empty());
        }
        assert_eq!(
            state.observe(&classify_ledger(&dark)).len(),
            1,
            "the streak restarts from zero after a healthy read"
        );
    }

    #[test]
    fn a_healthy_fleet_reports_nothing() {
        let body = ledger_body(0, json!([]));
        let mut state = ObserverState::default();
        for _ in 0..10 {
            assert!(state.observe(&classify_ledger(&body)).is_empty());
        }
    }

    // ── predicate (i): unreachable ──────────────────────────────────────

    #[test]
    fn unreachable_needs_three_cadences_then_fires_once() {
        let mut state = ObserverState::default();
        let out = ProbeOutcome::Unreachable {
            reason: "connection refused".into(),
        };
        assert!(state.observe(&out).is_empty(), "probe 1");
        assert!(state.observe(&out).is_empty(), "probe 2");
        let reports = state.observe(&out);
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].class, FaultClass::Unreachable);
        assert!(
            !reports[0].post_finding,
            "an unreachable coord is not a coord to post a finding to"
        );
        assert!(state.observe(&out).is_empty(), "probe 4 does not re-notify");
    }

    #[test]
    fn a_successful_read_clears_the_unreachable_streak() {
        let mut state = ObserverState::default();
        let down = ProbeOutcome::Unreachable {
            reason: "timeout".into(),
        };
        state.observe(&down);
        state.observe(&down);
        assert!(state
            .observe(&classify_ledger(&ledger_body(0, json!([]))))
            .is_empty());
        assert!(state.observe(&down).is_empty(), "the streak restarted");
        assert!(state.observe(&down).is_empty());
        assert_eq!(state.observe(&down).len(), 1);
    }

    // ── the UNKNOWN arm ─────────────────────────────────────────────────

    #[test]
    fn coords_honest_unknown_verdict_is_unusable_not_a_clean_fleet() {
        let body = json!({
            "instance": "workers",
            "drift_class": "unknown",
            "drift_subclass": "workers:no_observation",
            "d3_outcome": "escalate",
            "posterior": 0.0,
            "coverage": 0.0,
            "provenance": "no_observation_yet",
            "credibility": 0.0,
            "carve_out": [],
            "components": {
                "note": "coord.worker_heartbeats is empty — no wrapped worker has ticked yet",
                "workers": [],
                "counts": {"alive": 0, "stale": 0, "dead": 0, "not_leader_here": 0},
            }
        });
        let outcome = classify_ledger(&body);
        match &outcome {
            ProbeOutcome::Unusable { reason } => {
                assert!(reason.contains("NOT a clean worker fleet"), "{reason}")
            }
            other => panic!("expected Unusable, got {other:?}"),
        }
    }

    #[test]
    fn an_unusable_read_clears_unreachable_and_leaves_the_others_untouched() {
        let dark = ledger_body(0, json!([no_leader_row("alert_pageout_worker")]));
        let mut state = ObserverState::default();
        state.observe(&classify_ledger(&dark));
        state.observe(&classify_ledger(&dark));

        let unusable = ProbeOutcome::Unusable {
            reason: "credential refused".into(),
        };
        assert!(state.observe(&unusable).is_empty());
        assert_eq!(
            state.observe(&classify_ledger(&dark)).len(),
            1,
            "an UNKNOWN observation must neither advance nor reset the leader streak"
        );

        // …and it did reset the unreachable streak, because coord spoke.
        let down = ProbeOutcome::Unreachable {
            reason: "refused".into(),
        };
        state.observe(&down);
        state.observe(&down);
        assert!(state.observe(&unusable).is_empty());
        assert!(state.observe(&down).is_empty());
        assert!(state.observe(&down).is_empty());
        assert_eq!(state.observe(&down).len(), 1);
    }

    // ── the truncation blind spot ───────────────────────────────────────

    #[test]
    fn a_truncated_list_hiding_a_dead_worker_is_flagged_as_under_read() {
        // coord counted 3 dead; its 40-row cap let this runner name one.
        let body = truncated_ledger_body(3, json!([dead_row("a.sweep", true, false)]));
        let ProbeOutcome::Read(read) = classify_ledger(&body) else {
            panic!("expected a Read");
        };
        assert!(read.list_truncated, "coord's own flag is carried, not dropped");
        assert_eq!(read.dead_unaccounted(), 2);
        assert!(
            read.dead_is_underread(),
            "a truncated list with unaccounted dead workers is UNKNOWN, not clear"
        );
        // The one it COULD name still pages, and the body says what it could not.
        let mut state = ObserverState::default();
        let reports = state.observe(&ProbeOutcome::Read(read));
        assert_eq!(reports.len(), 1);
        assert!(
            reports[0].raw.contains("\"dead_unaccounted_for_in_the_list\": 2"),
            "the report body names the blind spot: {}",
            reports[0].raw
        );
    }

    #[test]
    fn an_untruncated_list_is_never_under_read_however_the_counts_fall() {
        // `counts.dead` is over EVERY worker while the classified population
        // is leader-gated only, so a surplus is ordinary here and must not
        // read as a blind spot.
        let body = ledger_body(5, json!([dead_row("follower.loop", false, false)]));
        let ProbeOutcome::Read(read) = classify_ledger(&body) else {
            panic!("expected a Read");
        };
        assert!(!read.list_truncated);
        assert_eq!(read.dead_unaccounted(), 5);
        assert!(
            !read.dead_is_underread(),
            "an untruncated list was fully read, whatever the counts say"
        );
    }

    #[test]
    fn a_rolled_off_exclusion_is_accounted_for_not_counted_as_missing() {
        let body = truncated_ledger_body(1, json!([dead_row("rolled.off", true, true)]));
        let ProbeOutcome::Read(read) = classify_ledger(&body) else {
            panic!("expected a Read");
        };
        assert_eq!(read.rolled_off_excluded.len(), 1);
        assert_eq!(read.dead_unaccounted(), 0);
        assert!(
            !read.dead_is_underread(),
            "an EXCLUDED row was read and judged — it is not an unread one"
        );
    }

    #[test]
    fn a_body_without_counts_is_unusable() {
        let body = json!({"instance": "workers", "components": {"something_else": 1}});
        assert!(matches!(
            classify_ledger(&body),
            ProbeOutcome::Unusable { .. }
        ));
    }

    // ── the vocabulary ──────────────────────────────────────────────────

    /// The breadcrumb tokens are what a reader greps `wedge-incidents.log`
    /// for after an outage, so they are part of the contract, not decoration.
    /// Mirrors `health_monitor`'s own
    /// `breadcrumb_reasons_are_distinct_and_stable`, which pins the wedge
    /// family in the same file.
    #[test]
    fn breadcrumb_reasons_and_error_codes_are_stable_and_distinct() {
        let classes = [
            FaultClass::Unreachable,
            FaultClass::WorkerDead,
            FaultClass::NoLeader,
        ];
        assert_eq!(
            FaultClass::Unreachable.breadcrumb_reason(),
            "coord_unreachable"
        );
        assert_eq!(
            FaultClass::WorkerDead.breadcrumb_reason(),
            "coord_worker_dead"
        );
        assert_eq!(FaultClass::NoLeader.breadcrumb_reason(), "coord_no_leader");

        let reasons: BTreeSet<&str> = classes.iter().map(|c| c.breadcrumb_reason()).collect();
        assert_eq!(reasons.len(), classes.len(), "reasons must be distinct");
        let codes: BTreeSet<&str> = classes.iter().map(|c| c.error_code()).collect();
        assert_eq!(codes.len(), classes.len(), "error codes must be distinct");
        let titles: BTreeSet<&str> = classes.iter().map(|c| c.title()).collect();
        assert_eq!(titles.len(), classes.len(), "titles must be distinct");

        for c in classes {
            // Every class owns a `coord_`-prefixed breadcrumb, so the coord
            // family stays separable from the wedge family sharing the file.
            assert!(c.breadcrumb_reason().starts_with("coord_"), "{c:?}");
            // And never suggests the one act this observer must not imply.
            let action = c.suggested_action().to_lowercase();
            assert!(
                !action.contains("restart the runner"),
                "{c:?} must never suggest restarting a runner"
            );
        }
    }

    // ── cadence arithmetic ──────────────────────────────────────────────

    #[test]
    fn ticks_per_probe_lands_at_about_one_probe_a_minute() {
        assert_eq!(
            CoordOutsideObserver::ticks_per_probe(Duration::from_secs(15)),
            4
        );
        assert_eq!(
            CoordOutsideObserver::ticks_per_probe(Duration::from_secs(60)),
            1
        );
        assert_eq!(
            CoordOutsideObserver::ticks_per_probe(Duration::from_secs(90)),
            1,
            "a host slower than the target probes every tick, never zero"
        );
        assert_eq!(
            CoordOutsideObserver::ticks_per_probe(Duration::from_secs(7)),
            9
        );
        assert_eq!(
            CoordOutsideObserver::ticks_per_probe(Duration::from_millis(1)),
            60,
            "a sub-second host cadence must not divide by zero"
        );
    }

    // ── transport: the stub that 503s three times ───────────────────────

    #[tokio::test]
    async fn three_consecutive_503s_raise_the_unreachable_report() {
        use axum::extract::State as AxumState;
        use axum::http::StatusCode;
        use axum::routing::post;
        use axum::Router;
        use std::sync::atomic::AtomicU32;
        use std::sync::Arc;
        use tokio::net::TcpListener;

        let hits = Arc::new(AtomicU32::new(0));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app: Router = Router::new()
            .route(
                "/mcp",
                post(|AxumState(hits): AxumState<Arc<AtomicU32>>| async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    (StatusCode::SERVICE_UNAVAILABLE, "upstream unavailable")
                }),
            )
            .with_state(hits.clone());
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let url = format!("http://{addr}/mcp");
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let mut state = ObserverState::default();
        let mut fired = Vec::new();
        for _ in 0..3 {
            let outcome = probe_door(&client, &url, TenantScope::Device).await;
            match &outcome {
                ProbeOutcome::Unreachable { reason } => assert!(reason.contains("503"), "{reason}"),
                other => panic!("a 503 must be Unreachable, got {other:?}"),
            }
            fired.extend(state.observe(&outcome));
        }
        assert_eq!(hits.load(Ordering::SeqCst), 3);
        assert_eq!(fired.len(), 1, "three cadences, one report");
        assert_eq!(fired[0].class, FaultClass::Unreachable);
    }

    #[tokio::test]
    async fn a_401_is_a_credential_fault_not_an_unreachable_coord() {
        use axum::http::StatusCode;
        use axum::routing::post;
        use axum::Router;
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app: Router = Router::new().route(
            "/mcp",
            post(|| async { (StatusCode::UNAUTHORIZED, "bad token") }),
        );
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let mut state = ObserverState::default();
        for _ in 0..5 {
            let outcome =
                probe_door(&client, &format!("http://{addr}/mcp"), TenantScope::Device).await;
            match &outcome {
                ProbeOutcome::Unusable { reason } => {
                    assert!(reason.contains("coord is up"), "{reason}")
                }
                other => panic!("a 401 must be Unusable, got {other:?}"),
            }
            assert!(
                state.observe(&outcome).is_empty(),
                "a credential fault must never render as `coord is down`"
            );
        }
    }

    #[tokio::test]
    async fn a_live_ledger_read_travels_the_whole_transport() {
        use axum::extract::State as AxumState;
        use axum::routing::post;
        use axum::Json;
        use axum::Router;
        use std::sync::Arc;
        use tokio::net::TcpListener;

        let body = Arc::new(ledger_body(
            1,
            json!([dead_row("work_unit_derive.sweep", true, false)]),
        ));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app: Router = Router::new()
            .route(
                "/mcp",
                post(|AxumState(b): AxumState<Arc<JsonValue>>| async move {
                    Json(rpc_ok(&b))
                }),
            )
            .with_state(body.clone());
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let outcome =
            probe_door(&client, &format!("http://{addr}/mcp"), TenantScope::Device).await;
        let mut state = ObserverState::default();
        let reports = state.observe(&outcome);
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].class, FaultClass::WorkerDead);
        assert!(
            reports[0].raw.contains("work_unit_derive.sweep"),
            "the notification carries the raw read"
        );
    }
}
