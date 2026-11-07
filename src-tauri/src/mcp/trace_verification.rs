//! Trace & Worker-Signal Utilities
//!
//! Contains functions for extracting trace/video data and parsing worker output signals.

use regex::Regex;
use tracing::{info, warn};

use crate::orchestrator::{WorkerSignal, WorkerSignalExt};

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
        let name = file.name().map_err(|e| e.to_string())?.to_string();

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
