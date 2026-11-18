//! This device's row of coord's free-resource model, read with the runner's
//! own credential and kept for a minute (plan
//! `2026-11-05-one-free-resource-model-places-sessions-builds-and-ci`, Phase 6,
//! "Runner loopback"; Phase 7's CI-node sizing reads it through the same two
//! calls).
//!
//! # Why this exists
//!
//! coord serves each device's free CPU and memory, its class costs and its
//! slots per work class at `GET /coord/fleet/capacity[?device_id=]` (the
//! plan's Phase 4: `qontinui-coord` `crates/coord/src/fleet_placement/
//! capacity_read.rs`). That door wants a device or agent JWT. The local tools
//! that size a build (qontinui-claude-config's `build-jobs.sh` and
//! `cargo-guard.sh`) hold no coord credential. So the runner reads THIS
//! device's row, which its loopback HTTP API can serve to them as
//! `GET /fleet/capacity`, and an in-process caller reads it through
//! [`current`] (fetching when the kept read is stale) or [`cached`] (never
//! fetching).
//!
//! # What a read is
//!
//! A [`CapacityRead`], whose `state` is one of:
//!
//! * `known` — coord served this device's row. `row` is that row exactly as
//!   coord served it (`devices[0]` of coord's answer: the identity fields, the
//!   capacity row with its `slots`, `costs`, `c3_cpu_cores` and
//!   `class_free_mem`, and the `parity` readout).
//! * `unknown` — there is no row, and `reason` says why: this runner is not
//!   connected to coord or has no device or no usable device JWT, coord did
//!   not answer, coord has written no model-v3 plan for this tenant yet
//!   (`no_plan_yet`), or coord refused (`device_not_in_plan` for a device the
//!   tenant's newest plan does not carry).
//!
//! Beside the row: `coord`, the rest of coord's answer (`model_version`,
//! `plan`, `thresholds`, `cadence_secs`, `coverage`, `note`, coord's own
//! `cache_age_secs`), whenever coord answered 2xx. The D9 dial is NOT part of
//! a read: coord serves it on `GET /coord/fleet-policy`, which the runner's
//! fleet-policy poller caches (`mcp::fleet_policy_poller::capacity_model_dial`),
//! and whatever serves it beside a read serves it with its age
//! (`mcp::fleet_policy_poller::capacity_model_view`).
//!
//! An `unknown` carries no capacity row and no capacity number (a `no_plan_yet`
//! read still carries coord's own answer beside it), so nothing can read it as
//! zero free capacity [policy: unknown-must-not-render-as-a-default]; a consumer
//! falls back to its own static sizing and says so. Inside `row`, coord's own
//! convention still holds: `{"state":"known","value":…}` is known and
//! `{"state":"unknown","reason":…}` is UNKNOWN (coord's
//! `fleet_placement::Known`).
//!
//! # The cache
//!
//! One read is kept per process: an answer from coord (a row, or `no_plan_yet`)
//! for [`CACHE_TTL`] (60 s); any other `unknown` for [`UNKNOWN_TTL`] (10 s), so
//! a coord outage costs one fetch per 10 s rather than one per caller. A TTL
//! runs from when the fetch FINISHED, so a slow failure (a timeout) is still
//! kept its full 10 s; the read's age runs from when it began (below). A fetch
//! is single-flight: a caller that finds the kept read stale fetches, and the
//! callers that arrive while it does wait for that fetch rather than start
//! their own. coord caches its own answer for up to 15 s per tenant, and the
//! row's `sample_age_s` says how old the sample behind it was.
//!
//! A read is stamped with the moment its fetch began (`fetched_at`: when the
//! runner asked coord), and every read is aged at the moment it is handed
//! back, so `cache_age_secs` holds the fetch's own time and any wait for a
//! fetch in flight: it never reads younger than the runner's ask. It does not
//! include coord's own cache: coord may answer from a read up to 15 s old,
//! and says how old in `coord.cache_age_secs`.
//!
//! A fetch has two stages ([`Prepared`]). The synchronous one resolves the
//! coord base and the device from small file reads, and either settles the
//! read there (no base, no usable device) or hands over the device and the
//! asynchronous rest. That rest (the credential read, which runs on the
//! blocking pool, and the HTTP round trip) runs under one deadline,
//! [`FETCH_DEADLINE`], so a stalled credential store cannot hold every caller
//! behind the single-flight lock: past it the read is `unknown`, kept for
//! [`UNKNOWN_TTL`], and still names the device it was asking about. The
//! synchronous reads sit outside the deadline, which could not interrupt
//! them anyway.
//!
//! [`current`] runs the read on its own task ([`get_detached`]), so a caller
//! that is dropped while a fetch is in flight (a loopback client that gave up
//! before coord answered) does not drop the fetch: it finishes and is kept,
//! and the callers after it are served it.
//!
//! # The credential
//!
//! The default binding's device JWT, through
//! [`crate::coord_mcp::read_usable_device_jwt`]: the same credential
//! `fleet::resource_sample` publishes this device's samples with, so the read
//! lands in the tenant those samples (the model's input) are stored under.
//! With no usable JWT the read is `unknown` and no request is sent; an
//! anonymous read would only be refused.

use std::future::Future;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

/// How long an answer from coord (a row, or `no_plan_yet`) is kept before
/// coord is asked again.
pub(crate) const CACHE_TTL: Duration = Duration::from_secs(60);

/// How long any other `unknown` is kept before the fetch is tried again.
pub(crate) const UNKNOWN_TTL: Duration = Duration::from_secs(10);

/// The HTTP request's own deadline. On a cache miss coord recomputes the tenant's
/// answer (the newest plan row, the newest runner sample per device), which a
/// loaded coord database can take seconds to serve.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// The fetch's deadline over everything in it that awaits: the credential read
/// and the request itself ([`FETCH_TIMEOUT`]), with a margin.
const FETCH_DEADLINE: Duration = Duration::from_secs(15);

/// The most bytes of coord's answer accepted. One device's row with its parity
/// readout is a few KiB. Enforced as the body arrives ([`read_body`]): a
/// declared length over it is refused before any of the body is read, and an
/// undeclared one is read no further than the bound.
const MAX_BODY_BYTES: usize = 1024 * 1024;

/// What one fetch of coord's capacity door produced.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Fetched {
    /// coord served this device's row (`state: "known"`).
    Known {
        device_id: Uuid,
        /// The row, as coord served it.
        row: Value,
        /// coord's answer without its `devices` list.
        coord: Value,
    },
    /// coord answered `state: "no_plan_yet"`: no model-v3 plan for this
    /// device's tenant yet. Read as `unknown`, but kept as long as a row.
    NoPlanYet {
        device_id: Uuid,
        coord: Value,
        reason: String,
    },
    /// No usable answer, and why.
    Unknown {
        device_id: Option<Uuid>,
        reason: String,
    },
}

impl Fetched {
    fn unknown(device_id: Option<Uuid>, reason: impl Into<String>) -> Self {
        Fetched::Unknown {
            device_id,
            reason: reason.into(),
        }
    }

    /// Its state and reason, which a change of is logged.
    fn outcome(&self) -> (&'static str, Option<&str>) {
        match self {
            Fetched::Known { .. } => ("known", None),
            Fetched::NoPlanYet { reason, .. } | Fetched::Unknown { reason, .. } => {
                ("unknown", Some(reason))
            }
        }
    }

    /// How long this answer is kept.
    fn ttl(&self) -> Duration {
        match self {
            Fetched::Unknown { .. } => UNKNOWN_TTL,
            Fetched::Known { .. } | Fetched::NoPlanYet { .. } => CACHE_TTL,
        }
    }
}

/// The synchronous stage of a fetch: either the read is already settled
/// (no coord base, no usable device), or the device is known and `fut` is the
/// asynchronous rest. Only `fut` runs under the fetch's deadline, so the
/// deadline's `unknown` still names `device_id`.
pub(crate) enum Prepared<Fut> {
    /// Settled without awaiting anything.
    Done(Fetched),
    /// The device is resolved; `fut` reads the credential and asks coord.
    Fetch { device_id: Uuid, fut: Fut },
}

/// One read of this device's capacity: served as-is on the loopback, and
/// returned to an in-process caller.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct CapacityRead {
    /// `known` or `unknown` (module docs).
    pub(crate) state: &'static str,
    /// The device asked about, once it was resolved.
    pub(crate) device_id: Option<String>,
    /// This device's row exactly as coord served it; only when `known`.
    pub(crate) row: Option<Value>,
    /// coord's answer without its `devices` list, whenever coord answered 2xx.
    pub(crate) coord: Option<Value>,
    /// Why there is no row; only when `unknown`.
    pub(crate) reason: Option<String>,
    /// When the runner asked coord, RFC 3339 UTC.
    pub(crate) fetched_at: String,
    /// Seconds since the runner asked coord.
    pub(crate) cache_age_secs: f64,
    /// How long this read is kept before coord is asked again; `0` when this
    /// read is not kept at all (the read's task failed, so nothing was
    /// stored and the next caller asks again). A kept read's TTL is never 0.
    pub(crate) cache_ttl_secs: u64,
}

impl CapacityRead {
    /// The row, when this read is `known`.
    pub(crate) fn known_row(&self) -> Option<&Value> {
        self.row.as_ref().filter(|_| self.state == "known")
    }
}

/// coord's answer to `GET /coord/fleet/capacity?device_id=<device_id>`, as a
/// [`Fetched`]. PURE.
///
/// A 2xx must be a JSON object whose `state` is `known` with exactly one row,
/// this device's, or `no_plan_yet`; anything else is `unknown`, never a row of
/// some other device and never an empty one. A refusal (coord's placement read
/// errors are `{"error": <code>, "detail": <text>}`) is `unknown` naming the
/// status and coord's code.
pub(crate) fn parse_answer(device_id: Uuid, status: u16, body: &[u8]) -> Fetched {
    let unknown = |reason: String| Fetched::unknown(Some(device_id), reason);
    if body.len() > MAX_BODY_BYTES {
        return unknown(format!(
            "coord's answer is {} bytes, over the {MAX_BODY_BYTES}-byte bound",
            body.len()
        ));
    }
    let json: Option<Value> = serde_json::from_slice(body).ok();
    if !(200..300).contains(&status) {
        let field = |key: &str| {
            json.as_ref()
                .and_then(|j| j.get(key))
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        let code = field("error").unwrap_or_else(|| "no error code".to_string());
        return unknown(match field("detail") {
            Some(detail) => format!("coord answered HTTP {status} {code}: {detail}"),
            None => format!("coord answered HTTP {status} {code}"),
        });
    }
    let Some(Value::Object(mut answer)) = json else {
        return unknown(format!(
            "coord answered HTTP {status} with a body that is not a JSON object"
        ));
    };
    let state = answer
        .get("state")
        .and_then(Value::as_str)
        .map(str::to_string);
    match state.as_deref() {
        Some("known") => {
            let row = match answer.remove("devices") {
                Some(Value::Array(rows)) => match <[Value; 1]>::try_from(rows) {
                    Ok([row]) if row_device(&row) == Some(device_id) => row,
                    _ => {
                        return unknown(
                            "coord answered state known without exactly this device's row"
                                .to_string(),
                        )
                    }
                },
                _ => return unknown("coord answered state known with no devices list".to_string()),
            };
            Fetched::Known {
                device_id,
                row,
                coord: Value::Object(answer),
            }
        }
        Some("no_plan_yet") => {
            answer.remove("devices");
            let note = answer
                .get("note")
                .and_then(Value::as_str)
                .unwrap_or("coord has written no model-v3 placement plan for this tenant yet");
            let reason = format!("no_plan_yet: {note}");
            Fetched::NoPlanYet {
                device_id,
                coord: Value::Object(answer),
                reason,
            }
        }
        Some(other) => unknown(format!("coord answered an unrecognised state {other:?}")),
        None => unknown("coord's answer carries no state".to_string()),
    }
}

/// The `device_id` a served row names.
fn row_device(served: &Value) -> Option<Uuid> {
    served
        .pointer("/device_id")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
}

/// One kept read.
#[derive(Debug)]
struct Entry {
    /// When its fetch began: the read's age runs from here.
    at: Instant,
    /// When its fetch finished: its TTL runs from here, so a slow fetch is
    /// kept as long as a quick one.
    done: Instant,
    fetched_at: DateTime<Utc>,
    fetched: Fetched,
}

impl Entry {
    fn fresh_at(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.done) < self.fetched.ttl()
    }

    /// This entry as a kept read: its TTL is the one it is kept for.
    fn read_at(&self, now: Instant) -> CapacityRead {
        self.read_kept_for(now, self.fetched.ttl().as_secs())
    }

    /// This entry as a read that is never kept: `cache_ttl_secs` is 0.
    fn unkept_read_at(&self, now: Instant) -> CapacityRead {
        self.read_kept_for(now, 0)
    }

    fn read_kept_for(&self, now: Instant, ttl_secs: u64) -> CapacityRead {
        let (state, device_id, row, coord, reason) = match &self.fetched {
            Fetched::Known {
                device_id,
                row,
                coord,
            } => (
                "known",
                Some(*device_id),
                Some(row.clone()),
                Some(coord.clone()),
                None,
            ),
            Fetched::NoPlanYet {
                device_id,
                coord,
                reason,
            } => (
                "unknown",
                Some(*device_id),
                None,
                Some(coord.clone()),
                Some(reason.clone()),
            ),
            Fetched::Unknown { device_id, reason } => {
                ("unknown", *device_id, None, None, Some(reason.clone()))
            }
        };
        CapacityRead {
            state,
            device_id: device_id.map(|d| d.to_string()),
            row,
            coord,
            reason,
            fetched_at: self.fetched_at.to_rfc3339_opts(SecondsFormat::Millis, true),
            cache_age_secs: now.saturating_duration_since(self.at).as_secs_f64(),
            cache_ttl_secs: ttl_secs,
        }
    }
}

/// The kept read, and the single-flight fetch lock.
pub(crate) struct CapacityCache {
    /// Never held across an await, so [`cached`] never waits on a fetch.
    last: Mutex<Option<Entry>>,
    /// Held across a fetch: callers that find the kept read stale while one
    /// is in flight wait here, then take what it kept.
    fetching: tokio::sync::Mutex<()>,
}

impl CapacityCache {
    pub(crate) fn new() -> Self {
        Self {
            last: Mutex::new(None),
            fetching: tokio::sync::Mutex::new(()),
        }
    }

    fn last_guard(&self) -> MutexGuard<'_, Option<Entry>> {
        self.last.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The kept read when it is fresh at `now`.
    fn fresh(&self, now: Instant) -> Option<CapacityRead> {
        self.last_guard()
            .as_ref()
            .filter(|e| e.fresh_at(now))
            .map(|e| e.read_at(now))
    }

    /// The kept read when it is fresh; otherwise a new one, kept and
    /// returned: `prepare` runs once, and a [`Prepared::Done`] is the read
    /// while a [`Prepared::Fetch`]'s future is awaited for it.
    /// `clock` tells the instant and wall time whenever it is asked: a read
    /// this call fetches is stamped with the moment its fetch began, and every
    /// read it returns, kept or fetched, is aged at the moment it is returned
    /// — so the fetch's own time, and any wait for a fetch in flight, are in
    /// the age, which never reads younger than the runner's ask. A fetch whose
    /// future has not answered within [`FETCH_DEADLINE`] is `unknown`, naming
    /// the device it was preparing to ask about.
    pub(crate) async fn get<Fut>(
        &self,
        clock: impl Fn() -> (Instant, DateTime<Utc>),
        prepare: impl FnOnce() -> Prepared<Fut>,
    ) -> CapacityRead
    where
        Fut: Future<Output = Fetched>,
    {
        self.get_within(FETCH_DEADLINE, clock, prepare).await
    }

    /// [`CapacityCache::get`] with the fetch's deadline given.
    async fn get_within<Fut>(
        &self,
        deadline: Duration,
        clock: impl Fn() -> (Instant, DateTime<Utc>),
        prepare: impl FnOnce() -> Prepared<Fut>,
    ) -> CapacityRead
    where
        Fut: Future<Output = Fetched>,
    {
        if let Some(read) = self.fresh(clock().0) {
            return read;
        }
        let _flight = self.fetching.lock().await;
        // A fetch that finished while this read waited is this read's answer,
        // aged now rather than when this read began to wait.
        if let Some(read) = self.fresh(clock().0) {
            return read;
        }
        let (at, fetched_at) = clock();
        let fetched = match prepare() {
            Prepared::Done(fetched) => fetched,
            Prepared::Fetch { device_id, fut } => tokio::time::timeout(deadline, fut)
                .await
                .unwrap_or_else(|_| {
                    Fetched::unknown(
                        Some(device_id),
                        format!(
                            "the capacity fetch did not finish within {} s (the credential \
                             read or coord stalled)",
                            deadline.as_secs_f64()
                        ),
                    )
                }),
        };
        let done = clock().0;
        let entry = Entry {
            at,
            done,
            fetched_at,
            fetched,
        };
        let read = entry.read_at(done);
        let mut last = self.last_guard();
        let changed = last.as_ref().map(|e| e.fetched.outcome()) != Some(entry.fetched.outcome());
        if changed {
            // The bearer never reaches a reason, so a reason is safe to log.
            tracing::info!(
                state = read.state,
                reason = read.reason.as_deref().unwrap_or(""),
                "fleet capacity: this device's read changed"
            );
        }
        *last = Some(entry);
        read
    }

    /// The last read, fresh or not, aged at `now`; `None` before any.
    pub(crate) fn last(&self, now: Instant) -> Option<CapacityRead> {
        self.last_guard().as_ref().map(|e| e.read_at(now))
    }
}

fn cache() -> &'static CapacityCache {
    static CACHE: OnceLock<CapacityCache> = OnceLock::new();
    CACHE.get_or_init(CapacityCache::new)
}

/// This device's capacity: the kept read while it is fresh, otherwise a new
/// read of coord's capacity door.
pub(crate) async fn current() -> CapacityRead {
    get_detached(cache(), prepare_live).await
}

/// `cache.get(prepare)` on a task of its own, so dropping the caller does not
/// drop a fetch in flight. A task that panicked is an `unknown` read that is
/// not kept (`cache_ttl_secs: 0`): nothing was stored, so the next caller asks
/// again.
async fn get_detached<P, Fut>(cache: &'static CapacityCache, prepare: P) -> CapacityRead
where
    P: FnOnce() -> Prepared<Fut> + Send + 'static,
    Fut: Future<Output = Fetched> + Send + 'static,
{
    match tokio::spawn(cache.get(|| (Instant::now(), Utc::now()), prepare)).await {
        Ok(read) => read,
        Err(e) => Entry {
            at: Instant::now(),
            done: Instant::now(),
            fetched_at: Utc::now(),
            fetched: Fetched::unknown(None, format!("the capacity read's task failed: {e}")),
        }
        .unkept_read_at(Instant::now()),
    }
}

/// The last read, synchronously and without asking coord: `None` before the
/// first read. It may be older than [`CACHE_TTL`]; `cache_age_secs` says how
/// old.
pub(crate) fn cached() -> Option<CapacityRead> {
    cache().last(Instant::now())
}

/// The synchronous stage of one live read of coord's capacity door for this
/// device: the coord base and the device file, then the asynchronous rest
/// ([`fetch_live`]) to run under the deadline.
///
/// The coord base is resolved first. A test that holds no isolated ambient is
/// deflected to "not connected" there (`ambient::canary`); one that holds
/// `isolated_ambient` resolves against its own empty fixture, which configures
/// no coord base. Either way a test read never reaches the operator's device
/// file or credential store.
fn prepare_live() -> Prepared<impl Future<Output = Fetched> + Send + 'static> {
    let Some(base) = qontinui_runner_lib::profiles::connected_coord_base() else {
        return Prepared::Done(Fetched::unknown(
            None,
            "no coord base is configured: this runner is not connected to coord",
        ));
    };
    // The file the sample publisher reads: a device with no samples has no row.
    let device_id = match device_from(qontinui_runner_lib::ambient::try_read_machine_json()) {
        Ok(device_id) => device_id,
        Err(reason) => return Prepared::Done(Fetched::unknown(None, reason)),
    };
    Prepared::Fetch {
        device_id,
        fut: fetch_live(base, device_id),
    }
}

/// The device a read of `machine.json` names, or why it names none. PURE.
///
/// The requirements are the sample publisher's (`fleet::load_device_file`):
/// a `device_id` AND a `hostname`, since a device the publisher skips has no
/// samples and so no row in coord's model. Each way the file can fail them
/// has its own reason, naming the path whenever one resolved.
fn device_from(
    read: Result<
        qontinui_runner_lib::ambient::MachineJson,
        qontinui_runner_lib::ambient::MachineJsonError,
    >,
) -> Result<Uuid, String> {
    use qontinui_runner_lib::ambient::MachineJsonError;
    const FILE: &str = "~/.qontinui/machine.json";
    let machine = match read {
        Ok(machine) => machine,
        Err(e) => {
            let path = e
                .path()
                .map_or_else(|| FILE.to_string(), |p| p.display().to_string());
            return Err(match e {
                MachineJsonError::NoHomeDir => format!(
                    "no home directory resolved (no QONTINUI_HOME, no home), so there is no \
                     {FILE} to name this runner's device"
                ),
                ref missing if missing.is_missing() => {
                    format!("{path} does not exist, so this runner has no device to ask about")
                }
                MachineJsonError::Read { source, .. } => {
                    format!("{path} could not be read ({source}), so this runner has no device")
                }
                MachineJsonError::Parse { source, .. } => format!(
                    "{path} is not a valid machine.json ({source}), so this runner has no device"
                ),
            });
        }
    };
    let Some(device_id) = machine.device_id.as_deref() else {
        return Err(format!(
            "{FILE} names no device_id, so this runner has no device to ask about"
        ));
    };
    if machine.hostname.is_none() {
        return Err(format!(
            "{FILE} names no hostname: the sample publisher skips a device with no hostname, \
             so coord holds no row for it"
        ));
    }
    Uuid::parse_str(device_id.trim())
        .map_err(|_| format!("{FILE}'s device_id {device_id:?} is not a UUID"))
}

/// The asynchronous stage of a live read, once [`prepare_live`] resolved the
/// base and the device: the credential read, then [`fetch_from`].
async fn fetch_live(base: String, device_id: Uuid) -> Fetched {
    let Some(bearer) = crate::coord_mcp::read_usable_device_jwt().await else {
        return Fetched::unknown(
            Some(device_id),
            "no usable device JWT (the runner is unpaired, or its token expired and has not \
             been re-minted yet)",
        );
    };
    fetch_from(&base, device_id, &bearer).await
}

/// One read of the capacity door of the coord at `base` for `device_id`,
/// presenting `bearer`: the HTTP half of [`fetch_live`], once the device and
/// the credential are resolved.
async fn fetch_from(base: &str, device_id: Uuid, bearer: &str) -> Fetched {
    let client = match qontinui_runner_lib::outbound_net::os_dns::client_builder()
        .timeout(FETCH_TIMEOUT)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return Fetched::unknown(Some(device_id), format!("http client build failed: {e}"))
        }
    };
    let url = format!(
        "{}/coord/fleet/capacity?device_id={device_id}",
        base.trim_end_matches('/')
    );
    let resp = match client.get(&url).bearer_auth(bearer).send().await {
        Ok(r) => r,
        Err(e) => {
            return Fetched::unknown(Some(device_id), format!("coord unreachable: {e}"));
        }
    };
    let status = resp.status().as_u16();
    match read_body(resp, MAX_BODY_BYTES).await {
        Ok(body) => parse_answer(device_id, status, &body),
        Err(why) => Fetched::unknown(
            Some(device_id),
            format!("coord answered HTTP {status} {why}"),
        ),
    }
}

/// `resp`'s body, read no further than `limit` bytes: refused unread when its
/// declared length is over `limit`, and refused once what has arrived passes
/// `limit` when it declares none. The error completes "coord answered HTTP
/// <status> …".
async fn read_body(mut resp: reqwest::Response, limit: usize) -> Result<Vec<u8>, String> {
    let over = |n: String| format!("with a body of {n} bytes, over the {limit}-byte bound");
    if let Some(declared) = resp.content_length().filter(|n| *n > limit as u64) {
        return Err(over(declared.to_string()));
    }
    let mut body = Vec::new();
    loop {
        match resp.chunk().await {
            Ok(Some(chunk)) => {
                if body.len().saturating_add(chunk.len()) > limit {
                    return Err(over(format!("more than {limit}")));
                }
                body.extend_from_slice(&chunk);
            }
            Ok(None) => return Ok(body),
            Err(e) => return Err(format!("but its body could not be read: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn device() -> Uuid {
        Uuid::from_u128(0x0bcf_4751_acd2_4000_8000_0000_0000_0001)
    }

    /// One device's row in the shape coord's capacity read serves it
    /// (`capacity_read.rs` `DeviceCapacity`: the identity fields, the
    /// flattened `CapacityRow`, the parity readout), `Known` values tagged.
    fn coord_row(id: Uuid) -> Value {
        json!({
            "device_id": id.to_string(),
            "machine_key": format!("{id}/"),
            "label": "nawa",
            "agent_lane": {"state": "open"},
            "latest_row_source": "runner",
            "sample_age_s": 12.5,
            "free_cpu_cores": {"state": "known", "value": 87.25},
            "free_mem_bytes": {"state": "known", "value": 150_323_855_360_i64},
            "class_free_mem": {
                "S": {"state": "known", "value": 150_323_855_360_i64},
                "C3": {"state": "unknown", "reason": "agent-builds.slice not published"},
                "C1": {"state": "known", "value": 20_000_000_000_i64},
                "C2": {"state": "known", "value": 20_000_000_000_i64},
                "B": {"state": "known", "value": 150_323_855_360_i64}
            },
            "c3_cpu_cores": {"state": "known", "value": 40.0},
            "binding": {"S": "memory", "C3": null, "C1": "cpu", "C2": "cpu", "B": null},
            "disk": "out_of_scope",
            "slots": {
                "S": {"state": "known", "value": 20},
                "C3": {"state": "known", "value": 10},
                "C1": {"state": "known", "value": 2},
                "C2": {"state": "known", "value": 2},
                "B": {"state": "known", "value": 9}
            },
            "unknown_reason": [],
            "parity": [{"row": 11, "decider": "local build parallelism", "answer": 8}]
        })
    }

    fn known_body(id: Uuid) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "state": "known",
            "model_version": 3,
            "plan": {"id": 13486, "computed_at": "2026-11-13T02:00:00Z", "age_secs": 40.0},
            "cadence_secs": {"costs": 300, "cache": 15},
            "thresholds": {"sample_stale_secs": 120},
            "note": "free capacity is recomputed on read",
            "coverage": {"capacity": null},
            "devices": [coord_row(id)],
            "cache_age_secs": 3.0
        }))
        .unwrap()
    }

    fn known() -> Fetched {
        parse_answer(device(), 200, &known_body(device()))
    }

    fn entry_at(at: Instant, fetched: Fetched) -> Entry {
        Entry {
            at,
            done: at,
            fetched_at: Utc::now(),
            fetched,
        }
    }

    /// A clock that stands at `t` (and at this moment's wall time).
    fn at(t: Instant) -> impl Fn() -> (Instant, DateTime<Utc>) + Clone + Send + 'static {
        let wall = Utc::now();
        move || (t, wall)
    }

    /// The row coord served comes back exactly as coord served it, and the
    /// rest of coord's answer comes back beside it without the device list.
    #[test]
    fn the_row_passes_through_exactly_as_coord_served_it() {
        let fetched = known();
        let Fetched::Known {
            device_id,
            row,
            coord,
        } = &fetched
        else {
            panic!("a known answer with this device's row parses as known: {fetched:?}");
        };
        assert_eq!(*device_id, device());
        assert_eq!(*row, coord_row(device()), "the row is coord's, unchanged");
        assert_eq!(coord["model_version"], 3);
        assert_eq!(coord["plan"]["id"], 13486);
        assert_eq!(coord["cache_age_secs"], 3.0);
        assert!(
            coord.get("devices").is_none(),
            "the row is not served twice"
        );

        let read = entry_at(Instant::now(), fetched).read_at(Instant::now());
        assert_eq!(read.state, "known");
        assert_eq!(read.known_row(), Some(&coord_row(device())));
        assert_eq!(read.coord.as_ref().unwrap()["model_version"], 3);
    }

    /// A 2xx that does not carry exactly this device's row is UNKNOWN, never
    /// another device's row and never an empty one.
    #[test]
    fn an_answer_without_exactly_this_devices_row_is_unknown() {
        let other = Uuid::from_u128(7);
        let cases: [(&str, Value); 4] = [
            (
                "another device",
                json!({"state": "known", "devices": [coord_row(other)]}),
            ),
            (
                "two rows",
                json!({"state": "known", "devices": [coord_row(device()), coord_row(device())]}),
            ),
            ("no row", json!({"state": "known", "devices": []})),
            ("no list", json!({"state": "known", "devices": null})),
        ];
        for (what, body) in cases {
            let fetched = parse_answer(device(), 200, &serde_json::to_vec(&body).unwrap());
            assert!(
                matches!(&fetched, Fetched::Unknown { device_id: Some(d), .. } if *d == device()),
                "{what}: {fetched:?}"
            );
        }
    }

    /// No plan yet is UNKNOWN with coord's note, no row, coord's answer kept,
    /// and held as long as a row (coord did answer).
    #[test]
    fn no_plan_yet_is_unknown_with_coords_note_and_no_row() {
        let body = json!({"state": "no_plan_yet", "model_version": 3,
                          "note": "the 300 s worker writes one per tick", "devices": null});
        let fetched = parse_answer(device(), 200, &serde_json::to_vec(&body).unwrap());
        assert!(matches!(fetched, Fetched::NoPlanYet { .. }), "{fetched:?}");
        let read = entry_at(Instant::now(), fetched).read_at(Instant::now());
        assert_eq!(read.state, "unknown");
        assert_eq!(read.row, None);
        assert_eq!(read.known_row(), None);
        assert_eq!(
            read.reason.as_deref(),
            Some("no_plan_yet: the 300 s worker writes one per tick")
        );
        assert!(read.coord.as_ref().unwrap().get("devices").is_none());
        assert_eq!(read.cache_ttl_secs, 60);
    }

    /// coord's typed refusals keep their status and code in the reason.
    #[test]
    fn a_refusal_is_unknown_naming_coords_status_and_code() {
        let refusal = json!({"error": "device_not_in_plan",
                             "detail": "device is not a workstation device of your tenant's newest plan"});
        let Fetched::Unknown { reason, .. } =
            parse_answer(device(), 404, &serde_json::to_vec(&refusal).unwrap())
        else {
            panic!("a 404 is unknown");
        };
        assert!(reason.contains("HTTP 404 device_not_in_plan"), "{reason}");
        assert!(reason.contains("not a workstation device"), "{reason}");

        let Fetched::Unknown { reason, .. } = parse_answer(device(), 502, b"<html>bad gateway")
        else {
            panic!("a 502 is unknown");
        };
        assert!(reason.contains("HTTP 502 no error code"), "{reason}");

        let Fetched::Unknown { reason, .. } = parse_answer(device(), 200, b"not json") else {
            panic!("a body that is not JSON is unknown");
        };
        assert!(reason.contains("not a JSON object"), "{reason}");

        let Fetched::Unknown { reason, .. } = parse_answer(
            device(),
            200,
            &serde_json::to_vec(&json!({"state": "shadow"})).unwrap(),
        ) else {
            panic!("an unrecognised state is unknown");
        };
        assert!(reason.contains("unrecognised state"), "{reason}");
    }

    /// A preparation that resolved [`device`] and hands over `fut`.
    fn prepared<Fut>(fut: Fut) -> Prepared<Fut> {
        Prepared::Fetch {
            device_id: device(),
            fut,
        }
    }

    /// A fetch that counts its calls and answers `answer`.
    fn counting(calls: &AtomicUsize, answer: Fetched) -> impl Future<Output = Fetched> + '_ {
        calls.fetch_add(1, Ordering::SeqCst);
        std::future::ready(answer)
    }

    /// Within the minute the kept read is returned and coord is not asked;
    /// its age is the time since the fetch.
    #[tokio::test]
    async fn a_read_within_the_ttl_is_served_from_the_cache() {
        let cache = CapacityCache::new();
        let calls = AtomicUsize::new(0);
        let t0 = Instant::now();
        let first = cache
            .get(at(t0), || prepared(counting(&calls, known())))
            .await;
        assert_eq!(first.state, "known");
        assert_eq!(first.cache_age_secs, 0.0);
        let later = t0 + Duration::from_secs(59);
        let second = cache
            .get(at(later), || {
                prepared(counting(&calls, Fetched::unknown(None, "must not run")))
            })
            .await;
        assert_eq!(calls.load(Ordering::SeqCst), 1, "coord was asked once");
        assert_eq!(second.state, "known");
        assert_eq!(second.row, first.row);
        assert_eq!(second.fetched_at, first.fetched_at);
        assert_eq!(second.cache_age_secs, 59.0);
        assert_eq!(second.cache_ttl_secs, 60);
    }

    /// A fetch whose future never answers (a stalled credential store) is cut
    /// off at the deadline: the read is UNKNOWN naming it, with no row, kept
    /// for the short TTL so the callers behind the single-flight lock are
    /// freed — and it still names the device the preparation resolved, since
    /// that was known before the deadline began.
    #[tokio::test(start_paused = true)]
    async fn a_fetch_past_its_deadline_is_unknown() {
        let cache = CapacityCache::new();
        let t0 = Instant::now();
        let read = cache
            .get_within(Duration::from_secs(15), at(t0), || Prepared::Fetch {
                device_id: Uuid::parse_str("0bcf4751-acd2-4000-8000-000000000001").unwrap(),
                fut: std::future::pending::<Fetched>(),
            })
            .await;
        assert_eq!(read.state, "unknown");
        assert!(read.row.is_none());
        assert_eq!(
            read.device_id.as_deref(),
            Some("0bcf4751-acd2-4000-8000-000000000001"),
            "the timed-out read keeps the device it was asking about"
        );
        let reason = read.reason.as_deref().unwrap_or_default();
        assert!(reason.contains("did not finish within 15 s"), "{reason}");
        assert!(!reason.contains("device file"), "{reason}");
        assert_eq!(read.cache_ttl_secs, UNKNOWN_TTL.as_secs());
    }

    /// A future that counts how often it is polled, and never answers.
    struct CountingPending(Arc<AtomicUsize>);

    impl Future for CountingPending {
        type Output = Fetched;
        fn poll(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Fetched> {
            self.0.fetch_add(1, Ordering::SeqCst);
            std::task::Poll::Pending
        }
    }

    /// A settled preparation is the read: no future is polled and no deadline
    /// runs, even a zero one, so its reason is its own and not the timeout's.
    #[tokio::test(start_paused = true)]
    async fn a_done_preparation_never_polls_a_future() {
        let cache = CapacityCache::new();
        let polls = Arc::new(AtomicUsize::new(0));
        let never = CountingPending(polls.clone());
        let read = cache
            .get_within(Duration::ZERO, at(Instant::now()), move || {
                // The future exists, but a `Done` drops it unpolled.
                drop(never);
                Prepared::<CountingPending>::Done(Fetched::unknown(None, "no coord base"))
            })
            .await;
        assert_eq!(polls.load(Ordering::SeqCst), 0, "no future was polled");
        assert_eq!(read.state, "unknown");
        assert_eq!(read.reason.as_deref(), Some("no coord base"));
        assert_eq!(read.device_id, None);
    }

    /// A slow failure is kept its full TTL from when it FINISHED: a fetch that
    /// took longer than the 10 s UNKNOWN TTL (a timeout) is not stale on
    /// arrival, so the next caller is served it rather than fetching again.
    #[tokio::test]
    async fn a_slow_failure_is_kept_its_ttl_from_when_it_finished() {
        let t0 = Instant::now();
        let now = Arc::new(std::sync::Mutex::new(t0));
        let clock = {
            let now = now.clone();
            move || (*now.lock().unwrap(), Utc::now())
        };
        let cache = CapacityCache::new();
        let calls = AtomicUsize::new(0);
        let slow = cache
            .get(clock.clone(), || {
                calls.fetch_add(1, Ordering::SeqCst);
                // The fetch took 15 s, past the 10 s TTL of what it returns.
                *now.lock().unwrap() += Duration::from_secs(15);
                prepared(std::future::ready(Fetched::unknown(None, "timed out")))
            })
            .await;
        assert_eq!(slow.state, "unknown");
        assert_eq!(slow.cache_age_secs, 15.0, "aged from when it began");
        *now.lock().unwrap() += Duration::from_secs(9);
        let next = cache
            .get(clock.clone(), || {
                prepared(counting(&calls, Fetched::unknown(None, "must not run")))
            })
            .await;
        assert_eq!(calls.load(Ordering::SeqCst), 1, "kept, not refetched");
        assert_eq!(next.reason.as_deref(), Some("timed out"));
        assert_eq!(next.cache_age_secs, 24.0);
        *now.lock().unwrap() += Duration::from_secs(1);
        cache
            .get(clock, || {
                prepared(counting(&calls, Fetched::unknown(None, "again")))
            })
            .await;
        assert_eq!(calls.load(Ordering::SeqCst), 2, "10 s after it finished");
    }

    /// A caller dropped while its fetch is in flight does not drop the fetch:
    /// it finishes on its own task and is kept, so the next caller is served
    /// it rather than asking coord again.
    #[tokio::test]
    async fn a_dropped_caller_does_not_drop_its_fetch() {
        let cache: &'static CapacityCache = Box::leak(Box::new(CapacityCache::new()));
        let calls = Arc::new(AtomicUsize::new(0));
        let (release, gate) = tokio::sync::oneshot::channel::<()>();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
        let first = {
            let calls = calls.clone();
            tokio::spawn(get_detached(cache, move || {
                prepared(async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    let _ = gate.await;
                    let _ = done_tx.send(());
                    known()
                })
            }))
        };
        while calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        first.abort();
        let _ = first.await;
        release.send(()).unwrap();
        done_rx.await.unwrap();
        // The detached fetch stores its read just after it answers.
        while cache.last(Instant::now()).is_none() {
            tokio::task::yield_now().await;
        }
        let again = {
            let calls = calls.clone();
            get_detached(cache, move || {
                prepared(async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Fetched::unknown(None, "must not run")
                })
            })
            .await
        };
        assert_eq!(calls.load(Ordering::SeqCst), 1, "coord was asked once");
        assert_eq!(again.state, "known");
    }

    /// A read whose task failed (here, a preparation that panics) is UNKNOWN
    /// naming the failure, and says it is not kept: its TTL is 0 and the
    /// cache holds nothing after it.
    #[tokio::test]
    async fn a_failed_task_is_an_unkept_unknown_read() {
        let cache: &'static CapacityCache = Box::leak(Box::new(CapacityCache::new()));
        let read = get_detached(cache, || -> Prepared<std::future::Ready<Fetched>> {
            panic!("the preparation failed")
        })
        .await;
        assert_eq!(read.state, "unknown");
        let reason = read.reason.as_deref().unwrap_or_default();
        assert!(reason.contains("task failed"), "{reason}");
        assert_eq!(read.cache_ttl_secs, 0, "a read that is not kept says 0");
        assert!(cache.last(Instant::now()).is_none(), "nothing was kept");
    }

    /// Once the read is a minute old coord is asked again, and the new
    /// answer, not the old row, is returned.
    #[tokio::test]
    async fn a_read_after_the_ttl_fetches_again() {
        let cache = CapacityCache::new();
        let calls = AtomicUsize::new(0);
        let t0 = Instant::now();
        cache
            .get(at(t0), || prepared(counting(&calls, known())))
            .await;
        let refetched = cache
            .get(at(t0 + CACHE_TTL), || {
                prepared(counting(
                    &calls,
                    Fetched::unknown(Some(device()), "coord unreachable: timed out"),
                ))
            })
            .await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "a 60 s old read is not served"
        );
        assert_eq!(refetched.state, "unknown");
        assert_eq!(
            refetched.row, None,
            "the old row is not served past its TTL"
        );
        assert_eq!(refetched.cache_age_secs, 0.0);
    }

    /// coord unreachable is UNKNOWN with the reason, carries no row and no
    /// number, and is retried after the shorter UNKNOWN TTL, not before.
    #[tokio::test]
    async fn coord_unreachable_is_unknown_with_its_reason_and_retried_after_ten_seconds() {
        let cache = CapacityCache::new();
        let calls = AtomicUsize::new(0);
        let t0 = Instant::now();
        let down = Fetched::unknown(
            Some(device()),
            "coord unreachable: error sending request: connection refused",
        );
        let read = cache
            .get(at(t0), || prepared(counting(&calls, down.clone())))
            .await;
        assert_eq!(read.state, "unknown");
        assert_eq!(read.row, None);
        assert_eq!(read.coord, None);
        assert!(read
            .reason
            .as_deref()
            .unwrap()
            .starts_with("coord unreachable"));
        assert_eq!(read.cache_ttl_secs, 10);
        // Never a zero: the read holds no capacity number at all.
        let wire = serde_json::to_value(&read).unwrap();
        assert!(wire["row"].is_null() && wire["coord"].is_null(), "{wire}");

        let held = cache
            .get(at(t0 + Duration::from_secs(9)), || {
                prepared(counting(&calls, known()))
            })
            .await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "an unknown is held for its TTL"
        );
        assert_eq!(held.state, "unknown");

        let retried = cache
            .get(at(t0 + UNKNOWN_TTL), || prepared(counting(&calls, known())))
            .await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(retried.state, "known");
    }

    /// Two reads that arrive together ask coord once: the second waits for
    /// the first's fetch and is given its answer.
    #[tokio::test]
    async fn concurrent_reads_ask_coord_once() {
        let cache = Arc::new(CapacityCache::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let (release, gate) = tokio::sync::oneshot::channel::<()>();
        let t0 = Instant::now();
        let first = {
            let (cache, calls) = (cache.clone(), calls.clone());
            tokio::spawn(async move {
                cache
                    .get(at(t0), || {
                        prepared(async move {
                            calls.fetch_add(1, Ordering::SeqCst);
                            let _ = gate.await;
                            known()
                        })
                    })
                    .await
            })
        };
        // Let the first read take the fetch lock and start its fetch.
        while calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        let second = {
            let (cache, calls) = (cache.clone(), calls.clone());
            tokio::spawn(async move {
                cache
                    .get(at(t0), || {
                        prepared(async move {
                            calls.fetch_add(1, Ordering::SeqCst);
                            Fetched::unknown(None, "must not run")
                        })
                    })
                    .await
            })
        };
        release.send(()).unwrap();
        let (a, b) = (first.await.unwrap(), second.await.unwrap());
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "one fetch answered both reads"
        );
        assert_eq!(a.state, "known");
        assert_eq!(b.row, a.row);
    }

    /// A read is aged when it is handed back, not when it was asked for: the
    /// fetch's own time is in the age of the read it fetched, and a read that
    /// waited for that fetch is aged at its own return too, so neither reads
    /// younger than the runner's ask.
    #[tokio::test]
    async fn a_read_is_aged_when_it_is_handed_back() {
        let t0 = Instant::now();
        let now = Arc::new(std::sync::Mutex::new(t0));
        let clock = {
            let now = now.clone();
            move || (*now.lock().unwrap(), Utc::now())
        };
        let cache = Arc::new(CapacityCache::new());
        let started = Arc::new(AtomicUsize::new(0));
        let (release, gate) = tokio::sync::oneshot::channel::<()>();
        let first = {
            let (cache, clock, now, started) =
                (cache.clone(), clock.clone(), now.clone(), started.clone());
            tokio::spawn(async move {
                cache
                    .get(clock, || {
                        prepared(async move {
                            started.fetch_add(1, Ordering::SeqCst);
                            let _ = gate.await;
                            // The fetch took 4 s.
                            *now.lock().unwrap() += Duration::from_secs(4);
                            known()
                        })
                    })
                    .await
            })
        };
        while started.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        let second = {
            let (cache, clock) = (cache.clone(), clock.clone());
            tokio::spawn(async move {
                cache
                    .get(clock, || {
                        prepared(async { Fetched::unknown(None, "must not run") })
                    })
                    .await
            })
        };
        release.send(()).unwrap();
        let (a, b) = (first.await.unwrap(), second.await.unwrap());
        assert_eq!(a.state, "known");
        assert_eq!(a.cache_age_secs, 4.0, "the fetch's own time is in its age");
        assert_eq!(
            b.row, a.row,
            "the waiting read was given that fetch's answer"
        );
        assert_eq!(b.cache_age_secs, 4.0, "and aged at its own return");
        assert_eq!(b.fetched_at, a.fetched_at);
    }

    /// `last` (behind [`cached`]) never fetches: nothing before the first
    /// read, then the last read at its true age, stale or not.
    #[tokio::test]
    async fn the_last_read_is_returned_without_fetching_at_its_age() {
        let cache = CapacityCache::new();
        let t0 = Instant::now();
        assert_eq!(
            cache.last(t0),
            None,
            "no read yet is no read, not an empty one"
        );
        let calls = AtomicUsize::new(0);
        cache
            .get(at(t0), || prepared(counting(&calls, known())))
            .await;
        let stale = cache
            .last(t0 + Duration::from_secs(90))
            .expect("the last read");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "last() asked coord nothing"
        );
        assert_eq!(stale.state, "known");
        assert_eq!(
            stale.cache_age_secs, 90.0,
            "its age says it is past the TTL"
        );
    }

    /// What a fake coord saw of each request: the raw query and the
    /// Authorization header.
    type Seen = Arc<std::sync::Mutex<Vec<(Option<String>, Option<String>)>>>;

    /// A fake coord on a loopback port serving `GET /coord/fleet/capacity`
    /// with `status` and `body`, and recording what each request carried.
    async fn fake_coord(status: u16, body: Value) -> (String, Seen) {
        use axum::extract::RawQuery;
        use axum::http::{HeaderMap, StatusCode};
        let seen: Seen = Arc::default();
        let log = seen.clone();
        let app = axum::Router::new().route(
            "/coord/fleet/capacity",
            axum::routing::get(move |RawQuery(query): RawQuery, headers: HeaderMap| {
                let (log, body) = (log.clone(), body.clone());
                async move {
                    let auth = headers
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string);
                    log.lock().unwrap().push((query, auth));
                    (StatusCode::from_u16(status).unwrap(), axum::Json(body))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        (format!("http://{addr}"), seen)
    }

    /// A fake coord whose capacity door answers 200 with `body`: whole, its
    /// length declared, or (`chunk` given) streamed in `chunk`-byte pieces
    /// with no length declared.
    async fn fake_coord_raw(body: Vec<u8>, chunk: Option<usize>) -> String {
        let app = axum::Router::new().route(
            "/coord/fleet/capacity",
            axum::routing::get(move || {
                let body = body.clone();
                async move {
                    match chunk {
                        Some(size) => {
                            let pieces: Vec<Result<bytes::Bytes, std::io::Error>> = body
                                .chunks(size)
                                .map(|c| Ok(bytes::Bytes::copy_from_slice(c)))
                                .collect();
                            axum::body::Body::from_stream(futures::stream::iter(pieces))
                        }
                        None => axum::body::Body::from(body),
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        format!("http://{addr}")
    }

    /// coord's answer is read no further than the bound: a declared length
    /// over it is refused before the body is read, and an undeclared one once
    /// what has arrived passes it — UNKNOWN naming the bound, never a row. A
    /// body within the bound that arrives in pieces is read whole.
    #[tokio::test]
    async fn an_answer_over_the_body_bound_is_unknown_and_read_no_further() {
        let big = vec![b' '; MAX_BODY_BYTES + 1];
        for chunk in [None, Some(64 * 1024)] {
            let base = fake_coord_raw(big.clone(), chunk).await;
            let fetched = fetch_from(&base, device(), "jwt").await;
            let Fetched::Unknown { reason, .. } = &fetched else {
                panic!("an answer over the bound is unknown ({chunk:?}): {fetched:?}");
            };
            let bound = format!("over the {MAX_BODY_BYTES}-byte bound");
            assert!(reason.contains(&bound), "{reason}");
            // Declared: refused by its length; streamed: by what arrived.
            assert_eq!(reason.contains("more than"), chunk.is_some(), "{reason}");
        }
        let base = fake_coord_raw(known_body(device()), Some(7)).await;
        let fetched = fetch_from(&base, device(), "jwt").await;
        let Fetched::Known { row, .. } = &fetched else {
            panic!("a body within the bound, in pieces, is read whole: {fetched:?}");
        };
        assert_eq!(*row, coord_row(device()));
    }

    /// The fetch over real HTTP: it asks coord for THIS device with the
    /// bearer, and the row coord sends back passes through unchanged.
    #[tokio::test]
    async fn the_fetch_asks_coord_for_this_device_with_the_bearer_and_passes_the_row_through() {
        let body: Value = serde_json::from_slice(&known_body(device())).unwrap();
        let (base, seen) = fake_coord(200, body).await;
        // A trailing slash on the base names the same door.
        let fetched = fetch_from(&format!("{base}/"), device(), "jwt-xyz").await;
        let Fetched::Known { row, coord, .. } = &fetched else {
            panic!("coord's row over HTTP is known: {fetched:?}");
        };
        assert_eq!(*row, coord_row(device()), "the row, unchanged");
        assert_eq!(coord["model_version"], 3, "coord's answer around it");
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 1, "one request");
        assert_eq!(
            seen[0].0.as_deref(),
            Some(format!("device_id={}", device()).as_str())
        );
        assert_eq!(seen[0].1.as_deref(), Some("Bearer jwt-xyz"));
    }

    /// Coord unreachable — nothing listening on its port — is UNKNOWN naming
    /// it, with no row and no number.
    #[tokio::test]
    async fn the_fetch_from_an_unreachable_coord_is_unknown() {
        // Bound but never listening: the port stays this test's, so no
        // parallel test's server can take it, and a connect to it is refused.
        let closed = tokio::net::TcpSocket::new_v4().unwrap();
        closed.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let addr = closed.local_addr().unwrap();
        let fetched = fetch_from(&format!("http://{addr}"), device(), "jwt").await;
        let Fetched::Unknown { device_id, reason } = &fetched else {
            panic!("an unreachable coord is unknown: {fetched:?}");
        };
        assert_eq!(*device_id, Some(device()));
        assert!(reason.starts_with("coord unreachable"), "{reason}");
    }

    /// Coord's typed refusal, over HTTP, is UNKNOWN keeping its code.
    #[tokio::test]
    async fn the_fetch_of_a_refusal_is_unknown_naming_its_code() {
        let refusal = json!({"error": "device_not_in_plan", "detail": "not a workstation"});
        let (base, _) = fake_coord(404, refusal).await;
        let fetched = fetch_from(&base, device(), "jwt").await;
        let Fetched::Unknown { reason, .. } = &fetched else {
            panic!("a refusal is unknown: {fetched:?}");
        };
        assert!(
            reason.contains("HTTP 404 device_not_in_plan: not a workstation"),
            "{reason}"
        );
    }

    /// A parsed `machine.json` from `doc`.
    fn machine(
        doc: Value,
    ) -> Result<
        qontinui_runner_lib::ambient::MachineJson,
        qontinui_runner_lib::ambient::MachineJsonError,
    > {
        Ok(
            qontinui_runner_lib::ambient::try_parse_machine_json(
                &serde_json::to_vec(&doc).unwrap(),
            )
            .unwrap(),
        )
    }

    fn device_reason(
        read: Result<
            qontinui_runner_lib::ambient::MachineJson,
            qontinui_runner_lib::ambient::MachineJsonError,
        >,
    ) -> String {
        device_from(read).expect_err("names no device")
    }

    #[test]
    fn device_from_a_complete_file_is_its_device() {
        let read = machine(json!({"device_id": device().to_string(), "hostname": "nawa"}));
        assert_eq!(device_from(read), Ok(device()));
    }

    #[test]
    fn device_from_no_home_dir_says_so() {
        let reason = device_reason(Err(
            qontinui_runner_lib::ambient::MachineJsonError::NoHomeDir,
        ));
        assert!(reason.contains("no home directory resolved"), "{reason}");
    }

    #[test]
    fn device_from_a_missing_file_says_it_does_not_exist() {
        let reason = device_reason(Err(qontinui_runner_lib::ambient::MachineJsonError::Read {
            path: "/h/.qontinui/machine.json".into(),
            source: std::io::Error::from(std::io::ErrorKind::NotFound),
        }));
        assert!(
            reason.contains("/h/.qontinui/machine.json does not exist"),
            "{reason}"
        );
        assert!(!reason.contains("names no"), "{reason}");
    }

    #[test]
    fn device_from_an_unreadable_file_names_the_io_error() {
        let reason = device_reason(Err(qontinui_runner_lib::ambient::MachineJsonError::Read {
            path: "/h/.qontinui/machine.json".into(),
            source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "access denied"),
        }));
        assert!(
            reason.contains("/h/.qontinui/machine.json could not be read (access denied)"),
            "{reason}"
        );
    }

    #[test]
    fn device_from_an_unparseable_file_names_the_parse_error() {
        let source = serde_json::from_slice::<Value>(b"{not json").unwrap_err();
        let detail = source.to_string();
        let reason = device_reason(Err(qontinui_runner_lib::ambient::MachineJsonError::Parse {
            path: "/h/.qontinui/machine.json".into(),
            source,
        }));
        assert!(
            reason.contains("/h/.qontinui/machine.json is not a valid machine.json"),
            "{reason}"
        );
        assert!(reason.contains(&detail), "{reason}");
    }

    #[test]
    fn device_from_a_file_without_device_id_says_only_that() {
        let reason = device_reason(machine(json!({"hostname": "nawa"})));
        assert!(reason.contains("names no device_id"), "{reason}");
        assert!(!reason.contains("hostname"), "{reason}");
    }

    #[test]
    fn device_from_a_file_without_hostname_says_coord_holds_no_row() {
        let reason = device_reason(machine(json!({"device_id": device().to_string()})));
        assert!(reason.contains("names no hostname"), "{reason}");
        assert!(
            reason.contains("the sample publisher skips a device with no hostname"),
            "{reason}"
        );
        assert!(reason.contains("coord holds no row for it"), "{reason}");
    }

    #[test]
    fn device_from_a_device_id_that_is_not_a_uuid_says_so() {
        let reason = device_reason(machine(json!({"device_id": "nawa-1", "hostname": "nawa"})));
        assert!(reason.contains("\"nawa-1\" is not a UUID"), "{reason}");
    }

    /// On an isolated machine the real reader finds no file, and the reason
    /// says the file is missing (not that it names nothing).
    #[test]
    fn device_from_the_isolated_machine_says_the_file_is_missing() {
        let _amb = crate::test_env::isolated_ambient();
        let reason = device_reason(qontinui_runner_lib::ambient::try_read_machine_json());
        assert!(reason.contains("does not exist"), "{reason}");
        assert!(reason.contains("machine.json"), "{reason}");
    }

    /// The fields a local tool reads are a contract: each is present (null
    /// when absent), and the row's own fields keep coord's spelling.
    #[test]
    fn the_read_names_its_fields() {
        let read = entry_at(Instant::now(), known()).read_at(Instant::now());
        let wire = serde_json::to_value(&read).unwrap();
        for key in [
            "state",
            "device_id",
            "row",
            "coord",
            "reason",
            "fetched_at",
            "cache_age_secs",
            "cache_ttl_secs",
        ] {
            assert!(
                wire.get(key).is_some(),
                "{key} is served (null when absent)"
            );
        }
        assert_eq!(wire["state"], "known");
        assert_eq!(wire["device_id"], device().to_string());
        assert_eq!(wire["row"]["slots"]["C3"]["value"], 10);
        assert_eq!(wire["row"]["c3_cpu_cores"]["value"], 40.0);
        assert!(
            wire.get("capacity_model").is_none(),
            "the dial is not part of a read"
        );
        assert!(wire["reason"].is_null());
    }
}
