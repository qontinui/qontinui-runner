#![allow(dead_code)]

//! pi CLI provider (`@earendil-works/pi-coding-agent`).
//!
//! Runs `pi -p --no-session <prompt>` in print mode and captures stdout as the
//! response. pi natively supports DeepSeek (plus every provider in its model
//! registry), and its print mode can run built-in tools (`read, bash, edit,
//! write` by default; `--tools read,grep,find,ls` restricts to read-only).
//!
//! Windows facts verified by the 2026-07-15 probes (plan
//! `2026-07-15-runner-pi-deepseek-subagent-analysis.md`, Probe results):
//! - `pi` resolves to an npm `pi.cmd` shim, so the primary invocation is
//!   `cmd /c pi …` (the `cmd_no_window` pattern shared with Gemini CLI).
//! - Piped stdin does NOT survive the `pi.cmd` shim — the prompt must go via
//!   argv. For oversized prompts the fallback is spawning
//!   `node …\pi-coding-agent\dist\cli.js` directly, where stdin DOES merge
//!   into the prompt context.
//! - An unknown `--provider` value is silently ignored (exit 0, falls back to
//!   pi's configured default) — provider selection must not be treated as
//!   validated by pi.
//! - Model/auth failures exit nonzero with the error on stderr.

use super::types::AiResponse;
use crate::config_facade::{ai_keychain, keychain_keys};
use crate::doctor::{DoctorHandle, ProcessRegistration, ProcessType};
use crate::settings::{self, CliExecutionMode};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tracing::{debug, error, info, warn};

/// Prompts longer than this go through the direct-node + stdin fallback on
/// Windows instead of argv: cmd.exe's command line is capped at 8191 chars,
/// and special chars (%, ^, &, ", <, >) in argv risk cmd-level mangling.
const MAX_ARGV_PROMPT_CHARS: usize = 6000;

/// Poll interval for the bounded wait loop.
const WAIT_POLL: Duration = Duration::from_millis(100);

/// Run a prompt via pi CLI (print mode, ephemeral session).
pub(super) fn run_pi_cli(
    prompt: &str,
    settings: &settings::PiCliSettings,
    model_override: Option<&str>,
    doctor_handle: Option<&DoctorHandle>,
) -> AiResponse {
    run_pi_cli_in_dir(prompt, settings, model_override, None, doctor_handle)
}

/// Run a prompt via pi CLI with an explicit working directory.
///
/// The subagent dispatch path (Phase 4) stages files into a temp dir and runs
/// pi with `cwd` pointed there so pi's built-in tools see exactly the staged
/// files; every other caller passes `None` (inherit the runner's cwd).
pub(crate) fn run_pi_cli_in_dir(
    prompt: &str,
    settings: &settings::PiCliSettings,
    model_override: Option<&str>,
    working_dir: Option<&Path>,
    doctor_handle: Option<&DoctorHandle>,
) -> AiResponse {
    let system = std::env::consts::OS;
    let effective_mode = match settings.execution_mode {
        CliExecutionMode::Auto => {
            if system == "windows" {
                CliExecutionMode::WindowsNative
            } else {
                CliExecutionMode::Native
            }
        }
        mode => mode,
    };

    let pi_program = settings.custom_path.as_deref().unwrap_or("pi");
    let flags = build_flag_args(settings, model_override);

    info!(
        "Running pi CLI (mode: {:?}, program: {}, provider: {:?}, model: {:?}, override: {}, prompt_chars: {})",
        effective_mode,
        pi_program,
        settings.provider,
        settings.model,
        model_override.is_some(),
        prompt.len()
    );

    // (argv, prompt-via-stdin payload)
    let invocation = match effective_mode {
        CliExecutionMode::WindowsNative | CliExecutionMode::Auto => {
            if prompt.len() > MAX_ARGV_PROMPT_CHARS {
                // Oversized prompt: the cmd.exe line limit (8191 chars) makes
                // argv unusable. Spawn node against pi's cli.js directly —
                // stdin merge into the prompt context is probe-verified there
                // (and NOT through the pi.cmd shim).
                match resolve_pi_cli_js(settings.custom_path.as_deref()) {
                    Some(cli_js) => {
                        let mut argv = vec!["node".to_string(), cli_js.display().to_string()];
                        argv.extend(flags.iter().cloned());
                        (argv, Some(prompt.to_string()))
                    }
                    None => {
                        return AiResponse::error(format!(
                            "pi prompt is {} chars — too long for the cmd.exe argv limit — and \
                             pi's cli.js could not be located for the direct-node stdin fallback. \
                             Install pi via npm (npm i -g @earendil-works/pi-coding-agent) or \
                             shorten the prompt.",
                            prompt.len()
                        ));
                    }
                }
            } else {
                let mut argv = vec![
                    "cmd-shim".to_string(),
                    "/c".to_string(),
                    pi_program.to_string(),
                ];
                argv.extend(flags.iter().cloned());
                argv.push(prompt.to_string());
                (argv, None)
            }
        }
        CliExecutionMode::Wsl => {
            let mut argv = vec!["wsl".to_string(), pi_program.to_string()];
            argv.extend(flags.iter().cloned());
            argv.push(prompt.to_string());
            (argv, None)
        }
        CliExecutionMode::Native => {
            let mut argv = vec![pi_program.to_string()];
            argv.extend(flags.iter().cloned());
            argv.push(prompt.to_string());
            (argv, None)
        }
    };

    let (argv, stdin_payload) = invocation;
    let timeout = if settings.timeout_seconds > 0 {
        Duration::from_secs(settings.timeout_seconds)
    } else {
        Duration::from_secs(600)
    };

    match execute_argv(
        &argv,
        stdin_payload.as_deref(),
        working_dir,
        timeout,
        deepseek_key_env(settings),
        doctor_handle,
    ) {
        ExecOutcome::Success { stdout, .. } => {
            debug!("pi CLI response length: {} chars", stdout.len());
            // pi doesn't expose token counts in print-mode stdout.
            AiResponse::success(stdout)
        }
        ExecOutcome::Failure {
            code,
            stdout,
            stderr,
        } => {
            error!("pi CLI failed (exit {:?}): {}", code, stderr);
            AiResponse::error_with_output(stdout, format!("pi CLI failed: {}", stderr))
        }
        ExecOutcome::Timeout => AiResponse::error(format!(
            "pi CLI timed out after {}s and was killed.",
            timeout.as_secs()
        )),
        ExecOutcome::SpawnError(e) => AiResponse::error(format!(
            "Failed to execute pi CLI: {}. Is pi installed (npm i -g \
             @earendil-works/pi-coding-agent) and in PATH?",
            e
        )),
    }
}

/// Build the flag portion of the pi argv (everything between the program name
/// and the positional prompt).
///
/// `--provider`/`--model` are always passed explicitly when configured — pi
/// silently ignores an unknown `--provider` (probe-verified), so relying on
/// the operator's `~/.pi/agent/settings.json` default would make provider
/// selection non-deterministic across machines.
fn build_flag_args(
    settings: &settings::PiCliSettings,
    model_override: Option<&str>,
) -> Vec<String> {
    let mut args = vec!["-p".to_string(), "--no-session".to_string()];
    if let Some(provider) = settings.provider.as_deref() {
        if !provider.is_empty() {
            args.push("--provider".to_string());
            args.push(provider.to_string());
        }
    }
    let model = model_override.or(settings.model.as_deref());
    if let Some(model) = model {
        if !model.is_empty() {
            args.push("--model".to_string());
            args.push(model.to_string());
        }
    }
    if let Some(tools) = settings.tools.as_deref() {
        if !tools.is_empty() {
            args.push("--tools".to_string());
            args.push(tools.to_string());
        }
    }
    args
}

/// Locate pi's `dist/cli.js` for the direct-node fallback.
///
/// If `custom_path` points at the shim (`…\pi.cmd`) or the cli.js itself, the
/// sibling npm layout is derived from it; otherwise the standard global-npm
/// location under `%APPDATA%` is probed.
fn resolve_pi_cli_js(custom_path: Option<&str>) -> Option<PathBuf> {
    if let Some(custom) = custom_path {
        let p = PathBuf::from(custom);
        if p.file_name().is_some_and(|f| f == "cli.js") && p.exists() {
            return Some(p);
        }
        // <npm-prefix>\pi.cmd → <npm-prefix>\node_modules\@earendil-works\…
        if let Some(prefix) = p.parent() {
            let candidate = prefix.join("node_modules/@earendil-works/pi-coding-agent/dist/cli.js");
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    let appdata = std::env::var_os("APPDATA")?;
    let candidate =
        PathBuf::from(appdata).join("npm/node_modules/@earendil-works/pi-coding-agent/dist/cli.js");
    candidate.exists().then_some(candidate)
}

/// If the configured provider is DeepSeek and `DEEPSEEK_API_KEY` is not
/// already in the process env, fall back to the shared `openai_compatible`
/// keychain slot so pi authenticates with the same key the OpenAI-compatible
/// API client uses.
fn deepseek_key_env(settings: &settings::PiCliSettings) -> Option<(String, String)> {
    if settings.provider.as_deref() != Some("deepseek") {
        return None;
    }
    if std::env::var("DEEPSEEK_API_KEY").is_ok_and(|v| !v.trim().is_empty()) {
        return None;
    }
    match ai_keychain().get(keychain_keys::OPENAI_COMPATIBLE_API_KEY) {
        Ok(Some(key)) if !key.trim().is_empty() => {
            debug!("pi CLI: injecting DEEPSEEK_API_KEY from the openai_compatible keychain slot");
            Some(("DEEPSEEK_API_KEY".to_string(), key))
        }
        Ok(_) => None,
        Err(e) => {
            warn!(
                "pi CLI: keychain read for DEEPSEEK_API_KEY fallback failed: {}",
                e
            );
            None
        }
    }
}

enum ExecOutcome {
    Success {
        stdout: String,
        stderr: String,
    },
    Failure {
        code: Option<i32>,
        stdout: String,
        stderr: String,
    },
    Timeout,
    SpawnError(String),
}

/// Spawn `argv` with a hard deadline, draining stdout/stderr concurrently.
///
/// `argv[0] == "cmd-shim"` selects `process_helpers::cmd_no_window()` (the
/// Windows `.cmd`-shim path); anything else spawns `argv[0]` directly via
/// `process_helpers::no_window`. Reader threads drain both pipes while the
/// main thread polls `try_wait` against the deadline — unlike a bare
/// poll-then-`wait_with_output`, a child producing more output than the OS
/// pipe buffer can never deadlock. On deadline the child is killed.
fn execute_argv(
    argv: &[String],
    stdin_payload: Option<&str>,
    working_dir: Option<&Path>,
    timeout: Duration,
    extra_env: Option<(String, String)>,
    doctor_handle: Option<&DoctorHandle>,
) -> ExecOutcome {
    let mut cmd = if argv[0] == "cmd-shim" {
        let mut c = crate::process_helpers::cmd_no_window();
        c.args(&argv[1..]);
        c
    } else {
        let mut c = crate::process_helpers::no_window(&argv[0]);
        c.args(&argv[1..]);
        c
    };
    if let Some(dir) = working_dir {
        cmd.current_dir(dir);
    }
    if let Some((k, v)) = extra_env {
        cmd.env(k, v);
    }
    cmd.env("QONTINUI_TRACE_ID", uuid::Uuid::new_v4().to_string());
    cmd.stdin(if stdin_payload.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return ExecOutcome::SpawnError(e.to_string()),
    };

    let pid = child.id();
    if let Some(handle) = doctor_handle {
        let reg = ProcessRegistration {
            pid,
            process_type: ProcessType::ResponseOneShot,
            label: "pi CLI response".to_string(),
            last_activity: None,
        };
        if let Err(e) = handle.register_blocking(reg) {
            warn!("Failed to register pi CLI process with Doctor: {}", e);
        }
    }

    if let Some(payload) = stdin_payload {
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            if let Err(e) = stdin.write_all(payload.as_bytes()) {
                warn!("pi CLI: writing prompt to stdin failed: {}", e);
                kill_child_tree(&mut child);
                unregister(doctor_handle, pid);
                return ExecOutcome::SpawnError(format!("writing prompt to stdin failed: {}", e));
            }
            // Drop closes stdin (EOF) so pi starts producing output.
        }
    }

    // Drain both pipes on reader threads so a chatty child can't fill the OS
    // pipe buffer and wedge against our poll loop.
    let stdout_reader = child.stdout.take().map(spawn_pipe_reader);
    let stderr_reader = child.stderr.take().map(spawn_pipe_reader);

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    warn!(
                        "pi CLI exceeded {}s timeout — killing pid {} (and its tree)",
                        timeout.as_secs(),
                        pid
                    );
                    kill_child_tree(&mut child);
                    break None;
                }
                std::thread::sleep(WAIT_POLL);
            }
            Err(e) => {
                warn!("pi CLI: try_wait failed: {}", e);
                kill_child_tree(&mut child);
                unregister(doctor_handle, pid);
                return ExecOutcome::SpawnError(format!("wait failed: {}", e));
            }
        }
    };

    unregister(doctor_handle, pid);

    let Some(status) = status else {
        // TIMED OUT — return NOW, without joining the reader threads.
        //
        // `Timeout` carries no output, so joining buys nothing here, and it
        // can cost everything: `kill_child_tree` is best-effort (taskkill can
        // fail or be slow under load, and then only `cmd.exe`/`node` dies).
        // A surviving grandchild still holds the pipe write-ends, so the
        // readers would never see EOF and the join would block for the
        // child's full natural lifetime — defeating the very timeout that
        // just fired. Bounding this path is the whole point of the timeout,
        // so it must not depend on the tree-kill having fully succeeded.
        //
        // The readers are detached: they exit on their own once the orphan
        // finally closes the pipes.
        return ExecOutcome::Timeout;
    };

    // Child is gone, so the pipes are closed (or closing) — these joins are
    // bounded and collect the output the caller actually gets.
    let stdout = join_pipe_reader(stdout_reader);
    let stderr = join_pipe_reader(stderr_reader);

    if status.success() {
        ExecOutcome::Success { stdout, stderr }
    } else {
        ExecOutcome::Failure {
            code: status.code(),
            stdout,
            stderr,
        }
    }
}

/// Kill the child and — on Windows — its whole process tree.
///
/// Killing only the direct child (`cmd.exe` for the shim path, `node` for
/// the fallback) orphans grandchildren (pi's actual node process), which
/// keep running AND keep the stdout/stderr pipe write-ends open, wedging
/// the reader threads until the orphan exits on its own. `taskkill /T`
/// takes the whole tree down (same pattern as `doctor/commands.rs`).
fn kill_child_tree(child: &mut std::process::Child) {
    #[cfg(target_os = "windows")]
    {
        let pid = child.id();
        match crate::process_helpers::no_window("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .output()
        {
            Ok(o) if o.status.success() => {}
            Ok(o) => {
                warn!(
                    "taskkill /T for pi pid {} failed ({}); falling back to plain kill",
                    pid,
                    String::from_utf8_lossy(&o.stderr).trim()
                );
                let _ = child.kill();
            }
            Err(e) => {
                warn!(
                    "taskkill spawn failed for pi pid {} ({}); falling back to plain kill",
                    pid, e
                );
                let _ = child.kill();
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = child.kill();
    }
    let _ = child.wait();
}

fn unregister(doctor_handle: Option<&DoctorHandle>, pid: u32) {
    if let Some(handle) = doctor_handle {
        if let Err(e) = handle.unregister_blocking(pid) {
            debug!("Failed to unregister pi CLI process with Doctor: {}", e);
        }
    }
}

fn spawn_pipe_reader<R: std::io::Read + Send + 'static>(
    mut pipe: R,
) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        String::from_utf8_lossy(&buf).to_string()
    })
}

fn join_pipe_reader(handle: Option<std::thread::JoinHandle<String>>) -> String {
    handle.and_then(|h| h.join().ok()).unwrap_or_default()
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn test_settings() -> settings::PiCliSettings {
        settings::PiCliSettings {
            execution_mode: CliExecutionMode::Auto,
            custom_path: None,
            provider: Some("deepseek".to_string()),
            model: None,
            tools: None,
            timeout_seconds: 600,
        }
    }

    #[test]
    fn flag_args_always_pin_print_and_no_session() {
        let args = build_flag_args(&test_settings(), None);
        assert_eq!(args, vec!["-p", "--no-session", "--provider", "deepseek"]);
    }

    #[test]
    fn model_override_wins_over_settings_model() {
        let mut settings = test_settings();
        settings.model = Some("deepseek-v4-flash".to_string());
        let args = build_flag_args(&settings, Some("deepseek-reasoner"));
        let model_pos = args.iter().position(|a| a == "--model").expect("--model");
        assert_eq!(args[model_pos + 1], "deepseek-reasoner");
    }

    #[test]
    fn tools_allowlist_is_forwarded() {
        let mut settings = test_settings();
        settings.tools = Some("read,grep,find,ls".to_string());
        let args = build_flag_args(&settings, None);
        let tools_pos = args.iter().position(|a| a == "--tools").expect("--tools");
        assert_eq!(args[tools_pos + 1], "read,grep,find,ls");
    }

    #[test]
    fn empty_provider_and_model_are_omitted() {
        let mut settings = test_settings();
        settings.provider = Some(String::new());
        settings.model = Some(String::new());
        let args = build_flag_args(&settings, None);
        assert_eq!(args, vec!["-p", "--no-session"]);
    }

    #[cfg(windows)]
    #[test]
    fn execute_argv_captures_stdout_and_exit() {
        let argv: Vec<String> = ["cmd-shim", "/c", "echo", "hello-pi"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        match execute_argv(&argv, None, None, Duration::from_secs(30), None, None) {
            ExecOutcome::Success { stdout, .. } => assert!(stdout.contains("hello-pi")),
            other => panic!("expected success, got {}", outcome_name(&other)),
        }
    }

    /// The timeout must be HARD: `execute_argv` returns promptly even when a
    /// live grandchild (`ping`, under `cmd.exe`) still holds the pipe
    /// write-ends.
    ///
    /// REGRESSION: this path originally joined the reader threads before
    /// returning `Timeout`. Those joins only finish once every pipe
    /// write-end closes — so whenever the best-effort `taskkill /T` missed
    /// the grandchild (it can fail or lag under parallel-suite load), the
    /// call blocked for the child's FULL natural lifetime instead of the
    /// deadline. It showed up as a load-dependent flake, but the real defect
    /// was that a wedged pi — exactly what the timeout exists for — could
    /// hang a caller for pi's entire runtime. The timeout path now returns
    /// without joining (it discards output anyway), so the bound holds
    /// whether or not the tree-kill won.
    #[cfg(windows)]
    #[test]
    fn execute_argv_timeout_is_bounded_even_if_tree_kill_misses() {
        // `ping -n 30 127.0.0.1` runs ~29s; the 1s deadline must pre-empt it.
        let argv: Vec<String> = ["cmd-shim", "/c", "ping", "-n", "30", "127.0.0.1"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let started = Instant::now();
        match execute_argv(&argv, None, None, Duration::from_secs(1), None, None) {
            ExecOutcome::Timeout => {}
            other => panic!("expected timeout, got {}", outcome_name(&other)),
        }
        // Generous vs the 1s deadline (CI is slow, and taskkill spawns a
        // process) but far below the grandchild's ~29s lifetime — so this
        // fails if the return ever waits on a surviving grandchild again.
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "timeout took {:?} — it must not wait on a surviving grandchild",
            started.elapsed()
        );
    }

    #[cfg(unix)]
    #[test]
    fn execute_argv_captures_stdout_and_exit() {
        let argv: Vec<String> = ["echo", "hello-pi"].iter().map(|s| s.to_string()).collect();
        match execute_argv(&argv, None, None, Duration::from_secs(30), None, None) {
            ExecOutcome::Success { stdout, .. } => assert!(stdout.contains("hello-pi")),
            other => panic!("expected success, got {}", outcome_name(&other)),
        }
    }

    #[cfg(unix)]
    #[test]
    fn execute_argv_timeout_is_bounded_even_if_tree_kill_misses() {
        let argv: Vec<String> = ["sleep", "30"].iter().map(|s| s.to_string()).collect();
        let started = Instant::now();
        match execute_argv(&argv, None, None, Duration::from_secs(1), None, None) {
            ExecOutcome::Timeout => {}
            other => panic!("expected timeout, got {}", outcome_name(&other)),
        }
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "timeout took {:?} — it must not wait on a surviving grandchild",
            started.elapsed()
        );
    }

    #[test]
    fn execute_argv_reports_spawn_error_for_missing_program() {
        let argv: Vec<String> = ["definitely-not-a-real-program-xyz"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        match execute_argv(&argv, None, None, Duration::from_secs(5), None, None) {
            ExecOutcome::SpawnError(_) => {}
            other => panic!("expected spawn error, got {}", outcome_name(&other)),
        }
    }

    /// Manual smoke for the Phase 3 gate (requires pi installed + a DeepSeek
    /// key). Run:
    /// `cargo test --bin qontinui-runner pi_cli_manual_smoke -- --ignored --nocapture`
    #[test]
    #[ignore = "requires pi CLI + DeepSeek key on the box; run manually"]
    fn pi_cli_manual_smoke() {
        let mut settings = test_settings();
        settings.timeout_seconds = 120;
        let resp = run_pi_cli(
            "What is 2+2? Answer with just the number.",
            &settings,
            None,
            None,
        );
        assert!(resp.success, "pi smoke failed: {:?}", resp.error);
        assert!(
            resp.output.contains('4'),
            "unexpected output: {}",
            resp.output
        );
    }

    fn outcome_name(o: &ExecOutcome) -> &'static str {
        match o {
            ExecOutcome::Success { .. } => "Success",
            ExecOutcome::Failure { .. } => "Failure",
            ExecOutcome::Timeout => "Timeout",
            ExecOutcome::SpawnError(_) => "SpawnError",
        }
    }
}
