//! Loopback routes of the build-admission broker, and its 2 s tick.
//!
//! | Route | Caller | Secret |
//! |---|---|---|
//! | `POST /build-admission/tickets` | the cargo wrappers | returns one |
//! | `GET /build-admission/tickets/{id}?wait=N` | the wrapper that opened it | `X-Build-Admission-Secret` |
//! | `POST /build-admission/leases/{id}/release` | the wrapper that opened it | `X-Build-Admission-Secret` |
//! | `GET /build-admission/state` | anyone (console, `/whereami`, Phase 7 publisher) | none; secrets never appear |
//!
//! The writes and the per-ticket poll are credential doors in the origin guard
//! (`origin_guard::CREDENTIAL_DOORS`), so a browser origin is refused under
//! every policy while the wrappers (curl, no `Origin`) are admitted.
//!
//! Only the instance that owns the shared root state runs the broker: a
//! secondary or temp runner answers 503 `BUILD_ADMISSION_NOT_PRIMARY`, and the
//! wrappers fall back to their degraded arm. `QONTINUI_BUILD_ADMISSION=0`
//! answers 503 `BUILD_ADMISSION_OFF` the same way. Neither ever blocks a build.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::{
    extract::{Path, Query},
    http::{HeaderMap, StatusCode},
    response::Json,
    routing::{get, post},
    Router,
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Notify;

use super::broker::{hash_secret, AccessError, Broker, Record, ReleaseReason, TicketRequest};
use super::facts::{self, FactsDetail, NonBuildTracker, Roots};
use super::persist;
use super::policy::{self, Level, Resolved};
use crate::mcp::types::{ApiResponse, ApiState};

pub const SECRET_HEADER: &str = "x-build-admission-secret";
/// Longest a poll may hold the connection.
const MAX_WAIT_S: u64 = 30;
const TICK: Duration = Duration::from_secs(2);

type Resp = (StatusCode, Json<ApiResponse<Value>>);

struct Shared {
    broker: Mutex<Broker>,
    facts: Mutex<Option<FactsDetail>>,
    tracker: Mutex<NonBuildTracker>,
    policy: Mutex<Resolved>,
    notes: Mutex<Vec<String>>,
    changed: Notify,
}

static SHARED: OnceLock<Shared> = OnceLock::new();
static STARTED: AtomicBool = AtomicBool::new(false);

fn now_s() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn shared() -> &'static Shared {
    SHARED.get_or_init(|| {
        let mut notes = Vec::new();
        let (mut broker, note) = match persist::dir() {
            Some(d) => persist::load_state(&d),
            None => (
                Broker::default(),
                Some("no ~/.qontinui directory: state is not persisted".into()),
            ),
        };
        notes.extend(note);
        if let Some(d) = persist::dir() {
            let (seeds, note) = persist::load_seeds(&d);
            broker.seeds = seeds;
            notes.extend(note);
        }
        Shared {
            broker: Mutex::new(broker),
            facts: Mutex::new(None),
            tracker: Mutex::new(NonBuildTracker::default()),
            policy: Mutex::new(current_policy()),
            notes: Mutex::new(notes),
            changed: Notify::new(),
        }
    })
}

fn current_policy() -> Resolved {
    let env = std::env::var("QONTINUI_BUILD_ADMISSION").ok();
    let dir = persist::dir();
    let over = dir
        .as_deref()
        .and_then(|d| persist::read_text(d, "overrides.toml"));
    let cached = dir
        .as_deref()
        .and_then(|d| persist::read_text(d, "policy.json"));
    policy::resolve(env.as_deref(), over.as_deref(), cached.as_deref())
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

/// Kernel start time of `pid` (Linux), the pid-reuse guard.
fn pid_start(pid: u32) -> Option<u64> {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|t| facts::parse_stat(&t))
        .map(|(_, _, start)| start)
}

fn pid_alive(pid: u32, start: Option<u64>) -> bool {
    if pid == 0 {
        return false;
    }
    if cfg!(target_os = "linux") {
        return match (pid_start(pid), start) {
            (None, _) => false,
            (Some(now), Some(then)) => now == then,
            (Some(_), None) => true,
        };
    }
    let p = sysinfo::Pid::from_u32(pid);
    let mut sys = sysinfo::System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[p]), true);
    sys.process(p).is_some()
}

/// One tick: re-read the policy, measure the host, reap dead wrappers, run the
/// shadow scheduler, persist on change.
fn tick_once() {
    let s = shared();
    let resolved = current_policy();
    let lease_pids = s.broker.lock().map(|b| b.lease_pids()).unwrap_or_default();
    let now = now_s();
    let detail = {
        let mut tr = s.tracker.lock().unwrap_or_else(|e| e.into_inner());
        // Host pause (D7) arrives with Phase 7's desired payload; until then it
        // is clear.
        facts::collect(&Roots::host(), own_uid(), &lease_pids, &mut tr, false, now)
    };
    let mut changed = false;
    if let Ok(mut b) = s.broker.lock() {
        changed |= b.reap(pid_alive, now) > 0;
        changed |= b.shadow_step(&detail.facts, &resolved.policy, now) > 0;
        if changed {
            persist_all(&b, &resolved, &detail, now);
        }
    }
    if let Ok(mut f) = s.facts.lock() {
        *f = Some(detail);
    }
    if let Ok(mut p) = s.policy.lock() {
        *p = resolved;
    }
    if changed {
        s.changed.notify_waiters();
    }
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

/// Routes contributed to the runner's main router; also starts the tick.
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

/// 503 when this runner does not run the broker, so the wrapper degrades.
fn gate() -> Result<Level, Resp> {
    if !cfg!(test) && !crate::instance::owns_shared_root_state() {
        return Err(err(
            StatusCode::SERVICE_UNAVAILABLE,
            "BUILD_ADMISSION_NOT_PRIMARY",
            "this runner instance does not run the build-admission broker; use the degraded arm",
        ));
    }
    let level = shared()
        .policy
        .lock()
        .map(|p| p.level)
        .unwrap_or(Level::Observe);
    if level == Level::Off {
        return Err(err(
            StatusCode::SERVICE_UNAVAILABLE,
            "BUILD_ADMISSION_OFF",
            "the build-admission broker is off on this host; use the degraded arm",
        ));
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
    })
}

fn facts_now() -> FactsDetail {
    let s = shared();
    if let Some(f) = s.facts.lock().ok().and_then(|f| f.clone()) {
        return f;
    }
    let pids = s.broker.lock().map(|b| b.lease_pids()).unwrap_or_default();
    let mut tr = s.tracker.lock().unwrap_or_else(|e| e.into_inner());
    facts::collect(&Roots::host(), own_uid(), &pids, &mut tr, false, now_s())
}

async fn open_ticket(Json(req): Json<TicketRequest>) -> Resp {
    let level = match gate() {
        Ok(l) => l,
        Err(e) => return e,
    };
    if req.output_dir.trim().is_empty() || req.repo.trim().is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "INVALID_REQUEST",
            "repo and output_dir are required",
        );
    }
    let detail = tokio::task::spawn_blocking(facts_now).await;
    let Ok(detail) = detail else {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "FACTS_FAILED",
            "could not read host facts",
        );
    };
    let mut raw = [0u8; 32];
    rand::rng().fill_bytes(&mut raw);
    let secret = hex::encode(raw);
    let id = uuid::Uuid::now_v7().to_string();
    let start = pid_start(req.pid);
    let s = shared();
    let policy = s.policy.lock().map(|p| p.policy).unwrap_or_default();
    let body = {
        let Ok(mut b) = s.broker.lock() else {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "LEDGER_POISONED",
                "build-admission ledger unavailable",
            );
        };
        let rec = b.open(
            id,
            hash_secret(&secret),
            req,
            start,
            &detail.facts,
            &policy,
            now_s(),
        );
        let mut v = view(rec, level);
        v["secret"] = Value::String(secret);
        if let Ok(p) = s.policy.lock() {
            persist_all(&b, &p, &detail, now_s());
        }
        v
    };
    s.changed.notify_waiters();
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
        Err(e) => return e,
    };
    let Some(secret) = secret_of(&headers) else {
        return access_err(AccessError::BadSecret);
    };
    // `wait` is accepted and bounded now so the wrapper's poll loop is final;
    // observe grants at open, so there is never anything to wait for and the
    // poll answers at once. The enforcing broker (Phase 4) holds it on
    // `changed` up to the bound.
    let _bounded_wait_s = q.wait.unwrap_or(0).min(MAX_WAIT_S);
    let Ok(b) = shared().broker.lock() else {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "LEDGER_POISONED",
            "build-admission ledger unavailable",
        );
    };
    match b.authorize(&id, &secret) {
        Ok(r) => (StatusCode::OK, Json(ApiResponse::success(view(r, level)))),
        Err(e) => access_err(e),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseBody {
    #[serde(default)]
    exit_code: Option<i32>,
    reason: ReleaseReason,
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
        Err(e) => return e,
    };
    let Some(secret) = secret_of(&headers) else {
        return access_err(AccessError::BadSecret);
    };
    let s = shared();
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1);
    let out = {
        let Ok(mut b) = s.broker.lock() else {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "LEDGER_POISONED",
                "build-admission ledger unavailable",
            );
        };
        let v = match b.release(
            &id,
            &secret,
            body.reason,
            body.exit_code,
            body.peak_anon_bytes,
            cpus,
            now_s(),
        ) {
            Ok(r) => view(r, level),
            Err(e) => return access_err(e),
        };
        let detail = s.facts.lock().ok().and_then(|f| f.clone());
        if let (Some(d), Ok(p)) = (detail, s.policy.lock()) {
            persist_all(&b, &p, &d, now_s());
        }
        v
    };
    s.changed.notify_waiters();
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
    tickets: Vec<Value>,
}

async fn state() -> Resp {
    let primary = cfg!(test) || crate::instance::owns_shared_root_state();
    if !primary {
        return err(
            StatusCode::SERVICE_UNAVAILABLE,
            "BUILD_ADMISSION_NOT_PRIMARY",
            "this runner instance does not run the build-admission broker",
        );
    }
    let s = shared();
    let p = s
        .policy
        .lock()
        .map(|p| p.clone())
        .unwrap_or_else(|_| current_policy());
    let facts = s.facts.lock().ok().and_then(|f| f.clone());
    let mut notes = p.notes.clone();
    notes.extend(s.notes.lock().map(|n| n.clone()).unwrap_or_default());
    let now = now_s();
    let (running, queued, oldest, tickets) = match s.broker.lock() {
        Ok(b) => (
            b.records
                .values()
                .filter(|r| !r.state.is_terminal())
                .count(),
            b.records
                .values()
                .filter(|r| r.shadow == super::broker::Shadow::Queued)
                .count(),
            b.oldest_shadow_wait_s(now),
            b.records
                .values()
                .rev()
                .take(50)
                .map(|r| view(r, p.level))
                .collect(),
        ),
        Err(_) => (0, 0, None, Vec::new()),
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
        let v = serde_json::from_slice(&bytes).unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into()));
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

    /// Open → poll → release round trip; wrong or missing secret refused;
    /// the state view never carries a secret.
    #[tokio::test]
    async fn ticket_round_trip_with_secrets() {
        let _amb = crate::test_env::isolated_ambient();
        let (st, v) = call(post_json(
            "/build-admission/tickets",
            json!({"repo":"qontinui-coord","subcommand":"check","output_dir":"/t/debug",
                   "target_dir_kind":"shared_warm","pid":std::process::id()}),
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
        let (st, v) = call(post_json(
            &rel,
            json!({"reason":"exit","exit_code":0}),
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
    }
}
