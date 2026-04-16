//! Trace & Verification Utilities
//!
//! Contains functions for extracting trace/video data, parsing worker output signals,
//! running deterministic verification checks, and generating verification feedback.

use regex::Regex;
use tracing::{info, warn};

use crate::config_storage::ConfigStorage;
use crate::orchestrator::{DeterministicVerifier, WorkerSignal, WorkerSignalExt};

// ============================================================================
// Trace and Video Extraction Utilities
// ============================================================================

/// Extract action timeline and screenshots from a Playwright trace ZIP file
pub fn extract_trace_data(
    trace_path: &str,
    max_screenshots: u32,
) -> Result<(String, Vec<String>), String> {
    use std::io::Read;

    info!("Extracting trace data from: {}", trace_path);

    let file =
        std::fs::File::open(trace_path).map_err(|e| format!("Failed to open trace file: {}", e))?;

    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| format!("Failed to read trace ZIP: {}", e))?;

    let mut timeline = String::new();
    let mut screenshot_paths = Vec::new();

    // Create temp directory for extracted screenshots
    let temp_dir = std::env::temp_dir().join(format!("trace_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp_dir).map_err(|e| format!("Failed to create temp dir: {}", e))?;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(|e| e.to_string())?;
        let name = file.name().to_string();

        // Extract action log for timeline
        if name.contains("actions") && name.ends_with(".json") {
            let mut contents = String::new();
            file.read_to_string(&mut contents).ok();
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&contents) {
                timeline = format_trace_timeline(&json);
            }
        }

        // Extract screenshots (limited by max_screenshots)
        if name.ends_with(".png") && screenshot_paths.len() < max_screenshots as usize {
            let out_path = temp_dir.join(format!("trace_screenshot_{}.png", i));
            if let Ok(mut out_file) = std::fs::File::create(&out_path) {
                if std::io::copy(&mut file, &mut out_file).is_ok() {
                    screenshot_paths.push(out_path.to_string_lossy().to_string());
                }
            }
        }
    }

    info!(
        "Extracted trace: {} chars timeline, {} screenshots",
        timeline.len(),
        screenshot_paths.len()
    );

    Ok((timeline, screenshot_paths))
}

/// Format trace events into a human-readable timeline
pub fn format_trace_timeline(json: &serde_json::Value) -> String {
    let mut timeline = String::from("## Action Timeline from Trace\n\n");

    if let Some(events) = json.as_array() {
        for (i, event) in events.iter().enumerate() {
            let action_type = event
                .get("type")
                .and_then(|t| t.as_str())
                .unwrap_or("unknown");
            let selector = event.get("selector").and_then(|s| s.as_str()).unwrap_or("");
            let value = event.get("value").and_then(|v| v.as_str()).unwrap_or("");

            if !selector.is_empty() {
                timeline.push_str(&format!(
                    "{}. {} on `{}` {}\n",
                    i + 1,
                    action_type,
                    selector,
                    if !value.is_empty() {
                        format!("with value '{}'", value)
                    } else {
                        String::new()
                    }
                ));
            } else {
                timeline.push_str(&format!("{}. {}\n", i + 1, action_type));
            }
        }
    }

    timeline
}

/// Extract key frames from a video file using ffmpeg
pub fn extract_video_frames(video_path: &str, max_frames: u32) -> Result<Vec<String>, String> {
    info!(
        "Extracting {} frames from video: {}",
        max_frames, video_path
    );

    // Create temp directory for frames
    let temp_dir = std::env::temp_dir().join(format!("video_frames_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp_dir).map_err(|e| format!("Failed to create temp dir: {}", e))?;

    let output_pattern = temp_dir
        .join("frame_%03d.png")
        .to_string_lossy()
        .to_string();

    // Use ffmpeg to extract frames evenly distributed throughout the video
    // -vf "select='not(mod(n,X))'" extracts every Xth frame
    // For 3 frames from a 30 fps, 10 sec video (300 frames), we'd extract every 100th frame
    let status = crate::process_helpers::no_window("ffmpeg")
        .args([
            "-y", // Overwrite output
            "-i",
            video_path,
            "-vf",
            &format!("select='lt(n\\,{})',setpts=N/FRAME_RATE/TB", max_frames),
            "-vsync",
            "vfr",
            "-frames:v",
            &max_frames.to_string(),
            &output_pattern,
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    match status {
        Ok(s) if s.success() => {
            // Collect extracted frames
            let mut frames = Vec::new();
            if let Ok(entries) = std::fs::read_dir(&temp_dir) {
                for entry in entries.flatten() {
                    if entry.path().extension().is_some_and(|e| e == "png") {
                        frames.push(entry.path().to_string_lossy().to_string());
                    }
                }
            }
            frames.sort(); // Ensure frames are in order
            info!("Extracted {} video frames", frames.len());
            Ok(frames)
        }
        Ok(_) => Err(
            "ffmpeg failed to extract frames. Ensure ffmpeg is installed and in PATH.".to_string(),
        ),
        Err(e) => Err(format!(
            "Failed to run ffmpeg: {}. Ensure ffmpeg is installed and in PATH.",
            e
        )),
    }
}

/// Collect all images for AI analysis (screenshots, trace screenshots, video frames)
pub fn collect_images_for_analysis(
    image_paths: &[String],
    video_paths: &[String],
    trace_path: Option<&str>,
    max_video_frames: u32,
    max_trace_screenshots: u32,
) -> (Vec<String>, Option<String>) {
    let mut all_images = image_paths.to_vec();
    let mut trace_timeline = None;

    // Extract trace data if provided
    if let Some(tp) = trace_path {
        match extract_trace_data(tp, max_trace_screenshots) {
            Ok((timeline, screenshots)) => {
                trace_timeline = Some(timeline);
                all_images.extend(screenshots);
            }
            Err(e) => {
                warn!("Failed to extract trace data: {}", e);
            }
        }
    }

    // Extract video frames if provided
    for video_path in video_paths {
        match extract_video_frames(video_path, max_video_frames) {
            Ok(frames) => {
                all_images.extend(frames);
            }
            Err(e) => {
                warn!("Failed to extract video frames: {}", e);
            }
        }
    }

    (all_images, trace_timeline)
}

// ============================================================================
// Worker Output Signal Parsing
// ============================================================================

/// Result of parsing worker output for signals
#[derive(Debug, Clone)]
pub enum WorkerOutputSignal {
    /// Worker signals work is complete, ready for verification
    WorkComplete { reason: Option<String> },
    /// Worker requests replan
    NeedReplan { reason: String },
    /// Legacy task complete marker (deprecated but still supported)
    TaskComplete,
    /// No signal found, worker continues
    Continue,
}

/// Parse worker output for orchestrator signals
/// This is the primary signal detection used by the orchestrator architecture
pub fn parse_worker_output_signal(output: &str) -> WorkerOutputSignal {
    // First check for the new orchestrator signals
    if let Some(signal) = WorkerSignal::parse_from_output(output) {
        match signal {
            WorkerSignal::WorkComplete { reason } => {
                info!("Worker signal detected: [WORK_COMPLETE]");
                return WorkerOutputSignal::WorkComplete { reason };
            }
            WorkerSignal::NeedReplan { reason } => {
                info!("Worker signal detected: [NEED_REPLAN] - {}", reason);
                return WorkerOutputSignal::NeedReplan { reason };
            }
            WorkerSignal::Finding(_) => {
                // Findings don't terminate the loop, they're just recorded
            }
            WorkerSignal::Continue => {}
        }
    }

    // Check for legacy TASK_COMPLETE marker (deprecated but supported for backward compatibility)
    let output_upper = output.to_uppercase();
    let legacy_markers = [
        "[TASK_COMPLETE]",
        "[GOAL_COMPLETE]",
        "[GOAL_ACHIEVED]",
        "[STOP_SESSION]",
        "[SESSION_COMPLETE]",
    ];

    for marker in &legacy_markers {
        if output_upper.contains(marker) {
            info!(
                "Legacy completion marker detected: {} - treating as WORK_COMPLETE",
                marker
            );
            // Treat legacy markers as WORK_COMPLETE so verification still runs
            return WorkerOutputSignal::WorkComplete {
                reason: Some(format!("Legacy marker: {}", marker)),
            };
        }
    }

    // Check for structured completion patterns
    let completion_patterns = [
        r#""goal_achieved":\s*true"#,
        r#""goal_achieved": true"#,
        r#"goal_achieved:\s*true"#,
        r#""status":\s*"complete""#,
        r#"status:\s*"complete""#,
    ];

    for pattern in &completion_patterns {
        if let Ok(re) = Regex::new(pattern) {
            if re.is_match(output) {
                info!(
                    "Goal completion pattern detected: {} - treating as WORK_COMPLETE",
                    pattern
                );
                return WorkerOutputSignal::WorkComplete {
                    reason: Some(format!("Pattern match: {}", pattern)),
                };
            }
        }
    }

    WorkerOutputSignal::Continue
}

/// Check if AI output contains goal completion markers
/// Returns true if any marker indicates the goal has been achieved
/// NOTE: This is kept for backward compatibility but parse_worker_output_signal should be preferred
pub fn check_goal_completion_markers(output: &str) -> bool {
    matches!(
        parse_worker_output_signal(output),
        WorkerOutputSignal::WorkComplete { .. } | WorkerOutputSignal::TaskComplete
    )
}

// ============================================================================
// Deterministic Verification
// ============================================================================

/// Result of running deterministic verification
#[derive(Debug, Clone)]
pub struct DeterministicVerificationResult {
    /// Whether all CRITICAL checks passed (non-critical failures are informational)
    pub all_passed: bool,
    /// Summary of what was checked
    pub checks_run: Vec<String>,
    /// Details of CRITICAL failures (these block completion)
    pub critical_failures: Vec<String>,
    /// Details of non-critical failures (informational only)
    pub non_critical_failures: Vec<String>,
    /// Raw output from checks
    pub raw_output: String,
}

/// Run the workflow's actual verification steps (if defined) instead of just build checks
///
/// This function:
/// 1. Gets the task_run from database
/// 2. Extracts verification_steps from execution_steps_json
/// 3. If verification_steps exist, runs them through StepExecutor
/// 4. Otherwise falls back to basic deterministic verification
pub async fn run_workflow_verification_for_task(
    app_state: &std::sync::Arc<crate::AppState>,
    config_storage: &std::sync::Arc<tokio::sync::Mutex<ConfigStorage>>,
    db_task_id: &str,
    workspace_root: &str,
) -> DeterministicVerificationResult {
    use crate::step_executor::{ExecutionStepConfig, StepExecutor};

    // Get the task run to extract verification steps, config_id, and session count
    let task_run = app_state
        .pg_db
        .get_task_run(db_task_id)
        .await
        .ok()
        .flatten();

    let config_id = task_run.as_ref().and_then(|t| t.config_id.clone());
    let session_num = task_run.as_ref().map(|t| t.sessions_count as i32);

    // Try to get verification steps from the task's execution_steps_json
    let verification_steps: Vec<ExecutionStepConfig> = task_run
        .as_ref()
        .and_then(|task| {
            task.execution_steps_json
                .as_ref()
                .and_then(|json| serde_json::from_str::<Vec<ExecutionStepConfig>>(json).ok())
                .map(|steps| {
                    steps
                        .into_iter()
                        .filter(|s| s.phase.as_deref() == Some("verification"))
                        .collect()
                })
        })
        .unwrap_or_default();

    // If no verification steps defined, fall back to basic deterministic verification
    if verification_steps.is_empty() {
        info!(
            "WORKFLOW-VERIFICATION: No verification_steps found for task {} - falling back to basic build checks",
            db_task_id
        );
        return run_deterministic_verification(
            workspace_root,
            None,
            None,
            config_id.as_deref(),
            Some(db_task_id),
            session_num,
        )
        .await;
    }

    info!(
        "WORKFLOW-VERIFICATION: Running {} verification_steps for task {}",
        verification_steps.len(),
        db_task_id
    );

    // Create a StepExecutor to run the verification steps
    let executor = StepExecutor::new(app_state.clone(), config_storage.clone());

    // Run verification steps
    let verification_result = executor
        .execute_verification_steps(&verification_steps, db_task_id, 1)
        .await;

    // Log the result
    info!(
        "WORKFLOW-VERIFICATION: Result for task {}: all_passed={}, passed={}/{}, failed={}",
        db_task_id,
        verification_result.all_passed,
        verification_result.passed_steps,
        verification_result.total_steps,
        verification_result.failed_steps
    );

    // Convert to DeterministicVerificationResult format
    let mut checks_run = Vec::new();
    let mut critical_failures = Vec::new();
    let mut raw_output = String::new();

    for result in &verification_result.step_results {
        let check_name = format!("{} ({})", result.step_name, result.step_type);
        checks_run.push(check_name.clone());

        if !result.success {
            let failure_msg = if let Some(ref error) = result.error {
                format!("{}: {}", check_name, error)
            } else {
                format!("{}: failed", check_name)
            };
            critical_failures.push(failure_msg);

            // Add detailed output if available
            if let Some(ref details) = result.verification_details {
                if let Some(ref stdout) = details.stdout {
                    raw_output.push_str(&format!("=== {} ===\n{}\n\n", check_name, stdout));
                }
            }
        }
    }

    DeterministicVerificationResult {
        all_passed: verification_result.all_passed,
        checks_run,
        critical_failures,
        non_critical_failures: Vec::new(),
        raw_output,
    }
}

/// Run deterministic verification for a task
/// This runs build, tests, type checks, etc. before allowing task completion
///
/// IMPORTANT: Tests have an `is_critical` flag. Only critical test failures
/// block task completion. Non-critical failures are reported but don't fail verification.
pub async fn run_deterministic_verification(
    workspace_root: &str,
    _verification_config: Option<&serde_json::Value>,
    _db: Option<&()>,
    config_id: Option<&str>,
    task_run_id: Option<&str>,
    session_num: Option<i32>,
) -> DeterministicVerificationResult {
    let _verifier = DeterministicVerifier::new(workspace_root.to_string());
    let mut checks_run = Vec::new();
    let mut critical_failures = Vec::new();
    let mut non_critical_failures: Vec<String> = Vec::new();
    let mut raw_output = String::new();

    // For Phase 1: Run basic build checks
    // Build checks are always CRITICAL - if the code doesn't compile, verification fails
    let workspace = std::path::Path::new(workspace_root);

    // Check for npm project
    if workspace.join("package.json").exists() {
        checks_run.push("npm build (critical)".to_string());
        info!("Running npm build verification in {}", workspace_root);

        let output = if cfg!(target_os = "windows") {
            crate::process_helpers::cmd_no_window()
                .args(["/C", "npm run build"])
                .current_dir(workspace_root)
                .output()
        } else {
            crate::process_helpers::no_window("sh")
                .args(["-c", "npm run build"])
                .current_dir(workspace_root)
                .output()
        };

        match output {
            Ok(result) => {
                let stdout = String::from_utf8_lossy(&result.stdout);
                let stderr = String::from_utf8_lossy(&result.stderr);
                raw_output.push_str(&format!(
                    "=== npm build (CRITICAL) ===\nExit: {}\nStdout:\n{}\nStderr:\n{}\n\n",
                    result.status.code().unwrap_or(-1),
                    stdout,
                    stderr
                ));

                if !result.status.success() {
                    critical_failures.push(format!(
                        "npm build failed with exit code {}",
                        result.status.code().unwrap_or(-1)
                    ));
                    // Extract error lines
                    for line in stderr.lines().chain(stdout.lines()) {
                        let lower = line.to_lowercase();
                        if lower.contains("error") || lower.contains("failed") {
                            critical_failures.push(line.to_string());
                        }
                    }
                }
            }
            Err(e) => {
                critical_failures.push(format!("Failed to run npm build: {}", e));
            }
        }
    }

    // Check for Cargo project
    if workspace.join("Cargo.toml").exists() {
        checks_run.push("cargo check (critical)".to_string());
        info!("Running cargo check verification in {}", workspace_root);

        let output = crate::process_helpers::no_window("cargo")
            .args(["check"])
            .current_dir(workspace_root)
            .output();

        match output {
            Ok(result) => {
                let stdout = String::from_utf8_lossy(&result.stdout);
                let stderr = String::from_utf8_lossy(&result.stderr);
                raw_output.push_str(&format!(
                    "=== cargo check (CRITICAL) ===\nExit: {}\nStdout:\n{}\nStderr:\n{}\n\n",
                    result.status.code().unwrap_or(-1),
                    stdout,
                    stderr
                ));

                if !result.status.success() {
                    critical_failures.push(format!(
                        "cargo check failed with exit code {}",
                        result.status.code().unwrap_or(-1)
                    ));
                    // Extract error lines
                    for line in stderr.lines() {
                        if line.contains("error[E") || line.starts_with("error:") {
                            critical_failures.push(line.to_string());
                        }
                    }
                }
            }
            Err(e) => {
                critical_failures.push(format!("Failed to run cargo check: {}", e));
            }
        }
    }

    // If no build system found, verification passes by default
    if checks_run.is_empty() {
        checks_run.push("(no build system detected)".to_string());
        raw_output.push_str("No package.json or Cargo.toml found. Skipping build verification.\n");
    }

    // Phase 2: Verification tests — checkpoint_db removed, not yet migrated to PG
    let _ = (config_id, task_run_id, session_num);

    DeterministicVerificationResult {
        // Only CRITICAL failures block completion
        all_passed: critical_failures.is_empty(),
        checks_run,
        critical_failures,
        non_critical_failures,
        raw_output,
    }
}

/// Generate feedback for failed verification to include in next iteration prompt
pub fn generate_verification_feedback(result: &DeterministicVerificationResult) -> String {
    let mut feedback = String::new();

    if !result.all_passed {
        feedback.push_str("## \u{26a0}\u{fe0f} Deterministic Verification Failed\n\n");
        feedback.push_str(
            "The system ran verification after your [WORK_COMPLETE] signal and found issues:\n\n",
        );

        feedback.push_str("### Checks Run\n");
        for check in &result.checks_run {
            feedback.push_str(&format!("- {}\n", check));
        }

        if !result.critical_failures.is_empty() {
            feedback.push_str("\n### \u{274c} Critical Failures (blocking)\n");
            feedback.push_str("These MUST be fixed before the task can complete:\n");
            for failure in &result.critical_failures {
                feedback.push_str(&format!("- {}\n", failure));
            }
        }

        if !result.non_critical_failures.is_empty() {
            feedback.push_str("\n### \u{26a0}\u{fe0f} Non-Critical Failures (informational)\n");
            feedback.push_str("These don't block completion but should be reviewed:\n");
            for failure in &result.non_critical_failures {
                feedback.push_str(&format!("- {}\n", failure));
            }
        }

        feedback.push_str("\n### Action Required\n");
        feedback.push_str(
            "Please fix the CRITICAL issues above and signal [WORK_COMPLETE] again when ready.\n",
        );
        feedback.push_str("The task will NOT be marked complete until all critical checks pass.\n");
    }

    feedback
}
