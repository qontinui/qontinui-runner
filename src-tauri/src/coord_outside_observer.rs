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
//! # The predicates
//!
//! | # | fires on | debounce |
//! |---|---|---|
//! | (i) [`FaultClass::Unreachable`] | coord did not answer, or answered non-2xx | 3 consecutive probes |
//! | (ii) [`FaultClass::WorkerDead`] | a leader-gated worker rolls up `dead` | first observation, per worker |
//! | (iii) [`FaultClass::NoLeader`] | a leader-gated worker reports `no_leader_tick` — NO live replica is running it | 3 consecutive probes |
//! | (iv) [`FaultClass::LivenessUnknown`] | coord answered and this runner could not read a ledger out of it | 3 consecutive probes |
//!
//! (ii) carries no cadence requirement because `dead` is already a debounced
//! verdict: coord's ledger only reaches it after >10 tick intervals of
//! silence. (i), (iii) and (iv) are single-sample facts and get the plan's
//! `≥ 3 cadences`.
//!
//! (iv) is not a fault in coord. It is a fault in this runner's VIEW of
//! coord, and it gets a surface for exactly the reason the other three do:
//! a watcher that has gone blind must SAY SO on the surface it would have
//! used, or its silence is indistinguishable from a clean fleet. Four ways it
//! becomes permanent rather than transient — a rotated or expired device
//! credential (or a WAF 401), coord's own `workers:no_observation`, a
//! response-shape change this build cannot parse, and a flaky load balancer
//! alternating `Unreachable`/`Unusable` so that NEITHER streak ever reaches
//! its cadence — are all states in which coord liveness is unobserved
//! indefinitely while the runner looks healthy.
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
//! Held for [`CADENCES_TO_FIRE`] probes it becomes predicate (iv) and is
//! surfaced, so the UNKNOWN reaches the operator instead of only the log.
//!
//! # Truncation is an UNKNOWN too, and it bites (iii) HARDEST
//!
//! coord's no-arg `coord_query_workers` caps `non_nominal_workers` at 40 rows
//! and reports the cut in `non_nominal_workers_truncated`. The rows it keeps
//! are NOT arbitrary — see [`LedgerRead::list_truncated`] — they are the
//! WORST-first, and `not_leader_here` shares the bottom severity with
//! `alive`. So the cap discards leaderless rows FIRST and dead rows LAST,
//! which inverts the naive risk model: predicate (iii), the one this module
//! exists for, is the one the cap silences. An empty `leaderless` list on a
//! TRUNCATED read is therefore UNKNOWN, never health, and
//! [`ObserverState`] neither advances nor clears (iii) on such a cycle —
//! exactly what it already does for an [`ProbeOutcome::Unusable`].

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

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
    /// (iv) coord ANSWERED and this runner could not read a worker ledger out
    /// of its answer, for [`CADENCES_TO_FIRE`] probes running.
    ///
    /// Not a statement about coord's health — a statement that this runner
    /// has no statement. It is a class rather than a log line because the
    /// states that produce it (a dead credential, a shape change, coord's own
    /// `workers:no_observation`) are typically PERMANENT until someone acts,
    /// and an observer that is permanently blind while the runner looks
    /// healthy is the exact defect this whole plan is about.
    LivenessUnknown,
}

impl FaultClass {
    /// The greppable token written into `wedge-incidents.log`, in the same
    /// vocabulary as `health_monitor`'s `backend_wedged` / `ui_thread_wedged`.
    pub fn breadcrumb_reason(self) -> &'static str {
        match self {
            FaultClass::Unreachable => "coord_unreachable",
            FaultClass::WorkerDead => "coord_worker_dead",
            FaultClass::NoLeader => "coord_no_leader",
            FaultClass::LivenessUnknown => "coord_liveness_unknown",
        }
    }

    /// The notification title — "titled for the class", per the plan.
    pub fn title(self) -> &'static str {
        match self {
            FaultClass::Unreachable => "Coord is not answering this runner",
            FaultClass::WorkerDead => "A coord leader-gated worker is dead",
            FaultClass::NoLeader => "Coord has no leader",
            FaultClass::LivenessUnknown => "Coord liveness is UNKNOWN to this runner",
        }
    }

    /// Stable code the operator card prints and a log reader greps.
    pub fn error_code(self) -> &'static str {
        match self {
            FaultClass::Unreachable => "COORD_LIVENESS_UNREACHABLE",
            FaultClass::WorkerDead => "COORD_LIVENESS_WORKER_DEAD",
            FaultClass::NoLeader => "COORD_LIVENESS_NO_LEADER",
            FaultClass::LivenessUnknown => "COORD_LIVENESS_UNKNOWN",
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
            FaultClass::LivenessUnknown => {
                "This is NOT a verdict on coord — it is this runner reporting that it \
                 has no verdict. coord answered every probe and none of the answers \
                 carried a readable worker ledger, so predicates (ii) and (iii) have \
                 been unobserved from here for as long as the message says. The usual \
                 causes, in order: an expired or rotated device credential (run \
                 `coord doctor` — a 401/403 is reported here, never as `coord is \
                 down`); the `coord_query_workers` tool masked for this principal; \
                 coord's own `workers:no_observation` verdict, which means its ledger \
                 has no rows to read; or a response shape this runner build cannot \
                 parse, which wants a runner upgrade. The details block carries the \
                 last reason verbatim."
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
    /// `components.counts.not_leader_here`, verbatim — predicate (iii)'s
    /// count over the FULL worker set, which is what makes a truncated list
    /// readable as UNKNOWN rather than as health.
    ///
    /// It is an exact analogue of [`Self::counts_dead`] because coord makes
    /// it one: a rollup status of `not_leader_here` is non-nominal BY
    /// CONSTRUCTION (`qontinui-coord` `worker_ledger::rollup`'s
    /// `(nominal, reason)` match — every `NotLeaderHere` gets
    /// `(false, Some("no_leader_tick"))`), so every worker this counts is
    /// also a worker the `non_nominal_workers` list would have carried had it
    /// not been capped.
    pub counts_not_leader_here: u64,
    /// Leader-gated `dead` rows EXCLUDED because coord could prove the verdict
    /// came from a replica a deploy had already replaced. Reported in the body
    /// so an excluded row is visible rather than silently dropped.
    pub rolled_off_excluded: Vec<String>,
    /// coord's own `components.non_nominal_workers_truncated`.
    ///
    /// The list is capped at `QUERY_WORKERS_MAX_LISTED` (40) by
    /// `qontinui-coord` `mcp::tools`' no-arg arm. **That `take(40)` runs over
    /// a SORTED order, and knowing which order is the whole point** — an
    /// earlier revision of this comment asserted there was "NO priority sort",
    /// which was false and which produced a real defect in predicate (iii)
    /// (see below).
    ///
    /// `worker_ledger::rollup` sorts before returning — *"Worst-first, then
    /// alphabetical — a bounded summary must show the actionable rows before
    /// it truncates"* — on `status.severity()` DESCENDING, then `nominal`,
    /// then name. And `WorkerLiveness::severity()` is:
    ///
    /// | status | severity |
    /// |---|---|
    /// | `dead` | 2 |
    /// | `stale` | 1 |
    /// | `alive`, `not_leader_here` | 0 |
    ///
    /// So the cap drops the severity-0 TAIL. For predicate (ii) that is
    /// benign-ish: `dead` sorts at the head and is discarded LAST, so a
    /// truncated list losing a dead row needs >40 dead-or-stale rows ahead of
    /// it. For predicate (iii) it INVERTS the risk: `not_leader_here` sits at
    /// severity 0, so leaderless rows are discarded **FIRST**. A coord that
    /// has lost its leader entirely flips every leader-gated worker to
    /// `not_leader_here` at once; if the non-nominal population then exceeds
    /// 40, every leaderless row is truncated away and a naive reader sees
    /// `leaderless: []` — the fault this module exists to catch, rendered as
    /// health.
    ///
    /// Carried rather than ignored so that BOTH under-reads
    /// ([`Self::dead_is_underread`], [`Self::leaderless_is_underread`]) read
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

    /// `counts.not_leader_here` this read could not name — the exact analogue
    /// of [`Self::dead_unaccounted`] for predicate (iii).
    ///
    /// Saturating, and not an error on its own: [`Self::leaderless`] is the
    /// LEADER-GATED subset while the count is over every worker, so a surplus
    /// is ordinary on an untruncated list.
    pub fn leaderless_unaccounted(&self) -> u64 {
        self.counts_not_leader_here
            .saturating_sub(self.leaderless.len() as u64)
    }

    /// True when coord counted leaderless workers this runner could not see,
    /// BECAUSE the list was truncated — the analogue of
    /// [`Self::dead_is_underread`], and the one that actually fires in
    /// practice, because the cap discards severity-0 rows first (see
    /// [`Self::list_truncated`]).
    ///
    /// When this holds, an EMPTY [`Self::leaderless`] is UNKNOWN rather than
    /// health, and [`ObserverState::observe`] must neither advance nor clear
    /// predicate (iii) on that cycle.
    pub fn leaderless_is_underread(&self) -> bool {
        self.list_truncated && self.leaderless_unaccounted() > 0
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
    // ABSENT is not ZERO. Both keys are emitted unconditionally by coord's
    // no-arg arm, so a missing one means this build is reading a shape it does
    // not understand — and defaulting either to 0 would make every truncation
    // predicate below read `false` and report a permanently clean fleet.
    let Some(counts_dead) = counts.get("dead").and_then(JsonValue::as_u64) else {
        return ProbeOutcome::Unusable {
            reason: "coord answered but `components.counts` carried no readable `dead` — this \
                 runner build cannot read this response shape, and an absent count is NOT zero"
                .to_string(),
        };
    };
    let Some(counts_not_leader_here) = counts.get("not_leader_here").and_then(JsonValue::as_u64)
    else {
        return ProbeOutcome::Unusable {
            reason: "coord answered but `components.counts` carried no readable \
                 `not_leader_here` — this runner build cannot read this response shape, and \
                 without that count a truncated list cannot be told from a fleet with a leader"
                .to_string(),
        };
    };

    // Present-and-empty and ABSENT are different facts, and only one of them
    // is a clean fleet. Collapsing them with `unwrap_or_default()` meant that
    // a rename of this key left `counts` parsing, `list_truncated` reading
    // `false` and every under-read predicate reading `false` — a permanently
    // healthy verdict with no warning anywhere. Asymmetric with the `counts`
    // arm above, which already refused; the asymmetry WAS the bug.
    let listed = match tool.pointer("/components/non_nominal_workers") {
        Some(JsonValue::Array(rows)) => rows.clone(),
        Some(other) => {
            return ProbeOutcome::Unusable {
                reason: format!(
                    "coord answered but `components.non_nominal_workers` is a {} rather than an \
                     array — this runner build cannot read this response shape",
                    kind_of(other)
                ),
            }
        }
        None => {
            return ProbeOutcome::Unusable {
                reason: format!(
                    "coord answered but the {WORKERS_TOOL} body carried no \
                     `components.non_nominal_workers` — an ABSENT list is UNKNOWN, not an empty \
                     one, and this runner build cannot read this response shape"
                ),
            }
        }
    };

    let mut read = LedgerRead {
        counts_dead,
        counts_not_leader_here,
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

/// The JSON type name, for a shape-mismatch message that names what it got.
fn kind_of(v: &JsonValue) -> &'static str {
    match v {
        JsonValue::Null => "null",
        JsonValue::Bool(_) => "bool",
        JsonValue::Number(_) => "number",
        JsonValue::String(_) => "string",
        JsonValue::Array(_) => "array",
        JsonValue::Object(_) => "object",
    }
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
    unreachable_since: Option<Instant>,
    leaderless_streak: u32,
    /// EVERY probe folded since the (iii) episode opened, not only the ones
    /// that advanced the streak. The streak deliberately survives cycles that
    /// observe nothing (an [`ProbeOutcome::Unusable`], an
    /// [`ProbeOutcome::Unreachable`], a truncated read whose leaderless rows
    /// were capped away), so `streak` and "consecutive probes" are NOT the
    /// same number and the summary must not claim they are.
    leaderless_probes: u32,
    leaderless_notified: bool,
    leaderless_since: Option<Instant>,
    unusable_streak: u32,
    unusable_notified: bool,
    unusable_since: Option<Instant>,
    /// Workers already reported dead in the current episode. A worker that
    /// recovers is dropped, so a second death is reported again.
    dead_notified: BTreeSet<String>,
}

impl ObserverState {
    /// Fold one probe outcome in and return everything that newly became
    /// worth reporting.
    ///
    /// Reads the clock exactly once, here, and hands it down — so every
    /// elapsed figure a report prints is MEASURED rather than reconstructed
    /// from `streak × PROBE_PERIOD_SECS`, which was never reliably true
    /// (`ticks_per_probe` rounds the real period UP with `div_ceil`, and the
    /// (iii) streak deliberately spans probes that observed nothing).
    /// [`Self::observe_at`] is the pure half, and the one tests drive.
    pub fn observe(&mut self, outcome: &ProbeOutcome) -> Vec<Report> {
        self.observe_at(outcome, Instant::now())
    }

    /// [`Self::observe`] with the clock injected. Pure: no I/O, no globals,
    /// and no clock of its own.
    pub fn observe_at(&mut self, outcome: &ProbeOutcome, now: Instant) -> Vec<Report> {
        // Every fold counts against an OPEN (iii) episode, including the ones
        // that observe nothing about it — that is what makes "N of the last M
        // probes" an honest sentence rather than a guess.
        if self.leaderless_since.is_some() {
            self.leaderless_probes = self.leaderless_probes.saturating_add(1);
        }
        match outcome {
            ProbeOutcome::Unreachable { reason } => self.observe_unreachable(reason, now),
            ProbeOutcome::Unusable { reason } => self.observe_unusable(reason, now),
            ProbeOutcome::Read(read) => {
                self.clear_unreachable();
                self.clear_unusable();
                let mut out = self.observe_dead(read);
                out.extend(self.observe_leaderless(read, now));
                out
            }
        }
    }

    fn clear_unreachable(&mut self) {
        self.unreachable_streak = 0;
        self.unreachable_notified = false;
        self.unreachable_since = None;
    }

    fn clear_unusable(&mut self) {
        self.unusable_streak = 0;
        self.unusable_notified = false;
        self.unusable_since = None;
    }

    fn clear_leaderless(&mut self) {
        self.leaderless_streak = 0;
        self.leaderless_probes = 0;
        self.leaderless_notified = false;
        self.leaderless_since = None;
    }

    fn observe_unreachable(&mut self, reason: &str, now: Instant) -> Vec<Report> {
        // An `Unreachable` CONTRADICTS the UNKNOWN streak — this probe did not
        // reach a coord that could answer unreadably. Left un-cleared, a load
        // balancer alternating the two arms inflated `unusable_streak` without
        // either predicate ever describing what was happening.
        self.clear_unusable();
        self.unreachable_streak = self.unreachable_streak.saturating_add(1);
        self.unreachable_since.get_or_insert(now);
        if self.unreachable_streak < CADENCES_TO_FIRE || self.unreachable_notified {
            return Vec::new();
        }
        self.unreachable_notified = true;
        vec![Report {
            class: FaultClass::Unreachable,
            summary: format!(
                "coord has not answered this runner on {} consecutive probes, over a measured \
                 {}s. Last failure: {reason}",
                self.unreachable_streak,
                elapsed_secs(self.unreachable_since, now),
            ),
            raw: reason.to_string(),
            post_finding: false,
        }]
    }

    /// Predicate (iv). coord SPOKE, so (i) is settled in the negative and
    /// (ii)/(iii) are left exactly where they were — an absent observation is
    /// not a contradicting one. What is NOT left alone is the operator: held
    /// for [`CADENCES_TO_FIRE`] probes this becomes a reported class, because
    /// every way it becomes permanent leaves coord liveness unobserved while
    /// the runner looks healthy.
    fn observe_unusable(&mut self, reason: &str, now: Instant) -> Vec<Report> {
        self.clear_unreachable();
        self.unusable_streak = self.unusable_streak.saturating_add(1);
        self.unusable_since.get_or_insert(now);
        if self.unusable_streak == 1 || self.unusable_streak.is_multiple_of(UNUSABLE_REWARN_EVERY) {
            warn!(
                probes = self.unusable_streak,
                reason = %reason,
                "coord outside observer: coord answered but this runner could not read \
                 its worker ledger — coord liveness is UNKNOWN here, not healthy"
            );
        }
        if self.unusable_streak < CADENCES_TO_FIRE || self.unusable_notified {
            return Vec::new();
        }
        self.unusable_notified = true;
        vec![Report {
            class: FaultClass::LivenessUnknown,
            summary: format!(
                "coord answered but this runner could not read its worker ledger on {} \
                 consecutive probes, over a measured {}s — coord liveness is UNOBSERVED from \
                 here, which is not the same as healthy. Last reason: {reason}",
                self.unusable_streak,
                elapsed_secs(self.unusable_since, now),
            ),
            raw: reason.to_string(),
            // Never. The one credential this runner has is the credential that
            // just failed to read a ledger, so a finding POST is the same call
            // with the same outcome — and the module's contract is to write
            // only to a coord that just ANSWERED a read.
            post_finding: false,
        }]
    }

    fn observe_dead(&mut self, read: &LedgerRead) -> Vec<Report> {
        // Everything this read still has an OPINION about — the rows it
        // classified dead, and the rows it EXCLUDED as a deploy-roll artifact.
        //
        // The exclusion belongs here for the same reason an `Unusable` does not
        // clear (iii): an absent observation is not a contradicting one.
        // `verdict_from_rolled_off_replica` FLAPS across adjacent samples
        // (coord finding `adac6331-724c-4a5f-be20-e21672d21b5d`), and a retain
        // over `dead_leader_gated` ALONE read a flap-to-excluded as a recovery
        // — dropping the latch, so the flap back re-paged. At the ~60s cadence
        // that is a card, an incident line and a finding POST every ~2 minutes,
        // forever. Holding the latch across an exclusion costs only that a
        // genuinely recovered-then-re-died worker is re-armed one probe later.
        let still_judged: BTreeSet<&str> = read
            .dead_leader_gated
            .iter()
            .map(|w| w.name.as_str())
            .chain(read.rolled_off_excluded.iter().map(String::as_str))
            .collect();
        // Re-arm anything that recovered, so a second death pages again.
        self.dead_notified
            .retain(|n| still_judged.contains(n.as_str()));

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

    fn observe_leaderless(&mut self, read: &LedgerRead, now: Instant) -> Vec<Report> {
        if read.leaderless.is_empty() {
            // An empty list is only EVIDENCE of a leader when the list was
            // whole. coord's 40-row cap discards severity-0 rows first and
            // `not_leader_here` IS severity 0 (see
            // `LedgerRead::list_truncated`), so the very failure this
            // predicate exists for — coord loses its leader, every
            // leader-gated worker flips at once, the population blows past
            // the cap — presents as `leaderless: []`. Treating that as health
            // reset the streak and cleared the latch; with the boundary
            // flapping around 40 it reset the streak every other sample, so
            // `CADENCES_TO_FIRE` was never reached at all: a permanent false
            // negative out of flapping input.
            //
            // So this cycle is UNKNOWN. Leave the streak, the probe count and
            // the latch exactly where they are — the same arm
            // `observe_unusable` already takes, for the same reason.
            if read.leaderless_is_underread() {
                return Vec::new();
            }
            self.clear_leaderless();
            return Vec::new();
        }
        if self.leaderless_since.is_none() {
            self.leaderless_since = Some(now);
            // `observe_at` only counts folds against an OPEN episode, so the
            // one that OPENS it counts itself.
            self.leaderless_probes = 1;
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
                 `no_leader_tick` on {} of the last {} probes, over a measured {}s — {}",
                names.len(),
                self.leaderless_streak,
                self.leaderless_probes.max(self.leaderless_streak),
                elapsed_secs(self.leaderless_since, now),
                names.join(", ")
            ),
            raw: render_read(read),
            post_finding: true,
        }]
    }
}

/// Seconds between an episode's opening instant and `now`, measured.
///
/// `None` is impossible on the paths that call this (every one sets the
/// instant before reading it) and returns 0 rather than panicking — an
/// observer must not die reporting.
fn elapsed_secs(since: Option<Instant>, now: Instant) -> u64 {
    since
        .map(|t| now.saturating_duration_since(t).as_secs())
        .unwrap_or(0)
}

/// Render a successful read for a notification body / incident line / finding.
fn render_read(read: &LedgerRead) -> String {
    serde_json::to_string_pretty(&json!({
        "counts_dead": read.counts_dead,
        "counts_not_leader_here": read.counts_not_leader_here,
        "dead_leader_gated": read.dead_leader_gated.iter().map(worker_json).collect::<Vec<_>>(),
        "leaderless": read.leaderless.iter().map(worker_json).collect::<Vec<_>>(),
        "excluded_as_rolled_off_replica": read.rolled_off_excluded,
        "non_nominal_workers_truncated": read.list_truncated,
        "dead_unaccounted_for_in_the_list": read.dead_unaccounted(),
        "not_leader_here_unaccounted_for_in_the_list": read.leaderless_unaccounted(),
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
        // The two shapes that would otherwise be SILENT: coord counted
        // non-nominal workers whose rows its own 40-row cap kept out of the
        // list, so the predicate over them is unsettled. Warned here rather
        // than in `ObserverState::observe_at`, which is pure by contract —
        // and warned even on a cycle that produces no report, because that is
        // precisely the cycle where the absence of a report means nothing.
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
            // The one that actually fires in practice: the cap discards
            // severity-0 rows FIRST, and `not_leader_here` is severity 0.
            if read.leaderless_is_underread() {
                warn!(
                    counts_not_leader_here = read.counts_not_leader_here,
                    unaccounted = read.leaderless_unaccounted(),
                    named = read.leaderless.len(),
                    "coord outside observer: coord's non_nominal_workers list was TRUNCATED and \
                     counts.not_leader_here exceeds what this read could name — predicate (iii) \
                     is UNKNOWN on this cycle, not clear, and the leaderless streak was held \
                     rather than reset. The cap drops `not_leader_here` BEFORE `dead`, so this \
                     is the blind spot a lost coord leader hides in. Call coord_query_workers \
                     with a `name` for the per-replica rows."
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

    /// A truncated body whose `counts.not_leader_here` the (capped) list
    /// cannot account for — the shape a coord that has LOST ITS LEADER
    /// produces once the non-nominal population passes the 40-row cap, since
    /// that cap discards severity-0 `not_leader_here` rows FIRST.
    fn truncated_leaderless_body(not_leader_here: u64, non_nominal: JsonValue) -> JsonValue {
        let mut body = truncated_ledger_body(0, non_nominal);
        body["components"]["counts"]["not_leader_here"] = json!(not_leader_here);
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
        let dead = ledger_body(
            1,
            json!([dead_row("merge_scheduler.dispatch", true, false)]),
        );
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
            json!([
                dead_row("a.sweep", true, false),
                dead_row("b.sweep", true, false)
            ]),
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
        assert!(
            read.list_truncated,
            "coord's own flag is carried, not dropped"
        );
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
            reports[0]
                .raw
                .contains("\"dead_unaccounted_for_in_the_list\": 2"),
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

    // ── F1: the truncation blind spot on predicate (iii) ────────────────

    #[test]
    fn a_truncated_list_that_capped_every_leaderless_row_is_under_read() {
        // coord counted 7 leader-gated workers reporting `no_leader_tick`.
        // Its 40-row cap sorts worst-first and `not_leader_here` is severity
        // 0, so the rows that survived the cut are the DEAD ones and not one
        // leaderless row reached this runner.
        let body = truncated_leaderless_body(7, json!([dead_row("a.sweep", true, false)]));
        let ProbeOutcome::Read(read) = classify_ledger(&body) else {
            panic!("expected a Read");
        };
        assert!(
            read.leaderless.is_empty(),
            "the cap took every leaderless row"
        );
        assert_eq!(
            read.counts_not_leader_here, 7,
            "the COUNT is over the full set"
        );
        assert_eq!(read.leaderless_unaccounted(), 7);
        // The predicate the `probe_and_report` warning is wired on — the same
        // way `dead_is_underread` is. An empty list here is UNKNOWN, never a
        // fleet with a leader.
        assert!(
            read.leaderless_is_underread(),
            "an empty leaderless list on a TRUNCATED read is UNKNOWN, not health"
        );
    }

    #[test]
    fn an_untruncated_empty_leaderless_list_is_still_a_clean_read() {
        // The other half of the same predicate: with the list whole, an empty
        // leaderless population really is evidence of a leader — and a
        // truncated list whose COUNT is zero is too, because the counts are
        // over the full set.
        let whole = ledger_body(0, json!([dead_row("a.sweep", true, false)]));
        let ProbeOutcome::Read(read) = classify_ledger(&whole) else {
            panic!("expected a Read");
        };
        assert!(!read.leaderless_is_underread());

        let capped_but_counted_zero =
            truncated_leaderless_body(0, json!([dead_row("a.sweep", true, false)]));
        let ProbeOutcome::Read(read) = classify_ledger(&capped_but_counted_zero) else {
            panic!("expected a Read");
        };
        assert!(read.list_truncated);
        assert!(
            !read.leaderless_is_underread(),
            "`counts.not_leader_here == 0` settles (iii) however short the list is"
        );
    }

    #[test]
    fn a_truncated_leaderless_cycle_neither_advances_nor_resets_the_streak() {
        let dark = ledger_body(0, json!([no_leader_row("alert_pageout_worker")]));
        // The surviving rows are follower-plane, so predicate (ii) is silent
        // and this test is about (iii) alone.
        let capped =
            truncated_leaderless_body(7, json!([dead_row("some.follower.loop", false, false)]));
        let mut state = ObserverState::default();
        assert!(state.observe(&classify_ledger(&dark)).is_empty(), "probe 1");
        assert!(state.observe(&classify_ledger(&dark)).is_empty(), "probe 2");
        // THIS is the cycle that used to read as a clean fleet: it reset the
        // streak to 0 and cleared the latch on an empty list it had no right
        // to trust. It must do neither.
        assert!(
            state.observe(&classify_ledger(&capped)).is_empty(),
            "an UNKNOWN cycle reports nothing itself…"
        );
        assert_eq!(
            state.observe(&classify_ledger(&dark)).len(),
            1,
            "…and the streak it HELD reaches the cadence on the next real observation"
        );
    }

    #[test]
    fn a_leaderless_population_flapping_across_the_cap_still_fires() {
        // The permanent false negative this fix exists for. A non-nominal
        // population oscillating around 40 alternately admits and caps the
        // leaderless rows; resetting on the capped samples meant the streak
        // never reached CADENCES_TO_FIRE at all, however long coord ran with
        // no leader.
        let dark = ledger_body(0, json!([no_leader_row("merge_scheduler.dispatch")]));
        let capped =
            truncated_leaderless_body(41, json!([dead_row("some.follower.loop", false, false)]));
        let mut state = ObserverState::default();
        let mut fired = Vec::new();
        for _ in 0..4 {
            fired.extend(state.observe(&classify_ledger(&dark)));
            fired.extend(state.observe(&classify_ledger(&capped)));
        }
        assert_eq!(fired.len(), 1, "one report, not zero: {fired:?}");
        assert_eq!(fired[0].class, FaultClass::NoLeader);
    }

    #[test]
    fn a_capped_leaderless_cycle_does_not_clear_the_latch_either() {
        // Latch AND streak: clearing the latch alone would re-page the same
        // episode every time the population crossed the cap.
        let dark = ledger_body(0, json!([no_leader_row("gate_evaluation_sweep")]));
        let capped = truncated_leaderless_body(9, json!([]));
        let mut state = ObserverState::default();
        for _ in 0..3 {
            state.observe(&classify_ledger(&dark));
        }
        let mut pages = 0;
        for _ in 0..10 {
            pages += state.observe(&classify_ledger(&capped)).len();
            pages += state.observe(&classify_ledger(&dark)).len();
        }
        assert_eq!(pages, 0, "one episode, one notification");
    }

    // ── F4: an absent key is not an empty list ──────────────────────────

    #[test]
    fn a_body_without_a_non_nominal_workers_key_is_unusable_not_a_clean_fleet() {
        // The asymmetry that WAS the bug: `counts` already refused, so a
        // renamed list key left `counts` parsing, `list_truncated` reading
        // false and every under-read predicate reading false — a permanently
        // healthy verdict with no warning anywhere.
        let mut body = ledger_body(0, json!([]));
        body["components"]
            .as_object_mut()
            .unwrap()
            .remove("non_nominal_workers");
        match classify_ledger(&body) {
            ProbeOutcome::Unusable { reason } => assert!(reason.contains("ABSENT"), "{reason}"),
            other => panic!("an absent list is UNKNOWN, not an empty one: {other:?}"),
        }
    }

    #[test]
    fn a_non_nominal_workers_key_of_the_wrong_type_is_unusable() {
        let mut body = ledger_body(0, json!([]));
        body["components"]["non_nominal_workers"] = json!({"a.sweep": "dead"});
        match classify_ledger(&body) {
            ProbeOutcome::Unusable { reason } => assert!(reason.contains("object"), "{reason}"),
            other => panic!("expected Unusable, got {other:?}"),
        }
    }

    #[test]
    fn counts_missing_either_count_this_module_reads_is_unusable() {
        for key in ["dead", "not_leader_here"] {
            let mut body = ledger_body(0, json!([]));
            body["components"]["counts"]
                .as_object_mut()
                .unwrap()
                .remove(key);
            assert!(
                matches!(classify_ledger(&body), ProbeOutcome::Unusable { .. }),
                "an absent `counts.{key}` is UNKNOWN, not zero"
            );
        }
    }

    // ── F5: a flapping exclusion is not a recovery ──────────────────────

    #[test]
    fn a_flapping_rolled_off_flag_does_not_re_page() {
        // `verdict_from_rolled_off_replica` flaps across adjacent samples
        // (coord finding adac6331-724c-4a5f-be20-e21672d21b5d). Retaining the
        // latch over `dead_leader_gated` ALONE read a flap-to-excluded as a
        // recovery, so the flap back paged again: at the 60s cadence, a card
        // plus an incident line plus a finding POST every ~2 minutes, forever.
        let judged = ledger_body(1, json!([dead_row("pr_merge.reconcile_once", true, false)]));
        let excluded = ledger_body(1, json!([dead_row("pr_merge.reconcile_once", true, true)]));
        let mut state = ObserverState::default();
        assert_eq!(
            state.observe(&classify_ledger(&judged)).len(),
            1,
            "the first death pages"
        );
        let mut pages = 0;
        for _ in 0..10 {
            pages += state.observe(&classify_ledger(&excluded)).len();
            pages += state.observe(&classify_ledger(&judged)).len();
        }
        assert_eq!(
            pages, 0,
            "an exclusion HOLDS the latch — it is an absent observation, not a contradicting one"
        );

        // …and a real recovery — the row gone from the ledger entirely —
        // still re-arms it.
        assert!(state
            .observe(&classify_ledger(&ledger_body(0, json!([]))))
            .is_empty());
        assert_eq!(
            state.observe(&classify_ledger(&judged)).len(),
            1,
            "a genuine recovery still re-arms the episode"
        );
    }

    // ── F3: predicate (iv) — a blind watcher says so ────────────────────

    #[test]
    fn consecutive_unusable_probes_reach_the_cadence_and_surface() {
        let mut state = ObserverState::default();
        let out = ProbeOutcome::Unusable {
            reason: "coord REACHED and REJECTED this runner's credential: HTTP 401".into(),
        };
        assert!(state.observe(&out).is_empty(), "probe 1");
        assert!(state.observe(&out).is_empty(), "probe 2");
        let reports = state.observe(&out);
        assert_eq!(
            reports.len(),
            1,
            "a watcher that has gone blind must SAY so"
        );
        assert_eq!(reports[0].class, FaultClass::LivenessUnknown);
        assert!(
            !reports[0].post_finding,
            "the credential that could not read a ledger is the credential that would post"
        );
        assert!(
            reports[0].summary.contains("not the same as healthy"),
            "{}",
            reports[0].summary
        );
        assert!(state.observe(&out).is_empty(), "probe 4 does not re-notify");

        // A readable answer re-arms it, so a SECOND blind episode is reported.
        assert!(state
            .observe(&classify_ledger(&ledger_body(0, json!([]))))
            .is_empty());
        for _ in 0..2 {
            assert!(state.observe(&out).is_empty());
        }
        assert_eq!(state.observe(&out).len(), 1);
    }

    #[test]
    fn coords_own_no_observation_verdict_reaches_the_operator_too() {
        // Not a credential fault — coord's own honest UNKNOWN, held. Same
        // class, because the consequence is identical: (ii) and (iii) are
        // unobserved from here for as long as it lasts.
        let body = json!({
            "instance": "workers",
            "drift_subclass": "workers:no_observation",
            "components": {"note": "coord.worker_heartbeats is empty"},
        });
        let mut state = ObserverState::default();
        let mut fired = Vec::new();
        for _ in 0..3 {
            fired.extend(state.observe(&classify_ledger(&body)));
        }
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].class, FaultClass::LivenessUnknown);
        assert_eq!(fired[0].class.error_code(), "COORD_LIVENESS_UNKNOWN");
    }

    // ── F8: an Unreachable contradicts the UNKNOWN streak ───────────────

    #[test]
    fn alternating_unreachable_and_unusable_inflates_neither_streak() {
        // A flaky load balancer. Neither predicate may creep to its cadence
        // on evidence the other arm contradicted.
        let mut state = ObserverState::default();
        let unusable = ProbeOutcome::Unusable {
            reason: "a shape this build cannot parse".into(),
        };
        let down = ProbeOutcome::Unreachable {
            reason: "connection refused".into(),
        };
        for _ in 0..10 {
            assert!(state.observe(&unusable).is_empty());
            assert!(state.observe(&down).is_empty());
        }
        // …and each still fires on its own uninterrupted evidence.
        assert!(state.observe(&unusable).is_empty());
        assert!(state.observe(&unusable).is_empty());
        assert_eq!(state.observe(&unusable).len(), 1);
    }

    // ── F6: elapsed is measured, and the count is the real one ──────────

    #[test]
    fn the_summaries_carry_a_measured_elapsed_not_streak_times_the_nominal_period() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);

        // (i) `ticks_per_probe` rounds the real period UP with `div_ceil`, so
        // `streak * PROBE_PERIOD_SECS` was never the elapsed time.
        let down = ProbeOutcome::Unreachable {
            reason: "connection refused".into(),
        };
        let mut state = ObserverState::default();
        state.observe_at(&down, at(0));
        state.observe_at(&down, at(63));
        let reports = state.observe_at(&down, at(127));
        assert_eq!(reports.len(), 1);
        assert!(
            reports[0].summary.contains("3 consecutive probes"),
            "{}",
            reports[0].summary
        );
        assert!(
            reports[0].summary.contains("measured 127s"),
            "{}",
            reports[0].summary
        );
        assert!(
            !reports[0].summary.contains("180"),
            "the fabricated streak x nominal-period figure is gone: {}",
            reports[0].summary
        );

        // (iii) the streak deliberately SURVIVES probes that observed nothing,
        // so "consecutive probes" was never true of it either.
        let dark = classify_ledger(&ledger_body(
            0,
            json!([no_leader_row("alert_pageout_worker")]),
        ));
        let blind = ProbeOutcome::Unusable {
            reason: "credential refused".into(),
        };
        let mut state = ObserverState::default();
        state.observe_at(&dark, at(0));
        state.observe_at(&blind, at(70));
        state.observe_at(&dark, at(140));
        let reports = state.observe_at(&dark, at(210));
        assert_eq!(reports.len(), 1);
        assert!(
            reports[0].summary.contains("on 3 of the last 4 probes"),
            "the report counts what it actually saw: {}",
            reports[0].summary
        );
        assert!(
            reports[0].summary.contains("measured 210s"),
            "{}",
            reports[0].summary
        );
    }

    #[test]
    fn the_report_body_names_both_under_read_populations() {
        let body = truncated_leaderless_body(7, json!([dead_row("a.sweep", true, false)]));
        let raw = match classify_ledger(&body) {
            ProbeOutcome::Read(read) => render_read(&read),
            other => panic!("expected a Read, got {other:?}"),
        };
        assert!(
            raw.contains("\"not_leader_here_unaccounted_for_in_the_list\": 7"),
            "{raw}"
        );
        assert!(raw.contains("\"counts_not_leader_here\": 7"), "{raw}");
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
            FaultClass::LivenessUnknown,
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
        assert_eq!(
            FaultClass::LivenessUnknown.breadcrumb_reason(),
            "coord_liveness_unknown"
        );

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
        let mut fired = Vec::new();
        for _ in 0..5 {
            let outcome =
                probe_door(&client, &format!("http://{addr}/mcp"), TenantScope::Device).await;
            match &outcome {
                ProbeOutcome::Unusable { reason } => {
                    assert!(reason.contains("coord is up"), "{reason}")
                }
                other => panic!("a 401 must be Unusable, got {other:?}"),
            }
            fired.extend(state.observe(&outcome));
        }
        // It never renders as `coord is down` — and it no longer renders as
        // NOTHING either. A rotated credential is permanent until someone
        // acts, so the operator hears it once, as an UNKNOWN.
        assert_eq!(fired.len(), 1, "one report across five probes");
        assert_eq!(fired[0].class, FaultClass::LivenessUnknown);
        assert!(
            !fired.iter().any(|r| r.class == FaultClass::Unreachable),
            "a credential fault must never render as `coord is down`"
        );
    }

    #[tokio::test]
    async fn a_non_json_body_is_unusable_not_unreachable() {
        // An intercepting proxy or a WAF error page: HTTP 200, HTML body.
        // coord is up (something answered on its behalf) and no ledger was
        // read, which is predicate (iv), never predicate (i).
        use axum::routing::post;
        use axum::Router;
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app: Router = Router::new().route(
            "/mcp",
            post(|| async { "<html><body>502 Bad Gateway</body></html>" }),
        );
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let outcome = probe_door(&client, &format!("http://{addr}/mcp"), TenantScope::Device).await;
        match &outcome {
            ProbeOutcome::Unusable { reason } => {
                assert!(reason.contains("not JSON"), "{reason}");
                assert!(
                    reason.contains("502 Bad Gateway"),
                    "the body is quoted, so the operator sees WHAT answered: {reason}"
                );
            }
            other => panic!("a 200 carrying HTML is coord-is-up-and-unreadable, got {other:?}"),
        }

        let mut state = ObserverState::default();
        let mut fired = Vec::new();
        for _ in 0..3 {
            fired.extend(state.observe(&outcome));
        }
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].class, FaultClass::LivenessUnknown);
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
                post(|AxumState(b): AxumState<Arc<JsonValue>>| async move { Json(rpc_ok(&b)) }),
            )
            .with_state(body.clone());
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let outcome = probe_door(&client, &format!("http://{addr}/mcp"), TenantScope::Device).await;
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
