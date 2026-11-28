//! Scheduled `Command` task: exec a program as an argv in-binary, no shell, no
//! Claude session.
//!
//! Plan `2026-09-13-move-fleet-scheduling-into-qontinui`, Phase 2 (D2, D10).
//!
//! A `Command` is the deterministic fleet job's executor: `program` is exec'd
//! with `args` exactly as given (no `sh -c`, `cmd /C` or PowerShell between
//! them), so there is no quoting drift across platforms. It runs on the
//! scheduler's ASYNC arm — a long run never holds up the tick — and is settled
//! by the same completion path as `Script` and `RemoteAgent`
//! (`SchedulerService::settle_launched_child`).
//!
//! It shares `Script`'s child core ([`crate::scheduler_script::spawn_tree`],
//! [`crate::scheduler_script::wait_tree`], [`crate::scheduler_script::ChildEnd`]):
//! spawned as the root of a killable tree, a wall-clock bound that kills the
//! WHOLE tree on expiry. It differs from `Script` in four ways:
//!
//! - **argv, not a shell string**, and a 3600 s default timeout
//!   ([`COMMAND_DEFAULT_TIMEOUT_SECONDS`]) rather than 600 s.
//! - **Output policy.** `Script` pumps both streams to a durable
//!   `~/.qontinui/scheduler-runs/<execution>.log`. A `Command` never does:
//!   under [`CommandOutput::Discard`] both streams go to the null device — no
//!   pipe exists, so not one byte of the child's output can reach a file, the
//!   database or `tracing` — and under [`CommandOutput::Tail`] only the LAST
//!   `min(bytes, 4096)` bytes of stderr are kept, in memory, and land in the
//!   execution record's `error_message` of a failed run. Stdout is always
//!   discarded.
//! - **Policy.** The rendered command line ([`render_command_line`]) goes
//!   through the same `PolicyEngine::evaluate_command` as the shell-command
//!   door, at create/update (the loud `4xx`, `mcp/scheduler.rs`) and again
//!   immediately before every spawn (the enforcing check, [`launch`]): a
//!   policy tightened after a task was registered still stops it.
//! - **`detach`** (D10): the child is launched outside the runner's
//!   process-lifetime grouping and recorded as launched, never awaited; its
//!   `output` and `timeout_seconds` are ignored (nothing is read, nothing is
//!   killed). It survives the runner's process group (Unix) or an enclosing
//!   job (Windows) — NOT a Linux systemd service stop under the default
//!   `KillMode=control-group`, which kills every process in the unit's cgroup,
//!   `setsid` or not. See [`launch_detached`].

use std::sync::{Arc, Mutex};
use std::time::Duration;

use qontinui_types::scheduler::{
    CommandOutput, ScheduledTaskType, COMMAND_DEFAULT_TIMEOUT_SECONDS,
};
use tokio::io::AsyncReadExt;
use tracing::info;

use crate::scheduler_remote_agent::{resolve_workdir, RemoteAgentLaunch, RemoteAgentOutcome};
use crate::scheduler_script::{spawn_tree, wait_tree, ChildEnd};
use crate::security::{PolicyEngine, SecurityPolicy};

/// How long a settled run waits for the stderr tail reader after the child
/// has exited. A grandchild the child left behind may still hold the pipe's
/// write end, so EOF is not guaranteed; the tail read so far is kept.
const TAIL_DRAIN_GRACE: Duration = Duration::from_secs(5);

/// Everything a scheduled `Command` task carries that the spawn needs.
#[derive(Debug, Clone)]
pub(crate) struct CommandSpec {
    pub task_name: String,
    pub program: String,
    pub args: Vec<String>,
    pub working_directory: Option<String>,
    pub timeout: Duration,
    pub output: CommandOutput,
    pub detach: bool,
}

impl CommandSpec {
    /// The spec a stored `Command` task runs with; `None` for any other type.
    pub(crate) fn from_task(task_name: &str, task: &ScheduledTaskType) -> Option<Self> {
        match task {
            ScheduledTaskType::Command {
                program,
                args,
                working_directory,
                timeout_seconds,
                output,
                detach,
            } => Some(Self {
                task_name: task_name.to_string(),
                program: program.clone(),
                args: args.clone(),
                working_directory: working_directory.clone(),
                timeout: effective_timeout(*timeout_seconds),
                output: *output,
                detach: *detach,
            }),
            _ => None,
        }
    }
}

/// The wall-clock bound for a `Command`: unset OR zero is the 3600 s default,
/// never an immediate kill.
pub(crate) fn effective_timeout(timeout_seconds: Option<u64>) -> Duration {
    Duration::from_secs(
        timeout_seconds
            .filter(|t| *t > 0)
            .unwrap_or(COMMAND_DEFAULT_TIMEOUT_SECONDS),
    )
}

/// How many trailing stderr bytes `output` keeps: `None` under `Discard`,
/// otherwise `bytes` capped at [`CommandOutput::MAX_TAIL_BYTES`].
pub(crate) fn tail_cap(output: CommandOutput) -> Option<usize> {
    match output {
        CommandOutput::Discard => None,
        CommandOutput::Tail { bytes } => Some(bytes.min(CommandOutput::MAX_TAIL_BYTES) as usize),
    }
}

// ── Policy ──────────────────────────────────────────────────────────────────

/// POSIX-quote one argv entry for the rendered command line, only when it
/// needs it: a plain word stays bare, so `rm -rf /` renders as exactly that
/// and a blocked-pattern regex written against shell lines still matches.
fn quote_for_display(arg: &str) -> String {
    let plain = !arg.is_empty()
        && arg.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(
                    c,
                    '-' | '_' | '.' | '/' | ':' | '=' | '+' | ',' | '@' | '%' | '\\'
                )
        });
    if plain {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', r"'\''"))
    }
}

/// The command line the policy engine judges: `program` followed by each arg,
/// shell-quoted where needed. It is never executed — the child is exec'd from
/// the argv — it is only the text the shell-command door's policy reads.
pub(crate) fn render_command_line(program: &str, args: &[String]) -> String {
    std::iter::once(program)
        .chain(args.iter().map(String::as_str))
        .map(quote_for_display)
        .collect::<Vec<_>>()
        .join(" ")
}

/// The security policy the shell-command door enforces right now, and the
/// `security.default_profile` setting it came from (named in a refusal so the
/// reader knows which setting to change).
pub(crate) fn current_policy() -> (SecurityPolicy, String) {
    let settings = crate::settings::get_security_settings();
    let policy = PolicyEngine::resolve_for_runtime(None, &settings);
    (policy, settings.default_profile)
}

/// `PolicyEngine::evaluate_command` over the rendered command line; `Err`
/// carries the denial's reason.
pub(crate) fn evaluate_policy(
    policy: &SecurityPolicy,
    program: &str,
    args: &[String],
) -> Result<(), String> {
    PolicyEngine::evaluate_command(policy, &render_command_line(program, args))
        .map_err(|denial| denial.reason)
}

/// Why a `Command` task was refused at create or update.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CommandTaskRefusal {
    /// The task is malformed (`400`).
    Invalid(String),
    /// The security policy denies its command line (`403`).
    PolicyDenied(String),
}

/// The policy-free half of [`validate_command_task`]: a blank `program` or an
/// out-of-range `Tail`. `None` for a well-formed `Command` or any other type.
pub(crate) fn command_shape_error(task: &ScheduledTaskType) -> Option<CommandTaskRefusal> {
    let ScheduledTaskType::Command {
        program, output, ..
    } = task
    else {
        return None;
    };
    if program.trim().is_empty() {
        return Some(CommandTaskRefusal::Invalid(
            "a Command task needs a non-empty program".to_string(),
        ));
    }
    if let CommandOutput::Tail { bytes } = output {
        if *bytes == 0 || *bytes > CommandOutput::MAX_TAIL_BYTES {
            return Some(CommandTaskRefusal::Invalid(format!(
                "a Command task's output Tail bytes must be between 1 and {} (got {bytes}); \
                 use output Discard to keep nothing",
                CommandOutput::MAX_TAIL_BYTES
            )));
        }
    }
    None
}

/// Create/update validation of a `Command` task: a non-blank `program`, a
/// `Tail` of 1..=4096 bytes, and a command line the current policy allows.
/// Any other task type passes untouched.
///
/// A `detach: true` task is NOT refused for carrying `output` or
/// `timeout_seconds`: `Tail` is the schema default, so every detached task
/// carries one. Both are simply ignored under detach (see [`launch_detached`]).
pub(crate) fn validate_command_task(
    task: &ScheduledTaskType,
    policy: &SecurityPolicy,
    profile: &str,
) -> Result<(), CommandTaskRefusal> {
    if let Some(refusal) = command_shape_error(task) {
        return Err(refusal);
    }
    let ScheduledTaskType::Command { program, args, .. } = task else {
        return Ok(());
    };
    evaluate_policy(policy, program, args).map_err(|reason| {
        CommandTaskRefusal::PolicyDenied(format!(
            "policy denied: {reason} (command line: {}; security policy profile \
             security.default_profile = \"{profile}\")",
            render_command_line(program, args)
        ))
    })
}

// ── Launch ──────────────────────────────────────────────────────────────────

/// Launch the command under `policy`. A policy denial, a blank program, a
/// missing working directory and an unspawnable program are LAUNCH failures
/// (`Err`) — nothing was spawned; everything after the spawn is an outcome.
///
/// The policy is evaluated HERE, immediately before the spawn, on every fire:
/// a denial spawns nothing and its error reads `policy denied: <reason>`.
pub(crate) async fn launch(
    spec: CommandSpec,
    policy: &SecurityPolicy,
) -> Result<RemoteAgentLaunch, String> {
    if spec.program.trim().is_empty() {
        return Err("Command task has an empty program".to_string());
    }
    evaluate_policy(policy, &spec.program, &spec.args)
        .map_err(|reason| format!("policy denied: {reason}"))?;
    let workdir = resolve_workdir(spec.working_directory.as_deref())?;
    if spec.detach {
        launch_detached(&spec, &workdir)
    } else {
        launch_awaited(spec, &workdir)
    }
}

/// The base `Command`: argv, no window, cwd, stdin closed.
fn base_command(spec: &CommandSpec, workdir: &std::path::Path) -> tokio::process::Command {
    let mut cmd = crate::process_helpers::tokio_no_window(&spec.program);
    cmd.args(&spec.args)
        .current_dir(workdir)
        .stdin(std::process::Stdio::null());
    cmd
}

fn launch_awaited(
    spec: CommandSpec,
    workdir: &std::path::Path,
) -> Result<RemoteAgentLaunch, String> {
    let cap = tail_cap(spec.output);
    let mut cmd = base_command(&spec, workdir);
    // Stdout is never read. Stderr is piped only when a tail is kept; under
    // Discard no pipe exists, so the child's output has no path into this
    // process at all.
    cmd.stdout(std::process::Stdio::null())
        .stderr(if cap.is_some() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        });
    let (mut child, tree) =
        spawn_tree(&mut cmd).map_err(|e| format!("spawn '{}': {e}", spec.program))?;
    let pid = child.id();
    // Names the program and the argv LENGTH only: an argument may carry
    // something the operator would not want in a log.
    info!(
        "scheduler: Command task '{}' spawned pid={:?} program={} argc={} cwd={} timeout={}s output={}",
        spec.task_name,
        pid,
        spec.program,
        spec.args.len(),
        workdir.display(),
        spec.timeout.as_secs(),
        if cap.is_some() { "tail" } else { "discard" }
    );

    let tail = Arc::new(Mutex::new(Vec::<u8>::new()));
    let reader = match (cap, child.stderr.take()) {
        (Some(cap), Some(stderr)) => Some(tokio::spawn(keep_tail(stderr, cap, tail.clone()))),
        _ => None,
    };
    let timeout = spec.timeout;
    let task_name = spec.task_name.clone();
    let completion = Box::pin(async move {
        let end = wait_tree(&mut child, tree, timeout, "Command", &task_name).await;
        if let Some(mut reader) = reader {
            if tokio::time::timeout(TAIL_DRAIN_GRACE, &mut reader)
                .await
                .is_err()
            {
                // A surviving descendant holds the pipe: stop reading (a
                // dropped handle would only detach the reader) and keep
                // what was read.
                reader.abort();
            }
        }
        let tail = cap.map(|cap| {
            let bytes = tail.lock().map(|b| b.clone()).unwrap_or_default();
            tail_text(&bytes, cap)
        });
        classify_command_end(end, timeout, tail.as_deref())
    });

    Ok(RemoteAgentLaunch {
        // A command has no Claude session; the scheduler skips an empty id.
        session_id: String::new(),
        workdir: workdir.to_string_lossy().to_string(),
        // Never a log file — the point of the output policy.
        log_path: None,
        completion,
    })
}

/// Read `stderr` to EOF, keeping only its last `cap` bytes in `tail`.
async fn keep_tail(mut stderr: tokio::process::ChildStderr, cap: usize, tail: Arc<Mutex<Vec<u8>>>) {
    let mut chunk = [0u8; 4096];
    loop {
        match stderr.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                if let Ok(mut buf) = tail.lock() {
                    push_tail(&mut buf, &chunk[..n], cap);
                }
            }
        }
    }
}

/// Append `chunk` to `buf`, then drop from the front so at most `cap` bytes
/// remain — always the most recent ones.
pub(crate) fn push_tail(buf: &mut Vec<u8>, chunk: &[u8], cap: usize) {
    buf.extend_from_slice(chunk);
    if buf.len() > cap {
        let excess = buf.len() - cap;
        buf.drain(..excess);
    }
}

/// The kept tail as text of at most `cap` bytes: a UTF-8 sequence the byte
/// cut split is dropped from the front, invalid bytes become U+FFFD, and if
/// that replacement grew the text past `cap`, the oldest characters go.
pub(crate) fn tail_text(bytes: &[u8], cap: usize) -> String {
    let start = bytes
        .iter()
        .position(|b| (b & 0b1100_0000) != 0b1000_0000)
        .unwrap_or(bytes.len());
    let text = String::from_utf8_lossy(&bytes[start..]).into_owned();
    if text.len() <= cap {
        return text;
    }
    let mut cut = text.len() - cap;
    while !text.is_char_boundary(cut) {
        cut += 1;
    }
    text.get(cut..).map(str::to_string).unwrap_or_default()
}

/// Word a `Command`'s end as the scheduler's outcome. Only exit code 0 is
/// success, and a success carries no message. A failure's message is
/// `exit <code>` (or the timeout / signal / wait failure), followed on the
/// next line by the stderr tail when one was kept and is non-empty.
pub(crate) fn classify_command_end(
    end: ChildEnd,
    timeout: Duration,
    tail: Option<&str>,
) -> RemoteAgentOutcome {
    let with_tail = |head: String| match tail.map(str::trim_end).filter(|t| !t.is_empty()) {
        Some(t) => format!("{head}\n{t}"),
        None => head,
    };
    let fail = |head: String, exit_code: Option<i64>, timed_out: bool| RemoteAgentOutcome {
        success: false,
        error: Some(with_tail(head)),
        exit_code,
        timed_out,
        // A command is no Claude session: it has no account to exhaust.
        rate_limited: None,
    };
    if end.timed_out {
        return fail(
            format!(
                "timed out after {}s (timeout_seconds); the process tree was killed",
                timeout.as_secs()
            ),
            None,
            true,
        );
    }
    match end.status {
        Ok(Some(0)) => RemoteAgentOutcome {
            success: true,
            error: None,
            exit_code: Some(0),
            timed_out: false,
            rate_limited: None,
        },
        Ok(Some(code)) => fail(format!("exit {code}"), Some(i64::from(code)), false),
        Ok(None) => fail("terminated by a signal".to_string(), None, false),
        Err(e) => fail(format!("waiting on the command failed: {e}"), None, false),
    }
}

// ── Detach (D10) ────────────────────────────────────────────────────────────

/// The execution record's message for a detached run: `success` means the
/// spawn succeeded, and this says the exit was never observed, so a detached
/// run is never mistaken for an awaited exit 0 in history.
pub(crate) const DETACHED_LAUNCH_MESSAGE: &str = "launched (detached; exit not observed)";

/// Spawn the child outside the runner's process-lifetime grouping and record
/// the run as LAUNCHED: the completion resolves at once with `success` and the
/// message [`DETACHED_LAUNCH_MESSAGE`] (so history never shows it as an
/// observed exit 0), and the task leaves `running_tasks` as soon as its row is
/// written. Nothing awaits the child and nothing reads its output (all three
/// streams are null), so the task's `output` and `timeout_seconds` are ignored.
///
/// - Unix: `setsid()` between fork and exec — a new session AND process
///   group, so neither a kill of the runner's group nor its controlling
///   terminal's hangup reaches it. Deliberately NOT `PR_SET_PDEATHSIG`, which
///   other runner spawns use to die with the runner. The child is still our
///   child until we exit; tokio reaps it when it exits first. **Limit:** a
///   runner run as a systemd unit with the default `KillMode=control-group`
///   takes the child down on a service stop or restart — the child stays in
///   the unit's cgroup whatever its session — so there it does NOT outlive
///   the runner's service.
/// - Windows: no `ChildTreeGuard` (whose kill-on-close job would reap it),
///   and no enrolment in the runner-wide job either (the runner enrols
///   children into that job one at a time and never sits in it itself, so a
///   child it does not enrol is already outside it). What remains is an OUTER
///   job the runner itself sits in — a supervisor's, a launcher's — which a
///   child joins by inheritance. So when this process is in a job the spawn
///   adds `CREATE_BREAKAWAY_FROM_JOB`, legal only when every enclosing job
///   sets `JOB_OBJECT_LIMIT_BREAKAWAY_OK` (the supervisor's does). A refused
///   breakaway fails the launch loudly: silently spawning a child the
///   enclosing job would reap is exactly what `detach` exists to prevent.
fn launch_detached(
    spec: &CommandSpec,
    workdir: &std::path::Path,
) -> Result<RemoteAgentLaunch, String> {
    let child = spawn_detached(base_command(spec, workdir), &spec.program)?;
    let pid = child.id();
    info!(
        "scheduler: Command task '{}' launched DETACHED pid={:?} program={} argc={} cwd={} (not awaited)",
        spec.task_name,
        pid,
        spec.program,
        spec.args.len(),
        workdir.display()
    );
    // Dropped without kill_on_drop: the process lives on, and on Unix tokio's
    // orphan queue reaps it if it exits while this runner is still up.
    drop(child);
    Ok(RemoteAgentLaunch {
        session_id: String::new(),
        workdir: workdir.to_string_lossy().to_string(),
        log_path: None,
        completion: Box::pin(async {
            RemoteAgentOutcome {
                success: true,
                error: Some(DETACHED_LAUNCH_MESSAGE.to_string()),
                exit_code: None,
                timed_out: false,
                rate_limited: None,
            }
        }),
    })
}

/// Spawn `cmd` detached (see [`launch_detached`]); its stdio is forced null.
pub(crate) fn spawn_detached(
    mut cmd: tokio::process::Command,
    program: &str,
) -> Result<tokio::process::Child, String> {
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(false);
    #[cfg(unix)]
    {
        // SAFETY: `setsid` is async-signal-safe and touches no memory of the
        // parent; it is the only thing run between fork and exec.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        cmd.spawn()
            .map_err(|e| format!("spawn '{program}' detached: {e}"))
    }
    #[cfg(windows)]
    {
        use qontinui_runner_win32::holder_spawn::{
            current_process_in_job, CREATE_BREAKAWAY_FROM_JOB, CREATE_NO_WINDOW,
            ERROR_ACCESS_DENIED,
        };
        let in_job = current_process_in_job().map_err(|e| {
            format!("detach: cannot tell whether this runner is in a job (IsProcessInJob: {e})")
        })?;
        // CREATE_NO_WINDOW keeps a console program console-less, as every
        // runner spawn is (`tokio_no_window`); creation_flags replaces, so it
        // is restated here.
        let flags = if in_job {
            CREATE_NO_WINDOW | CREATE_BREAKAWAY_FROM_JOB
        } else {
            CREATE_NO_WINDOW
        };
        cmd.creation_flags(flags);
        cmd.spawn().map_err(|e| {
            if in_job && e.raw_os_error() == Some(ERROR_ACCESS_DENIED) {
                format!(
                    "detach refused: this runner sits in a job that does not permit \
                     breakaway (CREATE_BREAKAWAY_FROM_JOB: {e}); a plain spawn would be \
                     killed with that job, so '{program}' was not started"
                )
            } else {
                format!("spawn '{program}' detached: {e}")
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::policy::ActionPolicy;

    fn permissive() -> SecurityPolicy {
        SecurityPolicy::permissive()
    }

    /// A policy whose blocklist denies any command line matching `pattern`.
    pub(crate) fn blocking(pattern: &str) -> SecurityPolicy {
        let mut p = SecurityPolicy::permissive();
        p.actions = ActionPolicy {
            blocked_commands: vec![pattern.to_string()],
            ..p.actions
        };
        p
    }

    fn spec(program: &str, args: &[&str]) -> CommandSpec {
        CommandSpec {
            task_name: "t".into(),
            program: program.into(),
            args: args.iter().map(|a| a.to_string()).collect(),
            working_directory: Some(std::env::temp_dir().to_string_lossy().to_string()),
            timeout: Duration::from_secs(30),
            output: CommandOutput::default(),
            detach: false,
        }
    }

    #[cfg(unix)]
    async fn run(s: CommandSpec) -> RemoteAgentOutcome {
        launch(s, &permissive())
            .await
            .expect("launch")
            .completion
            .await
    }

    #[test]
    fn zero_or_unset_timeout_is_the_hour_default_not_an_instant_kill() {
        assert_eq!(effective_timeout(None), Duration::from_secs(3600));
        assert_eq!(effective_timeout(Some(0)), Duration::from_secs(3600));
        assert_eq!(effective_timeout(Some(9)), Duration::from_secs(9));
    }

    #[test]
    fn the_rendered_line_quotes_only_what_needs_it() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(render_command_line("rm", &args(&["-rf", "/"])), "rm -rf /");
        assert_eq!(
            render_command_line("terraform", &args(&["plan", "-out=x y", "it's"])),
            r"terraform plan '-out=x y' 'it'\''s'"
        );
        assert_eq!(render_command_line("echo", &args(&[""])), "echo ''");
    }

    #[test]
    fn the_tail_keeps_the_last_bytes_and_never_more_than_the_cap() {
        let mut buf = Vec::new();
        for i in 0..100u32 {
            push_tail(&mut buf, format!("line {i:04}\n").as_bytes(), 4096);
        }
        assert!(buf.len() <= 4096);
        let mut big = Vec::new();
        push_tail(&mut big, &vec![b'a'; 10_000], 4096);
        push_tail(&mut big, b"END", 4096);
        assert_eq!(big.len(), 4096);
        assert!(big.ends_with(b"aEND"));
        // A split multi-byte char at the front is dropped, not mangled.
        assert_eq!(tail_text("é!".as_bytes()[1..].as_ref(), 10), "!");
        // Binary noise whose replacement chars outgrow the cap is re-cut.
        let t = tail_text(&[0xff; 4096], 4096);
        assert!(t.len() <= 4096, "{}", t.len());
        assert_eq!(tail_cap(CommandOutput::Tail { bytes: 100_000 }), Some(4096));
        assert_eq!(tail_cap(CommandOutput::Discard), None);
    }

    #[test]
    fn create_time_validation_refuses_blank_program_bad_tail_and_policy() {
        let task = |program: &str, output: CommandOutput| ScheduledTaskType::Command {
            program: program.into(),
            args: vec!["-rf".into(), "/".into()],
            working_directory: None,
            timeout_seconds: None,
            output,
            detach: false,
        };
        let p = permissive();
        assert!(
            validate_command_task(&task("rm", CommandOutput::default()), &p, "permissive").is_ok()
        );
        for blank in ["", "   "] {
            assert!(matches!(
                validate_command_task(&task(blank, CommandOutput::default()), &p, "permissive"),
                Err(CommandTaskRefusal::Invalid(_))
            ));
        }
        for bytes in [0, 4097] {
            assert!(matches!(
                validate_command_task(&task("rm", CommandOutput::Tail { bytes }), &p, "permissive"),
                Err(CommandTaskRefusal::Invalid(_))
            ));
        }
        assert!(
            validate_command_task(&task("rm", CommandOutput::Tail { bytes: 4096 }), &p, "x")
                .is_ok()
        );
        let Err(CommandTaskRefusal::PolicyDenied(msg)) = validate_command_task(
            &task("rm", CommandOutput::Discard),
            &blocking(r"^rm\b"),
            "custom",
        ) else {
            panic!("a blocked command line must be refused");
        };
        assert!(msg.starts_with("policy denied: "), "{msg}");
        assert!(
            msg.contains("security.default_profile = \"custom\""),
            "{msg}"
        );
        // Any other task type is not this validator's business.
        let script = ScheduledTaskType::Script {
            command: "rm -rf /".into(),
            working_directory: None,
            timeout_seconds: None,
        };
        assert!(validate_command_task(&script, &blocking("rm"), "custom").is_ok());
    }

    #[test]
    fn command_task_type_round_trips_through_serde() {
        let t = ScheduledTaskType::Command {
            program: "terraform".into(),
            args: vec!["plan".into(), "-detailed-exitcode".into()],
            working_directory: Some("/w".into()),
            timeout_seconds: Some(1800),
            output: CommandOutput::Discard,
            detach: false,
        };
        let json = serde_json::to_string(&t).unwrap();
        assert!(json.contains(r#""task_type":"Command""#), "{json}");
        let back: ScheduledTaskType = serde_json::from_str(&json).unwrap();
        assert_eq!(json, serde_json::to_string(&back).unwrap());
    }

    #[test]
    fn classification_table() {
        let t = Duration::from_secs(60);
        let end = |status, timed_out| ChildEnd { status, timed_out };
        let ok = classify_command_end(end(Ok(Some(0)), false), t, Some("noise"));
        assert!(ok.success && ok.error.is_none(), "{ok:?}");
        let bad = classify_command_end(end(Ok(Some(2)), false), t, Some("boom\n"));
        assert!(!bad.success);
        assert_eq!(bad.error.as_deref(), Some("exit 2\nboom"));
        assert_eq!(bad.exit_code, Some(2));
        let bare = classify_command_end(end(Ok(Some(2)), false), t, None);
        assert_eq!(bare.error.as_deref(), Some("exit 2"));
        let to = classify_command_end(end(Ok(Some(0)), true), t, None);
        assert!(!to.success && to.timed_out);
        assert!(!classify_command_end(end(Ok(None), false), t, None).success);
        assert!(!classify_command_end(end(Err("x".into()), false), t, None).success);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exit_zero_is_success_and_nonzero_carries_exit_code_and_tail() {
        let out = run(spec("true", &[])).await;
        assert!(out.success, "{out:?}");
        assert_eq!(out.exit_code, Some(0));

        let out = run(spec(
            "sh",
            &["-c", "echo first >&2; echo last-words >&2; exit 3"],
        ))
        .await;
        assert!(!out.success, "{out:?}");
        assert_eq!(out.exit_code, Some(3));
        let err = out.error.unwrap();
        assert!(err.starts_with("exit 3\n"), "{err}");
        assert!(err.ends_with("last-words"), "{err}");
    }

    /// The argv is exec'd as given: shell metacharacters in an arg are data.
    #[cfg(unix)]
    #[tokio::test]
    async fn args_are_an_argv_never_a_shell_string() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("pwned");
        let out = run(spec(
            "echo",
            &[&format!("; touch {}", marker.display()), "$(id)"],
        ))
        .await;
        assert!(out.success, "{out:?}");
        assert!(!marker.exists(), "an arg was interpreted by a shell");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn tail_caps_at_4096_and_keeps_the_last_bytes() {
        let mut s = spec(
            "sh",
            &[
                "-c",
                "head -c 20000 /dev/zero | tr '\\0' a >&2; printf TAIL-END >&2; exit 1",
            ],
        );
        s.output = CommandOutput::Tail { bytes: 9999 };
        let out = run(s).await;
        let err = out.error.unwrap();
        let tail = err.strip_prefix("exit 1\n").expect("exit header");
        assert_eq!(tail.len(), 4096, "the tail is capped at 4096 bytes");
        assert!(tail.ends_with("aTAIL-END"), "the LAST bytes are kept");

        let mut s = spec("sh", &["-c", "printf 0123456789 >&2; exit 1"]);
        s.output = CommandOutput::Tail { bytes: 4 };
        assert_eq!(run(s).await.error.as_deref(), Some("exit 1\n6789"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_blank_program_a_missing_dir_and_a_missing_program_are_launch_failures() {
        assert!(launch(spec("  ", &[]), &permissive()).await.is_err());
        let mut s = spec("true", &[]);
        s.working_directory = Some(
            std::env::temp_dir()
                .join(format!("qontinui-no-such-{}", uuid::Uuid::new_v4()))
                .to_string_lossy()
                .to_string(),
        );
        let err = launch(s, &permissive()).await.err().expect("must fail");
        assert!(err.contains("is not a directory"), "{err}");
        let err = launch(spec("qontinui-no-such-program-xyz", &[]), &permissive())
            .await
            .err()
            .expect("must fail");
        assert!(err.contains("qontinui-no-such-program-xyz"), "{err}");
    }

    /// A policy tightened after registration denies at FIRE time: nothing is
    /// spawned, and the launch error reads `policy denied: …`.
    #[cfg(unix)]
    #[tokio::test]
    async fn fire_time_policy_denial_spawns_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("ran");
        let s = spec("touch", &[&marker.display().to_string()]);
        let err = launch(s.clone(), &blocking(r"^touch\b"))
            .await
            .err()
            .expect("a denied command must not launch");
        assert!(err.starts_with("policy denied: "), "{err}");
        assert!(!marker.exists(), "the child was spawned despite the denial");
        // The same spec under the permissive default runs.
        assert!(run(s).await.success);
        assert!(marker.exists());
    }

    /// Timeout kills the whole tree: a GRANDCHILD the command started is gone
    /// too, not only the direct child.
    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_kills_a_grandchild() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("grandchild.pid");
        let script = format!("sleep 120 & echo $! > '{}'; wait", pidfile.display());
        let mut s = spec("sh", &["-c", &script]);
        s.timeout = Duration::from_secs(2);
        let started = std::time::Instant::now();
        let out = run(s).await;
        assert!(out.timed_out && !out.success, "{out:?}");
        assert!(started.elapsed() < Duration::from_secs(20));
        let gpid: i32 = std::fs::read_to_string(&pidfile)
            .expect("grandchild pid")
            .trim()
            .parse()
            .unwrap();
        assert!(
            wait_gone(gpid, Duration::from_secs(10)),
            "the grandchild {gpid} survived the timeout kill"
        );
    }

    /// Under Discard nothing the child writes reaches tracing (any level), the
    /// execution record, or a scheduler-runs log file.
    #[cfg(unix)]
    #[test]
    fn discard_leaks_nothing() {
        use tracing_subscriber::fmt::MakeWriter;

        #[derive(Clone, Default)]
        struct Sink(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Sink {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl<'a> MakeWriter<'a> for Sink {
            type Writer = Sink;
            fn make_writer(&'a self) -> Self::Writer {
                self.clone()
            }
        }

        let sink = Sink::default();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(sink.clone())
            .with_ansi(false)
            .finish();
        // `with_log_capture`, not a bare `set_default`: a parallel test that
        // reaches the launch callsite first under no subscriber would cache it
        // as `never` (measured here: the capture saw nothing). The helper's doc
        // carries the tracing-core mechanism. The runtime is built INSIDE the
        // capture so every poll happens on this thread, under it.
        let canary = format!("capture-canary-{}", uuid::Uuid::new_v4());
        let sentinel = format!("QONTINUI-SENTINEL-{}", uuid::Uuid::new_v4());
        let logs_before: Vec<_> = crate::scheduler_remote_agent::scheduler_logs_root()
            .and_then(|d| std::fs::read_dir(d).ok())
            .map(|rd| rd.filter_map(Result::ok).map(|e| e.path()).collect())
            .unwrap_or_default();
        crate::resource_guard::with_log_capture(subscriber, || {
            tracing::trace!("{canary}");
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(discard_runs(&sentinel));
        });
        let captured = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
        assert!(
            captured.contains(&canary),
            "the test subscriber captured nothing, so this test proves nothing \
             (LevelFilter::current() = {}): {captured}",
            tracing::level_filters::LevelFilter::current()
        );
        assert!(
            captured.contains("Command task 't' spawned"),
            "the launch line must reach the same capture the child's output would \
             (LevelFilter::current() = {}): {captured}",
            tracing::level_filters::LevelFilter::current()
        );
        assert!(
            !captured.contains(&sentinel),
            "child output reached tracing:\n{captured}"
        );
        let logs_after: Vec<_> = crate::scheduler_remote_agent::scheduler_logs_root()
            .and_then(|d| std::fs::read_dir(d).ok())
            .map(|rd| rd.filter_map(Result::ok).map(|e| e.path()).collect())
            .unwrap_or_default();
        for p in logs_after.iter().filter(|p| !logs_before.contains(p)) {
            let body = std::fs::read_to_string(p).unwrap_or_default();
            assert!(
                !body.contains(&sentinel),
                "a scheduler-runs log carries output: {}",
                p.display()
            );
        }
    }

    /// [`discard_leaks_nothing`]'s runs: one success and one failure under
    /// Discard, each child printing `sentinel` to stdout and stderr.
    #[cfg(unix)]
    async fn discard_runs(sentinel: &str) {
        for exit in [0, 7] {
            let mut s = spec(
                "sh",
                &[
                    "-c",
                    &format!("echo {sentinel}; echo {sentinel} >&2; exit {exit}"),
                ],
            );
            s.output = CommandOutput::Discard;
            let launched = launch(s, &permissive()).await.expect("launch");
            assert!(launched.log_path.is_none());
            let out = launched.completion.await;
            assert_eq!(out.exit_code, Some(exit));
            if let Some(e) = &out.error {
                assert!(!e.contains(sentinel), "the record carries output: {e}");
                assert_eq!(e, "exit 7");
            }
        }
    }

    // ── detach ──────────────────────────────────────────────────────────────

    /// `detach` through [`launch`]: the run settles as launched at once (not
    /// after the child's 120 s), the child is alive in its OWN session, and no
    /// output path or log exists.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_detached_launch_settles_at_once_in_its_own_session() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("pid");
        let mut s = spec(
            "sh",
            &[
                "-c",
                &format!(
                    "echo $$ > '{p}.tmp' && mv '{p}.tmp' '{p}'; exec sleep 120",
                    p = pidfile.display()
                ),
            ],
        );
        s.detach = true;
        s.output = CommandOutput::Tail { bytes: 4096 };
        let started = std::time::Instant::now();
        let launched = launch(s, &permissive()).await.expect("launch");
        assert!(launched.log_path.is_none());
        let out = launched.completion.await;
        assert!(out.success, "{out:?}");
        assert_eq!(out.exit_code, None, "a detach observes no exit");
        assert_eq!(
            out.error.as_deref(),
            Some(DETACHED_LAUNCH_MESSAGE),
            "a detached run must not read as an awaited exit 0"
        );
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "a detach is not awaited"
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !pidfile.exists() && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let pid: i32 = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let alive_now = alive(pid);
        // SAFETY: getsid/getpgid only read the target's ids.
        let (sid, pgid, our_pgid) =
            unsafe { (libc::getsid(pid), libc::getpgid(pid), libc::getpgid(0)) };
        kill_pid(pid);
        assert!(alive_now, "the detached child is running");
        assert_eq!(sid, pid, "setsid: the child leads its own session");
        assert_ne!(pgid, our_pgid, "the child left the runner's process group");
    }

    /// Whether `pid` is still a live process (a zombie counts as gone).
    #[cfg(unix)]
    pub(crate) fn alive(pid: i32) -> bool {
        // SAFETY: signal 0 only checks existence and permission.
        if unsafe { libc::kill(pid, 0) } != 0 {
            return false;
        }
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => stat
                .rsplit_once(") ")
                .and_then(|(_, rest)| rest.chars().next())
                .is_none_or(|state| state != 'Z'),
            Err(_) => true,
        }
    }

    #[cfg(windows)]
    pub(crate) fn alive(pid: i32) -> bool {
        use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        // SAFETY: plain Win32 calls; the handle is closed before returning.
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid as u32);
            if h.is_null() {
                return false;
            }
            let mut code = 0u32;
            let ok = GetExitCodeProcess(h, &mut code);
            CloseHandle(h);
            ok != 0 && code == STILL_ACTIVE as u32
        }
    }

    fn wait_gone(pid: i32, within: Duration) -> bool {
        let deadline = std::time::Instant::now() + within;
        while std::time::Instant::now() < deadline {
            if !alive(pid) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        !alive(pid)
    }

    fn kill_pid(pid: i32) {
        #[cfg(unix)]
        // SAFETY: best-effort cleanup of a process this test started.
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
        #[cfg(windows)]
        // SAFETY: plain Win32 calls; the handle is closed before returning.
        unsafe {
            use windows_sys::Win32::Foundation::CloseHandle;
            use windows_sys::Win32::System::Threading::{
                OpenProcess, TerminateProcess, PROCESS_TERMINATE,
            };
            let h = OpenProcess(PROCESS_TERMINATE, 0, pid as u32);
            if !h.is_null() {
                TerminateProcess(h, 1);
                CloseHandle(h);
            }
        }
    }

    /// A long-lived, quiet program to spawn in the detach probe.
    fn sleeper() -> (&'static str, Vec<String>) {
        #[cfg(unix)]
        {
            ("sleep", vec!["120".into()])
        }
        #[cfg(windows)]
        {
            ("ping", vec!["-n".into(), "120".into(), "127.0.0.1".into()])
        }
    }

    const PROBE_DIR_ENV: &str = "QONTINUI_COMMAND_DETACH_PROBE_DIR";

    /// The "runner" half of [`a_detached_child_outlives_the_runners_group_or_job_and_one_left_in_it_does_not`]:
    /// run only as a re-exec'd copy of this test binary. Waits for `go`, then
    /// launches one detached child through the real detach spawn and one
    /// contrast child, writes both pids, and waits until it is killed.
    #[tokio::test]
    #[ignore = "a child half: run only by a_detached_child_outlives_the_runners_group_or_job_and_one_left_in_it_does_not"]
    async fn detach_probe_child() {
        let Some(dir) = std::env::var_os(PROBE_DIR_ENV).map(std::path::PathBuf::from) else {
            return;
        };
        let go = dir.join("go");
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while !go.exists() {
            assert!(std::time::Instant::now() < deadline, "no go signal");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let (program, args) = sleeper();
        // Exactly the detach launch path's spawn (`launch_detached` is
        // `spawn_detached(base_command(..))`), which also hands back the pid.
        let s = spec(
            program,
            &args.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        let workdir = std::env::temp_dir();
        let detached_pid = match spawn_detached(base_command(&s, &workdir), program) {
            Ok(child) => child.id().expect("pid"),
            Err(e) => {
                std::fs::write(dir.join("error"), e).unwrap();
                return;
            }
        };

        // The contrast child.
        //
        // Unix: a plain child, which stays in this "runner's" process group.
        #[cfg(unix)]
        let plain = std::process::Command::new(program)
            .args(&args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("plain spawn");
        #[cfg(unix)]
        let plain_pid = plain.id();
        // Windows: a child ENROLLED the way the runner enrols its children —
        // assigned, after the spawn, to a kill-on-close job this process
        // owns (`ScopedKillOnCloseJob`, the shape of `assign_process_to_job`'s
        // runner-wide job). It is spawned with the same breakaway as the
        // detached one, so it leaves the OUTER job too; its death is then the
        // runner-shaped job's doing alone, when this process dies and the job's
        // last handle closes.
        #[cfg(windows)]
        let (plain_pid, _runner_job, _plain) = {
            let child =
                spawn_detached(base_command(&s, &workdir), program).expect("contrast spawn");
            let job = qontinui_runner_win32::job_object::ScopedKillOnCloseJob::create(None)
                .expect("a runner-shaped kill-on-close job");
            let handle = child.raw_handle().expect("a live process handle");
            // SAFETY: `child` is alive and owned here, so its handle is valid.
            unsafe { job.assign(handle as _) };
            (child.id().expect("pid"), job, child)
        };

        // Written then renamed, so the parent never reads half a line.
        std::fs::write(dir.join("pids.tmp"), format!("{detached_pid} {plain_pid}")).unwrap();
        std::fs::rename(dir.join("pids.tmp"), dir.join("pids")).unwrap();
        // Until the parent kills this probe; on Unix the plain child (a 120 s
        // sleeper) is reaped here if the probe somehow outlives it.
        #[cfg(unix)]
        {
            let mut plain = plain;
            let _ = tokio::time::timeout(
                Duration::from_secs(120),
                tokio::task::spawn_blocking(move || plain.wait()),
            )
            .await;
        }
        #[cfg(windows)]
        tokio::time::sleep(Duration::from_secs(120)).await;
    }

    /// D10: a detached child outlives the end of the runner's process-lifetime
    /// grouping, and a child left inside that grouping does not.
    ///
    /// - Windows, in the production topology: the probe "runner" sits in an
    ///   OUTER supervisor-shaped job (`KILL_ON_JOB_CLOSE | BREAKAWAY_OK`,
    ///   `OuterKillOnCloseJob`), which is closed. The detached child broke away
    ///   from it and survives. The contrast child is enrolled in a
    ///   runner-shaped `KILL_ON_JOB_CLOSE` job the probe owns, which closes
    ///   when the probe dies, and it dies.
    /// - Unix: the probe leads its own process group, which is killed with
    ///   SIGKILL (how a supervisor or a terminal takes a runner down). The
    ///   contrast child stayed in that group and dies; the detached one, in
    ///   its own session, survives. This is NOT a contrast with an awaited
    ///   `Command`: the awaited path already leads its own process group
    ///   (`spawn_tree`), so it also survives a kill of the runner's group,
    ///   exactly as `Script` does. What detach adds there is its own SESSION,
    ///   which [`a_detached_launch_settles_at_once_in_its_own_session`] checks.
    ///   Neither survives a systemd service stop under
    ///   `KillMode=control-group` (see [`launch_detached`]).
    #[tokio::test]
    async fn a_detached_child_outlives_the_runners_group_or_job_and_one_left_in_it_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let (_, in_crate) = module_path!().split_once("::").expect("crate path");
        let test_name = format!("{in_crate}::detach_probe_child");
        let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
        cmd.args([
            "--exact",
            &test_name,
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(PROBE_DIR_ENV, dir.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // The probe plays a runner that leads its own process group.
            cmd.process_group(0);
        }
        // Every exit from this test — a pass or any panic below — ends the
        // probe and both children it reported (`Cleanup`'s Drop).
        let mut cleanup = Cleanup {
            probe: Some(cmd.spawn().expect("re-exec the test binary")),
            pids: Vec::new(),
        };

        // The probe plays a runner a supervisor started: inside the
        // supervisor's job shape. It waits for `go`, so it has spawned
        // nothing before it is assigned.
        #[cfg(windows)]
        let outer = {
            let outer = qontinui_runner_win32::holder_spawn::OuterKillOnCloseJob::create(true)
                .expect("a supervisor-shaped outer job");
            outer
                .assign(cleanup.probe.as_ref().unwrap())
                .expect("assign the probe to the outer job");
            outer
        };
        std::fs::write(dir.path().join("go"), "").unwrap();

        let pids_path = dir.path().join("pids");
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while !pids_path.exists() {
            if let Ok(e) = std::fs::read_to_string(dir.path().join("error")) {
                #[cfg(windows)]
                if e.starts_with("detach refused")
                    && qontinui_runner_win32::holder_spawn::current_process_in_job()
                        .unwrap_or(false)
                {
                    // This test process itself sits in a job that forbids
                    // breakaway (some CI hosts), so the probe inherits it and
                    // nothing it starts can break away; the launch refused
                    // loudly, as designed. That proves nothing about detach,
                    // so it FAILS unless the host is explicitly declared one.
                    assert!(
                        std::env::var(ALLOW_NO_BREAKAWAY_ENV).as_deref() == Ok("1"),
                        "this test host's own job forbids breakaway, so detach cannot be \
                         tested here ({e}). Set {ALLOW_NO_BREAKAWAY_ENV}=1 to accept that \
                         on this host."
                    );
                    eprintln!(
                        "skipped under {ALLOW_NO_BREAKAWAY_ENV}=1: the host's job forbids \
                         breakaway: {e}"
                    );
                    return;
                }
                panic!("the detached launch failed in the probe: {e}");
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the probe never reported"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        let pids = std::fs::read_to_string(&pids_path).unwrap();
        let (detached, plain) = pids.trim().split_once(' ').expect("two pids");
        let (detached, plain): (i32, i32) = (detached.parse().unwrap(), plain.parse().unwrap());
        cleanup.pids = vec![detached, plain];
        assert!(alive(detached) && alive(plain), "both children start alive");

        // The runner's grouping ends.
        #[cfg(unix)]
        // SAFETY: the probe leads its own group (process_group(0) above).
        unsafe {
            libc::killpg(cleanup.probe.as_ref().unwrap().id() as i32, libc::SIGKILL);
        }
        #[cfg(windows)]
        drop(outer);
        // The probe itself must have been killed by that, well inside its own
        // 120 s wait.
        let probe = cleanup.probe.as_mut().unwrap();
        let probe_deadline = std::time::Instant::now() + Duration::from_secs(15);
        let probe_ended = loop {
            match probe.try_wait() {
                Ok(Some(_)) => break true,
                Ok(None) if std::time::Instant::now() < probe_deadline => {
                    std::thread::sleep(Duration::from_millis(50))
                }
                _ => break false,
            }
        };
        assert!(
            probe_ended,
            "the probe outlived the end of its own grouping (group kill / outer job close)"
        );

        let plain_gone = wait_gone(plain, Duration::from_secs(15));
        let detached_alive = alive(detached);
        assert!(
            plain_gone,
            "the non-detached child survived its runner's end"
        );
        assert!(detached_alive, "the detached child died with its runner");
    }

    /// The opt-out for [`a_detached_child_outlives_the_runners_group_or_job_and_one_left_in_it_does_not`]
    /// on a Windows host whose OWN job forbids breakaway.
    #[cfg(windows)]
    const ALLOW_NO_BREAKAWAY_ENV: &str = "QONTINUI_TEST_ALLOW_NO_BREAKAWAY";

    /// Kills the probe and every child it reported when the test ends, pass or
    /// panic, so a failing run leaves no 120 s sleeper behind.
    struct Cleanup {
        probe: Option<std::process::Child>,
        pids: Vec<i32>,
    }

    impl Drop for Cleanup {
        fn drop(&mut self) {
            if let Some(mut probe) = self.probe.take() {
                let _ = probe.kill();
                let _ = probe.wait();
            }
            for pid in self.pids.drain(..) {
                kill_pid(pid);
            }
        }
    }
}
