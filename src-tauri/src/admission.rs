//! Coord spawn admission, as THIS runner takes part in it — the spawn
//! reporter, the lease client, and the local token bucket that stands in for
//! coord when coord cannot answer.
//!
//! Plan `2026-10-01-runner-spawn-bursts-are-unregulated-coord-must-admit-spawns-per-machine`,
//! Phases 0–2 (runner half). On 2026-10-01 ~180 terminals landed on one runner
//! in five minutes and nothing outside the runner process saw or bounded any of
//! it: every spawn path asked only "is this one process over its thread ceiling
//! right now", against a sample that did not yet include the spawns racing
//! beside it. The fix moves *how many and how fast* to coord, which owns a
//! per-device bucket, and leaves *can this process physically take one more* to
//! the runner's own guards. Both must agree before an unattended spawn runs.
//!
//! ## The three pieces
//!
//! * **Reporter (Phase 0).** [`record_spawn`] is called at every spawn site in
//!   `runner_spawn_sites.txt` — attended ones included, because coord's live
//!   count must include the bare shells and operator chats its own session
//!   table never sees (the incident's 178 bare shells were invisible to coord).
//!   [`run_reporter`] POSTs the CUMULATIVE per-origin counters since this
//!   process started, tagged with a per-boot [`boot_id`], to
//!   `POST /coord/devices/me/spawn-admission/report` every 30 s. Cumulative so a
//!   lost report loses no count: coord computes deltas idempotently per boot.
//! * **Lease client (Phase 1).** [`acquire`] asks
//!   `POST /coord/devices/me/spawn-admission` for `count` permits and answers
//!   [`Admission::Granted`] (possibly `n == 0`, a coord refusal) or
//!   [`Admission::Unknown`]. [`release`] returns a lease with the number of
//!   permits actually used. Every admission-route response carries coord's
//!   standing `budget`, cached in [`cached_budget`].
//! * **Local bucket.** Admission UNKNOWN — no coord device, no device JWT, a 5xx,
//!   a transport failure, a timeout over 3 s — THROTTLES rather than stops:
//!   unattended spawns draw from a conservative [`TokenBucket`] (burst 4, refill
//!   4/min, env-tunable). This deliberately differs from the drain, where
//!   UNKNOWN defers everything: a drain is an explicit stop order, admission is
//!   a RATE, and an offline runner must still be able to do its work — just not
//!   180 spawns of it at once. The bucket is for a coord that HAS admission but
//!   cannot answer right now.
//! * **A 404 is not UNKNOWN — it is a definite answer that admission does not
//!   exist on this coord yet.** A 404 from the acquire route BEFORE this process
//!   has ever seen an admission route answer 2xx passes the spawn through: that
//!   is the pre-plan state, where the 64-session cap and the thread-pressure
//!   defer are the backstops, and throttling it to 4 + 4/min would punish a
//!   runner for meeting an older coord. Logged at info once per cooldown and
//!   counted (`spawn_admission_route_absent` in the continuation poll report).
//!   A 404 AFTER a 2xx is a regression or a misroute, not an older coord, and
//!   takes the local bucket like any other UNKNOWN.
//!
//! ## Who enforces what (Phase 2)
//!
//! Only gate/unit continuations take a permit today
//! ([`admit_continuation`], called by `agent_runtime::dispatch_gate_continuation`
//! BEFORE the per-row `tokio::spawn`). Every other autonomous site reports but
//! does not yet ask — that is plan
//! `2026-10-01-runner-local-spawns-and-restore-take-coord-admission-leases`.
//! Attended operator spawns are NEVER refused by coord; they are reported so the
//! count stays true. Coord's enforcement switch is server-side: in shadow mode
//! it grants everything and records the verdict it would have given, so landing
//! this client before coord enforces changes no spawn outcome against a coord
//! that serves the route.
//!
//! ## Failure posture
//!
//! Nothing here ever fails a spawn by erroring: reporter failures are logged and
//! the next tick re-sends the cumulative totals; a release that cannot be sent
//! lets the lease lapse at coord's 120 s TTL; an unanswerable acquire is
//! `Unknown`, which the caller turns into a local-bucket decision.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::coord_drain_state::SpawnOrigin;

/// Coord's acquire answer must arrive within this, or admission is UNKNOWN.
/// Contract: "timeout >3 s → UNKNOWN". A continuation dispatch waits on this
/// once per row, so it is also the worst-case latency admission adds to a
/// dispatch while coord is slow.
pub const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(3);

/// Timeout for one release POST. Best-effort: a release that does not land
/// lets the lease lapse at coord's TTL instead.
const RELEASE_TIMEOUT: Duration = Duration::from_secs(3);

/// Timeout for one report POST. Longer than the acquire's because nothing waits
/// on it — the reporter runs on its own tick.
const REPORT_TIMEOUT: Duration = Duration::from_secs(5);

/// The reporter's cadence (D5: "sent on a 30 s tick").
pub const REPORT_INTERVAL: Duration = Duration::from_secs(30);

/// Attended spawns since the last report that trigger an IMMEDIATE report
/// instead of waiting for the tick (D5: "and immediately when a batch of ≥ 4
/// attended spawns lands"). A restore wave or a burst of operator clicks is
/// exactly the moment coord's live count most needs to be current.
pub const ATTENDED_BURST_REPORT_THRESHOLD: u64 = 4;

/// After a transport failure, a timeout or a 5xx, further acquires skip the
/// network for this long and answer `Unknown` straight away. A continuation
/// backlog dispatches its rows one after another, each awaiting its own
/// acquire; without this, 130 rows against an unreachable coord would cost
/// 130 × 3 s ≈ 6.5 minutes of serial timeouts before the local bucket ever
/// answered. One report interval: the next tick is the natural retry.
const UNREACHABLE_COOLDOWN: Duration = Duration::from_secs(30);

/// After a 404 (the route is not served by this coord), further acquires skip
/// the network for this long. A coord build changes on a deploy, not between
/// two rows; five minutes keeps a pre-route coord from costing a round trip per
/// spawn while still noticing the deploy promptly.
const ROUTE_ABSENT_COOLDOWN: Duration = Duration::from_secs(300);

/// Local bucket burst when coord cannot answer (§3 Offline: "default burst 4").
pub const DEFAULT_LOCAL_BURST: u32 = 4;

/// Local bucket refill per minute when coord cannot answer ("refill 4/min").
pub const DEFAULT_LOCAL_REFILL_PER_MIN: u32 = 4;

/// Env override for [`DEFAULT_LOCAL_BURST`]. Read once, at first use.
pub const LOCAL_BURST_ENV: &str = "QONTINUI_SPAWN_ADMISSION_LOCAL_BURST";

/// Env override for [`DEFAULT_LOCAL_REFILL_PER_MIN`]. Read once, at first use.
pub const LOCAL_REFILL_ENV: &str = "QONTINUI_SPAWN_ADMISSION_LOCAL_REFILL_PER_MIN";

// ---------------------------------------------------------------------------
// Spawn class
// ---------------------------------------------------------------------------

/// The admission class a spawn belongs to — the contract's `class` vocabulary.
///
/// Derived from the [`SpawnOrigin`] alone ([`SpawnClass::of`]) so there is no
/// second vocabulary a site could disagree with: the origin is already stamped
/// by every spawner for the drain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SpawnClass {
    /// A runner-originated unattended spawn that is not a continuation:
    /// restore, looping agent, scheduler, HTTP/relay create, respawn, handoff,
    /// orchestration fan-out, boot resume. Coord's `local` class.
    Local,
    /// A gate or work-unit continuation coord delivered. Coord's
    /// `continuation` class — the one this runner takes permits for today.
    Continuation,
    /// An operator sitting at THIS runner asked for it. Never refused by coord;
    /// reported so coord's live count is true.
    Attended,
}

impl SpawnClass {
    /// The class for `origin`. Continuation origins are `Continuation`, the two
    /// operator origins are `Attended`, and EVERY other origin — `Unknown`
    /// included, which the drain also treats as autonomous — is `Local`.
    pub fn of(origin: SpawnOrigin) -> Self {
        match origin {
            SpawnOrigin::GateContinuation | SpawnOrigin::UnitContinuation => {
                SpawnClass::Continuation
            }
            o if !o.is_autonomous() => SpawnClass::Attended,
            _ => SpawnClass::Local,
        }
    }

    /// The contract wire value: `local` | `continuation` | `attended`.
    pub fn as_wire(self) -> &'static str {
        match self {
            SpawnClass::Local => "local",
            SpawnClass::Continuation => "continuation",
            SpawnClass::Attended => "attended",
        }
    }
}

// ---------------------------------------------------------------------------
// Phase 0 — the spawn reporter
// ---------------------------------------------------------------------------

/// Cumulative spawn counts since this process started, one slot per
/// [`SpawnOrigin::ALL`] entry. Atomics rather than a mutex because
/// [`record_spawn`] sits on every spawn path, attended clicks included, and
/// must never contend with the reporter's snapshot.
struct SpawnCounters {
    by_origin: [AtomicU64; SpawnOrigin::ALL.len()],
    /// Attended spawns recorded since the last report was SENT — the D5
    /// immediate-report trigger, separate from the cumulative totals.
    attended_since_report: AtomicU64,
}

impl SpawnCounters {
    const fn new() -> Self {
        Self {
            by_origin: [const { AtomicU64::new(0) }; SpawnOrigin::ALL.len()],
            attended_since_report: AtomicU64::new(0),
        }
    }

    /// Count one spawn of `origin`. Returns `true` when this spawn completes
    /// an attended burst that should be reported now rather than on the tick.
    fn record(&self, origin: SpawnOrigin) -> bool {
        self.by_origin[origin_index(origin)].fetch_add(1, Ordering::Relaxed);
        if SpawnClass::of(origin) == SpawnClass::Attended {
            let n = self.attended_since_report.fetch_add(1, Ordering::Relaxed) + 1;
            return n == ATTENDED_BURST_REPORT_THRESHOLD;
        }
        false
    }

    /// The cumulative totals, nonzero origins only, in `SpawnOrigin::ALL`
    /// order. Also re-arms the attended-burst trigger: the report being built
    /// carries every attended spawn counted so far.
    fn snapshot_for_report(&self) -> Vec<(SpawnOrigin, u64)> {
        self.attended_since_report.store(0, Ordering::Relaxed);
        SpawnOrigin::ALL
            .iter()
            .map(|o| (*o, self.by_origin[origin_index(*o)].load(Ordering::Relaxed)))
            .filter(|(_, n)| *n > 0)
            .collect()
    }
}

/// The slot of `origin` in [`SpawnOrigin::ALL`]. `ALL` lists every variant
/// (pinned by `coord_drain_state`'s own vocabulary test), so the lookup cannot
/// miss; the `unwrap_or` keeps a future variant added to the enum but not to
/// `ALL` from panicking a spawn path — it lands in the `Unknown` slot instead.
fn origin_index(origin: SpawnOrigin) -> usize {
    SpawnOrigin::ALL
        .iter()
        .position(|o| *o == origin)
        .unwrap_or(SpawnOrigin::ALL.len() - 1)
}

static COUNTERS: SpawnCounters = SpawnCounters::new();

/// Wakes the reporter early when an attended burst lands.
fn report_now() -> &'static tokio::sync::Notify {
    static NOTIFY: OnceLock<tokio::sync::Notify> = OnceLock::new();
    NOTIFY.get_or_init(tokio::sync::Notify::new)
}

/// Count one spawn of `origin` for coord's live count.
///
/// Called at every spawn site `runner_spawn_sites.txt` lists as `autonomous`
/// or `operator` — pinned by
/// `runner_spawn_sites::every_autonomous_and_operator_site_reports_its_spawn` —
/// at the point the site has decided to act — right after its drain /
/// agent-registry gate admits the spawn, or right before the spawn primitive
/// where that is the clearer seam. Cheap (one relaxed atomic add), synchronous,
/// never fails, never blocks: it is on the attended click path too.
///
/// It counts spawn INTENTS the runner admitted, not successful starts: a
/// request that then fails validation, the resource guard or the spawn itself
/// is still counted. That is the quantity a burst is made of and the one coord's
/// bucket meters; a precise started-session count is coord's own session table.
pub fn record_spawn(origin: SpawnOrigin) {
    if COUNTERS.record(origin) {
        report_now().notify_one();
    }
}

/// This process's boot id: stable for the life of the process, fresh on every
/// start. Coord keys the cumulative counters by it, so a restart (counters back
/// to zero) is a new series rather than a negative delta.
pub fn boot_id() -> &'static str {
    static BOOT_ID: OnceLock<String> = OnceLock::new();
    BOOT_ID.get_or_init(|| uuid::Uuid::now_v7().to_string())
}

/// One origin's cumulative count on the report wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReportCounter {
    pub class: &'static str,
    pub count: u64,
}

/// `POST /coord/devices/me/spawn-admission/report` body (contract).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReportBody {
    pub boot_id: String,
    pub counters: BTreeMap<&'static str, ReportCounter>,
}

/// PURE: the report body for `boot_id` and a cumulative snapshot. An origin
/// with no spawns is omitted rather than sent as zero — coord reads an absent
/// origin in a boot's latest report as zero for that boot, and the body stays
/// small on an idle runner.
pub fn report_body(boot_id: &str, snapshot: &[(SpawnOrigin, u64)]) -> ReportBody {
    ReportBody {
        boot_id: boot_id.to_string(),
        counters: snapshot
            .iter()
            .filter(|(_, n)| *n > 0)
            .map(|(o, n)| {
                (
                    o.as_wire(),
                    ReportCounter {
                        class: SpawnClass::of(*o).as_wire(),
                        count: *n,
                    },
                )
            })
            .collect(),
    }
}

#[derive(Debug, Deserialize)]
struct ReportResponse {
    #[serde(default)]
    budget: Option<Budget>,
}

/// The reporter loop: every [`REPORT_INTERVAL`], or at once when an attended
/// burst lands, POST the cumulative counters. Runs for the runner's lifetime on
/// the fleet-heartbeat runtime (the thread built so a busy sibling cannot
/// starve a 30 s cadence).
///
/// Sends even when nothing has been spawned: an empty-counter report is how
/// coord tells "reporter alive, nothing spawned" apart from "reporter dead",
/// and a missing report ages the device's reported-spawn term to UNKNOWN on
/// coord's side (D5), never to zero.
///
/// Every failure is logged and swallowed — the next tick re-sends the
/// cumulative totals, so a lost report loses nothing. A failure never touches
/// any spawn.
pub async fn run_reporter() {
    let mut tick = tokio::time::interval(REPORT_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    info!(boot_id = boot_id(), "admission: spawn reporter started");
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            _ = report_now().notified() => {}
        }
        report_once().await;
    }
}

/// Build and send one report. See [`run_reporter`].
async fn report_once() {
    let target = match resolve_target() {
        Ok(t) => t,
        Err(why) => {
            debug!("admission: spawn report not sent — {}", why.describe());
            return;
        }
    };
    let body = report_body(boot_id(), &COUNTERS.snapshot_for_report());
    let url = format!(
        "{}/coord/devices/me/spawn-admission/report",
        target.base.trim_end_matches('/')
    );
    // A per-report client, like `coord_drain_state::read_me_drain`'s: the
    // reporter runs on the fleet-heartbeat thread's dedicated current-thread
    // runtime, and a connection pooled by the shared client must not be opened
    // on one runtime and reused from another. One connection per 30 s is cheap.
    let client = match reqwest::Client::builder().build() {
        Ok(c) => c,
        Err(e) => {
            warn!("admission: spawn report not sent — reqwest builder: {e}");
            return;
        }
    };
    match post_json(&client, &url, &target.bearer, &body, REPORT_TIMEOUT).await {
        Ok((status, text)) if (200..300).contains(&status) => {
            note_route_answered();
            if let Ok(resp) = serde_json::from_str::<ReportResponse>(&text) {
                if let Some(budget) = resp.budget {
                    note_budget(budget);
                }
            }
            debug!(
                origins = body.counters.len(),
                "admission: spawn report recorded"
            );
        }
        Ok((404, _)) => debug!(
            "admission: spawn report route not served by this coord (404) — it predates \
             the admission routes; counters keep accumulating and the next report carries them"
        ),
        Ok((status, text)) => warn!(
            "admission: spawn report returned {status}: {}",
            excerpt(&text)
        ),
        Err(e) => warn!("admission: spawn report failed: {}", e.describe()),
    }
}

// ---------------------------------------------------------------------------
// Phase 1 — the lease client
// ---------------------------------------------------------------------------

/// Coord's standing budget for this device, carried on every admission-route
/// response (D3: the budget rides the authenticated routes, never the anonymous
/// heartbeat).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct Budget {
    /// Permits coord would grant right now. Signed: a device over its bucket
    /// can read negative on a coord that reports the overdraft.
    pub permits_available: i64,
    pub refill_per_min: u32,
    pub burst: u32,
}

/// The last budget coord sent, and when.
fn budget_cache() -> &'static Mutex<Option<(Budget, Instant)>> {
    static CACHE: OnceLock<Mutex<Option<(Budget, Instant)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

fn note_budget(budget: Budget) {
    *lock(budget_cache()) = Some((budget, Instant::now()));
}

/// The most recent budget coord sent and its age, or `None` when no
/// admission-route response has carried one yet. Named in every admission
/// deferral this runner logs, so an operator reading "deferred" also sees what
/// coord last said the device had left. The age is part of the answer: a budget
/// is a reading, and a stale reading must not read as a current one.
pub fn cached_budget() -> Option<(Budget, Duration)> {
    lock(budget_cache()).map(|(b, at)| (b, at.elapsed()))
}

/// Why admission is UNKNOWN — every arm the contract names, plus the parse and
/// unexpected-status arms a real network produces. Each one throttles through
/// the local bucket; none is ever read as a grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnknownReason {
    /// This runner is not a coord device (no `machine.json`, no coord URL).
    NotEnrolled(String),
    /// The local enrollment files could not be read, so whether this runner is
    /// a coord device is itself unknown.
    EnrollmentIndeterminate(String),
    /// A coord device, but no usable device JWT is held. The admission routes
    /// are device-authed; an anonymous request would be refused, and sending
    /// one would also be the wrong posture (an unauthenticated count is a lever
    /// on another device's admission — D3).
    NoDeviceJwt,
    /// 404 — coord does not serve the admission routes (a build that predates
    /// them). The plan's deploy order puts the route first, but a runner may
    /// still meet an older coord, and a 404 must never read as a grant.
    RouteAbsent,
    /// A 5xx from coord.
    ServerError(u16),
    /// Any other non-2xx (401/403 on a rejected credential, 409, 422, …).
    UnexpectedStatus(u16),
    /// No answer within [`ACQUIRE_TIMEOUT`].
    Timeout,
    /// The request could not be sent or its response not read.
    Transport(String),
    /// A 2xx whose body this build cannot parse.
    Unparseable(String),
    /// An earlier failure put the door in cooldown; no request was sent.
    CoolingDown(Box<UnknownReason>),
}

impl UnknownReason {
    /// Is this a 404 — coord does not serve the route — directly or as the
    /// cause of a running cooldown?
    pub fn is_route_absent(&self) -> bool {
        match self {
            UnknownReason::RouteAbsent => true,
            UnknownReason::CoolingDown(inner) => inner.is_route_absent(),
            _ => false,
        }
    }

    /// Operator-readable sentence.
    pub fn describe(&self) -> String {
        match self {
            UnknownReason::NotEnrolled(why) => format!("not a coord device ({why})"),
            UnknownReason::EnrollmentIndeterminate(why) => {
                format!("coord enrollment could not be read ({why})")
            }
            UnknownReason::NoDeviceJwt => "no usable device JWT".to_string(),
            UnknownReason::RouteAbsent => {
                "coord does not serve the spawn-admission route (404)".to_string()
            }
            UnknownReason::ServerError(s) => format!("coord answered {s}"),
            UnknownReason::UnexpectedStatus(s) => format!("coord answered {s}"),
            UnknownReason::Timeout => {
                format!("coord did not answer within {}s", ACQUIRE_TIMEOUT.as_secs())
            }
            UnknownReason::Transport(e) => format!("request failed: {e}"),
            UnknownReason::Unparseable(e) => format!("response did not parse: {e}"),
            UnknownReason::CoolingDown(inner) => {
                format!("{} (cooling down, not retried yet)", inner.describe())
            }
        }
    }

    /// How long this failure keeps the door in cooldown, if at all. Only the
    /// failures a retry a moment later would meet again — the network is down,
    /// coord is erroring, the route is not deployed — cool down. A missing
    /// credential or enrollment is resolved locally and re-checked on every
    /// call at no network cost; a 4xx or a parse failure names a specific
    /// answer worth seeing again.
    fn cooldown(&self) -> Option<Duration> {
        match self {
            UnknownReason::Timeout
            | UnknownReason::Transport(_)
            | UnknownReason::ServerError(_) => Some(UNREACHABLE_COOLDOWN),
            UnknownReason::RouteAbsent => Some(ROUTE_ABSENT_COOLDOWN),
            _ => None,
        }
    }
}

/// Coord's answer to one acquire, as this client reads it.
///
/// The contract names two arms, `Granted{n, lease_id}` and `Unknown{reason}`;
/// the extra `Granted` fields carry what coord said about a short grant so a
/// refusal can be stamped and logged honestly rather than reconstructed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    /// Coord answered. `n` permits were granted (`0` is a refusal: coord is
    /// reachable and said "not now"). `lease_id` addresses the grant for
    /// [`release`]; `None` when nothing was granted, or when a coord granted
    /// without a lease (nothing to return).
    Granted {
        n: u32,
        lease_id: Option<uuid::Uuid>,
        /// Coord's `held_by`-style refusal key, when it gave one.
        reason: Option<String>,
        retry_after_secs: Option<u64>,
        /// Coord is in shadow mode: it granted everything and recorded the
        /// verdict it would have given.
        shadow: bool,
    },
    /// Coord could not be asked, or did not answer usably.
    Unknown { reason: UnknownReason },
}

/// Where and as whom to send an admission request.
struct Target {
    base: String,
    bearer: String,
}

/// Resolve the coord base and the device JWT, or the UNKNOWN reason neither
/// exists. Enrollment is the drain's own three-way reading
/// ([`crate::coord_drain_state::enrollment`]) so the two coord surfaces cannot
/// disagree about whether this runner is a coord device.
#[cfg(not(test))]
fn resolve_target() -> Result<Target, UnknownReason> {
    use crate::coord_drain_state::Enrollment;
    let base = match crate::coord_drain_state::enrollment() {
        Enrollment::Enrolled(base) => base,
        Enrollment::NotEnrolled(why) => return Err(UnknownReason::NotEnrolled(why.to_string())),
        Enrollment::Indeterminate(why) => return Err(UnknownReason::EnrollmentIndeterminate(why)),
    };
    // `Device` scope: the admission routes key on the JWT's own device and take
    // the tenant from its claim, so the default binding's credential is the
    // right one however many tenants are paired.
    let bearer = crate::auth::device_bearer_scoped(crate::auth::TenantScope::Device)
        .ok_or(UnknownReason::NoDeviceJwt)?;
    Ok(Target { base, bearer })
}

/// The `cfg(test)` [`resolve_target`]: a unit test must never present the
/// operator's real device JWT to the real coord — a permit dropped inside a
/// test runtime would otherwise POST a release for a fabricated lease to
/// production. Tests reach a local fake coord through `tests::TEST_TARGET`, or
/// get the not-enrolled arm.
#[cfg(test)]
fn resolve_target() -> Result<Target, UnknownReason> {
    lock(&tests::TEST_TARGET)
        .clone()
        .map(|(base, bearer)| Target { base, bearer })
        .ok_or_else(|| UnknownReason::NotEnrolled("unit test: no coord door".to_string()))
}

/// `POST /coord/devices/me/spawn-admission` body (contract).
#[derive(Debug, Clone, Serialize)]
struct AcquireBody<'a> {
    origin: &'static str,
    class: &'static str,
    count: u32,
    work_keys: &'a [String],
}

#[derive(Debug, Deserialize)]
struct AcquireResponse {
    granted: u32,
    #[serde(default)]
    lease_id: Option<uuid::Uuid>,
    #[serde(default)]
    retry_after_secs: Option<u64>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    budget: Option<Budget>,
    #[serde(default)]
    shadow: bool,
}

/// PURE: read one acquire exchange. `outcome` is the status and body, or the
/// transport failure. `asked` clamps a coord that over-grants — the runner
/// never holds more permits than it asked for.
fn read_acquire(
    outcome: Result<(u16, String), PostError>,
    asked: u32,
) -> (Admission, Option<Budget>) {
    let unknown = |reason| (Admission::Unknown { reason }, None);
    let (status, body) = match outcome {
        Ok(pair) => pair,
        Err(PostError::Timeout) => return unknown(UnknownReason::Timeout),
        Err(PostError::Transport(e)) => return unknown(UnknownReason::Transport(e)),
    };
    match status {
        200..=299 => match serde_json::from_str::<AcquireResponse>(&body) {
            Ok(r) => (
                Admission::Granted {
                    n: r.granted.min(asked),
                    lease_id: r.lease_id,
                    reason: r.reason.filter(|s| !s.trim().is_empty()),
                    retry_after_secs: r.retry_after_secs,
                    shadow: r.shadow,
                },
                r.budget,
            ),
            Err(e) => unknown(UnknownReason::Unparseable(e.to_string())),
        },
        404 => unknown(UnknownReason::RouteAbsent),
        500..=599 => unknown(UnknownReason::ServerError(status)),
        other => unknown(UnknownReason::UnexpectedStatus(other)),
    }
}

/// The acquire door's cooldown: after a failure a retry would only repeat, skip
/// the network until it expires. See [`UNREACHABLE_COOLDOWN`].
#[derive(Debug, Default)]
struct DoorHealth {
    cooling: Option<(Instant, UnknownReason)>,
}

impl DoorHealth {
    /// The reason to answer without asking, while a cooldown is running.
    fn blocked(&self, now: Instant) -> Option<UnknownReason> {
        match &self.cooling {
            Some((until, why)) if now < *until => Some(why.clone()),
            _ => None,
        }
    }

    /// Fold one real answer: a cooling failure starts (or restarts) the
    /// cooldown; anything coord actually answered clears it.
    fn note(&mut self, admission: &Admission, now: Instant) {
        match admission {
            Admission::Unknown { reason } => {
                if let Some(d) = reason.cooldown() {
                    self.cooling = Some((now + d, reason.clone()));
                }
            }
            Admission::Granted { .. } => self.cooling = None,
        }
    }
}

fn door_health() -> &'static Mutex<DoorHealth> {
    static HEALTH: OnceLock<Mutex<DoorHealth>> = OnceLock::new();
    HEALTH.get_or_init(|| Mutex::new(DoorHealth::default()))
}

/// Whether any admission route has answered 2xx in this process. Separates the
/// two meanings of a 404: before any 2xx it is a coord that predates admission
/// (pass through); after one it is a regression or a misroute (local bucket).
/// Per-process and never reset: a coord that once served the route and now
/// 404s is exactly the case that must not run free.
static ROUTE_ANSWERED: AtomicBool = AtomicBool::new(false);

/// PURE: does this acquire answer prove coord serves the admission route? Only
/// a parsed 2xx grant does. Every non-2xx — a 403 `device_principal_required`
/// (a token that is not a paired device token: headless and temp runners), a
/// 400 for a malformed body, a 401, a 5xx — is an UNKNOWN answer that must not
/// flip [`ROUTE_ANSWERED`]: a 403 from a coord that predates admission's
/// router would otherwise turn every later 404 into a regression.
fn proves_route_served(admission: &Admission) -> bool {
    matches!(admission, Admission::Granted { .. })
}

fn note_route_answered() {
    ROUTE_ANSWERED.store(true, Ordering::Relaxed);
}

/// See [`ROUTE_ANSWERED`].
pub fn route_has_answered() -> bool {
    ROUTE_ANSWERED.load(Ordering::Relaxed)
}

/// Ask coord for `count` permits of `class` for a spawn of `origin`.
///
/// Never errors and never blocks past [`ACQUIRE_TIMEOUT`]: every failure is an
/// [`Admission::Unknown`] naming its cause. A 404 is reported as
/// [`UnknownReason::RouteAbsent`], never as a grant; whether it passes the spawn
/// through or draws on the local bucket is [`decide`]'s call, made against
/// [`route_has_answered`].
///
/// Uses the process-wide [`crate::coord_http::coord_client`]: every acquire and
/// release runs on the main runtime (the continuation dispatcher and the
/// permits it hands out), so one pooled client is correct there.
pub async fn acquire(
    origin: SpawnOrigin,
    class: SpawnClass,
    count: u32,
    work_keys: &[String],
) -> Admission {
    let target = match resolve_target() {
        Ok(t) => t,
        Err(reason) => return Admission::Unknown { reason },
    };
    if let Some(why) = lock(door_health()).blocked(Instant::now()) {
        return Admission::Unknown {
            reason: UnknownReason::CoolingDown(Box::new(why)),
        };
    }
    let Some(client) = crate::coord_http::coord_client() else {
        return Admission::Unknown {
            reason: UnknownReason::Transport("no shared coord HTTP client".to_string()),
        };
    };
    let admission = acquire_at(
        client,
        &target.base,
        &target.bearer,
        origin,
        class,
        count,
        work_keys,
        ACQUIRE_TIMEOUT,
    )
    .await;
    lock(door_health()).note(&admission, Instant::now());
    admission
}

/// [`acquire`] against an explicit base and bearer, with no cooldown — the
/// network half, separated so a test can drive it against a local server.
async fn acquire_at(
    client: &reqwest::Client,
    base: &str,
    bearer: &str,
    origin: SpawnOrigin,
    class: SpawnClass,
    count: u32,
    work_keys: &[String],
    timeout: Duration,
) -> Admission {
    let url = format!(
        "{}/coord/devices/me/spawn-admission",
        base.trim_end_matches('/')
    );
    let body = AcquireBody {
        origin: origin.as_wire(),
        class: class.as_wire(),
        count,
        work_keys,
    };
    let (admission, budget) =
        read_acquire(post_json(client, &url, bearer, &body, timeout).await, count);
    if let Some(budget) = budget {
        note_budget(budget);
    }
    if proves_route_served(&admission) {
        note_route_answered();
    }
    admission
}

/// `POST /coord/devices/me/spawn-admission/{lease_id}/release` body.
#[derive(Debug, Serialize)]
struct ReleaseBody {
    used: u32,
}

/// Return lease `lease_id` to coord, saying how many of its permits were used.
/// `used > 0` CONVERTS those permits (the sessions now count for themselves);
/// the rest go back to the bucket. Best-effort: every failure is logged, and a
/// release that never lands lapses at coord's 120 s TTL (D4), which coord
/// counts — so a runner that routinely fails to release is visible there.
pub async fn release(lease_id: uuid::Uuid, used: u32) {
    let target = match resolve_target() {
        Ok(t) => t,
        Err(why) => {
            debug!(
                "admission: lease {lease_id} not released ({}) — it lapses at coord's TTL",
                why.describe()
            );
            return;
        }
    };
    let Some(client) = crate::coord_http::coord_client() else {
        debug!("admission: lease {lease_id} not released (no shared coord HTTP client) — it lapses at coord's TTL");
        return;
    };
    release_at(
        client,
        &target.base,
        &target.bearer,
        lease_id,
        used,
        RELEASE_TIMEOUT,
    )
    .await;
}

/// [`release`] against an explicit client, base and bearer.
async fn release_at(
    client: &reqwest::Client,
    base: &str,
    bearer: &str,
    lease_id: uuid::Uuid,
    used: u32,
    timeout: Duration,
) {
    let url = format!(
        "{}/coord/devices/me/spawn-admission/{lease_id}/release",
        base.trim_end_matches('/')
    );
    match post_json(client, &url, bearer, &ReleaseBody { used }, timeout).await {
        Ok((status, text)) if (200..300).contains(&status) => {
            note_route_answered();
            if let Ok(resp) = serde_json::from_str::<ReportResponse>(&text) {
                if let Some(budget) = resp.budget {
                    note_budget(budget);
                }
            }
            debug!("admission: lease {lease_id} released (used={used})");
        }
        Ok((status, text)) => warn!(
            "admission: release of lease {lease_id} returned {status}: {} — it lapses at \
             coord's TTL",
            excerpt(&text)
        ),
        Err(e) => warn!(
            "admission: release of lease {lease_id} failed ({}) — it lapses at coord's TTL",
            e.describe()
        ),
    }
}

/// A POST that did not produce a status.
#[derive(Debug)]
enum PostError {
    Timeout,
    Transport(String),
}

impl PostError {
    fn describe(&self) -> String {
        match self {
            PostError::Timeout => "timed out".to_string(),
            PostError::Transport(e) => e.clone(),
        }
    }
}

/// The ONE coord write this module makes: POST `body` to `url` on `client`
/// with the device JWT `bearer` and a per-request `timeout`, and return the
/// status and body text. The client is the caller's choice — the shared pooled
/// one on the main runtime, a per-report one on the heartbeat runtime.
async fn post_json<B: Serialize + ?Sized>(
    client: &reqwest::Client,
    url: &str,
    bearer: &str,
    body: &B,
    timeout: Duration,
) -> Result<(u16, String), PostError> {
    // coord-auth-exempt(device-jwt-required): fails CLOSED — `resolve_target`
    // returns `UnknownReason::NoDeviceJwt` before any request when no device JWT
    // is held, and `bearer` is that JWT. The fail-soft helper would send the
    // admission request ANONYMOUSLY instead, which coord refuses (the routes are
    // device-authed), turning "no credential" into a misleading 401 and hiding
    // the local cause the UNKNOWN arm exists to name.
    let resp = client
        .post(url)
        .timeout(timeout)
        .bearer_auth(bearer)
        .json(body)
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                PostError::Timeout
            } else {
                PostError::Transport(e.to_string())
            }
        })?;
    let status = resp.status().as_u16();
    let text = resp.text().await.map_err(|e| {
        if e.is_timeout() {
            PostError::Timeout
        } else {
            PostError::Transport(format!("reading the response: {e}"))
        }
    })?;
    Ok((status, text))
}

fn excerpt(text: &str) -> String {
    text.chars().take(200).collect()
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match m.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    }
}

// ---------------------------------------------------------------------------
// The local token bucket (admission UNKNOWN)
// ---------------------------------------------------------------------------

/// A token bucket: up to `burst` tokens, refilled continuously at
/// `refill_per_min`. Starts FULL, so a runner that boots offline can still
/// restore a handful of sessions at once.
#[derive(Debug, Clone)]
pub struct TokenBucket {
    burst: f64,
    refill_per_sec: f64,
    tokens: f64,
    last: Instant,
}

impl TokenBucket {
    /// A full bucket. `burst == 0` is a CLOSED bucket (every take fails) —
    /// coord's `Ceilings::from_lookup` rule, where `0` means closed rather than
    /// unlimited. `refill_per_min == 0` never refills.
    pub fn new(burst: u32, refill_per_min: u32, now: Instant) -> Self {
        Self {
            burst: f64::from(burst),
            refill_per_sec: f64::from(refill_per_min) / 60.0,
            tokens: f64::from(burst),
            last: now,
        }
    }

    /// How long one token takes to refill — the earliest a deferred spawn could
    /// be admitted again — or `None` for a bucket that never refills.
    pub fn refill_period(&self) -> Option<Duration> {
        (self.refill_per_sec > 0.0).then(|| Duration::from_secs_f64(1.0 / self.refill_per_sec))
    }

    fn refill(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_per_sec).min(self.burst);
        self.last = now;
    }

    /// Take `n` whole tokens if all are available; otherwise take none.
    pub fn try_take(&mut self, n: u32, now: Instant) -> bool {
        self.refill(now);
        let want = f64::from(n);
        // A small epsilon so a refill computed from float seconds that lands a
        // hair under a whole token (0.9999999) still counts as one.
        if self.tokens + 1e-9 >= want {
            self.tokens = (self.tokens - want).max(0.0);
            true
        } else {
            false
        }
    }

    /// Whole tokens available at `now`.
    #[cfg(test)]
    pub fn available(&mut self, now: Instant) -> u32 {
        self.refill(now);
        // Bounded by `burst`, itself a u32, so the cast cannot overflow.
        (self.tokens + 1e-9).floor() as u32
    }
}

/// PURE: one bucket parameter from an env lookup — an absent or invalid value
/// is the default, a valid one (including `0`, which closes the bucket) is
/// taken as is. The same rule as coord's `Ceilings::from_lookup`.
pub fn bucket_param(lookup: impl Fn(&str) -> Option<String>, key: &str, default: u32) -> u32 {
    match lookup(key) {
        Some(raw) => match raw.trim().parse::<u32>() {
            Ok(v) => v,
            Err(_) => {
                warn!("admission: {key}={raw:?} is not a non-negative integer; using {default}");
                default
            }
        },
        None => default,
    }
}

/// The process-wide local bucket, sized from the env at first use.
fn local_bucket() -> &'static Mutex<TokenBucket> {
    static BUCKET: OnceLock<Mutex<TokenBucket>> = OnceLock::new();
    BUCKET.get_or_init(|| {
        let env = |k: &str| std::env::var(k).ok();
        let burst = bucket_param(env, LOCAL_BURST_ENV, DEFAULT_LOCAL_BURST);
        let refill = bucket_param(env, LOCAL_REFILL_ENV, DEFAULT_LOCAL_REFILL_PER_MIN);
        info!("admission: local bucket burst={burst} refill_per_min={refill}");
        Mutex::new(TokenBucket::new(burst, refill, Instant::now()))
    })
}

// ---------------------------------------------------------------------------
// Phase 2 — the continuation permit
// ---------------------------------------------------------------------------

/// Where a permit came from — which decides what releasing it means.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PermitSource {
    /// Coord granted it; `lease` (when coord issued one) is returned on
    /// release or conversion.
    Coord { lease: Option<uuid::Uuid> },
    /// Coord could not answer; the local bucket paid for it. A local token is
    /// spent at take and never returned — the bucket meters a RATE, and a
    /// spawn that then failed still happened as far as the rate is concerned.
    LocalBucket,
    /// Coord answered 404 and no admission route has ever answered 2xx in this
    /// process: coord predates admission, so the spawn passes through on the
    /// pre-plan backstops (the 64 cap and the thread-pressure defer).
    RouteAbsent,
    /// No admission applies to this spawn (see [`AdmissionPermit::not_required`]).
    NotRequired,
}

/// A held admission permit for ONE continuation spawn, from the check through
/// session registration (D4: "the runner holds a permit from check through
/// session registration, then releases or converts it").
///
/// The spawn path calls [`AdmissionPermit::converted`] at the moment the
/// session exists (the same moment it hands its anchor reservation to the
/// registry); every other exit drops the permit, which releases the lease with
/// `used = 0` so the permit goes back to coord's bucket immediately instead of
/// at the TTL.
#[derive(Debug)]
pub struct AdmissionPermit {
    source: PermitSource,
}

impl AdmissionPermit {
    /// A permit for a spawn admission does not govern. The one production
    /// caller is the legacy continuation frame with neither a `gate_id` nor a
    /// `dispatch_id`: it has no re-delivery, so a deferral there would be a
    /// silent DROP of the work rather than a "not now" — the same reason the
    /// boot-time presentation deferral excludes it. Tests use it too.
    pub fn not_required() -> Self {
        Self {
            source: PermitSource::NotRequired,
        }
    }

    /// `true` when this spawn passed through because coord does not serve the
    /// admission route at all (see [`PermitSource::RouteAbsent`]) — counted
    /// separately by the dispatcher so a fleet still on a pre-admission coord
    /// is visible in the poll report.
    pub fn passed_through_absent_route(&self) -> bool {
        self.source == PermitSource::RouteAbsent
    }

    /// The session now exists and counts for itself: convert the permit.
    pub fn converted(mut self) {
        self.settle(1);
    }

    /// Release or convert, once. A coord lease is returned on whatever runtime
    /// is current; with none (a drop outside tokio), the lease lapses at coord's
    /// TTL, which is the designed fallback, not a leak.
    fn settle(&mut self, used: u32) {
        let PermitSource::Coord { lease } = &mut self.source else {
            return;
        };
        let Some(lease_id) = lease.take() else {
            return;
        };
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(release(lease_id, used));
            }
            Err(_) => debug!(
                "admission: lease {lease_id} settled outside a tokio runtime — it lapses at \
                 coord's TTL"
            ),
        }
    }

    #[cfg(test)]
    fn lease(&self) -> Option<uuid::Uuid> {
        match &self.source {
            PermitSource::Coord { lease } => *lease,
            _ => None,
        }
    }
}

impl Drop for AdmissionPermit {
    fn drop(&mut self) {
        self.settle(0);
    }
}

/// The admission verdict for one continuation.
#[derive(Debug)]
pub enum ContinuationAdmission {
    /// Spawn, holding this permit until the session exists.
    Admitted(AdmissionPermit),
    /// Do not spawn now; leave the row pending.
    ///
    /// * `key` — `refused:<coord's reason>` or `local_bucket_empty`, for logs.
    /// * `detail` — the operator-readable sentence.
    /// * `retry_after` — the earliest a re-poll could be admitted: the larger of
    ///   coord's `retry_after_secs` and one refill period (coord's refill when
    ///   coord refused, the local bucket's when it paid). `None` when nothing
    ///   will refill — the periodic backstop poll is the only retry then.
    ///
    /// There is deliberately NO coord `continuation-deferred` stamp for this
    /// verdict: that stamp is rate-limited to one per gate per hour, and an
    /// admission deferral clears within seconds to a minute, so stamping it
    /// would mask the next real reason (`thread_pressure:`, `at_cap:`) for an
    /// hour — the same reason the boot-time presentation deferral posts none.
    /// Coord sees admission deferrals through its own ledger and the poll
    /// report's `spawn_admission_deferred` count instead.
    Deferred {
        key: String,
        detail: String,
        retry_after: Option<Duration>,
    },
}

/// What [`decide`] needs besides coord's answer and the bucket.
#[derive(Debug, Clone, Copy)]
pub struct DecideContext {
    /// [`route_has_answered`] at decision time.
    pub route_answered_before: bool,
    /// Coord's last-known refill rate (from the cached budget), used to time
    /// the re-poll after a coord refusal.
    pub coord_refill_per_min: Option<u32>,
}

/// The later of coord's hint and one refill period.
fn retry_after_of(hint_secs: Option<u64>, refill_period: Option<Duration>) -> Option<Duration> {
    match (hint_secs.map(Duration::from_secs), refill_period) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

/// PURE over its inputs: turn coord's answer into a verdict, drawing on the
/// local `bucket` only when coord has admission but could not answer.
///
/// * `Granted { n ≥ 1 }` → admitted on the coord lease.
/// * `Granted { n = 0 }` → coord is reachable and refused: deferred
///   `refused:<coord's reason>`. A lease coord attached to the refusal anyway is
///   released at once with `used = 0`. The local bucket is NOT consulted —
///   coord's "no" is an answer, and only its silence falls back.
/// * `Unknown` from a 404 while no admission route has ever answered 2xx →
///   admitted without admission ([`PermitSource::RouteAbsent`]): coord predates
///   the routes.
/// * any other `Unknown` (a 404 after a 2xx included) → one local token:
///   admitted, or deferred `local_bucket_empty` when the bucket is dry.
pub fn decide(
    admission: Admission,
    ctx: DecideContext,
    bucket: &mut TokenBucket,
    now: Instant,
) -> ContinuationAdmission {
    match admission {
        Admission::Granted { n, lease_id, .. } if n >= 1 => {
            ContinuationAdmission::Admitted(AdmissionPermit {
                source: PermitSource::Coord { lease: lease_id },
            })
        }
        Admission::Granted {
            lease_id,
            reason,
            retry_after_secs,
            ..
        } => {
            // A zero grant should carry no lease; if one does, hand it straight
            // back rather than let it sit out the TTL. Dropping the permit is
            // the release (used = 0).
            drop(AdmissionPermit {
                source: PermitSource::Coord { lease: lease_id },
            });
            let key = reason.unwrap_or_else(|| "no_reason".to_string());
            let retry = retry_after_secs
                .map(|s| format!("; coord suggests retrying in {s}s"))
                .unwrap_or_default();
            let coord_period = ctx
                .coord_refill_per_min
                .filter(|r| *r > 0)
                .map(|r| Duration::from_secs_f64(60.0 / f64::from(r)));
            ContinuationAdmission::Deferred {
                key: format!("refused:{key}"),
                detail: format!("coord refused the spawn permit ({key}){retry}"),
                retry_after: retry_after_of(retry_after_secs, coord_period),
            }
        }
        Admission::Unknown { reason } if reason.is_route_absent() && !ctx.route_answered_before => {
            ContinuationAdmission::Admitted(AdmissionPermit {
                source: PermitSource::RouteAbsent,
            })
        }
        Admission::Unknown { reason } => {
            if bucket.try_take(1, now) {
                ContinuationAdmission::Admitted(AdmissionPermit {
                    source: PermitSource::LocalBucket,
                })
            } else {
                ContinuationAdmission::Deferred {
                    key: "local_bucket_empty".to_string(),
                    detail: format!(
                        "coord admission unknown ({}) and the local spawn bucket is empty",
                        reason.describe()
                    ),
                    retry_after: bucket.refill_period(),
                }
            }
        }
    }
}

/// When the route-absent pass-through was last logged at info. One line per
/// [`ROUTE_ABSENT_COOLDOWN`] — every continuation passes through while coord
/// predates admission, and a line per row would bury the log.
fn route_absent_logged() -> &'static Mutex<Option<Instant>> {
    static LAST: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();
    LAST.get_or_init(|| Mutex::new(None))
}

/// Admit one continuation spawn of `origin` for `work_key` (a `gate:<id>` /
/// `dispatch:<id>` key). Called by the continuation dispatcher AFTER the cheap
/// local guards and BEFORE it spawns the run task, so a backlog drains at the
/// admitted rate instead of all at once — the in-flight race the 64-session
/// cap's doc says that cap cannot close (every row reads the pre-burst
/// registry).
pub async fn admit_continuation(origin: SpawnOrigin, work_key: &str) -> ContinuationAdmission {
    let admission = acquire(origin, SpawnClass::Continuation, 1, &[work_key.to_string()]).await;
    match &admission {
        Admission::Unknown { reason } => debug!(
            "admission: {work_key}: coord admission unknown ({})",
            reason.describe()
        ),
        // Shadow mode: coord granted regardless and recorded the verdict it
        // would have given. Logged so a shadow window is visible from this
        // side too; the grant itself is honoured exactly like a real one.
        Admission::Granted { shadow: true, .. } => {
            debug!("admission: {work_key}: coord granted in shadow mode")
        }
        Admission::Granted { .. } => {}
    }
    let ctx = DecideContext {
        route_answered_before: route_has_answered(),
        coord_refill_per_min: cached_budget().map(|(b, _)| b.refill_per_min),
    };
    let verdict = decide(admission, ctx, &mut lock(local_bucket()), Instant::now());
    match &verdict {
        ContinuationAdmission::Admitted(p) if p.passed_through_absent_route() => {
            let mut last = lock(route_absent_logged());
            if last.is_none_or(|t| t.elapsed() >= ROUTE_ABSENT_COOLDOWN) {
                *last = Some(Instant::now());
                info!(
                    "admission: coord does not serve the spawn-admission route (404) and never \
                     has in this process — it predates admission, so continuations pass through \
                     on the local backstops (64-session cap, thread-pressure defer); logged once \
                     per {}s",
                    ROUTE_ABSENT_COOLDOWN.as_secs()
                );
            }
        }
        ContinuationAdmission::Deferred { detail, .. } => {
            let budget = cached_budget()
                .map(|(b, age)| {
                    format!(
                        " (coord's last budget, {}s old: {} available, burst {}, refill {}/min)",
                        age.as_secs(),
                        b.permits_available,
                        b.burst,
                        b.refill_per_min
                    )
                })
                .unwrap_or_default();
            // Debug, not info: the dispatcher logs the one operator-facing
            // line per poll (it stops asking after the first deferral), and a
            // second line here would double it.
            debug!("admission: {work_key} deferred — {detail}{budget}");
        }
        ContinuationAdmission::Admitted(_) => {}
    }
    verdict
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// The coord door [`resolve_target`] answers under `cfg(test)`: `None`
    /// (the not-enrolled arm) unless a test points it at a local fake coord.
    pub(super) static TEST_TARGET: Mutex<Option<(String, String)>> = Mutex::new(None);

    /// A context where coord has served the admission routes before (so a 404
    /// would take the bucket) and no budget is cached.
    const SEEN: DecideContext = DecideContext {
        route_answered_before: true,
        coord_refill_per_min: None,
    };

    // --- class mapping -----------------------------------------------------

    #[test]
    fn every_origin_has_exactly_the_class_the_contract_names() {
        for origin in SpawnOrigin::ALL {
            let want = match origin {
                SpawnOrigin::GateContinuation | SpawnOrigin::UnitContinuation => "continuation",
                SpawnOrigin::OperatorTerminal | SpawnOrigin::OperatorChat => "attended",
                _ => "local",
            };
            assert_eq!(SpawnClass::of(origin).as_wire(), want, "{origin}");
        }
    }

    // --- reporter ------------------------------------------------------------

    #[test]
    fn counters_are_cumulative_and_the_report_carries_totals_not_deltas() {
        let c = SpawnCounters::new();
        c.record(SpawnOrigin::LoopingAgent);
        c.record(SpawnOrigin::LoopingAgent);
        c.record(SpawnOrigin::GateContinuation);
        let first = report_body("boot-a", &c.snapshot_for_report());
        assert_eq!(first.counters["looping_agent"].count, 2);
        assert_eq!(first.counters["looping_agent"].class, "local");
        assert_eq!(first.counters["gate_continuation"].count, 1);
        assert_eq!(first.counters["gate_continuation"].class, "continuation");
        assert!(
            !first.counters.contains_key("steward"),
            "an origin with no spawns is omitted, not sent as zero"
        );

        // A second report after one more spawn carries the RUNNING total, so a
        // lost first report would have lost nothing.
        c.record(SpawnOrigin::LoopingAgent);
        let second = report_body("boot-a", &c.snapshot_for_report());
        assert_eq!(second.counters["looping_agent"].count, 3);
        assert_eq!(second.counters["gate_continuation"].count, 1);
    }

    #[test]
    fn the_report_body_serializes_to_the_contract_shape() {
        let body = report_body(
            "b-1",
            &[
                (SpawnOrigin::OperatorTerminal, 5),
                (SpawnOrigin::Steward, 0),
            ],
        );
        let json = serde_json::to_value(&body).expect("serialize");
        assert_eq!(
            json,
            serde_json::json!({
                "boot_id": "b-1",
                "counters": {"operator_terminal": {"class": "attended", "count": 5}}
            })
        );
    }

    #[test]
    fn the_fourth_attended_spawn_since_a_report_triggers_an_immediate_report() {
        let c = SpawnCounters::new();
        // Unattended spawns never trigger it, however many.
        for _ in 0..10 {
            assert!(!c.record(SpawnOrigin::Respawn));
        }
        assert!(!c.record(SpawnOrigin::OperatorTerminal));
        assert!(!c.record(SpawnOrigin::OperatorChat));
        assert!(!c.record(SpawnOrigin::OperatorTerminal));
        assert!(
            c.record(SpawnOrigin::OperatorTerminal),
            "the 4th attended spawn completes the burst"
        );
        assert!(
            !c.record(SpawnOrigin::OperatorTerminal),
            "fires once per burst, not on every later spawn"
        );
        // Building a report re-arms it.
        let _ = c.snapshot_for_report();
        for _ in 0..3 {
            assert!(!c.record(SpawnOrigin::OperatorTerminal));
        }
        assert!(c.record(SpawnOrigin::OperatorTerminal));
    }

    #[test]
    fn the_boot_id_is_stable_for_the_process_and_a_valid_uuid() {
        let a = boot_id();
        let b = boot_id();
        assert_eq!(a, b, "one boot id per process");
        assert!(std::ptr::eq(a, b), "the same allocation, not a re-mint");
        assert!(uuid::Uuid::parse_str(a).is_ok(), "{a} is a uuid");
    }

    #[test]
    fn record_spawn_moves_the_process_counters() {
        let before =
            COUNTERS.by_origin[origin_index(SpawnOrigin::Scheduler)].load(Ordering::Relaxed);
        record_spawn(SpawnOrigin::Scheduler);
        let after =
            COUNTERS.by_origin[origin_index(SpawnOrigin::Scheduler)].load(Ordering::Relaxed);
        assert!(
            after > before,
            "record_spawn counts into the reported totals"
        );
    }

    // --- acquire: every UNKNOWN arm -----------------------------------------

    fn ok(status: u16, body: &str) -> Result<(u16, String), PostError> {
        Ok((status, body.to_string()))
    }

    #[test]
    fn a_404_is_unknown_never_a_grant() {
        let (a, _) = read_acquire(ok(404, "not found"), 1);
        assert_eq!(
            a,
            Admission::Unknown {
                reason: UnknownReason::RouteAbsent
            }
        );
    }

    #[test]
    fn a_5xx_is_unknown() {
        for s in [500, 502, 503] {
            let (a, _) = read_acquire(ok(s, ""), 1);
            assert_eq!(
                a,
                Admission::Unknown {
                    reason: UnknownReason::ServerError(s)
                }
            );
        }
    }

    #[test]
    fn a_timeout_and_a_transport_failure_are_unknown() {
        let (a, _) = read_acquire(Err(PostError::Timeout), 1);
        assert_eq!(
            a,
            Admission::Unknown {
                reason: UnknownReason::Timeout
            }
        );
        let (a, _) = read_acquire(Err(PostError::Transport("refused".into())), 1);
        assert!(matches!(
            a,
            Admission::Unknown {
                reason: UnknownReason::Transport(_)
            }
        ));
    }

    /// Coord answers 403 `device_principal_required` to a token that is not a
    /// paired device token, and 400 to a malformed body. Both are UNKNOWN —
    /// never a grant, never a hard refusal — take the local bucket even before
    /// any 2xx (they are not the route-absent 404), and never prove the route.
    #[test]
    fn a_403_or_400_is_unknown_takes_the_bucket_and_does_not_prove_the_route() {
        for status in [403u16, 400] {
            let (a, budget) =
                read_acquire(ok(status, r#"{"error":"device_principal_required"}"#), 1);
            assert_eq!(
                a,
                Admission::Unknown {
                    reason: UnknownReason::UnexpectedStatus(status)
                }
            );
            assert_eq!(budget, None);
            assert!(
                !proves_route_served(&a),
                "{status} must not flip the route bit"
            );
            let t0 = Instant::now();
            let mut bucket = TokenBucket::new(1, 0, t0);
            let fresh = DecideContext {
                route_answered_before: false,
                coord_refill_per_min: None,
            };
            match decide(a, fresh, &mut bucket, t0) {
                ContinuationAdmission::Admitted(p) => {
                    assert!(
                        !p.passed_through_absent_route(),
                        "{status} is not a pass-through"
                    )
                }
                other => panic!("{status} with a full bucket admits on it, got {other:?}"),
            }
            assert_eq!(bucket.available(t0), 0, "{status} spent the local token");
        }
        assert!(proves_route_served(&grant()));
    }

    #[test]
    fn an_unexpected_status_and_an_unparseable_2xx_are_unknown() {
        let (a, _) = read_acquire(ok(401, ""), 1);
        assert_eq!(
            a,
            Admission::Unknown {
                reason: UnknownReason::UnexpectedStatus(401)
            }
        );
        let (a, _) = read_acquire(ok(200, "{\"nope\":1}"), 1);
        assert!(matches!(
            a,
            Admission::Unknown {
                reason: UnknownReason::Unparseable(_)
            }
        ));
    }

    #[test]
    fn a_grant_is_read_with_its_budget_and_clamped_to_the_ask() {
        let lease = uuid::Uuid::now_v7();
        let body = serde_json::json!({
            "granted": 5, "lease_id": lease, "lease_expires_at": "2026-10-01T00:02:00Z",
            "retry_after_secs": null, "reason": null,
            "budget": {"permits_available": 7, "refill_per_min": 6, "burst": 8},
            "shadow": true
        })
        .to_string();
        let (a, budget) = read_acquire(ok(200, &body), 2);
        assert_eq!(
            a,
            Admission::Granted {
                n: 2,
                lease_id: Some(lease),
                reason: None,
                retry_after_secs: None,
                shadow: true
            },
            "an over-grant is clamped to what was asked"
        );
        assert_eq!(
            budget,
            Some(Budget {
                permits_available: 7,
                refill_per_min: 6,
                burst: 8
            })
        );
    }

    #[test]
    fn no_device_jwt_and_not_enrolled_are_unknown_reasons_that_throttle_without_cooling_down() {
        // These two arms are decided before any request (`resolve_target`);
        // what matters downstream is that they are UNKNOWN, throttle through
        // the bucket, and do not trip the network cooldown (they cost nothing
        // to re-check).
        for reason in [
            UnknownReason::NoDeviceJwt,
            UnknownReason::NotEnrolled("machine.json is missing".into()),
            UnknownReason::EnrollmentIndeterminate("machine.json unreadable".into()),
        ] {
            assert_eq!(reason.cooldown(), None, "{reason:?}");
            let mut bucket = TokenBucket::new(1, 0, Instant::now());
            assert!(matches!(
                decide(
                    Admission::Unknown {
                        reason: reason.clone()
                    },
                    SEEN,
                    &mut bucket,
                    Instant::now()
                ),
                ContinuationAdmission::Admitted(_)
            ));
        }
    }

    #[test]
    fn network_failures_cool_the_door_down_and_an_answer_clears_it() {
        let t0 = Instant::now();
        let mut h = DoorHealth::default();
        h.note(
            &Admission::Unknown {
                reason: UnknownReason::Timeout,
            },
            t0,
        );
        assert_eq!(
            h.blocked(t0 + Duration::from_secs(1)),
            Some(UnknownReason::Timeout)
        );
        assert_eq!(h.blocked(t0 + UNREACHABLE_COOLDOWN), None, "expires");

        h.note(
            &Admission::Unknown {
                reason: UnknownReason::RouteAbsent,
            },
            t0,
        );
        assert!(
            h.blocked(t0 + UNREACHABLE_COOLDOWN).is_some(),
            "404 cools longer"
        );
        assert!(h.blocked(t0 + ROUTE_ABSENT_COOLDOWN).is_none());

        h.note(
            &Admission::Unknown {
                reason: UnknownReason::ServerError(503),
            },
            t0,
        );
        h.note(
            &Admission::Granted {
                n: 1,
                lease_id: None,
                reason: None,
                retry_after_secs: None,
                shadow: false,
            },
            t0,
        );
        assert_eq!(h.blocked(t0), None, "a real answer clears the cooldown");
    }

    // --- acquire/release against a real local server ------------------------

    /// What the fake coord does for each acquire, in order.
    #[derive(Clone)]
    enum Script {
        Grant,
        Refuse,
        /// A zero grant that (wrongly) carries a lease anyway.
        RefuseWithLease(uuid::Uuid),
        Status(u16),
        Hang,
    }

    struct FakeCoord {
        base: String,
        acquires: Arc<Mutex<Vec<serde_json::Value>>>,
        releases: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
        auth: Arc<Mutex<Vec<Option<String>>>>,
    }

    async fn fake_coord(script: Vec<Script>) -> FakeCoord {
        use axum::extract::{Path, State};
        use axum::http::{HeaderMap, StatusCode};
        use axum::routing::post;
        use axum::{Json, Router};

        #[derive(Clone)]
        struct St {
            script: Arc<Mutex<std::collections::VecDeque<Script>>>,
            acquires: Arc<Mutex<Vec<serde_json::Value>>>,
            releases: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
            auth: Arc<Mutex<Vec<Option<String>>>>,
        }
        let st = St {
            script: Arc::new(Mutex::new(script.into())),
            acquires: Arc::default(),
            releases: Arc::default(),
            auth: Arc::default(),
        };
        async fn acquire_h(
            State(st): State<St>,
            headers: HeaderMap,
            Json(body): Json<serde_json::Value>,
        ) -> axum::response::Response {
            use axum::response::IntoResponse;
            lock(&st.auth).push(
                headers
                    .get("authorization")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string),
            );
            lock(&st.acquires).push(body);
            let step = lock(&st.script).pop_front().unwrap_or(Script::Grant);
            let budget =
                serde_json::json!({"permits_available": 3, "refill_per_min": 6, "burst": 8});
            match step {
                Script::Grant => Json(serde_json::json!({
                    "granted": 1, "lease_id": uuid::Uuid::now_v7(),
                    "lease_expires_at": "2026-10-01T00:02:00Z", "retry_after_secs": null,
                    "reason": null, "budget": budget, "shadow": false
                }))
                .into_response(),
                Script::Refuse => Json(serde_json::json!({
                    "granted": 0, "lease_id": null, "lease_expires_at": null,
                    "retry_after_secs": 10, "reason": "bucket_empty",
                    "budget": budget, "shadow": false
                }))
                .into_response(),
                Script::RefuseWithLease(lease) => Json(serde_json::json!({
                    "granted": 0, "lease_id": lease, "lease_expires_at": null,
                    "retry_after_secs": null, "reason": "bucket_empty",
                    "budget": budget, "shadow": false
                }))
                .into_response(),
                Script::Status(s) => StatusCode::from_u16(s)
                    .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
                    .into_response(),
                Script::Hang => {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    StatusCode::OK.into_response()
                }
            }
        }
        async fn release_h(
            State(st): State<St>,
            Path(lease): Path<String>,
            Json(body): Json<serde_json::Value>,
        ) -> Json<serde_json::Value> {
            lock(&st.releases).push((lease, body));
            Json(serde_json::json!({"released": true}))
        }
        let app = Router::new()
            .route("/coord/devices/me/spawn-admission", post(acquire_h))
            .route(
                "/coord/devices/me/spawn-admission/{lease_id}/release",
                post(release_h),
            )
            .with_state(st.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        FakeCoord {
            base: format!("http://{addr}"),
            acquires: st.acquires,
            releases: st.releases,
            auth: st.auth,
        }
    }

    #[tokio::test]
    async fn acquire_sends_the_contract_body_with_the_bearer_and_reads_each_arm() {
        let coord = fake_coord(vec![
            Script::Grant,
            Script::Refuse,
            Script::Status(404),
            Script::Status(503),
        ])
        .await;
        let keys = vec!["gate:abc".to_string()];
        let client = reqwest::Client::new();
        let go = || {
            acquire_at(
                &client,
                &coord.base,
                "tok",
                SpawnOrigin::GateContinuation,
                SpawnClass::Continuation,
                1,
                &keys,
                ACQUIRE_TIMEOUT,
            )
        };
        assert!(matches!(
            go().await,
            Admission::Granted {
                n: 1,
                lease_id: Some(_),
                ..
            }
        ));
        assert!(matches!(
            go().await,
            Admission::Granted { n: 0, lease_id: None, ref reason, retry_after_secs: Some(10), .. }
                if reason.as_deref() == Some("bucket_empty")
        ));
        assert_eq!(
            go().await,
            Admission::Unknown {
                reason: UnknownReason::RouteAbsent
            }
        );
        assert_eq!(
            go().await,
            Admission::Unknown {
                reason: UnknownReason::ServerError(503)
            }
        );
        let sent = lock(&coord.acquires).clone();
        assert_eq!(
            sent[0],
            serde_json::json!({"origin": "gate_continuation", "class": "continuation",
                               "count": 1, "work_keys": ["gate:abc"]})
        );
        assert!(lock(&coord.auth)
            .iter()
            .all(|h| h.as_deref() == Some("Bearer tok")));
        assert!(cached_budget().is_some(), "the response budget is cached");
    }

    #[tokio::test]
    async fn an_acquire_coord_does_not_answer_in_time_is_unknown_timeout() {
        let coord = fake_coord(vec![Script::Hang]).await;
        let started = Instant::now();
        let a = acquire_at(
            &reqwest::Client::new(),
            &coord.base,
            "tok",
            SpawnOrigin::UnitContinuation,
            SpawnClass::Continuation,
            1,
            &[],
            Duration::from_millis(300),
        )
        .await;
        assert_eq!(
            a,
            Admission::Unknown {
                reason: UnknownReason::Timeout
            }
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "bounded by the timeout"
        );
    }

    #[tokio::test]
    async fn release_posts_used_to_the_lease_route() {
        let coord = fake_coord(vec![]).await;
        let lease = uuid::Uuid::now_v7();
        release_at(
            &reqwest::Client::new(),
            &coord.base,
            "tok",
            lease,
            1,
            RELEASE_TIMEOUT,
        )
        .await;
        let got = lock(&coord.releases).clone();
        assert_eq!(
            got,
            vec![(lease.to_string(), serde_json::json!({"used": 1}))]
        );
    }

    // --- bucket math ---------------------------------------------------------

    #[test]
    fn the_bucket_starts_full_bursts_then_refills_at_the_rate() {
        let t0 = Instant::now();
        let mut b = TokenBucket::new(4, 4, t0);
        for i in 0..4 {
            assert!(b.try_take(1, t0), "burst token {i}");
        }
        assert!(!b.try_take(1, t0), "the burst is spent");
        // 4/min = one token per 15 s.
        assert!(!b.try_take(1, t0 + Duration::from_secs(14)));
        assert!(b.try_take(1, t0 + Duration::from_secs(15)));
        assert!(!b.try_take(1, t0 + Duration::from_secs(15)));
        // A long idle refills to the burst, never past it.
        assert_eq!(b.available(t0 + Duration::from_secs(3600)), 4);
    }

    #[test]
    fn a_multi_token_take_is_all_or_nothing() {
        let t0 = Instant::now();
        let mut b = TokenBucket::new(4, 0, t0);
        assert!(!b.try_take(5, t0));
        assert_eq!(b.available(t0), 4, "a failed take spends nothing");
        assert!(b.try_take(3, t0));
        assert_eq!(b.available(t0), 1);
    }

    #[test]
    fn a_zero_burst_is_closed_and_a_zero_refill_never_refills() {
        let t0 = Instant::now();
        let mut closed = TokenBucket::new(0, 60, t0);
        assert!(!closed.try_take(1, t0 + Duration::from_secs(600)));
        let mut once = TokenBucket::new(1, 0, t0);
        assert!(once.try_take(1, t0));
        assert!(!once.try_take(1, t0 + Duration::from_secs(86_400)));
    }

    #[test]
    fn bucket_params_take_valid_values_including_zero_and_default_the_rest() {
        let lookup = |v: Option<&'static str>| move |_: &str| v.map(str::to_string);
        assert_eq!(bucket_param(lookup(None), "K", 4), 4);
        assert_eq!(bucket_param(lookup(Some("9")), "K", 4), 9);
        assert_eq!(bucket_param(lookup(Some(" 2 ")), "K", 4), 2);
        assert_eq!(
            bucket_param(lookup(Some("0")), "K", 4),
            0,
            "0 = closed, not default"
        );
        assert_eq!(bucket_param(lookup(Some("-1")), "K", 4), 4);
        assert_eq!(bucket_param(lookup(Some("many")), "K", 4), 4);
    }

    // --- Phase 2: the continuation backlog -----------------------------------

    fn grant() -> Admission {
        Admission::Granted {
            n: 1,
            lease_id: Some(uuid::Uuid::now_v7()),
            reason: None,
            retry_after_secs: None,
            shadow: false,
        }
    }

    fn refusal() -> Admission {
        Admission::Granted {
            n: 0,
            lease_id: None,
            reason: Some("bucket_empty".into()),
            retry_after_secs: Some(10),
            shadow: false,
        }
    }

    /// A 10-row backlog against a coord that grants 3 then refuses: exactly 3
    /// rows take a permit, 7 defer keyed `refused:bucket_empty`, and
    /// the local bucket is never touched — coord's "no" is an answer.
    #[test]
    fn a_backlog_takes_coord_permits_and_defers_when_coord_refuses() {
        let t0 = Instant::now();
        let mut bucket = TokenBucket::new(4, 4, t0);
        let answers = (0..10).map(|i| if i < 3 { grant() } else { refusal() });
        let (mut admitted, mut stamps) = (Vec::new(), Vec::new());
        for a in answers {
            match decide(a, SEEN, &mut bucket, t0) {
                ContinuationAdmission::Admitted(p) => admitted.push(p),
                ContinuationAdmission::Deferred { key, .. } => stamps.push(key),
            }
        }
        assert_eq!(admitted.len(), 3);
        assert!(
            admitted.iter().all(|p| p.lease().is_some()),
            "each holds its coord lease"
        );
        assert_eq!(stamps.len(), 7);
        assert!(stamps.iter().all(|s| s == "refused:bucket_empty"));
        assert_eq!(
            bucket.available(t0),
            4,
            "a coord refusal never spends local tokens"
        );
        // Dropping a permit outside a runtime is the TTL fallback, not a panic.
        drop(admitted);
    }

    /// The same backlog with coord unreachable: the local bucket admits its
    /// burst of 4 and defers the rest, then admits one more per refill period.
    #[test]
    fn a_backlog_with_coord_unknown_drains_at_the_local_bucket_rate() {
        let t0 = Instant::now();
        let mut bucket = TokenBucket::new(DEFAULT_LOCAL_BURST, DEFAULT_LOCAL_REFILL_PER_MIN, t0);
        let unknown = || Admission::Unknown {
            reason: UnknownReason::Timeout,
        };
        let verdicts: Vec<_> = (0..10)
            .map(|_| decide(unknown(), SEEN, &mut bucket, t0))
            .collect();
        let admitted = verdicts
            .iter()
            .filter(|v| matches!(v, ContinuationAdmission::Admitted(_)))
            .count();
        assert_eq!(admitted, 4, "the burst, and no more, in one poll");
        assert!(verdicts.iter().skip(4).all(|v| matches!(
            v,
            ContinuationAdmission::Deferred { key, retry_after, .. }
                if key == "local_bucket_empty" && *retry_after == Some(Duration::from_secs(15))
        )));
        // The next poll 15 s later re-lists the deferred rows: one more runs.
        let later = t0 + Duration::from_secs(15);
        assert!(matches!(
            decide(unknown(), SEEN, &mut bucket, later),
            ContinuationAdmission::Admitted(_)
        ));
        assert!(matches!(
            decide(unknown(), SEEN, &mut bucket, later),
            ContinuationAdmission::Deferred { .. }
        ));
    }

    /// End to end through the PUBLIC path the dispatcher calls: a fake coord
    /// grants two then refuses. `admit_continuation` admits two rows and defers
    /// the third; converting one permit releases its lease with `used = 1`, and
    /// dropping the other (an early exit before the session existed) releases
    /// with `used = 0` — immediately, not at the TTL.
    ///
    /// The only test that touches the process-global door and cooldown, so it
    /// cannot race another test over them.
    #[tokio::test]
    async fn the_dispatch_seam_takes_permits_defers_on_refusal_and_settles_leases() {
        let stray = uuid::Uuid::now_v7();
        let coord = fake_coord(vec![
            Script::Grant,
            Script::Grant,
            Script::Refuse,
            Script::RefuseWithLease(stray),
        ])
        .await;
        *lock(&TEST_TARGET) = Some((coord.base.clone(), "tok".to_string()));
        *lock(door_health()) = DoorHealth::default();

        let mut verdicts = Vec::new();
        for i in 0..4 {
            verdicts.push(
                admit_continuation(SpawnOrigin::GateContinuation, &format!("gate:{i}")).await,
            );
        }
        let mut it = verdicts.into_iter();
        let (
            Some(ContinuationAdmission::Admitted(first)),
            Some(ContinuationAdmission::Admitted(second)),
        ) = (it.next(), it.next())
        else {
            panic!("the two grants admit");
        };
        assert!(matches!(
            it.next(),
            Some(ContinuationAdmission::Deferred { ref key, retry_after, .. })
                if key == "refused:bucket_empty" && retry_after == Some(Duration::from_secs(10))
        ));
        assert!(matches!(
            it.next(),
            Some(ContinuationAdmission::Deferred { .. })
        ));
        let (l1, l2) = (
            first.lease().expect("lease"),
            second.lease().expect("lease"),
        );
        first.converted();
        drop(second);

        // The releases are spawned onto this runtime; give them a bounded wait.
        let deadline = Instant::now() + Duration::from_secs(5);
        while lock(&coord.releases).len() < 3 && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        *lock(&TEST_TARGET) = None;
        let mut got = lock(&coord.releases).clone();
        let rank = |l: &str| {
            if l == l1.to_string() {
                0
            } else if l == l2.to_string() {
                1
            } else {
                2
            }
        };
        got.sort_by_key(|(l, _)| rank(l));
        assert_eq!(
            got,
            vec![
                (l1.to_string(), serde_json::json!({"used": 1})),
                (l2.to_string(), serde_json::json!({"used": 0})),
                (stray.to_string(), serde_json::json!({"used": 0})),
            ],
            "converted -> used 1, dropped -> used 0, a lease on a zero grant -> used 0"
        );
        let sent = lock(&coord.acquires).clone();
        assert_eq!(sent.len(), 4, "one acquire per row, BEFORE any spawn");
        assert_eq!(sent[2]["work_keys"], serde_json::json!(["gate:2"]));
    }

    /// A 404 before any admission route has answered 2xx: coord predates
    /// admission, so the spawn passes through and the local bucket is untouched.
    /// The same 404 after a 2xx is a regression and takes the bucket.
    #[test]
    fn a_404_passes_through_only_until_the_route_has_answered() {
        let t0 = Instant::now();
        let mut bucket = TokenBucket::new(1, 0, t0);
        let fresh = DecideContext {
            route_answered_before: false,
            coord_refill_per_min: None,
        };
        for reason in [
            UnknownReason::RouteAbsent,
            UnknownReason::CoolingDown(Box::new(UnknownReason::RouteAbsent)),
        ] {
            match decide(Admission::Unknown { reason }, fresh, &mut bucket, t0) {
                ContinuationAdmission::Admitted(p) => assert!(p.passed_through_absent_route()),
                other => panic!("a pre-admission coord passes through, got {other:?}"),
            }
        }
        assert_eq!(
            bucket.available(t0),
            1,
            "pass-through spends no local token"
        );

        // Other UNKNOWN causes never pass through, even before any 2xx.
        let five_hundred = decide(
            Admission::Unknown {
                reason: UnknownReason::ServerError(503),
            },
            fresh,
            &mut bucket,
            t0,
        );
        assert!(matches!(
            five_hundred,
            ContinuationAdmission::Admitted(ref p) if !p.passed_through_absent_route()
        ));
        assert_eq!(bucket.available(t0), 0, "a 5xx drew on the bucket");

        // After a 2xx, a 404 is a regression: bucket (now empty) -> deferred.
        assert!(matches!(
            decide(
                Admission::Unknown {
                    reason: UnknownReason::RouteAbsent
                },
                SEEN,
                &mut bucket,
                t0
            ),
            ContinuationAdmission::Deferred { ref key, .. } if key == "local_bucket_empty"
        ));
    }

    #[test]
    fn the_retry_after_is_the_later_of_coords_hint_and_one_refill_period() {
        let s = Duration::from_secs;
        assert_eq!(retry_after_of(Some(30), Some(s(10))), Some(s(30)));
        assert_eq!(retry_after_of(Some(2), Some(s(10))), Some(s(10)));
        assert_eq!(retry_after_of(None, Some(s(15))), Some(s(15)));
        assert_eq!(retry_after_of(Some(7), None), Some(s(7)));
        assert_eq!(retry_after_of(None, None), None);
        assert_eq!(
            TokenBucket::new(4, 4, Instant::now()).refill_period(),
            Some(s(15))
        );
        assert_eq!(TokenBucket::new(4, 0, Instant::now()).refill_period(), None);
        // A coord refusal is timed off coord's refill when no hint is given.
        let mut bucket = TokenBucket::new(4, 4, Instant::now());
        let ctx = DecideContext {
            route_answered_before: true,
            coord_refill_per_min: Some(6),
        };
        let refusal = Admission::Granted {
            n: 0,
            lease_id: None,
            reason: None,
            retry_after_secs: None,
            shadow: false,
        };
        assert!(matches!(
            decide(refusal, ctx, &mut bucket, Instant::now()),
            ContinuationAdmission::Deferred { retry_after: Some(d), .. } if d == s(10)
        ));
    }

    #[test]
    fn a_not_required_permit_releases_nothing() {
        let p = AdmissionPermit::not_required();
        assert_eq!(p.lease(), None);
        p.converted();
    }
}
