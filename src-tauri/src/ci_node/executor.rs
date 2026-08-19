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
//! order: tools ([`super::tools`]), siblings ([`super::sibling`]), then
//! services ([`super::services`]). All three are declared IN the manifest, so
//! none can run before it is read and validated; all three abort the dispatch
//! on failure, because a step that silently ran without its declared tool,
//! sibling or database produces a verdict that looks like a code failure and is
//! not. Services come last because they are the only stage that leaves
//! something RUNNING: there is no reason to start a database before finding out
//! a declared tool does not exist for this platform.
//!
//! ## Capture-before-cleanup (type-enforced, not conventional)
//!
//! The steps' JUnit report lives INSIDE the dispatch worktree that
//! [`super::checkout::cleanup_dispatch`] deletes, and it is coord's Tier-7
//! credibility-gate input. "Clean up, then report" therefore destroys the
//! artifact before anything can send it — which is precisely what happened
//! before this seam existed, and it was invisible because the dispatch itself
//! still went green.
//!
//! So the ordering is expressed in the types rather than in the statement
//! order of one function:
//!
//! 1. [`super::junit::capture`] returns a value;
//! 2. [`super::reporting::post_result`] takes that value as a REQUIRED
//!    parameter and is the only minter of [`reporting::ResultReported`];
//! 3. [`DispatchWorkspace::cleanup`] CONSUMES a `ResultReported` and is the
//!    only caller of `cleanup_dispatch`.
//!
//! There is no way to reach cleanup without having gone through reporting, and
//! no way to report without having been handed the capture slot. Re-breaking
//! this requires deleting a type, not reordering two lines.

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

/// Everything one dispatch brought into existence — the directory tree AND the
/// service containers — and the ONLY thing that may remove them.
///
/// [`cleanup`](DispatchWorkspace::cleanup) consumes a
/// [`reporting::ResultReported`] receipt, which only
/// [`reporting::post_result`] can mint. That makes "reported, then cleaned up"
/// the only expressible order — see the module docs for why the reverse order
/// is a silent, green-looking data-loss bug.
///
/// The service stack lives here, rather than beside the provisioning call,
/// precisely so that it rides that single exit path: every early return in
/// [`run_dispatch`] already goes through `cleanup`, so adding a second thing to
/// remove could not miss one. A leaked container is worse than a failed build.
struct DispatchWorkspace {
    root: PathBuf,
    repo: String,
    dispatch_id: String,
    /// Containers started for this dispatch. Empty (and inert) unless the
    /// manifest declared `[[services]]`.
    services: super::services::ServiceStack,
}

impl DispatchWorkspace {
    fn new(root: &Path, repo: &str, dispatch_id: &str) -> Self {
        Self {
            root: root.to_path_buf(),
            repo: repo.to_string(),
            dispatch_id: dispatch_id.to_string(),
            services: super::services::ServiceStack::new(dispatch_id),
        }
    }

    /// Destroy the dispatch's service containers, then its worktree and
    /// dispatch root. Consumes both `self` and the reporting receipt, so it can
    /// run at most once per dispatch and never before the result (with its
    /// JUnit artifact) has been POSTed.
    ///
    /// Containers first: they hold no state the report needs, and taking them
    /// down before a slow filesystem delete shortens the window in which a
    /// crash could leak one.
    async fn cleanup(mut self, _reported: reporting::ResultReported) {
        self.services.teardown().await;
        super::checkout::cleanup_dispatch(&self.root, &self.repo, &self.dispatch_id).await;
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
    // Everything on disk this dispatch owns. Only `workspace.cleanup(receipt)`
    // may remove it, and a receipt only exists after `post_result` — so no
    // path out of this function can delete the JUnit before it is sent.
    let mut workspace = DispatchWorkspace::new(&root, &payload.repo, &payload.dispatch_id);

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
            // No artifact: the checkout never completed, so no step ever ran.
            let reported = reporting::post_result(
                &base,
                &payload.dispatch_id,
                "failure",
                &steps_summary,
                None,
                &tail,
                None,
            )
            .await;
            // prepare_worktree may have left a partial worktree behind.
            workspace.cleanup(reported).await;
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
            // No artifact: the manifest was rejected, so no step ever ran.
            let reported = reporting::post_result(
                &base,
                &payload.dispatch_id,
                "failure",
                &steps_summary,
                None,
                &tail,
                None,
            )
            .await;
            workspace.cleanup(reported).await;
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
        "[ci-node] manifest ok: {} step(s), {} sibling(s), {} tool(s), {} service(s); \
         cargo_build_jobs={build_jobs} test_threads={test_threads} \
         (host sizing: {} / {})",
        manifest.steps.len(),
        manifest.siblings.len(),
        manifest.tools.len(),
        manifest.services.len(),
        host.cargo_build_jobs,
        host.test_threads
    ));

    // ── Provisioning (tools, siblings, then services) ──
    let provision_started = Instant::now();
    let provisioning = provision(
        &payload,
        &root,
        &worktree,
        &manifest,
        &sink,
        &cancel,
        &mut workspace.services,
    )
    .await;
    let (tool_dirs, service_env) = match provisioning {
        Ok(provisioned) => {
            steps_summary.push(StepSummary {
                name: "[setup] provision".to_string(),
                conclusion: "success".to_string(),
                duration_secs: provision_started.elapsed().as_secs(),
            });
            provisioned
        }
        Err(e) => {
            sink.push(&format!("[ci-node] provisioning failed: {e}"));
            steps_summary.push(StepSummary {
                name: "[setup] provision".to_string(),
                conclusion: "failure".to_string(),
                duration_secs: provision_started.elapsed().as_secs(),
            });
            let tail = sink.finish().await;
            // No artifact: provisioning aborts BEFORE the first step, so any
            // JUnit under the warm `.ci-target` cache belongs to a PREVIOUS
            // dispatch and must never be attributed to this head.
            let reported = reporting::post_result(
                &base,
                &payload.dispatch_id,
                "failure",
                &steps_summary,
                None,
                &tail,
                None,
            )
            .await;
            workspace.cleanup(reported).await;
            return;
        }
    };

    // Persistent per-repo CI target dir (warm across dispatches).
    let ci_target_dir = root
        .join(".ci-target")
        .join(crate::agent_runtime::local_repo_name(&payload.repo));

    let dispatch_env = DispatchEnv::build(
        build_jobs,
        test_threads,
        &ci_target_dir,
        &tool_dirs,
        &service_env,
    );

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

    // ── Capture the JUnit BEFORE the worktree can be removed ──
    //
    // Ordered here, ahead of `sink.finish()`, so the capture outcome reaches
    // coord in this dispatch's own progress stream AND its log_tail. Unconditional
    // on `conclusion`: a RED suite's report is exactly the evidence the
    // credibility gate needs, and a cancelled/timed-out step may still have
    // written a partial one worth ingesting.
    let capture = super::junit::capture(&worktree, &ci_target_dir, &manifest.steps);
    sink.push(&capture.log_line());

    let tail = sink.finish().await;
    let reported = reporting::post_result(
        &base,
        &payload.dispatch_id,
        conclusion,
        &steps_summary,
        None,
        &tail,
        capture.artifact(),
    )
    .await;
    // The receipt is the only key to cleanup; the artifact is already sent.
    workspace.cleanup(reported).await;
}

/// Provision what the manifest declares. Returns the tool directories to
/// prepend to the step PATH, and the service connection env to export to every
/// step.
///
/// Tools first, siblings second, services last. A sibling fetch is slower and
/// more failure-prone than a tool install (network, declaration validation),
/// and there is no reason to pay for it before finding out a declared tool does
/// not exist for this platform — and less reason still to have a database
/// RUNNING while finding that out. Services are also the only stage whose
/// failure can leave something behind, which is why the stack is owned by the
/// caller's `DispatchWorkspace` and passed in: a partially-provisioned stack
/// still names every container it started, and the caller's cleanup removes
/// them.
async fn provision(
    payload: &CiDispatchPayload,
    root: &Path,
    worktree: &Path,
    manifest: &CiManifest,
    sink: &ProgressSink,
    cancel: &CancellationToken,
    services: &mut super::services::ServiceStack,
) -> Result<(Vec<PathBuf>, Vec<(String, String)>), String> {
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

    let service_env = services
        .provision(&manifest.services, &mut log, cancel)
        .await?;

    Ok((tool_dirs, service_env))
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
        service_env: &[(String, String)],
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
            ("NEXTEST_TEST_THREADS".to_string(), test_threads.to_string()),
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
        // Service connections. Executor-owned for a reason the manifest cannot
        // work around: the host port is assigned at dispatch time and the
        // password is generated per dispatch, so a committed file could only
        // ever hold a wrong value. The manifest rejects these keys outright
        // with a pointer to the `[[services]]` entry that provides them.
        exports.extend(service_env.iter().cloned());
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
        let env = DispatchEnv::build(3, 7, Path::new("/ci-target"), &[], &[]);
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
        let env = DispatchEnv::build(1, 2, Path::new("/ci-target"), &[dir.clone()], &[]);
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
        let env = DispatchEnv::build(1, 2, Path::new("/ci-target"), &[], &[]);
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

    /// Capture-before-cleanup, asserted on the observable consequence rather
    /// than on statement order: cleanup really does destroy the JUnit, so if
    /// the two ever swap back the report is gone.
    ///
    /// (The compile-time half of this guarantee is `DispatchWorkspace::cleanup`
    /// consuming a `reporting::ResultReported`, which only `post_result` mints
    /// and which `post_result` cannot be called without passing the capture
    /// slot. This test covers the runtime half: that the file is genuinely
    /// inside what cleanup removes.)
    #[test]
    fn cleanup_destroys_the_junit_so_capture_must_precede_it() {
        let base = std::env::temp_dir().join(format!("ci-order-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dispatch_id = "0198f2b4-1111-7aaa-bbbb-cccccccccccc";
        let repo = "qontinui/qontinui-coord";

        let worktree = super::super::checkout::ci_worktree_path(&base, dispatch_id, repo);
        let dispatch_root = super::super::checkout::ci_dispatch_root(&base, dispatch_id);
        let profile = worktree.join("target").join("nextest").join("ci-pr");
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::write(
            profile.join("junit.xml"),
            "<testsuites><testcase name=\"t\" classname=\"c\"/></testsuites>",
        )
        .unwrap();

        // The report is INSIDE the tree `cleanup_dispatch` removes — this is
        // the whole reason the ordering is load-bearing.
        assert!(
            profile.join("junit.xml").starts_with(&dispatch_root),
            "the JUnit must live inside the dispatch root cleanup deletes"
        );

        // Capture (the step that must come first) finds it.
        let ci_target = base.join(".ci-target").join("qontinui-coord");
        let captured = super::super::junit::capture(&worktree, &ci_target, &[]);
        let artifact = captured
            .artifact()
            .expect("capture must find the report while the worktree exists");
        assert!(artifact.raw.contains("<testcase"));

        // After the dispatch root is gone, capture finds nothing — i.e. the
        // reversed order yields an empty artifact and a silently fail-closed
        // credibility tier.
        std::fs::remove_dir_all(&dispatch_root).unwrap();
        assert!(
            super::super::junit::capture(&worktree, &ci_target, &[])
                .artifact()
                .is_none(),
            "post-cleanup capture must find nothing — proving order matters"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// Service connection env reaches the steps, and it is applied AFTER the
    /// step's own env, so the executor's value is the one the child sees.
    #[test]
    fn service_env_is_exported_to_every_step() {
        let service_env = super::super::services::exports_for(
            super::super::services::ServiceKind::Redis,
            6399,
            &super::super::services::ServiceCreds {
                user: "u".to_string(),
                password: "p".to_string(),
                database: "d".to_string(),
            },
        );
        let env = DispatchEnv::build(1, 2, Path::new("/ci-target"), &[], &service_env);
        assert_eq!(
            exports(&env, "REDIS_URL").as_deref(),
            Some("redis://127.0.0.1:6399")
        );
        assert_eq!(exports(&env, "REDIS_PORT").as_deref(), Some("6399"));
        // Nothing declared ⇒ nothing exported: a manifest without services
        // must not gain a connection variable pointing nowhere.
        let bare = DispatchEnv::build(1, 2, Path::new("/ci-target"), &[], &[]);
        assert!(exports(&bare, "REDIS_URL").is_none());
        assert!(exports(&bare, "DATABASE_URL").is_none());
    }

    /// TEARDOWN RIDES THE FAILURE PATH. `cleanup` is the single exit of
    /// `run_dispatch` — every early return (checkout failure, manifest
    /// rejection, provisioning failure) reaches it — so this asserts the two
    /// halves it must do: remove the service containers, and remove EXACTLY
    /// the one dispatch directory.
    ///
    /// The runtime named here does not exist, which is the point: teardown is
    /// best-effort, so a removal that cannot run must still empty the stack and
    /// must never abort the directory cleanup that follows it.
    #[tokio::test]
    async fn cleanup_tears_down_services_and_removes_exactly_one_directory() {
        let base = std::env::temp_dir().join(format!(
            "ci-cleanup-test-{}-{}",
            std::process::id(),
            rand::random::<u32>()
        ));
        let dispatch_id = "0198f2b4-2222-7aaa-bbbb-cccccccccccc";
        let repo = "qontinui/qontinui-coord";

        // Inside the cleanup unit: the worktree and a provisioned sibling.
        let dispatch_root = super::super::checkout::ci_dispatch_root(&base, dispatch_id);
        let worktree = super::super::checkout::ci_worktree_path(&base, dispatch_id, repo);
        std::fs::create_dir_all(worktree.join("src")).unwrap();
        std::fs::write(worktree.join("src").join("lib.rs"), "fn main() {}").unwrap();
        let sibling = dispatch_root.join("qontinui-schemas");
        std::fs::create_dir_all(&sibling).unwrap();
        std::fs::write(sibling.join("Cargo.toml"), "[package]").unwrap();

        // OUTSIDE it: a peer dispatch, the warm CI target cache, and the
        // primary checkout. None of these may be touched.
        let peer =
            super::super::checkout::ci_dispatch_root(&base, "0198f2b4-3333-7aaa-bbbb-cccccccccccc");
        std::fs::create_dir_all(&peer).unwrap();
        std::fs::write(peer.join("keep.txt"), "peer").unwrap();
        let warm_target = base.join(".ci-target").join("qontinui-coord");
        std::fs::create_dir_all(&warm_target).unwrap();
        std::fs::write(warm_target.join("keep.txt"), "warm").unwrap();
        let primary = base.join("qontinui-coord");
        std::fs::create_dir_all(&primary).unwrap();
        std::fs::write(primary.join("keep.txt"), "primary").unwrap();

        let mut workspace = DispatchWorkspace::new(&base, repo, dispatch_id);
        workspace.services = super::super::services::ServiceStack::for_test(
            dispatch_id,
            Some("qontinui-no-such-container-runtime"),
            &["qontinui-ci-0198f2b4-2222-7aaa-bbbb-cccccccccccc-redis"],
        );
        workspace
            .cleanup(reporting::ResultReported::for_test())
            .await;

        assert!(
            !dispatch_root.exists(),
            "the dispatch root must be removed: {}",
            dispatch_root.display()
        );
        assert!(
            peer.join("keep.txt").exists(),
            "a peer dispatch was removed"
        );
        assert!(
            warm_target.join("keep.txt").exists(),
            "the warm CI target cache was removed"
        );
        assert!(
            primary.join("keep.txt").exists(),
            "the primary checkout was removed"
        );

        let _ = std::fs::remove_dir_all(&base);
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
