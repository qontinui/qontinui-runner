//! Step execution for one CI dispatch: checkout → manifest → sequential
//! steps (first failure short-circuits) → result POST → cleanup.
//!
//! Resource posture (plan §4.6 — load-bearing, not polish):
//! - children run at `BELOW_NORMAL_PRIORITY_CLASS` (net-new) combined with
//!   the existing `CREATE_NO_WINDOW`, and are assigned to the global Job
//!   Object (kill-on-close) like every other runner child;
//! - additionally each dispatch gets its OWN Job Object carrying a
//!   job-wide committed-memory limit ([`CI_JOB_MEMORY_LIMIT_BYTES`]) — the
//!   OOM backstop the plan calls load-bearing — plus kill-on-close scoped
//!   to the dispatch (dropping the handle at dispatch end reaps strays);
//! - `CARGO_BUILD_JOBS` **and** the test-concurrency caps are exported by the
//!   executor, sized from the host and bounded by the manifest's `[limits]`
//!   (see [`super::host_sizing`]) — the manifest cannot raise them, and it no
//!   longer has to smuggle them through argv;
//! - cargo artifacts go to a persistent per-repo CI target dir
//!   (`<root>/.ci-target/<repo basename>`) — warm across dispatches, never
//!   contending with the developer's own `target/`.
//!
//! Provisioning happens between the manifest read and the first step, in this
//! order: tools ([`super::tools`]) then siblings ([`super::sibling`]). Both
//! are declared IN the manifest, so neither can run before it is read and
//! validated; both abort the dispatch on failure, because a step that
//! silently ran without its declared tool or sibling produces a verdict that
//! looks like a code failure and is not.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::host_sizing;
use super::manifest::{CiManifest, CiStep};
use super::reporting::{self, LinePusher, ProgressSink, StepSummary};
use super::CiDispatchPayload;

/// Job-wide committed-memory ceiling for one dispatch's process tree
/// (Windows). Deliberately generous — rustc at `CARGO_BUILD_JOBS=1` on this
/// workspace legitimately commits >10 GiB — because this is a runaway
/// backstop, not a tuning knob; the real throttle is the jobs cap.
#[cfg(windows)]
const CI_JOB_MEMORY_LIMIT_BYTES: usize = 32 * 1024 * 1024 * 1024;

/// Per-dispatch Job Object carrying the memory backstop. A no-op unit on
/// non-Windows so call sites stay platform-uniform.
struct CiJob {
    #[cfg(windows)]
    inner: Option<crate::job_object::ScopedMemoryLimitedJob>,
}

impl CiJob {
    fn create() -> Self {
        #[cfg(windows)]
        {
            let inner =
                crate::job_object::ScopedMemoryLimitedJob::create(CI_JOB_MEMORY_LIMIT_BYTES);
            if inner.is_none() {
                warn!(
                    "ci_node: per-dispatch memory-limit job unavailable — \
                     builds run with the global kill-on-close job only"
                );
            }
            Self { inner }
        }
        #[cfg(not(windows))]
        {
            Self {}
        }
    }

    /// Assign a spawned step child to the dispatch job (in addition to the
    /// global job the caller already assigned it to).
    #[cfg_attr(not(windows), allow(unused_variables))]
    fn assign(&self, child: &tokio::process::Child) {
        #[cfg(windows)]
        if let (Some(job), Some(raw)) = (self.inner.as_ref(), child.raw_handle()) {
            job.assign(raw as windows_sys::Win32::Foundation::HANDLE);
        }
    }
}

/// Outcome of one step.
#[derive(Debug, PartialEq, Eq)]
enum StepOutcome {
    Success,
    /// Non-zero exit (carries the human-readable detail already logged).
    Failure,
    Timeout,
    Cancelled,
}

impl StepOutcome {
    fn as_conclusion(&self) -> &'static str {
        match self {
            StepOutcome::Success => "success",
            // A timeout is a failure with a log line explaining why.
            StepOutcome::Failure | StepOutcome::Timeout => "failure",
            StepOutcome::Cancelled => "cancelled",
        }
    }
}

/// Run one dispatch end-to-end. Never panics; always POSTs a result (via
/// the retrying reporter) and always cleans up the worktree it created.
pub(crate) async fn run_dispatch(
    payload: CiDispatchPayload,
    root: PathBuf,
    cancel: CancellationToken,
) {
    let base = {
        let pinned = payload.coord_http_url.trim().trim_end_matches('/');
        if pinned.is_empty() {
            match super::coord_http_base() {
                Some(b) => b,
                None => {
                    warn!(
                        "ci_node: dispatch {} has no coord_http_url and no profile coord base — \
                         cannot report; dropping",
                        payload.dispatch_id
                    );
                    return;
                }
            }
        } else {
            pinned.to_string()
        }
    };

    let sink = ProgressSink::start(base.clone(), payload.dispatch_id.clone(), cancel.clone());
    sink.push(&format!(
        "[ci-node] dispatch {} repo={} sha={} check={}",
        payload.dispatch_id, payload.repo, payload.head_sha, payload.check_name
    ));

    let mut steps_summary: Vec<StepSummary> = Vec::new();
    let started = Instant::now();

    // ── Checkout ──
    sink.push(&format!(
        "[ci-node] fetching {} ({})",
        payload.candidate_ref, payload.fetch_url
    ));
    let checkout_started = Instant::now();
    let worktree = match super::checkout::prepare_worktree(
        &root,
        &payload.repo,
        &payload.dispatch_id,
        &payload.fetch_url,
        &payload.candidate_ref,
        &payload.head_sha,
    )
    .await
    {
        Ok(p) => p,
        Err(e) => {
            sink.push(&format!("[ci-node] checkout failed: {e}"));
            steps_summary.push(StepSummary {
                name: "[setup] checkout".to_string(),
                conclusion: "failure".to_string(),
                duration_secs: checkout_started.elapsed().as_secs(),
            });
            let tail = sink.finish().await;
            reporting::post_result(
                &base,
                &payload.dispatch_id,
                "failure",
                &steps_summary,
                None,
                &tail,
            )
            .await;
            // prepare_worktree may have left a partial worktree behind.
            super::checkout::cleanup_dispatch(&root, &payload.repo, &payload.dispatch_id).await;
            return;
        }
    };
    steps_summary.push(StepSummary {
        name: "[setup] checkout".to_string(),
        conclusion: "success".to_string(),
        duration_secs: checkout_started.elapsed().as_secs(),
    });

    // ── Manifest (from the CHECKED-OUT tree, never coord) ──
    let manifest = match load_manifest(&worktree, &payload.manifest_path) {
        Ok(m) => m,
        Err(e) => {
            sink.push(&format!("[ci-node] manifest rejected: {e}"));
            steps_summary.push(StepSummary {
                name: "[setup] manifest".to_string(),
                conclusion: "failure".to_string(),
                duration_secs: 0,
            });
            let tail = sink.finish().await;
            reporting::post_result(
                &base,
                &payload.dispatch_id,
                "failure",
                &steps_summary,
                None,
                &tail,
            )
            .await;
            super::checkout::cleanup_dispatch(&root, &payload.repo, &payload.dispatch_id).await;
            return;
        }
    };

    // Host-derived caps, bounded by the manifest's [limits] (which are
    // ceilings, never raises). Probed ONCE per dispatch — the answer cannot
    // change mid-build in any way worth re-reading, and the probe is a
    // blocking sysinfo refresh.
    let host = host_sizing::derive(host_sizing::probe());
    let build_jobs = manifest.limits.effective_cargo_build_jobs(host);
    let test_threads = manifest.limits.effective_test_threads(host);
    sink.push(&format!(
        "[ci-node] manifest ok: {} step(s), {} sibling(s), {} tool(s); \
         cargo_build_jobs={build_jobs} test_threads={test_threads} \
         (host sizing: {} / {})",
        manifest.steps.len(),
        manifest.siblings.len(),
        manifest.tools.len(),
        host.cargo_build_jobs,
        host.test_threads
    ));

    // ── Provisioning (tools, then siblings) ──
    let provisioning = provision(&payload, &root, &worktree, &manifest, &sink).await;
    let tool_dirs = match provisioning {
        Ok(dirs) => {
            steps_summary.push(StepSummary {
                name: "[setup] provision".to_string(),
                conclusion: "success".to_string(),
                duration_secs: 0,
            });
            dirs
        }
        Err(e) => {
            sink.push(&format!("[ci-node] provisioning failed: {e}"));
            steps_summary.push(StepSummary {
                name: "[setup] provision".to_string(),
                conclusion: "failure".to_string(),
                duration_secs: 0,
            });
            let tail = sink.finish().await;
            reporting::post_result(
                &base,
                &payload.dispatch_id,
                "failure",
                &steps_summary,
                None,
                &tail,
            )
            .await;
            super::checkout::cleanup_dispatch(&root, &payload.repo, &payload.dispatch_id).await;
            return;
        }
    };

    // Persistent per-repo CI target dir (warm across dispatches).
    let ci_target_dir = root
        .join(".ci-target")
        .join(crate::agent_runtime::local_repo_name(&payload.repo));

    let dispatch_env = DispatchEnv::build(build_jobs, test_threads, &ci_target_dir, &tool_dirs);

    // Per-dispatch memory-backstop job (held across all steps; dropping it
    // at dispatch end kill-on-closes any stray build processes).
    let ci_job = CiJob::create();

    // ── Steps (sequential; first failure short-circuits) ──
    let mut overall = StepOutcome::Success;
    for step in &manifest.steps {
        if cancel.is_cancelled() {
            overall = StepOutcome::Cancelled;
            break;
        }
        sink.push(&format!(
            "[ci-node] ── step {} ── {:?} (timeout {}s)",
            step.name,
            step.command,
            step.effective_timeout_secs()
        ));
        let step_started = Instant::now();
        let outcome = run_step(step, &dispatch_env, &worktree, &ci_job, &sink, &cancel).await;
        let duration = step_started.elapsed().as_secs();
        sink.push(&format!(
            "[ci-node] step {} → {} in {duration}s",
            step.name,
            outcome.as_conclusion()
        ));
        steps_summary.push(StepSummary {
            name: step.name.clone(),
            conclusion: outcome.as_conclusion().to_string(),
            duration_secs: duration,
        });
        if outcome != StepOutcome::Success {
            overall = outcome;
            break;
        }
    }

    let conclusion = match overall {
        StepOutcome::Success => "success",
        StepOutcome::Failure | StepOutcome::Timeout => "failure",
        StepOutcome::Cancelled => "cancelled",
    };
    sink.push(&format!(
        "[ci-node] dispatch {} finished: {conclusion} in {}s",
        payload.dispatch_id,
        started.elapsed().as_secs()
    ));
    info!(
        "ci_node: dispatch {} finished conclusion={conclusion}",
        payload.dispatch_id
    );

    let tail = sink.finish().await;
    reporting::post_result(
        &base,
        &payload.dispatch_id,
        conclusion,
        &steps_summary,
        None,
        &tail,
    )
    .await;
    super::checkout::cleanup_dispatch(&root, &payload.repo, &payload.dispatch_id).await;
}

/// Provision what the manifest declares. Returns the tool directories to
/// prepend to the step PATH.
///
/// Tools first, siblings second: a sibling fetch is the slower and more
/// failure-prone half (network, declaration validation), and there is no
/// reason to pay for it before finding out a declared tool does not exist for
/// this platform.
async fn provision(
    payload: &CiDispatchPayload,
    root: &Path,
    worktree: &Path,
    manifest: &CiManifest,
    sink: &ProgressSink,
) -> Result<Vec<PathBuf>, String> {
    let mut log = |line: String| sink.push(&line);

    let tool_dirs = super::tools::provision(root, &manifest.tools, &mut log).await?;

    let dispatch_root = super::checkout::ci_dispatch_root(root, &payload.dispatch_id);
    let worktree_dir_name = worktree
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    super::sibling::provision(
        &dispatch_root,
        &worktree_dir_name,
        &manifest.siblings,
        &payload.repo,
        payload.pr_number,
        &mut log,
    )
    .await?;

    Ok(tool_dirs)
}

/// Read + validate the manifest from the checked-out tree. The path is
/// repo-relative and structurally constrained (no traversal), then
/// canonicalize+prefix-checked against the worktree.
fn load_manifest(worktree: &Path, manifest_path: &str) -> Result<CiManifest, String> {
    let rel = Path::new(manifest_path);
    if rel.is_absolute()
        || manifest_path.contains(':')
        || rel.components().any(|c| {
            !matches!(
                c,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        })
    {
        return Err(format!(
            "manifest_path {manifest_path:?} must be repo-relative"
        ));
    }
    let full = worktree.join(rel);
    let canonical = full.canonicalize().map_err(|e| {
        format!("manifest {manifest_path:?} not readable in the checked-out tree: {e}")
    })?;
    let canonical_wt = worktree
        .canonicalize()
        .map_err(|e| format!("worktree not canonicalizable: {e}"))?;
    if !canonical.starts_with(&canonical_wt) {
        return Err(format!(
            "manifest_path {manifest_path:?} escapes the worktree"
        ));
    }
    let text = std::fs::read_to_string(&canonical)
        .map_err(|e| format!("read {}: {e}", canonical.display()))?;
    super::manifest::parse_and_validate(&text)
}

/// Resolve a step's working dir inside the worktree (canonicalize +
/// prefix-check — the runtime enforcement behind the manifest's structural
/// validation).
fn resolve_step_cwd(worktree: &Path, step: &CiStep) -> Result<PathBuf, String> {
    let cwd = match &step.working_dir {
        Some(wd) => worktree.join(wd),
        None => worktree.to_path_buf(),
    };
    let canonical = cwd.canonicalize().map_err(|e| {
        format!(
            "working_dir {:?} not present in the tree: {e}",
            step.working_dir
        )
    })?;
    let canonical_wt = worktree
        .canonicalize()
        .map_err(|e| format!("worktree not canonicalizable: {e}"))?;
    if !canonical.starts_with(&canonical_wt) {
        return Err(format!(
            "working_dir {:?} escapes the worktree",
            step.working_dir
        ));
    }
    Ok(canonical)
}

/// Build the tokio Command for a step's argv. On Windows the creation
/// flags combine `CREATE_NO_WINDOW` with `BELOW_NORMAL_PRIORITY_CLASS` so a
/// CI build never steals the foreground from the developer.
fn build_step_command(program: &str, args: &[String]) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args);
    #[cfg(target_os = "windows")]
    {
        #[allow(unused_imports)]
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;
        cmd.creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS);
    }
    cmd
}

/// Spawn a step child, falling back to a `cmd.exe /C` respawn on Windows
/// when the program is a `.cmd`/`.bat` shim (pnpm) that `CreateProcess`
/// cannot launch directly. Argv tokens are metacharacter-validated by the
/// manifest, so the shim path cannot smuggle shell syntax.
fn spawn_step_child(
    step: &CiStep,
    cwd: &Path,
    envs: &[(String, String)],
) -> Result<tokio::process::Child, String> {
    let apply = |cmd: &mut tokio::process::Command| {
        cmd.current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in envs {
            cmd.env(k, v);
        }
    };
    let mut direct = build_step_command(&step.command[0], &step.command[1..]);
    apply(&mut direct);
    match direct.spawn() {
        Ok(child) => Ok(child),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && cfg!(target_os = "windows") => {
            let mut argv: Vec<String> = vec!["/C".to_string()];
            argv.extend(step.command.iter().cloned());
            let mut shim = build_step_command("cmd.exe", &argv);
            apply(&mut shim);
            shim.spawn()
                .map_err(|e2| format!("spawn {:?} (direct: {e}; cmd /C: {e2})", step.command))
        }
        Err(e) => Err(format!("spawn {:?}: {e}", step.command)),
    }
}

/// Kill a step's process tree. On Windows `taskkill /T` takes the whole
/// tree (killing only the direct child would orphan cargo/node under a
/// `cmd.exe` shim until Job-Object close); elsewhere `start_kill` suffices.
async fn kill_step_tree(child: &mut tokio::process::Child) {
    #[cfg(target_os = "windows")]
    {
        if let Some(pid) = child.id() {
            let _ = crate::process_helpers::tokio_no_window("taskkill")
                .args(["/F", "/T", "/PID", &pid.to_string()])
                .output()
                .await;
        }
    }
    let _ = child.start_kill();
    let _ = child.wait().await;
}

/// The env the executor exports to EVERY step of a dispatch, computed once.
///
/// These win over a step's own `env` — the manifest's validation rejects the
/// keys here outright with a pointer to the manifest key that does control
/// them, so "the step set it and it was ignored" is not a state this can
/// reach.
struct DispatchEnv {
    exports: Vec<(String, String)>,
}

impl DispatchEnv {
    fn build(
        build_jobs: u32,
        test_threads: u32,
        ci_target_dir: &Path,
        tool_dirs: &[PathBuf],
    ) -> Self {
        let mut exports = vec![
            // The build-phase cap.
            ("CARGO_BUILD_JOBS".to_string(), build_jobs.to_string()),
            // The TEST-phase cap, exported for both harnesses. This is the
            // half that used to be missing: the incident behind these caps
            // killed the Actions agent in the test phase as well as the build
            // phase, so bounding only the build leaves half the failure mode
            // open. `RUST_TEST_THREADS` bounds libtest (`cargo test`);
            // `NEXTEST_TEST_THREADS` bounds nextest's process-per-test model.
            // Both are exported unconditionally because a manifest may use
            // either harness, and the unused one is inert.
            ("RUST_TEST_THREADS".to_string(), test_threads.to_string()),
            (
                "NEXTEST_TEST_THREADS".to_string(),
                test_threads.to_string(),
            ),
            // Keeps cargo out of the developer's own target/.
            (
                "CARGO_TARGET_DIR".to_string(),
                ci_target_dir.to_string_lossy().to_string(),
            ),
            // Mark the environment as CI for tools that branch on it.
            ("CI".to_string(), "true".to_string()),
        ];
        if let Some(path) = tool_path(tool_dirs) {
            // PATH is NOT settable from a manifest — it is the canonical
            // "redirect binary resolution" sink and stays off the allowlist.
            // The executor prepends only directories it provisioned itself,
            // from the closed tool registry, at versions the manifest pinned.
            // The host's own PATH is preserved after them so cargo, git and
            // node still resolve.
            exports.push(("PATH".to_string(), path));
        }
        Self { exports }
    }
}

/// Build the step PATH: provisioned tool directories first, the runner's own
/// PATH after. `None` when nothing was provisioned, so the child simply
/// inherits.
fn tool_path(tool_dirs: &[PathBuf]) -> Option<String> {
    if tool_dirs.is_empty() {
        return None;
    }
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let mut entries: Vec<PathBuf> = tool_dirs.to_vec();
    entries.extend(std::env::split_paths(&inherited));
    std::env::join_paths(entries)
        .ok()
        .map(|s| s.to_string_lossy().to_string())
}

/// Run one step: spawn, pump output, enforce timeout + cancellation.
async fn run_step(
    step: &CiStep,
    dispatch_env: &DispatchEnv,
    worktree: &Path,
    ci_job: &CiJob,
    sink: &ProgressSink,
    cancel: &CancellationToken,
) -> StepOutcome {
    let cwd = match resolve_step_cwd(worktree, step) {
        Ok(c) => c,
        Err(e) => {
            sink.push(&format!("[ci-node] step {}: {e}", step.name));
            return StepOutcome::Failure;
        }
    };

    // Env: allowlisted step env first, then the executor's exports, which WIN
    // over the step.
    let mut envs: Vec<(String, String)> = step
        .env
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    envs.extend(dispatch_env.exports.iter().cloned());

    let mut child = match spawn_step_child(step, &cwd, &envs) {
        Ok(c) => c,
        Err(e) => {
            sink.push(&format!("[ci-node] step {}: {e}", step.name));
            return StepOutcome::Failure;
        }
    };

    // Job Object (kill-on-close): same crash-safety net as every other
    // runner child. Children spawned by the step inherit membership.
    #[cfg(target_os = "windows")]
    {
        if let Some(raw) = child.raw_handle() {
            crate::job_object::assign_process_to_job(raw as windows_sys::Win32::Foundation::HANDLE);
        }
    }
    // Plus the per-dispatch memory-backstop job (plan §4.6).
    ci_job.assign(&child);

    // Pump stdout/stderr per-line into the sink (tail ring + batched
    // progress POSTs — the pump_subprocess/forward_stream pattern).
    let out_task = child.stdout.take().map(|s| {
        let pusher = sink.pusher();
        tokio::spawn(pump_lines(s, pusher, step.name.clone(), "out"))
    });
    let err_task = child.stderr.take().map(|s| {
        let pusher = sink.pusher();
        tokio::spawn(pump_lines(s, pusher, step.name.clone(), "err"))
    });

    let timeout = Duration::from_secs(step.effective_timeout_secs());
    let outcome = tokio::select! {
        status = child.wait() => match status {
            Ok(s) if s.success() => StepOutcome::Success,
            Ok(s) => {
                sink.push(&format!("[ci-node] step {} exited {s}", step.name));
                StepOutcome::Failure
            }
            Err(e) => {
                sink.push(&format!("[ci-node] step {} wait error: {e}", step.name));
                StepOutcome::Failure
            }
        },
        _ = tokio::time::sleep(timeout) => {
            sink.push(&format!(
                "[ci-node] step {} timed out after {}s — killing process tree",
                step.name,
                timeout.as_secs()
            ));
            kill_step_tree(&mut child).await;
            StepOutcome::Timeout
        }
        _ = cancel.cancelled() => {
            sink.push(&format!(
                "[ci-node] step {} cancelled — killing process tree",
                step.name
            ));
            kill_step_tree(&mut child).await;
            StepOutcome::Cancelled
        }
    };

    // The pumps end when the pipes close (child exit or kill).
    if let Some(t) = out_task {
        let _ = t.await;
    }
    if let Some(t) = err_task {
        let _ = t.await;
    }
    outcome
}

/// Per-line pump for one stream.
async fn pump_lines<R: tokio::io::AsyncRead + Unpin + Send + 'static>(
    stream: R,
    pusher: LinePusher,
    step_name: String,
    stream_tag: &'static str,
) {
    let mut lines = BufReader::new(stream).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        pusher.push(&format!("[{step_name}:{stream_tag}] {line}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_outcome_conclusions() {
        assert_eq!(StepOutcome::Success.as_conclusion(), "success");
        assert_eq!(StepOutcome::Failure.as_conclusion(), "failure");
        assert_eq!(StepOutcome::Timeout.as_conclusion(), "failure");
        assert_eq!(StepOutcome::Cancelled.as_conclusion(), "cancelled");
    }

    fn exports(env: &DispatchEnv, key: &str) -> Option<String> {
        env.exports
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    }

    /// The caps the executor owns are exported for BOTH test harnesses, not
    /// just the build — the failure mode these exist for killed the Actions
    /// agent in the test phase too.
    #[test]
    fn executor_exports_both_phases_of_cap() {
        let env = DispatchEnv::build(3, 7, Path::new("/ci-target"), &[]);
        assert_eq!(exports(&env, "CARGO_BUILD_JOBS").as_deref(), Some("3"));
        assert_eq!(exports(&env, "RUST_TEST_THREADS").as_deref(), Some("7"));
        assert_eq!(exports(&env, "NEXTEST_TEST_THREADS").as_deref(), Some("7"));
        assert_eq!(exports(&env, "CI").as_deref(), Some("true"));
        assert!(exports(&env, "CARGO_TARGET_DIR").is_some());
        // Nothing provisioned ⇒ the child inherits PATH untouched.
        assert!(exports(&env, "PATH").is_none());
    }

    /// A provisioned tool goes on the FRONT of PATH, and the host's own PATH
    /// survives behind it (cargo/git/node must still resolve).
    #[test]
    fn tool_dirs_prepend_without_replacing_the_host_path() {
        let dir = PathBuf::from("/tools/cargo-nextest/0.9.98");
        let env = DispatchEnv::build(1, 2, Path::new("/ci-target"), &[dir.clone()]);
        let path = exports(&env, "PATH").expect("PATH must be exported when a tool is provisioned");
        let mut entries = std::env::split_paths(&path);
        assert_eq!(entries.next().as_deref(), Some(dir.as_path()));
        let inherited: Vec<_> =
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect();
        let rest: Vec<_> = entries.collect();
        assert_eq!(rest, inherited);
    }

    /// The executor's exports are appended AFTER the step's own env, and
    /// `spawn_step_child` applies them in order, so the executor's value is
    /// the one the child sees. (The manifest also rejects these keys
    /// outright — this asserts the second half of that belt-and-braces.)
    #[test]
    fn executor_exports_are_applied_last() {
        let env = DispatchEnv::build(1, 2, Path::new("/ci-target"), &[]);
        let step_env = vec![("CARGO_BUILD_JOBS".to_string(), "64".to_string())];
        let mut envs = step_env.clone();
        envs.extend(env.exports.iter().cloned());
        let last = envs
            .iter()
            .filter(|(k, _)| k == "CARGO_BUILD_JOBS")
            .next_back()
            .expect("present");
        assert_eq!(last.1, "1");
    }

    /// The manifest-path structural gate (pre-canonicalize half).
    #[test]
    fn manifest_path_structural_rejections() {
        let wt = std::env::temp_dir(); // any existing dir works for the structural half
        for bad in ["../up.toml", "/abs.toml", "C:\\abs.toml", "a/../../b.toml"] {
            let err = load_manifest(&wt, bad).unwrap_err();
            assert!(
                err.contains("repo-relative") || err.contains("not readable"),
                "{bad}: got {err}"
            );
        }
    }
}
