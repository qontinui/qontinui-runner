//! Scheduled `Script` task: run a shell command in-binary, no Claude session.
//!
//! Plan `2026-09-28-ccfg-bundle-parity-bound-outruns-runner-train-latency`,
//! Phase 4.
//!
//! A deterministic script needs no model to report its own exit. The
//! `RemoteAgent` path records success from a headless `claude` child's exit
//! code, which is 0 however the work INSIDE the session ended; a `Script` task
//! records success from the COMMAND's exit code: `0` is success, any other
//! code is a failure carrying that code, a signal-terminated run is a failure,
//! and a run that exceeds `timeout_seconds` is killed (whole process tree) and
//! is a failure with `timed_out`.
//!
//! It reuses [`RemoteAgentOutcome`] / [`RemoteAgentLaunch`] so the scheduler's
//! one completion-recording path (`finalize_async_execution`) settles it, and
//! the same `~/.qontinui/scheduler-runs/<execution>.log` transcript.

use std::path::PathBuf;
use std::time::Duration;

use tracing::{info, warn};

use crate::scheduler_remote_agent::{
    pump_to_log, resolve_workdir, scheduler_logs_root, RemoteAgentLaunch, RemoteAgentOutcome,
};

/// Default wall-clock bound when the task sets no `timeout_seconds`.
pub(crate) const DEFAULT_TIMEOUT_SECS: u64 = 600;

/// Everything a scheduled `Script` task carries that the spawn needs.
#[derive(Debug, Clone)]
pub(crate) struct ScriptSpec {
    pub task_name: String,
    /// The scheduler history row this run writes — names the log file.
    pub execution_id: String,
    pub command: String,
    pub working_directory: Option<String>,
    pub timeout: Duration,
}

/// Classify a command's end into the scheduler's outcome. Timeout wins over
/// whatever status was read; only exit code 0 is success.
pub(crate) fn classify_script_exit(
    status: Result<Option<i32>, String>,
    timed_out: bool,
    timeout: Duration,
) -> RemoteAgentOutcome {
    let fail = |error: String, exit_code: Option<i64>, timed_out: bool| RemoteAgentOutcome {
        success: false,
        error: Some(error),
        exit_code,
        timed_out,
    };
    if timed_out {
        return fail(
            format!(
                "script exceeded timeout_seconds={} and was killed",
                timeout.as_secs()
            ),
            None,
            true,
        );
    }
    match status {
        Ok(Some(0)) => RemoteAgentOutcome {
            success: true,
            error: None,
            exit_code: Some(0),
            timed_out: false,
        },
        Ok(Some(code)) => fail(
            format!("script exited with code {code}"),
            Some(i64::from(code)),
            false,
        ),
        Ok(None) => fail("script was terminated by a signal".to_string(), None, false),
        Err(e) => fail(format!("waiting on the script failed: {e}"), None, false),
    }
}

/// True for the Windows WSL launcher (`System32\bash.exe`, `SysWOW64`, the
/// `WindowsApps` alias): a bare `bash` on PATH can resolve to it, and it runs
/// the command inside a Linux distro (or fails when none is installed) instead
/// of Git Bash. Pure, so it is testable off Windows.
pub(crate) fn is_wsl_launcher(path: &str) -> bool {
    let p = path.replace('/', "\\").to_ascii_lowercase();
    p.contains("\\windows\\system32\\")
        || p.contains("\\windows\\syswow64\\")
        || p.contains("\\windowsapps\\")
}

/// Explicit Git Bash locations first, then any PATH `bash.exe` that is not the
/// WSL launcher. Pure over its inputs.
pub(crate) fn windows_bash_paths(
    program_files: &[String],
    local_app_data: Option<&str>,
    path_dirs: &[String],
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for pf in program_files {
        out.push(format!("{pf}\\Git\\bin\\bash.exe"));
    }
    if let Some(l) = local_app_data {
        out.push(format!("{l}\\Programs\\Git\\bin\\bash.exe"));
    }
    for d in path_dirs {
        out.push(format!("{}\\bash.exe", d.trim_end_matches(['\\', '/'])));
    }
    out.retain(|c| !is_wsl_launcher(c));
    out
}

/// The shells to try, in order, for `command`. Unix: `sh -c`. Windows: an
/// explicit Git Bash (never the WSL launcher), then `cmd /C` as the fallback.
fn shell_candidates(command: &str) -> Vec<(String, Vec<String>)> {
    #[cfg(windows)]
    {
        let env_var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let pfs: Vec<String> = ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"]
            .iter()
            .filter_map(|k| env_var(k))
            .collect();
        let path_dirs: Vec<String> = std::env::var("PATH")
            .map(|p| {
                p.split(';')
                    .filter(|d| !d.is_empty())
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();
        let local = env_var("LOCALAPPDATA");
        let mut v: Vec<(String, Vec<String>)> =
            windows_bash_paths(&pfs, local.as_deref(), &path_dirs)
                .into_iter()
                .filter(|p| std::path::Path::new(p).is_file())
                .map(|p| (p, vec!["-c".to_string(), command.to_string()]))
                .collect();
        v.push((
            "cmd".to_string(),
            vec!["/C".to_string(), command.to_string()],
        ));
        v
    }
    #[cfg(not(windows))]
    {
        vec![(
            "sh".to_string(),
            vec!["-c".to_string(), command.to_string()],
        )]
    }
}

fn spawn_shell(command: &str, workdir: &std::path::Path) -> Result<tokio::process::Child, String> {
    let mut last_err = String::from("no shell candidates");
    for (program, args) in shell_candidates(command) {
        let mut cmd = crate::process_helpers::tokio_no_window(&program);
        cmd.args(&args)
            .current_dir(workdir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(false);
        crate::process_helpers::ChildTreeGuard::arm_tokio(&mut cmd);
        match cmd.spawn() {
            Ok(child) => return Ok(child),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                last_err = format!("{program}: {e}");
                continue;
            }
            Err(e) => return Err(format!("spawn script via {program}: {e}")),
        }
    }
    Err(format!("no usable shell found ({last_err})"))
}

/// The wall-clock bound for a task: unset OR zero is the default, never an
/// immediate kill.
pub(crate) fn effective_timeout(timeout_seconds: Option<u64>) -> Duration {
    Duration::from_secs(
        timeout_seconds
            .filter(|t| *t > 0)
            .unwrap_or(DEFAULT_TIMEOUT_SECS),
    )
}

/// Launch the command. A missing working directory or an unspawnable shell is
/// a LAUNCH failure (`Err`); everything after the spawn is an outcome.
pub(crate) async fn launch(spec: ScriptSpec) -> Result<RemoteAgentLaunch, String> {
    if spec.command.trim().is_empty() {
        return Err("Script task has an empty command".to_string());
    }
    let workdir = resolve_workdir(spec.working_directory.as_deref())?;
    let workdir_s = workdir.to_string_lossy().to_string();

    let mut child = spawn_shell(&spec.command, &workdir)?;
    let tree = crate::process_helpers::ChildTreeGuard::attach_armed_tokio(&child);
    let pid = child.id();
    info!(
        "scheduler: Script task '{}' spawned pid={:?} cwd={} timeout={}s",
        spec.task_name,
        pid,
        workdir_s,
        spec.timeout.as_secs()
    );

    let log_path: Option<PathBuf> =
        scheduler_logs_root().map(|d| d.join(format!("{}.log", spec.execution_id)));
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
                tree.disarm();
                (Ok(st.code()), false)
            }
            Ok(Err(e)) => {
                drop(tree);
                (Err(e.to_string()), false)
            }
            Err(_elapsed) => {
                warn!(
                    "scheduler: Script task '{task_name}' hit its {}s timeout — killing pid={pid:?} and its process tree",
                    timeout.as_secs()
                );
                drop(tree);
                let _ = child.kill().await;
                (Ok(None), true)
            }
        };
        let _ = tokio::time::timeout(Duration::from_secs(5), pump).await;
        classify_script_exit(status, timed_out, timeout)
    });

    Ok(RemoteAgentLaunch {
        // A script has no Claude session; the scheduler skips an empty id.
        session_id: String::new(),
        workdir: workdir_s,
        log_path,
        completion,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(command: &str, timeout: Duration) -> ScriptSpec {
        ScriptSpec {
            task_name: "t".into(),
            execution_id: format!("script-test-{}", uuid::Uuid::new_v4()),
            command: command.into(),
            working_directory: Some(std::env::temp_dir().to_string_lossy().to_string()),
            timeout,
        }
    }

    async fn run(command: &str, timeout: Duration) -> RemoteAgentOutcome {
        launch(spec(command, timeout))
            .await
            .expect("launch")
            .completion
            .await
    }

    #[tokio::test]
    async fn exit_zero_is_success() {
        let out = run("exit 0", Duration::from_secs(30)).await;
        assert!(out.success, "{out:?}");
        assert_eq!(out.exit_code, Some(0));
        assert!(!out.timed_out);
    }

    #[tokio::test]
    async fn nonzero_exit_is_failure_carrying_the_code() {
        let out = run("exit 3", Duration::from_secs(30)).await;
        assert!(!out.success, "{out:?}");
        assert_eq!(out.exit_code, Some(3));
        assert!(out.error.as_deref().unwrap().contains("code 3"));
        assert!(!out.timed_out);
    }

    #[tokio::test]
    async fn timeout_is_failure_and_kills_the_command() {
        let started = std::time::Instant::now();
        let out = run("sleep 30", Duration::from_secs(1)).await;
        assert!(!out.success, "{out:?}");
        assert!(out.timed_out);
        assert!(started.elapsed() < Duration::from_secs(20));
    }

    #[tokio::test]
    async fn empty_command_and_missing_directory_are_launch_failures() {
        assert!(launch(spec("  ", Duration::from_secs(1))).await.is_err());
        let mut s = spec("exit 0", Duration::from_secs(1));
        s.working_directory = Some(
            std::env::temp_dir()
                .join(format!("qontinui-no-such-{}", uuid::Uuid::new_v4()))
                .to_string_lossy()
                .to_string(),
        );
        let err = launch(s).await.err().expect("must fail");
        assert!(err.contains("is not a directory"));
    }

    #[test]
    fn script_task_type_round_trips_through_serde() {
        use qontinui_types::scheduler::ScheduledTaskType;
        let t = ScheduledTaskType::Script {
            command: "bash x.sh --a".into(),
            working_directory: Some("/w".into()),
            timeout_seconds: Some(900),
        };
        let json = serde_json::to_string(&t).unwrap();
        assert!(json.contains(r#""task_type":"Script""#));
        let back: ScheduledTaskType = serde_json::from_str(&json).unwrap();
        assert_eq!(json, serde_json::to_string(&back).unwrap());
    }

    #[test]
    fn zero_or_unset_timeout_is_the_default_not_an_instant_kill() {
        assert_eq!(
            effective_timeout(None),
            Duration::from_secs(DEFAULT_TIMEOUT_SECS)
        );
        assert_eq!(
            effective_timeout(Some(0)),
            Duration::from_secs(DEFAULT_TIMEOUT_SECS)
        );
        assert_eq!(effective_timeout(Some(7)), Duration::from_secs(7));
    }

    #[test]
    fn the_wsl_launcher_is_never_a_bash_candidate() {
        assert!(is_wsl_launcher(r"C:\Windows\System32\bash.exe"));
        assert!(is_wsl_launcher("C:/Windows/system32/bash.exe"));
        assert!(is_wsl_launcher(
            r"C:\Users\u\AppData\Local\Microsoft\WindowsApps\bash.exe"
        ));
        let got = windows_bash_paths(
            &[r"C:\Program Files".to_string()],
            Some(r"C:\Users\u\AppData\Local"),
            &[
                r"C:\Windows\System32".to_string(),
                r"D:\tools\bin\".to_string(),
            ],
        );
        assert_eq!(
            got,
            vec![
                r"C:\Program Files\Git\bin\bash.exe".to_string(),
                r"C:\Users\u\AppData\Local\Programs\Git\bin\bash.exe".to_string(),
                r"D:\tools\bin\bash.exe".to_string(),
            ]
        );
    }

    #[test]
    fn classification_table() {
        let t = Duration::from_secs(60);
        assert!(classify_script_exit(Ok(Some(0)), false, t).success);
        assert!(!classify_script_exit(Ok(Some(2)), false, t).success);
        assert!(!classify_script_exit(Ok(None), false, t).success);
        assert!(!classify_script_exit(Err("x".into()), false, t).success);
        let to = classify_script_exit(Ok(Some(0)), true, t);
        assert!(!to.success && to.timed_out);
    }
}
