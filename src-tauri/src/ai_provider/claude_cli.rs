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
/// On Windows route through cmd.exe `/c` (so a `.cmd`/`.ps1` shim on PATH
/// resolves), elsewhere invoke the program directly. The prompt is piped over
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
/// last: the returned command is spawned with no further env mutation (a
/// caller may only set the working directory). See
/// `crate::terminal::CREDENTIAL_VALUE_ENV_VARS`.
///
/// Returned by value (rather than the env work being inlined at the call site)
/// so the constructed environment is unit-testable without spawning `claude`.
pub(crate) fn build_print_command(
    program: &str,
    config_dir: Option<&str>,
    extra_args: &[String],
) -> std::process::Command {
    let mut cmd = if std::env::consts::OS == "windows" {
        let mut c = crate::process_helpers::cmd_no_window();
        c.args(["/c", program, "--print"]);
        c
    } else {
        let mut c = crate::process_helpers::no_window(program);
        c.arg("--print");
        c
    };
    cmd.args(extra_args);
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

    cmd
}

/// Build the one-shot scorer's `claude --print` command (no extra flags).
/// See [`build_print_command`] for the env discipline.
pub(crate) fn build_scorer_command(
    program: &str,
    config_dir: Option<&str>,
) -> std::process::Command {
    build_print_command(program, config_dir, &[])
}

/// Why a single `claude --print` child produced no usable stdout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PrintRunError {
    /// The child could not be spawned (missing binary, bad cwd, no thread for
    /// the stdin writer, …).
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

    let cmd = build_scorer_command(&program, config_dir.as_deref());

    let stdout = match run_print_child(cmd, prompt, timeout) {
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OneshotTools {
    /// Repository inspection: `Read`, `Grep`, `Glob` (confined to the working
    /// directory by `--restricted`) and the git subcommands in
    /// [`READONLY_GIT_BASH_PATTERNS`] through `Bash`, minus the write forms
    /// [`READONLY_GIT_DENY_PATTERNS`] refuses. Anything not pre-approved is
    /// denied by `--permission-mode dontAsk`.
    ReadonlyGit,
    /// No tools at all (`--tools ""`): a pure text-in, text-out call.
    None,
}

/// The git `Bash` patterns [`OneshotTools::ReadonlyGit`] pre-approves.
///
/// What is and is not true of them: `--restricted` does NOT confine `Bash`,
/// so these patterns are the whole write barrier for shell commands. Each
/// approved subcommand is read-only EXCEPT for the `--output=<file>` option of
/// `log`/`diff`/`show`, which writes an arbitrary path — that form is refused
/// by [`READONLY_GIT_DENY_PATTERNS`] (deny rules win over allow rules).
/// `git branch` is deliberately absent (`-D`/`-f`/`-m` mutate); branch
/// listing goes through `for-each-ref`, which has no write mode.
/// `git status` may refresh the index's stat cache, which changes no content.
const READONLY_GIT_BASH_PATTERNS: &[&str] = &[
    "Bash(git log:*)",
    "Bash(git status:*)",
    "Bash(git diff:*)",
    "Bash(git show:*)",
    "Bash(git for-each-ref:*)",
    "Bash(git rev-parse:*)",
    "Bash(git merge-base:*)",
];

/// Deny rules rendered as `--disallowedTools` for
/// [`OneshotTools::ReadonlyGit`]. `--ou` rather than `--output` because git
/// accepts any unambiguous abbreviation of a long option (`--outp=…`); the
/// mid-pattern `*` also catches the option anywhere after the subcommand.
/// Refusing the harmless `--ours`/`--output-indicator-*` too is accepted.
const READONLY_GIT_DENY_PATTERNS: &[&str] = &[
    "Bash(git log *--ou*)",
    "Bash(git diff *--ou*)",
    "Bash(git show *--ou*)",
];

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
/// - `--restricted`: user/project/local settings files are ignored, so an
///   operator's broad allow rules cannot widen the set below; code-running
///   tools exist only when `--tools` names them; the file tools are confined
///   to the working directory. It does NOT confine `Bash` — the allow/deny
///   patterns do that.
/// - `--strict-mcp-config` with no `--mcp-config`: no MCP servers at all.
/// - `--permission-mode dontAsk`: anything not pre-approved is denied (never
///   prompts — nobody is at the keyboard).
/// - `--tools`: the built-in tools that EXIST (`""` = none).
/// - `--disallowedTools` then `--allowedTools`: deny rules, then what is
///   pre-approved. Both flags are variadic, so they come LAST and every value
///   is a `Tool(...)` pattern — none starts with `-`, so neither can swallow a
///   flag, and each list ends at the next `--` flag.
pub(crate) fn oneshot_cli_args(tools: OneshotTools, max_turns: u32) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "--output-format".into(),
        "json".into(),
        "--max-turns".into(),
        max_turns.max(1).to_string(),
        "--restricted".into(),
        "--strict-mcp-config".into(),
        "--permission-mode".into(),
        "dontAsk".into(),
        "--tools".into(),
    ];
    match tools {
        OneshotTools::None => args.push(String::new()),
        OneshotTools::ReadonlyGit => {
            args.push("Read,Grep,Glob,Bash".into());
            args.push("--disallowedTools".into());
            args.extend(READONLY_GIT_DENY_PATTERNS.iter().map(|s| s.to_string()));
            args.push("--allowedTools".into());
            args.extend(["Read", "Grep", "Glob"].iter().map(|s| s.to_string()));
            args.extend(READONLY_GIT_BASH_PATTERNS.iter().map(|s| s.to_string()));
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
/// - `max_turns`: bounds the tool loop (floored at 1).
/// - `timeout`: the child is killed and [`OneshotError::Run`]`(Timeout)`
///   returned if it does not exit in time.
///
/// Blocking (spawns + polls a subprocess): call it on the blocking pool.
pub(crate) fn run_structured_oneshot(
    prompt: &str,
    cwd: Option<&Path>,
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
        tools,
        max_turns,
        timeout,
    )
}

/// [`run_structured_oneshot`] with the program and account already resolved —
/// the seam the spawn tests drive with a fake `claude` script, so they never
/// touch the process env, the settings file or the real CLI.
pub(crate) fn run_structured_oneshot_with(
    program: &str,
    config_dir: Option<&str>,
    prompt: &str,
    cwd: Option<&Path>,
    tools: OneshotTools,
    max_turns: u32,
    timeout: Duration,
) -> Result<serde_json::Value, OneshotError> {
    let mut cmd = build_print_command(program, config_dir, &oneshot_cli_args(tools, max_turns));
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
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
        let cmd = build_scorer_command("claude", Some("/tmp/claude-config"));

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
        let cmd = build_scorer_command("claude", Some("/tmp/claude-config"));
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
        oneshot_cli_args, strip_code_fences, OneshotError, OneshotTools,
    };

    fn strs(v: &[String]) -> Vec<&str> {
        v.iter().map(String::as_str).collect()
    }

    #[test]
    fn oneshot_args_readonly_git_pin_the_exact_tool_set() {
        let args = oneshot_cli_args(OneshotTools::ReadonlyGit, 8);
        assert_eq!(
            strs(&args),
            vec![
                "--output-format",
                "json",
                "--max-turns",
                "8",
                "--restricted",
                "--strict-mcp-config",
                "--permission-mode",
                "dontAsk",
                "--tools",
                "Read,Grep,Glob,Bash",
                "--disallowedTools",
                "Bash(git log *--ou*)",
                "Bash(git diff *--ou*)",
                "Bash(git show *--ou*)",
                "--allowedTools",
                "Read",
                "Grep",
                "Glob",
                "Bash(git log:*)",
                "Bash(git status:*)",
                "Bash(git diff:*)",
                "Bash(git show:*)",
                "Bash(git for-each-ref:*)",
                "Bash(git rev-parse:*)",
                "Bash(git merge-base:*)",
            ]
        );
        // Both permission flags are variadic: nothing after `--disallowedTools`
        // may be a non-flag value of another option, and no rule value may look
        // like a flag (or it would end its list early / be swallowed).
        let deny = args.iter().position(|a| a == "--disallowedTools").unwrap();
        let allow = args.iter().position(|a| a == "--allowedTools").unwrap();
        assert!(deny < allow);
        for v in args[deny + 1..].iter().filter(|a| *a != "--allowedTools") {
            assert!(!v.starts_with('-'), "rule value {v:?} looks like a flag");
        }
        assert!(!args.iter().any(|a| a == "--dangerously-skip-permissions"));
    }

    /// Claude Code's Bash rule matching, as far as these rules use it: a
    /// legacy `prefix:*` is a prefix match; otherwise `*` matches any run of
    /// characters (including none) and the rest is literal.
    fn rule_matches(rule: &str, command: &str) -> bool {
        let Some(inner) = rule.strip_prefix("Bash(").and_then(|r| r.strip_suffix(')')) else {
            return false;
        };
        if let Some(prefix) = inner.strip_suffix(":*") {
            return command == prefix || command.starts_with(&format!("{prefix} "));
        }
        fn glob(p: &[u8], s: &[u8]) -> bool {
            match p.split_first() {
                None => s.is_empty(),
                Some((b'*', rest)) => (0..=s.len()).any(|i| glob(rest, &s[i..])),
                Some((c, rest)) => s.first() == Some(c) && glob(rest, &s[1..]),
            }
        }
        glob(inner.as_bytes(), command.as_bytes())
    }

    fn admitted(args: &[String], command: &str) -> bool {
        let deny = args.iter().position(|a| a == "--disallowedTools");
        let allow = args.iter().position(|a| a == "--allowedTools");
        let (Some(d), Some(a)) = (deny, allow) else {
            return false;
        };
        let denied = args[d + 1..a].iter().any(|r| rule_matches(r, command));
        let allowed = args[a + 1..].iter().any(|r| rule_matches(r, command));
        allowed && !denied
    }

    /// Review CRITICAL: no allow rule may admit a known mutating git form,
    /// and the deny rules for `--output` must be present.
    #[test]
    fn readonly_git_rules_admit_no_mutating_form() {
        let args = oneshot_cli_args(OneshotTools::ReadonlyGit, 4);
        for rule in [
            "Bash(git log *--ou*)",
            "Bash(git diff *--ou*)",
            "Bash(git show *--ou*)",
        ] {
            assert!(args.iter().any(|a| a == rule), "deny rule {rule} missing");
        }
        for mutating in [
            "git branch -D main",
            "git branch -f main HEAD~1",
            "git branch -m old new",
            "git branch --delete main",
            "git log --output=/tmp/x",
            "git log -p --output=/tmp/x",
            "git log --outp=/tmp/x",
            "git diff --output=/tmp/x HEAD",
            "git diff HEAD --output /tmp/x",
            "git show --output=/tmp/x",
            "git show HEAD --outpu=x",
            "git push origin main",
            "git checkout main",
            "git reset --hard",
            "git commit -m x",
            "rm -rf /",
        ] {
            assert!(
                !admitted(&args, mutating),
                "{mutating:?} must not be admitted"
            );
        }
        // And the read forms still are — the rules are not vacuous.
        for read in [
            "git log --oneline -5",
            "git status --porcelain",
            "git diff HEAD~1",
            "git show HEAD",
            "git for-each-ref refs/heads",
            "git rev-parse HEAD",
            "git merge-base --is-ancestor a b",
        ] {
            assert!(admitted(&args, read), "{read:?} should be admitted");
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
            &oneshot_cli_args(OneshotTools::ReadonlyGit, 4),
        );
        crate::terminal::assert_credentials_scrubbed_std(&cmd, "build_print_command");
        crate::credential_helper::assert_non_interactive_git_posture_std(
            &cmd,
            "build_print_command",
        );
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        let print = args.iter().position(|a| a == "--print").unwrap();
        assert_eq!(args[print + 1], "--output-format");
        assert!(args.iter().any(|a| a == "Bash(git merge-base:*)"));
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
            OneshotTools::ReadonlyGit,
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

    /// The deadline returns promptly AND the child's tree is dead: the script
    /// would write `finished` after its sleep, and that file never appears.
    #[test]
    fn spawn_timeout_kills_the_child_tree() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("finished");
        let m = marker.to_string_lossy().to_string();
        let prog = fake_claude(
            dir.path(),
            &format!("ping -n 5 127.0.0.1 >nul\necho done> \"{m}\""),
            &format!("sleep 4\necho done > '{m}'"),
        );
        let started = Instant::now();
        let r = run_structured_oneshot_with(
            &prog,
            None,
            "p",
            Some(dir.path()),
            OneshotTools::None,
            1,
            Duration::from_secs(1),
        );
        let elapsed = started.elapsed();
        assert!(
            matches!(&r, Err(e) if e.is_timeout()),
            "expected a timeout, got {r:?}"
        );
        assert!(
            elapsed < Duration::from_secs(4),
            "deadline not honoured: {elapsed:?}"
        );
        std::thread::sleep(Duration::from_secs(6));
        assert!(!marker.exists(), "the killed child's tree kept running");
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
                OneshotTools::None,
                1,
                Duration::from_secs(30),
            ) {
                Err(OneshotError::Unparseable(_)) => {}
                other => panic!("expected Unparseable for {unix:?}, got {other:?}"),
            }
        }
    }

    /// The permission flags reach the child intact through the real builder —
    /// on Windows through `cmd /c`: the empty `--tools ""` and the
    /// space-and-paren-bearing `Bash(git log:*)` arrive as their own quoted
    /// tokens on the child's command line (which is what the shim forwards to
    /// the CLI's argv parser).
    #[test]
    fn spawn_permission_flags_reach_the_child_intact() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let prog = fake_claude(
            dir.path(),
            "echo ARGV: %*",
            "printf 'ARGV:'\nfor a in \"$@\"; do printf '[%s]' \"$a\"; done\necho",
        );
        let none = build_print_command(&prog, None, &oneshot_cli_args(OneshotTools::None, 1));
        let out_none = run_print_child(none, "", Duration::from_secs(30)).unwrap();
        let git = build_print_command(&prog, None, &oneshot_cli_args(OneshotTools::ReadonlyGit, 2));
        let out_git = run_print_child(git, "", Duration::from_secs(30)).unwrap();
        if cfg!(windows) {
            assert!(out_none.ends_with("--tools \"\""), "{out_none}");
            assert!(out_git.contains(" \"Bash(git log:*)\" "), "{out_git}");
            assert!(out_git.contains(" \"Bash(git log *--ou*)\" "), "{out_git}");
        } else {
            assert!(out_none.ends_with("[--tools][]"), "{out_none}");
            assert!(out_git.contains("[Bash(git log:*)]"), "{out_git}");
            assert!(out_git.contains("[Bash(git log *--ou*)]"), "{out_git}");
        }
    }
}
