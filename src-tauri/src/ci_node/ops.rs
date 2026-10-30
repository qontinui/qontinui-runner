//! Coord-dispatched remote ops tasks (plan
//! `2026-10-19-ci-nodes-run-the-tauri-runner-always-on-as-ci-node-role`
//! Phase 4, decision D4) - the runner half.
//!
//! Coord publishes `events.ops.task_requested.<device_id>` on the `device_ci`
//! socket this module's sibling [`super::subscription`] already holds. The
//! payload names a task id and a pinned commit of the ops repo
//! (`qontinui-claude-config`); it is NEVER a command. This module:
//!
//! 1. ignores the channel entirely unless `QONTINUI_OPS_TASKS_ENABLED` is
//!    `1`/`true` (dark by default),
//! 2. refuses any ops repo other than the configured one, so coord cannot point
//!    the runner at a repo of its choosing,
//! 3. fetches that repo at the pinned sha, reads `ops/tasks.toml` FROM THAT
//!    CHECKOUT and looks the task id up AGAIN - an id absent from the allowlist
//!    is refused here whatever coord said - and re-validates the arguments
//!    against the task's schema,
//! 4. runs the script the ALLOWLIST names (the payload's script path is only
//!    cross-checked) in an `ops-<id>` child cgroup on Linux, under the task's
//!    timeout, with arguments passed only as `OPS_ARG_*` environment variables
//!    (never argv, never a shell), and
//!    The runner never runs anything as root: a `needs_root` task still runs as
//!    the runner's user and its script escalates itself through the documented
//!    sudoers entry (`ops/README.md` in qontinui-claude-config).
//! 5. posts the result to `POST /coord/ops/tasks/:id/result`.
//!
//! Non-Linux hosts, and Linux hosts with no writable delegated cgroup, are
//! reported EXPLICITLY in the result log (`containment=...`) rather than
//! silently run uncontained; a platform with no scripts to run (Windows) fails
//! the task with that reason.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use tracing::{debug, info, warn};

/// Env flag. Exactly `1`/`true` (case-insensitive, trimmed) enables.
const FLAG_ENV: &str = "QONTINUI_OPS_TASKS_ENABLED";
/// The only repo ops scripts are ever fetched from (overridable for forks).
const OPS_REPO_ENV: &str = "QONTINUI_OPS_TASKS_REPO";
const DEFAULT_OPS_REPO: &str = "qontinui/qontinui-claude-config";
const ALLOWLIST_PATH: &str = "ops/tasks.toml";
const LOG_TAIL_MAX_BYTES: usize = 64 * 1024;
const FETCH_TIMEOUT: Duration = Duration::from_secs(300);
const LOCAL_GIT_TIMEOUT: Duration = Duration::from_secs(60);
/// Remember this many task ids so a re-delivered frame never re-runs a task.
const SEEN_CAP: usize = 256;

fn flag(raw: Option<&str>) -> bool {
    matches!(
        raw.map(|s| s.trim().to_ascii_lowercase()).as_deref(),
        Some("1") | Some("true")
    )
}

/// Whether the ops channel is honoured on this runner.
pub(crate) fn enabled() -> bool {
    flag(std::env::var(FLAG_ENV).ok().as_deref())
}

fn ops_repo() -> String {
    std::env::var(OPS_REPO_ENV).unwrap_or_else(|_| DEFAULT_OPS_REPO.to_string())
}

/// `events.ops.task_requested.<device_id>` payload (coord `ops_tasks::post_task`).
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct OpsTaskPayload {
    pub ops_task_id: String,
    pub task_id: String,
    pub script_repo: String,
    #[serde(default)]
    pub script_path: String,
    pub script_sha: String,
    #[serde(default)]
    pub args: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    /// RFC 3339; an unparseable value is treated as no expiry bound.
    pub expires_at: Option<String>,
}

// ---------------------------------------------------------------------------
// Allowlist - pure (mirror of coord's `ops_tasks::resolve_task`)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
struct Allowlist {
    schema_version: u32,
    #[serde(default)]
    task: Vec<TaskDef>,
}

#[derive(Debug, Clone, Deserialize)]
struct TaskDef {
    id: String,
    script: String,
    #[serde(default)]
    needs_root: bool,
    timeout_secs: u64,
    #[serde(default)]
    args: Vec<ArgDef>,
}

#[derive(Debug, Clone, Deserialize)]
struct ArgDef {
    name: String,
    #[serde(rename = "type")]
    ty: String,
    #[serde(default)]
    values: Vec<String>,
    #[serde(default)]
    min: Option<i64>,
    #[serde(default)]
    max: Option<i64>,
    #[serde(default)]
    required: bool,
    #[serde(default)]
    default: Option<toml::Value>,
}

/// A task that passed every runner-side check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Resolved {
    pub script_path: String,
    pub needs_root: bool,
    pub timeout_secs: u64,
    /// `OPS_ARG_<NAME>` -> value.
    pub env: BTreeMap<String, String>,
}

fn id_ok(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn arg_name_ok(n: &str) -> bool {
    n.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
        && n.len() <= 32
        && n.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

fn arg_value(a: &ArgDef, v: &serde_json::Value) -> Result<String, String> {
    match a.ty.as_str() {
        "enum" => {
            let s = v.as_str().ok_or("must be a string")?;
            if a.values.iter().any(|x| x == s) {
                Ok(s.to_string())
            } else {
                Err(format!("{s:?} is not one of {:?}", a.values))
            }
        }
        "int" => {
            let n = v.as_i64().ok_or("must be an integer")?;
            if a.min.is_some_and(|m| n < m) || a.max.is_some_and(|m| n > m) {
                return Err(format!("{n} is outside the allowed range"));
            }
            Ok(n.to_string())
        }
        "bool" => Ok(v.as_bool().ok_or("must be a boolean")?.to_string()),
        other => Err(format!("unknown arg type {other:?}")),
    }
}

fn default_json(a: &ArgDef) -> Option<serde_json::Value> {
    match (a.ty.as_str(), a.default.as_ref()?) {
        ("enum", toml::Value::String(s)) => Some(serde_json::json!(s)),
        ("int", toml::Value::Integer(i)) => Some(serde_json::json!(i)),
        ("bool", toml::Value::Boolean(b)) => Some(serde_json::json!(b)),
        _ => None,
    }
}

/// Look `task_id` up in the allowlist text read from the PINNED checkout and
/// validate `args`. Err is a refusal reason.
pub(crate) fn resolve(
    allowlist_text: &str,
    task_id: &str,
    args: &BTreeMap<String, serde_json::Value>,
) -> Result<Resolved, String> {
    let list: Allowlist =
        toml::from_str(allowlist_text).map_err(|e| format!("allowlist unparseable: {e}"))?;
    if list.schema_version != 1 {
        return Err(format!(
            "unsupported allowlist schema_version {}",
            list.schema_version
        ));
    }
    let task = list
        .task
        .iter()
        .find(|t| t.id == task_id)
        .ok_or_else(|| format!("task {task_id:?} is not in {ALLOWLIST_PATH}"))?;
    if !id_ok(&task.id)
        || !task.script.starts_with("ops/scripts/")
        || task.script.contains("..")
        || task.script.contains('\\')
        || !(1..=3600).contains(&task.timeout_secs)
    {
        return Err(format!("task {task_id:?} has an invalid allowlist entry"));
    }
    for a in &task.args {
        if !arg_name_ok(&a.name) {
            return Err(format!("task {task_id:?} has a bad arg name {:?}", a.name));
        }
    }
    for k in args.keys() {
        if !task.args.iter().any(|a| &a.name == k) {
            return Err(format!("unknown arg {k:?} for task {task_id}"));
        }
    }
    let mut env = BTreeMap::new();
    for a in &task.args {
        match args.get(&a.name).cloned().or_else(|| default_json(a)) {
            Some(v) => {
                let s = arg_value(a, &v).map_err(|e| format!("arg {}: {e}", a.name))?;
                env.insert(format!("OPS_ARG_{}", a.name.to_ascii_uppercase()), s);
            }
            None if a.required => return Err(format!("missing required arg {}", a.name)),
            None => {}
        }
    }
    Ok(Resolved {
        script_path: task.script.clone(),
        needs_root: task.needs_root,
        timeout_secs: task.timeout_secs,
        env,
    })
}

/// What to do with an inbound frame, before any I/O.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PreDecision {
    /// Flag off, expired, or already seen: do nothing, post nothing.
    Ignore(&'static str),
    /// Malformed or from the wrong repo: post a `failed` result with this reason.
    Refuse(String),
    /// Proceed to fetch and resolve.
    Proceed,
}

/// PURE pre-flight over the payload: gate on the flag, then on shape.
pub(crate) fn pre_decide(
    p: &OpsTaskPayload,
    enabled: bool,
    configured_repo: &str,
    now: chrono::DateTime<chrono::Utc>,
    already_seen: bool,
) -> PreDecision {
    if !enabled {
        return PreDecision::Ignore("ops tasks disabled on this runner");
    }
    if uuid::Uuid::parse_str(&p.ops_task_id).is_err() {
        return PreDecision::Ignore("ops_task_id is not a uuid");
    }
    if already_seen {
        return PreDecision::Ignore("duplicate delivery");
    }
    if p.expires_at.as_deref().is_some_and(|e| {
        chrono::DateTime::parse_from_rfc3339(e).is_ok_and(|t| t.with_timezone(&chrono::Utc) < now)
    }) {
        return PreDecision::Ignore("task expired before delivery");
    }
    if p.script_repo != configured_repo {
        return PreDecision::Refuse(format!(
            "script_repo {:?} is not this runner's ops repo",
            p.script_repo
        ));
    }
    let sha = &p.script_sha;
    if sha.len() != 40
        || !sha
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return PreDecision::Refuse("script_sha is not a full lowercase sha".to_string());
    }
    PreDecision::Proceed
}

fn tail_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut start = s.len() - max;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    s.get(start..).unwrap_or("").to_string()
}

// ---------------------------------------------------------------------------
// Fetch + run
// ---------------------------------------------------------------------------

async fn fetch_at_sha(dir: &Path, repo: &str, sha: &str) -> Result<(), String> {
    use super::checkout::run_git;
    run_git(dir, &["init", "-q"], LOCAL_GIT_TIMEOUT).await?;
    let url = format!("https://github.com/{repo}.git");
    run_git(
        dir,
        &["fetch", "-q", "--depth", "1", &url, sha],
        FETCH_TIMEOUT,
    )
    .await?;
    run_git(
        dir,
        &["checkout", "-q", "--detach", "FETCH_HEAD"],
        LOCAL_GIT_TIMEOUT,
    )
    .await?;
    let head = run_git(dir, &["rev-parse", "HEAD"], LOCAL_GIT_TIMEOUT).await?;
    if head.trim() != sha {
        return Err(format!("fetched {} but pinned {sha}", head.trim()));
    }
    Ok(())
}

struct RunOutcome {
    exit_code: Option<i32>,
    timed_out: bool,
    log: String,
    containment: String,
}

#[cfg(target_os = "linux")]
mod cgroup {
    use std::path::PathBuf;

    /// The runner's own cgroup directory (cgroup v2), if readable.
    pub(super) fn own_dir() -> Option<PathBuf> {
        let s = std::fs::read_to_string("/proc/self/cgroup").ok()?;
        let line = s.lines().find(|l| l.starts_with("0::"))?;
        let rel = line.trim_start_matches("0::").trim_start_matches('/');
        let p = PathBuf::from("/sys/fs/cgroup").join(rel);
        p.is_dir().then_some(p)
    }

    /// Create `ops-<id>` under the runner's delegated subtree.
    pub(super) fn create(id: &str) -> Result<PathBuf, String> {
        let base = own_dir().ok_or("no cgroup v2 hierarchy for this process")?;
        let dir = base.join(format!("ops-{id}"));
        std::fs::create_dir(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
        Ok(dir)
    }

    /// Kill everything in the cgroup (cgroup.kill, kernel >= 5.14; else walk procs).
    pub(super) fn kill_all(dir: &std::path::Path) {
        if std::fs::write(dir.join("cgroup.kill"), "1").is_ok() {
            return;
        }
        if let Ok(procs) = std::fs::read_to_string(dir.join("cgroup.procs")) {
            for pid in procs.lines().filter_map(|l| l.trim().parse::<i32>().ok()) {
                // SAFETY: plain kill(2) on a pid read from the task's own cgroup.
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        }
    }

    /// Remove the (emptied) cgroup, retrying briefly while the kernel reaps.
    pub(super) async fn remove(dir: &std::path::Path) {
        for _ in 0..20 {
            if std::fs::remove_dir(dir).is_ok() || !dir.exists() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        tracing::warn!("ops: could not remove cgroup {}", dir.display());
    }
}

#[cfg(unix)]
async fn run_script(
    repo_dir: &Path,
    script: &str,
    env: &BTreeMap<String, String>,
    timeout: Duration,
    id: &str,
) -> RunOutcome {
    use std::process::Stdio;
    use tokio::io::AsyncReadExt;

    #[allow(unused_mut)]
    let mut containment = "none (non-Linux host)".to_string();
    #[cfg(target_os = "linux")]
    let cg: Option<std::path::PathBuf> = match cgroup::create(id) {
        Ok(d) => {
            containment = format!("cgroup {}", d.display());
            Some(d)
        }
        Err(e) => {
            containment = format!("none ({e}); process-group kill only");
            None
        }
    };
    #[cfg(not(target_os = "linux"))]
    let _ = id;

    let build = |with_cg: bool| {
        let mut cmd = crate::process_helpers::tokio_no_window("bash");
        cmd.arg(script)
            .current_dir(repo_dir)
            .env_clear()
            .env(
                "PATH",
                std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into()),
            )
            .env("HOME", std::env::var("HOME").unwrap_or_default())
            .env("OPS_TASK_ID", id)
            .envs(env.iter())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .process_group(0);
        #[cfg(target_os = "linux")]
        if with_cg {
            if let Some(dir) = &cg {
                use std::ffi::CString;
                let procs =
                    CString::new(dir.join("cgroup.procs").to_string_lossy().as_bytes()).ok();
                if let Some(procs) = procs {
                    // SAFETY: only async-signal-safe libc calls (open/write/close)
                    // between fork and exec; "0" means "the calling process".
                    unsafe {
                        cmd.pre_exec(move || {
                            let fd = libc::open(procs.as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC);
                            if fd < 0 {
                                return Err(std::io::Error::last_os_error());
                            }
                            let n = libc::write(fd, b"0".as_ptr().cast(), 1);
                            libc::close(fd);
                            if n != 1 {
                                return Err(std::io::Error::last_os_error());
                            }
                            Ok(())
                        });
                    }
                }
            }
        }
        #[cfg(not(target_os = "linux"))]
        let _ = with_cg;
        cmd
    };
    #[allow(unused_mut)]
    let mut cmd = build(cfg!(target_os = "linux"));
    #[allow(unused_mut)]
    let mut spawned = cmd.spawn();
    #[cfg(target_os = "linux")]
    if spawned.is_err() && cg.is_some() {
        containment = format!(
            "none (could not enter {}); process-group kill only",
            containment
        );
        spawned = build(false).spawn();
    }
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => {
            #[cfg(target_os = "linux")]
            if let Some(dir) = &cg {
                cgroup::remove(dir).await;
            }
            return RunOutcome {
                exit_code: None,
                timed_out: false,
                log: format!("spawn failed: {e}"),
                containment,
            };
        }
    };
    let pgid = child.id().map(|p| p as i32);

    // Merge both streams into one bounded buffer, keeping the END.
    let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let mut readers = Vec::new();
    for stream in [
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>),
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>),
    ]
    .into_iter()
    .flatten()
    {
        let buf = buf.clone();
        readers.push(tokio::spawn(async move {
            let mut s = stream;
            let mut chunk = [0u8; 8192];
            while let Ok(n) = s.read(&mut chunk).await {
                if n == 0 {
                    break;
                }
                if let Ok(mut b) = buf.lock() {
                    b.extend_from_slice(&chunk[..n]);
                    if b.len() > LOG_TAIL_MAX_BYTES * 2 {
                        let cut = b.len() - LOG_TAIL_MAX_BYTES;
                        b.drain(..cut);
                    }
                }
            }
        }));
    }

    let waited = tokio::time::timeout(timeout, child.wait()).await;
    let (exit_code, timed_out) = match waited {
        Ok(Ok(status)) => (status.code(), false),
        Ok(Err(_)) => (None, false),
        Err(_) => {
            // Kill the whole group while the leader is still unreaped (its pid
            // cannot have been recycled yet), then reap it.
            if let Some(pg) = pgid {
                // SAFETY: kill(2) on the task's own, still-live process group.
                unsafe { libc::kill(-pg, libc::SIGKILL) };
            }
            let _ = child.start_kill();
            let _ = child.wait().await;
            (None, true)
        }
    };
    #[cfg(target_os = "linux")]
    let cg_active = cg.is_some();
    #[cfg(not(target_os = "linux"))]
    let cg_active = false;
    // Whatever the exit path, nothing the script started may outlive its task:
    // kill the cgroup and the process group BEFORE joining the readers, so a
    // daemon holding the pipes cannot stall them.
    #[cfg(target_os = "linux")]
    if let Some(dir) = &cg {
        cgroup::kill_all(dir);
    }
    // Without a cgroup the process group is the only containment, so a script
    // that backgrounded a daemon and exited 0 is still reaped here. The kernel
    // does not recycle a pid while it is the id of a live process group; if the
    // group is empty this is an ESRCH no-op. (The timeout path already killed it.)
    if !timed_out && !cg_active {
        if let Some(pg) = pgid {
            // SAFETY: kill(2) on the task's own process group.
            unsafe { libc::kill(-pg, libc::SIGKILL) };
        }
    }
    for mut r in readers {
        if tokio::time::timeout(Duration::from_secs(5), &mut r)
            .await
            .is_err()
        {
            // A daemon still holds the pipe: stop reading rather than leak the task.
            r.abort();
        }
    }
    #[cfg(target_os = "linux")]
    if let Some(dir) = &cg {
        cgroup::remove(dir).await;
    }
    let log = buf
        .lock()
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    RunOutcome {
        exit_code,
        timed_out,
        log,
        containment,
    }
}

#[cfg(not(unix))]
async fn run_script(
    _repo_dir: &Path,
    _script: &str,
    _env: &BTreeMap<String, String>,
    _timeout: Duration,
    _id: &str,
) -> RunOutcome {
    RunOutcome {
        exit_code: None,
        timed_out: false,
        log: "ops tasks are not supported on this platform".to_string(),
        containment: "unsupported platform".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Result posting
// ---------------------------------------------------------------------------

async fn post_result(id: &str, state: &str, exit_code: Option<i32>, log: &str) {
    let Some(base) = qontinui_runner_lib::profiles::connected_coord_base() else {
        warn!("ops: no coord base; result for {id} not posted");
        return;
    };
    let url = format!("{}/coord/ops/tasks/{id}/result", base.trim_end_matches('/'));
    let body = serde_json::json!({
        "state": state,
        "exit_code": exit_code,
        "log_tail": tail_str(log, LOG_TAIL_MAX_BYTES),
    });
    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
    else {
        return;
    };
    for (attempt, wait) in [0u64, 2, 4, 8, 16].into_iter().enumerate() {
        if wait > 0 {
            tokio::time::sleep(Duration::from_secs(wait)).await;
        }
        let Some(bearer) = crate::coord_mcp::read_usable_device_jwt().await else {
            debug!("ops: no usable device JWT (attempt {attempt})");
            continue;
        };
        // coord-auth-exempt(device-jwt-required): coord authorises the result
        // against the task's assignee device and tenant.
        match client
            .post(&url)
            .bearer_auth(bearer)
            .json(&body)
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => return,
            // Only a definitive refusal ends the loop: a 401 (device JWT expired
            // mid-task; the next attempt re-reads it), 408 and 429 are retried.
            Ok(r) if matches!(r.status().as_u16(), 400 | 403 | 404 | 409 | 422) => {
                warn!("ops: result for {id} refused by coord: {}", r.status());
                return;
            }
            Ok(r) => debug!("ops: result POST {} (attempt {attempt})", r.status()),
            Err(e) => debug!("ops: result POST error {e} (attempt {attempt})"),
        }
    }
    warn!("ops: result for {id} not delivered after retries");
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

static SEEN: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
static ONE_AT_A_TIME: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
/// Tasks parked behind the running one; more than this are refused so a burst
/// cannot pile up unbounded waiters (each could otherwise run long after expiry).
const MAX_WAITING: usize = 8;
static WAITING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn is_seen(id: &str) -> bool {
    SEEN.lock()
        .map(|s| s.iter().any(|x| x == id))
        .unwrap_or(false)
}

fn mark_seen(id: &str) {
    if let Ok(mut seen) = SEEN.lock() {
        if seen.len() >= SEEN_CAP {
            seen.remove(0);
        }
        seen.push(id.to_string());
    }
}

/// Handle one `events.ops.task_requested.<this device>` payload. Never blocks
/// the subscribe loop: the work runs on its own task.
pub(crate) fn handle(inner: serde_json::Value) {
    handle_with(inner, enabled());
}

/// [`handle`] with the flag passed in, so the dark path is testable. When
/// `enabled` is false nothing is parsed, remembered, spawned or posted.
pub(crate) fn handle_with(inner: serde_json::Value, enabled: bool) {
    if !enabled {
        debug!("ops: task_requested ignored (QONTINUI_OPS_TASKS_ENABLED is off)");
        return;
    }
    let payload: OpsTaskPayload = match serde_json::from_value(inner) {
        Ok(p) => p,
        Err(e) => {
            warn!("ops: task_requested payload did not parse: {e}");
            return;
        }
    };
    let decision = pre_decide(
        &payload,
        true,
        &ops_repo(),
        chrono::Utc::now(),
        is_seen(&payload.ops_task_id),
    );
    // Only a well-formed, in-time, first delivery is remembered; a garbage id
    // cannot evict a real entry. The set is in memory: a redelivery after a
    // runner restart can re-run a task, which coord bounds by expires_at and by
    // the ledger row's state (a terminal row refuses a second result).
    if !matches!(decision, PreDecision::Ignore(_)) {
        mark_seen(&payload.ops_task_id);
    }
    match decision {
        PreDecision::Ignore(why) => debug!("ops: {} ignored: {why}", payload.ops_task_id),
        PreDecision::Refuse(why) => {
            warn!("ops: {} refused: {why}", payload.ops_task_id);
            tokio::spawn(async move {
                post_result(
                    &payload.ops_task_id,
                    "failed",
                    None,
                    &format!("refused: {why}"),
                )
                .await;
            });
        }
        PreDecision::Proceed => {
            if WAITING.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= MAX_WAITING {
                WAITING.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                tokio::spawn(async move {
                    post_result(
                        &payload.ops_task_id,
                        "failed",
                        None,
                        "refused: runner ops queue is full",
                    )
                    .await;
                });
                return;
            }
            tokio::spawn(async move { execute(payload).await });
        }
    }
}

async fn fail(id: &str, why: String) {
    warn!("ops: {id} failed: {why}");
    post_result(id, "failed", None, &why).await;
}

async fn execute(p: OpsTaskPayload) {
    let id = p.ops_task_id.clone();
    let permit = ONE_AT_A_TIME.acquire().await;
    WAITING.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    let Ok(permit_guard) = permit else { return };
    // The wait behind a running task can outlast the task's own expiry.
    if p.expires_at.as_deref().is_some_and(|e| {
        chrono::DateTime::parse_from_rfc3339(e)
            .is_ok_and(|t| t.with_timezone(&chrono::Utc) < chrono::Utc::now())
    }) {
        return fail(&id, "expired while queued behind another ops task".into()).await;
    }
    post_result(&id, "running", None, "").await;

    let Ok(tmp) = tempfile::Builder::new().prefix("qontinui-ops-").tempdir() else {
        return fail(&id, "could not create a work directory".into()).await;
    };
    let dir = tmp.path();
    if let Err(e) = fetch_at_sha(dir, &p.script_repo, &p.script_sha).await {
        return fail(&id, format!("fetch failed: {e}")).await;
    }
    let Ok(text) = std::fs::read_to_string(dir.join(ALLOWLIST_PATH)) else {
        return fail(&id, format!("{ALLOWLIST_PATH} missing at {}", p.script_sha)).await;
    };
    let resolved = match resolve(&text, &p.task_id, &p.args) {
        Ok(r) => r,
        Err(why) => return fail(&id, format!("refused: {why}")).await,
    };
    if !p.script_path.is_empty() && p.script_path != resolved.script_path {
        return fail(
            &id,
            format!(
                "refused: payload script {:?} differs from the allowlist's {:?}",
                p.script_path, resolved.script_path
            ),
        )
        .await;
    }
    let script_file = dir.join(&resolved.script_path);
    let plain_file = std::fs::symlink_metadata(&script_file).is_ok_and(|m| m.is_file());
    if !plain_file {
        return fail(
            &id,
            format!(
                "script {} is missing (or not a regular file) at the pinned sha",
                resolved.script_path
            ),
        )
        .await;
    }
    info!(
        "ops: running {} ({}) for {id}",
        p.task_id, resolved.script_path
    );
    let out = run_script(
        dir,
        &resolved.script_path,
        &resolved.env,
        Duration::from_secs(resolved.timeout_secs),
        &id,
    )
    .await;
    let header = format!(
        "# ops task {} sha={} containment={} needs_root={}\n",
        p.task_id, p.script_sha, out.containment, resolved.needs_root
    );
    let (state, note) = match (out.exit_code, out.timed_out) {
        (Some(0), false) => ("succeeded", String::new()),
        (_, true) => (
            "failed",
            format!(
                "\n# timed out after {}s and was killed\n",
                resolved.timeout_secs
            ),
        ),
        (c, false) => ("failed", format!("\n# exit {c:?}\n")),
    };
    // The script is done: free the slot before the (possibly retrying) post.
    drop(permit_guard);
    post_result(
        &id,
        state,
        out.exit_code,
        &format!("{header}{}{note}", out.log),
    )
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &str = r#"
schema_version = 1

[[task]]
id = "report-ci-slice-health"
script = "ops/scripts/report-ci-slice-health.sh"
timeout_secs = 60

[[task]]
id = "reinstall-orphan-reaper"
script = "ops/scripts/reinstall-orphan-reaper.sh"
needs_root = true
timeout_secs = 300

[[task.args]]
name = "mode"
type = "enum"
values = ["install", "check"]
default = "install"
"#;

    fn payload(task: &str) -> OpsTaskPayload {
        OpsTaskPayload {
            ops_task_id: uuid::Uuid::now_v7().to_string(),
            task_id: task.to_string(),
            script_repo: DEFAULT_OPS_REPO.to_string(),
            script_path: String::new(),
            script_sha: "a".repeat(40),
            args: BTreeMap::new(),
            expires_at: None,
        }
    }

    #[test]
    fn the_runner_refuses_a_task_id_not_in_the_allowlist() {
        let e = resolve(LIST, "format-the-disk", &BTreeMap::new()).unwrap_err();
        assert!(e.contains("not in ops/tasks.toml"), "{e}");
        for near in ["", "report-ci-slice-health ", "REPORT-CI-SLICE-HEALTH"] {
            assert!(resolve(LIST, near, &BTreeMap::new()).is_err(), "{near:?}");
        }
    }

    #[test]
    fn an_allowlisted_task_resolves_with_validated_env_args() {
        let r = resolve(LIST, "reinstall-orphan-reaper", &BTreeMap::new()).unwrap();
        assert_eq!(
            r.env.get("OPS_ARG_MODE").map(String::as_str),
            Some("install")
        );
        assert!(r.needs_root);
        let mut a = BTreeMap::new();
        a.insert("mode".to_string(), serde_json::json!("check"));
        assert_eq!(
            resolve(LIST, "reinstall-orphan-reaper", &a).unwrap().env["OPS_ARG_MODE"],
            "check"
        );
        a.insert("mode".to_string(), serde_json::json!("rm -rf /"));
        assert!(resolve(LIST, "reinstall-orphan-reaper", &a).is_err());
        let mut extra = BTreeMap::new();
        extra.insert("x".to_string(), serde_json::json!(1));
        assert!(resolve(LIST, "report-ci-slice-health", &extra).is_err());
    }

    #[test]
    fn a_script_outside_ops_scripts_is_refused_even_if_allowlisted() {
        let bad =
            "schema_version = 1\n[[task]]\nid=\"a\"\nscript=\"scripts/x.sh\"\ntimeout_secs=5\n";
        assert!(resolve(bad, "a", &BTreeMap::new()).is_err());
        let trav = "schema_version = 1\n[[task]]\nid=\"a\"\nscript=\"ops/scripts/../../x.sh\"\ntimeout_secs=5\n";
        assert!(resolve(trav, "a", &BTreeMap::new()).is_err());
    }

    #[test]
    fn the_channel_is_ignored_when_the_flag_is_off() {
        assert!(!flag(None));
        assert!(!flag(Some("0")));
        assert!(flag(Some(" TRUE ")));
        let p = payload("report-ci-slice-health");
        assert_eq!(
            pre_decide(&p, false, DEFAULT_OPS_REPO, chrono::Utc::now(), false),
            PreDecision::Ignore("ops tasks disabled on this runner")
        );
        // The real gate: with the flag off handle_with parses nothing,
        // remembers nothing and spawns nothing (this test has no runtime, so a
        // spawn would panic).
        let id = uuid::Uuid::now_v7().to_string();
        handle_with(
            serde_json::json!({
                "ops_task_id": id, "task_id": "report-ci-slice-health",
                "script_repo": DEFAULT_OPS_REPO, "script_sha": "a".repeat(40),
            }),
            false,
        );
        assert!(!is_seen(&id));
    }

    #[test]
    fn pre_decide_gates_repo_sha_expiry_and_duplicates() {
        let now = chrono::Utc::now();
        let ok = payload("report-ci-slice-health");
        assert_eq!(
            pre_decide(&ok, true, DEFAULT_OPS_REPO, now, false),
            PreDecision::Proceed
        );
        assert!(matches!(
            pre_decide(&ok, true, DEFAULT_OPS_REPO, now, true),
            PreDecision::Ignore(_)
        ));
        let mut foreign = payload("x");
        foreign.script_repo = "evil/repo".into();
        assert!(matches!(
            pre_decide(&foreign, true, DEFAULT_OPS_REPO, now, false),
            PreDecision::Refuse(_)
        ));
        let mut badsha = payload("x");
        badsha.script_sha = "main".into();
        assert!(matches!(
            pre_decide(&badsha, true, DEFAULT_OPS_REPO, now, false),
            PreDecision::Refuse(_)
        ));
        let mut stale = payload("x");
        stale.expires_at = Some((now - chrono::Duration::seconds(5)).to_rfc3339());
        assert!(matches!(
            pre_decide(&stale, true, DEFAULT_OPS_REPO, now, false),
            PreDecision::Ignore(_)
        ));
        let mut badid = payload("x");
        badid.ops_task_id = "../../etc".into();
        assert!(matches!(
            pre_decide(&badid, true, DEFAULT_OPS_REPO, now, false),
            PreDecision::Ignore(_)
        ));
    }

    #[test]
    fn payload_parses_the_coord_wire_shape() {
        let p: OpsTaskPayload = serde_json::from_value(serde_json::json!({
            "ops_task_id": "0198f2b4-1111-7aaa-bbbb-cccccccccccc",
            "task_id": "report-ci-slice-health",
            "script_repo": "qontinui/qontinui-claude-config",
            "script_path": "ops/scripts/report-ci-slice-health.sh",
            "script_sha": "a".repeat(40),
            "needs_root": false,
            "timeout_secs": 60,
            "args": {"mode": "check"},
            "expires_at": "2026-10-30T12:00:00Z",
        }))
        .unwrap();
        assert_eq!(p.task_id, "report-ci-slice-health");
        assert!(p.expires_at.is_some());
    }

    #[test]
    fn tail_keeps_the_end_on_a_char_boundary() {
        assert_eq!(tail_str("abcdef", 3), "def");
        let s = "a\u{00e9}\u{00e9}\u{00e9}";
        assert!(tail_str(s, 5).len() <= 5);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_script_passes_env_args_captures_output_and_enforces_the_timeout() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("ok.sh"),
            "echo mode=$OPS_ARG_MODE; echo err >&2\n",
        )
        .unwrap();
        let mut env = BTreeMap::new();
        env.insert("OPS_ARG_MODE".to_string(), "check".to_string());
        let out = run_script(
            dir.path(),
            "ok.sh",
            &env,
            Duration::from_secs(20),
            &format!("test-ok-{}", uuid::Uuid::now_v7()),
        )
        .await;
        assert_eq!(out.exit_code, Some(0));
        assert!(
            out.log.contains("mode=check") && out.log.contains("err"),
            "{}",
            out.log
        );
        assert!(!out.containment.is_empty());

        std::fs::write(dir.path().join("slow.sh"), "sleep 30\n").unwrap();
        let t = std::time::Instant::now();
        let out = run_script(
            dir.path(),
            "slow.sh",
            &BTreeMap::new(),
            Duration::from_millis(500),
            &format!("test-slow-{}", uuid::Uuid::now_v7()),
        )
        .await;
        assert!(out.timed_out);
        assert!(t.elapsed() < Duration::from_secs(15));
    }
}
