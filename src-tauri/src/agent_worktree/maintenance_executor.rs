//! Scheduled-maintenance executor (Phase 1, runner side).
//!
//! Plan reference:
//! `D:/qontinui-root/plans/2026-06-08-coord-scheduled-maintenance-subsystem.md`
//! §3 (architecture), §4 Phase 1 step 7, §5 (safety floor).
//!
//! coord's maintenance scheduler DECIDES which low-risk housekeeping
//! actions a quiet machine should perform (reset a finished checkout back
//! to its default branch, etc.) but it has **no host filesystem access** —
//! only the runner can act on the operator's Windows disk. coord therefore
//! exposes pending maintenance *instructions* per device; this module
//! periodically PULLs them and EXECUTEs (or, while un-armed, log-only
//! dry-runs) the `checkout_main_ff_pull` action.
//!
//! ## Posture — EXTENDS the reclaim pull-loop
//!
//! This is a deliberate sibling of [`super::reclaim`]: identical machine-
//! wide, anonymous, device-keyed identity (`census::load_device_id_pub` /
//! `profiles::connected_coord_base`), the same `tokio::time::interval` +
//! `MissedTickBehavior::Skip` cadence (env
//! `QONTINUI_MAINTENANCE_INTERVAL_SECS`, default 300s, floored 30s), and
//! the same warn-and-retry, never-panic loop. The reclaim module owns
//! worktree teardown; this one owns the branch-reset maintenance action.
//! They poll independent coord endpoints but share every primitive.
//!
//! ## Arming (fail-safe, per-instruction)
//!
//! Each instruction carries an `armed` bool which defaults `false`
//! (`#[serde(default)]`), so a missing field — or coord with the whole
//! subsystem advisory (`COORD_MAINTENANCE_ENABLED` unset) — fails SAFE:
//! the action is **log-only**, emitting a clear
//! `[maintenance dry-run] would reset …` line and doing nothing else. Only
//! `armed == true` AND a full pass of the runner-side safety re-checks
//! actually mutates the working tree.
//!
//! ## Safety floor re-check (TOCTOU defense — MANDATORY)
//!
//! coord re-checked the safety floor at emit time, but state can change
//! between coord's DECIDE and the runner's EXECUTE. Before ANY mutation we
//! independently re-verify, in `repo_path`:
//!   1. `git status --porcelain` is EMPTY (not dirty) — refuse otherwise.
//!   2. `git rev-parse --abbrev-ref HEAD` still equals `current_branch` —
//!      refuse if the branch changed under us.
//!   3. `git merge-base --is-ancestor <current_branch> origin/<default>` —
//!      refuse if `current_branch` is not an ancestor of the default
//!      branch (i.e. it carries unmerged work).
//! Any failure → SKIP + a logged refusal; the action is never performed.
//!
//! ## Ack
//!
//! There is NO ack endpoint — mirroring reclaim, the next census plus the
//! `git_ops` record this module emits for a real reset serve as the ack.

use std::path::Path;
use std::time::Duration;

use crate::ai_provider::claude_cli::{run_structured_oneshot, OneshotError, OneshotTools};
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;
use serde::Deserialize;
use tracing::{debug, info, warn};

/// Default maintenance poll cadence — 300s (5 min), matching reclaim/census.
/// Override via `QONTINUI_MAINTENANCE_INTERVAL_SECS`.
const DEFAULT_MAINTENANCE_INTERVAL_SECS: u64 = 300;

// ---------------------------------------------------------------------------
// Wire types — coord serializes these. Shape pinned by the plan §4 step 7.
//
// {
//   "instructions": [
//     { "run_id": "uuid", "task_id": "uuid", "task_name": "string",
//       "action": { "kind": "checkout_main_ff_pull", "repo_path": "abs",
//                   "current_branch": "branch", "default_branch": "main" },
//       "armed": false, "reason": "string" }
//   ]
// }
// ---------------------------------------------------------------------------

/// The pull-endpoint body: `GET {coord}/coord/maintenance/instructions/{device_id}`.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct MaintenancePull {
    #[serde(default)]
    pub instructions: Vec<MaintenanceInstruction>,
}

/// One maintenance instruction coord wants this device to perform.
#[derive(Debug, Clone, Deserialize)]
pub struct MaintenanceInstruction {
    /// The `coord.maintenance_runs` row this instruction reconciles. Carried
    /// through into the emitted `git_op` metadata so coord can correlate.
    #[serde(default)]
    pub run_id: String,
    /// The `coord.maintenance_tasks` definition that produced it.
    #[serde(default)]
    pub task_id: String,
    /// Human-readable task name (for the log lines + git_op metadata).
    #[serde(default)]
    pub task_name: String,
    /// What to do. Forward-compatible: an unknown `kind` deserializes to
    /// [`MaintenanceAction::Unknown`] and is logged + ignored, never acted on.
    pub action: MaintenanceAction,
    /// Per-instruction arm. Defaults `false` (`#[serde(default)]`) → log-only
    /// dry-run. Only `true` + a full safety re-check pass mutates the tree.
    #[serde(default)]
    pub armed: bool,
    /// Free-form reason coord attached (e.g. which predicates matched).
    #[serde(default)]
    pub reason: String,
}

/// The action of a maintenance instruction. Serde-tagged by `kind`.
///
/// Forward-compatibility: an action kind this runner build doesn't know
/// deserializes to [`MaintenanceAction::Unknown`] (via the `other` catch-all),
/// which the executor logs and ignores — it is NEVER destructive.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MaintenanceAction {
    /// Reset a finished checkout back to its default branch: `git checkout
    /// <default_branch>` then `git pull --ff-only`. Only performed when armed
    /// AND every safety re-check passes.
    CheckoutMainFfPull {
        /// Absolute path to the checkout on the runner's host disk.
        repo_path: String,
        /// The branch coord observed as current (and verified clean+merged).
        current_branch: String,
        /// The default branch to reset onto (e.g. `main`).
        default_branch: String,
    },
    /// An action kind this build doesn't recognize. Logged + ignored.
    #[serde(other)]
    Unknown,
}

// ---------------------------------------------------------------------------
// Runner-side safety re-check (TOCTOU defense) + execution (side-effecting).
// ---------------------------------------------------------------------------

/// Budget for a purely LOCAL git command — `status --porcelain`,
/// `rev-parse`, `merge-base --is-ancestor`, `checkout`.
///
/// These touch only the working tree and the object store, so a healthy one
/// finishes in milliseconds even on a large repo. The reason the budget is 30s
/// rather than 1s is the pathology, not the happy path: the hang class here is
/// an `index.lock` held by a concurrent git, which typically clears in a few
/// seconds. 30s absorbs a genuinely busy tree while still bounding the thread.
const GIT_LOCAL_TIMEOUT: Duration = Duration::from_secs(30);

/// Budget for a git command that talks to a REMOTE — `pull --ff-only`.
///
/// Deliberately separate from [`GIT_LOCAL_TIMEOUT`] and deliberately larger: a
/// legitimate fetch over a slow link on a repo that has not been pulled in days
/// can genuinely take minutes, and killing a healthy fetch would make this
/// maintenance action never succeed. What it must NOT do is wait *forever*: an
/// unreachable remote or a credential prompt is precisely how a `git pull`
/// parks a blocking-pool thread permanently. `run_with_timeout` already routes
/// stdin from null, so a credential prompt fails fast rather than blocking; this
/// bound covers the unreachable-remote and wedged-transport cases.
const GIT_NETWORK_TIMEOUT: Duration = Duration::from_secs(300);

/// Run `git -C <dir> <args>` capturing output, under a hard time budget
/// ([`GIT_LOCAL_TIMEOUT`] for local commands, [`GIT_NETWORK_TIMEOUT`] for
/// `pull --ff-only`). `Ok((success, stdout, stderr))` — `success` is the
/// process exit status. `Err` on spawn failure **or on timeout**, so every
/// caller's existing conservative `Err` handling (refuse / report failure)
/// covers a hang too.
///
/// **Time-bounded by contract — do not reintroduce a bare `.output()` here.**
/// This used to call `Command::output()` with no timeout at all, and it is
/// driven off a 300s `tokio::time::interval` through `spawn_blocking`. A `git`
/// that never returns — an `index.lock` held by another process, a credential
/// prompt, an unreachable remote — therefore consumed one tokio blocking-pool
/// thread *permanently*, once per hung instruction. That is the same defect
/// class that wedged the runner on 2026-08-30 via the WMI process snapshot;
/// this site is lower volume (one instruction at a time) but identical in kind.
fn git_capture_with_timeout(
    dir: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<(bool, String, String), String> {
    let mut cmd = crate::process_helpers::no_window("git");
    cmd.arg("-C").arg(dir).args(args);
    run_git_capture(cmd, dir, args, timeout)
}

/// Execute an already-built git `Command` under `timeout`.
///
/// Split out from [`git_capture_with_timeout`] so a regression test can hand it
/// a command that genuinely never returns — the hang this whole function exists
/// to survive — without needing a wedged git on the test machine.
fn run_git_capture(
    cmd: std::process::Command,
    dir: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<(bool, String, String), String> {
    match crate::process_helpers::run_with_timeout(cmd, timeout) {
        Ok(crate::process_helpers::TimedOutput::Completed(out)) => Ok((
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )),
        // A timeout is an `Err`, never a `false` success flag: `recheck_safety`
        // must REFUSE on a tree it could not reason about, and the execute path
        // must report a hang as a failure. Reporting it as "the command ran and
        // said no" would silently downgrade a wedge into a routine verdict.
        Ok(crate::process_helpers::TimedOutput::TimedOut { pid, reaped }) => {
            warn!(
                dir = %dir,
                args = ?args,
                timeout_secs = timeout.as_secs(),
                child_pid = pid,
                reaped,
                "maintenance: git timed out and was killed"
            );
            Err(format!(
                "git {args:?} in {dir} exceeded its {}s budget and was killed \
                 (pid={pid}, reaped={reaped})",
                timeout.as_secs()
            ))
        }
        Err(e) => Err(format!("spawn git {args:?} in {dir}: {e}")),
    }
}

/// How the safety re-check and the execute path reach git.
///
/// A function seam rather than a direct call so a regression test can drive
/// both paths with a git that HANGS — the failure mode this module was wedging
/// on — without needing a wedged git on the test machine. Production always
/// passes [`git_capture_with_timeout`] itself.
type GitRunner<'a> = &'a dyn Fn(&str, &[&str], Duration) -> Result<(bool, String, String), String>;

/// Outcome of the runner-side safety re-check.
#[derive(Debug, PartialEq, Eq)]
enum SafetyVerdict {
    /// Every floor rule passed — the action MAY proceed (when armed).
    Ok,
    /// A floor rule fired — carries the human-readable refusal reason.
    Refuse(String),
}

/// Re-verify the safety floor on the runner's live disk (defense in depth —
/// coord's DECIDE-time check may be stale). All three rules must pass:
///   1. `git status --porcelain --untracked-files=all` EMPTY (clean — an
///      untracked file hidden by `status.showUntrackedFiles=no` still counts).
///   2. `git rev-parse --abbrev-ref HEAD` == `current_branch`.
///   3. `git merge-base --is-ancestor <current_branch> origin/<default>`.
///
/// On a git spawn failure — **or a git TIMEOUT** — we conservatively REFUSE
/// (never act on a tree we can't reason about). Both arrive as `Err` from
/// [`git_capture_with_timeout`], so the timeout needs no separate arm: a hung
/// `status --porcelain` can no longer be mistaken for a clean tree.
fn recheck_safety(repo_path: &str, current_branch: &str, default_branch: &str) -> SafetyVerdict {
    recheck_safety_with(
        repo_path,
        current_branch,
        default_branch,
        &git_capture_with_timeout,
    )
}

/// [`recheck_safety`] with an injectable git runner — see [`GitRunner`].
fn recheck_safety_with(
    repo_path: &str,
    current_branch: &str,
    default_branch: &str,
    git: GitRunner<'_>,
) -> SafetyVerdict {
    if !Path::new(repo_path).is_dir() {
        return SafetyVerdict::Refuse(format!("repo_path {repo_path} is not a directory"));
    }

    // (1) Dirty gate — any uncommitted/untracked change → refuse.
    match git(
        repo_path,
        super::dirty::STATUS_PORCELAIN_ALL_UNTRACKED,
        GIT_LOCAL_TIMEOUT,
    ) {
        Ok((true, stdout, _)) if !stdout.trim().is_empty() => {
            return SafetyVerdict::Refuse(format!(
                "{repo_path} is dirty (git status --porcelain --untracked-files=all non-empty) — refusing reset"
            ));
        }
        Ok((true, _, _)) => {} // clean — proceed to next check.
        Ok((false, _, stderr)) => {
            return SafetyVerdict::Refuse(format!(
                "{repo_path} git status --porcelain failed: {}",
                stderr.trim()
            ));
        }
        Err(e) => return SafetyVerdict::Refuse(e),
    }

    // (2) Current-branch confirmation — the branch must not have changed.
    match git(
        repo_path,
        &["rev-parse", "--abbrev-ref", "HEAD"],
        GIT_LOCAL_TIMEOUT,
    ) {
        Ok((true, stdout, _)) => {
            let head = stdout.trim();
            if head != current_branch {
                return SafetyVerdict::Refuse(format!(
                    "{repo_path} current branch changed ({head:?} != expected {current_branch:?}) \
                     — refusing reset"
                ));
            }
        }
        Ok((false, _, stderr)) => {
            return SafetyVerdict::Refuse(format!(
                "{repo_path} git rev-parse --abbrev-ref HEAD failed: {}",
                stderr.trim()
            ));
        }
        Err(e) => return SafetyVerdict::Refuse(e),
    }

    // (3) Ancestor gate — current_branch must be merged into origin/<default>
    // (no unmerged work would be discarded by the reset). `is-ancestor`
    // exits 0 when true, 1 when false, >1 on error.
    let origin_default = format!("origin/{default_branch}");
    match git(
        repo_path,
        &[
            "merge-base",
            "--is-ancestor",
            current_branch,
            &origin_default,
        ],
        GIT_LOCAL_TIMEOUT,
    ) {
        Ok((true, _, _)) => {} // ancestor — merged — proceed.
        Ok((false, _, stderr)) => {
            return SafetyVerdict::Refuse(format!(
                "{repo_path} {current_branch} is NOT an ancestor of {origin_default} \
                 (unmerged work) — refusing reset{}",
                if stderr.trim().is_empty() {
                    String::new()
                } else {
                    format!(": {}", stderr.trim())
                }
            ));
        }
        Err(e) => return SafetyVerdict::Refuse(e),
    }

    SafetyVerdict::Ok
}

/// Outcome of executing one `checkout_main_ff_pull` so the caller can decide
/// whether (and what) to report to coord's git_ops feed.
#[derive(Debug)]
enum ExecOutcome {
    /// The reset succeeded: `default_branch` checked out + ff-pulled.
    Done,
    /// Nothing was mutated (dry-run, unknown action, or a safety refusal).
    Skipped,
    /// A git step failed mid-way. `checkout` is attempted first; a failed
    /// checkout never proceeds to pull — so this never leaves a half-state
    /// beyond what git itself does. Carries the error for the log.
    Failed(String),
}

/// Execute (or log-only dry-run) one instruction. NEVER panics; a git
/// failure logs and returns [`ExecOutcome::Failed`] (the loop continues to
/// the next instruction and retries next tick).
fn execute_instruction(instr: &MaintenanceInstruction) -> ExecOutcome {
    execute_instruction_with(instr, &git_capture_with_timeout)
}

/// [`execute_instruction`] with an injectable git runner — see [`GitRunner`].
///
/// A git that times out surfaces as `Err` and therefore as
/// [`ExecOutcome::Failed`] carrying the killed pid and the budget. It is never
/// reported as success, and it never advances from `checkout` to `pull`.
fn execute_instruction_with(instr: &MaintenanceInstruction, git: GitRunner<'_>) -> ExecOutcome {
    let MaintenanceAction::CheckoutMainFfPull {
        repo_path,
        current_branch,
        default_branch,
    } = &instr.action
    else {
        info!(
            "maintenance: ignoring unknown action kind for task={:?} (reason={})",
            instr.task_name, instr.reason
        );
        return ExecOutcome::Skipped;
    };

    // Safety re-check ALWAYS — even for a dry-run, so the log reflects whether
    // the reset *would* be allowed right now.
    let verdict = recheck_safety_with(repo_path, current_branch, default_branch, git);
    if let SafetyVerdict::Refuse(reason) = &verdict {
        warn!(
            "maintenance: refusing reset of {repo_path} ({current_branch} -> {default_branch}) \
             [task={}] — {reason}",
            instr.task_name
        );
        return ExecOutcome::Skipped;
    }

    // Un-armed → advisory log-only, regardless of the (passed) safety verdict.
    if !instr.armed {
        info!(
            "[maintenance dry-run] would reset {repo_path} ({current_branch} -> {default_branch}): {}",
            instr.reason
        );
        return ExecOutcome::Skipped;
    }

    // Armed AND safe — perform the reset. `git checkout <default>` first; only
    // on success `git pull --ff-only`. A failed checkout aborts before pull so
    // we never pull onto an unexpected branch.
    match git(repo_path, &["checkout", default_branch], GIT_LOCAL_TIMEOUT) {
        Ok((true, _, _)) => {
            info!(
                "maintenance: checked out {default_branch} in {repo_path} [task={}]",
                instr.task_name
            );
        }
        Ok((false, _, stderr)) => {
            let msg = format!(
                "git checkout {default_branch} in {repo_path} failed: {}",
                stderr.trim()
            );
            warn!("maintenance: {msg}");
            return ExecOutcome::Failed(msg);
        }
        Err(e) => {
            warn!("maintenance: {e}");
            return ExecOutcome::Failed(e);
        }
    }

    match git(repo_path, &["pull", "--ff-only"], GIT_NETWORK_TIMEOUT) {
        Ok((true, stdout, _)) => {
            info!(
                "maintenance: ff-pulled {default_branch} in {repo_path} [task={}] — {}",
                instr.task_name,
                stdout.trim()
            );
            ExecOutcome::Done
        }
        Ok((false, _, stderr)) => {
            let msg = format!(
                "git pull --ff-only in {repo_path} (on {default_branch}) failed: {}",
                stderr.trim()
            );
            warn!("maintenance: {msg}");
            ExecOutcome::Failed(msg)
        }
        Err(e) => {
            warn!("maintenance: {e}");
            ExecOutcome::Failed(e)
        }
    }
}

// ---------------------------------------------------------------------------
// git_ops reporting — reuse the SHIPPED observable-bridge record path.
// ---------------------------------------------------------------------------

/// Read `active_tenant_id` from `~/.qontinui/machine.json` — Phase 8b
/// semantics: the DEVICE-LEVEL DEFAULT binding (default-for-new-sessions),
/// not the-only-tenant. Correct here because maintenance branch-resets act
/// on CANONICAL checkouts (a device-scoped surface), never on allocate-
/// stamped agent worktrees. `None` for single-tenant operators, which is
/// fine. Mirrors `census::resolve_tenant_id` / `fs_backstop::resolve_tenant_id`
/// (both private; the read is trivial and attribution is best-effort).
fn resolve_tenant_id() -> Option<uuid::Uuid> {
    qontinui_runner_lib::ambient::read_machine_json().active_tenant_uuid()
}

/// Report a completed branch-reset to coord's `git_ops` feed, reusing the
/// SAME `POST /coord/git-ops/record` path
/// (`qontinui_runner_lib::observable_bridge::git_ops_client`) the GitOpBridge
/// AND `fleet::pull_executor` use (tenant via `X-Qontinui-Tenant-Id`; the
/// record path resolves tenant from the header, not the JWT, so the
/// device_token arg is passed empty exactly as `fleet.rs:1831` does).
/// Best-effort: a missing tenant / coord-base logs and returns — the reset
/// still happened and the next census also serves as the ack.
async fn report_reset_git_op(
    repo_path: &str,
    default_branch: &str,
    instr: &MaintenanceInstruction,
) {
    use qontinui_runner_lib::observable_bridge::git_ops_client;
    use qontinui_types::git_ops::RecordGitOpRequest;

    let Some(tenant_id) = resolve_tenant_id() else {
        debug!("maintenance: no tenant_id — skipping git_op record for {repo_path}");
        return;
    };
    let Some(base) = qontinui_runner_lib::profiles::connected_coord_base() else {
        debug!("maintenance: no coord_url — skipping git_op record for {repo_path}");
        return;
    };
    let client = match git_ops_client::build_client() {
        Ok(c) => c,
        Err(e) => {
            warn!("maintenance: build git_ops client failed: {e}");
            return;
        }
    };

    // Repo basename from the checkout's origin URL (falling back to the
    // dir name) — same resolution the GitOpBridge uses for its feed.
    let repo = resolve_repo_name(repo_path);
    let req = RecordGitOpRequest {
        repo,
        branch: Some(default_branch.to_string()),
        op_kind: "checkout".to_string(),
        sha: None,
        message: Some(format!(
            "maintenance branch-reset: {} -> {default_branch}",
            instr.task_name
        )),
        metadata: Some(serde_json::json!({
            "source": "maintenance",
            "task_id": instr.task_id,
            "task_name": instr.task_name,
            "run_id": instr.run_id,
            "action": "checkout_main_ff_pull",
        })),
    };

    // device_token "" — the record path keys tenant off the header, mirroring
    // fleet::pull_executor's call (`fleet.rs:1830-1832`).
    if let Err(e) = git_ops_client::record(&client, &base, "", tenant_id, &req).await {
        warn!("maintenance: git_op record for {repo_path} failed: {e}");
    } else {
        debug!("maintenance: recorded git_op for {repo_path}");
    }
}

/// Resolve a repo name from a checkout path: `.git/config` origin URL
/// basename, falling back to the path's own dir name. Reuses the bridge's
/// `repo_basename_from_url` (pub, crosses the lib↔bin boundary — the same
/// function `agent_worktree::canonical_paths` imports).
fn resolve_repo_name(repo_path: &str) -> String {
    use qontinui_runner_lib::observable_bridge::git_ops::repo_basename_from_url;
    let dir = Path::new(repo_path);
    let config = dir.join(".git").join("config");
    if let Ok(text) = std::fs::read_to_string(&config) {
        let mut in_origin = false;
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('[') {
                in_origin = trimmed
                    .replace(' ', "")
                    .eq_ignore_ascii_case("[remote\"origin\"]");
                continue;
            }
            if in_origin {
                if let Some(rest) = trimmed.strip_prefix("url") {
                    let rest = rest.trim_start();
                    if let Some(eq) = rest.strip_prefix('=') {
                        return repo_basename_from_url(eq.trim());
                    }
                }
            }
        }
    }
    dir.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown-repo")
        .to_string()
}

// ---------------------------------------------------------------------------
// Pull + tick + spawn (mirrors reclaim.rs identity/base resolution).
// ---------------------------------------------------------------------------

/// Execute every instruction in one pull. The git subprocesses run on the
/// blocking pool (in [`tick_once`]); a real reset's git_op report is the one
/// async tail, performed here per completed instruction.
async fn execute_pull(pull: MaintenancePull) {
    if pull.instructions.is_empty() {
        debug!("maintenance: no instructions");
        return;
    }
    info!("maintenance: {} instruction(s)", pull.instructions.len());
    for instr in &pull.instructions {
        // The git status/branch/checkout/pull subprocesses are synchronous;
        // run them off the async worker so a slow disk/git doesn't pin it.
        let instr_clone = instr.clone();
        let outcome = match spawn_blocking_tracked(move || execute_instruction(&instr_clone)).await
        {
            Ok(o) => o,
            Err(e) => {
                warn!("maintenance: instruction execution panicked: {e}");
                continue;
            }
        };
        if let ExecOutcome::Done = outcome {
            if let MaintenanceAction::CheckoutMainFfPull {
                repo_path,
                default_branch,
                ..
            } = &instr.action
            {
                report_reset_git_op(repo_path, default_branch, instr).await;
            }
        }
    }
}

/// One maintenance cycle: pull + execute. Returns `Ok(())` on a clean skip
/// or a successful pull; `Err` only on a transport / non-2xx failure.
pub async fn tick_once() -> Result<(), String> {
    let device_id = match super::census::load_device_id_pub() {
        Some(id) => id,
        None => {
            debug!("maintenance: no device_id — skipping");
            return Ok(());
        }
    };
    let base = match qontinui_runner_lib::profiles::connected_coord_base() {
        Some(b) => b,
        None => {
            debug!("maintenance: no coord_url configured — skipping");
            return Ok(());
        }
    };

    let url = format!(
        "{}/coord/maintenance/instructions/{}",
        base.trim_end_matches('/'),
        device_id
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| format!("build maintenance http client: {e}"))?;
    let resp = crate::coord_http::coord_get(&client, &url)
        .send()
        .await
        .map_err(|e| format!("GET {url}: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        let excerpt: String = body.chars().take(200).collect();
        return Err(format!("coord returned {status} for GET {url}: {excerpt}"));
    }
    let pull: MaintenancePull = resp
        .json()
        .await
        .map_err(|e| format!("decode maintenance pull: {e}"))?;

    execute_pull(pull).await;
    Ok(())
}

/// Spawn the periodic maintenance poller. Interval from
/// `QONTINUI_MAINTENANCE_INTERVAL_SECS` (default 300s, floored 30s).
/// `MissedTickBehavior::Skip` + warn-and-retry, mirroring
/// [`super::reclaim::spawn_reclaim`].
pub fn spawn_maintenance() {
    let secs: u64 = std::env::var("QONTINUI_MAINTENANCE_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_MAINTENANCE_INTERVAL_SECS)
        .max(30);

    info!(
        "maintenance: starting periodic maintenance poller, interval={}s",
        secs
    );

    // Read ONCE: the agent-request arm is a spawn-time posture, logged here so
    // the runner log says which side of the gate this process is on.
    let agent_enabled = agent_requests_enabled();
    info!(
        "maintenance: agent requests {} ({AGENT_ENABLED_ENV}={})",
        if agent_enabled {
            "ENABLED"
        } else {
            "disabled (default)"
        },
        std::env::var(AGENT_ENABLED_ENV).unwrap_or_else(|_| "<unset>".to_string())
    );

    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(secs));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            if let Err(e) = tick_once().await {
                warn!("maintenance: {e}");
            }
            // After the instruction pull, independent of its outcome: at most
            // one coord maintenance agent request per tick.
            if agent_enabled {
                if let Err(e) = agent_requests_once().await {
                    warn!("maintenance agent: {e}");
                }
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Agent requests (plan Phases 2-3): coord's semantic verdicts + NL task
// compiles, answered by a single-shot headless `claude`.
//
// Pull:   GET  {coord}/coord/maintenance/agent-requests?limit=1
//         → {"requests":[{"id","kind":"verdict"|"compile","prompt",
//                         "repo_path":<str|null>,"tools":"readonly_git"|"none",
//                         "expires_at":<rfc3339>}]}
// Answer: POST {coord}/coord/maintenance/agent-requests/{id}/answer with
//         exactly one of
//           {"verdict":"yes"|"no"|"unknown","confidence":0..1,"rationale":".."}
//           {"result":{..},"rationale":".."}
//           {"error":".."}
// Device + tenant come from the device JWT `coord_get`/`coord_post` attach.
// ---------------------------------------------------------------------------

/// Arms agent-request processing. Truthy (`1`/`true`/`yes`/`on`) = on;
/// anything else, including unset, = OFF.
const AGENT_ENABLED_ENV: &str = "RUNNER_MAINTENANCE_AGENT_ENABLED";

/// Per-request budget for the one-shot `claude`, further capped by the time
/// left before the request's `expires_at`. Override with
/// `RUNNER_MAINTENANCE_AGENT_TIMEOUT_SECS` (floored 30s).
const DEFAULT_AGENT_TIMEOUT_SECS: u64 = 240;

/// Tool-loop bound for a verdict (read-only repo inspection).
const VERDICT_MAX_TURNS: u32 = 16;
/// A compile has no tools: one answering turn (plus slack for the CLI's own
/// accounting).
const COMPILE_MAX_TURNS: u32 = 2;

/// Longest `error` string posted back to coord.
const MAX_ERROR_CHARS: usize = 500;

/// Whether `value` (the env var's value, if set) arms agent requests.
fn agent_enabled_from(value: Option<&str>) -> bool {
    matches!(
        value.map(|v| v.trim().to_ascii_lowercase()).as_deref(),
        Some("1") | Some("true") | Some("yes") | Some("on")
    )
}

/// `RUNNER_MAINTENANCE_AGENT_ENABLED` read from the process env; default OFF.
pub fn agent_requests_enabled() -> bool {
    agent_enabled_from(std::env::var(AGENT_ENABLED_ENV).ok().as_deref())
}

fn agent_timeout() -> Duration {
    let secs = std::env::var("RUNNER_MAINTENANCE_AGENT_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(DEFAULT_AGENT_TIMEOUT_SECS)
        .max(30);
    Duration::from_secs(secs)
}

/// The agent-request pull body.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct AgentRequestPull {
    #[serde(default)]
    pub requests: Vec<AgentRequest>,
}

/// One claimed agent request. `kind` and `tools` stay strings so a value a
/// newer coord introduces is answered with an error rather than failing the
/// whole pull's decode (which would strand the claimed row until expiry).
#[derive(Debug, Clone, Deserialize)]
pub struct AgentRequest {
    pub id: String,
    pub kind: String,
    pub prompt: String,
    #[serde(default)]
    pub repo_path: Option<String>,
    #[serde(default)]
    pub tools: String,
    #[serde(default)]
    pub expires_at: Option<String>,
}

/// The answer body. Untagged: each variant serializes to exactly the
/// contract's object for it.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(untagged)]
pub enum AgentAnswer {
    Verdict {
        verdict: String,
        confidence: f64,
        rationale: String,
    },
    Compile {
        result: serde_json::Value,
        rationale: String,
    },
    Error {
        error: String,
    },
}

impl AgentAnswer {
    fn error(msg: impl Into<String>) -> Self {
        let msg: String = msg.into();
        Self::Error {
            error: msg.chars().take(MAX_ERROR_CHARS).collect(),
        }
    }

    fn unparseable(detail: &str) -> Self {
        Self::error(format!("unparseable claude output: {detail}"))
    }
}

/// Map a request's `tools` field to the one-shot tool set. `None` for a value
/// this build does not know.
fn oneshot_tools_for(tools: &str) -> Option<OneshotTools> {
    match tools {
        "readonly_git" => Some(OneshotTools::ReadonlyGit),
        "none" => Some(OneshotTools::None),
        _ => None,
    }
}

/// Validate a verdict object: `verdict` ∈ {yes,no,unknown}, numeric
/// `confidence` (clamped to [0,1]), string `rationale`.
pub fn validate_verdict(v: &serde_json::Value) -> AgentAnswer {
    let verdict = match v.get("verdict").and_then(|x| x.as_str()) {
        Some(s @ ("yes" | "no" | "unknown")) => s.to_string(),
        Some(other) => {
            return AgentAnswer::unparseable(&format!(
                "verdict must be yes|no|unknown, got {:?}",
                other.chars().take(40).collect::<String>()
            ))
        }
        None => return AgentAnswer::unparseable("missing string `verdict`"),
    };
    let confidence = match v.get("confidence").and_then(|x| x.as_f64()) {
        Some(c) if c.is_finite() => c.clamp(0.0, 1.0),
        _ => return AgentAnswer::unparseable("missing numeric `confidence`"),
    };
    let rationale = match v.get("rationale").and_then(|x| x.as_str()) {
        Some(r) => r.to_string(),
        None => return AgentAnswer::unparseable("missing string `rationale`"),
    };
    AgentAnswer::Verdict {
        verdict,
        confidence,
        rationale,
    }
}

/// Validate a compile object: an object `result`. `rationale` is carried
/// when it is a string, else sent empty (the contract's answer shape always
/// has it). Deep validation of `result` is coord's (its closed types).
pub fn validate_compile(v: &serde_json::Value) -> AgentAnswer {
    let result = match v.get("result") {
        Some(r @ serde_json::Value::Object(_)) => r.clone(),
        Some(_) => return AgentAnswer::unparseable("`result` is not an object"),
        None => return AgentAnswer::unparseable("missing object `result`"),
    };
    let rationale = v
        .get("rationale")
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_string();
    AgentAnswer::Compile { result, rationale }
}

/// Validate the model's parsed object against the request kind.
pub fn validate_for_kind(kind: &str, v: &serde_json::Value) -> AgentAnswer {
    match kind {
        "verdict" => validate_verdict(v),
        "compile" => validate_compile(v),
        other => AgentAnswer::error(format!("unsupported request kind {other:?}")),
    }
}

/// Time left before `expires_at`. `Some(ZERO)` when already expired; `None`
/// when absent or unparseable (coord only hands out unexpired rows, so an
/// unreadable stamp does not block the request).
fn time_until_expiry(
    expires_at: Option<&str>,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<Duration> {
    let at = chrono::DateTime::parse_from_rfc3339(expires_at?).ok()?;
    let left = at.with_timezone(&chrono::Utc) - now;
    Some(left.to_std().unwrap_or(Duration::ZERO))
}

/// What to do with one request before any `claude` runs.
#[derive(Debug, PartialEq)]
enum Preflight {
    /// Already expired: skip, post nothing (coord expires the row).
    Skip,
    /// Refuse with this error answer.
    Refuse(AgentAnswer),
    /// Run the one-shot in `cwd` with `tools`.
    Run {
        cwd: std::path::PathBuf,
        tools: OneshotTools,
        max_turns: u32,
        timeout: Duration,
    },
}

fn preflight(
    req: &AgentRequest,
    now: chrono::DateTime<chrono::Utc>,
    budget: Duration,
) -> Preflight {
    let timeout = match time_until_expiry(req.expires_at.as_deref(), now) {
        Some(left) if left.is_zero() => return Preflight::Skip,
        Some(left) => budget.min(left),
        None => budget,
    };
    let max_turns = match req.kind.as_str() {
        "verdict" => VERDICT_MAX_TURNS,
        "compile" => COMPILE_MAX_TURNS,
        other => {
            return Preflight::Refuse(AgentAnswer::error(format!(
                "unsupported request kind {other:?}"
            )))
        }
    };
    let Some(tools) = oneshot_tools_for(&req.tools) else {
        return Preflight::Refuse(AgentAnswer::error(format!(
            "unsupported tools mode {:?}",
            req.tools
        )));
    };
    let repo = req
        .repo_path
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty());
    let cwd = match repo {
        Some(p) if Path::new(p).is_dir() => std::path::PathBuf::from(p),
        Some(p) => {
            return Preflight::Refuse(AgentAnswer::error(format!(
                "repo_path is not an existing directory on this device: {p}"
            )))
        }
        // A repo-inspecting request with no checkout cannot be answered.
        None if tools == OneshotTools::ReadonlyGit => {
            return Preflight::Refuse(AgentAnswer::error(
                "repo_path is required for a readonly_git request",
            ))
        }
        // No repo (a compile): a neutral cwd, so no project CLAUDE.md or
        // settings from the runner's own working directory leak in.
        None => std::env::temp_dir(),
    };
    Preflight::Run {
        cwd,
        tools,
        max_turns,
        timeout,
    }
}

/// Map a one-shot outcome to the answer body for `kind`.
fn answer_from_outcome(
    kind: &str,
    outcome: Result<serde_json::Value, OneshotError>,
) -> AgentAnswer {
    match outcome {
        Ok(v) => validate_for_kind(kind, &v),
        Err(OneshotError::Unparseable(detail)) => AgentAnswer::unparseable(&detail),
        Err(e) => AgentAnswer::error(e.to_string()),
    }
}

/// Answer one request: preflight, run the one-shot on the blocking pool,
/// validate, and POST the answer. `Ok(())` also when the request was skipped.
async fn process_agent_request(
    client: &reqwest::Client,
    base: &str,
    req: AgentRequest,
) -> Result<(), String> {
    let answer = match preflight(&req, chrono::Utc::now(), agent_timeout()) {
        Preflight::Skip => {
            info!(id = %req.id, "maintenance agent: request already expired — skipping");
            return Ok(());
        }
        Preflight::Refuse(a) => a,
        Preflight::Run {
            cwd,
            tools,
            max_turns,
            timeout,
        } => {
            info!(
                id = %req.id,
                kind = %req.kind,
                tools = ?tools,
                cwd = %cwd.display(),
                timeout_secs = timeout.as_secs(),
                "maintenance agent: running single-shot claude"
            );
            let prompt = req.prompt.clone();
            let joined = spawn_blocking_tracked(move || {
                run_structured_oneshot(&prompt, Some(&cwd), tools, max_turns, timeout)
            })
            .await;
            match joined {
                Ok(outcome) => {
                    if let Err(e) = &outcome {
                        warn!(id = %req.id, timeout = e.is_timeout(), "maintenance agent: one-shot failed: {e}");
                    }
                    answer_from_outcome(&req.kind, outcome)
                }
                Err(e) => AgentAnswer::error(format!("one-shot task failed: {e}")),
            }
        }
    };

    let url = format!(
        "{}/coord/maintenance/agent-requests/{}/answer",
        base.trim_end_matches('/'),
        req.id
    );
    let resp = crate::coord_http::coord_post(client, &url)
        .json(&answer)
        .send()
        .await
        .map_err(|e| format!("POST {url}: {e}"))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    let excerpt: String = body.chars().take(200).collect();
    if !status.is_success() {
        return Err(format!("coord returned {status} for POST {url}: {excerpt}"));
    }
    info!(
        id = %req.id,
        answered_as = match &answer {
            AgentAnswer::Verdict { .. } => "verdict",
            AgentAnswer::Compile { .. } => "compile",
            AgentAnswer::Error { .. } => "error",
        },
        "maintenance agent: answer posted: {excerpt}"
    );
    Ok(())
}

/// One agent-request cycle: claim at most ONE request and answer it. A 404
/// (a coord predating the route) is no work. `Err` only on a transport /
/// non-2xx failure.
pub async fn agent_requests_once() -> Result<(), String> {
    if super::census::load_device_id_pub().is_none() {
        debug!("maintenance agent: no device_id — skipping");
        return Ok(());
    }
    let Some(base) = qontinui_runner_lib::profiles::connected_coord_base() else {
        debug!("maintenance agent: no coord_url configured — skipping");
        return Ok(());
    };

    let url = format!(
        "{}/coord/maintenance/agent-requests?limit=1",
        base.trim_end_matches('/')
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| format!("build maintenance agent http client: {e}"))?;
    let resp = crate::coord_http::coord_get(&client, &url)
        .send()
        .await
        .map_err(|e| format!("GET {url}: {e}"))?;
    let status = resp.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        debug!("maintenance agent: coord has no agent-requests route (404) — no work");
        return Ok(());
    }
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        let excerpt: String = body.chars().take(200).collect();
        return Err(format!("coord returned {status} for GET {url}: {excerpt}"));
    }
    let pull: AgentRequestPull = resp
        .json()
        .await
        .map_err(|e| format!("decode agent-request pull: {e}"))?;

    // At most one per tick, even if a coord ignores `limit`.
    let Some(req) = pull.requests.into_iter().next() else {
        debug!("maintenance agent: no requests");
        return Ok(());
    };
    process_agent_request(&client, &base, req).await
}

// ---------------------------------------------------------------------------
// Tests — the wire-shape parse + the safety-floor decision are the
// load-bearing assertions (the safety re-check itself shells out to git, so
// the deterministic decision logic is what we pin here).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod agent_request_tests {
    use super::*;
    use serde_json::json;

    fn now() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-11-21T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    fn req(kind: &str, tools: &str, repo: Option<&str>, expires: &str) -> AgentRequest {
        AgentRequest {
            id: "r1".into(),
            kind: kind.into(),
            prompt: "p".into(),
            repo_path: repo.map(str::to_string),
            tools: tools.into(),
            expires_at: Some(expires.into()),
        }
    }

    // ── wire shapes ───────────────────────────────────────────────────────

    #[test]
    fn pull_parses_the_contract_shape() {
        let j = r#"{"requests":[
            {"id":"6f1c0a52-0000-4000-8000-000000000001","kind":"verdict",
             "prompt":"Is this branch abandoned?","repo_path":"D:/x/repo",
             "tools":"readonly_git","expires_at":"2026-11-21T13:00:00Z"},
            {"id":"6f1c0a52-0000-4000-8000-000000000002","kind":"compile",
             "prompt":"compile this","repo_path":null,"tools":"none",
             "expires_at":"2026-11-21T13:00:00+00:00"}
        ]}"#;
        let p: AgentRequestPull = serde_json::from_str(j).unwrap();
        assert_eq!(p.requests.len(), 2);
        let v = &p.requests[0];
        assert_eq!(v.id, "6f1c0a52-0000-4000-8000-000000000001");
        assert_eq!(v.kind, "verdict");
        assert_eq!(v.prompt, "Is this branch abandoned?");
        assert_eq!(v.repo_path.as_deref(), Some("D:/x/repo"));
        assert_eq!(v.tools, "readonly_git");
        assert_eq!(v.expires_at.as_deref(), Some("2026-11-21T13:00:00Z"));
        let c = &p.requests[1];
        assert_eq!(c.kind, "compile");
        assert_eq!(c.repo_path, None);
        assert_eq!(c.tools, "none");
    }

    #[test]
    fn empty_pull_and_unknown_kind_decode() {
        let p: AgentRequestPull = serde_json::from_str("{}").unwrap();
        assert!(p.requests.is_empty());
        // A kind/tools a newer coord adds must not fail the decode.
        let p: AgentRequestPull = serde_json::from_str(
            r#"{"requests":[{"id":"x","kind":"summarize","prompt":"p","tools":"web"}]}"#,
        )
        .unwrap();
        assert_eq!(p.requests[0].kind, "summarize");
    }

    #[test]
    fn answers_serialize_to_exactly_the_contract_objects() {
        let v = AgentAnswer::Verdict {
            verdict: "yes".into(),
            confidence: 0.75,
            rationale: "merged".into(),
        };
        assert_eq!(
            serde_json::to_value(&v).unwrap(),
            json!({"verdict":"yes","confidence":0.75,"rationale":"merged"})
        );
        let c = AgentAnswer::Compile {
            result: json!({"name":"n","predicates":[]}),
            rationale: "r".into(),
        };
        assert_eq!(
            serde_json::to_value(&c).unwrap(),
            json!({"result":{"name":"n","predicates":[]},"rationale":"r"})
        );
        assert_eq!(
            serde_json::to_value(AgentAnswer::error("boom")).unwrap(),
            json!({"error":"boom"})
        );
    }

    // ── validation ────────────────────────────────────────────────────────

    #[test]
    fn verdict_accepts_and_clamps() {
        let a = validate_verdict(&json!({"verdict":"no","confidence":1.7,"rationale":"x"}));
        assert_eq!(
            a,
            AgentAnswer::Verdict {
                verdict: "no".into(),
                confidence: 1.0,
                rationale: "x".into()
            }
        );
        let a = validate_verdict(&json!({"verdict":"unknown","confidence":-3,"rationale":""}));
        assert!(matches!(a, AgentAnswer::Verdict { confidence, .. } if confidence == 0.0));
    }

    #[test]
    fn verdict_rejects_bad_or_missing_fields() {
        for bad in [
            json!({"confidence":0.5,"rationale":"x"}),
            json!({"verdict":"maybe","confidence":0.5,"rationale":"x"}),
            json!({"verdict":true,"confidence":0.5,"rationale":"x"}),
            json!({"verdict":"yes","rationale":"x"}),
            json!({"verdict":"yes","confidence":"high","rationale":"x"}),
            json!({"verdict":"yes","confidence":0.5}),
            json!({"verdict":"yes","confidence":0.5,"rationale":3}),
        ] {
            match validate_verdict(&bad) {
                AgentAnswer::Error { error } => assert!(
                    error.starts_with("unparseable claude output: "),
                    "{bad} → {error}"
                ),
                other => panic!("{bad} should be refused, got {other:?}"),
            }
        }
    }

    #[test]
    fn compile_needs_an_object_result() {
        let ok = validate_compile(&json!({"result":{"name":"t"},"rationale":"why"}));
        assert_eq!(
            ok,
            AgentAnswer::Compile {
                result: json!({"name":"t"}),
                rationale: "why".into()
            }
        );
        // Missing rationale is carried as "".
        assert!(matches!(
            validate_compile(&json!({"result":{}})),
            AgentAnswer::Compile { rationale, .. } if rationale.is_empty()
        ));
        for bad in [
            json!({"rationale":"x"}),
            json!({"result":"a string"}),
            json!({"result":[1]}),
            json!({"result":null}),
        ] {
            assert!(
                matches!(validate_compile(&bad), AgentAnswer::Error { .. }),
                "{bad} should be refused"
            );
        }
    }

    #[test]
    fn validate_for_kind_dispatches_and_refuses_unknown_kinds() {
        let v = json!({"verdict":"yes","confidence":0.9,"rationale":"r"});
        assert!(matches!(
            validate_for_kind("verdict", &v),
            AgentAnswer::Verdict { .. }
        ));
        // A verdict object for a compile request is the wrong shape.
        assert!(matches!(
            validate_for_kind("compile", &v),
            AgentAnswer::Error { .. }
        ));
        assert!(matches!(
            validate_for_kind("nope", &v),
            AgentAnswer::Error { .. }
        ));
    }

    #[test]
    fn outcome_errors_map_to_error_answers() {
        assert_eq!(
            answer_from_outcome("verdict", Err(OneshotError::Unparseable("hi".into()))),
            AgentAnswer::error("unparseable claude output: hi")
        );
        assert_eq!(
            answer_from_outcome(
                "verdict",
                Err(OneshotError::Run(
                    crate::ai_provider::claude_cli::PrintRunError::Timeout
                ))
            ),
            AgentAnswer::error("claude CLI timed out")
        );
        let long = "x".repeat(2000);
        match AgentAnswer::error(long) {
            AgentAnswer::Error { error } => assert_eq!(error.chars().count(), MAX_ERROR_CHARS),
            other => panic!("{other:?}"),
        }
    }

    // ── preflight ─────────────────────────────────────────────────────────

    #[test]
    fn expired_requests_are_skipped() {
        let r = req("verdict", "readonly_git", None, "2026-11-21T11:59:59Z");
        assert_eq!(
            preflight(&r, now(), Duration::from_secs(240)),
            Preflight::Skip
        );
    }

    #[test]
    fn timeout_is_capped_by_time_to_expiry() {
        let dir = std::env::temp_dir();
        let r = req(
            "verdict",
            "readonly_git",
            Some(dir.to_str().unwrap()),
            "2026-11-21T12:01:00Z",
        );
        match preflight(&r, now(), Duration::from_secs(240)) {
            Preflight::Run {
                cwd,
                tools,
                max_turns,
                timeout,
            } => {
                assert_eq!(cwd, dir);
                assert_eq!(tools, OneshotTools::ReadonlyGit);
                assert_eq!(max_turns, VERDICT_MAX_TURNS);
                assert_eq!(timeout, Duration::from_secs(60));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn missing_repo_path_dir_is_refused() {
        let r = req(
            "verdict",
            "readonly_git",
            Some("Z:/definitely/not/a/dir/qontinui-xyz"),
            "2026-11-21T13:00:00Z",
        );
        assert!(matches!(
            preflight(&r, now(), Duration::from_secs(240)),
            Preflight::Refuse(AgentAnswer::Error { .. })
        ));
        // readonly_git with no repo at all is refused too.
        let r = req("verdict", "readonly_git", None, "2026-11-21T13:00:00Z");
        assert!(matches!(
            preflight(&r, now(), Duration::from_secs(240)),
            Preflight::Refuse(_)
        ));
    }

    #[test]
    fn compile_without_repo_runs_toolless_in_a_neutral_dir() {
        let r = req("compile", "none", None, "2026-11-21T13:00:00Z");
        match preflight(&r, now(), Duration::from_secs(240)) {
            Preflight::Run {
                cwd,
                tools,
                max_turns,
                timeout,
            } => {
                assert_eq!(cwd, std::env::temp_dir());
                assert_eq!(tools, OneshotTools::None);
                assert_eq!(max_turns, COMPILE_MAX_TURNS);
                assert_eq!(timeout, Duration::from_secs(240));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn unknown_kind_or_tools_is_refused() {
        let r = req("summarize", "none", None, "2026-11-21T13:00:00Z");
        assert!(matches!(
            preflight(&r, now(), Duration::from_secs(240)),
            Preflight::Refuse(_)
        ));
        let r = req("compile", "web", None, "2026-11-21T13:00:00Z");
        assert!(matches!(
            preflight(&r, now(), Duration::from_secs(240)),
            Preflight::Refuse(_)
        ));
    }

    // ── env gate ──────────────────────────────────────────────────────────

    #[test]
    fn env_gate_defaults_off() {
        assert!(!agent_enabled_from(None));
        for off in ["", "0", "false", "off", "no", "nope", "2"] {
            assert!(!agent_enabled_from(Some(off)), "{off:?} must be OFF");
        }
        for on in ["1", "true", "TRUE", "yes", "On", " on "] {
            assert!(agent_enabled_from(Some(on)), "{on:?} must be ON");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pull_parses_exact_plan_shape() {
        let j = r#"{
            "instructions": [
                {
                    "run_id": "11111111-1111-1111-1111-111111111111",
                    "task_id": "22222222-2222-2222-2222-222222222222",
                    "task_name": "branch-reset",
                    "action": {
                        "kind": "checkout_main_ff_pull",
                        "repo_path": "D:/qontinui-root/qontinui-runner-wt-foo",
                        "current_branch": "feat/done",
                        "default_branch": "main"
                    },
                    "armed": false,
                    "reason": "git_clean + branch_is_ancestor_of_origin_main"
                }
            ]
        }"#;
        let p: MaintenancePull = serde_json::from_str(j).unwrap();
        assert_eq!(p.instructions.len(), 1);
        let i = &p.instructions[0];
        assert_eq!(i.task_name, "branch-reset");
        assert!(
            !i.armed,
            "armed must default-parse to the wire value (false)"
        );
        match &i.action {
            MaintenanceAction::CheckoutMainFfPull {
                repo_path,
                current_branch,
                default_branch,
            } => {
                assert_eq!(repo_path, "D:/qontinui-root/qontinui-runner-wt-foo");
                assert_eq!(current_branch, "feat/done");
                assert_eq!(default_branch, "main");
            }
            other => panic!("expected CheckoutMainFfPull, got {other:?}"),
        }
    }

    #[test]
    fn missing_armed_defaults_off_fail_safe() {
        // No `armed` field at all → must parse as false (advisory/log-only).
        let j = r#"{
            "instructions": [
                { "action": { "kind": "checkout_main_ff_pull",
                              "repo_path": "p", "current_branch": "b",
                              "default_branch": "main" } }
            ]
        }"#;
        let p: MaintenancePull = serde_json::from_str(j).unwrap();
        assert!(!p.instructions[0].armed, "missing armed → false (safe)");
        assert_eq!(p.instructions[0].run_id, "");
        assert_eq!(p.instructions[0].task_name, "");
    }

    #[test]
    fn empty_body_parses_to_no_instructions() {
        let p: MaintenancePull = serde_json::from_str("{}").unwrap();
        assert!(p.instructions.is_empty());
        let p2: MaintenancePull = serde_json::from_str(r#"{"instructions":[]}"#).unwrap();
        assert!(p2.instructions.is_empty());
    }

    #[test]
    fn unknown_action_kind_is_forward_compatible() {
        // An action kind a newer coord introduces must NOT fail the whole
        // pull — it deserializes to Unknown (logged + ignored, never acted on).
        let j = r#"{
            "instructions": [
                { "action": { "kind": "rebuild_and_test", "repo": "x" },
                  "armed": true }
            ]
        }"#;
        let p: MaintenancePull = serde_json::from_str(j).unwrap();
        assert_eq!(p.instructions.len(), 1);
        assert!(matches!(
            p.instructions[0].action,
            MaintenanceAction::Unknown
        ));
        // And executing an Unknown action mutates nothing.
        assert!(matches!(
            execute_instruction(&p.instructions[0]),
            ExecOutcome::Skipped
        ));
    }

    // ── Bounded-execution regression tests (Phase 2) ────────────────────
    //
    // These fail if the `run_with_timeout` routing is reverted: a bare
    // `.output()` would block for the sleeper's full ~60s and blow the
    // elapsed-time assertion (and, in the pool test, the outer bound).

    /// A command that blocks for far longer than any budget we hand it.
    /// Mirrors `process_helpers::timeout_tests::sleeper`.
    fn sleeper() -> std::process::Command {
        #[cfg(target_os = "windows")]
        {
            let mut c = crate::process_helpers::no_window("cmd.exe");
            c.args(["/C", "ping -n 60 127.0.0.1"]);
            c
        }
        #[cfg(not(target_os = "windows"))]
        {
            let mut c = crate::process_helpers::no_window("sh");
            c.args(["-c", "sleep 60"]);
            c
        }
    }

    #[test]
    fn a_hung_git_returns_within_budget_as_an_error_not_a_verdict() {
        let budget = Duration::from_millis(300);
        let started = std::time::Instant::now();
        let got = run_git_capture(sleeper(), "/some/repo", &["status", "--porcelain"], budget);
        let elapsed = started.elapsed();

        let err = got.expect_err("a timed-out git must be Err, never Ok((false, ..))");
        assert!(
            err.contains("budget") && err.contains("pid="),
            "the timeout error must carry the budget and the killed pid; got {err:?}"
        );
        assert!(
            err.contains("reaped=true"),
            "the killed child must be reaped, not leaked as a zombie; got {err:?}"
        );
        assert!(
            elapsed < budget * 8,
            "run_git_capture blocked for {elapsed:?} against a {budget:?} budget"
        );
    }

    /// Edge 9171555e: the pre-reset dirty gate must see an untracked file that
    /// `status.showUntrackedFiles=no` hides from plain porcelain.
    #[test]
    fn recheck_safety_refuses_an_untracked_file_hidden_by_show_untracked_no() {
        let repo = super::super::dirty::repo_with_hidden_untracked_file();
        let path = repo.path().to_string_lossy().to_string();
        match recheck_safety(&path, "main", "main") {
            SafetyVerdict::Refuse(reason) => assert!(
                reason.contains("is dirty"),
                "the refusal must be the dirty gate's, got {reason:?}"
            ),
            SafetyVerdict::Ok => panic!("a tree with untracked work must never be reset"),
        }
    }

    #[test]
    fn recheck_safety_refuses_when_git_times_out() {
        // A runner that behaves exactly as a timed-out `git_capture_with_timeout`
        // does, for EVERY subcommand. The tree is unknowable, so all three
        // floor rules must refuse rather than fall through.
        let timed_out =
            |_dir: &str, args: &[&str], _t: Duration| -> Result<(bool, String, String), String> {
                Err(format!(
                    "git {args:?} exceeded its 30s budget and was killed (pid=4242, reaped=true)"
                ))
            };
        // Use a directory that really exists so the is_dir() gate passes and
        // the git rules are what decide.
        let dir = std::env::temp_dir();
        let dir = dir.to_string_lossy().to_string();

        let v = recheck_safety_with(&dir, "feat/x", "main", &timed_out);
        match v {
            SafetyVerdict::Refuse(reason) => {
                assert!(
                    reason.contains("budget"),
                    "the refusal must name the timeout, got {reason:?}"
                );
            }
            SafetyVerdict::Ok => panic!("a tree we could not read must never be declared safe"),
        }
    }

    #[test]
    fn armed_execute_reports_a_git_timeout_as_failed_and_never_pulls() {
        // Safety passes; the `checkout` step then times out. The outcome must
        // be Failed (carrying pid + budget) and `pull` must never be reached.
        let calls = std::cell::RefCell::new(Vec::<String>::new());
        let runner =
            |_dir: &str, args: &[&str], _t: Duration| -> Result<(bool, String, String), String> {
                calls.borrow_mut().push(args.join(" "));
                match args.first().copied() {
                    // Safety floor: clean tree, expected branch, merged.
                    Some("status") | Some("merge-base") => Ok((true, String::new(), String::new())),
                    Some("rev-parse") => Ok((true, "feat/x\n".to_string(), String::new())),
                    // The mutation step hangs.
                    Some("checkout") => Err(
                        "git [\"checkout\", \"main\"] exceeded its 30s budget and was killed \
                     (pid=4242, reaped=true)"
                            .to_string(),
                    ),
                    other => panic!("unexpected git call {other:?}"),
                }
            };

        let dir = std::env::temp_dir().to_string_lossy().to_string();
        let instr = MaintenanceInstruction {
            run_id: "r".into(),
            task_id: "t".into(),
            task_name: "branch-reset".into(),
            action: MaintenanceAction::CheckoutMainFfPull {
                repo_path: dir,
                current_branch: "feat/x".into(),
                default_branch: "main".into(),
            },
            armed: true,
            reason: "test".into(),
        };

        match execute_instruction_with(&instr, &runner) {
            ExecOutcome::Failed(msg) => {
                assert!(
                    msg.contains("budget") && msg.contains("pid="),
                    "a timeout must be reported with its pid + budget, got {msg:?}"
                );
            }
            other => panic!("a hung git must be Failed, never {other:?}"),
        }
        assert!(
            !calls.borrow().iter().any(|c| c.starts_with("pull")),
            "a failed checkout must never advance to pull; calls were {:?}",
            calls.borrow()
        );
    }

    #[test]
    fn recheck_refuses_nonexistent_path() {
        let v = recheck_safety(
            "D:/qontinui-root/definitely-not-a-real-checkout-xyz",
            "main",
            "main",
        );
        assert!(matches!(v, SafetyVerdict::Refuse(_)));
    }

    #[test]
    fn dry_run_never_executes_even_when_safe_path_unknown() {
        // An un-armed instruction over a non-existent path: the safety
        // re-check refuses (path missing) BEFORE the dry-run log, so the
        // outcome is Skipped — and crucially never Done/Failed (no mutation).
        let instr = MaintenanceInstruction {
            run_id: "r".into(),
            task_id: "t".into(),
            task_name: "branch-reset".into(),
            action: MaintenanceAction::CheckoutMainFfPull {
                repo_path: "D:/qontinui-root/definitely-not-a-real-checkout-xyz".into(),
                current_branch: "feat/x".into(),
                default_branch: "main".into(),
            },
            armed: false,
            reason: "test".into(),
        };
        assert!(matches!(execute_instruction(&instr), ExecOutcome::Skipped));
    }
}
