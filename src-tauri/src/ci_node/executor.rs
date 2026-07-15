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
//! - `CARGO_BUILD_JOBS` is exported from the manifest's `[limits]`
//!   (default 1 — this workspace OOMs above that);
//! - cargo artifacts go to a persistent per-repo CI target dir
//!   (`<root>/.ci-target/<repo basename>`) — warm across dispatches, never
//!   contending with the developer's own `target/`.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

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
            super::checkout::cleanup_worktree(&root, &payload.repo, &payload.dispatch_id).await;
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
            super::checkout::cleanup_worktree(&root, &payload.repo, &payload.dispatch_id).await;
            return;
        }
    };
    sink.push(&format!(
        "[ci-node] manifest ok: {} step(s), cargo_build_jobs={}",
        manifest.steps.len(),
        manifest.limits.effective_cargo_build_jobs()
    ));

    // Persistent per-repo CI target dir (warm across dispatches).
    let ci_target_dir = root
        .join(".ci-target")
        .join(crate::agent_runtime::local_repo_name(&payload.repo));

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
        let outcome = run_step(
            step,
            &manifest,
            &worktree,
            &ci_target_dir,
            &ci_job,
            &sink,
            &cancel,
        )
        .await;
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
    super::checkout::cleanup_worktree(&root, &payload.repo, &payload.dispatch_id).await;
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

/// Run one step: spawn, pump output, enforce timeout + cancellation.
#[allow(clippy::too_many_arguments)]
async fn run_step(
    step: &CiStep,
    manifest: &CiManifest,
    worktree: &Path,
    ci_target_dir: &Path,
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

    // Env: allowlisted step env first, then the machine-safety knobs, which
    // WIN over the step (`CARGO_BUILD_JOBS` from [limits] is a cap the step
    // cannot raise; the CI target dir keeps cargo out of the dev's target/).
    let mut envs: Vec<(String, String)> = step
        .env
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    envs.push((
        "CARGO_BUILD_JOBS".to_string(),
        manifest.limits.effective_cargo_build_jobs().to_string(),
    ));
    envs.push((
        "CARGO_TARGET_DIR".to_string(),
        ci_target_dir.to_string_lossy().to_string(),
    ));
    // Mark the environment as CI for tools that branch on it.
    envs.push(("CI".to_string(), "true".to_string()));

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
