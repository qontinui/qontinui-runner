//! Shell Command Step Handler
//!
//! Handles shell command execution steps with variable expansion,
//! PowerShell detection, and timeout support.

use async_trait::async_trait;
use serde_json::json;
use std::collections::HashMap;
use std::process::Stdio;
use tokio::time::{timeout, Duration};
use tracing::{info, warn};

use super::{HandlerContext, StepHandler, StepHandlerResult};
use crate::orchestrator::context_propagation::ExpressionEvaluator;
use crate::step_executor::events::TreeEventEmitter;
use crate::step_executor::ExecutionStepConfig;
use crate::str_utils::truncate_str;

/// Handler for shell command execution steps.
pub struct ShellCommandHandler;

#[async_trait]
impl StepHandler for ShellCommandHandler {
    fn step_type(&self) -> &'static str {
        "shell_command"
    }

    fn display_name(&self) -> &'static str {
        "Shell Command"
    }

    async fn execute(
        &self,
        step: &ExecutionStepConfig,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        // Get the command template
        let template_command = match &step.shell_command {
            Some(cmd) => cmd.clone(),
            None => {
                if step.shell_command_id.is_some() {
                    return StepHandlerResult::failure(
                        "Shell command lookup by ID not yet implemented in step executor",
                    );
                }
                return StepHandlerResult::failure("No shell command specified");
            }
        };

        // Expand variables in the command using runtime context
        let evaluator = ExpressionEvaluator::new();
        let has_variables = evaluator.has_expressions(&template_command);
        let command = evaluator.evaluate(&template_command, context.runtime_context());

        // Track which variables were resolved (for logging)
        let resolved_variables: Option<HashMap<String, String>> = if has_variables {
            let expressions = evaluator.find_expressions(&template_command);
            let mut vars = HashMap::new();
            for expr in expressions {
                let resolved =
                    evaluator.evaluate(&format!("{{{{{}}}}}", expr), context.runtime_context());
                if !resolved.contains("{{") {
                    vars.insert(expr, resolved);
                }
            }
            if vars.is_empty() {
                None
            } else {
                Some(vars)
            }
        } else {
            None
        };

        // Log variable expansion if applicable
        if has_variables {
            info!(
                "Shell command variables expanded: template='{}' -> resolved='{}'",
                template_command, command
            );
            if let Some(ref vars) = resolved_variables {
                info!("Resolved variables: {:?}", vars);
            }
        }

        // Runtime sanitization: replace jq with python since jq is unavailable on Windows MSYS
        let command = if command.contains("| jq ") {
            let sanitized = Self::replace_jq_with_python(&command);
            if sanitized != command {
                info!(
                    "Runtime jq→python replacement applied: {}",
                    &sanitized[..sanitized.len().min(100)]
                );
            }
            sanitized
        } else {
            command
        };

        // Runtime sanitization: on Windows, Python outputs \r\n line endings.
        // When piped to xargs, the \r gets embedded in the substituted value,
        // breaking URLs and other string interpolations. Insert `tr -d '\r'`
        // before xargs to strip carriage returns.
        let command = if cfg!(target_os = "windows") && command.contains("| xargs") {
            let sanitized = Self::fix_crlf_before_xargs(&command);
            if sanitized != command {
                info!(
                    "Runtime \\r fix applied before xargs: {}",
                    &sanitized[..sanitized.len().min(100)]
                );
            }
            sanitized
        } else {
            command
        };

        let step_name = step.name.as_deref().unwrap_or("Shell Command");
        let working_directory = step.shell_command_working_directory.clone();
        let fail_on_error = step.shell_command_fail_on_error.unwrap_or(true);
        let timeout_secs = step.timeout_seconds;

        // Detect shell type: PowerShell > bash > cmd (on Windows)
        let is_powershell = Self::is_powershell_command(&command);
        let is_bash = !is_powershell && Self::is_bash_command(&command);
        let shell_type = Self::get_shell_type(is_powershell, is_bash);

        let timeout_str = timeout_secs
            .map(|t| format!("{}s", t))
            .unwrap_or_else(|| "disabled".to_string());
        info!(
            "Executing shell command '{}': {} (shell: {}, timeout: {}, working_dir: {:?})",
            step_name, command, shell_type, timeout_str, working_directory
        );

        // Generate action ID and emit start event
        let sequence = TreeEventEmitter::next_sequence();
        let action_id = TreeEventEmitter::generate_action_id("shell-command", sequence);

        // Truncate command for display
        let command_display = if command.len() > 50 {
            format!("{}...", truncate_str(&command, 50))
        } else {
            command.clone()
        };

        let metadata = json!({
            "command": &command_display,
            "shell_type": shell_type,
            "working_directory": working_directory.as_deref().unwrap_or(""),
            "timeout_seconds": timeout_secs,
        });

        let (start_timestamp, sequence) = context
            .event_emitter
            .emit_action_started(&action_id, &format!("SHELL: {}", step_name), metadata)
            .await;

        // Execute the command
        let start = std::time::Instant::now();
        let (success, exit_code, stdout, stderr) = Self::run_command(
            &command,
            is_powershell,
            is_bash,
            working_directory.as_deref(),
            timeout_secs,
        )
        .await;
        let duration_ms = start.elapsed().as_millis() as u64;

        info!(
            "Shell command '{}' completed: success={}, exit_code={:?}, duration={}ms",
            step_name, success, exit_code, duration_ms
        );

        // Log output if present
        if !stdout.is_empty() {
            info!("Shell command stdout:\n{}", stdout.trim());
        }
        if !stderr.is_empty() {
            if success {
                info!("Shell command stderr:\n{}", stderr.trim());
            } else {
                warn!("Shell command stderr:\n{}", stderr.trim());
            }
        }

        // Truncate stdout/stderr for display
        let stdout_display = Self::truncate_for_display(&stdout, 200);
        let stderr_display = Self::truncate_for_display(&stderr, 200);

        // Determine overall success based on fail_on_error setting
        let (final_success, error_msg, output_data) = if success {
            let output_data = if stdout.is_empty() {
                None
            } else {
                Some(json!(stdout))
            };
            (true, None, output_data)
        } else if fail_on_error {
            let error_msg = if !stderr.is_empty() {
                format!(
                    "Command failed (exit code {:?}): {}",
                    exit_code,
                    stderr.trim()
                )
            } else {
                format!("Command failed with exit code {:?}", exit_code)
            };
            (false, Some(error_msg), None)
        } else {
            info!(
                "Shell command '{}' failed but fail_on_error=false, continuing",
                step_name
            );
            let error_msg = if !stderr.is_empty() {
                format!("(ignored) Command failed: {}", stderr.trim())
            } else {
                format!("(ignored) Command failed with exit code {:?}", exit_code)
            };
            (true, Some(error_msg), Some(json!(stdout)))
        };

        // Build result metadata
        let result_metadata = json!({
            "command": &command_display,
            "shell_type": shell_type,
            "working_directory": working_directory.as_deref().unwrap_or(""),
            "exit_code": exit_code,
            "stdout": &stdout_display,
            "stderr": &stderr_display,
            "duration_ms": duration_ms,
        });

        // Emit completion event
        if final_success {
            context
                .event_emitter
                .emit_action_completed(
                    &action_id,
                    &format!("SHELL: {}", step_name),
                    start_timestamp,
                    sequence,
                    result_metadata,
                )
                .await;

            let mut result = StepHandlerResult::success();
            if let Some(data) = output_data {
                result = result.with_data(data);
            }
            result
        } else {
            context
                .event_emitter
                .emit_action_failed(
                    &action_id,
                    &format!("SHELL: {}", step_name),
                    start_timestamp,
                    sequence,
                    error_msg.as_deref().unwrap_or("Unknown error"),
                    Some(result_metadata),
                )
                .await;

            StepHandlerResult::failure(error_msg.unwrap_or_else(|| "Command failed".to_string()))
        }
    }
}

impl ShellCommandHandler {
    /// Extract Unix-style KEY=VALUE env prefixes from a command string.
    /// cmd.exe doesn't support inline env vars like `SKIP_WEB_SERVER=1 npx ...`,
    /// so we parse them out and pass via Command::env() instead.
    fn extract_env_prefix_for_cmd(command: &str) -> (Vec<(String, String)>, String) {
        let mut envs = Vec::new();
        let mut remaining = command.trim();
        while let Some(eq_pos) = remaining.find('=') {
            let prefix = &remaining[..eq_pos];
            if prefix.is_empty()
                || prefix.contains(' ')
                || !prefix.chars().all(|c| c.is_alphanumeric() || c == '_')
            {
                break;
            }
            let after_eq = &remaining[eq_pos + 1..];
            let value_end = after_eq.find(' ').unwrap_or(after_eq.len());
            let value = &after_eq[..value_end];
            envs.push((prefix.to_string(), value.to_string()));
            remaining = after_eq[value_end..].trim_start();
        }
        (envs, remaining.to_string())
    }

    /// Find Git for Windows bash.exe, preferring it over WSL's bash.
    ///
    /// Checks common Git install locations. Returns None if not found,
    /// in which case the caller falls back to bare "bash" (PATH lookup).
    pub(crate) fn find_git_bash() -> Option<String> {
        let candidates = [
            r"C:\Program Files\Git\usr\bin\bash.exe",
            r"C:\Program Files\Git\bin\bash.exe",
            r"C:\Program Files (x86)\Git\usr\bin\bash.exe",
            r"C:\Program Files (x86)\Git\bin\bash.exe",
        ];
        for path in &candidates {
            if std::path::Path::new(path).exists() {
                return Some(path.to_string());
            }
        }
        // Try to find via `where.exe git` and derive bash path
        if let Ok(output) = crate::process_helpers::no_window("where.exe")
            .arg("git")
            .output()
        {
            if let Ok(stdout) = String::from_utf8(output.stdout) {
                if let Some(git_path) = stdout.lines().next() {
                    // git.exe is typically at Git/cmd/git.exe; bash is at Git/usr/bin/bash.exe
                    let git_dir = std::path::Path::new(git_path)
                        .parent()
                        .and_then(|p| p.parent());
                    if let Some(dir) = git_dir {
                        let bash = dir.join("usr").join("bin").join("bash.exe");
                        if bash.exists() {
                            return Some(bash.to_string_lossy().to_string());
                        }
                    }
                }
            }
        }
        None
    }

    /// On Windows, insert `tr -d '\r'` before `| xargs` to strip carriage returns
    /// that Python and other tools emit with \r\n line endings.
    fn fix_crlf_before_xargs(command: &str) -> String {
        command.replace("| xargs", "| tr -d '\\r' | xargs")
    }

    /// Check if a command uses PowerShell syntax.
    fn is_powershell_command(command: &str) -> bool {
        command.contains("Get-")
            || command.contains("Set-")
            || command.contains("New-")
            || command.contains("Remove-")
            || command.contains("Invoke-")
            || command.contains("ForEach-Object")
            || command.contains("Where-Object")
            || command.contains("Select-Object")
            || command.contains("$_")
            || command.contains("$env:")
            || command.contains("-ErrorAction")
            || command.contains("| %")
            || command.contains("| ?")
    }

    /// Check if a command uses bash/Unix syntax that cmd.exe cannot handle.
    pub(crate) fn is_bash_command(command: &str) -> bool {
        let trimmed = command.trim();
        let first_word = trimmed.split_whitespace().next().unwrap_or("");

        // Bash builtins and common Unix commands not available in cmd.exe
        let bash_commands = [
            "test", "export", "source", "[", "[[", "unset", "eval", "which", "xargs", "wc", "tr",
            "tee", "sort", "head", "tail", "grep", "sed", "awk", "cut", "basename", "dirname",
            "curl", "cat", "rm", "ls", "cp", "mv", "touch", "mkdir", "echo", "printf", "find",
            "diff", "chmod", "chown", "ln", "readlink", "stat", "file", "tar", "gzip", "gunzip",
        ];
        if bash_commands.contains(&first_word) {
            return true;
        }

        // For piped commands, also check subsequent pipe segments.
        // e.g. `cat foo | grep bar` — if any segment starts with a bash command, use bash.
        if trimmed.contains(" | ") {
            for segment in trimmed.split(" | ").skip(1) {
                let seg_first = segment.split_whitespace().next().unwrap_or("");
                if bash_commands.contains(&seg_first) {
                    return true;
                }
            }
        }

        // Bash negation prefix: `! command` inverts the exit code.
        // This is exclusively bash syntax — cmd.exe cannot handle it.
        if first_word == "!" {
            return true;
        }

        // Bash syntax patterns
        trimmed.contains("$(")      // command substitution
            || trimmed.contains("${")  // variable expansion
            || trimmed.contains(">/dev/null")  // Unix null device
            || trimmed.contains("<<EOF")  // heredoc
            || trimmed.contains("<<'EOF'")  // heredoc
            || trimmed.contains("python -c")  // Python one-liners need bash for proper quoting
            || trimmed.contains("python3 -c")  // Python3 one-liners need bash for proper quoting
            || trimmed.contains("node -e")  // Node.js eval needs bash for proper quoting
            || trimmed.contains("node --eval")  // Node.js eval (long form)
            || (trimmed.starts_with("if ") && trimmed.contains("; then"))  // bash if
            || (trimmed.starts_with("for ") && trimmed.contains("; do")) // bash for
    }

    /// Get the shell type string for logging.
    fn get_shell_type(is_powershell: bool, is_bash: bool) -> &'static str {
        if cfg!(target_os = "windows") && is_powershell {
            "powershell"
        } else if cfg!(target_os = "windows") && is_bash {
            "bash"
        } else if cfg!(target_os = "windows") {
            "cmd"
        } else {
            "sh"
        }
    }

    /// Replace `jq` commands with Python equivalents at runtime.
    /// jq is not available on Windows MSYS/Git Bash, so we convert common patterns.
    /// Note: UI Bridge SDK `/elements` returns `{"data": [...]}` not `{"elements": [...], "total": N}`.
    pub fn replace_jq_with_python_static(cmd: &str) -> String {
        Self::replace_jq_with_python(cmd)
    }

    fn replace_jq_with_python(cmd: &str) -> String {
        if let Some(pipe_idx) = cmd.find("| jq ") {
            let curl_part = cmd[..pipe_idx].trim();
            let jq_part = &cmd[pipe_idx + 5..]; // skip "| jq "

            let jq_expr = jq_part
                .trim()
                .trim_start_matches("-e ")
                .trim()
                .trim_matches('"')
                .trim_matches('\'');

            let is_sdk_elements = curl_part.contains("ui-bridge/sdk/elements");

            // Pattern: .data | length > N
            if jq_expr.contains("data") && jq_expr.contains("length") {
                let threshold = Self::extract_threshold(jq_expr);
                return format!(
                    "{} | python -c \"import sys,json; d=json.load(sys.stdin); items=d.get('data',[]); assert len(items)>{}, 'Expected >{} items, got '+str(len(items))\"",
                    curl_part, threshold, threshold
                );
            }

            // Pattern: .elements | length > N
            if jq_expr.contains("elements") && jq_expr.contains("length") {
                let threshold = Self::extract_threshold(jq_expr);
                // SDK /elements returns {data: [...]} not {elements: [...]}
                let key = if is_sdk_elements { "data" } else { "elements" };
                return format!(
                    "{} | python -c \"import sys,json; d=json.load(sys.stdin); elems=d.get('{}',d.get('data',[])); assert len(elems)>{}, 'Expected >{} elements, got '+str(len(elems))\"",
                    curl_part, key, threshold, threshold
                );
            }

            // Pattern: .total > N
            if jq_expr.contains("total") {
                let threshold = Self::extract_threshold(jq_expr);
                if is_sdk_elements {
                    // SDK /elements has no total field — use len(data) instead
                    return format!(
                        "{} | python -c \"import sys,json; d=json.load(sys.stdin); items=d.get('data',[]); assert len(items)>{}, 'Expected >{} elements, got '+str(len(items))\"",
                        curl_part, threshold, threshold
                    );
                }
                return format!(
                    "{} | python -c \"import sys,json; d=json.load(sys.stdin); t=d.get('total',len(d.get('data',[]))); assert t>{}, 'Expected total>{}, got '+str(t)\"",
                    curl_part, threshold, threshold
                );
            }

            // Pattern: .results | length > N
            if jq_expr.contains("results") && jq_expr.contains("length") {
                let threshold = Self::extract_threshold(jq_expr);
                return format!(
                    "{} | python -c \"import sys,json; d=json.load(sys.stdin); r=d.get('results',[]); assert len(r)>{}, 'Expected >{} results, got '+str(len(r))\"",
                    curl_part, threshold, threshold
                );
            }
        }
        cmd.to_string()
    }

    /// Extract a numeric threshold from an expression like "> 0" or "length > 2"
    fn extract_threshold(s: &str) -> u32 {
        for (i, b) in s.bytes().enumerate() {
            if b == b'>' && i + 1 < s.len() {
                let rest = s[i + 1..].trim_start();
                if let Some(end) = rest.find(|c: char| !c.is_ascii_digit()) {
                    if let Ok(n) = rest[..end].parse() {
                        return n;
                    }
                } else if let Ok(n) = rest.parse() {
                    return n;
                }
            }
        }
        0
    }

    /// Truncate a string for display purposes.
    fn truncate_for_display(s: &str, max_len: usize) -> String {
        if s.len() > max_len {
            format!("{}...", &s[..max_len])
        } else {
            s.to_string()
        }
    }

    /// Run a shell command with optional timeout.
    async fn run_command(
        command: &str,
        is_powershell: bool,
        is_bash: bool,
        working_directory: Option<&str>,
        timeout_secs: Option<u64>,
    ) -> (bool, Option<i32>, String, String) {
        // Build the command
        let mut cmd = if cfg!(target_os = "windows") {
            if is_powershell {
                let mut c = crate::process_helpers::tokio_no_window("powershell");
                c.args(["-NoProfile", "-NonInteractive", "-Command", command]);
                c
            } else if is_bash {
                // Use Git Bash for Unix-style commands on Windows.
                // Resolve the full path to avoid accidentally picking up WSL's
                // bash.exe (C:\Windows\System32\bash.exe) which can fail with
                // "execvpe(/bin/bash) failed" when WSL has no distro installed.
                let bash_path = Self::find_git_bash().unwrap_or_else(|| "bash".to_string());
                let mut c = crate::process_helpers::tokio_no_window(&bash_path);
                // Ensure MSYS2 /usr/bin is on PATH so tools like cat, grep, sed
                // are available even when bash is invoked non-interactively.
                if let Some(usr_bin) = std::path::Path::new(&bash_path).parent()
                // e.g. C:\Program Files\Git\usr\bin
                {
                    let usr_bin_str = usr_bin.to_string_lossy();
                    let current_path = std::env::var("PATH").unwrap_or_default();
                    if !current_path.contains(&*usr_bin_str) {
                        c.env("PATH", format!("{};{}", usr_bin_str, current_path));
                    }
                }
                c.args(["-c", command]);
                c
            } else {
                // cmd.exe doesn't understand single quotes as string delimiters —
                // strip them so tools receive unquoted paths correctly.
                let stripped = command.replace('\'', "");
                // Extract Unix-style KEY=VALUE env prefixes that cmd.exe can't handle
                let (extra_envs, actual_cmd) = Self::extract_env_prefix_for_cmd(&stripped);
                let mut c = crate::process_helpers::tokio_cmd_no_window();
                // Use raw_arg to avoid Rust's automatic MSVC-style escaping of double
                // quotes (\" instead of ""). cmd.exe doesn't understand \" and passes
                // the literal backslash-quote to child processes, breaking quoted paths.
                c.arg("/C");
                #[cfg(windows)]
                c.raw_arg(&actual_cmd);
                #[cfg(not(windows))]
                c.arg(&actual_cmd);
                for (key, value) in extra_envs {
                    c.env(key, value);
                }
                c
            }
        } else {
            let mut c = crate::process_helpers::tokio_no_window("sh");
            c.args(["-c", command]);
            c
        };

        // Set working directory if specified (resolve relative paths against workspace root)
        if let Some(wd) = working_directory {
            let resolved = crate::paths::resolve_working_directory(wd);
            if !resolved.exists() {
                warn!(
                    "Shell command: working directory does not exist: {} (resolved from '{}')",
                    resolved.display(),
                    wd
                );
            }
            cmd.current_dir(&resolved);
        }

        // Inject trace ID for cross-process correlation
        cmd.env("QONTINUI_TRACE_ID", uuid::Uuid::new_v4().to_string());

        // Capture stdout and stderr
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        // Execute with or without timeout
        if let Some(timeout_secs_val) = timeout_secs {
            let timeout_duration = Duration::from_secs(timeout_secs_val);
            match timeout(timeout_duration, cmd.output()).await {
                Ok(Ok(output)) => {
                    let exit_code = output.status.code();
                    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                    let success = output.status.success();
                    (success, exit_code, stdout, stderr)
                }
                Ok(Err(e)) => (
                    false,
                    None,
                    String::new(),
                    format!("Failed to execute command: {}", e),
                ),
                Err(_) => (
                    false,
                    None,
                    String::new(),
                    format!("Command timed out after {} seconds", timeout_secs_val),
                ),
            }
        } else {
            match cmd.output().await {
                Ok(output) => {
                    let exit_code = output.status.code();
                    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                    let success = output.status.success();
                    (success, exit_code, stdout, stderr)
                }
                Err(e) => (
                    false,
                    None,
                    String::new(),
                    format!("Failed to execute command: {}", e),
                ),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shell_command_handler_step_type() {
        let handler = ShellCommandHandler;
        assert_eq!(handler.step_type(), "shell_command");
        assert_eq!(handler.display_name(), "Shell Command");
    }

    #[test]
    fn test_is_powershell_command() {
        assert!(ShellCommandHandler::is_powershell_command("Get-Process"));
        assert!(ShellCommandHandler::is_powershell_command("Set-Location"));
        assert!(ShellCommandHandler::is_powershell_command("$env:PATH"));
        assert!(ShellCommandHandler::is_powershell_command(
            "gci | Where-Object { $_.Name }"
        ));
        assert!(!ShellCommandHandler::is_powershell_command("echo hello"));
        assert!(!ShellCommandHandler::is_powershell_command("ls -la"));
    }

    #[test]
    fn test_is_bash_command() {
        // Unix builtins
        assert!(ShellCommandHandler::is_bash_command("test -z \"output\""));
        assert!(ShellCommandHandler::is_bash_command("export FOO=bar"));
        assert!(ShellCommandHandler::is_bash_command("grep pattern file"));
        assert!(ShellCommandHandler::is_bash_command("[ -f file.txt ]"));
        assert!(ShellCommandHandler::is_bash_command(
            "curl -sf -X GET 'http://localhost:9876/reflection-fixes?workflow_name=test&status=applied'"
        ));

        // Bash negation prefix
        assert!(ShellCommandHandler::is_bash_command(
            "! grep -qE 'pattern' file.txt"
        ));
        assert!(ShellCommandHandler::is_bash_command("! test -f /tmp/file"));

        // Bash syntax patterns
        assert!(ShellCommandHandler::is_bash_command("echo $(git status)"));
        assert!(ShellCommandHandler::is_bash_command("echo ${HOME}"));
        assert!(ShellCommandHandler::is_bash_command("cmd >/dev/null 2>&1"));
        assert!(ShellCommandHandler::is_bash_command(
            "if test -z \"out\"; then echo ok; fi"
        ));

        // Common Unix file commands
        assert!(ShellCommandHandler::is_bash_command("cat /tmp/file.txt"));
        assert!(ShellCommandHandler::is_bash_command("rm -rf /tmp/test"));
        assert!(ShellCommandHandler::is_bash_command("ls -la /home"));
        assert!(ShellCommandHandler::is_bash_command("cp src dst"));
        assert!(ShellCommandHandler::is_bash_command("mv old new"));
        assert!(ShellCommandHandler::is_bash_command("touch /tmp/marker"));
        assert!(ShellCommandHandler::is_bash_command("mkdir -p /tmp/dir"));
        assert!(ShellCommandHandler::is_bash_command("find . -name '*.rs'"));

        // Piped commands where first word isn't in list but later segments are
        assert!(ShellCommandHandler::is_bash_command(
            "cat /tmp/file.txt | grep -q 'SMOKE_TEST_PASSED'"
        ));
        assert!(ShellCommandHandler::is_bash_command(
            "git log --oneline | head -5"
        ));
        assert!(ShellCommandHandler::is_bash_command(
            "cargo test 2>&1 | grep FAILED | wc -l"
        ));

        // Not bash - regular commands
        assert!(!ShellCommandHandler::is_bash_command("git status"));
        assert!(!ShellCommandHandler::is_bash_command("npm test"));
        assert!(!ShellCommandHandler::is_bash_command("cargo build"));

        // 2>&1 is valid in PowerShell and cmd.exe too, so it should NOT trigger bash detection
        assert!(!ShellCommandHandler::is_bash_command("cmd 2>&1"));
        assert!(!ShellCommandHandler::is_bash_command(
            "some-tool --flag 2>&1"
        ));
    }

    #[test]
    fn test_get_shell_type() {
        assert_eq!(
            ShellCommandHandler::get_shell_type(true, false),
            if cfg!(target_os = "windows") {
                "powershell"
            } else {
                "sh"
            }
        );
        assert_eq!(
            ShellCommandHandler::get_shell_type(false, true),
            if cfg!(target_os = "windows") {
                "bash"
            } else {
                "sh"
            }
        );
        assert_eq!(
            ShellCommandHandler::get_shell_type(false, false),
            if cfg!(target_os = "windows") {
                "cmd"
            } else {
                "sh"
            }
        );
    }

    #[test]
    fn test_truncate_for_display() {
        assert_eq!(
            ShellCommandHandler::truncate_for_display("short", 10),
            "short"
        );
        assert_eq!(
            ShellCommandHandler::truncate_for_display("this is a long string", 10),
            "this is a ..."
        );
    }

    #[test]
    fn test_replace_jq_with_python_total() {
        let cmd = r#"curl -sf http://localhost:9876/ui-bridge/sdk/elements | jq -e ".total > 0""#;
        let result = ShellCommandHandler::replace_jq_with_python(cmd);
        assert!(!result.contains("jq"), "Should not contain jq: {}", result);
        assert!(
            result.contains("python -c"),
            "Should contain python: {}",
            result
        );
        // SDK /elements has no total field — should use len(data) instead
        assert!(
            result.contains("data"),
            "Should use 'data' key for SDK elements: {}",
            result
        );
    }

    #[test]
    fn test_replace_jq_with_python_elements() {
        let cmd = r#"curl -sf http://localhost:9876/ui-bridge/sdk/elements | jq -e '.elements | length > 2'"#;
        let result = ShellCommandHandler::replace_jq_with_python(cmd);
        assert!(!result.contains("jq"), "Should not contain jq: {}", result);
        assert!(
            result.contains("python -c"),
            "Should contain python: {}",
            result
        );
        assert!(
            result.contains("elements"),
            "Should check elements: {}",
            result
        );
    }

    #[test]
    fn test_replace_jq_noop_for_non_jq() {
        let cmd = "curl -sf http://localhost:9876/ui-bridge/sdk/snapshot | grep elements";
        let result = ShellCommandHandler::replace_jq_with_python(cmd);
        assert_eq!(result, cmd, "Non-jq commands should be unchanged");
    }

    #[test]
    fn test_extract_threshold() {
        assert_eq!(ShellCommandHandler::extract_threshold(".total > 0"), 0);
        assert_eq!(
            ShellCommandHandler::extract_threshold(".elements | length > 2"),
            2
        );
        assert_eq!(ShellCommandHandler::extract_threshold("length > 10"), 10);
    }
}
