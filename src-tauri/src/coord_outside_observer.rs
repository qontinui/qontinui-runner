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
//! | (iii) [`FaultClass::NoLeader`] | coord reports leader-gated workers with no live replica running them (`counts.not_leader_here`, whether or not their rows survived the cap) | 3 probes |
//! | (iv) [`FaultClass::LivenessUnknown`] | this runner has had no USABLE observation of coord | 3 consecutive probes |
//!
//! (ii) carries no cadence requirement because `dead` is already a debounced
//! verdict: coord's ledger only reaches it after >10 tick intervals of
//! silence. (i), (iii) and (iv) are single-sample facts and get the plan's
//! `≥ 3 cadences`.
//!
//! (iv) is not a fault in coord. It is a fault in this runner's VIEW of
//! coord, and it gets a surface for exactly the reason the other three do:
//! a watcher that has gone blind must SAY SO on the surface it would have
//! used, or its silence is indistinguishable from a clean fleet.
//!
//! **(iv)'s counter is the one thing here that no other arm resets**
//! ([`ObserverState::unobserved_streak`]). That is deliberate and it is the
//! whole point of the class: the states this exists to catch are the ones in
//! which no SINGLE predicate ever holds long enough to fire, while coord is
//! nonetheless unobserved throughout. A rotated or expired device credential
//! (or a WAF 401), coord's own `workers:no_observation`, and a response-shape
//! change this build cannot parse each pin `Unusable`; a load balancer with
//! one target serving a 200 maintenance page and one refusing connections
//! ALTERNATES `Unusable` and `Unreachable`, and each arm legitimately
//! contradicts the other's streak — so before this counter existed, neither
//! (i) nor (iv) could ever reach `CADENCES_TO_FIRE` and a wholly-down coord
//! produced no card, no incident line and no finding, indefinitely. A counter
//! that only a SETTLING read clears cannot be starved that way.
//!
//! It is suppressed while (i) has already fired, because "coord has not
//! answered on N consecutive probes" is a strictly stronger statement about
//! the same episode and a second card beside it is noise, not a second fact.
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
//! exists for, is the one the cap silences.
//!
//! The answer is to stop reading (iii) off the LIST at all. `counts` is over
//! the full worker set and is never capped, and every worker inside
//! `counts.not_leader_here` is leader-gated and non-nominal BY CONSTRUCTION
//! (the proof is on [`LedgerRead::counts_not_leader_here`]) — so that count
//! IS predicate (iii)'s population, and the list only ever supplies NAMES.
//! [`ObserverState::observe_leaderless`] therefore advances on the count and
//! reports with whatever names survived, saying so when they did not. A count
//! coord affirms is an observation, not an UNKNOWN: "coord says twelve
//! leader-gated workers have no leader and I cannot name them" is an outage
//! report, not a blind spot.
//!
//! What remains of the truncation story for (ii): `counts.dead` genuinely
//! spans the follower plane as well, so a surplus there cannot be read as
//! (ii) FIRING, and only [`LedgerRead::dead_is_underread`] — a surplus ON A
//! TRUNCATED LIST, after the follower-plane rows this read classified are
//! subtracted — is evidence of a truncation blind spot. That asymmetry with
//! [`LedgerRead::leaderless_is_underread`] is load-bearing rather than an
//! oversight.
//!
//! **It settles truncation only, and an earlier revision of this paragraph
//! wrongly read as settling (ii)'s OBSERVABILITY too.** It does not: "a
//! surplus on `counts.dead` is ordinary" says why that count cannot DRIVE
//! (ii), and says nothing about whether (ii) was observed. So (ii) needs its
//! own observability defense, and [`LedgerRead::unclassified`] is it — the
//! rows this build could not place in ANY bucket. Predicate (ii)'s population
//! is read off the list and only off the list, so a list this build cannot
//! READ leaves (ii) unobserved however the counts fall, with no truncation in
//! it at all: retype `leader_gated` to the string `"true"`, or rename
//! `status`, and every row falls out of the classifier while
//! `non_nominal_workers_truncated` stays `false`. `unclassified` catches both
//! retypes and renames, it is a property of this build's own reading rather
//! than of a coord field that could itself be renamed next, and it rides
//! [`LedgerRead::unobserved_reason`] into predicate (iv).
//!
//! **Rows that never arrived.** `unclassified` covers rows that are PRESENT
//! and UNREADABLE. It cannot cover rows that are ABSENT while
//! `non_nominal_workers_truncated` reads `false`, because there is no row to
//! fail: coord emits `counts.dead = 12`, `non_nominal_workers: []`,
//! `non_nominal_workers_truncated: false`, and neither the classifier nor the
//! truncation-gated [`LedgerRead::dead_is_underread`] has anything to say.
//! Not reachable against coord as it stands (the list and the flag are
//! computed from the same `non_nominal` vector in the same function), but
//! reachable the moment anyone filters the listed rows — authorization
//! scoping, a dedupe, a `name.is_some()` guard — without touching the flag's
//! formula.
//!
//! [`LedgerRead::rows_absent`] closes it: `counts.non_nominal` against
//! [`LedgerRead::listed_rows`] on an untruncated read. The two defenses are
//! **complementary, not competing** — `unclassified` catches a whole list this
//! build cannot READ, a `listed_rows` shortfall catches a list coord did not
//! SEND, and neither shape is a subset of the other. Both ride
//! [`LedgerRead::unobserved_reason`] into predicate (iv). So, stated exactly:
//! the count governs firing, the classifier governs the observability of rows
//! that ARRIVED, and the shortfall governs the observability of rows that
//! did not.

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

/// How often an UNOBSERVED streak re-warns, in probes. The first one always
/// warns; after that roughly hourly, so a runner whose credential has gone
/// dark says so without burying the log.
///
/// It counts the streak that NOTHING but a settling read clears, which is the
/// only counter on which "roughly hourly" is true. Counted off the old
/// per-arm `Unusable` streak it was a promise the code could not keep: an
/// alternating door reset that streak to 1 on every other probe, so `== 1`
/// matched forever and the warn fired every ~2 minutes for as long as the
/// flap lasted.
const UNOBSERVED_REWARN_EVERY: u32 = 60;

/// The coord tool this observer calls. Swapped in for the doctor's
/// `tools/list` — same door, a question with an answer in it.
const WORKERS_TOOL: &str = "coord_query_workers";

/// Topic every finding this module posts is filed under (plan Phase 3b.2).
const FINDING_TOPIC: &str = "coord-merge-train";

/// coord's `FINDING_TITLE_MAX_BYTES` (`qontinui-coord` `findings.rs`): a
/// longer title is a 400, never truncated server-side — and this observer
/// posts once per episode, so a refused title is a lost finding.
const FINDING_TITLE_MAX_BYTES: usize = 500;

/// coord's `FINDING_BODY_MAX_BYTES` for `kind: "investigation"`
/// (`qontinui-coord` `findings.rs`): refused, never truncated, over it.
const FINDING_BODY_MAX_BYTES: usize = 8 * 1024;

/// Appended where [`finding_body`] cut the evidence. [`post_finding`] keys
/// its full-read log line on it, which is what makes the promise true.
const FINDING_EVIDENCE_CUT: &str = "\n… [evidence cut to fit coord's finding-body cap; the full \
                                    read is in the posting runner's log]";

/// The honest-unknown subclass coord returns for a ledger it cannot read
/// (pre-migration, or a replica that has never observed a row). Never a clean
/// fleet — see the module doc.
const NO_OBSERVATION_SUBCLASS: &str = "workers:no_observation";

/// coord's rollup status for a worker whose loop task is gone.
const DEAD_STATUS: &str = "dead";

/// Every rollup status `qontinui-coord`'s `WorkerLiveness::as_str` can emit.
///
/// A listed row carrying one of these was READ, whether or not this module
/// holds a predicate over it. Anything else is a shape this build cannot
/// read, and lands in [`LedgerRead::unclassified`] — so this constant is the
/// line between "deliberately not my class" and "I could not tell what this
/// row was", and widening it silently is how predicate (ii) goes blind again.
const KNOWN_ROLLUP_STATUSES: [&str; 4] = ["alive", "stale", DEAD_STATUS, "not_leader_here"];

/// How many `unclassified` labels a SUMMARY may carry.
///
/// [`LedgerRead::unobserved_reason`] becomes `Report.summary`, and that one
/// string is both the `wedge-incidents.log` line and `UserFacingError.message`
/// — the operator card's HEADLINE, not its collapsed details. Unbounded, a
/// `join("; ")` over coord's own 40-row cap with ordinary worker names
/// measures at ~3.5 KB in one log line and one headline, and this runner does
/// not enforce coord's cap, so nothing else bounds it. Every other list in
/// this module renders into the body instead.
///
/// Nothing is lost by the bound: [`render_read`] already carries the FULL
/// list on the same card as `rows_this_build_could_not_classify`.
const UNCLASSIFIED_IN_SUMMARY: usize = 5;

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
    /// (iv) this runner has had no USABLE observation of coord for
    /// [`CADENCES_TO_FIRE`] probes running — it did not answer, answered
    /// unreadably, or answered readably while leaving a predicate unsettled.
    ///
    /// Not a statement about coord's health — a statement that this runner
    /// has no statement. It is a class rather than a log line because the
    /// states that produce it (a dead credential, a shape change, coord's own
    /// `workers:no_observation`, a door alternating between two failure
    /// modes) are typically PERMANENT until someone acts, and an observer
    /// that is permanently blind while the runner looks healthy is the exact
    /// defect this whole plan is about.
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
                "A coord background singleton has stopped doing its work, so whatever \
                 it drives (merge dispatch, gate sweeps, alert page-out) is stalled. \
                 `last_tick_secs_ago` may be a FOLLOWER writing `follower_skip` — a \
                 replica saying \"not me\", not the work running — so a fresh value \
                 there does not refute this; read `last_work_tick_secs_ago`, the \
                 freshest write that WAS the work running (null is UNKNOWN, never \
                 fresh). `leader_body_in_flight_secs` is a number when the lease \
                 holder is stuck inside its work; null means nothing positively says a \
                 body is running, which is NOT evidence that none is. \
                 Read `coord_query_workers` with {\"name\": \"<worker>\"} for the \
                 per-replica rows, lease holder first."
            }
            FaultClass::NoLeader => {
                "No coord replica is running the leader-gated plane, so every \
                 detector, pager and gate sweep in coord is idle. Check coord's \
                 leader lease and replica presence; nothing on this runner can \
                 elect one."
            }
            FaultClass::LivenessUnknown => {
                "This is NOT a verdict on coord — it is this runner reporting that it \
                 has no verdict. No probe in the window produced a usable observation, \
                 so predicates (ii) and (iii) have \
                 been unobserved from here for as long as the message says. The usual \
                 causes, in order: an expired or rotated device credential (run \
                 `coord doctor` — a 401/403 is reported here, never as `coord is \
                 down`); the `coord_query_workers` tool masked for this principal; \
                 coord's own `workers:no_observation` verdict, which means its ledger \
                 has no rows to read; a response shape this runner build cannot \
                 parse, which wants a runner upgrade; or a door alternating between \
                 two failure modes, which is coord being wholly unavailable behind a \
                 load balancer whose targets fail differently. The details block \
                 carries the last reason verbatim."
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
    /// The freshest write of ANY kind — INCLUDING a follower's
    /// `follower_skip`, which says "not me", not "the work ran". On
    /// 2026-09-19 this read 3 s on a worker whose lease holder had not
    /// returned for 857 s, and a real train freeze was dismissed off it
    /// (coord finding `4a987e27`). Carried so the card can say so beside the
    /// field that answers the question readers thought this one did.
    pub last_tick_secs_ago: Served<f64>,
    /// The freshest write that WAS the work running (outcome ok / error /
    /// stream_end). `Null` is coord's UNKNOWN — no live replica's MOST RECENT
    /// write was a body execution (rows keep only the last outcome, so coord
    /// cannot say whether the body ran earlier) — and is never rendered as 0
    /// or as fresh.
    pub last_work_tick_secs_ago: Served<f64>,
    /// A LOWER bound on how long the lease holder's body has been in flight,
    /// measured from its row's stale bound (coord `worker_ledger.rs`). A
    /// number beside an old `worst_replica_age_secs` is a loop alive and stuck
    /// inside its work; `Null` is "nothing positively says a body is
    /// running", which is NOT evidence none is — it also reads null before
    /// the stale bound, on an unreadable lease, and on a coord whose
    /// `body_started_at` migration has not applied.
    pub leader_body_in_flight_secs: Served<f64>,
}

/// A field coord may or may not serve, read WITHOUT collapsing its states.
///
/// `Option` cannot carry this: coord's `null` on `last_work_tick_secs_ago`
/// is its own UNKNOWN, and an ABSENT key is an older coord that predates the
/// field — two different facts, and neither is `0` or "not in flight"
/// (served policy `verification-and-evidence`
/// `unknown-must-not-render-as-a-default`). A value of the wrong JSON type is
/// a fourth fact — a shape this build cannot read — and gets its own arm
/// rather than borrowing "absent", which would blame the coord version.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Served<T> {
    /// The key is not in the response: a coord that predates the field.
    #[default]
    Absent,
    /// The key is present and `null`.
    Null,
    /// The key is present with a JSON type this build cannot read.
    Unreadable(&'static str),
    Value(T),
}

impl<T> Served<T> {
    /// Read `key` off `obj` with `parse`, keeping absent, null and
    /// wrong-typed apart.
    fn read(obj: &JsonValue, key: &str, parse: impl Fn(&JsonValue) -> Option<T>) -> Self {
        match obj.get(key) {
            None => Served::Absent,
            Some(JsonValue::Null) => Served::Null,
            Some(v) => parse(v).map_or_else(|| Served::Unreadable(kind_of(v)), Served::Value),
        }
    }

    /// The UNKNOWN wording for every arm but `Value`, `null_means` naming
    /// what coord's own `null` means for this field.
    fn unknown_text(&self, null_means: &str) -> Option<String> {
        match self {
            Served::Value(_) => None,
            Served::Null => Some(format!("UNKNOWN (null: {null_means})")),
            Served::Absent => Some("UNKNOWN (absent: this coord predates the field)".to_string()),
            Served::Unreadable(kind) => Some(format!(
                "UNKNOWN (coord served a {kind} this runner build cannot read)"
            )),
        }
    }
}

impl Served<f64> {
    /// For the raw evidence block: a number, `null`, or a string naming why
    /// there is no number — never a silently dropped key.
    fn to_json(&self) -> JsonValue {
        match self {
            Served::Value(v) => json!(v),
            Served::Null => JsonValue::Null,
            Served::Absent => json!("UNKNOWN: absent — this coord predates the field"),
            Served::Unreadable(kind) => json!(format!("UNKNOWN: unreadable {kind}")),
        }
    }
}

impl Served<String> {
    fn to_json(&self) -> JsonValue {
        match self {
            Served::Value(v) => json!(v),
            Served::Null => JsonValue::Null,
            Served::Absent => json!("UNKNOWN: absent — this coord predates the field"),
            Served::Unreadable(kind) => json!(format!("UNKNOWN: unreadable {kind}")),
        }
    }
}

/// The lease holder, for the `WorkerDead` card. coord holds ONE leader lease
/// for the whole leader-gated plane. The value is the replica LAST RECORDED
/// in that lease — coord does not check it for expiry, and a crashed leader
/// stays recorded until a follower takes over, which is exactly when this
/// card fires — so it is labelled as such rather than as a live holder.
fn render_lease_holder(holder: &Served<String>) -> String {
    match holder {
        Served::Value(id) => format!("replica `{}` (last recorded)", truncate(id, 64)),
        other => other
            .unknown_text("coord could not read a leader lease row")
            .unwrap_or_default(),
    }
}

/// Whether the lease holder's body is in flight. An absent field is
/// UNKNOWN, never "not in flight".
fn render_body_in_flight(in_flight: &Served<f64>) -> String {
    match in_flight {
        // coord's value is a LOWER bound measured from the row's stale bound
        // (`worker_ledger.rs`), so the true duration is longer.
        Served::Value(secs) => format!("in flight at least {secs:.0}s past its stale bound"),
        Served::Null => "not observed in flight (null: nothing positively says a body is \
                         running — not proof the loop is gone)"
            .to_string(),
        other => format!(
            "{} — NOT \"not in flight\"",
            other.unknown_text("").unwrap_or_default()
        ),
    }
}

fn render_age(age: &Served<f64>, null_means: &str) -> String {
    match age {
        Served::Value(secs) => format!("{secs:.0}s ago"),
        other => other.unknown_text(null_means).unwrap_or_default(),
    }
}

/// A successful read of coord's worker ledger.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LedgerRead {
    /// Leader-gated workers that rolled up `dead` and survived the rolled-off
    /// discrimination — predicate (ii)'s population.
    pub dead_leader_gated: Vec<WorkerVerdict>,
    /// Leader-gated workers no live replica is running (`reason:
    /// no_leader_tick`) — the NAMES for predicate (iii). Its population is
    /// [`Self::counts_not_leader_here`], because this list is capped and that
    /// count is not.
    pub leaderless: Vec<WorkerVerdict>,
    /// `components.counts.dead`, verbatim, for the report body.
    pub counts_dead: u64,
    /// `components.counts.not_leader_here`, verbatim — predicate (iii)'s
    /// POPULATION, over the FULL worker set and never capped.
    ///
    /// **It is NOT an analogue of [`Self::counts_dead`], and the difference
    /// is the whole of predicate (iii).** `counts.dead` spans the follower
    /// plane, so a surplus over the leader-gated rows this module classifies
    /// does not on its own say a dead worker went unseen, and that count
    /// therefore cannot DRIVE predicate (ii) the way this one drives (iii).
    /// It says nothing about whether (ii) was OBSERVED — that half is
    /// [`Self::unclassified`]'s, and conflating the two is how (ii) stayed
    /// blind to a `leader_gated` retype long after (iii) was fixed.
    /// `counts.not_leader_here` needs neither defense: in `qontinui-coord`,
    ///
    /// * `WorkerLiveness::NotLeaderHere` is reachable only from
    ///   `last_outcome == FollowerSkip` (`worker_ledger.rs`);
    /// * a `FollowerSkip` row is written only under
    ///   `if spec.leader_gated && !state.leader.is_leader()`;
    /// * the rollup sets `leader_gated = group.iter().any(|r| r.leader_gated)`,
    ///   and every `NotLeaderHere` rollup is non-nominal —
    ///   `(false, Some("no_leader_tick"))`.
    ///
    /// So **every worker inside this count is leader-gated and non-nominal**,
    /// and on an untruncated read `leaderless.len()` equals it EXACTLY. A
    /// surplus is never ordinary here: it is always rows the 40-row cap took,
    /// or rows this build failed to classify. Either way coord has
    /// affirmatively said there are leaderless workers, which is an
    /// observation of (iii) rather than an UNKNOWN about it — so
    /// [`ObserverState::observe_leaderless`] advances on this number and
    /// treats [`Self::leaderless`] as a source of names only.
    pub counts_not_leader_here: u64,
    /// Leader-gated `dead` rows EXCLUDED because coord could prove the verdict
    /// came from a replica a deploy had already replaced. Reported in the body
    /// so an excluded row is visible rather than silently dropped.
    pub rolled_off_excluded: Vec<String>,
    /// `dead` rows this read SAW, classified, and deliberately dropped as
    /// follower-plane (`leader_gated: false`) — not this observer's class,
    /// and visible to coord's own pager.
    ///
    /// Carried for exactly one reason: [`Self::dead_unaccounted`] subtracts
    /// it. Without that subtraction a dead follower row is counted by
    /// `counts.dead` and named by NOTHING this runner tracks, so on any fleet
    /// whose non-nominal population passes the 40-row cap the surplus is
    /// permanently non-zero, [`Self::dead_is_underread`] is permanently true,
    /// and [`Self::unobserved_reason`] reports "the list named none of the N
    /// dead workers" about a list that named every one of them. Routed into
    /// [`ObserverState::unobserved_streak`], which only a SETTLING read
    /// clears, that is a permanent false `LivenessUnknown` card plus an
    /// hourly `warn!` — on the one surface whose entire meaning is *"I have
    /// no verdict"*. A row that was read and judged is not an unread one,
    /// which is the same rule [`Self::rolled_off_excluded`] is subtracted
    /// under.
    pub dead_follower_plane: u64,
    /// Rows in `non_nominal_workers` this build could not place in ANY
    /// bucket, labelled with what made them unreadable — predicate (ii)'s
    /// observability defense, and the F1 half the truncation story never
    /// covered.
    ///
    /// A row is unclassified when `leader_gated` is absent or is not a bool
    /// at all (a `false` is READ and deliberately dropped, and lands in
    /// [`Self::dead_follower_plane`] instead), or when its `status` is
    /// outside [`KNOWN_ROLLUP_STATUSES`] and it carries no `no_leader_tick`
    /// reason to rescue it. Both arms are shape faults in THIS build's
    /// reading, never a judgement about the fleet:
    ///
    /// * a `stale` or `alive` leader-gated row is CLASSIFIED — this module
    ///   deliberately holds no predicate over it, so counting it here would
    ///   false-positive on every ordinary fleet;
    /// * a `leader_gated` retyped to the string `"true"`, and a renamed
    ///   `status`, are the two shapes that used to drop every row silently
    ///   while `non_nominal_workers_truncated` read `false` — a truncation-
    ///   gated under-read predicate cannot see either.
    ///
    /// Not replaceable by [`Self::rows_absent`]: a renamed `status` or a
    /// retyped `leader_gated` leaves the list COMPLETE, so
    /// `listed_rows == counts.non_nominal` exactly while nothing in it
    /// classifies. The two are complementary — this one catches a list that
    /// ARRIVED and could not be read, `rows_absent` a list that arrived SHORT
    /// with coord's truncation flag reading `false`.
    pub unclassified: Vec<String>,
    /// How many rows `non_nominal_workers` carried, for a report that can say
    /// "N of M" rather than "N".
    pub listed_rows: u64,
    /// `components.counts.non_nominal`, verbatim — how many rows coord's list
    /// SHOULD carry before its cap. Read only by [`Self::rows_absent`].
    pub counts_non_nominal: u64,
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
    /// Carried rather than ignored so that [`Self::dead_is_underread`] reads
    /// as UNKNOWN instead of as a clean fleet (served policy
    /// `verification-and-evidence` `unknown-must-not-render-as-a-default`),
    /// and so a (iii) report can say its names were capped away rather than
    /// print an empty list.
    ///
    /// Whether the cap is REACHABLE on a given fleet is a runtime property of
    /// that fleet's worker population, not a fact this file can assert: it is
    /// one row of a live table away from changing, and no dated measurement
    /// of it is cited here. Predicate (iii) no longer depends on the answer —
    /// it reads [`Self::counts_not_leader_here`], which the cap never touches.
    pub list_truncated: bool,
    /// `components.lease_holder_replica_id` — the replica last recorded in
    /// coord's ONE leader lease (not checked for expiry by coord). Three-valued:
    /// an older coord does not serve it on the summary.
    pub lease_holder_replica_id: Served<String>,
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
    ///
    /// **Every row this read had an OPINION about is subtracted, not only the
    /// ones it kept.** All three subtrahends are rows coord counted and this
    /// runner READ: the ones it judged dead-and-leader-gated, the ones it
    /// excluded as a deploy-roll artifact, and the ones it dropped as
    /// follower-plane. Leaving that last class in was a real defect, not a
    /// conservative margin — see [`Self::dead_follower_plane`] for the
    /// permanent false page it produced.
    pub fn dead_unaccounted(&self) -> u64 {
        self.counts_dead
            .saturating_sub(self.dead_leader_gated.len() as u64)
            .saturating_sub(self.rolled_off_excluded.len() as u64)
            .saturating_sub(self.dead_follower_plane)
    }

    /// True when coord counted dead workers this runner could not see,
    /// BECAUSE the list was truncated.
    ///
    /// This is (ii)'s TRUNCATION defense and only that. The other way (ii)
    /// goes blind — a list this build cannot classify, whole and untruncated
    /// — is [`Self::classification_is_underread`]; both feed
    /// [`Self::unobserved_reason`], and neither covers the other's shape.
    pub fn dead_is_underread(&self) -> bool {
        self.list_truncated && self.dead_unaccounted() > 0
    }

    /// `counts.not_leader_here` this read could not NAME.
    ///
    /// Saturating. Unlike [`Self::dead_unaccounted`] this is never ordinary:
    /// every worker the count covers is leader-gated and non-nominal by
    /// coord's own construction (the proof is on
    /// [`Self::counts_not_leader_here`]), so on a whole, parseable read it is
    /// exactly zero.
    pub fn leaderless_unaccounted(&self) -> u64 {
        self.counts_not_leader_here
            .saturating_sub(self.leaderless.len() as u64)
    }

    /// True when coord counted leaderless workers this runner could not name.
    ///
    /// **Deliberately NOT conditioned on [`Self::list_truncated`].** The
    /// conjunct used to be there, justified by a claim that a surplus was
    /// ordinary on an untruncated list; that claim was false (see
    /// [`Self::counts_not_leader_here`]) and it opened a silent false
    /// negative with no truncation in it at all. If coord renames a row key
    /// or changes `leader_gated`'s type, every row falls through
    /// `classify_ledger`'s `_` arm and is dropped: `counts.not_leader_here`
    /// reads 12, the list carries 12 rows, `non_nominal_workers_truncated` is
    /// `false`, `leaderless` is empty — and a truncation-gated predicate
    /// reported a clean fleet for a coord with no leader.
    ///
    /// Reduced to the count alone it says "coord counts leaderless workers
    /// and I named none of them", which cannot false-positive: an untruncated
    /// read that classified every row makes it exactly `false`.
    ///
    /// It is a REPORTING signal, not a gate. Predicate (iii) advances on
    /// [`Self::counts_not_leader_here`] either way; this is what lets the
    /// report say the names were unavailable instead of printing nothing.
    ///
    /// The asymmetry with [`Self::dead_is_underread`] survives, but read it
    /// narrowly: `dead_is_underread` is the TRUNCATION half of (ii) and is
    /// gated on truncation because that is the only shape it claims to
    /// cover. The retype/rename shape this doc names above is (ii)'s too, and
    /// [`Self::classification_is_underread`] carries it — ungated, exactly
    /// like this predicate, and for the same reason.
    pub fn leaderless_is_underread(&self) -> bool {
        self.leaderless_unaccounted() > 0
    }

    /// True when this read established NO predicate of its own: it named no
    /// dead leader-gated worker, and coord's leaderless population is zero
    /// however it is counted.
    ///
    /// It gates the classification arm of [`Self::unobserved_reason`] for the
    /// reason [`ObserverState::observe_unobserved`] suppresses itself under a
    /// fired predicate (i): *"I have no verdict"* is FALSE beside a verdict,
    /// and a `LivenessUnknown` card raised next to a `NoLeader` one would say
    /// coord was unobserved on a cycle whose own report quotes what coord
    /// said. The unreadable rows still travel — [`render_read`] puts them on
    /// that card's body as `rows_this_build_could_not_classify` — so the
    /// blind spot is disclosed where the operator is already looking rather
    /// than suppressed.
    fn observed_nothing_else(&self) -> bool {
        self.dead_leader_gated.is_empty()
            && self.counts_not_leader_here == 0
            && self.leaderless.is_empty()
    }

    /// True when this build failed to place at least one listed row in any
    /// bucket — predicate (ii)'s observability defense.
    ///
    /// Unlike [`Self::dead_is_underread`] it is not gated on truncation and
    /// does not consult a single count, because the shape it exists for has
    /// neither: a `leader_gated` coord emits as the string `"true"`, or a
    /// renamed `status`, drops every row through the classifier while the
    /// list is WHOLE and coord's truncation flag reads `false`. See
    /// [`Self::unclassified`].
    pub fn classification_is_underread(&self) -> bool {
        !self.unclassified.is_empty()
    }

    /// Non-nominal rows coord COUNTED but did not LIST, on a read whose
    /// truncation flag says nothing was cut — the rows that never arrived.
    ///
    /// Zero on a truncated read: there the shortfall is the cap, already
    /// reported by coord's own flag and covered by [`Self::dead_is_underread`].
    /// On an untruncated read coord builds the list and `counts.non_nominal`
    /// from the same vector, so any shortfall means rows were dropped between
    /// the two — a filter added to one and not the other — and predicate
    /// (ii), which reads its population off the list, did not see them.
    pub fn rows_absent(&self) -> u64 {
        if self.list_truncated {
            return 0;
        }
        self.counts_non_nominal.saturating_sub(self.listed_rows)
    }

    /// The unclassified labels, bounded for a headline — see
    /// [`UNCLASSIFIED_IN_SUMMARY`]. The remainder is COUNTED rather than
    /// dropped, so the summary never understates the blind spot, and
    /// [`render_read`] carries every label in full on the same card.
    fn unclassified_summary(&self) -> String {
        let shown = self.unclassified.len().min(UNCLASSIFIED_IN_SUMMARY);
        let head = self.unclassified[..shown].join("; ");
        match self.unclassified.len() - shown {
            0 => head,
            more => format!("{head}; and {more} more"),
        }
    }

    /// Why this read left a predicate UNOBSERVED, or `None` when it settled
    /// both — the input [`ObserverState::unobserved_streak`] folds for a
    /// `Read`.
    ///
    /// Only (ii) can reach here. (iii) is settled either way by
    /// [`Self::counts_not_leader_here`]: a zero is a leader, a non-zero is
    /// predicate (iii) itself. (ii) is unobserved on three shapes, and they
    /// are independent:
    ///
    /// 1. **the classifier failed** — at least one listed row was unreadable,
    ///    so (ii)'s population was never fully assembled. Gated on nothing
    ///    about truncation, because the shapes it catches (a `leader_gated`
    ///    retype, a `status` rename) leave the list whole and coord's flag
    ///    `false`; gated only on [`Self::observed_nothing_else`], so it never
    ///    contradicts a verdict this same read produced;
    /// 2. **rows never arrived** — [`Self::rows_absent`]: an untruncated list
    ///    shorter than `counts.non_nominal`. Gated the same way as (1);
    /// 3. **the cap ate them all** — the truncated list named NONE of the
    ///    dead workers coord counted, after the rows this read DID classify
    ///    (kept, excluded, or dropped as follower-plane) are subtracted. A
    ///    read that named one has already put a `WorkerDead` card up and the
    ///    operator is looking.
    pub fn unobserved_reason(&self) -> Option<String> {
        if self.classification_is_underread() && self.observed_nothing_else() {
            return Some(format!(
                "coord answered and this runner could not classify {} of the {} row(s) in its \
                 `non_nominal_workers` list ({}) — predicate (ii)'s population is read off those \
                 rows, so it was not observed on this cycle",
                self.unclassified.len(),
                self.listed_rows,
                self.unclassified_summary(),
            ));
        }
        if self.rows_absent() > 0 && self.observed_nothing_else() {
            return Some(format!(
                "coord answered and counted {} non-nominal worker(s) but its untruncated \
                 `non_nominal_workers` list carried only {} — predicate (ii)'s population is \
                 read off those rows, so the {} absent row(s) were not observed on this cycle",
                self.counts_non_nominal,
                self.listed_rows,
                self.rows_absent(),
            ));
        }
        if self.dead_leader_gated.is_empty() && self.dead_is_underread() {
            return Some(format!(
                "coord answered and its `non_nominal_workers` list was TRUNCATED, naming none of \
                 the {} dead worker(s) `counts.dead` reports that this read did not otherwise \
                 account for ({} counted in all) — predicate (ii) was not observed on this cycle",
                self.dead_unaccounted(),
                self.counts_dead,
            ));
        }
        None
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
    // ABSENT is not ZERO. All three counts are emitted unconditionally by
    // coord's no-arg arm, so a missing one means this build is reading a shape it does
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
    let Some(counts_non_nominal) = counts.get("non_nominal").and_then(JsonValue::as_u64) else {
        return ProbeOutcome::Unusable {
            reason: "coord answered but `components.counts` carried no readable `non_nominal` — \
                 this runner build cannot read this response shape, and without that count a \
                 list missing rows cannot be told from a complete one"
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

    // The flag gets the SAME arm as the two counts above, for the same reason
    // the comment there gives. Defaulting it to `false` reintroduced exactly
    // the harm that comment names: `dead_is_underread` reads `false` on every
    // cycle, and the one under-read predicate that still consults the flag
    // reports a clean fleet forever, with no warning anywhere. An absent or
    // wrong-typed flag is a shape this build cannot read, which is
    // `Unusable` — and three of those reach predicate (iv).
    let list_truncated = match tool.pointer("/components/non_nominal_workers_truncated") {
        Some(JsonValue::Bool(b)) => *b,
        other => {
            return ProbeOutcome::Unusable {
                reason: format!(
                    "coord answered but `components.non_nominal_workers_truncated` is {} rather \
                     than a bool — this runner build cannot read this response shape, and an \
                     absent truncation flag is UNKNOWN, not `false`",
                    other.map(kind_of).unwrap_or("absent")
                ),
            }
        }
    };

    let mut read = LedgerRead {
        counts_dead,
        counts_not_leader_here,
        counts_non_nominal,
        list_truncated,
        listed_rows: listed.len() as u64,
        lease_holder_replica_id: tool.get("components").map_or(Served::Absent, |c| {
            Served::read(c, "lease_holder_replica_id", |v| {
                v.as_str().map(str::to_string)
            })
        }),
        ..LedgerRead::default()
    };
    for row in &listed {
        // Leader-gated only. A follower-plane worker's death is not the class
        // this observer exists for, and coord's own pager can see it.
        //
        // Three arms rather than `and_then(as_bool) != Some(true)`, because
        // that spelling collapsed two different facts into one `continue`: a
        // row READ and deliberately dropped (`false`), and a row this build
        // could not read at all (absent, or a bool retyped to a string). The
        // collapse is what let a `leader_gated: "true"` drop every row in
        // silence on an UNTRUNCATED list — see `LedgerRead::unclassified`.
        match row.get("leader_gated") {
            Some(JsonValue::Bool(true)) => {}
            Some(JsonValue::Bool(false)) => {
                // Read, judged, out of scope — and REMEMBERED, because
                // `counts.dead` spans this plane and `dead_unaccounted()`
                // must subtract what this read accounted for. See
                // `LedgerRead::dead_follower_plane`.
                if row.get("status").and_then(JsonValue::as_str) == Some(DEAD_STATUS) {
                    read.dead_follower_plane = read.dead_follower_plane.saturating_add(1);
                }
                continue;
            }
            other => {
                read.unclassified.push(format!(
                    "`{}`: `leader_gated` is {} rather than a bool",
                    row_name(row),
                    other.map(kind_of).unwrap_or("absent"),
                ));
                continue;
            }
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
            other => {
                if verdict.reason.as_deref() == Some("no_leader_tick") {
                    read.leaderless.push(verdict);
                } else if !KNOWN_ROLLUP_STATUSES.contains(&other) {
                    // A status outside coord's own vocabulary: renamed key,
                    // renamed value, or a variant this build predates. Either
                    // way this row was NOT placed, and predicate (ii) reads
                    // its population off these rows — so say so rather than
                    // dropping it into the same silence `alive`/`stale` get.
                    read.unclassified.push(format!(
                        "`{}`: status `{}` is outside this build's vocabulary",
                        // `row_name`, not `verdict.name`: the same bound the
                        // `leader_gated` arm 50 lines above already applies.
                        // `worker_verdict` copies `name` VERBATIM, so a
                        // 5000-char name rendered 5057 bytes here against
                        // that arm's 142 — into a label that reaches a
                        // headline.
                        row_name(row),
                        truncate(other, 40),
                    ));
                }
                // `alive` and `stale` land here and are CLASSIFIED: this
                // module holds no predicate over them (the `stale` gap is
                // coord's own pager's, and it sees it while a leader is
                // live), so counting them as unreadable would page on every
                // ordinary fleet.
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

/// A row's `name` for a diagnostic label, without building a whole
/// [`WorkerVerdict`] out of a row that did not classify.
fn row_name(row: &JsonValue) -> String {
    row.get("name")
        .and_then(JsonValue::as_str)
        .map(|n| truncate(n, 80))
        .unwrap_or_else(|| "(unnamed)".to_string())
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
        last_tick_secs_ago: Served::read(row, "last_tick_secs_ago", JsonValue::as_f64),
        last_work_tick_secs_ago: Served::read(row, "last_work_tick_secs_ago", JsonValue::as_f64),
        leader_body_in_flight_secs: Served::read(
            row,
            "leader_body_in_flight_secs",
            JsonValue::as_f64,
        ),
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
    /// observe nothing about (iii) — an [`ProbeOutcome::Unusable`] or an
    /// [`ProbeOutcome::Unreachable`] — so `streak` and "consecutive probes"
    /// are NOT the same number and the summary must not claim they are. (A
    /// truncated read is no longer one of those cycles: it carries
    /// `counts.not_leader_here`, which settles (iii) either way.)
    leaderless_probes: u32,
    leaderless_notified: bool,
    leaderless_since: Option<Instant>,
    /// Predicate (iv)'s counter: consecutive probes that produced NO usable
    /// observation of coord — it did not answer, it answered unreadably, or
    /// it answered readably and left (ii) unsettled.
    ///
    /// **Nothing but a settling read clears it**, which is the property the
    /// class is named for and the reason it is a separate counter rather than
    /// a rename of the old per-arm `Unusable` streak. The per-arm streaks
    /// correctly contradict each other — an `Unreachable` really does refute
    /// "coord answered unreadably", and an `Unusable` really does refute
    /// "coord did not answer" — so an alternating door pinned both at 1 and
    /// NEITHER predicate could ever reach its cadence. This counter asks the
    /// question neither arm can: *has this runner had a usable observation of
    /// coord in the last N probes?*
    unobserved_streak: u32,
    unobserved_notified: bool,
    unobserved_since: Option<Instant>,
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
        let mut out = match outcome {
            ProbeOutcome::Unreachable { reason } => self.observe_unreachable(reason, now),
            ProbeOutcome::Unusable { reason } => self.observe_unusable(reason),
            ProbeOutcome::Read(read) => {
                self.clear_unreachable();
                let mut out = self.observe_dead(read);
                out.extend(self.observe_leaderless(read, now));
                out
            }
        };
        // LAST, and over the outcome as a whole rather than inside any arm:
        // predicate (iv) is the one that must survive every OTHER arm's reset.
        out.extend(self.observe_unobserved(outcome, now));
        out
    }

    fn clear_unreachable(&mut self) {
        self.unreachable_streak = 0;
        self.unreachable_notified = false;
        self.unreachable_since = None;
    }

    fn clear_unobserved(&mut self) {
        self.unobserved_streak = 0;
        self.unobserved_notified = false;
        self.unobserved_since = None;
    }

    fn clear_leaderless(&mut self) {
        self.leaderless_streak = 0;
        self.leaderless_probes = 0;
        self.leaderless_notified = false;
        self.leaderless_since = None;
    }

    fn observe_unreachable(&mut self, reason: &str, now: Instant) -> Vec<Report> {
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

    /// coord SPOKE, so predicate (i) is settled in the negative and (ii)/(iii)
    /// are left exactly where they were — an absent observation is not a
    /// contradicting one.
    ///
    /// It reports nothing itself. Predicate (iv) is owned entirely by
    /// [`Self::observe_unobserved`], which folds this outcome along with every
    /// other unusable one; splitting the firing across both would have raised
    /// two `LivenessUnknown` cards for a single blind episode.
    fn observe_unusable(&mut self, _reason: &str) -> Vec<Report> {
        self.clear_unreachable();
        Vec::new()
    }

    /// Predicate (iv), folded over the outcome as a WHOLE.
    ///
    /// The honest question no per-arm streak can ask: *has this runner had a
    /// usable observation of coord in the last [`CADENCES_TO_FIRE`] probes?*
    /// An `Unreachable`, an `Unusable`, and a `Read` that left a predicate
    /// unsettled all answer no, and only a settling `Read` clears it.
    fn observe_unobserved(&mut self, outcome: &ProbeOutcome, now: Instant) -> Vec<Report> {
        let Some(reason) = unobserved_reason_of(outcome) else {
            self.clear_unobserved();
            return Vec::new();
        };
        self.unobserved_streak = self.unobserved_streak.saturating_add(1);
        self.unobserved_since.get_or_insert(now);
        if self.unobserved_streak == 1
            || self
                .unobserved_streak
                .is_multiple_of(UNOBSERVED_REWARN_EVERY)
        {
            warn!(
                probes = self.unobserved_streak,
                reason = %reason,
                "coord outside observer: no usable observation of coord on this probe — \
                 coord liveness is UNKNOWN here, not healthy"
            );
        }
        if self.unobserved_streak < CADENCES_TO_FIRE || self.unobserved_notified {
            return Vec::new();
        }
        // Predicate (i) has already put a card in front of the operator naming
        // this same episode, and "coord has not answered on N consecutive
        // probes" is strictly stronger than "I have no verdict". A second card
        // beside it is noise, not a second fact. Not latched: if the door later
        // flaps to an `Unusable`, (i) is cleared and this becomes the only
        // honest thing left to say.
        if self.unreachable_notified {
            return Vec::new();
        }
        self.unobserved_notified = true;
        vec![Report {
            class: FaultClass::LivenessUnknown,
            summary: format!(
                "this runner has had NO usable observation of coord on {} consecutive probes, \
                 over a measured {}s — coord liveness is UNOBSERVED from here, which is not the \
                 same as healthy. Last reason: {reason}",
                self.unobserved_streak,
                elapsed_secs(self.unobserved_since, now),
            ),
            // A `Read` that left (ii) unobserved still HAS a read, and the
            // card's details are where every other class puts it — without it
            // the operator is told the counts disagree and not shown them.
            raw: match outcome {
                ProbeOutcome::Read(read) => format!("{reason}\n\n{}", render_read(read)),
                _ => reason,
            },
            // Never. Whatever kept this runner from observing coord — no
            // answer, a refused credential, an unreadable shape — applies
            // identically to a finding POST, and the module's contract is to
            // write only to a coord that just ANSWERED a read.
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
                     replica age {}, last decision code {}). Lease holder: {}; its body: {}. \
                     Last WORK tick: {}. Last write of any kind: {} — may be a follower's \
                     `follower_skip`, which is not the work running. counts.dead = {}.",
                    worker.name,
                    worker.status,
                    worker.live_status.as_deref().unwrap_or("unknown"),
                    worker
                        .worst_replica_age_secs
                        .map(|a| format!("{a:.0}s"))
                        .unwrap_or_else(|| "unknown".into()),
                    worker.last_decision_code.as_deref().unwrap_or("none"),
                    render_lease_holder(&read.lease_holder_replica_id),
                    render_body_in_flight(&worker.leader_body_in_flight_secs),
                    render_age(
                        &worker.last_work_tick_secs_ago,
                        "no live replica's most recent write was the work running — never 0, \
                         never fresh",
                    ),
                    render_age(&worker.last_tick_secs_ago, "no live replica has written"),
                    read.counts_dead,
                ),
                raw: render_read(read),
                post_finding: true,
            });
        }
        out
    }

    fn observe_leaderless(&mut self, read: &LedgerRead, now: Instant) -> Vec<Report> {
        // (iii)'s population is the COUNT, never the list. `counts` is over
        // the full worker set and the 40-row cap never touches it, while every
        // worker inside `counts.not_leader_here` is leader-gated and
        // non-nominal by coord's own construction (the proof is on
        // `LedgerRead::counts_not_leader_here`). So a non-zero count IS
        // predicate (iii), names or no names, and the list supplies only the
        // names.
        //
        // `.max()` rather than the count alone so a coord that somehow lists
        // more leaderless rows than it counts still fires: the report must
        // never be weaker than either half of the evidence.
        //
        // This is also why a capped cycle is no longer an UNKNOWN that holds
        // the streak in place. The shape that used to be invisible — coord
        // loses its leader, every leader-gated worker flips at once, the
        // non-nominal population blows past the cap, `leaderless: []` — now
        // ADVANCES on the count and pages after the cadence, saying that the
        // names were unavailable. And a leaderless episode capped away from
        // its very ONSET, which held the streak at zero forever and produced
        // nothing but a per-probe `warn!`, now reaches the operator.
        let population = read
            .counts_not_leader_here
            .max(read.leaderless.len() as u64);
        if population == 0 {
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
        // The names are the part the cap eats, so they are reported as a
        // separate fact from the population — never by silently shrinking it,
        // and never as an empty list masquerading as "none".
        let named = if names.is_empty() {
            format!(
                "coord named NONE of them in its `non_nominal_workers` list \
                 (truncated: {}), so this report carries the count without the names — read \
                 `coord_query_workers` with a `name` for the per-replica rows",
                read.list_truncated
            )
        } else if read.leaderless_is_underread() {
            format!(
                "{} — and {} more coord counted but did not name (truncated: {})",
                names.join(", "),
                read.leaderless_unaccounted(),
                read.list_truncated
            )
        } else {
            names.join(", ")
        };
        vec![Report {
            class: FaultClass::NoLeader,
            summary: format!(
                "no coord replica reports itself leader: {population} leader-gated worker(s) \
                 have read `no_leader_tick` on {} of the last {} probes, over a measured {}s — {}",
                self.leaderless_streak,
                self.leaderless_probes.max(self.leaderless_streak),
                elapsed_secs(self.leaderless_since, now),
                named
            ),
            raw: render_read(read),
            post_finding: true,
        }]
    }
}

/// Why one probe outcome counts as UNOBSERVED, or `None` when it settled the
/// predicates — predicate (iv)'s single input, over all three arms.
fn unobserved_reason_of(outcome: &ProbeOutcome) -> Option<String> {
    match outcome {
        ProbeOutcome::Unreachable { reason } => Some(format!("coord did not answer: {reason}")),
        ProbeOutcome::Unusable { reason } => Some(reason.clone()),
        ProbeOutcome::Read(read) => read.unobserved_reason(),
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
        "lease_holder_replica_id": read.lease_holder_replica_id.to_json(),
        "dead_leader_gated": read.dead_leader_gated.iter().map(worker_json).collect::<Vec<_>>(),
        "leaderless": read.leaderless.iter().map(worker_json).collect::<Vec<_>>(),
        "excluded_as_rolled_off_replica": read.rolled_off_excluded,
        "dead_dropped_as_follower_plane": read.dead_follower_plane,
        "non_nominal_workers_truncated": read.list_truncated,
        "listed_rows": read.listed_rows,
        "counts_non_nominal": read.counts_non_nominal,
        "rows_absent_from_an_untruncated_list": read.rows_absent(),
        "rows_this_build_could_not_classify": read.unclassified,
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
        "last_tick_secs_ago": w.last_tick_secs_ago.to_json(),
        "last_work_tick_secs_ago": w.last_work_tick_secs_ago.to_json(),
        "leader_body_in_flight_secs": w.leader_body_in_flight_secs.to_json(),
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

/// The finding's title: the report's one-line claim, bounded to coord's
/// [`FINDING_TITLE_MAX_BYTES`] with the attribution suffix kept whole.
///
/// The CLAIM is what gets cut, never the suffix, and a cut says so and points
/// at the body, which carries the summary in full ([`finding_body`]).
fn finding_title(summary: &str, device_id: &str) -> String {
    let suffix = format!(
        " (observed from OUTSIDE coord by runner device {})",
        truncate(device_id, 64)
    );
    const CUT: &str = "… [cut to fit; full text in the body]";
    let whole = format!("{summary}{suffix}");
    if whole.len() <= FINDING_TITLE_MAX_BYTES {
        return whole;
    }
    let budget = FINDING_TITLE_MAX_BYTES.saturating_sub(suffix.len() + CUT.len());
    let mut end = budget.min(summary.len());
    while !summary.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{CUT}{suffix}", &summary[..end])
}

/// The finding's body: the class title, the FULL summary (the title may have
/// been cut to fit), then the evidence, method and advice — bounded to coord's
/// [`FINDING_BODY_MAX_BYTES`].
///
/// Only the EVIDENCE is ever cut. It is the pretty-printed read of every
/// dead or leaderless worker, so it grows with exactly the event this
/// observer exists for — a leader dying takes every leader-gated worker with
/// it — and an over-cap body is a 400 on a once-per-episode post, i.e. a lost
/// finding. The full read still travels on this runner's notification
/// (`UserFacingError::details`) when the runner has a UI, and [`post_finding`]
/// logs it in full whenever a cut happens — so a headless runner keeps it too.
fn finding_body(report: &Report, door_url: &str) -> String {
    let advice = match report.class {
        FaultClass::WorkerDead => {
            "Read `coord_query_workers` with the worker's name for the per-replica rows. \
             `last_tick_secs_ago` may be a follower's `follower_skip`, so a fresh value there \
             does not refute a dead verdict; read `last_work_tick_secs_ago` (null is UNKNOWN, \
             never fresh) and `leader_body_in_flight_secs` instead."
        }
        _ => {
            "Read `coord_query_workers` with each worker name listed in the evidence for the \
             per-replica rows, and check coord's leader lease and replica presence."
        }
    };
    let head = format!(
        "{}\n\n{}\n\nEVIDENCE — the `{WORKERS_TOOL}` read this verdict was computed from:\n\n",
        report.class.title(),
        report.summary,
    );
    let tail = format!(
        "\n\nMETHOD: one JSON-RPC `tools/call` for `{WORKERS_TOOL}` against {door_url} from \
         inside a runner process, on the ~{PROBE_PERIOD_SECS}s cadence of \
         `session::coord_sync`'s heartbeat loop. The predicate is \
         `coord_outside_observer::classify_ledger` + `ObserverState::observe` (plan \
         2026-09-12-merge-train-alerts-page-a-reader-and-act-on-nothing Phase 3b): a \
         leader-gated worker rolling up `dead` after the `verdict_from_rolled_off_replica` \
         discrimination, or `no_leader_tick` held for {CADENCES_TO_FIRE} consecutive \
         probes.\n\n\
         WHAT A PEER SHOULD DO DIFFERENTLY: this is an OUTSIDE observation — coord's own \
         leader-gated pager cannot report it, which is the whole reason it exists. {advice} \
         The runner that posted this has no lever on coord and did not attempt one."
    );
    let budget = FINDING_BODY_MAX_BYTES.saturating_sub(head.len() + tail.len());
    let evidence = if report.raw.len() <= budget {
        std::borrow::Cow::Borrowed(report.raw.as_str())
    } else {
        let mut end = budget
            .saturating_sub(FINDING_EVIDENCE_CUT.len())
            .min(report.raw.len());
        while !report.raw.is_char_boundary(end) {
            end -= 1;
        }
        std::borrow::Cow::Owned(format!("{}{FINDING_EVIDENCE_CUT}", &report.raw[..end]))
    };
    let mut body = format!("{head}{evidence}{tail}");
    // Last resort: a summary long enough to eat the evidence budget on its
    // own. Cut the whole body on a char boundary rather than send a 400.
    if body.len() > FINDING_BODY_MAX_BYTES {
        let mut end = FINDING_BODY_MAX_BYTES;
        while !body.is_char_boundary(end) {
            end -= 1;
        }
        body.truncate(end);
    }
    body
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
    let device_id = qontinui_runner_lib::machine_identity::read_device_id()
        .unwrap_or_else(|_| "unknown".to_string());
    let finding = finding_body(report, &door.url);
    if finding.contains(FINDING_EVIDENCE_CUT) {
        // The finding's evidence was cut to fit coord's cap; this line is
        // where the cut part survives, on a headless runner as on any other.
        warn!(
            class = report.class.breadcrumb_reason(),
            raw = %report.raw,
            "coord outside observer: finding evidence cut to fit coord's body cap; full read follows"
        );
    }
    let body = json!({
        "title": finding_title(&report.summary, &device_id),
        "body": finding,
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
            // A 2xx is not storage. With the `coord_findings` migration
            // unapplied coord answers **200** with `{"posted": false,
            // "reason": …}` rather than an error — the graceful degradation
            // `session::coord_sync::finding_outcome` documents and handles in
            // this same repo. Logging success on any 2xx made this observer
            // claim a finding a later session will never find. Not retried:
            // a retry cannot apply a migration.
            let status = resp.status();
            match resp.json::<JsonValue>().await {
                Ok(body) if body.get("posted").and_then(JsonValue::as_bool) == Some(false) => {
                    warn!(
                        class = report.class.breadcrumb_reason(),
                        reason = ?body.get("reason").and_then(JsonValue::as_str),
                        "coord outside observer: coord accepted the observation but did NOT \
                         store it (coord.findings not provisioned) — the finding is LOST"
                    );
                }
                Ok(_) => info!(
                    class = report.class.breadcrumb_reason(),
                    "coord outside observer: observation posted to coord as a finding"
                ),
                Err(e) => warn!(
                    class = report.class.breadcrumb_reason(),
                    status = %status,
                    error = %e,
                    "coord outside observer: coord answered 2xx with a body this runner could \
                     not read — storage is NOT confirmed"
                ),
            }
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
            // (iii) is NOT unsettled by this — it advances on the count — but
            // the missing names are worth a line, and they are the only thing
            // a per-name follow-up read can recover. Two causes, neither of
            // which this runner can tell apart from here: coord's cap (which
            // drops `not_leader_here` BEFORE `dead`), or rows this build could
            // not classify.
            if read.leaderless_is_underread() {
                warn!(
                    counts_not_leader_here = read.counts_not_leader_here,
                    unaccounted = read.leaderless_unaccounted(),
                    named = read.leaderless.len(),
                    truncated = read.list_truncated,
                    "coord outside observer: coord counts more leaderless leader-gated workers \
                     than this read could NAME — predicate (iii) still advances on the count, \
                     but the names are unavailable. Either coord's 40-row cap took them or this \
                     build could not classify the rows. Call coord_query_workers with a `name` \
                     for the per-replica rows."
                );
            }
            // (ii)'s OTHER blind shape, and the only one of the three with no
            // count behind it: rows this build could not PLACE. Warned beside
            // its two siblings for the reason the comment above them gives,
            // and for one more that is specific to it — on the path where
            // `observed_nothing_else()` is false because this same read named
            // a dead worker, `unobserved_reason()` returns `None` and no
            // report carries the blind spot, so without this line the
            // classifier's failure is recorded NOWHERE.
            if read.classification_is_underread() {
                warn!(
                    unclassified = read.unclassified.len(),
                    listed_rows = read.listed_rows,
                    truncated = read.list_truncated,
                    detail = %read.unclassified_summary(),
                    "coord outside observer: this runner build could not classify every row in \
                     coord's non_nominal_workers list — predicate (ii) reads its population off \
                     those rows, so it is UNKNOWN for the rows that did not classify, not clear. \
                     A renamed `status` or a retyped `leader_gated` wants a runner upgrade."
                );
            }
            // The fourth shape: rows coord counted and never sent, on a list
            // its own flag says was not cut. Warned for the same reason as
            // the line above — beside a named dead worker no report carries it.
            if read.rows_absent() > 0 {
                warn!(
                    counts_non_nominal = read.counts_non_nominal,
                    listed_rows = read.listed_rows,
                    absent = read.rows_absent(),
                    "coord outside observer: coord counted more non-nominal workers than its \
                     UNTRUNCATED non_nominal_workers list carried — predicate (ii) is UNKNOWN for \
                     the absent rows, not clear. Call coord_query_workers with a `name` for the \
                     per-replica rows."
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
    ///
    /// `counts.non_nominal` is DERIVED from the list, as coord derives both
    /// from one vector — a hardcoded value would make every multi-row body a
    /// short list and every test of [`LedgerRead::rows_absent`] vacuous.
    fn ledger_body(counts_dead: u64, non_nominal: JsonValue) -> JsonValue {
        let listed = non_nominal.as_array().map_or(0, Vec::len);
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
                    "dead": counts_dead, "not_leader_here": 0, "non_nominal": listed
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
        // coord sets the flag only when its count passes the 40-row cap.
        body["components"]["counts"]["non_nominal"] = json!(41);
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

    /// The 2026-09-19 shape, as a coord serving plan
    /// `2026-09-19-leader-gated-worker-liveness-reads-a-follower-skip-as-the-work-running`
    /// Phases 1-2 renders it: the lease holder's row frozen at 857 s with its
    /// body in flight, a follower writing `follower_skip` 3 s ago, and no
    /// live replica having run the body in the window.
    fn tonight_row() -> JsonValue {
        let mut row = dead_row("merge_scheduler.dispatch", true, false);
        row["reason"] = json!("body_in_flight");
        row["last_tick_secs_ago"] = json!(3.0);
        row["worst_replica_age_secs"] = json!(857.0);
        row["last_work_tick_secs_ago"] = JsonValue::Null;
        row["leader_body_in_flight_secs"] = json!(790.0);
        row
    }

    fn tonight_body() -> JsonValue {
        let mut body = ledger_body(1, json!([tonight_row()]));
        body["components"]["lease_holder_replica_id"] =
            json!("7c1e2a4b-0000-4000-8000-00000000beef");
        body
    }

    fn only_worker_dead_report(body: &JsonValue) -> Report {
        let outcome = classify_ledger(body);
        let mut state = ObserverState::default();
        let mut reports = state.observe(&outcome);
        assert_eq!(
            reports.len(),
            1,
            "dead still fires on the first observation"
        );
        let report = reports.remove(0);
        assert_eq!(report.class, FaultClass::WorkerDead);
        report
    }

    #[test]
    fn the_tonight_shaped_body_names_the_lease_holder_its_body_and_the_work_tick() {
        let report = only_worker_dead_report(&tonight_body());
        let s = &report.summary;
        assert!(s.contains("merge_scheduler.dispatch"), "{s}");
        assert!(
            s.contains(
                "Lease holder: replica `7c1e2a4b-0000-4000-8000-00000000beef` (last recorded)"
            ),
            "{s}"
        );
        assert!(
            s.contains("its body: in flight at least 790s past its stale bound"),
            "{s}"
        );
        // coord's null is UNKNOWN — never rendered as 0 or as fresh.
        assert!(
            s.contains("Last WORK tick: UNKNOWN (null: no live replica's most recent write"),
            "{s}"
        );
        assert!(!s.contains("Last WORK tick: 0"), "{s}");
        // The follower's 3 s write is shown, and labelled for what it is.
        assert!(
            s.contains("Last write of any kind: 3s ago — may be a follower's"),
            "{s}"
        );

        let raw: JsonValue = serde_json::from_str(&report.raw).expect("raw is JSON");
        assert_eq!(
            raw["lease_holder_replica_id"],
            json!("7c1e2a4b-0000-4000-8000-00000000beef")
        );
        let w = &raw["dead_leader_gated"][0];
        assert_eq!(w["last_work_tick_secs_ago"], JsonValue::Null);
        assert_eq!(w["leader_body_in_flight_secs"], json!(790.0));
        assert_eq!(w["last_tick_secs_ago"], json!(3.0));
    }

    #[test]
    fn an_older_coord_body_classifies_the_same_and_marks_every_new_field_unknown() {
        // `dead_row` is the pre-Phase-2 shape: no work tick, no in-flight
        // field, and no lease holder on the summary.
        let body = ledger_body(1, json!([dead_row("work_unit_derive.sweep", true, false)]));
        let ProbeOutcome::Read(read) = classify_ledger(&body) else {
            panic!("expected a Read");
        };
        assert_eq!(read.dead_leader_gated.len(), 1);
        assert_eq!(read.lease_holder_replica_id, Served::Absent);
        let w = &read.dead_leader_gated[0];
        assert_eq!(w.last_work_tick_secs_ago, Served::Absent);
        assert_eq!(w.leader_body_in_flight_secs, Served::Absent);
        assert_eq!(w.last_tick_secs_ago, Served::Value(1499.0));

        let report = only_worker_dead_report(&body);
        let s = &report.summary;
        assert!(
            s.contains("Lease holder: UNKNOWN (absent: this coord predates the field)"),
            "{s}"
        );
        // Absent is UNKNOWN, never "not in flight".
        assert!(
            s.contains(
                "its body: UNKNOWN (absent: this coord predates the field) — NOT \"not in flight\""
            ),
            "{s}"
        );
        assert!(!s.contains("not observed in flight"), "{s}");
        assert!(
            s.contains("Last WORK tick: UNKNOWN (absent: this coord predates the field)"),
            "{s}"
        );

        let raw: JsonValue = serde_json::from_str(&report.raw).expect("raw is JSON");
        let unknown = json!("UNKNOWN: absent — this coord predates the field");
        assert_eq!(raw["lease_holder_replica_id"], unknown);
        assert_eq!(
            raw["dead_leader_gated"][0]["last_work_tick_secs_ago"],
            unknown
        );
        assert_eq!(
            raw["dead_leader_gated"][0]["leader_body_in_flight_secs"],
            unknown
        );
    }

    #[test]
    fn a_null_in_flight_reads_as_not_observed_and_never_as_a_dead_loop() {
        let mut row = tonight_row();
        row["leader_body_in_flight_secs"] = JsonValue::Null;
        row["last_work_tick_secs_ago"] = json!(12.0);
        let mut body = ledger_body(1, json!([row]));
        body["components"]["lease_holder_replica_id"] = JsonValue::Null;

        let s = only_worker_dead_report(&body).summary;
        assert!(s.contains("its body: not observed in flight (null"), "{s}");
        assert!(s.contains("not proof the loop is gone"), "{s}");
        assert!(s.contains("Last WORK tick: 12s ago"), "{s}");
        assert!(
            s.contains("Lease holder: UNKNOWN (null: coord could not read"),
            "{s}"
        );

        // coord's null stays JSON null in the evidence block — distinct from
        // the string an ABSENT field renders as.
        let raw: JsonValue =
            serde_json::from_str(&only_worker_dead_report(&body).raw).expect("raw is JSON");
        assert_eq!(raw["lease_holder_replica_id"], JsonValue::Null);
        assert_eq!(
            raw["dead_leader_gated"][0]["leader_body_in_flight_secs"],
            JsonValue::Null
        );
    }

    #[test]
    fn a_wrong_typed_field_is_unreadable_not_absent() {
        let mut row = tonight_row();
        row["last_work_tick_secs_ago"] = json!("12");
        let body = ledger_body(1, json!([row]));
        let ProbeOutcome::Read(read) = classify_ledger(&body) else {
            panic!("a retyped OPTIONAL field must not cost the whole read");
        };
        assert_eq!(
            read.dead_leader_gated[0].last_work_tick_secs_ago,
            Served::Unreadable("string")
        );
        let s = only_worker_dead_report(&body).summary;
        assert!(
            s.contains("Last WORK tick: UNKNOWN (coord served a string this runner build"),
            "{s}"
        );
    }

    #[test]
    fn a_wrong_typed_in_flight_or_lease_holder_is_unreadable_never_not_in_flight() {
        let mut row = tonight_row();
        row["leader_body_in_flight_secs"] = json!("790");
        let mut body = ledger_body(1, json!([row]));
        body["components"]["lease_holder_replica_id"] = json!(42);

        let report = only_worker_dead_report(&body);
        let s = &report.summary;
        assert!(
            s.contains("its body: UNKNOWN (coord served a string this runner build"),
            "{s}"
        );
        assert!(!s.contains("not observed in flight"), "{s}");
        assert!(
            s.contains("Lease holder: UNKNOWN (coord served a number"),
            "{s}"
        );

        let raw: JsonValue = serde_json::from_str(&report.raw).expect("raw is JSON");
        assert_eq!(
            raw["lease_holder_replica_id"],
            json!("UNKNOWN: unreadable number")
        );
        assert_eq!(
            raw["dead_leader_gated"][0]["leader_body_in_flight_secs"],
            json!("UNKNOWN: unreadable string")
        );
    }

    /// coord refuses a finding title over `FINDING_TITLE_MAX_BYTES` with a
    /// 400 and this observer posts once per episode — so an over-long title
    /// is a LOST finding, not a truncated one. Built at the worst case: every
    /// new field UNKNOWN, an 80-char worker name, a 36-char device id.
    #[test]
    fn the_worker_dead_finding_title_fits_coords_bound_and_the_body_keeps_it_all() {
        let name = "w".repeat(80);
        let body = ledger_body(1, json!([dead_row(&name, true, false)]));
        let report = only_worker_dead_report(&body);
        let device = "eb2155ed-4152-4a91-be82-5d4346f717fc";

        let title = finding_title(&report.summary, device);
        assert!(
            title.len() <= FINDING_TITLE_MAX_BYTES,
            "{} bytes: {title}",
            title.len()
        );
        assert!(
            title.contains(&name),
            "the claim's subject survives the cut: {title}"
        );
        assert!(
            title.ends_with(&format!("by runner device {device})")),
            "the attribution suffix is never the part cut: {title}"
        );
        assert!(
            title.contains("full text in the body"),
            "a cut says so: {title}"
        );

        let finding = finding_body(&report, "https://coord.example/mcp");
        assert!(
            finding.contains(&report.summary),
            "the body carries the whole summary"
        );
        assert!(finding.contains("last_work_tick_secs_ago"), "{finding}");
        assert!(!finding.contains("`live_status` beside"), "{finding}");
    }

    /// A leader dying takes every leader-gated worker `dead` at once, and each
    /// report's `raw` carries ALL of them — so the body grows with exactly the
    /// event it reports. At coord's 40-row list cap, all-absent, 80-char
    /// names, it must still fit coord's 8 KiB cap, cutting only the evidence.
    #[test]
    fn a_leader_death_sized_finding_body_fits_coords_cap_and_keeps_its_advice() {
        let rows: Vec<JsonValue> = (0..40)
            .map(|i| dead_row(&format!("{i:02}{}", "w".repeat(78)), true, false))
            .collect();
        let body = ledger_body(40, JsonValue::Array(rows));
        let ProbeOutcome::Read(read) = classify_ledger(&body) else {
            panic!("expected a Read");
        };
        let mut state = ObserverState::default();
        let reports = state.observe(&ProbeOutcome::Read(read));
        assert_eq!(reports.len(), 40, "every dead worker still fires");
        let report = &reports[0];
        assert!(
            report.raw.len() > FINDING_BODY_MAX_BYTES,
            "the fixture must force a cut"
        );

        let finding = finding_body(report, "https://coord.example/mcp");
        assert!(
            finding.len() <= FINDING_BODY_MAX_BYTES,
            "{} bytes",
            finding.len()
        );
        assert!(finding.contains("evidence cut to fit"), "a cut says so");
        assert!(
            finding.contains(&report.summary),
            "the summary is never the part cut"
        );
        assert!(
            finding.ends_with("did not attempt one."),
            "the method and advice are never the part cut"
        );
        assert!(finding.contains("last_work_tick_secs_ago"));
    }

    #[test]
    fn a_no_leader_finding_gets_advice_for_its_own_class() {
        let report = Report {
            class: FaultClass::NoLeader,
            summary: "coord reports 12 leader-gated workers with no leader.".to_string(),
            raw: "x".repeat(20_000),
            post_finding: true,
        };
        let finding = finding_body(&report, "https://coord.example/mcp");
        assert!(
            finding.len() <= FINDING_BODY_MAX_BYTES,
            "{} bytes",
            finding.len()
        );
        assert!(finding.contains("leader lease"), "{finding}");
        assert!(
            !finding.contains("does not refute a dead verdict"),
            "dead-worker advice does not belong on a no-leader finding"
        );
    }

    #[test]
    fn a_short_summary_title_is_left_whole() {
        let title = finding_title("coord has no leader.", "dev-1");
        assert_eq!(
            title,
            "coord has no leader. (observed from OUTSIDE coord by runner device dev-1)"
        );
    }

    #[test]
    fn a_title_cut_lands_on_a_char_boundary() {
        let summary = "é".repeat(400);
        let title = finding_title(&summary, "dev-1");
        assert!(title.len() <= FINDING_TITLE_MAX_BYTES);
        // Building it at all proves the slice was on a boundary; the content
        // check proves the cut kept a prefix of the claim.
        assert!(title.starts_with("éé"), "{title}");
    }

    #[test]
    fn worker_dead_advice_points_at_the_work_tick_not_live_status() {
        let action = FaultClass::WorkerDead.suggested_action();
        assert!(action.contains("last_work_tick_secs_ago"), "{action}");
        assert!(action.contains("follower_skip"), "{action}");
        assert!(action.contains("leader_body_in_flight_secs"), "{action}");
        assert!(
            !action.contains("check `live_status` beside `status`"),
            "the advice that let a follower's skip read as liveness is gone: {action}"
        );
    }

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
        // Predicate (i)'s streak is reset here — but three probes running have
        // now produced no usable observation, which is predicate (iv) and no
        // longer silence (D3).
        let reports = state.observe(&unusable);
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].class, FaultClass::LivenessUnknown);
        // (i) itself needs three CONSECUTIVE non-answers, and the count
        // restarted.
        assert!(state.observe(&down).is_empty());
        assert!(state.observe(&down).is_empty());
        let reports = state.observe(&down);
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].class, FaultClass::Unreachable);
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
        // 4, not 5: this constant CHANGED deliberately. One of coord's five
        // dead workers is the follower-plane row in this very list — read,
        // classified and dropped as out of scope — so `dead_unaccounted()`
        // now subtracts it along with the kept and excluded rows. The old 5
        // counted a row the observer had in its hand as one it never saw.
        assert_eq!(read.dead_follower_plane, 1);
        assert_eq!(read.dead_unaccounted(), 4);
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

    /// The remaining under-read that is genuinely an UNKNOWN rather than an
    /// observation: coord's cap named NONE of the dead workers it counted, so
    /// predicate (ii) was not observed on that cycle. Held for the cadence it
    /// reaches predicate (iv) — a `Read` outcome that nonetheless advances the
    /// unobserved streak.
    #[test]
    fn a_truncated_read_that_named_no_dead_worker_reaches_predicate_four() {
        let body = truncated_ledger_body(3, json!([dead_row("some.follower.loop", false, false)]));
        let ProbeOutcome::Read(read) = classify_ledger(&body) else {
            panic!("expected a Read");
        };
        assert!(read.dead_leader_gated.is_empty());
        assert!(read.dead_is_underread());
        assert!(
            read.unobserved_reason().is_some(),
            "a read that named none of the dead workers coord counted settled nothing"
        );

        let outcome = ProbeOutcome::Read(read);
        let mut state = ObserverState::default();
        let mut fired = Vec::new();
        for _ in 0..3 {
            fired.extend(state.observe(&outcome));
        }
        assert_eq!(fired.len(), 1, "{fired:?}");
        assert_eq!(fired[0].class, FaultClass::LivenessUnknown);
        assert!(
            fired[0].summary.contains("predicate (ii) was not observed"),
            "{}",
            fired[0].summary
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

    /// D1. The shape the truncation-gated predicate could not see AT ALL, and
    /// the reason attestation was refused: **no truncation is involved**.
    ///
    /// coord renames a row key or emits `leader_gated` as a string. Every row
    /// is then dropped unclassified — through `classify_ledger`'s `_` arm, or
    /// through the leader-gated filter above it — so
    /// `leaderless` is empty while `counts.not_leader_here` says 12 and
    /// `non_nominal_workers_truncated` says `false`. With the old
    /// `list_truncated &&` conjunct that read `leaderless_is_underread() ==
    /// false`, so the observer cleared the streak and the latch and reported a
    /// CLEAN FLEET for a coord with no leader — no truncation, no warning
    /// anywhere, and outcome `Read` so predicate (iv) never engaged either.
    #[test]
    fn an_untruncated_read_that_named_no_leaderless_row_is_under_read_and_still_fires() {
        // Two ways one renamed or retyped key drops every row, both silent:
        // a `status` coord spells differently falls through `classify_ledger`'s
        // `_` arm (and its `reason` fallback with it), and a `leader_gated`
        // emitted as a string is dropped by the leader-gated filter above it.
        let status_renamed = |name: &str| {
            let mut row = no_leader_row(name);
            row["status"] = json!("NotLeaderHere");
            row["reason"] = json!("noLeaderTick");
            row
        };
        let retyped_gate = |name: &str| {
            let mut row = no_leader_row(name);
            row["leader_gated"] = json!("true");
            row
        };
        let mut body = ledger_body(
            0,
            json!([
                status_renamed("merge_dispatch"),
                status_renamed("gate_sweep"),
                retyped_gate("alert_pageout_worker"),
            ]),
        );
        body["components"]["counts"]["not_leader_here"] = json!(12);
        let ProbeOutcome::Read(read) = classify_ledger(&body) else {
            panic!("expected a Read — the body parses; it is the ROWS that do not");
        };

        assert!(!read.list_truncated, "no truncation is involved");
        assert!(
            read.leaderless.is_empty(),
            "every row was dropped, unclassified"
        );
        assert_eq!(read.counts_not_leader_here, 12);
        assert_eq!(read.leaderless_unaccounted(), 12);
        assert!(
            read.leaderless_is_underread(),
            "coord counted leaderless workers and this read named NONE — an under-read, \
             whatever the truncation flag says"
        );

        // …and it is not merely flagged: predicate (iii) advances on the
        // count and pages, rather than clearing the streak as health.
        let mut state = ObserverState::default();
        let outcome = ProbeOutcome::Read(read);
        assert!(state.observe(&outcome).is_empty(), "probe 1");
        assert!(state.observe(&outcome).is_empty(), "probe 2");
        let reports = state.observe(&outcome);
        assert_eq!(
            reports.len(),
            1,
            "a coord with no leader must page: {reports:?}"
        );
        assert_eq!(reports[0].class, FaultClass::NoLeader);
        assert!(
            reports[0].summary.contains("12 leader-gated worker(s)"),
            "the count is the population: {}",
            reports[0].summary
        );
        assert!(
            reports[0].summary.contains("named NONE of them"),
            "and the report says the names were unavailable rather than printing none: {}",
            reports[0].summary
        );
    }

    /// **F1 — the D1 shape on predicate (ii)'s side of the house.** The
    /// mirror of
    /// `an_untruncated_read_that_named_no_leaderless_row_is_under_read_and_still_fires`,
    /// and the defect that shape's fix deliberately left open: (iii) was made
    /// immune by reading `counts.not_leader_here`, and (ii) — which has no
    /// count it can read, because `counts.dead` spans the follower plane —
    /// was left on the LIST with only a truncation-gated defense.
    ///
    /// So the same retype sinks it. coord emits `leader_gated` as the string
    /// `"true"` (or renames `status`); every row drops out of
    /// `classify_ledger`; `counts.dead` says 12; the list is WHOLE and
    /// `non_nominal_workers_truncated` is `false`. Before
    /// `LedgerRead::unclassified`, `dead_is_underread()`'s `list_truncated &&`
    /// conjunct read `false`, `unobserved_reason()` returned `None`, and the
    /// (iv) counter was CLEARED every cycle — no card, no incident line, no
    /// finding, indefinitely, on a runner that could not read a single row.
    #[test]
    fn an_untruncated_read_that_classified_no_row_is_under_read_and_reaches_predicate_four() {
        // Both silent retypes, on DEAD rows this time: a `leader_gated`
        // emitted as a string is dropped by the leader-gated filter, and a
        // `status` coord spells differently falls through the match below it.
        let retyped_gate = |name: &str| {
            let mut row = dead_row(name, true, false);
            row["leader_gated"] = json!("true");
            row
        };
        let status_renamed = |name: &str| {
            let mut row = dead_row(name, true, false);
            row["status"] = json!("Dead");
            row["reason"] = json!("deadLoop");
            row
        };
        let body = ledger_body(
            12,
            json!([
                retyped_gate("merge_dispatch"),
                retyped_gate("gate_sweep"),
                status_renamed("alert_pageout_worker"),
            ]),
        );
        let ProbeOutcome::Read(read) = classify_ledger(&body) else {
            panic!("expected a Read — the body parses; it is the ROWS that do not");
        };

        assert!(!read.list_truncated, "no truncation is involved");
        assert!(
            read.dead_leader_gated.is_empty() && read.rolled_off_excluded.is_empty(),
            "every row was dropped, unclassified"
        );
        assert_eq!(
            read.dead_follower_plane, 0,
            "a retyped gate is NOT a read `false` — it must not be accounted for as one"
        );
        assert_eq!(read.counts_dead, 12);
        assert!(
            !read.dead_is_underread(),
            "the truncation-gated predicate still cannot see this — which is the point"
        );
        assert!(
            read.classification_is_underread(),
            "a list this build could not classify leaves (ii) unobserved, truncation or not"
        );
        let reason = read
            .unobserved_reason()
            .expect("an unreadable list settles nothing about (ii)");
        assert!(
            reason.contains("could not classify 3 of the 3 row(s)"),
            "{reason}"
        );
        assert_eq!(
            read.unclassified.len(),
            3,
            "all three rows are unreadable, and named: {:?}",
            read.unclassified
        );

        // …and it is not merely flagged: it rides the counter that nothing
        // but a settling read clears, and reaches predicate (iv).
        let outcome = ProbeOutcome::Read(read);
        let mut state = ObserverState::default();
        let mut fired = Vec::new();
        for _ in 0..3 {
            fired.extend(state.observe(&outcome));
        }
        assert_eq!(fired.len(), 1, "{fired:?}");
        assert_eq!(fired[0].class, FaultClass::LivenessUnknown);
        assert!(
            fired[0].summary.contains("could not classify"),
            "{}",
            fired[0].summary
        );
    }

    /// The rows-that-never-arrived shape the classifier cannot see: coord
    /// counts twelve dead workers, sends an EMPTY list, and its truncation
    /// flag reads `false`. Before [`LedgerRead::rows_absent`] this reached
    /// `unobserved_reason() == None` and 500 probes raised no card.
    #[test]
    fn an_untruncated_list_short_of_counts_non_nominal_reaches_predicate_four() {
        let mut body = ledger_body(12, json!([]));
        body["components"]["counts"]["non_nominal"] = json!(12);
        let ProbeOutcome::Read(read) = classify_ledger(&body) else {
            panic!("expected a Read");
        };
        assert!(!read.list_truncated);
        assert!(
            !read.classification_is_underread() && !read.dead_is_underread(),
            "neither older defense can see a row that is not there"
        );
        assert_eq!(read.rows_absent(), 12);
        let reason = read
            .unobserved_reason()
            .expect("a list missing rows settles nothing about (ii)");
        assert!(reason.contains("carried only 0"), "{reason}");

        let outcome = ProbeOutcome::Read(read);
        let mut state = ObserverState::default();
        let mut fired = Vec::new();
        for _ in 0..3 {
            fired.extend(state.observe(&outcome));
        }
        assert_eq!(fired.len(), 1, "{fired:?}");
        assert_eq!(fired[0].class, FaultClass::LivenessUnknown);
        assert!(fired[0]
            .raw
            .contains("\"rows_absent_from_an_untruncated_list\": 12"));
    }

    /// The shortfall is only a signal when coord says NOTHING was cut. On a
    /// truncated list it is the cap, and `dead_is_underread` owns that.
    #[test]
    fn a_short_list_is_not_rows_absent_when_coord_says_it_truncated() {
        let body = truncated_ledger_body(0, json!([dead_row("follower.loop", false, false)]));
        let ProbeOutcome::Read(read) = classify_ledger(&body) else {
            panic!("expected a Read");
        };
        assert!(read.counts_non_nominal > read.listed_rows);
        assert_eq!(read.rows_absent(), 0);
        assert_eq!(read.unobserved_reason(), None);
    }

    /// Gated like the classifier arm: a read that named a dead worker has a
    /// `WorkerDead` card up, and "I have no verdict" beside it is false. The
    /// shortfall still travels in the card's body.
    #[test]
    fn rows_absent_beside_a_named_dead_worker_does_not_claim_no_verdict() {
        let mut body = ledger_body(3, json!([dead_row("merge_dispatch", true, false)]));
        body["components"]["counts"]["non_nominal"] = json!(3);
        let ProbeOutcome::Read(read) = classify_ledger(&body) else {
            panic!("expected a Read");
        };
        assert_eq!(read.rows_absent(), 2);
        assert_eq!(read.unobserved_reason(), None);
        let fired = ObserverState::default().observe(&ProbeOutcome::Read(read));
        assert_eq!(fired.len(), 1, "{fired:?}");
        assert_eq!(fired[0].class, FaultClass::WorkerDead);
        assert!(fired[0]
            .raw
            .contains("\"rows_absent_from_an_untruncated_list\": 2"));
    }

    /// `UNCLASSIFIED_IN_SUMMARY` bounds a HEADLINE; the remainder is counted,
    /// never dropped. Unpinned, a bound of 0 passed every other test and
    /// rendered `"; and 7 more"`.
    #[test]
    fn the_unclassified_summary_is_bounded_and_counts_the_remainder() {
        let label = |i: usize| format!("`w{i}`: status `x` is outside this build's vocabulary");
        let mut read = LedgerRead {
            unclassified: (0..UNCLASSIFIED_IN_SUMMARY + 2).map(label).collect(),
            ..LedgerRead::default()
        };
        let summary = read.unclassified_summary();
        let shown: Vec<String> = (0..UNCLASSIFIED_IN_SUMMARY).map(label).collect();
        assert_eq!(summary, format!("{}; and 2 more", shown.join("; ")));
        assert!(!summary.contains(&label(UNCLASSIFIED_IN_SUMMARY)));

        read.unclassified.truncate(UNCLASSIFIED_IN_SUMMARY);
        assert_eq!(read.unclassified_summary(), shown.join("; "));
        read.unclassified.truncate(1);
        assert_eq!(read.unclassified_summary(), label(0));
    }

    /// The other half of F1's fix, so it cannot be "hardened" into a predicate
    /// that pages on every ordinary fleet: a leader-gated `stale` or `alive`
    /// row is CLASSIFIED. This module deliberately holds no predicate over
    /// either (the `stale` class is coord's own pager's, and it sees it while
    /// a leader is live), and counting a deliberate non-interest as an
    /// unreadable row would make `LivenessUnknown` permanent everywhere.
    #[test]
    fn a_status_this_module_ignores_is_classified_not_unreadable() {
        let mut stale = no_leader_row("some.sweep");
        stale["status"] = json!("stale");
        stale["live_status"] = json!("stale");
        stale["reason"] = json!("stale");
        let mut alive = no_leader_row("other.sweep");
        alive["status"] = json!("alive");
        alive["live_status"] = json!("alive");
        alive["reason"] = json!(null);

        let body = ledger_body(0, json!([stale, alive]));
        let ProbeOutcome::Read(read) = classify_ledger(&body) else {
            panic!("expected a Read");
        };
        assert!(
            read.unclassified.is_empty(),
            "a known status this module ignores was READ: {:?}",
            read.unclassified
        );
        assert!(!read.classification_is_underread());
        assert_eq!(read.unobserved_reason(), None);
    }

    /// **F2 — a PERMANENT false page on a fleet this observer fully
    /// observed.** `dead_unaccounted()` subtracted the rows it kept and the
    /// rows it excluded, but not the dead rows it READ and dropped as
    /// follower-plane. Any fleet with more than 40 non-nominal workers (41
    /// stale rows will do — no dead row is needed to trip the cap) plus one
    /// dead follower-plane worker therefore produced a permanent
    /// `dead_is_underread()`, an hourly `warn!`, and — routed into a counter
    /// only a settling read clears — a `LivenessUnknown` card that never
    /// comes down, saying the list named none of the dead workers coord
    /// counted about a list that named every one of them.
    #[test]
    fn a_truncated_list_that_named_every_dead_worker_is_not_under_read() {
        // coord counts 2 dead; both are in the list, both follower-plane,
        // both read and correctly judged out of scope. The list is truncated
        // because 41 OTHER non-nominal rows tripped the cap — which is a fact
        // about `stale` rows, not about anything predicate (ii) cares for.
        let body = truncated_ledger_body(
            2,
            json!([
                dead_row("follower.one", false, false),
                dead_row("follower.two", false, false),
            ]),
        );
        let ProbeOutcome::Read(read) = classify_ledger(&body) else {
            panic!("expected a Read");
        };
        assert!(read.list_truncated);
        assert_eq!(read.counts_dead, 2);
        assert_eq!(
            read.dead_follower_plane, 2,
            "both dead rows were READ and classified"
        );
        assert_eq!(
            read.dead_unaccounted(),
            0,
            "a row this read judged is not a row it missed"
        );
        assert!(
            !read.dead_is_underread(),
            "predicate (ii) WAS observed on this cycle: the list named both dead workers"
        );
        assert_eq!(
            read.unobserved_reason(),
            None,
            "nothing about this read is UNKNOWN"
        );

        // 200 probes of it, because the defect's whole character is that it
        // never clears: the counter it fed is reset only by a settling read.
        let outcome = ProbeOutcome::Read(read);
        let mut state = ObserverState::default();
        let mut fired = Vec::new();
        for _ in 0..200 {
            fired.extend(state.observe(&outcome));
        }
        assert!(
            fired.is_empty(),
            "a fully-observed fleet must raise no card, ever: {fired:?}"
        );
    }

    /// F2's fix must not blunt the truncation predicate it corrects: a
    /// truncated list that named SOME of the dead workers is still an
    /// under-read for the rest.
    #[test]
    fn a_truncated_list_is_still_under_read_for_the_dead_rows_it_did_not_name() {
        // The third row is the one that pins F2's `status == dead` conjunct:
        // a follower-plane row that is NOT dead. `dead_follower_plane` must
        // not move for it. Without that conjunct the subtraction would eat
        // every follower-plane row whatever its status, and a truncated list
        // whose surviving follower rows are `stale` while the DEAD ones were
        // capped away would read as fully accounted for — silent, forever.
        let mut stale_follower = dead_row("follower.stale", false, false);
        stale_follower["status"] = json!("stale");
        stale_follower["live_status"] = json!("stale");
        stale_follower["reason"] = json!("stale");

        let body = truncated_ledger_body(
            4,
            json!([
                dead_row("follower.one", false, false),
                dead_row("a.sweep", true, false),
                stale_follower,
            ]),
        );
        let ProbeOutcome::Read(read) = classify_ledger(&body) else {
            panic!("expected a Read");
        };
        assert_eq!(
            read.dead_follower_plane, 1,
            "only the DEAD follower-plane row is subtractable; a `stale` one was never counted \
             by `counts.dead` and subtracting it would hide a capped-away dead worker"
        );
        // 4 counted − 1 leader-gated named − 1 dead follower-plane read = 2 unseen.
        assert_eq!(read.dead_unaccounted(), 2);
        assert!(read.dead_is_underread());
    }

    /// D1's asymmetry, pinned so nobody "fixes" it into a second analogue:
    /// `counts.dead` genuinely spans the follower plane, so a surplus there
    /// cannot DRIVE predicate (ii) and `dead_is_underread` MUST stay
    /// conditioned on truncation. (ii)'s OBSERVABILITY is a separate
    /// question, answered by `classification_is_underread` — see F1's test
    /// above.
    #[test]
    fn the_two_under_read_predicates_are_not_analogues() {
        let body = ledger_body(5, json!([dead_row("follower.loop", false, false)]));
        let ProbeOutcome::Read(read) = classify_ledger(&body) else {
            panic!("expected a Read");
        };
        assert!(!read.list_truncated);
        // 4, not 5 — the same deliberate correction as
        // `an_untruncated_list_is_never_under_read_however_the_counts_fall`:
        // the one follower-plane row in this list was SEEN, so it is
        // accounted for rather than counted as missing.
        assert_eq!(read.dead_unaccounted(), 4);
        assert!(
            !read.dead_is_underread(),
            "an untruncated `counts.dead` surplus is the follower plane, not a blind spot"
        );

        let mut body = ledger_body(0, json!([]));
        body["components"]["counts"]["not_leader_here"] = json!(5);
        let ProbeOutcome::Read(read) = classify_ledger(&body) else {
            panic!("expected a Read");
        };
        assert!(!read.list_truncated);
        assert!(
            read.leaderless_is_underread(),
            "the SAME untruncated surplus on `not_leader_here` is always a blind spot: every \
             worker that count covers is leader-gated and non-nominal by construction"
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
    fn a_truncated_leaderless_cycle_advances_the_streak_on_the_count() {
        let dark = ledger_body(0, json!([no_leader_row("alert_pageout_worker")]));
        // The surviving rows are follower-plane, so predicate (ii) is silent
        // and this test is about (iii) alone.
        let capped =
            truncated_leaderless_body(7, json!([dead_row("some.follower.loop", false, false)]));
        let mut state = ObserverState::default();
        assert!(state.observe(&classify_ledger(&dark)).is_empty(), "probe 1");
        assert!(state.observe(&classify_ledger(&dark)).is_empty(), "probe 2");
        // This cycle once read as a clean fleet (streak to 0, latch cleared),
        // then as an UNKNOWN that merely HELD the streak. Both were weaker
        // than the evidence: coord affirmatively counts 7 leaderless
        // leader-gated workers here. It is (iii), and it advances.
        let reports = state.observe(&classify_ledger(&capped));
        assert_eq!(
            reports.len(),
            1,
            "a count coord affirms is an observation, not an UNKNOWN: {reports:?}"
        );
        assert_eq!(reports[0].class, FaultClass::NoLeader);
        assert!(
            reports[0].summary.contains("7 leader-gated worker(s)"),
            "the population is the COUNT, not the surviving rows: {}",
            reports[0].summary
        );
    }

    /// D4. The onset case — every leaderless row capped away from the FIRST
    /// probe. The streak could never leave zero, so this paged nothing at all,
    /// forever, against the module's own doctrine that a watcher which has
    /// gone blind must say so on the surface it would have used.
    #[test]
    fn a_leaderless_episode_capped_away_from_its_onset_still_pages() {
        let capped =
            truncated_leaderless_body(38, json!([dead_row("some.follower.loop", false, false)]));
        let mut state = ObserverState::default();
        assert!(
            state.observe(&classify_ledger(&capped)).is_empty(),
            "probe 1"
        );
        assert!(
            state.observe(&classify_ledger(&capped)).is_empty(),
            "probe 2"
        );
        let reports = state.observe(&classify_ledger(&capped));
        assert_eq!(reports.len(), 1, "probe 3 reaches the cadence: {reports:?}");
        assert_eq!(reports[0].class, FaultClass::NoLeader);
        assert!(
            reports[0].post_finding,
            "coord ANSWERED this cycle, so the observation is carried back"
        );
        assert!(
            reports[0].summary.contains("named NONE of them"),
            "it reports the count and says the names are unavailable: {}",
            reports[0].summary
        );
        assert!(
            reports[0].summary.contains("38 leader-gated worker(s)"),
            "{}",
            reports[0].summary
        );
        // One episode, one card.
        for _ in 0..5 {
            assert!(state.observe(&classify_ledger(&capped)).is_empty());
        }
        // …and a coord that regains its leader re-arms it.
        assert!(state
            .observe(&classify_ledger(&ledger_body(0, json!([]))))
            .is_empty());
        for _ in 0..2 {
            assert!(state.observe(&classify_ledger(&capped)).is_empty());
        }
        assert_eq!(state.observe(&classify_ledger(&capped)).len(), 1);
    }

    /// A partially-capped report names what it has AND says how much it does
    /// not — never a silently shrunken population.
    #[test]
    fn a_partially_named_leaderless_report_says_how_many_it_could_not_name() {
        let mut body = truncated_leaderless_body(9, json!([no_leader_row("merge_dispatch")]));
        body["components"]["counts"]["not_leader_here"] = json!(9);
        let mut state = ObserverState::default();
        let mut fired = Vec::new();
        for _ in 0..3 {
            fired.extend(state.observe(&classify_ledger(&body)));
        }
        assert_eq!(fired.len(), 1);
        let summary = &fired[0].summary;
        assert!(summary.contains("9 leader-gated worker(s)"), "{summary}");
        assert!(summary.contains("merge_dispatch"), "{summary}");
        assert!(
            summary.contains("8 more coord counted but did not name"),
            "{summary}"
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
        for key in ["dead", "not_leader_here", "non_nominal"] {
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

    /// D2. The truncation flag itself defaulted to `false` twenty-five lines
    /// below a comment refusing to default its two neighbours — and naming
    /// this exact harm, in the words *"`list_truncated` reading `false`"*. A
    /// renamed or retyped flag left `dead_is_underread` reading `false` on
    /// every cycle: a permanently clean fleet with no warning anywhere.
    #[test]
    fn an_absent_or_wrong_typed_truncation_flag_is_unusable_not_false() {
        let mut absent = ledger_body(0, json!([]));
        absent["components"]
            .as_object_mut()
            .unwrap()
            .remove("non_nominal_workers_truncated");
        match classify_ledger(&absent) {
            ProbeOutcome::Unusable { reason } => {
                assert!(reason.contains("absent"), "{reason}");
                assert!(reason.contains("UNKNOWN, not `false`"), "{reason}");
            }
            other => panic!("an absent truncation flag is UNKNOWN, not `false`: {other:?}"),
        }

        for wrong in [json!("true"), json!(1), json!(null), json!(["true"])] {
            let mut body = ledger_body(0, json!([]));
            body["components"]["non_nominal_workers_truncated"] = wrong.clone();
            assert!(
                matches!(classify_ledger(&body), ProbeOutcome::Unusable { .. }),
                "a {wrong} truncation flag is UNKNOWN, not `false`"
            );
        }
    }

    /// …and, like its two neighbours, it reaches predicate (iv) after three
    /// cadences rather than dying in the log.
    #[test]
    fn a_missing_truncation_flag_reaches_predicate_four() {
        let mut body = ledger_body(0, json!([]));
        body["components"]
            .as_object_mut()
            .unwrap()
            .remove("non_nominal_workers_truncated");
        let mut state = ObserverState::default();
        let mut fired = Vec::new();
        for _ in 0..3 {
            fired.extend(state.observe(&classify_ledger(&body)));
        }
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].class, FaultClass::LivenessUnknown);
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

    // ── D3: an alternating door is coord being DOWN, and must page ──────

    /// F8 was right in isolation and wrong in composition. An `Unreachable`
    /// really does contradict "coord answered unreadably" and an `Unusable`
    /// really does contradict "coord did not answer" — so a door alternating
    /// between them pinned BOTH per-arm streaks at 1 and neither predicate
    /// could ever reach `CADENCES_TO_FIRE`. The test that stood here asserted
    /// that silence as correct.
    ///
    /// The real shape: a load balancer with one target serving a 200
    /// maintenance page (→ `Unusable`) and one refusing connections (→
    /// `Unreachable`). coord wholly down, and the observer produced no card,
    /// no incident line and no finding, indefinitely.
    #[test]
    fn an_alternating_door_reaches_the_cadence_as_liveness_unknown() {
        let mut state = ObserverState::default();
        let unusable = ProbeOutcome::Unusable {
            reason: "a shape this build cannot parse".into(),
        };
        let down = ProbeOutcome::Unreachable {
            reason: "connection refused".into(),
        };

        // The per-arm streaks still contradict each other, so NEITHER (i) nor
        // a per-arm (iv) fires — correctly.
        assert!(state.observe(&unusable).is_empty(), "probe 1");
        assert!(state.observe(&down).is_empty(), "probe 2");
        // …but three probes have now produced no usable observation of coord,
        // and THAT is the honest predicate.
        let reports = state.observe(&unusable);
        assert_eq!(
            reports.len(),
            1,
            "a wholly-down coord behind a flapping door must not be silence: {reports:?}"
        );
        assert_eq!(reports[0].class, FaultClass::LivenessUnknown);
        assert!(
            reports[0].summary.contains("NO usable observation"),
            "{}",
            reports[0].summary
        );
        assert!(
            reports[0].summary.contains("3 consecutive probes"),
            "the counter nothing resets: {}",
            reports[0].summary
        );
        assert!(
            !reports[0].post_finding,
            "there is no coord to post to — that is the whole condition"
        );

        // One episode, one card, however long the flap lasts.
        for _ in 0..20 {
            assert!(state.observe(&down).is_empty());
            assert!(state.observe(&unusable).is_empty());
        }
        // A settling read is the ONLY thing that clears it — and re-arms it.
        assert!(state
            .observe(&classify_ledger(&ledger_body(0, json!([]))))
            .is_empty());
        assert!(state.observe(&down).is_empty());
        assert!(state.observe(&unusable).is_empty());
        assert_eq!(
            state.observe(&down).len(),
            1,
            "a second blind episode pages again"
        );
    }

    /// Predicate (i) still owns a coord that is simply not answering, and
    /// (iv) must not double-card the same episode.
    #[test]
    fn a_plain_unreachable_outage_raises_one_card_not_two() {
        let mut state = ObserverState::default();
        let down = ProbeOutcome::Unreachable {
            reason: "connection refused".into(),
        };
        let mut fired = Vec::new();
        for _ in 0..10 {
            fired.extend(state.observe(&down));
        }
        assert_eq!(fired.len(), 1, "one card: {fired:?}");
        assert_eq!(
            fired[0].class,
            FaultClass::Unreachable,
            "`coord has not answered on N consecutive probes` is the stronger statement"
        );
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
