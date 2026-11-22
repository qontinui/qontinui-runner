#![allow(dead_code)]

use super::config::get_effective_config_dir;
use super::process::spawn_and_wait_with_doctor;
use super::types::AiResponse;
use crate::doctor::DoctorHandle;
use crate::settings::{self, CliExecutionMode};
use crate::str_utils::truncate_str;
use tracing::{debug, error, info, warn};

/// Run a prompt via Claude CLI.
///
/// The process runs until completion — health monitoring is handled by the Doctor service.
pub(super) fn run_claude_cli(
    prompt: &str,
    settings: &settings::ClaudeCliSettings,
    model_override: Option<&str>,
    doctor_handle: Option<&DoctorHandle>,
) -> AiResponse {
    let system = std::env::consts::OS;

    // Determine effective execution mode
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

    let claude_program = settings.custom_path.as_deref().unwrap_or("claude");
    let (effective_dir, _config_dir_source) = get_effective_config_dir(settings);
    let config_dir = effective_dir.as_deref();

    // Silently refresh OAuth credentials if expired before spawning the subprocess.
    super::oauth_refresh::try_ensure_valid_credentials(config_dir);

    info!(
        "Running Claude CLI (mode: {:?}, program: {}, config_dir: {:?}, model_override: {:?}, prompt_len: {})",
        effective_mode, claude_program, config_dir, model_override, prompt.len()
    );

    // On Windows, always use stdin piping (file-based approach) because
    // cmd.exe /c interprets special characters (", %, ^, &, |, >, <) in
    // command-line arguments, which corrupts JSON content in prompts.
    // On other platforms, only use stdin for long prompts.
    let use_stdin =
        matches!(effective_mode, CliExecutionMode::WindowsNative) || prompt.len() > 8000;

    if use_stdin {
        // Use file-based approach for long prompts (avoids Windows cmd length limits)
        run_claude_cli_with_file(
            prompt,
            claude_program,
            effective_mode,
            config_dir,
            model_override,
            doctor_handle,
        )
    } else {
        // Use command-line argument for short prompts
        run_claude_cli_with_arg(
            prompt,
            claude_program,
            effective_mode,
            config_dir,
            model_override,
            doctor_handle,
        )
    }
}

/// Run Claude CLI with prompt in a temp file (for long prompts).
///
/// Writes the prompt to a temp file and uses PowerShell to read it and pipe to Claude.
/// This avoids Windows command line length limitations.
/// The process runs until completion — health monitoring is handled by the Doctor service.
fn run_claude_cli_with_file(
    prompt: &str,
    claude_program: &str,
    effective_mode: CliExecutionMode,
    config_dir: Option<&str>,
    model_override: Option<&str>,
    doctor_handle: Option<&DoctorHandle>,
) -> AiResponse {
    // Write prompt to a temp file
    let temp_dir = std::env::temp_dir();
    let prompt_file = temp_dir.join(format!("ai-prompt-{}.txt", uuid::Uuid::new_v4()));

    if let Err(e) = std::fs::write(&prompt_file, prompt) {
        return AiResponse::error(format!("Failed to write prompt to temp file: {}", e));
    }

    // Build a PowerShell command that reads the file and pipes to Claude
    let prompt_path = prompt_file.to_string_lossy();

    // Build model flag if override is provided
    let model_flag = model_override
        .map(|m| format!(" --model {}", m))
        .unwrap_or_default();

    let output_result = match effective_mode {
        CliExecutionMode::WindowsNative | CliExecutionMode::Auto => {
            // Use PowerShell to read file and pipe to Claude
            // This properly handles the stdin piping that cmd.exe struggles with
            // If config_dir is set, we need to set the env var in PowerShell
            let ps_command = if let Some(dir) = config_dir {
                let escaped_dir = dir.replace('\'', "''");
                format!(
                    "$env:CLAUDE_CONFIG_DIR = '{}'; Get-Content -Path '{}' -Raw -Encoding UTF8 | {} --print{}",
                    escaped_dir, prompt_path, claude_program, model_flag
                )
            } else {
                format!(
                    "Get-Content -Path '{}' -Raw -Encoding UTF8 | {} --print{}",
                    prompt_path, claude_program, model_flag
                )
            };
            spawn_and_wait_with_doctor(
                crate::process_helpers::no_window("powershell.exe").args([
                    "-NoProfile",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-Command",
                    &ps_command,
                ]),
                "Claude CLI response (file)",
                doctor_handle,
            )
        }
        CliExecutionMode::Wsl => {
            // For WSL, use cat to read file and pipe
            let wsl_path = prompt_file.to_string_lossy().replace("\\", "/");
            // Convert Windows path to WSL path
            let wsl_prompt = if let Some(dir) = config_dir {
                let escaped_dir = dir.replace('\'', "'\\''");
                format!(
                    "export CLAUDE_CONFIG_DIR='{}'; cat '{}' | {} --print{}",
                    escaped_dir,
                    wsl_path.replace("C:", "/mnt/c"),
                    claude_program,
                    model_flag
                )
            } else {
                format!(
                    "cat '{}' | {} --print{}",
                    wsl_path.replace("C:", "/mnt/c"),
                    claude_program,
                    model_flag
                )
            };
            spawn_and_wait_with_doctor(
                crate::process_helpers::no_window("wsl").args(["bash", "-c", &wsl_prompt]),
                "Claude CLI response (WSL file)",
                doctor_handle,
            )
        }
        CliExecutionMode::Native => {
            // On Unix, use cat to read and pipe
            let native_cmd = if let Some(dir) = config_dir {
                let escaped_dir = dir.replace('\'', "'\\''");
                format!(
                    "export CLAUDE_CONFIG_DIR='{}'; cat '{}' | {} --print{}",
                    escaped_dir, prompt_path, claude_program, model_flag
                )
            } else {
                format!(
                    "cat '{}' | {} --print{}",
                    prompt_path, claude_program, model_flag
                )
            };
            spawn_and_wait_with_doctor(
                crate::process_helpers::no_window("sh").args(["-c", &native_cmd]),
                "Claude CLI response (native file)",
                doctor_handle,
            )
        }
    };

    // Clean up temp file
    let _ = std::fs::remove_file(&prompt_file);

    match output_result {
        Ok(output) => process_cli_output(output),
        Err(e) => {
            let error_msg = format!(
                "Failed to execute Claude CLI: {}. Is Claude Code installed and in PATH?",
                e
            );
            error!("{}", error_msg);
            AiResponse::error(error_msg)
        }
    }
}

/// Run Claude CLI with prompt as command-line argument (for short prompts).
///
/// The process runs until completion — health monitoring is handled by the Doctor service.
fn run_claude_cli_with_arg(
    prompt: &str,
    claude_program: &str,
    effective_mode: CliExecutionMode,
    config_dir: Option<&str>,
    model_override: Option<&str>,
    doctor_handle: Option<&DoctorHandle>,
) -> AiResponse {
    let output_result = match effective_mode {
        CliExecutionMode::WindowsNative | CliExecutionMode::Auto => {
            let mut cmd = crate::process_helpers::cmd_no_window();
            let mut args = vec!["/c", claude_program, "--print", "-p", prompt];
            if let Some(model) = model_override {
                args.push("--model");
                args.push(model);
            }
            cmd.args(&args);
            if let Some(dir) = config_dir {
                cmd.env("CLAUDE_CONFIG_DIR", dir);
            }
            spawn_and_wait_with_doctor(&mut cmd, "Claude CLI response (arg)", doctor_handle)
        }
        CliExecutionMode::Wsl => {
            let mut cmd = crate::process_helpers::no_window("wsl");
            let mut args = vec![claude_program, "--print", "-p", prompt];
            if let Some(model) = model_override {
                args.push("--model");
                args.push(model);
            }
            cmd.args(&args);
            if let Some(dir) = config_dir {
                cmd.env("CLAUDE_CONFIG_DIR", dir);
            }
            spawn_and_wait_with_doctor(&mut cmd, "Claude CLI response (WSL arg)", doctor_handle)
        }
        CliExecutionMode::Native => {
            let mut cmd = crate::process_helpers::no_window(claude_program);
            let mut args = vec!["--print", "-p", prompt];
            if let Some(model) = model_override {
                args.push("--model");
                args.push(model);
            }
            cmd.args(&args);
            if let Some(dir) = config_dir {
                cmd.env("CLAUDE_CONFIG_DIR", dir);
            }
            spawn_and_wait_with_doctor(&mut cmd, "Claude CLI response (native arg)", doctor_handle)
        }
    };

    match output_result {
        Ok(output) => process_cli_output(output),
        Err(e) => {
            let error_msg = format!(
                "Failed to execute Claude CLI: {}. Is Claude Code installed and in PATH?",
                e
            );
            error!("{}", error_msg);
            AiResponse::error(error_msg)
        }
    }
}

// ── One-shot option scorer (terminal auto-response judge-rework Phase 2) ──────
//
// The terminal auto-response engine's `resolve_by_scoring` rules need each
// policy option scored on each dimension *before* coord composes a winner.
// Rather than coord running an LLM, the runner scores via its OWN authenticated
// Claude CLI — same binary resolution + config-dir/auth/env discipline as
// `run_claude_cli` above, but as a non-interactive `claude -p <prompt>` print
// call with a bounded timeout that returns the raw stdout (the scores JSON) and
// fails closed (`None`) on a nonzero exit / timeout / missing binary. The
// caller (`terminal::auto_response`) builds the prompt, parses the JSON, and is
// the one that fails-safe-injects-nothing on a `None`.

use crate::settings::ClaudeCliSettings;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

/// Path to the `claude` binary for the one-shot scorer. Honors
/// `QONTINUI_CLAUDE_BIN` (the same override the agent runtime + mock fixture
/// use) first, then the user's configured `custom_path`, then PATH `claude` —
/// so the scorer runs the SAME CLI the runner already uses.
fn scorer_claude_program(settings: &ClaudeCliSettings) -> String {
    std::env::var("QONTINUI_CLAUDE_BIN")
        .ok()
        .or_else(|| settings.custom_path.clone())
        .unwrap_or_else(|| "claude".to_string())
}

/// Build a one-shot `claude --print <extra_args…>` command, fully
/// env-prepared. Shared by the option scorer ([`build_scorer_command`], no
/// extra args) and the structured one-shot ([`run_structured_oneshot`]).
///
/// On Windows route through `cmd.exe /d /s /c` ([`cmd_s_c_line`], so a
/// `.cmd`/`.ps1` shim on PATH resolves and a quoted program path survives),
/// elsewhere invoke the program directly. On Windows a program or argument
/// holding a character cmd.exe interprets even inside quotes is REFUSED
/// ([`PrintRunError::Spawn`]) before anything is spawned. The prompt is piped over
/// STDIN by the caller (not passed as a `-p` argv) — prompts here are long,
/// multi-line and JSON-heavy, and cmd.exe corrupts special chars
/// (`"`, `%`, `^`, `&`, `|`, `<`, `>`) in arguments (the exact reason
/// `run_claude_cli` always pipes on Windows). `claude --print` reads the prompt
/// from stdin when no positional prompt is given.
///
/// **This seam does NOT route through
/// [`super::process::spawn_and_wait_with_doctor`]** — it spawns and polls its
/// own child ([`run_print_child`]) so it can enforce a timeout — so it needs
/// its own copy of the env preamble, credential scrub included. The scrub is
/// last; a caller may add only its working directory and, for the structured
/// one-shot, the git pins of [`run_structured_oneshot_with`] — none of them a
/// credential. See `crate::terminal::CREDENTIAL_VALUE_ENV_VARS`.
///
/// Returned by value (rather than the env work being inlined at the call site)
/// so the constructed environment is unit-testable without spawning `claude`.
pub(crate) fn build_print_command(
    program: &str,
    config_dir: Option<&str>,
    extra_args: &[String],
) -> Result<std::process::Command, PrintRunError> {
    #[cfg(windows)]
    let mut cmd = {
        use std::os::windows::process::CommandExt;
        let line = cmd_s_c_line(program, extra_args).map_err(PrintRunError::Spawn)?;
        let mut c = crate::process_helpers::cmd_no_window();
        c.raw_arg(line);
        // cmd.exe (and launchers that follow the same Windows rule) look a
        // bare program name up in the CURRENT DIRECTORY before PATH; the
        // structured one-shot's cwd is an untrusted checkout, so a planted
        // `claude.cmd` / `git.exe` there must not win. Setting this variable
        // turns that lookup off for cmd.exe and everything it starts
        // (`NeedCurrentDirectoryForExePathW`). The maintenance gate also
        // refuses such files at the checkout root.
        c.env("NoDefaultCurrentDirectoryInExePath", "1");
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = crate::process_helpers::no_window(program);
        c.arg("--print");
        c.args(extra_args);
        c
    };
    if let Some(dir) = config_dir {
        cmd.env("CLAUDE_CONFIG_DIR", dir);
    }
    cmd.env_remove("CLAUDECODE");
    // Same rule, sibling marker — see `session::transport::claude_cli` docs.
    cmd.env_remove(qontinui_runner_lib::claude_env::CLAUDE_CHILD_SESSION_ENV);
    cmd.env("QONTINUI_TRACE_ID", uuid::Uuid::new_v4().to_string());
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Full non-interactive git credential posture — this seam spawns a
    // `claude` that can run `git push`, so without it a credential prompt
    // is an infinite silent hang. One shared list, eight seams; see
    // `credential_helper::non_interactive_git_env`.
    crate::credential_helper::apply_non_interactive_git_env_std(&mut cmd);
    crate::terminal::scrub_credential_env_std(&mut cmd);

    Ok(cmd)
}

/// `arg` quoted for the MSVC CRT rules: left bare when it is non-empty and
/// holds no whitespace or cmd.exe operator; otherwise wrapped in quotes, with
/// its trailing backslashes doubled so the closing quote stays a quote.
/// [`cmd_s_c_line`] refuses any `"` before this runs, so no quote ever needs
/// escaping inside the argument.
#[cfg_attr(not(windows), allow(dead_code))] // Windows-only path; tested everywhere
fn msvc_quote(arg: &str) -> String {
    debug_assert!(!arg.contains('"'), "cmd_s_c_line refuses quotes first");
    let needs = arg.is_empty()
        || arg
            .chars()
            .any(|c| matches!(c, ' ' | '\t' | '&' | '|' | '<' | '>' | '^'));
    if !needs {
        return arg.to_string();
    }
    let trailing = arg.len() - arg.trim_end_matches('\\').len();
    format!("\"{arg}{}\"", "\\".repeat(trailing))
}

/// Characters [`cmd_s_c_line`] refuses in the program or any argument, because
/// cmd.exe acts on them even inside a quoted argument:
/// - `"` toggles cmd's quote state, and cmd ignores the MSVC `\"` escape —
///   so a quote inside an argument leaves the rest of the line UNQUOTED to
///   cmd, where `&` starts a second command (measured:
///   `"a\"b" … "a&echo INJECTED"` ran `echo INJECTED`);
/// - `%` expands `%VAR%` inside quotes, and `!` does under delayed expansion;
/// - a CR or LF ends the command line.
#[cfg_attr(not(windows), allow(dead_code))] // Windows-only path; tested everywhere
const CMD_LINE_REFUSED: [char; 5] = ['"', '%', '!', '\r', '\n'];

/// The `cmd.exe` arguments that run `<program> --print <args…>` on Windows:
/// `/d /s /c "<quoted program> --print <quoted args…>"`, or an error naming
/// the first program or argument that holds a [`CMD_LINE_REFUSED`] character
/// (cmd.exe would interpret it, so there is no safe way to pass it).
///
/// Plain `/c` strips the first and the last quote of the line whenever the line
/// starts with a quote — so a program path that needs quoting (spaces) together
/// with a quoted argument such as the empty `--tools ""` came out mangled. With
/// `/s` cmd strips exactly the outer pair added here and runs the rest as
/// written. `/d` skips AutoRun commands. With every `"` refused, the only quotes
/// in the line are the pairs [`msvc_quote`] adds, so cmd's quote state matches
/// the argument boundaries, and the operators `&|<>^` it would act on appear
/// only inside them.
#[cfg_attr(not(windows), allow(dead_code))] // Windows-only path; tested everywhere
pub(crate) fn cmd_s_c_line(program: &str, args: &[String]) -> Result<String, String> {
    for a in std::iter::once(program).chain(args.iter().map(String::as_str)) {
        if let Some(c) = a.chars().find(|c| CMD_LINE_REFUSED.contains(c)) {
            return Err(format!(
                "refusing to run through cmd.exe: {a:?} contains {c:?}, which cmd.exe \
                 interprets even inside quotes"
            ));
        }
    }
    let mut line = msvc_quote(program);
    line.push_str(" --print");
    for a in args {
        line.push(' ');
        line.push_str(&msvc_quote(a));
    }
    Ok(format!("/d /s /c \"{line}\""))
}

/// Build the one-shot scorer's `claude --print` command (no extra flags).
/// See [`build_print_command`] for the env discipline and the refusal.
pub(crate) fn build_scorer_command(
    program: &str,
    config_dir: Option<&str>,
) -> Result<std::process::Command, PrintRunError> {
    build_print_command(program, config_dir, &[])
}

/// Why a single `claude --print` child produced no usable stdout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PrintRunError {
    /// The child could not be spawned (missing binary, bad cwd, no thread for
    /// the stdin writer, a command line [`cmd_s_c_line`] refused, …).
    Spawn(String),
    /// The child did not exit within the budget; its process tree was killed.
    Timeout,
    /// The child exited but its captured stdout/stderr is incomplete.
    Truncated(String),
    /// The child exited nonzero.
    NonZeroExit {
        code: Option<i32>,
        stderr_excerpt: String,
    },
}

impl std::fmt::Display for PrintRunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(e) => write!(f, "failed to spawn claude CLI: {e}"),
            Self::Timeout => write!(f, "claude CLI timed out"),
            Self::Truncated(why) => write!(f, "claude CLI output incomplete: {why}"),
            Self::NonZeroExit {
                code,
                stderr_excerpt,
            } => write!(f, "claude CLI exited {code:?}: {stderr_excerpt}"),
        }
    }
}

/// Spawn `cmd`, feed `prompt` on its stdin (then EOF), and wait at most
/// `timeout` for it to exit. Returns the child's TRIMMED stdout on a zero
/// exit. Every failure is an `Err` so the callers fail closed.
///
/// Routed through [`crate::process_helpers::run_with_timeout_input`]: every
/// wait is on a hard clock (no unbounded `wait_with_output`), the prompt is
/// written by a detached thread so a child that never reads stdin cannot block
/// us, and on expiry the whole process TREE is killed — on Windows the real
/// `claude` is a grandchild of `cmd /c`, which a plain `child.kill()` leaves
/// running.
fn run_print_child(
    cmd: std::process::Command,
    prompt: &str,
    timeout: Duration,
) -> Result<String, PrintRunError> {
    use crate::process_helpers::TimedOutput;

    let run =
        crate::process_helpers::run_with_timeout_input(cmd, timeout, prompt.as_bytes().to_vec())
            .map_err(|e| PrintRunError::Spawn(e.to_string()))?;

    let output = match run.outcome {
        TimedOutput::Completed(o) => o,
        TimedOutput::TimedOut { .. } => return Err(PrintRunError::Timeout),
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(PrintRunError::NonZeroExit {
            code: output.status.code(),
            stderr_excerpt: truncate_str(&stderr, 300).to_string(),
        });
    }
    if let Some(t) = run.truncation {
        return Err(PrintRunError::Truncated(format!("{t:?}")));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Run `claude -p <prompt>` (non-interactive print mode) once and return its
/// trimmed stdout, or `None` on any failure (spawn error / missing binary /
/// nonzero exit / timeout). Fail-closed by design — the auto-response caller
/// injects NOTHING on `None`.
///
/// Auth/config reuse: resolves the effective config dir the same way the rest
/// of the CLI provider does ([`get_effective_config_dir`]) and exports it as
/// `CLAUDE_CONFIG_DIR`, refreshing OAuth credentials first — so the one-shot
/// authenticates as the SAME account the runner's sessions use. `CLAUDECODE` is
/// removed so a runner already running inside Claude Code can still spawn the
/// nested print call (mirrors [`process::spawn_and_wait_with_doctor`]).
///
/// Bounded by `timeout`: the child is killed and `None` returned if it does not
/// exit in time, so a wedged CLI never strands the scheduler task.
pub(crate) fn score_options_via_cli(
    prompt: &str,
    settings: &ClaudeCliSettings,
    timeout: Duration,
) -> Option<String> {
    let program = scorer_claude_program(settings);
    let (config_dir, _config_dir_source) = get_effective_config_dir(settings);

    // Refresh OAuth before spawning, same as run_claude_cli.
    super::oauth_refresh::try_ensure_valid_credentials(config_dir.as_deref());

    let stdout = match build_scorer_command(&program, config_dir.as_deref())
        .and_then(|cmd| run_print_child(cmd, prompt, timeout))
    {
        Ok(s) => s,
        Err(PrintRunError::Timeout) => {
            warn!(
                timeout_secs = timeout.as_secs(),
                "auto_response scorer: claude CLI timed out — killing, no scores"
            );
            return None;
        }
        Err(PrintRunError::NonZeroExit {
            code,
            stderr_excerpt,
        }) => {
            warn!(
                exit = ?code,
                stderr = %stderr_excerpt,
                "auto_response scorer: claude CLI nonzero exit — no scores"
            );
            return None;
        }
        Err(e) => {
            warn!(error = %e, "auto_response scorer: claude CLI failed — no scores");
            return None;
        }
    };

    if stdout.is_empty() {
        debug!("auto_response scorer: claude CLI produced empty stdout — no scores");
        return None;
    }
    Some(stdout)
}

// ── Structured single-shot (scheduled-maintenance Phases 2-3) ────────────────
//
// coord's maintenance subsystem asks the runner semantic questions (verdicts
// over a checkout) and to compile natural-language task descriptions. coord
// composes the full prompt; the runner runs it ONCE through its own
// authenticated Claude CLI with a closed tool set and returns the first JSON
// object of the model's final answer. Same env discipline + bounded wait as the
// scorer above (shared `build_print_command` / `run_print_child`).

/// The tool set a structured one-shot may use.
///
/// Both modes run under `--safe-mode`, which the CLI's `--help` (2.1.330)
/// describes, verbatim: "Start with all customizations (CLAUDE.md, skills,
/// installed plugins, hooks, MCP servers, custom commands and agents, output
/// styles, workflows, custom themes, keybindings, and more) disabled — useful
/// for troubleshooting a broken configuration. Admin-managed (policy) settings
/// and the hooks of managed plugins still apply. Auth, model selection,
/// built-in tools and plugins, and permissions work normally. Sets
/// CLAUDE_CODE_SAFE_MODE=1."
///
/// Neither mode includes `Bash`, or any other tool that runs a command. A shell
/// allow-list was tried and dropped: Claude Code matches rules against the raw
/// command string, so shell quoting (`--"output"=…`) gets past deny rules, and
/// "read-only" git still reads files outside the checkout (`diff --no-index`)
/// and runs programs named in repo config (`diff.external`, textconv,
/// `core.fsmonitor`, clean filters, `gpg.program`). Git facts a verdict needs
/// are gathered by the RUNNER before the run through libgit2 (the runner never
/// spawns the git CLI here) and appended to the prompt as delimited data
/// (`agent_worktree::maintenance_executor`). The child cannot use the git CLI
/// on the checkout either — the claude CLI runs git itself — because
/// [`run_structured_oneshot_with`] points its `GIT_DIR` at a directory that is
/// not a repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OneshotTools {
    /// File inspection only: `Read`, `Grep`, `Glob`. `--restricted` confines
    /// them to the working directory (the checkout root), and they are the only
    /// tools that exist (`--tools`) and the only ones pre-approved
    /// (`--allowedTools`) under `--permission-mode dontAsk`. Reads under any
    /// `.git/` are denied ([`READONLY_REPO_DENY`]).
    ReadonlyRepo,
    /// No tools at all (`--tools ""`): a pure text-in, text-out call.
    None,
}

/// Inherited git location overrides removed from a structured one-shot's env
/// (its `GIT_DIR` is pinned — see [`run_structured_oneshot_with`]).
const CHILD_GIT_OVERRIDES_REMOVED: [&str; 6] = [
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
];

/// The only tools [`OneshotTools::ReadonlyRepo`] provides — none of them runs a
/// command or writes.
const READONLY_REPO_TOOLS: &[&str] = &["Read", "Grep", "Glob"];

/// Deny rules (`--disallowedTools`) for [`OneshotTools::ReadonlyRepo`]: no
/// reads under the checkout's `.git/` (config, objects, hooks, credentials in
/// remotes). Claude Code's path rules: `./x` is relative to the working
/// directory, and a gitignore-style `**/.git/**` matches at the root and at
/// any depth (nested repos, submodules). Deny beats allow, so
/// `--allowedTools Read` cannot re-open these.
///
/// What is true about Grep and Glob: the CLI has NO `Grep(path)` / `Glob(path)`
/// specifier. Its docs state that `Read(path)` deny rules are applied to the
/// file-reading tools Grep and Glob on a best-effort basis, so these rules are
/// the only lever for them too.
const READONLY_REPO_DENY: &[&str] = &["Read(./.git/**)", "Read(**/.git/**)"];

/// Why a structured one-shot produced no answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OneshotError {
    /// The `claude` child failed (spawn / stdin / timeout / nonzero exit).
    Run(PrintRunError),
    /// The CLI's `--output-format json` envelope reported an error result
    /// (e.g. `error_max_turns`), or carried no result text.
    CliError(String),
    /// The model's answer contained no parseable JSON object.
    Unparseable(String),
}

impl OneshotError {
    /// Whether this failure was the bounded wait expiring.
    pub(crate) fn is_timeout(&self) -> bool {
        matches!(self, Self::Run(PrintRunError::Timeout))
    }
}

impl std::fmt::Display for OneshotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Run(e) => write!(f, "{e}"),
            Self::CliError(e) => write!(f, "claude CLI reported an error: {e}"),
            Self::Unparseable(e) => write!(f, "unparseable claude output: {e}"),
        }
    }
}

/// The CLI flags (after `--print`) for a structured one-shot. Pure, so each
/// tools mode's argv is pinned by a test without spawning `claude`.
///
/// - `--output-format json`: one JSON envelope whose `result` is the model's
///   final text ([`final_text_from_cli_output`]).
/// - `--max-turns`: bounds the tool loop.
/// - `--safe-mode`: per `--help`, all customizations disabled (CLAUDE.md,
///   skills, installed plugins, hooks, MCP servers, custom commands and agents,
///   output styles, workflows, custom themes, keybindings, and more);
///   admin-managed (policy) settings and the hooks of managed plugins still
///   apply; auth, model selection, built-in tools and plugins, and permissions
///   work normally. See [`OneshotTools`].
/// - `--restricted`: user/project/local settings files are ignored, so an
///   operator's allow rules cannot add tools; command-running tools exist only
///   if `--tools` names them (it never does here); the file tools are confined
///   to the working directory.
/// - `--strict-mcp-config` with no `--mcp-config`: no MCP servers at all.
/// - `--permission-mode dontAsk`: anything not pre-approved is denied (never
///   prompts — nobody is at the keyboard).
/// - `--tools`: the built-in tools that EXIST — `Read,Grep,Glob`, or `""`
///   for none.
/// - `--disallowedTools`: the `.git/` read denials ([`READONLY_REPO_DENY`]).
/// - `--allowedTools`: the three tools, pre-approved.
///
/// Both permission flags are variadic, so they come last and every value is a
/// `Tool(...)` rule or a tool name — none starts with `-`, so each list ends at
/// the next `--` flag. No `Bash` token appears in either mode — see
/// [`OneshotTools`].
pub(crate) fn oneshot_cli_args(tools: OneshotTools, max_turns: u32) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "--output-format".into(),
        "json".into(),
        "--max-turns".into(),
        max_turns.max(1).to_string(),
        "--safe-mode".into(),
        "--restricted".into(),
        "--strict-mcp-config".into(),
        "--permission-mode".into(),
        "dontAsk".into(),
        "--tools".into(),
    ];
    match tools {
        OneshotTools::None => args.push(String::new()),
        OneshotTools::ReadonlyRepo => {
            args.push(READONLY_REPO_TOOLS.join(","));
            args.push("--disallowedTools".into());
            args.extend(READONLY_REPO_DENY.iter().map(|s| s.to_string()));
            args.push("--allowedTools".into());
            args.extend(READONLY_REPO_TOOLS.iter().map(|s| s.to_string()));
        }
    }
    args
}

/// Extract the model's final text from `claude --print --output-format json`
/// stdout: `{"type":"result","subtype":"success","is_error":false,
/// "result":"<text>",…}`. A stdout that is not such an envelope is returned
/// verbatim (a CLI that ignored the flag prints plain text).
pub(crate) fn final_text_from_cli_output(stdout: &str) -> Result<String, OneshotError> {
    let trimmed = stdout.trim();
    let Ok(serde_json::Value::Object(env)) = serde_json::from_str::<serde_json::Value>(trimmed)
    else {
        return Ok(trimmed.to_string());
    };
    if env.get("type").and_then(|v| v.as_str()) != Some("result") {
        // A JSON object that is not the CLI envelope — the answer itself.
        return Ok(trimmed.to_string());
    }
    let is_error = env
        .get("is_error")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let subtype = env
        .get("subtype")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    if is_error || subtype != "success" {
        let detail = env.get("result").and_then(|v| v.as_str()).unwrap_or("");
        return Err(OneshotError::CliError(format!(
            "{subtype}: {}",
            truncate_str(detail, 200)
        )));
    }
    match env.get("result").and_then(|v| v.as_str()) {
        Some(text) => Ok(text.to_string()),
        None => Err(OneshotError::CliError(
            "result envelope carried no result text".to_string(),
        )),
    }
}

/// Strip a single leading/trailing Markdown code fence pair if present,
/// tolerating an optional language tag (```json). Returns the inner content;
/// if no fence is found, returns the input trimmed.
pub(crate) fn strip_code_fences(raw: &str) -> String {
    let t = raw.trim();
    if let Some(rest) = t.strip_prefix("```") {
        // Drop the optional language tag up to the first newline.
        let after_tag = match rest.split_once('\n') {
            Some((_tag, body)) => body,
            None => rest,
        };
        let inner = after_tag.strip_suffix("```").unwrap_or(after_tag);
        // Also handle a trailing fence on its own line.
        return inner.trim_end_matches('`').trim().to_string();
    }
    t.to_string()
}

/// The first JSON OBJECT in `raw`, tolerating Markdown fences and prose around
/// it. Tries the fence-stripped text as a whole first, then scans each `{` in
/// order and takes the first position from which a complete object parses.
/// `None` when no object is present.
pub(crate) fn extract_first_json_object(raw: &str) -> Option<serde_json::Value> {
    let cleaned = strip_code_fences(raw);
    if let Ok(v @ serde_json::Value::Object(_)) = serde_json::from_str(&cleaned) {
        return Some(v);
    }
    for (idx, _) in raw.match_indices('{') {
        let Some(tail) = raw.get(idx..) else {
            continue;
        };
        let mut stream = serde_json::Deserializer::from_str(tail).into_iter::<serde_json::Value>();
        if let Some(Ok(v @ serde_json::Value::Object(_))) = stream.next() {
            return Some(v);
        }
    }
    None
}

/// Run `prompt` ONCE through the runner's authenticated Claude CLI with the
/// given closed tool set, and return the first JSON object of the model's
/// final answer.
///
/// - `cwd`: the child's working directory (`None` = inherit the runner's).
/// - `git_dir`: an existing directory that is NOT a git repository (an empty
///   runner-private one); becomes the child's `GIT_DIR` — see
///   [`run_structured_oneshot_with`].
/// - `max_turns`: bounds the tool loop (floored at 1).
/// - `timeout`: the child is killed and [`OneshotError::Run`]`(Timeout)`
///   returned if it does not exit in time.
///
/// Blocking (spawns + polls a subprocess): call it on the blocking pool.
pub(crate) fn run_structured_oneshot(
    prompt: &str,
    cwd: Option<&Path>,
    git_dir: &Path,
    tools: OneshotTools,
    max_turns: u32,
    timeout: Duration,
) -> Result<serde_json::Value, OneshotError> {
    let settings = crate::settings::load_settings();
    let cli = &settings.ai.claude_cli;
    let program = scorer_claude_program(cli);
    let (config_dir, _config_dir_source) = get_effective_config_dir(cli);
    super::oauth_refresh::try_ensure_valid_credentials(config_dir.as_deref());

    run_structured_oneshot_with(
        &program,
        config_dir.as_deref(),
        prompt,
        cwd,
        git_dir,
        tools,
        max_turns,
        timeout,
    )
}

/// [`run_structured_oneshot`] with the program and account already resolved —
/// the seam the spawn tests drive with a fake `claude` script, so they never
/// touch the process env, the settings file or the real CLI.
///
/// The child's git is pinned away from the checkout: the claude CLI runs the
/// git CLI in its working directory on its own (2.1.330 ran `rev-parse`,
/// `log`, `status` and `remote` there), and the git CLI executes programs a
/// repository's config names (`core.fsmonitor`, filters, `gpg.program`, …).
/// `GIT_DIR` is set to `git_dir`, which is not a repository, so every git the
/// child — or anything it starts — runs fails with "not a git repository"
/// instead of reading the checkout's config; inherited `GIT_WORK_TREE` /
/// `GIT_COMMON_DIR` / index / object-store overrides are removed.
/// `GIT_CEILING_DIRECTORIES` would not do it: git still examines the working
/// directory itself before it walks upward.
pub(crate) fn run_structured_oneshot_with(
    program: &str,
    config_dir: Option<&str>,
    prompt: &str,
    cwd: Option<&Path>,
    git_dir: &Path,
    tools: OneshotTools,
    max_turns: u32,
    timeout: Duration,
) -> Result<serde_json::Value, OneshotError> {
    let mut cmd = build_print_command(program, config_dir, &oneshot_cli_args(tools, max_turns))
        .map_err(OneshotError::Run)?;
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd.env("GIT_DIR", git_dir);
    for var in CHILD_GIT_OVERRIDES_REMOVED {
        cmd.env_remove(var);
    }

    let stdout = run_print_child(cmd, prompt, timeout).map_err(OneshotError::Run)?;
    let text = final_text_from_cli_output(&stdout)?;
    extract_first_json_object(&text)
        .ok_or_else(|| OneshotError::Unparseable(truncate_str(text.trim(), 160).to_string()))
}

/// Process CLI output into AiResponse
///
/// Note: CLI providers don't expose token counts in their output,
/// so input_tokens and output_tokens will be None.
fn process_cli_output(output: std::process::Output) -> AiResponse {
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if output.status.success() {
        debug!("Claude CLI response length: {} chars", stdout.len());
        if stdout.trim().is_empty() && !stderr.trim().is_empty() {
            // CLI exited 0 but produced no stdout — stderr likely has the real error
            warn!(
                "Claude CLI exited successfully but stdout is empty. stderr ({} chars): {}",
                stderr.len(),
                if stderr.len() > 1000 {
                    truncate_str(&stderr, 1000)
                } else {
                    &stderr
                }
            );
            // Return as error since empty output is not useful
            AiResponse::error(format!(
                "Claude CLI produced no output. stderr: {}",
                if stderr.len() > 500 {
                    truncate_str(&stderr, 500)
                } else {
                    &stderr
                }
            ))
        } else {
            AiResponse::success(stdout)
        }
    } else {
        let exit_code = output
            .status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "signal".to_string());

        // Include stdout in error when stderr is empty (common on Windows where
        // PowerShell piping may redirect child stderr to its own error stream)
        let diagnostic = if stderr.trim().is_empty() && !stdout.trim().is_empty() {
            format!(
                "Claude CLI failed (exit {}): stdout: {}",
                exit_code,
                if stdout.len() > 500 {
                    truncate_str(&stdout, 500)
                } else {
                    &stdout
                }
            )
        } else if stderr.trim().is_empty() {
            format!(
                "Claude CLI failed (exit {}) with no output. Check that 'claude' is in PATH and authenticated.",
                exit_code
            )
        } else {
            format!("Claude CLI failed (exit {}): {}", exit_code, stderr)
        };

        error!("{}", diagnostic);
        AiResponse::error_with_output(stdout, diagnostic)
    }
}

#[cfg(test)]
mod tests {
    use super::build_scorer_command;

    // =======================================================================
    // Auto-response option scorer — production call-site coverage for the
    // credential scrub (plan
    // 2026-08-07-runner-context-visibility-and-session-env-secret-hygiene).
    //
    // This seam was missed by #1056's review: it spawns `claude --print`
    // directly instead of going through `process::spawn_and_wait_with_doctor`,
    // so the choke-point fix does NOT cover it. `build_scorer_command` IS the
    // command `score_options_via_cli` spawns — deleting the
    // `scrub_credential_env_std` call from it reddens this test.
    // =======================================================================

    #[test]
    fn scorer_command_scrubs_credential_values() {
        // The scrub applies the LIVE model-gateway plan, which reads the
        // runner config dir. A sibling test's `IsolatedAmbient` rewrites the
        // process env (on Windows, `USERPROFILE` too, which can make the
        // platform config dir unresolvable mid-test); an unresolvable dir is
        // an unresolved gateway that pins `CLAUDE_CONFIG_DIR` away. Hold the
        // fixture so this test reads a known, gateway-free config dir.
        let _amb = crate::test_env::isolated_ambient();
        let cmd = build_scorer_command("claude", Some("/tmp/claude-config")).expect("builds");

        crate::terminal::assert_credentials_scrubbed_std(&cmd, "build_scorer_command");

        let envs: Vec<(String, Option<String>)> = cmd
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().to_string(),
                    v.map(|v| v.to_string_lossy().to_string()),
                )
            })
            .collect();
        assert!(
            envs.iter().any(
                |(k, v)| k == "CLAUDE_CONFIG_DIR" && v.as_deref() == Some("/tmp/claude-config")
            ),
            "the resolved account pin must survive"
        );
        for marker in ["CLAUDECODE", "CLAUDE_CODE_CHILD_SESSION"] {
            assert!(
                envs.iter().any(|(k, v)| k == marker && v.is_none()),
                "{marker} must still be stripped"
            );
        }
    }

    /// The non-interactive git credential posture, asserted from the ONE shared
    /// list so this seam cannot drift from the other seven. Removing the
    /// `apply_non_interactive_git_env_*` call from the production function
    /// reddens this test.
    #[test]
    fn scorer_command_applies_non_interactive_git_posture() {
        let cmd = build_scorer_command("claude", Some("/tmp/claude-config")).expect("builds");
        crate::credential_helper::assert_non_interactive_git_posture_std(
            &cmd,
            "build_scorer_command",
        );
    }

    // =======================================================================
    // Structured single-shot (scheduled-maintenance Phases 2-3).
    // =======================================================================

    use super::{
        build_print_command, extract_first_json_object, final_text_from_cli_output,
        oneshot_cli_args, strip_code_fences, OneshotError, OneshotTools, READONLY_REPO_DENY,
    };

    fn strs(v: &[String]) -> Vec<&str> {
        v.iter().map(String::as_str).collect()
    }

    #[test]
    fn oneshot_args_readonly_repo_pin_the_exact_tool_set() {
        let args = oneshot_cli_args(OneshotTools::ReadonlyRepo, 8);
        assert_eq!(
            strs(&args),
            vec![
                "--output-format",
                "json",
                "--max-turns",
                "8",
                "--safe-mode",
                "--restricted",
                "--strict-mcp-config",
                "--permission-mode",
                "dontAsk",
                "--tools",
                "Read,Grep,Glob",
                "--disallowedTools",
                "Read(./.git/**)",
                "Read(**/.git/**)",
                "--allowedTools",
                "Read",
                "Grep",
                "Glob",
            ]
        );
        // Both permission flags are variadic: deny comes before allow, and no
        // value after either looks like a flag.
        let deny = args.iter().position(|a| a == "--disallowedTools").unwrap();
        let allow = args.iter().position(|a| a == "--allowedTools").unwrap();
        assert!(deny < allow);
        for v in args[deny + 1..].iter().filter(|a| *a != "--allowedTools") {
            assert!(!v.starts_with('-'), "rule value {v:?} looks like a flag");
        }
    }

    /// Design decision after review: the one-shot gets NO shell. No token in
    /// either mode's argv may name `Bash` (or any rule over it), and no
    /// permission-widening flag may appear.
    #[test]
    fn no_bash_token_in_either_mode() {
        for tools in [OneshotTools::ReadonlyRepo, OneshotTools::None] {
            let args = oneshot_cli_args(tools, 4);
            for a in &args {
                assert!(
                    !a.to_ascii_lowercase().contains("bash"),
                    "{tools:?}: argv token {a:?} names Bash"
                );
            }
            for forbidden in [
                "--disallowed-tools",
                "--dangerously-skip-permissions",
                "--add-dir",
                "--mcp-config",
                "--settings",
            ] {
                assert!(
                    !args.iter().any(|a| a == forbidden),
                    "{tools:?}: unexpected {forbidden}"
                );
            }
            for required in [
                "--safe-mode",
                "--restricted",
                "--strict-mcp-config",
                "dontAsk",
            ] {
                assert!(
                    args.iter().any(|a| a == required),
                    "{tools:?}: missing {required}"
                );
            }
            // The only deny rules are the `.git/` read denials.
            if let Some(d) = args.iter().position(|a| a == "--disallowedTools") {
                assert_eq!(tools, OneshotTools::ReadonlyRepo);
                let allow = args.iter().position(|a| a == "--allowedTools").unwrap();
                assert_eq!(&args[d + 1..allow], READONLY_REPO_DENY);
            }
        }
    }

    #[test]
    fn oneshot_args_none_disables_every_tool() {
        let args = oneshot_cli_args(OneshotTools::None, 1);
        assert_eq!(
            strs(&args),
            vec![
                "--output-format",
                "json",
                "--max-turns",
                "1",
                "--safe-mode",
                "--restricted",
                "--strict-mcp-config",
                "--permission-mode",
                "dontAsk",
                "--tools",
                "",
            ]
        );
        assert!(!args.iter().any(|a| a == "--allowedTools"));
        assert!(!args.iter().any(|a| a == "--disallowedTools"));
    }

    #[test]
    fn oneshot_args_floor_max_turns_at_one() {
        let args = oneshot_cli_args(OneshotTools::None, 0);
        let pos = args.iter().position(|a| a == "--max-turns").unwrap();
        assert_eq!(args[pos + 1], "1");
    }

    /// The structured one-shot spawns through the same env preamble as the
    /// scorer — credential scrub + git posture — and carries its flags.
    #[test]
    fn print_command_with_oneshot_args_keeps_the_scrub_and_posture() {
        let _amb = crate::test_env::isolated_ambient();
        let cmd = build_print_command(
            "claude",
            Some("/tmp/claude-config"),
            &oneshot_cli_args(OneshotTools::ReadonlyRepo, 4),
        )
        .expect("builds");
        crate::terminal::assert_credentials_scrubbed_std(&cmd, "build_print_command");
        crate::credential_helper::assert_non_interactive_git_posture_std(
            &cmd,
            "build_print_command",
        );
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        if cfg!(windows) {
            assert_eq!(args.len(), 1, "one raw cmd.exe argument: {args:?}");
            assert!(
                args[0].starts_with("/d /s /c \"claude --print --output-format json "),
                "{args:?}"
            );
            assert!(args[0].contains(" --safe-mode --restricted "), "{args:?}");
            assert!(args[0].contains(" --tools Read,Grep,Glob "), "{args:?}");
            // Review round 7: no current-directory program lookup, so a
            // planted `claude.cmd` in the checkout cwd cannot win over PATH.
            let pinned = cmd
                .get_envs()
                .find(|(k, _)| *k == "NoDefaultCurrentDirectoryInExePath")
                .and_then(|(_, v)| v);
            assert_eq!(pinned, Some(std::ffi::OsStr::new("1")));
        } else {
            let print = args.iter().position(|a| a == "--print").unwrap();
            assert_eq!(args[print + 1], "--output-format");
            assert!(args.iter().any(|a| a == "Read,Grep,Glob"));
        }
    }

    /// The cmd.exe line quotes a program path with spaces, keeps an empty
    /// argument as `""`, doubles trailing backslashes in a quoted argument, and
    /// wraps the whole line in the one quote pair `/s` strips.
    #[test]
    fn cmd_s_c_line_quotes_program_and_empty_args() {
        let line = super::cmd_s_c_line(
            r"C:\Program Files\claude\claude.cmd",
            &[
                "--tools".to_string(),
                String::new(),
                "Read(./.git/**)".to_string(),
                r"x y\".to_string(),
                r"C:\plain\".to_string(),
                "a&b".to_string(),
            ],
        )
        .expect("no refused character");
        assert_eq!(
            line,
            r#"/d /s /c ""C:\Program Files\claude\claude.cmd" --print --tools "" Read(./.git/**) "x y\\" C:\plain\ "a&b""#
        );
    }

    /// Round-6 A1: a `"` toggles cmd.exe's quote state (it ignores the MSVC
    /// `\"` escape), so `"a\"b" … "a&echo INJECTED"` ran `echo INJECTED`; `%`
    /// expands inside quotes, `!` under delayed expansion, and CR/LF end the
    /// line. Every one is REFUSED — in the program or in any argument — and
    /// the builder maps the refusal to a spawn error before anything runs.
    #[test]
    fn cmd_s_c_line_refuses_what_cmd_interprets_inside_quotes() {
        let ok = vec!["--tools".to_string(), String::new()];
        for bad in ["a\"b", "%PATH%", "50%", "a!b!", "a\rb", "a\nb"] {
            let err = super::cmd_s_c_line("claude", &[bad.to_string(), "a&echo INJECTED".into()])
                .expect_err(bad);
            assert!(err.contains("cmd.exe"), "{bad:?}: {err}");
            let err = super::cmd_s_c_line(&format!(r"C:\x\{bad}\claude.cmd"), &ok).expect_err(bad);
            assert!(err.contains("cmd.exe"), "program {bad:?}: {err}");
        }
        assert!(super::cmd_s_c_line("claude", &ok).is_ok());
        #[cfg(windows)]
        {
            match build_print_command("claude", None, &["a\"b".to_string()]) {
                Err(PrintRunError::Spawn(e)) => assert!(e.contains("cmd.exe"), "{e}"),
                other => panic!("expected a spawn refusal, got {:?}", other.map(|_| ())),
            }
        }
    }

    #[test]
    fn final_text_reads_the_json_envelope_result() {
        let out = r#"{"type":"result","subtype":"success","is_error":false,"result":"```json\n{\"verdict\":\"yes\"}\n```","num_turns":2}"#;
        assert_eq!(
            final_text_from_cli_output(out).unwrap(),
            "```json\n{\"verdict\":\"yes\"}\n```"
        );
    }

    #[test]
    fn final_text_maps_an_error_envelope_to_cli_error() {
        let out = r#"{"type":"result","subtype":"error_max_turns","is_error":true}"#;
        match final_text_from_cli_output(out) {
            Err(OneshotError::CliError(m)) => assert!(m.starts_with("error_max_turns")),
            other => panic!("expected CliError, got {other:?}"),
        }
    }

    #[test]
    fn final_text_passes_plain_text_and_bare_answers_through() {
        assert_eq!(final_text_from_cli_output("  hello  ").unwrap(), "hello");
        // A JSON object that is not the CLI envelope is the answer itself.
        assert_eq!(
            final_text_from_cli_output(r#"{"verdict":"no"}"#).unwrap(),
            r#"{"verdict":"no"}"#
        );
    }

    #[test]
    fn extract_json_from_fenced_output() {
        let raw = "```json\n{\"verdict\":\"yes\",\"confidence\":0.9,\"rationale\":\"r\"}\n```";
        let v = extract_first_json_object(raw).unwrap();
        assert_eq!(v["verdict"], "yes");
    }

    #[test]
    fn extract_json_from_bare_fence_and_surrounding_prose() {
        let fenced = "```\n{\"a\":1}\n```";
        assert_eq!(extract_first_json_object(fenced).unwrap()["a"], 1);
        let prose = "Here is my answer:\n{\"result\":{\"name\":\"x\"},\"rationale\":\"ok\"}\nThanks {not json}";
        let v = extract_first_json_object(prose).unwrap();
        assert_eq!(v["result"]["name"], "x");
        // Prose then a fenced block.
        let mixed = "Sure.\n```json\n{\"b\":2}\n```\n";
        assert_eq!(extract_first_json_object(mixed).unwrap()["b"], 2);
    }

    #[test]
    fn extract_json_skips_a_broken_brace_and_rejects_non_objects() {
        assert_eq!(
            extract_first_json_object("{oops} then {\"c\":3}").unwrap()["c"],
            3
        );
        assert!(extract_first_json_object("no json here").is_none());
        assert!(extract_first_json_object("[1,2,3]").is_none());
        assert!(extract_first_json_object("").is_none());
    }

    #[test]
    fn strip_code_fences_handles_tag_and_no_fence() {
        assert_eq!(strip_code_fences("```json\n{}\n```"), "{}");
        assert_eq!(strip_code_fences("  {}  "), "{}");
    }

    // =======================================================================
    // Spawn tests — a fake `claude` script per arm, driven through the real
    // command builder and bounded runner (`run_structured_oneshot_with`), so
    // the env scrub, `cmd /c` routing, stdin feed and tree kill are all live.
    // Hermetic: the script lives in a tempdir; nothing reads the process env,
    // the settings file or the real CLI.
    // =======================================================================

    use super::{run_print_child, run_structured_oneshot_with, PrintRunError};
    use std::time::{Duration, Instant};

    /// Write a fake `claude` into `dir`: a `.cmd` on Windows (reached through
    /// `cmd /c`, exactly as a real npm shim is), an executable `sh` script
    /// elsewhere. Returns the program path to hand to the builder.
    fn fake_claude(dir: &std::path::Path, windows_body: &str, unix_body: &str) -> String {
        if cfg!(windows) {
            let p = dir.join("fake-claude.cmd");
            std::fs::write(
                &p,
                format!("@echo off\r\n{}\r\n", windows_body.replace('\n', "\r\n")),
            )
            .unwrap();
            p.to_string_lossy().to_string()
        } else {
            let p = dir.join("fake-claude.sh");
            std::fs::write(&p, format!("#!/bin/sh\n{unix_body}\n")).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            p.to_string_lossy().to_string()
        }
    }

    const ENVELOPE_OK: &str = r#"{"type":"result","subtype":"success","is_error":false,"result":"{\"verdict\":\"yes\",\"confidence\":0.9,\"rationale\":\"r\"}"}"#;

    #[test]
    fn spawn_success_envelope_yields_the_answer_object() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let prog = fake_claude(
            dir.path(),
            &format!("echo {ENVELOPE_OK}"),
            &format!("cat >/dev/null\nprintf '%s\\n' '{ENVELOPE_OK}'"),
        );
        let v = run_structured_oneshot_with(
            &prog,
            None,
            "the prompt",
            Some(dir.path()),
            dir.path(),
            OneshotTools::ReadonlyRepo,
            4,
            Duration::from_secs(30),
        )
        .unwrap();
        assert_eq!(v["verdict"], "yes");
        assert_eq!(v["rationale"], "r");
    }

    #[test]
    fn spawn_nonzero_exit_is_a_run_error() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let prog = fake_claude(
            dir.path(),
            "echo boom 1>&2\nexit /b 3",
            "echo boom >&2\nexit 3",
        );
        match run_structured_oneshot_with(
            &prog,
            None,
            "p",
            Some(dir.path()),
            dir.path(),
            OneshotTools::None,
            1,
            Duration::from_secs(30),
        ) {
            Err(OneshotError::Run(PrintRunError::NonZeroExit {
                code,
                stderr_excerpt,
            })) => {
                assert_eq!(code, Some(3));
                assert!(stderr_excerpt.contains("boom"), "{stderr_excerpt:?}");
            }
            other => panic!("expected NonZeroExit, got {other:?}"),
        }
    }

    /// The deadline returns promptly AND the child's whole TREE is dead.
    ///
    /// The fake `claude` launches a GRANDCHILD (a backgrounded `sh -c` on Unix,
    /// `start /b cmd /c` on Windows) that writes `started` at once and
    /// `finished` after a sleep longer than the timeout, while the child itself
    /// just waits. A kill that reached only the direct child (`child.kill()`)
    /// would leave the grandchild running, and `finished` would appear — so
    /// this test fails unless the tree reaper (process group / job object)
    /// fires. `started` must exist, so the test cannot pass vacuously because
    /// the grandchild never ran.
    #[test]
    fn spawn_timeout_kills_the_child_tree() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let started = dir.path().join("started");
        let finished = dir.path().join("finished");
        let (s, f) = (
            started.to_string_lossy().to_string(),
            finished.to_string_lossy().to_string(),
        );
        let grand = dir.path().join("grandchild.cmd");
        if cfg!(windows) {
            std::fs::write(
                &grand,
                format!(
                    "@echo off\r\necho s> \"{s}\"\r\nping -n 7 127.0.0.1 >nul\r\necho d> \"{f}\"\r\n"
                ),
            )
            .unwrap();
        }
        let g = grand.to_string_lossy().to_string();
        let prog = fake_claude(
            dir.path(),
            &format!("start \"\" /b cmd /c \"{g}\"\nping -n 15 127.0.0.1 >nul"),
            &format!("sh -c \"echo s > '{s}'; sleep 6; echo d > '{f}'\" &\nwait"),
        );
        let t0 = Instant::now();
        let r = run_structured_oneshot_with(
            &prog,
            None,
            "p",
            Some(dir.path()),
            dir.path(),
            OneshotTools::None,
            1,
            Duration::from_secs(3),
        );
        let elapsed = t0.elapsed();
        assert!(
            matches!(&r, Err(e) if e.is_timeout()),
            "expected a timeout, got {r:?}"
        );
        assert!(
            elapsed < Duration::from_secs(6),
            "deadline not honoured: {elapsed:?}"
        );
        assert!(
            started.exists(),
            "the grandchild never started — the test would prove nothing"
        );
        // Past the grandchild's own finish time (6s after it started).
        std::thread::sleep(Duration::from_secs(7));
        assert!(
            !finished.exists(),
            "the grandchild survived the timeout kill — only the direct child died"
        );
    }

    #[test]
    fn spawn_truncated_or_unparseable_output_is_unparseable() {
        let _amb = crate::test_env::isolated_ambient();
        for (win, unix) in [
            // A truncated envelope (stdout cut mid-object).
            (
                r#"echo {"type":"result","subtype":"succ"#,
                r#"printf '%s\n' '{"type":"result","subtype":"succ'"#,
            ),
            // A complete envelope whose answer holds no JSON object.
            (
                r#"echo {"type":"result","subtype":"success","is_error":false,"result":"I think yes."}"#,
                r#"printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"result":"I think yes."}'"#,
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let prog = fake_claude(dir.path(), win, unix);
            match run_structured_oneshot_with(
                &prog,
                None,
                "p",
                Some(dir.path()),
                dir.path(),
                OneshotTools::None,
                1,
                Duration::from_secs(30),
            ) {
                Err(OneshotError::Unparseable(_)) => {}
                other => panic!("expected Unparseable for {unix:?}, got {other:?}"),
            }
        }
    }

    /// The tool flags reach the child intact through the real builder — on
    /// Windows through `cmd /c`: the empty `--tools ""` arrives as its own
    /// quoted token, `Read,Grep,Glob` as one token, and nothing naming `Bash`
    /// is on the child's command line.
    #[test]
    fn spawn_permission_flags_reach_the_child_intact() {
        let _amb = crate::test_env::isolated_ambient();
        let tmp = tempfile::tempdir().unwrap();
        // A10: a program path that needs quoting — the case plain `cmd /c`
        // mangled together with the empty `--tools ""`.
        let spaced = tmp.path().join("dir with space");
        std::fs::create_dir_all(&spaced).unwrap();
        let dir = &spaced;
        let prog = fake_claude(
            dir,
            "echo ARGV: %*",
            "printf 'ARGV:'\nfor a in \"$@\"; do printf '[%s]' \"$a\"; done\necho",
        );
        let none = build_print_command(&prog, None, &oneshot_cli_args(OneshotTools::None, 1))
            .expect("builds");
        let out_none = run_print_child(none, "", Duration::from_secs(30)).unwrap();
        let repo = build_print_command(
            &prog,
            None,
            &oneshot_cli_args(OneshotTools::ReadonlyRepo, 2),
        )
        .expect("builds");
        let out_repo = run_print_child(repo, "", Duration::from_secs(30)).unwrap();
        if cfg!(windows) {
            assert!(out_none.ends_with("--tools \"\""), "{out_none}");
            assert!(
                out_none.contains(" --safe-mode --restricted "),
                "{out_none}"
            );
            assert!(out_repo.contains(" --tools Read,Grep,Glob "), "{out_repo}");
            assert!(
                out_repo.contains(
                    " --disallowedTools Read(./.git/**) Read(**/.git/**) --allowedTools "
                ),
                "{out_repo}"
            );
        } else {
            assert!(out_none.ends_with("[--tools][]"), "{out_none}");
            assert!(
                out_none.contains("[--safe-mode][--restricted]"),
                "{out_none}"
            );
            assert!(out_repo.contains("[--tools][Read,Grep,Glob]"), "{out_repo}");
            assert!(
                out_repo.contains(
                    "[--disallowedTools][Read(./.git/**)][Read(**/.git/**)][--allowedTools]"
                ),
                "{out_repo}"
            );
        }
        for out in [&out_none, &out_repo] {
            assert!(!out.to_ascii_lowercase().contains("bash"), "{out}");
        }
    }

    /// Round-6 A4: the child's git is pinned away from the checkout. The fake
    /// `claude` writes its own `GIT_DIR` / `GIT_WORK_TREE` / `GIT_COMMON_DIR`
    /// to a file: `GIT_DIR` must be the non-repository directory handed in, and
    /// the location overrides must be absent — so a git the claude CLI runs in
    /// the checkout finds no repository.
    #[test]
    fn spawn_child_git_dir_is_pinned_to_a_non_repository() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let no_git = dir.path().join("no-git-dir");
        std::fs::create_dir_all(&no_git).unwrap();
        let out = dir.path().join("child-env.txt");
        let o = out.to_string_lossy().to_string();
        let prog = fake_claude(
            dir.path(),
            &format!(
                "echo [%GIT_DIR%]> \"{o}\"\necho [%GIT_WORK_TREE%]>> \"{o}\"\necho [%GIT_COMMON_DIR%]>> \"{o}\"\necho {ENVELOPE_OK}"
            ),
            &format!(
                "cat >/dev/null\nprintf '[%s]\\n' \"$GIT_DIR\" \"${{GIT_WORK_TREE-}}\" \"${{GIT_COMMON_DIR-}}\" > '{o}'\nprintf '%s\\n' '{ENVELOPE_OK}'"
            ),
        );
        let v = run_structured_oneshot_with(
            &prog,
            None,
            "p",
            Some(dir.path()),
            &no_git,
            OneshotTools::ReadonlyRepo,
            2,
            Duration::from_secs(30),
        )
        .unwrap();
        assert_eq!(v["verdict"], "yes", "the run itself succeeded");
        let env = std::fs::read_to_string(&out).expect("the fake claude ran");
        let lines: Vec<&str> = env.lines().map(str::trim_end).collect();
        assert_eq!(
            lines,
            vec![format!("[{}]", no_git.display()).as_str(), "[]", "[]"],
            "{env:?}"
        );
    }
}
