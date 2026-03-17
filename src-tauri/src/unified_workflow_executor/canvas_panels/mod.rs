//! Canvas Panel Manager for workflow execution intelligence.
//!
//! Emits structured canvas panels at workflow lifecycle events,
//! providing a live execution dashboard in the Canvas widget.

mod builders;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use serde_json::Value;
use tauri::Emitter;
use tracing::{info, warn};

use crate::database::CheckpointDb;
use crate::event_system::AppEvent;
use crate::mcp::canvas::{CanvasState, StoredPanel};
use crate::orchestrator::types::Finding;
use crate::step_executor::{StepExecutionResult, VerificationPhaseResult};
use crate::str_utils::truncate_str;
use crate::unified_workflow_executor::types::{AgenticOutcome, LoopConfig, LoopResult};

/// Snapshot of a single iteration's results (internal tracking).
#[derive(Debug, Clone)]
pub(crate) struct IterationSnapshot {
    pub iteration: u32,
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub total: usize,
    pub failed_step_names: Vec<String>,
    pub failed_step_errors: Vec<(String, String)>,
    pub verify_duration_ms: u64,
    pub agentic_duration_ms: Option<u64>,
    pub agentic_outcome: Option<String>,
    pub findings_count: usize,
    pub injected_steps_count: usize,
}

/// Manages canvas panel emission during workflow execution.
///
/// Created once per workflow run, called at each lifecycle point.
/// All emission is best-effort — failures are logged but never block execution.
pub struct CanvasPanelManager {
    canvas_state: Arc<tokio::sync::RwLock<CanvasState>>,
    checkpoint_db: Arc<CheckpointDb>,
    app_handle: tauri::AppHandle,
    execution_id: String,
    workflow_name: String,
    stage_prefix: Option<String>,
    iteration_snapshots: Vec<IterationSnapshot>,
    accumulated_findings: Vec<Finding>,
    workflow_start_time: Instant,
    max_iterations: u32,
    max_sessions: Option<u32>,
    setup_duration_ms: u64,
    setup_step_count: usize,
    // Acceptance criteria tracking
    acceptance_criteria: Option<Value>,
    criterion_statuses: HashMap<String, (String, Option<String>)>, // id -> (status, last_error)
    step_to_criterion: HashMap<String, String>,                    // step_name -> criterion_id
    // Mission Brief state for live updates
    brief_prompt: String,
    brief_model: String,
    brief_reflection: bool,
    brief_approval_gate: bool,
    brief_stages: Vec<Value>,
    brief_current_stage: Option<u32>,
    brief_current_iteration: u32,
    brief_phase: Option<String>,
    brief_activity: Option<String>,
}

impl CanvasPanelManager {
    /// Create a new canvas panel manager.
    ///
    /// The `execution_id` and `workflow_name` are set later via `on_workflow_start()`
    /// since they come from LoopConfig which isn't available at construction time.
    pub fn new(
        canvas_state: Arc<tokio::sync::RwLock<CanvasState>>,
        checkpoint_db: Arc<CheckpointDb>,
        app_handle: tauri::AppHandle,
    ) -> Self {
        Self {
            canvas_state,
            checkpoint_db,
            app_handle,
            execution_id: String::new(),
            workflow_name: String::new(),
            stage_prefix: None,
            iteration_snapshots: Vec::new(),
            accumulated_findings: Vec::new(),
            workflow_start_time: Instant::now(),
            max_iterations: 5,
            max_sessions: None,
            setup_duration_ms: 0,
            setup_step_count: 0,
            acceptance_criteria: None,
            criterion_statuses: HashMap::new(),
            step_to_criterion: HashMap::new(),
            brief_prompt: String::new(),
            brief_model: String::from("default"),
            brief_reflection: false,
            brief_approval_gate: false,
            brief_stages: Vec::new(),
            brief_current_stage: None,
            brief_current_iteration: 0,
            brief_phase: None,
            brief_activity: None,
        }
    }

    // ─── Lifecycle Methods ───

    /// Called at workflow start. Clears canvas and emits Mission Brief.
    pub async fn on_workflow_start(&mut self, config: &LoopConfig) {
        self.execution_id = config.execution_id.clone();
        self.workflow_name = config.workflow_name.clone();
        self.max_iterations = config.max_iterations;
        self.max_sessions = config.max_sessions;
        self.workflow_start_time = Instant::now();
        self.iteration_snapshots.clear();
        self.accumulated_findings.clear();
        self.stage_prefix = None;

        // Store Mission Brief state for live updates
        self.brief_prompt = config.base_prompt.clone();
        self.brief_model = config
            .model_override
            .as_deref()
            .unwrap_or("default")
            .to_string();
        self.brief_reflection = config.reflection_mode;
        self.brief_approval_gate = config.approval_gate;
        self.brief_stages = if config.stages.is_empty() {
            vec![
                serde_json::json!({ "name": config.workflow_name, "index": 0, "max_iterations": config.max_iterations }),
            ]
        } else {
            config
                .stages
                .iter()
                .map(|s| {
                    serde_json::json!({
                        "name": s.name,
                        "index": s.index,
                        "max_iterations": s.max_iterations,
                    })
                })
                .collect()
        };
        self.brief_current_stage = None;
        self.brief_current_iteration = 0;
        self.brief_phase = None;
        self.brief_activity = None;

        // Clear canvas for fresh run
        {
            let mut canvas = self.canvas_state.write().await;
            canvas.clear();
            canvas.set_task_run_id(Some(self.execution_id.clone()));
        }

        // Clear persisted panels
        let db = self.checkpoint_db.clone();
        let exec_id = self.execution_id.clone();
        tokio::task::spawn_blocking(move || {
            let _ = db.clear_canvas_panels_for_task_run(&exec_id);
        });

        // Initialize acceptance criteria tracking
        self.acceptance_criteria = config.acceptance_criteria.clone();
        self.criterion_statuses.clear();
        self.step_to_criterion.clear();

        if let Some(ref criteria) = self.acceptance_criteria {
            // Initialize all criteria to "pending"
            if let Some(criteria_array) = criteria.get("criteria").and_then(|c| c.as_array()) {
                for criterion in criteria_array {
                    if let Some(id) = criterion.get("id").and_then(|v| v.as_str()) {
                        self.criterion_statuses
                            .insert(id.to_string(), ("pending".to_string(), None));
                    }
                }
            }

            // Load step_name -> criterion_id mapping from the embedded step_mapping
            if let Some(mapping) = criteria.get("step_mapping").and_then(|v| v.as_object()) {
                for (step_name, cid_val) in mapping {
                    if let Some(cid) = cid_val.as_str() {
                        self.step_to_criterion
                            .insert(step_name.clone(), cid.to_string());
                    }
                }
            }

            // Emit initial AcceptanceCriteria panel
            let data = builders::build_acceptance_criteria(criteria, &self.criterion_statuses);
            self.emit_panel(
                "acceptance-criteria",
                "AcceptanceCriteria",
                "Acceptance Criteria",
                data,
                4, // Right after outcome (1), waterfall (2), step-durations (3)
                "normal",
                Some("Overview"),
            )
            .await;
        }

        // Emit Mission Brief
        let data = builders::build_mission_brief(config);
        self.emit_panel(
            "overview",
            "MissionBrief",
            "Mission Brief",
            data,
            5,
            "normal",
            Some("Overview"),
        )
        .await;
    }

    /// Called when a stage begins (multi-stage workflows).
    pub async fn on_stage_start(&mut self, stage_idx: u32, stage_name: &str, total_stages: usize) {
        if total_stages > 1 {
            self.stage_prefix = Some(format!("s{}", stage_idx));
        } else {
            self.stage_prefix = None;
        }
        // Reset per-stage tracking
        self.iteration_snapshots.clear();

        info!(
            "Canvas: stage start {}/{} '{}'",
            stage_idx + 1,
            total_stages,
            stage_name
        );

        // Update Mission Brief with stage progress
        self.brief_current_stage = Some(stage_idx);
        self.brief_current_iteration = 0;
        self.brief_phase = Some("setup".to_string());
        self.brief_activity = Some(format!("Starting stage: {}", stage_name));
        self.emit_mission_brief_update().await;
    }

    /// Called after setup phase completes.
    pub async fn on_setup_complete(&mut self, success: bool, results: &[StepExecutionResult]) {
        let duration_ms: u64 = results.iter().map(|r| r.duration_ms).sum();
        self.setup_duration_ms = duration_ms;
        self.setup_step_count = results.len();
        let data = builders::build_setup_report(success, results, duration_ms);
        self.emit_panel(
            "setup",
            "KeyValue",
            "Setup",
            data,
            6,
            "compact",
            Some("Overview"),
        )
        .await;

        // Update Mission Brief: setup done, entering verification
        let status = if success {
            "Setup complete"
        } else {
            "Setup failed"
        };
        self.brief_phase = Some("verification".to_string());
        self.brief_activity = Some(status.to_string());
        self.emit_mission_brief_update().await;
    }

    /// Called after verification phase completes.
    pub async fn on_verification_complete(
        &mut self,
        iteration: u32,
        result: &VerificationPhaseResult,
        verification_history: &HashMap<String, Vec<bool>>,
    ) {
        let failed_step_names: Vec<String> = result
            .step_results
            .iter()
            .filter(|r| !r.success)
            .map(|r| r.step_name.clone())
            .collect();

        let failed_step_errors: Vec<(String, String)> = result
            .step_results
            .iter()
            .filter(|r| !r.success)
            .map(|r| {
                (
                    r.step_name.clone(),
                    r.error
                        .clone()
                        .unwrap_or_else(|| "Unknown error".to_string()),
                )
            })
            .collect();

        let snapshot = IterationSnapshot {
            iteration,
            passed: result.passed_steps,
            failed: result.failed_steps,
            skipped: result.skipped_steps,
            total: result.total_steps,
            failed_step_names,
            failed_step_errors,
            verify_duration_ms: result.total_duration_ms,
            agentic_duration_ms: None,
            agentic_outcome: None,
            findings_count: 0,
            injected_steps_count: 0,
        };

        self.iteration_snapshots.push(snapshot.clone());

        // Update acceptance criteria statuses from verification results
        if self.acceptance_criteria.is_some() {
            let mut criteria_changed = false;
            for step_result in &result.step_results {
                if let Some(criterion_id) = self.step_to_criterion.get(&step_result.step_name) {
                    let new_status = if step_result.success {
                        "passed".to_string()
                    } else {
                        "failed".to_string()
                    };
                    let last_error = if step_result.success {
                        None
                    } else {
                        step_result.error.clone()
                    };
                    self.criterion_statuses
                        .insert(criterion_id.clone(), (new_status, last_error));
                    criteria_changed = true;
                }
            }

            if criteria_changed {
                if let Some(ref criteria) = self.acceptance_criteria {
                    let data =
                        builders::build_acceptance_criteria(criteria, &self.criterion_statuses);
                    self.emit_panel(
                        "acceptance-criteria",
                        "AcceptanceCriteria",
                        "Acceptance Criteria",
                        data,
                        4,
                        "normal",
                        Some("Overview"),
                    )
                    .await;
                }
            }
        }

        // Verification Matrix (update in-place each iteration)
        let matrix_data =
            builders::build_verification_matrix(&self.iteration_snapshots, verification_history);
        self.emit_panel(
            "matrix",
            "Table",
            "Verification Matrix",
            matrix_data,
            10,
            "normal",
            Some("Progress"),
        )
        .await;

        // Convergence Tracker (after 2+ iterations)
        if self.iteration_snapshots.len() >= 2 {
            let conv_data = builders::build_convergence_tracker(&self.iteration_snapshots);
            self.emit_panel(
                "convergence",
                "ProgressChart",
                "Convergence",
                conv_data,
                11,
                "compact",
                Some("Progress"),
            )
            .await;
        }

        // Summary Stats Bar
        let stats_data = builders::build_summary_stats(&self.iteration_snapshots);
        self.emit_panel(
            "summary-stats",
            "SummaryStats",
            "Check Status",
            stats_data,
            8,
            "compact",
            Some("Progress"),
        )
        .await;

        // State Timeline (after 2+ iterations, more useful than convergence alone)
        if self.iteration_snapshots.len() >= 2 {
            let timeline_data = builders::build_state_timeline(verification_history);
            self.emit_panel(
                "state-timeline",
                "StateTimeline",
                "Step Convergence",
                timeline_data,
                12,
                "normal",
                Some("Progress"),
            )
            .await;

            // Sparkline
            let sparkline_data = builders::build_sparkline(verification_history);
            self.emit_panel(
                "sparkline",
                "Sparkline",
                "Step Trends",
                sparkline_data,
                13,
                "compact",
                Some("Progress"),
            )
            .await;
        }

        // Waffle Chart
        let waffle_data = builders::build_waffle_chart(&self.iteration_snapshots);
        self.emit_panel(
            "waffle",
            "WaffleChart",
            "Check Coverage",
            waffle_data,
            9,
            "compact",
            Some("Progress"),
        )
        .await;

        // Iteration Digest
        let prev = if self.iteration_snapshots.len() >= 2 {
            Some(&self.iteration_snapshots[self.iteration_snapshots.len() - 2])
        } else {
            None
        };
        let digest_data = builders::build_iteration_digest_verification(&snapshot, prev);
        let priority = 20 + (self.max_iterations.saturating_sub(iteration)) as i32;
        let group = if let Some(ref prefix) = self.stage_prefix {
            format!("Stage {} · Iteration {}", prefix, iteration)
        } else {
            format!("Iteration {}", iteration)
        };
        self.emit_panel(
            &format!("iter-{}", iteration),
            "Markdown",
            &format!("Iteration {}", iteration),
            digest_data,
            priority,
            "compact",
            Some(&group),
        )
        .await;

        // Iteration Comparison (after 2+ iterations)
        if self.iteration_snapshots.len() >= 2 {
            let comparison_data = builders::build_iteration_comparison(&self.iteration_snapshots);
            self.emit_panel(
                "iter-comparison",
                "IterationComparison",
                "Iteration Comparison",
                comparison_data,
                14,
                "compact",
                Some("Progress"),
            )
            .await;
        }

        // Phase Timeline
        let phase_timeline_data = builders::build_phase_timeline(
            &self.iteration_snapshots,
            self.setup_duration_ms,
            self.setup_step_count,
            self.workflow_start_time.elapsed(),
        );
        self.emit_panel(
            "phase-timeline",
            "PhaseTimeline",
            "Phase Timeline",
            phase_timeline_data,
            7,
            "compact",
            Some("Overview"),
        )
        .await;

        // Phase Distribution
        let distribution_data = builders::build_phase_distribution(
            &self.iteration_snapshots,
            self.setup_duration_ms,
            self.workflow_start_time.elapsed(),
        );
        self.emit_panel(
            "phase-distribution",
            "PhaseDistribution",
            "Time Distribution",
            distribution_data,
            15,
            "compact",
            Some("Progress"),
        )
        .await;

        // Resource Dashboard
        let sessions_count = self.get_sessions_count().await;
        let resource_data = builders::build_resource_dashboard(
            iteration,
            self.max_iterations,
            sessions_count,
            self.max_sessions,
            self.workflow_start_time.elapsed(),
        );
        self.emit_panel(
            "resources",
            "KeyValue",
            "Resources",
            resource_data,
            50,
            "compact",
            Some("Resources"),
        )
        .await;

        // Update Mission Brief with verification results
        self.brief_current_iteration = iteration;
        if result.all_passed {
            self.brief_phase = Some("completion".to_string());
            self.brief_activity = Some(format!(
                "Verification passed ({}/{} checks)",
                result.passed_steps, result.total_steps
            ));
        } else {
            self.brief_phase = Some("agentic".to_string());
            self.brief_activity = Some(format!(
                "Verification: {}/{} passed — running AI fix",
                result.passed_steps, result.total_steps
            ));
        }
        self.emit_mission_brief_update().await;
    }

    /// Called after agentic phase completes.
    pub async fn on_agentic_complete(
        &mut self,
        iteration: u32,
        outcome: &AgenticOutcome,
        findings: &[Finding],
        injected_count: usize,
        agentic_duration_ms: u64,
    ) {
        // Update the latest snapshot with agentic results
        if let Some(snapshot) = self.iteration_snapshots.last_mut() {
            snapshot.agentic_duration_ms = Some(agentic_duration_ms);
            snapshot.agentic_outcome = Some(match outcome {
                AgenticOutcome::Success { .. } => "Completed".to_string(),
                AgenticOutcome::Failed { .. } => "Failed".to_string(),
                AgenticOutcome::Error { .. } => "Error".to_string(),
                AgenticOutcome::Skipped => "Skipped".to_string(),
            });
            snapshot.findings_count = findings.len();
            snapshot.injected_steps_count = injected_count;

            // Update iteration digest with full info
            let digest_data = builders::build_iteration_digest_full(snapshot);
            let priority = 20 + (self.max_iterations.saturating_sub(iteration)) as i32;
            let group = if let Some(ref prefix) = self.stage_prefix {
                format!("Stage {} · Iteration {}", prefix, iteration)
            } else {
                format!("Iteration {}", iteration)
            };
            self.emit_panel(
                &format!("iter-{}", iteration),
                "Markdown",
                &format!("Iteration {}", iteration),
                digest_data,
                priority,
                "compact",
                Some(&group),
            )
            .await;
        }

        // Accumulate findings
        self.accumulated_findings.extend(findings.iter().cloned());

        // Findings Board
        if !self.accumulated_findings.is_empty() {
            let findings_data = builders::build_findings_board(&self.accumulated_findings);
            self.emit_panel(
                "findings",
                "FindingList",
                "Findings",
                findings_data,
                40,
                "normal",
                Some("Intelligence"),
            )
            .await;
        }

        // Update Mission Brief with agentic outcome
        let activity = match outcome {
            AgenticOutcome::Success { .. } => {
                format!("AI completed — re-verifying (iteration {})", iteration)
            }
            AgenticOutcome::Failed { error, .. } => {
                format!("AI failed: {}", truncate_str(error, 80))
            }
            AgenticOutcome::Error { error } => {
                format!("AI error: {}", truncate_str(error, 80))
            }
            AgenticOutcome::Skipped => "AI phase skipped".to_string(),
        };
        self.brief_phase = Some("verification".to_string());
        self.brief_activity = Some(activity);
        self.emit_mission_brief_update().await;
    }

    /// Called when the workflow completes (success or failure).
    pub async fn on_workflow_complete(
        &mut self,
        result: &LoopResult,
        duration: std::time::Duration,
        sessions_count: u32,
    ) {
        let data = builders::build_outcome_alert(result, duration, sessions_count);
        self.emit_panel(
            "outcome",
            "Alert",
            "Outcome",
            data,
            1,
            "normal",
            Some("Overview"),
        )
        .await;

        // Final acceptance criteria update: mark remaining pending as skipped
        if let Some(ref criteria) = self.acceptance_criteria {
            let mut changed = false;
            for (_id, (status, _err)) in self.criterion_statuses.iter_mut() {
                if status == "pending" || status == "running" {
                    *status = "skipped".to_string();
                    changed = true;
                }
            }
            if changed {
                let data =
                    builders::build_acceptance_criteria(criteria, &self.criterion_statuses);
                self.emit_panel(
                    "acceptance-criteria",
                    "AcceptanceCriteria",
                    "Acceptance Criteria",
                    data,
                    4,
                    "normal",
                    Some("Overview"),
                )
                .await;
            }
        }

        // Final Mission Brief update
        self.brief_phase = Some("completed".to_string());
        self.brief_activity = Some(result.summary());
        self.emit_mission_brief_update().await;

        // Waterfall timing panel
        let mut step_timings: Vec<(String, u64, u64, &str, Option<&str>)> = Vec::new();
        let mut offset_ms: u64 = 0;

        // Add iteration timings
        for snapshot in &self.iteration_snapshots {
            let verify_status = if snapshot.failed == 0 {
                "success"
            } else {
                "failed"
            };
            step_timings.push((
                format!("Verify #{}", snapshot.iteration),
                offset_ms,
                snapshot.verify_duration_ms,
                verify_status,
                Some("verification"),
            ));
            offset_ms += snapshot.verify_duration_ms;

            if let Some(agentic_ms) = snapshot.agentic_duration_ms {
                let agentic_status = match snapshot.agentic_outcome.as_deref() {
                    Some("Completed") => "success",
                    Some("Failed") | Some("Error") => "failed",
                    _ => "skipped",
                };
                step_timings.push((
                    format!("AI #{}", snapshot.iteration),
                    offset_ms,
                    agentic_ms,
                    agentic_status,
                    Some("agentic"),
                ));
                offset_ms += agentic_ms;
            }
        }

        let total_duration_ms = duration.as_millis() as u64;

        // Step Duration Chart (top 10 longest steps)
        let duration_chart_data: Vec<(String, u64, &str, Option<&str>)> = step_timings
            .iter()
            .map(|(name, _start, dur, status, phase)| (name.clone(), *dur, *status, *phase))
            .collect();
        let step_duration_data = builders::build_step_duration_chart(&duration_chart_data, 10);
        self.emit_panel(
            "step-durations",
            "StepDurationChart",
            "Slowest Steps",
            step_duration_data,
            3,
            "compact",
            Some("Overview"),
        )
        .await;

        let waterfall_data = builders::build_waterfall(&step_timings, total_duration_ms);
        self.emit_panel(
            "waterfall",
            "Waterfall",
            "Execution Timeline",
            waterfall_data,
            2,
            "normal",
            Some("Overview"),
        )
        .await;

        // Final Phase Timeline (with accurate total duration)
        let final_phase_timeline = builders::build_phase_timeline(
            &self.iteration_snapshots,
            self.setup_duration_ms,
            self.setup_step_count,
            duration,
        );
        self.emit_panel(
            "phase-timeline",
            "PhaseTimeline",
            "Phase Timeline",
            final_phase_timeline,
            7,
            "compact",
            Some("Overview"),
        )
        .await;

        // Final Phase Distribution (with accurate total duration)
        let final_distribution = builders::build_phase_distribution(
            &self.iteration_snapshots,
            self.setup_duration_ms,
            duration,
        );
        self.emit_panel(
            "phase-distribution",
            "PhaseDistribution",
            "Time Distribution",
            final_distribution,
            15,
            "compact",
            Some("Progress"),
        )
        .await;
    }

    /// Called when the workflow fails on an early-return path (no LoopResult available).
    pub async fn on_workflow_failed(&self, reason: &str) {
        let elapsed = self.workflow_start_time.elapsed();
        let data = serde_json::json!({
            "severity": "error",
            "message": reason,
            "details": format!("Duration: {}", builders::format_duration_pub(elapsed)),
        });
        self.emit_panel(
            "outcome",
            "Alert",
            "Outcome",
            data,
            1,
            "normal",
            Some("Overview"),
        )
        .await;
    }

    // ─── Internal Helpers ───

    /// Re-emit the Mission Brief panel with updated progress data.
    async fn emit_mission_brief_update(&self) {
        let data = builders::build_mission_brief_progress(
            &self.workflow_name,
            &self.brief_prompt,
            &self.brief_model,
            self.brief_reflection,
            self.brief_approval_gate,
            &self.brief_stages,
            self.max_iterations,
            self.brief_current_stage,
            self.brief_current_iteration,
            self.brief_phase.as_deref(),
            self.brief_activity.as_deref(),
        );
        self.emit_panel(
            "overview",
            "MissionBrief",
            "Mission Brief",
            data,
            5,
            "normal",
            Some("Overview"),
        )
        .await;
    }

    /// Emit a panel to canvas state, persist to SQLite, and broadcast event.
    async fn emit_panel(
        &self,
        panel_suffix: &str,
        component: &str,
        title: &str,
        data: Value,
        priority: i32,
        size: &str,
        group: Option<&str>,
    ) {
        let panel_id = self.make_panel_id(panel_suffix);
        let now = chrono::Utc::now().to_rfc3339();

        let panel = StoredPanel {
            panel_id: panel_id.clone(),
            component: component.to_string(),
            title: title.to_string(),
            data,
            priority,
            size: size.to_string(),
            group: group.map(|g| g.to_string()),
            task_run_id: self.execution_id.clone(),
            created_at: now.clone(),
            updated_at: now,
        };

        // Insert into in-memory state
        {
            let mut canvas = self.canvas_state.write().await;
            canvas.insert_panel(panel.clone());
        }

        // Persist to SQLite (background, non-blocking)
        let db = self.checkpoint_db.clone();
        let panel_for_db = panel.clone();
        tokio::task::spawn_blocking(move || {
            if let Err(e) = db.insert_or_update_canvas_panel(&panel_for_db) {
                warn!("Canvas: failed to persist panel: {}", e);
            }
        });

        // Emit event
        let event = AppEvent::CanvasUpdate {
            action: "update".to_string(),
            panel_id: panel.panel_id.clone(),
            panel: Some(serde_json::to_value(&panel).unwrap_or_default()),
            task_run_id: Some(self.execution_id.clone()),
        };
        if let Err(e) = self.app_handle.emit(event.event_name(), &event) {
            warn!("Canvas: failed to emit event: {}", e);
        }
    }

    /// Build a panel ID with optional stage prefix.
    fn make_panel_id(&self, suffix: &str) -> String {
        if let Some(ref prefix) = self.stage_prefix {
            format!("{}:{}:{}", self.execution_id, prefix, suffix)
        } else {
            format!("{}:{}", self.execution_id, suffix)
        }
    }

    /// Get the current sessions count from the database.
    pub async fn get_sessions_count(&self) -> u32 {
        let db = self.checkpoint_db.clone();
        let exec_id = self.execution_id.clone();
        tokio::task::spawn_blocking(move || {
            db.get_task_run(&exec_id)
                .ok()
                .flatten()
                .map(|t| t.sessions_count)
                .unwrap_or(0)
        })
        .await
        .unwrap_or(0)
    }
}
