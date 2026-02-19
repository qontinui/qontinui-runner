//! Check Step Handler
//!
//! Handles check steps that run code quality checks (lint, format, typecheck, etc.)
//! with automatic language detection and tool selection.

use async_trait::async_trait;
use serde_json::json;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;
use tracing::{info, warn};

use super::{HandlerContext, StepHandler, StepHandlerResult};
use crate::step_executor::executor::ExecutionStepConfig;

/// Handler for check steps.
pub struct CheckHandler;

#[async_trait]
impl StepHandler for CheckHandler {
    fn step_type(&self) -> &'static str {
        "check"
    }

    fn display_name(&self) -> &'static str {
        "Check"
    }

    async fn execute(
        &self,
        step: &ExecutionStepConfig,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        let check_type = step.check_type.as_deref().unwrap_or("custom_command");
        let step_name = step.name.as_deref().unwrap_or("Check");
        let timeout_secs = step.timeout_seconds;

        // Note: Due to serde alias conflict, "working_directory" goes to shell_command_working_directory
        // So we check both fields for backwards compatibility
        let working_directory = step
            .check_working_directory
            .clone()
            .or_else(|| step.shell_command_working_directory.clone());

        info!(
            "execute_check_step: step_name={:?}, check_type={:?}, check_command={:?}, working_dir={:?}",
            step.name, step.check_type, step.check_command, step.check_working_directory
        );

        // Handle http_status check type separately
        if check_type == "http_status" {
            return Self::execute_http_status_check(step, step_name, timeout_secs, context).await;
        }

        // Handle ai_review check type - AI-based semantic review
        if check_type == "ai_review" {
            return Self::execute_ai_review_check(step, step_name, timeout_secs, context).await;
        }

        // Handle ci_cd check type - GitHub CI/CD pipeline status
        if check_type == "ci_cd" {
            return Self::execute_ci_cd_check(step, step_name, timeout_secs, context).await;
        }

        // Detect project type from working directory
        let detected_language = Self::detect_language(working_directory.as_deref().unwrap_or("."));

        info!(
            "Check step '{}': detected language = {}",
            step_name, detected_language
        );

        // Get the command to run
        let explicit_command = step
            .check_command
            .as_ref()
            .filter(|s| !s.is_empty())
            .or_else(|| step.shell_command.as_ref().filter(|s| !s.is_empty()));

        let command = match explicit_command {
            Some(cmd) => Some(cmd.clone()),
            None => Self::get_auto_command(check_type, &detected_language, step_name),
        };

        // Handle the case where no command could be determined
        let command = match command {
            Some(cmd) => cmd,
            None => {
                info!(
                    "Check step '{}' skipped: no applicable check for type '{}' and language '{}'",
                    step_name, check_type, detected_language
                );
                return StepHandlerResult::success_with_data(json!({
                    "skipped": true,
                    "reason": format!(
                        "No {} check available for {} projects. Specify a command explicitly if needed.",
                        check_type, detected_language
                    )
                }));
            }
        };

        let auto_fix = step.check_auto_fix.unwrap_or(false);
        let final_command = if auto_fix {
            Self::apply_auto_fix(&command, check_type, &detected_language)
        } else {
            command.clone()
        };

        let timeout_str = timeout_secs
            .map(|t| format!("{}s", t))
            .unwrap_or_else(|| "disabled".to_string());
        info!(
            "Executing check '{}' ({}): {} (timeout: {}, working_dir: {:?})",
            step_name, check_type, final_command, timeout_str, working_directory
        );

        // Generate sequence number and timestamp for tree events
        use std::sync::atomic::{AtomicU32, Ordering};
        static CHECK_SEQUENCE: AtomicU32 = AtomicU32::new(1);
        let sequence = CHECK_SEQUENCE.fetch_add(1, Ordering::SeqCst);
        let _timestamp = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
        let action_id = format!("check-{}", sequence);

        // Truncate command for display
        let command_display = if final_command.len() > 50 {
            format!("{}...", &final_command[..50])
        } else {
            final_command.clone()
        };

        // Emit action_started tree event
        let action_name = format!("CHECK: {}", step_name);
        let metadata = json!({
            "check_type": check_type,
            "command": &command_display,
            "working_directory": working_directory.as_deref().unwrap_or(""),
            "auto_fix": auto_fix,
            "timeout_seconds": timeout_secs,
        });
        let (timestamp, sequence) =
            Self::emit_started(context, &action_id, &action_name, metadata).await;

        // Build the command
        // On Windows, detect PowerShell syntax and run directly via PowerShell
        // This avoids cmd.exe quote escaping issues that cause "string is missing terminator" errors
        let is_powershell = Self::is_powershell_syntax(&final_command);
        let mut cmd = if cfg!(target_os = "windows") {
            if is_powershell {
                // PowerShell syntax detected - run directly via PowerShell
                let mut c = Command::new("powershell");
                c.args(["-NoProfile", "-NonInteractive", "-Command", &final_command]);
                c
            } else {
                // cmd.exe doesn't understand single quotes as string delimiters —
                // strip them so tools receive unquoted paths (e.g., Next.js route groups
                // like src/app/(app)/... work correctly).
                let stripped = final_command.replace('\'', "");
                // Extract Unix-style KEY=VALUE env prefixes that cmd.exe can't handle
                let (extra_envs, actual_cmd) = Self::extract_env_prefix_for_cmd(&stripped);
                let mut c = Command::new("cmd");
                c.args(["/C", &actual_cmd]);
                for (key, value) in extra_envs {
                    c.env(key, value);
                }
                c
            }
        } else {
            let mut c = Command::new("sh");
            c.args(["-c", &final_command]);
            c
        };

        // Set working directory if specified
        if let Some(ref wd) = working_directory {
            cmd.current_dir(wd);
        }

        // Capture stdout and stderr
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let start = std::time::Instant::now();

        // Execute with or without timeout
        let (final_success, error_msg, output_data) = if let Some(timeout_secs_val) = timeout_secs {
            let timeout_duration = Duration::from_secs(timeout_secs_val);
            let output_result = timeout(timeout_duration, cmd.output()).await;
            let duration_ms = start.elapsed().as_millis() as u64;

            match output_result {
                Ok(Ok(output)) => Self::process_output(output, step_name, duration_ms),
                Ok(Err(e)) => {
                    warn!("Failed to execute check '{}': {}", step_name, e);
                    (false, Some(format!("Failed to execute check: {}", e)), None)
                }
                Err(_) => {
                    warn!(
                        "Check '{}' timed out after {}s",
                        step_name, timeout_secs_val
                    );
                    (
                        false,
                        Some(format!(
                            "Check timed out after {} seconds",
                            timeout_secs_val
                        )),
                        None,
                    )
                }
            }
        } else {
            let duration_ms = start.elapsed().as_millis() as u64;
            match cmd.output().await {
                Ok(output) => Self::process_output(output, step_name, duration_ms),
                Err(e) => {
                    warn!("Failed to execute check '{}': {}", step_name, e);
                    (false, Some(format!("Failed to execute check: {}", e)), None)
                }
            }
        };

        // Emit completion event
        let total_duration_ms = start.elapsed().as_millis() as u64;

        let result_metadata = json!({
            "check_type": check_type,
            "command": &command_display,
            "working_directory": working_directory.as_deref().unwrap_or(""),
            "duration_ms": total_duration_ms,
        });

        if final_success {
            Self::emit_completed(
                context,
                &action_id,
                &action_name,
                timestamp,
                sequence,
                result_metadata,
            )
            .await;
        } else {
            Self::emit_failed(
                context,
                &action_id,
                &action_name,
                timestamp,
                sequence,
                error_msg.as_deref().unwrap_or("Check failed"),
                Some(result_metadata),
            )
            .await;
        }

        if final_success {
            match output_data {
                Some(data) => StepHandlerResult::success_with_data(json!({ "output": data })),
                None => StepHandlerResult::success(),
            }
        } else {
            match output_data {
                Some(data) => StepHandlerResult::failure_with_data(
                    error_msg.unwrap_or_else(|| "Check failed".to_string()),
                    json!({ "output": data }),
                ),
                None => StepHandlerResult::failure(
                    error_msg.unwrap_or_else(|| "Check failed".to_string()),
                ),
            }
        }
    }
}

impl CheckHandler {
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

    /// Check if a command uses PowerShell syntax.
    /// This mirrors the detection logic from shell_command.rs
    fn is_powershell_syntax(command: &str) -> bool {
        command.contains("Get-")
            || command.contains("Set-")
            || command.contains("New-")
            || command.contains("Remove-")
            || command.contains("Invoke-")
            || command.contains("Write-Host")
            || command.contains("Write-Output")
            || command.contains("$env:")
            || command.contains("$null")
            || command.contains("| %")
            || command.contains("| ?")
            || command.starts_with("$")
    }

    /// Detect project language from marker files
    fn detect_language(work_dir: &str) -> String {
        let path = std::path::Path::new(work_dir);

        if path.join("Cargo.toml").exists() {
            return "rust".to_string();
        }
        if path.join("pyproject.toml").exists()
            || path.join("setup.py").exists()
            || path.join("requirements.txt").exists()
        {
            return "python".to_string();
        }
        if path.join("go.mod").exists() {
            return "go".to_string();
        }
        if path.join("tsconfig.json").exists() {
            return "typescript".to_string();
        }
        if path.join("package.json").exists() {
            return "javascript".to_string();
        }
        if path.join("CMakeLists.txt").exists()
            || path.join("Makefile").exists()
            || path.join("configure.ac").exists()
        {
            return "c_cpp".to_string();
        }
        if path.join("build.gradle").exists()
            || path.join("build.gradle.kts").exists()
            || path.join("pom.xml").exists()
        {
            return "java".to_string();
        }
        if path.join("mix.exs").exists() {
            return "elixir".to_string();
        }
        if path.join("Gemfile").exists() {
            return "ruby".to_string();
        }
        if path.join("composer.json").exists() {
            return "php".to_string();
        }
        if path.join("Package.swift").exists() {
            return "swift".to_string();
        }

        // Check for .NET projects
        if let Ok(entries) = std::fs::read_dir(path) {
            let has_dotnet = entries.filter_map(|e| e.ok()).any(|entry| {
                let name = entry.file_name().to_string_lossy().to_string();
                name.ends_with(".csproj") || name.ends_with(".sln") || name.ends_with(".fsproj")
            });
            if has_dotnet {
                return "dotnet".to_string();
            }
        }

        "unknown".to_string()
    }

    /// Get auto-detected command for check type and language
    fn get_auto_command(check_type: &str, language: &str, step_name: &str) -> Option<String> {
        match (check_type, language) {
            // Python
            ("lint", "python") => Some("ruff check .".to_string()),
            ("format", "python") => Some("black --check .".to_string()),
            ("typecheck", "python") => Some("mypy .".to_string()),
            ("analyze", "python") => Some("ruff check . --statistics".to_string()),
            ("security", "python") => Some("pip-audit".to_string()),

            // Rust
            ("lint", "rust") => Some("cargo clippy -- -D warnings".to_string()),
            ("format", "rust") => Some("cargo fmt --check".to_string()),
            ("typecheck", "rust") => Some("cargo check".to_string()),
            ("analyze", "rust") => Some("cargo clippy --all-targets --all-features".to_string()),
            ("security", "rust") => Some("cargo audit".to_string()),

            // Go
            ("lint", "go") => Some("golangci-lint run".to_string()),
            ("format", "go") => Some("gofmt -l .".to_string()),
            ("typecheck", "go") => Some("go vet ./...".to_string()),
            ("analyze", "go") => Some("go vet ./... && staticcheck ./...".to_string()),
            ("security", "go") => Some("gosec ./...".to_string()),

            // TypeScript
            ("lint", "typescript") => Some("npx eslint . --ext .ts,.tsx".to_string()),
            ("format", "typescript") => Some("npx prettier --check .".to_string()),
            ("typecheck", "typescript") => Some("npx tsc --noEmit".to_string()),
            ("analyze", "typescript") => {
                Some("npx eslint . --ext .ts,.tsx --format json".to_string())
            }
            ("security", "typescript") => Some("npm audit".to_string()),

            // JavaScript
            ("lint", "javascript") => Some("npx eslint .".to_string()),
            ("format", "javascript") => Some("npx prettier --check .".to_string()),
            ("typecheck", "javascript") => None,
            ("analyze", "javascript") => Some("npx eslint . --format json".to_string()),
            ("security", "javascript") => Some("npm audit".to_string()),

            // C/C++
            ("lint", "c_cpp") => Some("cppcheck --enable=all .".to_string()),
            ("format", "c_cpp") => {
                Some("clang-format --dry-run -Werror **/*.cpp **/*.c **/*.h".to_string())
            }
            ("typecheck", "c_cpp") => Some("make -n".to_string()),
            ("analyze", "c_cpp") => Some("cppcheck --enable=all --xml .".to_string()),
            ("security", "c_cpp") => Some("flawfinder .".to_string()),

            // Java
            ("lint", "java") => {
                Some("./gradlew checkstyleMain || mvn checkstyle:check".to_string())
            }
            ("format", "java") => Some("./gradlew spotlessCheck || mvn spotless:check".to_string()),
            ("typecheck", "java") => Some("./gradlew compileJava || mvn compile".to_string()),
            ("analyze", "java") => Some("./gradlew pmd || mvn pmd:check".to_string()),
            ("security", "java") => Some(
                "./gradlew dependencyCheckAnalyze || mvn org.owasp:dependency-check-maven:check"
                    .to_string(),
            ),

            // Ruby
            ("lint", "ruby") => Some("bundle exec rubocop".to_string()),
            ("format", "ruby") => Some("bundle exec rubocop --format offenses".to_string()),
            ("typecheck", "ruby") => Some("bundle exec srb tc".to_string()),
            ("analyze", "ruby") => Some("bundle exec rubocop --format json".to_string()),
            ("security", "ruby") => Some("bundle exec bundler-audit check".to_string()),

            // PHP
            ("lint", "php") => Some("./vendor/bin/phpcs".to_string()),
            ("format", "php") => Some("./vendor/bin/php-cs-fixer fix --dry-run --diff".to_string()),
            ("typecheck", "php") => Some("./vendor/bin/phpstan analyse".to_string()),
            ("analyze", "php") => {
                Some("./vendor/bin/phpmd . text cleancode,codesize,controversial".to_string())
            }
            ("security", "php") => Some("composer audit".to_string()),

            // Elixir
            ("lint", "elixir") => Some("mix credo".to_string()),
            ("format", "elixir") => Some("mix format --check-formatted".to_string()),
            ("typecheck", "elixir") => Some("mix dialyzer".to_string()),
            ("analyze", "elixir") => Some("mix credo --format json".to_string()),
            ("security", "elixir") => Some("mix deps.audit".to_string()),

            // Swift
            ("lint", "swift") => Some("swiftlint".to_string()),
            ("format", "swift") => Some("swiftformat --lint .".to_string()),
            ("typecheck", "swift") => Some("swift build".to_string()),
            ("analyze", "swift") => Some("swiftlint --reporter json".to_string()),
            ("security", "swift") => None,

            // .NET
            ("lint", "dotnet") | ("format", "dotnet") => {
                Some("dotnet format --verify-no-changes".to_string())
            }
            ("typecheck", "dotnet") => Some("dotnet build --no-restore".to_string()),
            ("analyze", "dotnet") => Some("dotnet build /p:TreatWarningsAsErrors=true".to_string()),
            ("security", "dotnet") => Some("dotnet list package --vulnerable".to_string()),

            // Unknown language
            (check_type_val, "unknown") => {
                warn!(
                    "Check step '{}': No language detected, skipping {} check.",
                    step_name, check_type_val
                );
                None
            }

            // Unrecognized combination
            _ => {
                warn!(
                    "Check step '{}': Unsupported check type '{}' for language '{}'",
                    step_name, check_type, language
                );
                None
            }
        }
    }

    /// Apply auto-fix modifications to command
    fn apply_auto_fix(command: &str, check_type: &str, language: &str) -> String {
        match (check_type, language) {
            ("lint", "python") => command.replace("ruff check", "ruff check --fix"),
            ("format", "python") => command.replace("--check", ""),
            ("lint", "rust") => command.replace("cargo clippy", "cargo clippy --fix"),
            ("format", "rust") => command.replace("--check", ""),
            ("lint", "go") => command.replace("golangci-lint run", "golangci-lint run --fix"),
            ("format", "go") => command.replace("gofmt -l", "gofmt -w"),
            ("lint", "typescript") | ("lint", "javascript") => {
                if command.contains("eslint") {
                    format!("{} --fix", command)
                } else {
                    command.replace("lint", "lint:fix")
                }
            }
            ("format", "typescript") | ("format", "javascript") => {
                if command.contains("prettier") {
                    command.replace("--check", "--write")
                } else {
                    command
                        .replace("format:check", "format")
                        .replace("--check", "")
                }
            }
            ("format", "c_cpp") => command.replace("--dry-run -Werror", "-i"),
            ("lint", "ruby") | ("format", "ruby") => format!("{} --autocorrect", command),
            ("lint", "php") => command.replace("phpcs", "phpcbf"),
            ("format", "php") => command.replace("--dry-run --diff", ""),
            ("format", "elixir") => command.replace("--check-formatted", ""),
            ("lint", "swift") => format!("{} --fix", command),
            ("format", "swift") => command.replace("--lint", ""),
            ("lint", "dotnet") | ("format", "dotnet") => command.replace("--verify-no-changes", ""),
            _ => command.to_string(),
        }
    }

    /// Process command output
    fn process_output(
        output: std::process::Output,
        step_name: &str,
        duration_ms: u64,
    ) -> (bool, Option<String>, Option<String>) {
        let exit_code = output.status.code();
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let success = output.status.success();

        info!(
            "Check '{}' completed: success={}, exit_code={:?}, duration={}ms",
            step_name, success, exit_code, duration_ms
        );

        if success {
            let output_data = if stdout.is_empty() {
                None
            } else {
                Some(stdout)
            };
            (true, None, output_data)
        } else {
            let mut combined_output = String::new();
            if !stdout.is_empty() {
                combined_output.push_str("=== STDOUT ===\n");
                combined_output.push_str(&stdout);
            }
            if !stderr.is_empty() {
                if !combined_output.is_empty() {
                    combined_output.push_str("\n\n");
                }
                combined_output.push_str("=== STDERR ===\n");
                combined_output.push_str(&stderr);
            }
            let error_summary = if !stderr.is_empty() {
                stderr.lines().take(5).collect::<Vec<_>>().join("\n")
            } else {
                stdout.lines().take(5).collect::<Vec<_>>().join("\n")
            };
            (
                false,
                Some(format!(
                    "Check failed (exit code {:?}): {}",
                    exit_code,
                    error_summary.trim()
                )),
                Some(combined_output),
            )
        }
    }

    /// Emit started tree event for UI updates
    async fn emit_started(
        context: &HandlerContext,
        action_id: &str,
        action_name: &str,
        metadata: serde_json::Value,
    ) -> (f64, u32) {
        context
            .event_emitter
            .emit_action_started(action_id, action_name, metadata)
            .await
    }

    /// Emit completed tree event for UI updates
    async fn emit_completed(
        context: &HandlerContext,
        action_id: &str,
        action_name: &str,
        start_timestamp: f64,
        sequence: u32,
        metadata: serde_json::Value,
    ) {
        context
            .event_emitter
            .emit_action_completed(action_id, action_name, start_timestamp, sequence, metadata)
            .await;
    }

    /// Emit failed tree event for UI updates
    async fn emit_failed(
        context: &HandlerContext,
        action_id: &str,
        action_name: &str,
        start_timestamp: f64,
        sequence: u32,
        error: &str,
        metadata: Option<serde_json::Value>,
    ) {
        context
            .event_emitter
            .emit_action_failed(
                action_id,
                action_name,
                start_timestamp,
                sequence,
                error,
                metadata,
            )
            .await;
    }

    /// Execute AI review check — uses AI to semantically review a file
    async fn execute_ai_review_check(
        step: &ExecutionStepConfig,
        step_name: &str,
        _timeout_secs: Option<u64>,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        use std::sync::atomic::{AtomicU32, Ordering};
        static AI_REVIEW_SEQUENCE: AtomicU32 = AtomicU32::new(1);
        let seq_num = AI_REVIEW_SEQUENCE.fetch_add(1, Ordering::SeqCst);
        let action_id = format!("ai-review-{}", seq_num);
        let action_name = format!("AI REVIEW: {}", step_name);

        // Read the input file
        let input_path = match &step.ai_review_input_path {
            Some(p) => p.clone(),
            None => {
                return StepHandlerResult::failure(
                    "ai_review_input_path is required for ai_review check",
                );
            }
        };

        let input_content = match std::fs::read_to_string(&input_path) {
            Ok(content) => content,
            Err(e) => {
                return StepHandlerResult::failure(format!(
                    "Failed to read input file '{}': {}",
                    input_path, e
                ));
            }
        };

        // Optionally run deterministic workflow validation first
        if step.ai_review_validate_as_workflow.unwrap_or(false) {
            match serde_json::from_str::<crate::unified_workflows::UnifiedWorkflow>(&input_content)
            {
                Ok(workflow) => {
                    let errors =
                        crate::workflow_generation::validation::validate_workflow(&workflow);
                    if !errors.is_empty() {
                        let issues: Vec<String> = errors.iter().map(|e| e.to_string()).collect();
                        return StepHandlerResult::failure_with_data(
                            format!("Structural validation failed: {} issues", issues.len()),
                            json!({
                                "passed": false,
                                "issues": issues,
                                "validation_type": "structural"
                            }),
                        );
                    }
                }
                Err(e) => {
                    return StepHandlerResult::failure_with_data(
                        format!("Input is not valid workflow JSON: {}", e),
                        json!({
                            "passed": false,
                            "issues": [format!("JSON parse error: {}", e)],
                            "validation_type": "structural"
                        }),
                    );
                }
            }
        }

        // Build the AI review prompt
        let review_prompt = step
            .ai_review_prompt
            .as_deref()
            .unwrap_or("Review the following content for correctness, completeness, and quality.");

        let full_prompt = format!(
            "{}\n\n## Input to Review\n\n{}\n\n## Output Format\nReturn ONLY a JSON object (no markdown fencing): {{\"passed\": bool, \"issues\": [\"issue description\", ...]}}",
            review_prompt, input_content
        );

        let metadata = json!({
            "check_type": "ai_review",
            "input_path": &input_path,
            "validate_as_workflow": step.ai_review_validate_as_workflow.unwrap_or(false),
        });

        let (timestamp, sequence) =
            Self::emit_started(context, &action_id, &action_name, metadata).await;

        let start = std::time::Instant::now();

        // Run AI in spawn_blocking (run_prompt_with_routing is sync)
        let task_context = crate::ai_router::TaskContext::from_prompt(&full_prompt);
        let prompt_clone = full_prompt.clone();
        let doctor_handle = context.app_state.doctor_handle.lock().await.clone();
        let ai_result = tokio::task::spawn_blocking(move || {
            crate::ai_provider::run_prompt_with_routing(
                &prompt_clone,
                &task_context,
                doctor_handle.as_ref(),
            )
        })
        .await;

        let duration_ms = start.elapsed().as_millis() as u64;

        let result = match ai_result {
            Ok(response) => {
                if !response.success {
                    let err = response
                        .error
                        .unwrap_or_else(|| "AI review failed".to_string());
                    (false, err, None)
                } else {
                    // Try to parse JSON response
                    let output = response.output;
                    // Extract JSON object from response — the AI may prefix
                    // analysis text before the JSON, so find the first { and last }
                    let json_str = if let Some(start) = output.find('{') {
                        if let Some(end) = output.rfind('}') {
                            &output[start..=end]
                        } else {
                            output.trim()
                        }
                    } else {
                        output.trim()
                    };

                    match serde_json::from_str::<serde_json::Value>(json_str) {
                        Ok(parsed) => {
                            let passed = parsed
                                .get("passed")
                                .and_then(|p| p.as_bool())
                                .unwrap_or(false);
                            let issues: Vec<String> = parsed
                                .get("issues")
                                .and_then(|i| i.as_array())
                                .map(|arr| {
                                    arr.iter()
                                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                        .collect()
                                })
                                .unwrap_or_default();

                            if passed {
                                (
                                    true,
                                    String::new(),
                                    Some(json!({ "passed": true, "issues": issues })),
                                )
                            } else {
                                let issue_summary = issues.join("; ");
                                (
                                    false,
                                    format!("AI review failed: {}", issue_summary),
                                    Some(json!({ "passed": false, "issues": issues })),
                                )
                            }
                        }
                        Err(_) => {
                            // If we can't parse as JSON, treat non-empty output as issues
                            (
                                false,
                                format!("AI review returned non-JSON response: {}", json_str),
                                Some(
                                    json!({ "passed": false, "issues": [json_str], "raw_output": true }),
                                ),
                            )
                        }
                    }
                }
            }
            Err(e) => (false, format!("Failed to run AI review: {}", e), None),
        };

        let (success, error_msg, output_data) = result;

        let result_metadata = json!({
            "check_type": "ai_review",
            "input_path": &input_path,
            "duration_ms": duration_ms,
        });

        if success {
            Self::emit_completed(
                context,
                &action_id,
                &action_name,
                timestamp,
                sequence,
                result_metadata,
            )
            .await;
            match output_data {
                Some(data) => StepHandlerResult::success_with_data(data),
                None => StepHandlerResult::success(),
            }
        } else {
            Self::emit_failed(
                context,
                &action_id,
                &action_name,
                timestamp,
                sequence,
                &error_msg,
                Some(result_metadata),
            )
            .await;
            match output_data {
                Some(data) => StepHandlerResult::failure_with_data(error_msg, data),
                None => StepHandlerResult::failure(error_msg),
            }
        }
    }

    /// Execute HTTP status check
    async fn execute_http_status_check(
        step: &ExecutionStepConfig,
        step_name: &str,
        timeout_secs: Option<u64>,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        let url = match &step.check_url {
            Some(u) => u.clone(),
            None => {
                return StepHandlerResult::failure("check_url is required for http_status check");
            }
        };

        let expected_status = step.expected_status.unwrap_or(200);
        let timeout = timeout_secs
            .map(|t| Duration::from_secs(t.min(300)))
            .unwrap_or(Duration::from_secs(300));

        info!(
            "Executing HTTP status check '{}': url={}, expected_status={}, timeout={}s",
            step_name,
            url,
            expected_status,
            timeout.as_secs()
        );

        // Generate action ID
        use std::sync::atomic::{AtomicU32, Ordering};
        static HTTP_CHECK_SEQUENCE: AtomicU32 = AtomicU32::new(1);
        let seq_num = HTTP_CHECK_SEQUENCE.fetch_add(1, Ordering::SeqCst);
        let action_id = format!("http-check-{}", seq_num);
        let action_name = format!("HTTP CHECK: {}", step_name);

        let metadata = json!({
            "check_type": "http_status",
            "url": &url,
            "expected_status": expected_status,
            "timeout_seconds": timeout.as_secs(),
        });

        let (timestamp, sequence) =
            Self::emit_started(context, &action_id, &action_name, metadata).await;

        let start = std::time::Instant::now();
        let client = match reqwest::Client::builder().timeout(timeout).build() {
            Ok(c) => c,
            Err(e) => {
                return StepHandlerResult::failure(format!("Failed to create HTTP client: {}", e));
            }
        };

        let result = client.get(&url).send().await;
        let duration_ms = start.elapsed().as_millis() as u64;

        let (final_success, error_msg, output_data) = match result {
            Ok(response) => {
                let actual_status = response.status().as_u16();
                info!(
                    "HTTP check '{}' completed: actual_status={}, expected={}, duration={}ms",
                    step_name, actual_status, expected_status, duration_ms
                );

                if actual_status == expected_status {
                    (
                        true,
                        None,
                        Some(json!({
                            "status": actual_status,
                            "url": url,
                            "duration_ms": duration_ms
                        })),
                    )
                } else {
                    (
                        false,
                        Some(format!(
                            "Expected status {} but got {}",
                            expected_status, actual_status
                        )),
                        Some(json!({
                            "expected_status": expected_status,
                            "actual_status": actual_status,
                            "url": url,
                            "duration_ms": duration_ms
                        })),
                    )
                }
            }
            Err(e) => {
                warn!("HTTP check '{}' failed: {}", step_name, e);
                (false, Some(format!("HTTP request failed: {}", e)), None)
            }
        };

        // Emit completion event
        let result_metadata = json!({
            "check_type": "http_status",
            "url": &url,
            "duration_ms": duration_ms,
        });

        if final_success {
            Self::emit_completed(
                context,
                &action_id,
                &action_name,
                timestamp,
                sequence,
                result_metadata,
            )
            .await;
        } else {
            Self::emit_failed(
                context,
                &action_id,
                &action_name,
                timestamp,
                sequence,
                error_msg.as_deref().unwrap_or("HTTP check failed"),
                Some(result_metadata),
            )
            .await;
        }

        if final_success {
            match output_data {
                Some(data) => StepHandlerResult::success_with_data(data),
                None => StepHandlerResult::success(),
            }
        } else {
            match output_data {
                Some(data) => StepHandlerResult::failure_with_data(
                    error_msg.unwrap_or_else(|| "HTTP check failed".to_string()),
                    data,
                ),
                None => StepHandlerResult::failure(
                    error_msg.unwrap_or_else(|| "HTTP check failed".to_string()),
                ),
            }
        }
    }

    /// Execute a CI/CD check by querying GitHub Actions run status via `gh` CLI.
    async fn execute_ci_cd_check(
        step: &ExecutionStepConfig,
        step_name: &str,
        timeout_secs: Option<u64>,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        let timeout = timeout_secs
            .map(|t| Duration::from_secs(t.min(600)))
            .unwrap_or(Duration::from_secs(300));

        // 1. Resolve the repository
        let working_directory = step
            .check_working_directory
            .clone()
            .or_else(|| step.shell_command_working_directory.clone());

        let repo = if let Some(r) = &step.ci_cd_repository {
            if !r.is_empty() {
                r.clone()
            } else {
                // Fall through to auto-detect
                String::new()
            }
        } else {
            String::new()
        };

        let repo = if repo.is_empty() {
            // Auto-detect from working_directory
            let work_dir = match &working_directory {
                Some(d) if !d.is_empty() => d.clone(),
                _ => {
                    return StepHandlerResult::failure(
                        "CI/CD check requires either 'repository' (owner/repo) or 'working_directory' pointing to a git repo",
                    );
                }
            };

            info!(
                "CI/CD check '{}': auto-detecting repo from '{}'",
                step_name, work_dir
            );

            let detect_result = tokio::time::timeout(Duration::from_secs(15), async {
                Command::new("gh")
                    .args([
                        "repo",
                        "view",
                        "--json",
                        "nameWithOwner",
                        "-q",
                        ".nameWithOwner",
                    ])
                    .current_dir(&work_dir)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .output()
                    .await
            })
            .await;

            match detect_result {
                Ok(Ok(output)) if output.status.success() => {
                    let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    if name.is_empty() || !name.contains('/') {
                        return StepHandlerResult::failure(format!(
                            "Could not detect GitHub repo from '{}'. Set 'repository' explicitly.",
                            work_dir
                        ));
                    }
                    info!("CI/CD check '{}': detected repo '{}'", step_name, name);
                    name
                }
                Ok(Ok(output)) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    return StepHandlerResult::failure(format!(
                        "Failed to detect repo from '{}': {}",
                        work_dir,
                        stderr.trim()
                    ));
                }
                Ok(Err(e)) => {
                    return StepHandlerResult::failure(format!(
                        "Failed to run 'gh repo view' in '{}': {}. Is the GitHub CLI installed?",
                        work_dir, e
                    ));
                }
                Err(_) => {
                    return StepHandlerResult::failure(
                        "Timed out detecting repo from working directory",
                    );
                }
            }
        } else {
            repo
        };

        info!(
            "Executing CI/CD check '{}': repo={}, workflow={:?}, branch={:?}, wait={:?}",
            step_name, repo, step.ci_cd_workflow_name, step.ci_cd_branch, step.ci_cd_wait
        );

        // Generate action ID for event emission
        use std::sync::atomic::{AtomicU32, Ordering};
        static CICD_CHECK_SEQUENCE: AtomicU32 = AtomicU32::new(1);
        let seq_num = CICD_CHECK_SEQUENCE.fetch_add(1, Ordering::SeqCst);
        let action_id = format!("cicd-check-{}", seq_num);
        let action_name = format!("CI/CD CHECK: {}", step_name);

        let metadata = json!({
            "check_type": "ci_cd",
            "repository": &repo,
            "workflow_name": step.ci_cd_workflow_name,
            "branch": step.ci_cd_branch,
            "wait_for_completion": step.ci_cd_wait.unwrap_or(false),
        });

        let (timestamp, sequence) =
            Self::emit_started(context, &action_id, &action_name, metadata).await;

        let start = std::time::Instant::now();

        // 2. Build gh run list command
        let poll_result = Self::poll_ci_cd_status(
            &repo,
            step.ci_cd_workflow_name.as_deref(),
            step.ci_cd_branch.as_deref(),
            step.ci_cd_wait.unwrap_or(false),
            timeout,
            step_name,
        )
        .await;

        let duration_ms = start.elapsed().as_millis() as u64;

        let (final_success, error_msg, output_data) = match poll_result {
            Ok(data) => {
                let conclusion = data["conclusion"].as_str().unwrap_or("");
                let success = conclusion == "success";
                if success {
                    (true, None, Some(data))
                } else {
                    let msg = format!(
                        "CI run conclusion: '{}' (run #{}, {})",
                        conclusion,
                        data["run_id"].as_u64().unwrap_or(0),
                        data["url"].as_str().unwrap_or("")
                    );
                    (false, Some(msg), Some(data))
                }
            }
            Err(e) => (false, Some(e), None),
        };

        // Emit completion/failure event
        let result_metadata = json!({
            "check_type": "ci_cd",
            "repository": &repo,
            "duration_ms": duration_ms,
        });

        if final_success {
            Self::emit_completed(
                context,
                &action_id,
                &action_name,
                timestamp,
                sequence,
                result_metadata,
            )
            .await;
        } else {
            Self::emit_failed(
                context,
                &action_id,
                &action_name,
                timestamp,
                sequence,
                error_msg.as_deref().unwrap_or("CI/CD check failed"),
                Some(result_metadata),
            )
            .await;
        }

        if final_success {
            match output_data {
                Some(data) => StepHandlerResult::success_with_data(data),
                None => StepHandlerResult::success(),
            }
        } else {
            match output_data {
                Some(data) => StepHandlerResult::failure_with_data(
                    error_msg.unwrap_or_else(|| "CI/CD check failed".to_string()),
                    data,
                ),
                None => StepHandlerResult::failure(
                    error_msg.unwrap_or_else(|| "CI/CD check failed".to_string()),
                ),
            }
        }
    }

    /// Poll GitHub Actions for the latest run status. If `wait` is true, re-checks every 15s
    /// until the run concludes or the timeout expires.
    async fn poll_ci_cd_status(
        repo: &str,
        workflow_name: Option<&str>,
        branch: Option<&str>,
        wait: bool,
        timeout_duration: Duration,
        step_name: &str,
    ) -> Result<serde_json::Value, String> {
        let deadline = tokio::time::Instant::now() + timeout_duration;

        loop {
            // Build command
            let mut args = vec![
                "run".to_string(),
                "list".to_string(),
                "--repo".to_string(),
                repo.to_string(),
                "--limit".to_string(),
                "1".to_string(),
                "--json".to_string(),
                "status,conclusion,name,headBranch,databaseId,url,event".to_string(),
            ];

            if let Some(wf) = workflow_name {
                if !wf.is_empty() {
                    args.push("--workflow".to_string());
                    args.push(wf.to_string());
                }
            }

            if let Some(br) = branch {
                if !br.is_empty() {
                    args.push("--branch".to_string());
                    args.push(br.to_string());
                }
            }

            let output = Command::new("gh")
                .args(&args)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .await
                .map_err(|e| {
                    format!(
                        "Failed to run 'gh run list': {}. Is the GitHub CLI installed?",
                        e
                    )
                })?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(format!("gh run list failed: {}", stderr.trim()));
            }

            let stdout = String::from_utf8_lossy(&output.stdout);
            let runs: Vec<serde_json::Value> = serde_json::from_str(stdout.trim())
                .map_err(|e| format!("Failed to parse gh output: {}", e))?;

            if runs.is_empty() {
                return Err(format!(
                    "No workflow runs found for repo '{}' (workflow={:?}, branch={:?})",
                    repo, workflow_name, branch
                ));
            }

            let run = &runs[0];
            let status = run["status"].as_str().unwrap_or("");
            let conclusion = run["conclusion"].as_str().unwrap_or("");
            let run_id = run["databaseId"].as_u64().unwrap_or(0);
            let url = run["url"].as_str().unwrap_or("");
            let workflow = run["name"].as_str().unwrap_or("");
            let head_branch = run["headBranch"].as_str().unwrap_or("");

            info!(
                "CI/CD check '{}': run #{} status={}, conclusion={}, workflow={}, branch={}",
                step_name, run_id, status, conclusion, workflow, head_branch
            );

            // If the run has concluded, return the result
            if status == "completed" || !conclusion.is_empty() {
                return Ok(json!({
                    "repository": repo,
                    "run_id": run_id,
                    "status": status,
                    "conclusion": conclusion,
                    "workflow_name": workflow,
                    "branch": head_branch,
                    "url": url,
                }));
            }

            // Run is still in progress
            if !wait {
                return Ok(json!({
                    "repository": repo,
                    "run_id": run_id,
                    "status": status,
                    "conclusion": "in_progress",
                    "workflow_name": workflow,
                    "branch": head_branch,
                    "url": url,
                }));
            }

            // Wait and retry
            if tokio::time::Instant::now() + Duration::from_secs(15) > deadline {
                return Err(format!(
                    "CI/CD check timed out waiting for run #{} to complete (status: {})",
                    run_id, status
                ));
            }

            info!(
                "CI/CD check '{}': run #{} still {}, polling again in 15s...",
                step_name, run_id, status
            );
            tokio::time::sleep(Duration::from_secs(15)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_check_handler_step_type() {
        let handler = CheckHandler;
        assert_eq!(handler.step_type(), "check");
        assert_eq!(handler.display_name(), "Check");
    }

    #[test]
    fn test_detect_language_rust() {
        // This test would need a temp directory with Cargo.toml
        // For now, just verify the function exists and compiles
        let lang = CheckHandler::detect_language(".");
        assert!(!lang.is_empty());
    }

    #[test]
    fn test_get_auto_command_rust_lint() {
        let cmd = CheckHandler::get_auto_command("lint", "rust", "test");
        assert_eq!(cmd, Some("cargo clippy -- -D warnings".to_string()));
    }

    #[test]
    fn test_apply_auto_fix_python_lint() {
        let cmd = CheckHandler::apply_auto_fix("ruff check .", "lint", "python");
        assert_eq!(cmd, "ruff check --fix .");
    }

    #[test]
    fn test_extract_env_prefix_simple() {
        let (envs, cmd) =
            CheckHandler::extract_env_prefix_for_cmd("SKIP_WEB_SERVER=1 npx playwright test");
        assert_eq!(envs, vec![("SKIP_WEB_SERVER".to_string(), "1".to_string())]);
        assert_eq!(cmd, "npx playwright test");
    }

    #[test]
    fn test_extract_env_prefix_multiple() {
        let (envs, cmd) = CheckHandler::extract_env_prefix_for_cmd("FOO=bar BAZ=123 echo hello");
        assert_eq!(envs.len(), 2);
        assert_eq!(envs[0], ("FOO".to_string(), "bar".to_string()));
        assert_eq!(envs[1], ("BAZ".to_string(), "123".to_string()));
        assert_eq!(cmd, "echo hello");
    }

    #[test]
    fn test_extract_env_prefix_none() {
        let (envs, cmd) = CheckHandler::extract_env_prefix_for_cmd("npx eslint .");
        assert!(envs.is_empty());
        assert_eq!(cmd, "npx eslint .");
    }
}
