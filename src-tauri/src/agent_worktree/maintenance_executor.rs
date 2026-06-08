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
//! `census::coord_http_base_pub`), the same `tokio::time::interval` +
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

/// Run `git -C <dir> <args>` capturing output. `Ok((success, stdout, stderr))`
/// — `success` is the process exit status. `Err` only on spawn failure.
fn git_capture(dir: &str, args: &[&str]) -> Result<(bool, String, String), String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("spawn git {args:?} in {dir}: {e}"))?;
    Ok((
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    ))
}

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
///   1. `git status --porcelain` EMPTY (clean).
///   2. `git rev-parse --abbrev-ref HEAD` == `current_branch`.
///   3. `git merge-base --is-ancestor <current_branch> origin/<default>`.
/// On a git spawn failure we conservatively REFUSE (never act on a tree we
/// can't reason about).
fn recheck_safety(repo_path: &str, current_branch: &str, default_branch: &str) -> SafetyVerdict {
    if !Path::new(repo_path).is_dir() {
        return SafetyVerdict::Refuse(format!("repo_path {repo_path} is not a directory"));
    }

    // (1) Dirty gate — any uncommitted/untracked change → refuse.
    match git_capture(repo_path, &["status", "--porcelain"]) {
        Ok((true, stdout, _)) if !stdout.trim().is_empty() => {
            return SafetyVerdict::Refuse(format!(
                "{repo_path} is dirty (git status --porcelain non-empty) — refusing reset"
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
    match git_capture(repo_path, &["rev-parse", "--abbrev-ref", "HEAD"]) {
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
    match git_capture(
        repo_path,
        &[
            "merge-base",
            "--is-ancestor",
            current_branch,
            &origin_default,
        ],
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
    let verdict = recheck_safety(repo_path, current_branch, default_branch);
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
    match git_capture(repo_path, &["checkout", default_branch]) {
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

    match git_capture(repo_path, &["pull", "--ff-only"]) {
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

/// Read `active_tenant_id` from `~/.qontinui/machine.json` (advisory — coord
/// DB-resolves the real tenant). `None` for single-tenant operators, which is
/// fine. Mirrors `census::resolve_tenant_id` / `fs_backstop::resolve_tenant_id`
/// (both private; the read is trivial and attribution is best-effort).
fn resolve_tenant_id() -> Option<uuid::Uuid> {
    let path = dirs::home_dir()?.join(".qontinui").join("machine.json");
    let bytes = std::fs::read(path).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let raw = value.get("active_tenant_id").and_then(|v| v.as_str())?;
    uuid::Uuid::parse_str(raw.trim()).ok()
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
    let Some(base) = git_ops_client::coord_http_base() else {
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
        let outcome =
            match tokio::task::spawn_blocking(move || execute_instruction(&instr_clone)).await {
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
    let base = match super::census::coord_http_base_pub() {
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

    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(secs));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            if let Err(e) = tick_once().await {
                warn!("maintenance: {e}");
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Tests — the wire-shape parse + the safety-floor decision are the
// load-bearing assertions (the safety re-check itself shells out to git, so
// the deterministic decision logic is what we pin here).
// ---------------------------------------------------------------------------

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
