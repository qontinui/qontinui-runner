//! Loopback routes of the build-admission broker, and its 2 s tick.
//!
//! | Route | Caller | Secret |
//! |---|---|---|
//! | `POST /build-admission/tickets` | the cargo wrappers | returns one |
//! | `GET /build-admission/tickets/{id}?wait=N` | the wrapper that opened it | `X-Build-Admission-Secret` |
//! | `POST /build-admission/leases/{id}/release` | the wrapper that opened it | `X-Build-Admission-Secret` |
//! | `GET /build-admission/state` | anyone (console, `/whereami`, Phase 7 publisher) | none; secrets never appear |
//!
//! The secret travels in a header, never in a body or a URL, so it stays out
//! of request bodies and access logs (this refines plan D4's
//! `release {secret, exit, reason}`; the body keeps `exit_code` — `exit` is
//! accepted as an alias — and `reason`).
//!
//! The writes and the per-ticket poll are credential doors in the origin guard
//! (`origin_guard::CREDENTIAL_DOORS`), so a browser origin is refused under
//! every policy while the wrappers (curl, no `Origin`) are admitted.
//!
//! Only the instance that owns the shared root state runs the broker: a
//! secondary or temp runner answers 503 `BUILD_ADMISSION_NOT_PRIMARY`, and the
//! wrappers fall back to their degraded arm. `QONTINUI_BUILD_ADMISSION=0`
//! answers 503 `BUILD_ADMISSION_OFF` the same way. Neither ever blocks a build.
//!
//! All file I/O happens on the tick (in `spawn_blocking`): a handler only
//! marks the ledger dirty, so a host stalling on fsync never stalls a request.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use axum::{
    extract::{Path, Query},
    http::{HeaderMap, StatusCode},
    response::Json,
    routing::{get, post},
    Router,
};
use qontinui_types::build_admission::{Fact, HostFacts};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::broker::{
    hash_secret, AccessError, Broker, OpenError, Record, ReleaseReason, Shadow, TicketRequest,
};
use super::facts::{self, FactsDetail, NonBuildTracker, Roots};
use super::persist;
use super::policy::{self, Layer, Level, Resolved};
use crate::mcp::types::{ApiResponse, ApiState};

pub const SECRET_HEADER: &str = "x-build-admission-secret";
/// Longest a poll may hold the connection.
const MAX_WAIT_S: u64 = 30;
const TICK: Duration = Duration::from_secs(2);
/// Terminal records listed by `/state` beside every open one.
const STATE_RECENT_TERMINAL: usize = 50;

type Resp = (StatusCode, Json<ApiResponse<Value>>);

/// The last successfully parsed layer of each policy file (D8: an unreadable
/// file keeps its last-known value, never falls through to the layer below).
#[derive(Default)]
struct LastKnown {
    overrides: Option<Layer>,
    coord: Option<Layer>,
}

struct Shared {
    broker: Mutex<Broker>,
    facts: Mutex<Option<FactsDetail>>,
    tracker: Mutex<NonBuildTracker>,
    policy: Mutex<Resolved>,
    last_known: Mutex<LastKnown>,
    notes: Mutex<Vec<String>>,
    dirty: AtomicBool,
}

static SHARED: OnceLock<Shared> = OnceLock::new();
static STARTED: AtomicBool = AtomicBool::new(false);

/// Lock, recovering a poisoned guard: every value behind these mutexes is
/// plain data, and a broker that silently stops after one panic is worse than
/// one that carries on with the last consistent value.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn now_s() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn shared() -> &'static Shared {
    SHARED.get_or_init(|| {
        let mut notes = Vec::new();
        let mut broker = match persist::dir() {
            Some(d) => {
                let (b, note) = persist::load_state(&d);
                notes.extend(note);
                b
            }
            None => {
                notes.push("no ~/.qontinui directory: state is not persisted".into());
                Broker::default()
            }
        };
        reload_seeds(&mut broker, &mut notes);
        let mut last = LastKnown::default();
        let resolved = current_policy(&mut last);
        Shared {
            broker: Mutex::new(broker),
            facts: Mutex::new(None),
            tracker: Mutex::new(NonBuildTracker::default()),
            policy: Mutex::new(resolved),
            last_known: Mutex::new(last),
            notes: Mutex::new(notes),
            dirty: AtomicBool::new(false),
        }
    })
}

fn reload_seeds(broker: &mut Broker, notes: &mut Vec<String>) {
    if let Some(d) = persist::dir() {
        let (seeds, note) = persist::load_seeds(&d);
        broker.seeds = seeds;
        notes.extend(note);
    }
}

/// Re-read both policy files; a file that cannot be read or parsed keeps its
/// last-known layer and says so.
fn current_policy(last: &mut LastKnown) -> Resolved {
    let env = std::env::var("QONTINUI_BUILD_ADMISSION").ok();
    let mut notes = Vec::new();
    if let Some(d) = persist::dir() {
        refresh(
            &d,
            "overrides.toml",
            policy::parse_override,
            &mut last.overrides,
            &mut notes,
        );
        refresh(
            &d,
            "policy.json",
            policy::parse_coord,
            &mut last.coord,
            &mut notes,
        );
    }
    policy::resolve_layers(
        env.as_deref(),
        last.overrides.as_ref(),
        last.coord.as_ref(),
        notes,
    )
}

fn refresh(
    dir: &std::path::Path,
    name: &str,
    parse: fn(&str) -> Result<Layer, String>,
    slot: &mut Option<Layer>,
    notes: &mut Vec<String>,
) {
    match persist::read_text(dir, name) {
        Ok(None) => *slot = None,
        Ok(Some(text)) => match parse(&text) {
            Ok(l) => *slot = Some(l),
            Err(e) => notes.push(format!(
                "{name} unparseable ({e}); keeping its last-known value"
            )),
        },
        Err(e) => notes.push(format!(
            "{name} unreadable ({e}); keeping its last-known value"
        )),
    }
}

#[cfg(target_os = "linux")]
fn own_uid() -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self").ok().map(|m| m.uid())
}

#[cfg(not(target_os = "linux"))]
fn own_uid() -> Option<u32> {
    None
}

/// A process's start time — the pid-reuse guard on every platform: kernel
/// clock ticks from `/proc/<pid>/stat` on Linux, sysinfo's start time
/// elsewhere.
fn pid_start(pid: u32) -> Option<u64> {
    if cfg!(target_os = "linux") {
        return std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|t| facts::parse_stat(&t))
            .map(|(_, _, start)| start);
    }
    let p = sysinfo::Pid::from_u32(pid);
    let mut sys = sysinfo::System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[p]), true);
    sys.process(p).map(|pr| pr.start_time())
}

fn pid_alive(pid: u32, start: Option<u64>) -> bool {
    if pid == 0 {
        return false;
    }
    match (pid_start(pid), start) {
        (None, _) => false,
        (Some(now), Some(then)) => now == then,
        (Some(_), None) => true,
    }
}

/// The runner's own pid and its ancestors (Linux), which a ticket must never
/// name: such a "wrapper" would never die, would hold its directory forever,
/// and would claim every build on the box as leased.
fn own_lineage() -> Vec<u32> {
    let mut out = vec![std::process::id()];
    if cfg!(target_os = "linux") {
        let mut cur = std::process::id();
        for _ in 0..64 {
            let Some((_, ppid, _)) = std::fs::read_to_string(format!("/proc/{cur}/stat"))
                .ok()
                .and_then(|t| facts::parse_stat(&t))
            else {
                break;
            };
            if ppid <= 1 {
                break;
            }
            out.push(ppid);
            cur = ppid;
        }
    }
    out
}

fn pid_uid(pid: u32) -> Option<u32> {
    std::fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()
        .and_then(|t| {
            t.lines()
                .find_map(|l| l.strip_prefix("Uid:"))
                .and_then(|v| v.split_whitespace().next()?.parse().ok())
        })
}

/// Why a ticket's pid is refused, if it is.
pub fn pid_refusal(
    pid: u32,
    lineage: &[u32],
    start: Option<u64>,
    linux: bool,
    uid: Option<u32>,
    own: Option<u32>,
) -> Option<&'static str> {
    if pid <= 1 {
        return Some("pid must be the wrapper's own pid");
    }
    if lineage.contains(&pid) {
        return Some("pid names the runner or one of its ancestors");
    }
    if start.is_none() {
        return Some("no such process");
    }
    if linux && uid != own {
        return Some("pid belongs to another user");
    }
    None
}

/// One tick: re-read the policy and seeds, measure the host, sample the
/// leased trees, reap dead wrappers, run the shadow scheduler, persist when
/// anything changed.
fn tick_once() {
    let s = shared();
    let resolved = current_policy(&mut lock(&s.last_known));
    let lease_pids = lock(&s.broker).lease_pids();
    let now = now_s();
    let detail = facts::collect(
        &Roots::host(),
        own_uid(),
        &lease_pids,
        &mut lock(&s.tracker),
        // Host pause (D7) arrives with Phase 7's desired payload; until then
        // it is clear.
        false,
        now,
    );
    let mut seed_notes = Vec::new();
    {
        let mut b = lock(&s.broker);
        reload_seeds(&mut b, &mut seed_notes);
        if let Some(per) = detail.build_rss.as_ref().map(|r| &r.per_lease) {
            if !b.lease_pids().is_empty() {
                b.observe_usage(per);
                s.dirty.store(true, Ordering::Relaxed);
            }
        }
        if b.reap(pid_alive, now) > 0 {
            s.dirty.store(true, Ordering::Relaxed);
        }
        if b.shadow_step(&detail.facts, &resolved.policy, now) > 0 {
            s.dirty.store(true, Ordering::Relaxed);
        }
        if s.dirty.swap(false, Ordering::Relaxed) {
            persist_all(&b, &resolved, &detail, now);
        }
    }
    {
        let mut n = lock(&s.notes);
        n.retain(|x| !x.starts_with("seeds.json"));
        n.extend(seed_notes);
    }
    *lock(&s.facts) = Some(detail);
    *lock(&s.policy) = resolved;
}

fn persist_all(b: &Broker, resolved: &Resolved, detail: &FactsDetail, now: u64) {
    let Some(d) = persist::dir() else { return };
    if let Err(e) = persist::save_state(&d, b) {
        tracing::warn!(error = %e, "build admission: state.json write failed");
    }
    let file = persist::EstimatesFile {
        written_at_s: now,
        non_build_p95_bytes: detail.facts.non_build_p95_bytes,
        estimates: persist::estimates(b, &resolved.policy),
    };
    if let Err(e) = persist::save_estimates(&d, &file) {
        tracing::warn!(error = %e, "build admission: estimates.json write failed");
    }
}

/// Start the tick (idempotent; never under `cfg(test)`, never on a secondary).
pub fn start() {
    if cfg!(test) || !crate::instance::owns_shared_root_state() {
        return;
    }
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    crate::worker_supervisor::spawn_supervised("build_admission.tick", || async {
        loop {
            if let Err(e) = tokio::task::spawn_blocking(tick_once).await {
                tracing::warn!(error = %e, "build admission tick panicked");
            }
            tokio::time::sleep(TICK).await;
        }
    });
}

/// Routes contributed to the runner's main router; also the boot hook for the
/// tick (the same idempotent start-from-`routes()` pattern as
/// `github_budget_api`, which keeps `mcp_api.rs`'s share to one merge line).
pub fn routes() -> Router<Arc<ApiState>> {
    start();
    Router::new()
        .route("/build-admission/tickets", post(open_ticket))
        .route("/build-admission/tickets/{id}", get(poll_ticket))
        .route("/build-admission/leases/{id}/release", post(release_lease))
        .route("/build-admission/state", get(state))
}

fn err(status: StatusCode, code: &str, msg: impl Into<String>) -> Resp {
    let mut r = ApiResponse::<Value>::error(msg);
    r.code = Some(code.into());
    (status, Json(r))
}

/// The served level, or the 503 that sends the wrapper to its degraded arm.
fn gate() -> Result<Level, Box<Resp>> {
    if !cfg!(test) && !crate::instance::owns_shared_root_state() {
        return Err(Box::new(err(
            StatusCode::SERVICE_UNAVAILABLE,
            "BUILD_ADMISSION_NOT_PRIMARY",
            "this runner instance does not run the build-admission broker; use the degraded arm",
        )));
    }
    let level = lock(&shared().policy).level;
    if level == Level::Off {
        return Err(Box::new(err(
            StatusCode::SERVICE_UNAVAILABLE,
            "BUILD_ADMISSION_OFF",
            "the build-admission broker is off on this host; use the degraded arm",
        )));
    }
    Ok(level)
}

/// The record as a caller may see it: no secret, no hash.
fn view(r: &Record, level: Level) -> Value {
    json!({
        "ticket_id": r.id,
        "lease_id": r.id,
        "state": r.state,
        "level": level,
        // Observe imposes nothing: the caller's own -j stands, else none.
        "jobs": r.request.requested_jobs,
        "class": r.class,
        "class_demoted_reason": r.class_demoted_reason,
        "estimate": r.est,
        "shadow": r.shadow,
        "would": r.would,
        "queued_at_s": r.queued_at_s,
        "ended_at_s": r.ended_at_s,
        "exit_code": r.exit_code,
        "max_sampled_anon_bytes": r.max_sampled_anon_bytes,
    })
}

fn facts_now() -> FactsDetail {
    let s = shared();
    if let Some(f) = lock(&s.facts).clone() {
        return f;
    }
    let pids = lock(&s.broker).lease_pids();
    facts::collect(
        &Roots::host(),
        own_uid(),
        &pids,
        &mut lock(&s.tracker),
        false,
        now_s(),
    )
}

/// Facts for a step taken before the first tick has measured anything: every
/// memory input unknown, so the shadow withholds rather than guessing.
fn unknown_facts() -> HostFacts {
    HostFacts {
        mem_total_bytes: 0,
        cpus: std::thread::available_parallelism()
            .map(|n| n.get() as u32)
            .unwrap_or(1),
        mem_available_bytes: Fact::Unknown,
        psi_mem_full_avg10: Fact::Unknown,
        non_build_p95_bytes: Fact::Unknown,
        ci_reservation_bytes: Fact::Unknown,
        unleased_build_rss_bytes: Fact::Unknown,
        admissions_paused: false,
    }
}

async fn open_ticket(Json(req): Json<TicketRequest>) -> Resp {
    let level = match gate() {
        Ok(l) => l,
        Err(e) => return *e,
    };
    if req.output_dir.trim().is_empty() || req.repo.trim().is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "INVALID_REQUEST",
            "repo and output_dir are required",
        );
    }
    let pid = req.pid;
    let probe = tokio::task::spawn_blocking(move || {
        let start = pid_start(pid);
        let refusal = pid_refusal(
            pid,
            &own_lineage(),
            start,
            cfg!(target_os = "linux"),
            pid_uid(pid),
            own_uid(),
        );
        (start, refusal, facts_now())
    })
    .await;
    let Ok((start, refusal, detail)) = probe else {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "FACTS_FAILED",
            "could not read host facts",
        );
    };
    if let Some(why) = refusal {
        return err(StatusCode::BAD_REQUEST, "INVALID_PID", why);
    }
    let mut raw = [0u8; 32];
    rand::rng().fill_bytes(&mut raw);
    let secret = hex::encode(raw);
    let id = uuid::Uuid::now_v7().to_string();
    let s = shared();
    let policy = lock(&s.policy).policy;
    let mut body = {
        let mut b = lock(&s.broker);
        match b.open(
            id,
            hash_secret(&secret),
            req,
            start,
            &detail.facts,
            &policy,
            now_s(),
        ) {
            Ok(rec) => view(rec, level),
            Err(OpenError::TooMany) => {
                return err(
                    StatusCode::TOO_MANY_REQUESTS,
                    "TOO_MANY_OPEN_TICKETS",
                    "too many build-admission tickets are open; use the degraded arm",
                )
            }
        }
    };
    s.dirty.store(true, Ordering::Relaxed);
    body["secret"] = Value::String(secret);
    (StatusCode::CREATED, Json(ApiResponse::success(body)))
}

fn secret_of(headers: &HeaderMap) -> Option<String> {
    headers
        .get(SECRET_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

fn access_err(e: AccessError) -> Resp {
    match e {
        AccessError::NotFound => err(StatusCode::NOT_FOUND, "TICKET_NOT_FOUND", "no such ticket"),
        AccessError::BadSecret => err(
            StatusCode::FORBIDDEN,
            "BAD_TICKET_SECRET",
            "the ticket secret does not match",
        ),
    }
}

#[derive(Debug, Deserialize)]
struct WaitQuery {
    wait: Option<u64>,
}

async fn poll_ticket(
    Path(id): Path<String>,
    Query(q): Query<WaitQuery>,
    headers: HeaderMap,
) -> Resp {
    let level = match gate() {
        Ok(l) => l,
        Err(e) => return *e,
    };
    let Some(secret) = secret_of(&headers) else {
        return access_err(AccessError::BadSecret);
    };
    // `wait` is accepted and bounded now so the wrapper's poll loop is final;
    // observe grants at open, so there is never anything to wait for and the
    // poll answers at once. The enforcing broker (Phase 4) holds it up to the
    // bound.
    let _bounded_wait_s = q.wait.unwrap_or(0).min(MAX_WAIT_S);
    let b = lock(&shared().broker);
    match b.authorize(&id, &secret) {
        Ok(r) => (StatusCode::OK, Json(ApiResponse::success(view(r, level)))),
        Err(e) => access_err(e),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseBody {
    #[serde(default, alias = "exit")]
    exit_code: Option<i32>,
    reason: ReleaseReason,
    /// The wrapper's own peak reading: recorded as a cross-check only.
    #[serde(default)]
    peak_anon_bytes: Option<u64>,
}

async fn release_lease(
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<ReleaseBody>,
) -> Resp {
    let level = match gate() {
        Ok(l) => l,
        Err(e) => return *e,
    };
    let Some(secret) = secret_of(&headers) else {
        return access_err(AccessError::BadSecret);
    };
    let s = shared();
    let facts = lock(&s.facts)
        .clone()
        .map(|d| d.facts)
        .unwrap_or_else(unknown_facts);
    let policy = lock(&s.policy).policy;
    let out = {
        let mut b = lock(&s.broker);
        let r = b.release(
            &id,
            &secret,
            body.reason,
            body.exit_code,
            body.peak_anon_bytes,
            facts.cpus,
            &facts,
            &policy,
            now_s(),
        );
        match r {
            Ok(r) => view(r, level),
            Err(e) => return access_err(e),
        }
    };
    s.dirty.store(true, Ordering::Relaxed);
    (StatusCode::OK, Json(ApiResponse::success(out)))
}

/// What the state endpoint serves. No secret or hash ever appears.
#[derive(Debug, Serialize)]
struct StateView {
    arm: &'static str,
    level: Level,
    level_requested: Level,
    level_source: policy::LevelSource,
    policy: qontinui_types::build_admission::Policy,
    notes: Vec<String>,
    broker_git_sha: &'static str,
    facts: Option<FactsDetail>,
    budget: Option<qontinui_types::build_admission::budget::Budget>,
    running: usize,
    shadow_queued: usize,
    oldest_shadow_wait_s: Option<u64>,
    /// Every open ticket, then the newest terminal ones.
    tickets: Vec<Value>,
}

async fn state() -> Resp {
    if !cfg!(test) && !crate::instance::owns_shared_root_state() {
        return err(
            StatusCode::SERVICE_UNAVAILABLE,
            "BUILD_ADMISSION_NOT_PRIMARY",
            "this runner instance does not run the build-admission broker",
        );
    }
    let s = shared();
    let p = lock(&s.policy).clone();
    let facts = lock(&s.facts).clone();
    let mut notes = p.notes.clone();
    notes.extend(lock(&s.notes).iter().cloned());
    let now = now_s();
    let (running, queued, oldest, tickets) = {
        let b = lock(&s.broker);
        let mut open: Vec<&Record> = b
            .records
            .values()
            .filter(|r| !r.state.is_terminal())
            .collect();
        let mut done: Vec<&Record> = b
            .records
            .values()
            .filter(|r| r.state.is_terminal())
            .collect();
        open.sort_by_key(|r| r.queued_at_s);
        done.sort_by_key(|r| std::cmp::Reverse(r.ended_at_s));
        let tickets: Vec<Value> = open
            .iter()
            .chain(done.iter().take(STATE_RECENT_TERMINAL))
            .map(|r| view(r, p.level))
            .collect();
        (
            open.len(),
            b.records
                .values()
                .filter(|r| r.shadow == Shadow::Queued)
                .count(),
            b.oldest_shadow_wait_s(now),
            tickets,
        )
    };
    let v = StateView {
        arm: "broker",
        level: p.level,
        level_requested: p.level_requested,
        level_source: p.level_source,
        policy: p.policy,
        notes,
        broker_git_sha: env!("QONTINUI_GIT_SHA"),
        budget: facts
            .as_ref()
            .map(|f| qontinui_types::build_admission::budget::budget(&f.facts, &p.policy)),
        facts,
        running,
        shadow_queued: queued,
        oldest_shadow_wait_s: oldest,
        tickets,
    };
    match serde_json::to_value(v) {
        Ok(v) => (StatusCode::OK, Json(ApiResponse::success(v))),
        Err(e) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "SERIALIZE_FAILED",
            e.to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn app() -> Router {
        Router::new()
            .route("/build-admission/tickets", post(open_ticket))
            .route("/build-admission/tickets/{id}", get(poll_ticket))
            .route("/build-admission/leases/{id}/release", post(release_lease))
            .route("/build-admission/state", get(state))
    }

    async fn call(req: Request<Body>) -> (StatusCode, Value) {
        let resp = app().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        // An extractor rejection in a bare test router is plain text (the
        // production router wraps it in the envelope).
        let v = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into()));
        (status, v)
    }

    fn post_json(uri: &str, body: Value, secret: Option<&str>) -> Request<Body> {
        let mut b = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json");
        if let Some(s) = secret {
            b = b.header(SECRET_HEADER, s);
        }
        b.body(Body::from(body.to_string())).unwrap()
    }

    /// A real, killable child process to stand in for a wrapper.
    fn wrapper() -> std::process::Child {
        if cfg!(windows) {
            std::process::Command::new("cmd")
                .args(["/C", "ping -n 30 127.0.0.1 >NUL"])
                .spawn()
                .unwrap()
        } else {
            std::process::Command::new("sleep")
                .arg("30")
                .spawn()
                .unwrap()
        }
    }

    /// Open → poll → release round trip; wrong or missing secret refused;
    /// the state view never carries a secret.
    #[tokio::test]
    async fn ticket_round_trip_with_secrets() {
        let _amb = crate::test_env::isolated_ambient();
        let mut child = wrapper();
        let (st, v) = call(post_json(
            "/build-admission/tickets",
            json!({"repo":"qontinui-coord","subcommand":"check","output_dir":"/t/debug",
                   "target_dir_kind":"shared_warm","pid":child.id()}),
            None,
        ))
        .await;
        assert_eq!(st, StatusCode::CREATED, "{v}");
        let id = v["data"]["ticket_id"].as_str().unwrap().to_owned();
        let secret = v["data"]["secret"].as_str().unwrap().to_owned();
        assert_eq!(v["data"]["state"], "running");
        assert_eq!(v["data"]["level"], "observe");
        assert!(v["data"]["jobs"].is_null(), "observe imposes no job count");

        let poll = |s: Option<&str>| {
            let mut b = Request::builder().uri(format!("/build-admission/tickets/{id}?wait=5"));
            if let Some(s) = s {
                b = b.header(SECRET_HEADER, s);
            }
            b.body(Body::empty()).unwrap()
        };
        assert_eq!(call(poll(Some(&secret))).await.0, StatusCode::OK);
        let (st, v) = call(poll(Some("nope"))).await;
        assert_eq!(
            (st, v["code"].as_str()),
            (StatusCode::FORBIDDEN, Some("BAD_TICKET_SECRET"))
        );
        assert_eq!(call(poll(None)).await.0, StatusCode::FORBIDDEN);

        let rel = format!("/build-admission/leases/{id}/release");
        let (st, _) = call(post_json(
            &rel,
            json!({"reason":"exit","exit_code":0}),
            Some("nope"),
        ))
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN);
        // `exit` is accepted as an alias of `exit_code`.
        let (st, v) = call(post_json(
            &rel,
            json!({"reason":"exit","exit":0}),
            Some(&secret),
        ))
        .await;
        assert_eq!(
            (st, v["data"]["state"].as_str()),
            (StatusCode::OK, Some("done"))
        );
        let (st, _) = call(post_json(
            &rel,
            json!({"reason":"exit","bogus":1}),
            Some(&secret),
        ))
        .await;
        assert!(st.is_client_error(), "unknown release fields are refused");

        let (st, v) = call(
            Request::builder()
                .uri("/build-admission/state")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["data"]["arm"], "broker");
        let text = v.to_string();
        assert!(!text.contains(&secret) && !text.contains(&hash_secret(&secret)));
        assert_eq!(
            call(post_json(
                "/build-admission/tickets",
                json!({"repo":"r"}),
                None
            ))
            .await
            .0
            .as_u16()
                / 100,
            4
        );
        let _ = child.kill();
        let _ = child.wait();
    }

    /// The runner's own pid, pid 0/1, or a dead pid is never a wrapper.
    #[tokio::test]
    async fn a_ticket_naming_the_runner_or_a_dead_pid_is_refused() {
        let _amb = crate::test_env::isolated_ambient();
        for pid in [std::process::id(), 1, 0, u32::MAX - 7] {
            let (st, v) = call(post_json(
                "/build-admission/tickets",
                json!({"repo":"r","subcommand":"check","output_dir":"/o",
                       "target_dir_kind":"shared_warm","pid":pid}),
                None,
            ))
            .await;
            assert_eq!(
                (st, v["code"].as_str()),
                (StatusCode::BAD_REQUEST, Some("INVALID_PID")),
                "pid {pid}"
            );
        }
    }

    #[test]
    fn pid_refusals() {
        let lineage = [100, 50, 10];
        assert!(pid_refusal(1, &lineage, Some(1), true, Some(5), Some(5)).is_some());
        assert!(pid_refusal(50, &lineage, Some(1), true, Some(5), Some(5)).is_some());
        assert!(pid_refusal(200, &lineage, None, true, Some(5), Some(5)).is_some());
        assert!(pid_refusal(200, &lineage, Some(1), true, Some(6), Some(5)).is_some());
        assert!(pid_refusal(200, &lineage, Some(1), true, Some(5), Some(5)).is_none());
        // Off Linux the uid is not readable and not compared.
        assert!(pid_refusal(200, &lineage, Some(1), false, None, None).is_none());
    }

    /// The writes and the poll are credential doors (a browser origin is
    /// refused under every policy); the read-only state is not.
    #[test]
    fn origin_guard_classifies_the_routes() {
        use crate::mcp::origin_guard::is_credential_door;
        assert!(is_credential_door("POST", "/build-admission/tickets"));
        assert!(is_credential_door(
            "POST",
            "/build-admission/leases/{id}/release"
        ));
        assert!(is_credential_door("GET", "/build-admission/tickets/{id}"));
        assert!(!is_credential_door("GET", "/build-admission/state"));
    }
}
