//! Scheduled `RemoteAgent` launch through the runner's OWN spawn seam.
//!
//! Plan `2026-09-13-nightly-return-to-main-sweep`, Phase 5b.
//!
//! ## What was wrong
//!
//! `SchedulerService::launch_remote_agent` used to `POST /prompts/run` to the
//! runner's own HTTP API, whose `run_prompt` handler spawns
//! `qontinui-claude-config/scripts/spawn-independent-claude.py`. Three defects,
//! all measured in that plan's Phase 0 (2026-09-13, runner build `82a0c8b75`):
//!
//! 1. **It needs a `qontinui-claude-config` checkout.** A runner-only device —
//!    the population the nightly `/return-to-main` job is meant to reach — has
//!    no such checkout, so the launch fails there before any session exists.
//!    And even where the checkout exists, the path provisions no bundled fleet
//!    commands or skills: `/whereami` resolved on the operator box only because
//!    that box has the config repo.
//! 2. **The scheduler could not observe completion.** The session exited 0 and
//!    was still marked `failed: "Auto-failed by zombie sweep: no sessions ever
//!    started"`, with the scheduler history stuck at `running` — the spawn
//!    wrapper never wrote the `task_runs` row the poller watches.
//! 3. **The prompt was buried.** The task prompt landed at line 291 of a
//!    387-line generated wrapper, and a probe session refused its task as a
//!    suspected prompt injection.
//!
//! ## What this does instead
//!
//! The same primitives the gate-continuation and looping-agent spawns use —
//! `.mcp.json` provisioning, the bundled fleet commands and skills, the
//! most-available-account pin, the trust pre-accept, the runner-context
//! briefing — feed ONE headless `claude` child spawned by
//! [`crate::agent_runtime::spawn_claude_child`], with the task prompt as the
//! child's primary instruction and nothing wrapped around it. The scheduler
//! then AWAITS the child: exit code 0 is success, anything else is a failure
//! with the code, and the task's `timeout_seconds` kills it. Completion is the
//! child exiting, which needs no `task_runs` row and no zombie sweep.
//!
//! ## Why a headless child and not the visible terminal seam
//!
//! The plan's vet pointed at `run_continuation_terminal` (a visible, INTERACTIVE
//! tab). The property it wanted from that seam was *provisioning built in and
//! no ccfg dependency* — which the headless child also has — but an interactive
//! session has no exit: it idles at its prompt after the work, so "the job
//! finished" is unobservable without an idle heuristic, `--max-turns` is a
//! print-mode flag the interactive CLI ignores, and the terminal seam needs a
//! Tauri webview, which is exactly what a headless Linux runner lacks — the
//! plan's Linux Phase-7 continuation, gate `c59bd18e`, recorded
//! `consumed_outcome: "spawn_failed: no Tauri AppHandle (runner has no webview
//! runtime) — cannot open a visible terminal"` at 2026-09-14T11:13Z
//! (`coord_gate_list`, work unit `2026-09-13-nightly-return-to-main-sweep`). A
//! scheduled job is
//! unattended by definition — nobody is at the keyboard to answer an
//! `AskUserQuestion` — so the interactive property bought nothing and cost the
//! completion signal. The headless child is the shape whose end is its exit.
//!
//! The child's stdout/stderr are pumped to `~/.qontinui/scheduler-runs/<execution>.log`
//! so a night's transcript survives the process.
//!
//! ## The child's tree, and the runner's own end
//!
//! The child is the leader of its own process group (a job object on Windows),
//! so the timeout kill reaches the `bash` / `git` / MCP descendants a
//! `/return-to-main` session is mid-way through, not `claude` alone; on a clean
//! exit the group is released rather than reaped, and on an unreadable exit
//! status it is reaped, matching [`crate::process_helpers::run_with_timeout`]
//! on both arms. What
//! this path does NOT do is watch the scheduler's `stop_signal` or the
//! runtime's shutdown. What happens to the child then depends on HOW the
//! runner ends: on the `std::process::exit` paths `main.rs` takes no
//! destructor runs, the child outlives the runner, and its history row stays
//! `running` until the next runner start's reconciler looks at it; on a path
//! that DROPS the detached task holding the guard (a runtime shutdown, a
//! `JoinHandle::abort`) the guard's drop kills the whole tree with no
//! `finalize_async_execution`; and under a systemd unit with
//! `KillMode=control-group` the cgroup kill takes the child regardless of
//! process group. That is the same posture every other `tokio::spawn`ed
//! scheduler task has today (`stop_scheduler_service` has no caller), stated
//! here with its conditions rather than implied.

use std::path::PathBuf;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::{info, warn};

use qontinui_types::scheduler::McpConnectionRef;

/// Everything a scheduled `RemoteAgent` task carries that the spawn needs.
#[derive(Debug, Clone)]
pub(crate) struct RemoteAgentSpec {
    pub task_name: String,
    /// The scheduler history row this run writes — names the log file.
    pub execution_id: String,
    pub prompt: String,
    pub working_directory: Option<String>,
    pub model: Option<String>,
    pub allowed_tools: Vec<String>,
    pub mcp_connections: Vec<McpConnectionRef>,
    pub max_turns: u32,
    pub timeout: Duration,
}

/// How a scheduled run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteAgentOutcome {
    pub success: bool,
    pub error: Option<String>,
    pub exit_code: Option<i64>,
    pub timed_out: bool,
}

/// A launched run: the identity the scheduler records, plus the future that
/// resolves when the child exits (or is killed at the timeout).
pub(crate) struct RemoteAgentLaunch {
    /// The pinned Claude session id — what the scheduler history row records
    /// as its `session_id`.
    pub session_id: String,
    pub workdir: String,
    pub log_path: Option<PathBuf>,
    pub completion: std::pin::Pin<Box<dyn std::future::Future<Output = RemoteAgentOutcome> + Send>>,
}

/// Where a scheduled run's transcript lands: `~/.qontinui/scheduler-runs/`.
fn scheduler_logs_root() -> Option<PathBuf> {
    qontinui_runner_lib::ambient::qontinui_dir().map(|d| d.join("scheduler-runs"))
}

/// The cwd the session runs in: the task's `working_directory`, else the
/// workspace root (the directory whose depth-1 children are the primary
/// checkouts — what `/return-to-main` needs), else the runner's own cwd.
///
/// A configured directory that does not exist is a launch FAILURE, not a
/// silent fallback: the task named a place and it is not there.
pub(crate) fn resolve_workdir(configured: Option<&str>) -> Result<PathBuf, String> {
    if let Some(dir) = configured.map(str::trim).filter(|d| !d.is_empty()) {
        let p = PathBuf::from(dir);
        return if p.is_dir() {
            Ok(p)
        } else {
            Err(format!(
                "working_directory '{dir}' is not a directory on this device"
            ))
        };
    }
    if let Some(root) = crate::workspace_paths::workspace_root() {
        return Ok(root);
    }
    std::env::current_dir().map_err(|e| {
        format!("no working_directory, no workspace root, and the runner's cwd is unreadable: {e}")
    })
}

/// The prompt the child receives: the task prompt FIRST and verbatim, with the
/// declared MCP connection refs (if any) appended as a trailing section rather
/// than a header. Phase 0 measured a session refusing a task that arrived
/// inside a wrapper; the task must be the first thing the model reads.
pub(crate) fn compose_prompt(prompt: &str, mcp_connections: &[McpConnectionRef]) -> String {
    if mcp_connections.is_empty() {
        return prompt.to_string();
    }
    let mut out = String::from(prompt.trim_end());
    out.push_str(
        "\n\n---\n\n## Requested MCP connections\n\nThis scheduled task declared the following MCP \
         connection refs. They are resolved against the runner's existing MCP config; \
         per-call overrides are not wired.\n\n",
    );
    for conn in mcp_connections {
        match &conn.url {
            Some(url) => out.push_str(&format!("- **{}** (override URL: {url})\n", conn.name)),
            None => out.push_str(&format!(
                "- **{}** (use the runner's configured URL)\n",
                conn.name
            )),
        }
    }
    out
}

/// The CLI flags that make the child a bounded, single-shot run. Pure, so the
/// argv shape is pinned by a test rather than by whatever the CLI accepted last.
///
/// `-p` (print mode) is what makes `--max-turns` honoured and what makes the
/// child EXIT when the task is done — an interactive session would idle at its
/// prompt forever. `--dangerously-skip-permissions` for the same reason every
/// autonomous spawn carries it: nobody is at the keyboard to click Allow.
pub(crate) fn claude_args(
    session_id: &str,
    model: Option<&str>,
    allowed_tools: &[String],
    max_turns: u32,
) -> Vec<String> {
    let mut args = vec![
        "-p".to_string(),
        "--dangerously-skip-permissions".to_string(),
        "--session-id".to_string(),
        session_id.to_string(),
        "--max-turns".to_string(),
        max_turns.to_string(),
    ];
    if let Some(m) = model.map(str::trim).filter(|m| !m.is_empty()) {
        args.push("--model".to_string());
        args.push(m.to_string());
    }
    let tools: Vec<&str> = allowed_tools
        .iter()
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .collect();
    if !tools.is_empty() {
        args.push("--allowedTools".to_string());
        args.extend(tools.into_iter().map(str::to_string));
    }
    args
}

/// Classify a child's end into the scheduler's `(success, error)` pair.
pub(crate) fn classify_exit(
    status: Result<Option<i32>, String>,
    timed_out: bool,
    timeout: Duration,
) -> RemoteAgentOutcome {
    if timed_out {
        return RemoteAgentOutcome {
            success: false,
            error: Some(format!(
                "scheduled session exceeded timeout_seconds={} and was killed",
                timeout.as_secs()
            )),
            exit_code: None,
            timed_out: true,
        };
    }
    match status {
        Ok(Some(0)) => RemoteAgentOutcome {
            success: true,
            error: None,
            exit_code: Some(0),
            timed_out: false,
        },
        Ok(Some(code)) => RemoteAgentOutcome {
            success: false,
            error: Some(format!("scheduled session exited with code {code}")),
            exit_code: Some(i64::from(code)),
            timed_out: false,
        },
        Ok(None) => RemoteAgentOutcome {
            success: false,
            error: Some("scheduled session was terminated by a signal".to_string()),
            exit_code: None,
            timed_out: false,
        },
        Err(e) => RemoteAgentOutcome {
            success: false,
            error: Some(format!("waiting on the scheduled session failed: {e}")),
            exit_code: None,
            timed_out: false,
        },
    }
}

/// Launch the run. Provisioning, account pin and the spawn happen HERE; the
/// returned `completion` future owns the child and resolves when it ends.
pub(crate) async fn launch(
    spec: RemoteAgentSpec,
    bound_port: Option<u16>,
) -> Result<RemoteAgentLaunch, String> {
    let workdir = resolve_workdir(spec.working_directory.as_deref())?;
    let workdir_s = workdir.to_string_lossy().to_string();

    // UNATTENDED spawn — respect the critical resource floor, exactly like the
    // gate-continuation and looping-agent spawns. A refusal is a launch
    // failure the scheduler's backoff path retries; it never starts a `claude`
    // on a starved box.
    crate::resource_guard::precheck_spawn("scheduled remote agent", false)?;

    // Provision what a session on a runner-only device has no other source
    // for: the coord-mcp door, the bundled subagent definitions, fleet
    // commands and fleet skills (`/return-to-main` among them). Every one of
    // these skips a destination the enclosing repo tracks, so a workspace root
    // whose `.claude/` is a checkout is left alone.
    let coord_mcp = crate::coord_mcp::provision_coord_mcp_for_session(&workdir_s, bound_port, None);
    crate::session_assets::provision_session_assets(&workdir_s);

    // Most-available account, so the child does not spawn under a
    // quota-exhausted default and die on its first request.
    let _ = qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked(
        crate::ai_provider::pick_best_account,
    )
    .await;

    let session_id = uuid::Uuid::new_v4().to_string();
    let prompt = compose_prompt(&spec.prompt, &spec.mcp_connections);
    let args = claude_args(
        &session_id,
        spec.model.as_deref(),
        &spec.allowed_tools,
        spec.max_turns,
    );

    let (mut child, _preconditions) =
        crate::agent_runtime::spawn_claude_child(&workdir_s, &prompt, None, coord_mcp, &args, true)
            .await
            .map_err(|e| format!("spawn scheduled session: {e:#}"))?;
    // The whole tree, addressable by one kill (see the module doc).
    let tree = crate::process_helpers::ChildTreeGuard::attach_armed_tokio(&child);

    let pid = child.id();
    if let Some(p) = pid {
        // Exempt from the session-tracking health check for its lifetime — a
        // headless child legitimately has no PTY lifecycle record.
        crate::session::tracking_health::register_headless_claude_pid(p);
    }
    info!(
        "scheduler: RemoteAgent task '{}' spawned headless claude pid={:?} session_id={} cwd={} max_turns={} timeout={}s",
        spec.task_name,
        pid,
        session_id,
        workdir_s,
        spec.max_turns,
        spec.timeout.as_secs()
    );

    let log_path = scheduler_logs_root().map(|d| d.join(format!("{}.log", spec.execution_id)));
    let pump_log = log_path.clone();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let timeout = spec.timeout;
    let task_name = spec.task_name.clone();

    let completion = Box::pin(async move {
        let pump = tokio::spawn(pump_to_log(stdout, stderr, pump_log));
        let waited = tokio::time::timeout(timeout, child.wait()).await;
        let (status, timed_out) = match waited {
            Ok(Ok(st)) => {
                // Clean end: release the group, do not reap what it left.
                tree.disarm();
                (Ok(st.code()), false)
            }
            Ok(Err(e)) => {
                // A tokio `wait()` error means the leader's status could not
                // be read (in effect ECHILD). That says nothing about its
                // descendants — a process group outlives its leader, and the
                // control test on the tree guard relies on exactly that — so
                // an unreadable status is closer to "unknown" than to
                // "clean", and the guard is DROPPED (the group is reaped),
                // as `run_with_timeout` does on its wait-error arm. Unreachable
                // in practice: nothing in this binary does `waitpid(-1)` or
                // ignores SIGCHLD, so tokio cannot lose the status today.
                drop(tree);
                (Err(e.to_string()), false)
            }
            Err(_elapsed) => {
                warn!(
                    "scheduler: RemoteAgent task '{task_name}' hit its {}s timeout — killing pid={pid:?} and its process tree",
                    timeout.as_secs()
                );
                // Drop = kill the whole group / job; then reap the leader.
                drop(tree);
                let _ = child.kill().await;
                (Ok(None), true)
            }
        };
        // Let the pump drain what the child wrote before it died.
        let _ = tokio::time::timeout(Duration::from_secs(5), pump).await;
        if let Some(p) = pid {
            crate::session::tracking_health::unregister_headless_claude_pid(p);
        }
        classify_exit(status, timed_out, timeout)
    });

    Ok(RemoteAgentLaunch {
        session_id,
        workdir: workdir_s,
        log_path,
        completion,
    })
}

/// Copy the child's stdout and stderr, line by line, into the run's log file.
/// Fail-soft: a log that cannot be opened costs the transcript, not the run.
async fn pump_to_log(
    stdout: Option<tokio::process::ChildStdout>,
    stderr: Option<tokio::process::ChildStderr>,
    log_path: Option<PathBuf>,
) {
    let mut log = match log_path {
        Some(p) => {
            if let Some(parent) = p.parent() {
                let _ = tokio::fs::create_dir_all(parent).await;
            }
            match tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&p)
                .await
            {
                Ok(f) => Some(f),
                Err(e) => {
                    warn!("scheduler: cannot open run log {}: {e}", p.display());
                    None
                }
            }
        }
        None => None,
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(256);
    let mut readers = Vec::new();
    if let Some(out) = stdout {
        let tx = tx.clone();
        readers.push(tokio::spawn(async move {
            let mut lines = BufReader::new(out).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                if tx.send(l).await.is_err() {
                    break;
                }
            }
        }));
    }
    if let Some(err) = stderr {
        let tx = tx.clone();
        readers.push(tokio::spawn(async move {
            let mut lines = BufReader::new(err).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                if tx.send(format!("[stderr] {l}")).await.is_err() {
                    break;
                }
            }
        }));
    }
    drop(tx);
    while let Some(line) = rx.recv().await {
        if let Some(f) = log.as_mut() {
            let _ = f.write_all(line.as_bytes()).await;
            let _ = f.write_all(b"\n").await;
        }
    }
    for r in readers {
        let _ = r.await;
    }
    if let Some(f) = log.as_mut() {
        let _ = f.flush().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_is_first_and_verbatim_when_no_mcp_refs() {
        assert_eq!(
            compose_prompt("/return-to-main --shadow", &[]),
            "/return-to-main --shadow"
        );
    }

    #[test]
    fn mcp_refs_trail_the_prompt_rather_than_head_it() {
        let refs = vec![
            McpConnectionRef {
                name: "coord".into(),
                url: None,
            },
            McpConnectionRef {
                name: "web".into(),
                url: Some("http://127.0.0.1:1/mcp".into()),
            },
        ];
        let p = compose_prompt("do the thing", &refs);
        assert!(p.starts_with("do the thing\n\n---\n\n## Requested MCP connections"));
        assert!(p.contains("- **coord** (use the runner's configured URL)"));
        assert!(p.contains("- **web** (override URL: http://127.0.0.1:1/mcp)"));
    }

    #[test]
    fn args_are_print_mode_bounded_and_pinned() {
        let args = claude_args("sid-1", None, &[], 200);
        assert_eq!(
            args,
            vec![
                "-p",
                "--dangerously-skip-permissions",
                "--session-id",
                "sid-1",
                "--max-turns",
                "200"
            ]
        );
    }

    #[test]
    fn args_carry_model_and_one_token_per_allowed_tool() {
        let tools = vec!["Bash".to_string(), " Read ".to_string(), "".to_string()];
        let args = claude_args("sid", Some("claude-opus-5"), &tools, 5);
        assert_eq!(
            args,
            vec![
                "-p",
                "--dangerously-skip-permissions",
                "--session-id",
                "sid",
                "--max-turns",
                "5",
                "--model",
                "claude-opus-5",
                "--allowedTools",
                "Bash",
                "Read"
            ]
        );
    }

    #[test]
    fn exit_zero_is_the_only_success() {
        let t = Duration::from_secs(60);
        assert!(classify_exit(Ok(Some(0)), false, t).success);
        let non_zero = classify_exit(Ok(Some(3)), false, t);
        assert!(!non_zero.success);
        assert_eq!(non_zero.exit_code, Some(3));
        assert!(non_zero.error.as_deref().unwrap().contains("code 3"));
        let signalled = classify_exit(Ok(None), false, t);
        assert!(!signalled.success);
        assert!(signalled.error.as_deref().unwrap().contains("signal"));
        let failed = classify_exit(Err("boom".into()), false, t);
        assert!(failed.error.as_deref().unwrap().contains("boom"));
    }

    #[test]
    fn timeout_wins_over_whatever_the_status_says() {
        let out = classify_exit(Ok(Some(0)), true, Duration::from_secs(3600));
        assert!(!out.success);
        assert!(out.timed_out);
        assert!(out
            .error
            .as_deref()
            .unwrap()
            .contains("timeout_seconds=3600"));
    }

    #[test]
    fn a_configured_directory_that_is_missing_is_a_launch_failure() {
        let missing =
            std::env::temp_dir().join(format!("qontinui-no-such-{}", uuid::Uuid::new_v4()));
        let err = resolve_workdir(Some(missing.to_str().unwrap())).unwrap_err();
        assert!(err.contains("is not a directory"));
    }

    #[test]
    fn a_configured_directory_that_exists_is_used_verbatim() {
        let dir = std::env::temp_dir();
        assert_eq!(resolve_workdir(Some(dir.to_str().unwrap())).unwrap(), dir);
    }
}
