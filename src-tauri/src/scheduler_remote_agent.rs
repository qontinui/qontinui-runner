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
pub(crate) fn scheduler_logs_root() -> Option<PathBuf> {
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
///
/// When the operator selected the allow-list session posture
/// (`settings.claude_session_permission`, plan
/// `2026-10-09-spec-front-end-of-the-software-factory` D7), bypass is replaced by
/// `--permission-mode dontAsk` and a tool list that can only NARROW the
/// operator's: the INTERSECTION of the task's `allowed_tools` and the
/// operator's list (review M1), or the operator's list when the task declares
/// none. A task whose tools share nothing with the operator's list is refused —
/// running it with the task's wider list, or with none, would both be wrong.
/// `--allowedTools` is rendered by the shared
/// [`allowed_tools_args`](crate::claude_session::launch_spec::allowed_tools_args).
pub(crate) fn claude_args(
    session_id: &str,
    model: Option<&str>,
    allowed_tools: &[String],
    max_turns: u32,
    session_permission: &crate::claude_session::launch_spec::SessionPermissionSetting,
) -> Result<Vec<String>, String> {
    use crate::claude_session::launch_spec::{
        allowed_tools_args, intersect_tool_lists, PermissionMode,
    };

    let task_tools = allowed_tools_args(allowed_tools);
    let (permission, tools_tail) = match session_permission.apply(PermissionMode::DangerouslySkip) {
        PermissionMode::AllowList { tools } => {
            let effective = if task_tools.is_empty() {
                tools.clone()
            } else {
                let kept = intersect_tool_lists(&tools, allowed_tools);
                if kept.is_empty() {
                    return Err(format!(
                        "scheduled task refused: none of its allowed_tools {allowed_tools:?} is on \
                         the operator's session allow-list {tools:?}"
                    ));
                }
                kept
            };
            (
                PermissionMode::AllowList { tools: Vec::new() }.render(),
                allowed_tools_args(&effective),
            )
        }
        other => (other.render(), task_tools),
    };

    let mut args = vec!["-p".to_string()];
    args.extend(permission);
    args.extend([
        "--session-id".to_string(),
        session_id.to_string(),
        "--max-turns".to_string(),
        max_turns.to_string(),
    ]);
    if let Some(m) = model.map(str::trim).filter(|m| !m.is_empty()) {
        args.push("--model".to_string());
        args.push(m.to_string());
    }
    args.extend(tools_tail);
    Ok(args)
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
    // for: the coord-mcp door, the bundled fleet commands, the bundled fleet
    // skills (`/return-to-main` among them) and the bundled subagent
    // definitions.
    //
    // The working directory is the OPERATOR's choice, so the fleet trees go
    // through the same gate the terminal chokepoint uses
    // (`isolated_edit::provision_fleet_for_cwd`): where the cwd's `.claude/` is
    // tracked by its repository, commands and skills are skipped whole-tree,
    // and agent definitions are written only into an allocated agent worktree
    // — an operator's checkout gets no untracked agent files. Elsewhere all
    // three are provisioned as before, each skipping a destination the
    // enclosing repo tracks.
    let coord_mcp = crate::coord_mcp::provision_coord_mcp_for_session(&workdir_s, bound_port, None);
    crate::agent_worktree::isolated_edit::provision_fleet_for_cwd(&workdir_s);

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
        &crate::settings::get_claude_session_permission(),
    )?;

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

/// Forward `reader`'s lines to `tx`, decoding lossily. Byte-wise (`read_until`)
/// rather than `lines()`, which errors on non-UTF-8 and would end the loop,
/// close the pipe and let the child die on SIGPIPE/EPIPE. Keeps draining until
/// EOF or a real read error; stops early only when the receiver is gone.
async fn read_lossy_lines<R: tokio::io::AsyncRead + Unpin>(
    reader: R,
    prefix: &str,
    tx: tokio::sync::mpsc::Sender<String>,
) {
    use tokio::io::AsyncBufReadExt;
    let mut reader = BufReader::new(reader);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                while matches!(buf.last(), Some(b'\n' | b'\r')) {
                    buf.pop();
                }
                let line = format!("{prefix}{}", String::from_utf8_lossy(&buf));
                if tx.send(line).await.is_err() {
                    break;
                }
            }
        }
    }
}

/// Copy the child's stdout and stderr, line by line, into the run's log file.
/// Fail-soft: a log that cannot be opened costs the transcript, not the run.
pub(crate) async fn pump_to_log(
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
            read_lossy_lines(out, "", tx).await;
        }));
    }
    if let Some(err) = stderr {
        let tx = tx.clone();
        readers.push(tokio::spawn(async move {
            read_lossy_lines(err, "[stderr] ", tx).await;
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

    /// A3: the launch provisions the fleet trees through the operator-cwd gate,
    /// never through the ungated per-provisioner calls. Pinned on the source,
    /// because `launch` spawns a real `claude`.
    #[test]
    fn scheduler_launch_provisions_through_the_operator_cwd_gate() {
        let src = include_str!("scheduler_remote_agent.rs");
        let launch = &src[src.find("pub(crate) async fn launch(").expect("launch")..];
        let launch = &launch[..launch.find("\n}\n").expect("end of launch")];
        assert!(launch.contains("isolated_edit::provision_fleet_for_cwd(&workdir_s)"));
        for ungated in [
            "provision_agent_definitions_recorded(",
            "provision_fleet_commands_for_session(",
            "provision_fleet_skills_for_session(",
        ] {
            assert!(!launch.contains(ungated), "launch calls {ungated} ungated");
        }
    }

    /// A3: through that gate, an operator's checkout whose `.claude/` the
    /// repository tracks (not an allocated worktree) receives no agent files.
    #[test]
    fn scheduler_gate_writes_no_agent_files_into_a_tracked_operator_checkout() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path();
        crate::provision_guard::test_support::git_init(root);
        std::fs::create_dir_all(root.join(".claude/commands")).unwrap();
        std::fs::write(root.join(".claude/commands/own.md"), b"own").unwrap();
        crate::provision_guard::test_support::git_add(
            root,
            std::path::Path::new(".claude/commands/own.md"),
        );

        crate::agent_worktree::isolated_edit::provision_fleet_for_cwd(root.to_str().unwrap());

        let agents = root.join(".claude/agents");
        let written = std::fs::read_dir(&agents).map(|d| d.count()).unwrap_or(0);
        assert_eq!(
            written, 0,
            "no agent file lands in an operator's tracked checkout"
        );
        assert_eq!(
            std::fs::read(root.join(".claude/commands/own.md")).unwrap(),
            b"own",
            "the checkout's own tracked command is byte-unchanged"
        );
        let commands = std::fs::read_dir(root.join(".claude/commands"))
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(commands, vec!["own.md".to_string()], "no command was added");

        // The skip is RECORDED, not silent: the ledger holds the operator-checkout
        // agent row, and the commands pass as repo-authored.
        let ledger = crate::capability_manifest::session_provision_ledger(root.to_str().unwrap())
            .expect("the gate records its skips against this workdir");
        let agent_rows: Vec<_> = ledger
            .reports
            .iter()
            .filter(|r| r.capability == "agent_definitions")
            .collect();
        assert_eq!(agent_rows.len(), 1, "{agent_rows:?}");
        assert_eq!(agent_rows[0].written, 0);
        assert!(
            agent_rows[0].skipped.iter().any(|u| matches!(
                &u.reason,
                crate::capability_manifest::SkipReason::Unresolved(why)
                    if why.contains("not an allocated agent worktree")
            )),
            "{:?}",
            agent_rows[0].skipped
        );
        assert!(
            ledger
                .reports
                .iter()
                .any(|r| r.capability == "fleet_commands"
                    && r.written == 0
                    && !r.skipped.is_empty()
                    && r.skipped
                        .iter()
                        .all(|u| u.reason == crate::capability_manifest::SkipReason::RepoAuthored)),
            "the commands pass is recorded as repo-authored: {:?}",
            ledger.reports
        );
    }

    #[tokio::test]
    async fn read_lossy_lines_survives_non_utf8_and_keeps_draining() {
        let data: &[u8] = b"ok\n\xff\xfe bad\r\nafter\n";
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        read_lossy_lines(data, "p:", tx).await;
        let mut got = Vec::new();
        while let Some(l) = rx.recv().await {
            got.push(l);
        }
        assert_eq!(got, vec!["p:ok", "p:\u{fffd}\u{fffd} bad", "p:after"]);
    }

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
        let args = claude_args("sid-1", None, &[], 200, &Default::default()).unwrap();
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
        let args =
            claude_args("sid", Some("claude-opus-5"), &tools, 5, &Default::default()).unwrap();
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

    /// Review M1: under the allow-list posture the task's tools are
    /// INTERSECTED with the operator's list — a task can narrow, never widen.
    #[test]
    fn allow_list_posture_intersects_task_and_operator_tools() {
        use crate::claude_session::launch_spec::SessionPermissionSetting;
        let op = SessionPermissionSetting::AllowList {
            tools: vec!["Read".to_string(), "Grep".to_string()],
        };
        let task = vec!["Bash(git status)".to_string(), "Read".to_string()];
        let args = claude_args("sid", None, &task, 3, &op).expect("non-empty intersection");
        assert_eq!(
            args,
            vec![
                "-p",
                "--permission-mode",
                "dontAsk",
                "--session-id",
                "sid",
                "--max-turns",
                "3",
                "--allowedTools",
                "Read"
            ]
        );
        assert!(!args
            .iter()
            .any(|a| a.contains("dangerously") || a.contains("Bash")));

        // No task tools: the operator's list applies.
        let args = claude_args("sid", None, &[], 3, &op).unwrap();
        assert_eq!(
            args[args.len() - 3..].to_vec(),
            vec!["--allowedTools", "Read", "Grep"]
        );
    }

    /// Review M1: a task whose tools share nothing with the operator's list is
    /// refused rather than run with the task's (wider) list or with none.
    #[test]
    fn allow_list_posture_refuses_an_empty_intersection() {
        use crate::claude_session::launch_spec::SessionPermissionSetting;
        let op = SessionPermissionSetting::AllowList {
            tools: vec!["Read".to_string()],
        };
        let err = claude_args("sid", None, &["Bash".to_string()], 3, &op).unwrap_err();
        assert!(err.contains("allow-list"), "{err}");
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
